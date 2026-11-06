// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The fleet task board: one row per worker this Mac started, on any member or on this Mac,
//! kept in its state folder and served to every member it lets drive it
//! (`docs/DESIGN-fleet-task-board-2026-11-04.md`).
//!
//! The board is the one record of where a worker this Mac started runs: `--host any @<sid> …`
//! follows a worker by its row ([`crate::remote`]), `await task board` waits on every open row
//! ([`crate::await_task`]), and the aterm serving this Mac keeps the rows current ([`keep`]).

use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The board's folder under the state folder.
pub const DIR: &str = "board";

/// The lock every read-modify-write of a row holds, in the board's folder.
#[cfg(unix)]
const LOCK: &str = ".lock";

/// The lock the one keeper of a board holds while it runs, in the board's folder ([`keep`]).
#[cfg(unix)]
const KEEPER_LOCK: &str = ".keeper";

/// How long a write waits for another writer of the board.
#[cfg(unix)]
const LOCK_WITHIN: Duration = Duration::from_secs(10);

/// The most a row's file is read for.
#[cfg(unix)]
const ROW_MAX_BYTES: usize = 16 * 1024;

/// The most rows a read of the board shows ([`shown`]).
pub const SHOWN_MAX: usize = 512;

/// The most bytes of rows a read of the board shows, so its reply fits a fleet reply.
const SHOWN_MAX_BYTES: usize = 192 * 1024;

/// How long a closed row stays on the board.
const CLOSED_KEPT: Duration = Duration::from_secs(7 * 24 * 3600);

/// How long a row nothing was heard of stays on the board.
const UNHEARD_KEPT: Duration = Duration::from_secs(30 * 24 * 3600);

/// How long the keeper asks after a row nothing was heard of.
#[cfg(unix)]
const UNHEARD_ASKED: Duration = Duration::from_secs(7 * 24 * 3600);

/// How stale a row's `heard_ms=` may grow before an answer that changes nothing else is written.
const HEARD_WRITTEN_EVERY: Duration = Duration::from_secs(300);

/// The longest a label or an accept word is.
const WORD_MAX: usize = 64;

/// The row's keys, in the order a row is written.
const KEYS: [&str; 25] = [
    "sid",
    "label",
    "node",
    "host",
    "program",
    "model",
    "account",
    "autopilot",
    "cwd",
    "brief",
    "brief_bytes",
    "from",
    "started_ms",
    "state",
    "rev",
    "conclusion",
    "why",
    "checked",
    "landed",
    "attention",
    "accept",
    "accept_ms",
    "heard_ms",
    "after",
    "area",
];

/// One worker this Mac started.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Row {
    /// Its global session id (`s-<hex>`), on the Mac it runs on.
    pub sid: String,
    /// The dispatcher's label for the package it works on (`label=` at spawn).
    pub label: Option<String>,
    /// The node id of the Mac it runs on, as that Mac proved it to the spawn; `None` for the
    /// board's own Mac.
    pub node: Option<String>,
    /// How a person names that Mac.
    pub host: Option<String>,
    /// The program it runs (`agent=`).
    pub program: Option<String>,
    /// The model asked for (`model=`).
    pub model: Option<String>,
    /// The account row `--host any` chose its Mac by, masked (`ab…@…`).
    pub account: Option<String>,
    /// Its Autopilot (`autopilot=`; a worker's default, `land`, where none was named).
    pub autopilot: Option<String>,
    /// Its working folder on its Mac (`cwd=`), where the spawn named one.
    pub cwd: Option<String>,
    /// `sha256:<64 hex>` of the task sent; the task's text is never kept.
    pub brief: Option<String>,
    /// The task's size in bytes.
    pub brief_bytes: u64,
    /// The dispatcher session that started it (`from=`).
    pub from: Option<String>,
    /// When it was started, in Unix milliseconds on this Mac's clock.
    pub started_ms: u64,
    /// Its task state as last read, or `closed`.
    pub state: String,
    /// Its task record's revision as last read (0 before any read).
    pub rev: u64,
    /// `finished` or `failed`, once its task has concluded.
    pub conclusion: Option<String>,
    /// Why it concluded or stalled (the task record's `why=`).
    pub why: Option<String>,
    /// Its landing receipt (`checked=<12 hex>@<ref>`, or `pushed|no-remote|no-repo|gone`).
    pub checked: Option<String>,
    /// The commit it landed (the 12 hex of a `checked=<head>@<ref>`), kept once read.
    pub landed: Option<String>,
    /// The ask it stands at, as `await task` shows it (`attention=`).
    pub attention: Option<String>,
    /// What the dispatcher observed of the package's accept test ([`Board::accept`]).
    pub accept: Option<String>,
    /// When it recorded that, in Unix milliseconds.
    pub accept_ms: Option<u64>,
    /// When its Mac last answered for it, in Unix milliseconds on this Mac's clock.
    pub heard_ms: Option<u64>,
    /// The labels its package waits on (DX-3's; nothing schedules by them yet).
    pub after: Option<String>,
    /// The file areas its package claims (DX-3's; nothing schedules by them yet).
    pub area: Option<String>,
    /// Words of keys this aterm does not know (a later aterm's), as read: written back unchanged.
    pub extra: Vec<(String, String)>,
}

impl Row {
    /// A worker just started: `working` at revision 0, nothing heard yet.
    #[must_use]
    pub fn started(sid: &str, started_ms: u64) -> Self {
        Self {
            sid: sid.to_owned(),
            started_ms,
            state: "working".to_owned(),
            ..Self::default()
        }
    }

    /// Whether its worker's tab still stands, as last read: a board waits on it. A state this
    /// aterm does not know (a later aterm's) is open.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.state != "closed"
    }

    /// Whether `from` and `label` (a [`Filter`]) both take this row.
    #[must_use]
    pub fn matches(&self, filter: &Filter) -> bool {
        filter
            .from
            .as_deref()
            .is_none_or(|from| self.from.as_deref() == Some(from))
            && filter.label.as_deref().is_none_or(|want| {
                self.label
                    .as_deref()
                    .is_some_and(|label| match want.strip_suffix('*') {
                        Some(prefix) => label.starts_with(prefix),
                        None => label == want,
                    })
            })
    }

    /// The row as one line of `key=value` words, values percent-encoded, `-` for none.
    #[must_use]
    pub fn line(&self) -> String {
        let num = |n: u64| n.to_string();
        let opt_num = |n: Option<u64>| n.map_or_else(|| "-".to_owned(), |n| n.to_string());
        let word = |s: &Option<String>| s.as_deref().map_or_else(|| "-".to_owned(), encode);
        let values: [String; 25] = [
            encode(&self.sid),
            word(&self.label),
            word(&self.node),
            word(&self.host),
            word(&self.program),
            word(&self.model),
            word(&self.account),
            word(&self.autopilot),
            word(&self.cwd),
            word(&self.brief),
            num(self.brief_bytes),
            word(&self.from),
            num(self.started_ms),
            encode(&self.state),
            num(self.rev),
            word(&self.conclusion),
            word(&self.why),
            word(&self.checked),
            word(&self.landed),
            word(&self.attention),
            word(&self.accept),
            opt_num(self.accept_ms),
            opt_num(self.heard_ms),
            word(&self.after),
            word(&self.area),
        ];
        KEYS.iter()
            .zip(values)
            .map(|(key, value)| format!("{key}={value}"))
            .chain(
                self.extra
                    .iter()
                    .map(|(key, value)| format!("{key}={value}")),
            )
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Read a row's line ([`Row::line`]): a key it does not know (a later aterm's) is kept as
    /// it was written ([`Row::extra`]).
    /// # Errors
    /// A word that is no `key=value`, a key named twice or no word, no `sid=` of `s-<hex>`, a
    /// `state=` that is no word, or a number that is none.
    pub fn parse(line: &str) -> Result<Self, String> {
        let mut seen: Vec<&str> = Vec::new();
        let mut row = Self::default();
        let mut sid = None;
        let mut state = None;
        for word in line.split_whitespace() {
            let (key, value) = word
                .split_once('=')
                .ok_or_else(|| format!("`{}` is no key=value", one_line(word)))?;
            if seen.contains(&key) {
                return Err(format!("`{}` is named twice", one_line(key)));
            }
            if !valid_word(key) {
                return Err(format!("`{}` is no key", one_line(key)));
            }
            seen.push(key);
            let text = || (value != "-").then(|| decode(value));
            let number = || -> Result<u64, String> {
                parse_u64(value).ok_or_else(|| format!("{key}= is no number"))
            };
            let opt_number = || -> Result<Option<u64>, String> {
                if value == "-" {
                    Ok(None)
                } else {
                    number().map(Some)
                }
            };
            match key {
                "sid" => sid = Some(decode(value)),
                "label" => row.label = text(),
                "node" => row.node = text(),
                "host" => row.host = text(),
                "program" => row.program = text(),
                "model" => row.model = text(),
                "account" => row.account = text(),
                "autopilot" => row.autopilot = text(),
                "cwd" => row.cwd = text(),
                "brief" => row.brief = text(),
                "brief_bytes" => row.brief_bytes = number()?,
                "from" => row.from = text(),
                "started_ms" => row.started_ms = number()?,
                "state" => state = Some(decode(value)),
                "rev" => row.rev = number()?,
                "conclusion" => row.conclusion = text(),
                "why" => row.why = text(),
                "checked" => row.checked = text(),
                "landed" => row.landed = text(),
                "attention" => row.attention = text(),
                "accept" => row.accept = text(),
                "accept_ms" => row.accept_ms = opt_number()?,
                "heard_ms" => row.heard_ms = opt_number()?,
                "after" => row.after = text(),
                "area" => row.area = text(),
                // Kept in the board's own encoding, so no byte a peer sent reaches a terminal.
                _ => row.extra.push((key.to_owned(), encode(&decode(value)))),
            }
        }
        row.sid = sid.filter(|sid| is_sid(sid)).ok_or("no sid= of s-<hex>")?;
        row.state = state
            .filter(|state| valid_word(state))
            .ok_or("no state= word")?;
        Ok(row)
    }

    /// The row as `await task` prints a worker's: what a wait shows of a worker it could not
    /// read on its Mac, ending `read=board`.
    #[must_use]
    pub(crate) fn await_row(&self) -> String {
        let mut row = format!(
            "@{} rev={} state={} agent=- program_state=- attention={} result_bytes=0",
            self.sid,
            self.rev,
            self.state,
            self.attention
                .as_deref()
                .map_or_else(|| "-".to_owned(), encode)
        );
        for (key, value) in [
            ("conclusion", &self.conclusion),
            ("why", &self.why),
            ("checked", &self.checked),
        ] {
            if let Some(value) = value {
                row.push_str(&format!(" {key}={}", encode(value)));
            }
        }
        row.push_str(" read=board");
        row
    }

    /// The words a wait adds to each row it prints of this worker: its label and its Mac.
    #[must_use]
    pub(crate) fn wait_tail(&self, board_host: Option<&str>) -> String {
        let host = self.host.as_deref().or(board_host);
        format!(
            " label={} host={}",
            self.label.as_deref().map_or_else(|| "-".to_owned(), encode),
            host.map_or_else(|| "-".to_owned(), encode)
        )
    }

    /// Merge `seen`, a worker's row as `await task` answered it, read at `now_ms`: whether
    /// anything but `heard_ms` changed.
    ///
    /// The board's `rev` is the highest its worker's Mac reported. A live row replaces the state
    /// when its `rev` is at least the board's, a `closed` row's too: a worker read as gone (its
    /// Mac restarting, its tab not restored yet) that answers again at its own rev is open again,
    /// and a row read from before a close made at the board's rev is the record the close was
    /// read from (the next ask closes it again). A `closed` row always applies. Its conclusion,
    /// why and receipt replace the board's only where its `rev` is at least the board's: one
    /// below it was built from an older row than the board has heard, and the board's words
    /// stand, as they do where the close carries none (a closed tab with no conclusion is
    /// `failed`, as `await task` reads it). It raises the board's `rev` to its own only where
    /// that rev is one its Mac reported, which a row says by ending `task_news=`: every row a
    /// Mac writes does, and so does a close a wait makes from the last row the Mac answered for
    /// the worker, which keeps that row's words, its rev among them. A live row read before such
    /// a close, at a lower rev, never reopens it. A close a wait makes with no row of the Mac's
    /// read carries the asker's edge and no `task_news=`, and leaves the board's `rev`, so the
    /// worker's own answer at a rev at least the board's reopens it. `landed` is kept once read:
    /// a worker that landed a commit landed it still.
    pub fn merge(&mut self, seen: &str, now_ms: u64) -> bool {
        let field = |key: &str| crate::await_task::row_field(seen, key).map(decode);
        let given = |key: &str| field(key).filter(|v| !v.is_empty() && v != "-");
        let Some(state) = field("state").filter(|s| valid_word(s)) else {
            return false;
        };
        let rev = field("rev").and_then(|r| parse_u64(&r));
        let before = self.clone();
        if state == "closed" {
            // `task_news=` marks a rev the worker's Mac reported: a row it wrote, or a close a
            // wait built from the last row it answered (`await_task::closed_row`). A stale live
            // row below that rev must not reopen the row and drop what the close kept. A close a
            // wait built from no row carries the asker's edge, which the board does not take, so
            // the worker's own answer at its own rev still reopens the row.
            // A close's words are as new as the row it was built from: one below the board's rev
            // (a wait's last row, read before the board heard the worker move on) keeps the
            // board's.
            let current = rev.is_some_and(|rev| rev >= self.rev);
            let word = |key: &str| given(key).filter(|_| current);
            if field("task_news").is_some() {
                self.rev = rev.map_or(self.rev, |rev| rev.max(self.rev));
            }
            self.state = state;
            self.conclusion = word("conclusion")
                .or_else(|| self.conclusion.take())
                .or_else(|| Some("failed".to_owned()));
            self.why = word("why").or_else(|| self.why.take());
            self.checked = word("checked").or_else(|| self.checked.take());
            self.attention = None;
        } else {
            let Some(rev) = rev.filter(|&rev| rev >= self.rev) else {
                return false;
            };
            self.state = state;
            self.rev = rev;
            self.conclusion = given("conclusion");
            self.why = given("why");
            self.checked = given("checked");
            self.attention = given("attention");
        }
        if let Some(head) = self.checked.as_deref().and_then(landed_head) {
            self.landed = Some(head.to_owned());
        }
        let was_open = before.is_open();
        let changed = *self
            != Self {
                heard_ms: self.heard_ms,
                ..before
            };
        // A closed row's `heard_ms` is when it was last heard open or closed: a worker read as
        // gone again does not move it, so the keeper stops asking a day after the close and
        // the row is pruned a week after it.
        if was_open || self.is_open() {
            self.heard_ms = Some(now_ms);
        }
        changed
    }
}

/// The commit a `checked=<12 hex>@<ref>` receipt landed: its head.
fn landed_head(checked: &str) -> Option<&str> {
    let (head, main) = checked.split_once('@')?;
    (head.len() == 12 && head.bytes().all(|b| b.is_ascii_hexdigit()) && !main.is_empty())
        .then_some(head)
}

/// What a read of a board found: its rows, oldest first, and how many files it could not read.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Scan {
    /// The rows read, by `started_ms`, oldest first.
    pub rows: Vec<Row>,
    /// Files in the board's folder named like a row that were no private regular file, or held
    /// no row.
    pub unread: usize,
}

/// One board: a folder of rows ([`DIR`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Board {
    dir: PathBuf,
}

#[cfg(test)]
std::thread_local! {
    static HERE_OVERRIDE: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// While it lives, [`Board::here`] on this thread is the board in its folder (a test's own).
#[cfg(test)]
pub(crate) struct HereGuard;

#[cfg(test)]
impl HereGuard {
    pub(crate) fn at(dir: &Path) -> Self {
        HERE_OVERRIDE.with(|cell| *cell.borrow_mut() = Some(dir.to_path_buf()));
        Self
    }
}

#[cfg(test)]
impl Drop for HereGuard {
    fn drop(&mut self) {
        HERE_OVERRIDE.with(|cell| *cell.borrow_mut() = None);
    }
}

impl Board {
    /// This Mac's board, in its state folder; `None` where it has none, or keeps no board (a
    /// platform with no private files).
    #[must_use]
    pub fn here() -> Option<Self> {
        #[cfg(test)]
        if let Some(dir) = HERE_OVERRIDE.with(|cell| cell.borrow().clone()) {
            return Some(Self::at(&dir));
        }
        if !cfg!(unix) {
            return None;
        }
        aterm_types::dirs::state_dir().map(|state| Self::in_state(&state))
    }

    /// The board of the state folder `state`.
    #[must_use]
    pub fn in_state(state: &Path) -> Self {
        Self {
            dir: state.join(DIR),
        }
    }

    /// The board kept in `dir` itself.
    #[must_use]
    pub fn at(dir: &Path) -> Self {
        Self {
            dir: dir.to_path_buf(),
        }
    }

    /// Its folder.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Every row, oldest first; none where the folder is missing or is no private folder.
    #[must_use]
    pub fn scan(&self) -> Scan {
        let mut scan = Scan::default();
        #[cfg(unix)]
        {
            let Ok(judged) = aterm_uds::private_file::JudgedDir::judge(&self.dir) else {
                return scan;
            };
            if !judged.is_private() {
                return scan;
            }
            let Ok(entries) = std::fs::read_dir(&self.dir) else {
                return scan;
            };
            for entry in entries.flatten() {
                let name = entry.file_name();
                let Some(name) = name.to_str().filter(|name| is_sid(name)) else {
                    continue;
                };
                match Self::read_in(&judged, name) {
                    Some(row) => scan.rows.push(row),
                    None => scan.unread += 1,
                }
            }
        }
        scan.rows
            .sort_by(|a, b| (a.started_ms, &a.sid).cmp(&(b.started_ms, &b.sid)));
        scan
    }

    /// The row of `sid`, where the board holds one it can read.
    #[must_use]
    pub fn row(&self, sid: &str) -> Option<Row> {
        if !is_sid(sid) {
            return None;
        }
        self.read(sid)
    }

    #[cfg(unix)]
    fn read(&self, name: &str) -> Option<Row> {
        let bytes = aterm_uds::private_file::read_private(&self.dir, name, ROW_MAX_BYTES).ok()?;
        Self::parse_row(name, bytes)
    }

    #[cfg(unix)]
    fn read_in(judged: &aterm_uds::private_file::JudgedDir<'_>, name: &str) -> Option<Row> {
        let bytes = judged.read(name, ROW_MAX_BYTES).ok()?;
        Self::parse_row(name, bytes)
    }

    #[cfg(unix)]
    fn parse_row(name: &str, bytes: Vec<u8>) -> Option<Row> {
        let text = String::from_utf8(bytes).ok()?;
        Row::parse(text.lines().next()?)
            .ok()
            .filter(|row| row.sid == name)
    }

    #[cfg(not(unix))]
    fn read(&self, _name: &str) -> Option<Row> {
        None
    }

    /// Write `row`, a worker just started, over any row of its sid.
    /// # Errors
    /// The folder cannot be made private, its lock is held past [`LOCK_WITHIN`], or the write
    /// fails.
    pub fn record(&self, row: &Row) -> io::Result<()> {
        let _lock = self.lock()?;
        self.write(row)?;
        self.prune_locked(now_ms());
        Ok(())
    }

    /// Merge each of `seen` (rows as `await task` answers them, `@<sid> rev= state= …`) into
    /// the rows of their sids, read at `now_ms`; a row of a sid the board does not hold is left
    /// out. Answers how many rows changed.
    /// # Errors
    /// As [`Board::record`].
    pub fn apply(&self, seen: &[String], now_ms: u64) -> io::Result<usize> {
        let sids: Vec<(&str, &String)> = seen
            .iter()
            .filter_map(|row| Some((seen_sid(row)?, row)))
            .collect();
        if sids.is_empty() || sids.iter().all(|(sid, _)| self.row(sid).is_none()) {
            return Ok(0);
        }
        let _lock = self.lock()?;
        let mut changed = 0;
        for (sid, seen) in sids {
            let Some(mut row) = self.row(sid) else {
                continue;
            };
            let heard = row.heard_ms;
            if row.merge(seen, now_ms) {
                changed += 1;
                self.write(&row)?;
            } else if row.heard_ms != heard
                && heard
                    .is_none_or(|heard| now_ms.saturating_sub(heard) >= millis(HEARD_WRITTEN_EVERY))
            {
                self.write(&row)?;
            }
        }
        Ok(changed)
    }

    /// Record `word`, what the dispatcher observed of the package's accept test, on the row
    /// `who` names (`@<sid>`, or a label: the newest row with it), at `now_ms`.
    /// # Errors
    /// `word` or `who` is no word a board takes, no row is named, or as [`Board::record`].
    pub fn accept(&self, who: &str, word: &str, now_ms: u64) -> io::Result<Row> {
        let invalid = |why: String| io::Error::new(io::ErrorKind::InvalidInput, why);
        if !valid_name(word) {
            return Err(invalid(format!(
                "board accept: `{}` is no accept word (1-{WORD_MAX} of A-Z a-z 0-9 . _ -, not \
                 `-` alone)",
                one_line(word)
            )));
        }
        let _lock = self.lock()?;
        let scan = self.scan();
        let named = match who.strip_prefix('@') {
            Some(sid) => scan.rows.into_iter().find(|row| row.sid == sid),
            None if valid_name(who) => scan
                .rows
                .into_iter()
                .rev()
                .find(|row| row.label.as_deref() == Some(who)),
            None => {
                return Err(invalid(format!(
                    "board accept: `{}` names no worker: @<sid> or its label",
                    one_line(who)
                )));
            }
        };
        let mut row = named.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("board accept: no row of `{}` on this board", one_line(who)),
            )
        })?;
        row.accept = Some(word.to_owned());
        row.accept_ms = Some(now_ms);
        self.write(&row)?;
        Ok(row)
    }

    /// Remove the rows closed for [`CLOSED_KEPT`], and those unheard for [`UNHEARD_KEPT`].
    pub fn prune(&self) {
        if let Ok(_lock) = self.lock() {
            self.prune_locked(now_ms());
        }
    }

    /// [`Board::prune`], with the board's lock held.
    fn prune_locked(&self, now_ms: u64) {
        for row in self.scan().rows {
            let last = row
                .heard_ms
                .unwrap_or(0)
                .max(row.started_ms)
                .max(row.accept_ms.unwrap_or(0));
            let age = now_ms.saturating_sub(last);
            if (!row.is_open() && age >= millis(CLOSED_KEPT)) || age >= millis(UNHEARD_KEPT) {
                self.remove(&row.sid);
            }
        }
    }

    #[cfg(unix)]
    fn remove(&self, sid: &str) {
        let _ = aterm_uds::private_file::remove_private(&self.dir, sid);
    }

    #[cfg(not(unix))]
    fn remove(&self, _sid: &str) {}

    #[cfg(unix)]
    fn lock(&self) -> io::Result<aterm_uds::private_file::PrivateLock> {
        self.ensure()?;
        aterm_uds::private_file::PrivateLock::take(
            &self.dir,
            LOCK,
            LOCK_WITHIN,
            "another writer held the board's lock",
        )
    }

    #[cfg(not(unix))]
    fn lock(&self) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "this platform keeps no board",
        ))
    }

    #[cfg(unix)]
    fn ensure(&self) -> io::Result<()> {
        if let Some(state) = self.dir.parent()
            && !state.as_os_str().is_empty()
            && !state.exists()
        {
            aterm_uds::private_file::ensure_private_dir(state)?;
        }
        aterm_uds::private_file::ensure_private_dir(&self.dir).map(|_| ())
    }

    #[cfg(unix)]
    fn write(&self, row: &Row) -> io::Result<()> {
        aterm_uds::private_file::write_private(
            &self.dir,
            &row.sid,
            format!("{}\n", row.line()).as_bytes(),
        )
    }

    #[cfg(not(unix))]
    fn write(&self, _row: &Row) -> io::Result<()> {
        self.lock()
    }

    /// Take the keeper's lock without waiting: `None` where another keeper of this board holds
    /// it, or the board's folder cannot be made private.
    #[cfg(unix)]
    #[must_use]
    pub fn keeper_lock(&self) -> Option<aterm_uds::private_file::PrivateLock> {
        self.ensure().ok()?;
        aterm_uds::private_file::PrivateLock::take(
            &self.dir,
            KEEPER_LOCK,
            Duration::ZERO,
            "another aterm keeps this board",
        )
        .ok()
    }
}

/// The wait one leg of the keeper asks a Mac for ([`keep`]).
#[cfg(unix)]
const KEEP_LEG: Duration = Duration::from_secs(20);

/// How often a keeper with nothing to wait on reads its board again.
#[cfg(unix)]
const KEEP_IDLE: Duration = Duration::from_secs(5);

/// How often a keeper that found another keeping its board asks for the board again.
#[cfg(unix)]
const KEEP_ELSEWHERE: Duration = Duration::from_secs(30);

/// The longest a keeper waits before it asks a Mac that did not answer again.
#[cfg(unix)]
const KEEP_BACKOFF_MAX: Duration = Duration::from_secs(600);

/// How often a keeper prunes its board.
#[cfg(unix)]
const KEEP_PRUNE_EVERY: Duration = Duration::from_secs(3600);

/// How long after a row closed the keeper still asks after it: a worker read as gone while its
/// Mac restarted answers again once its tab is restored, and its row opens again.
#[cfg(unix)]
const CLOSED_ASKED: Duration = Duration::from_secs(24 * 3600);

/// How often the keeper asks after a closed row ([`CLOSED_ASKED`]).
#[cfg(unix)]
const CLOSED_ASKED_EVERY: Duration = Duration::from_secs(600);

/// What a keeper needs of the aterm it runs in ([`keep`]).
pub struct Keeper<'a> {
    /// The drive dial to another Mac.
    pub drive: Option<crate::FleetDriveFn>,
    /// This Mac's instance socket, for a row `node=-` its socket folder names no instance for.
    pub here: &'a dyn Fn() -> Option<String>,
    /// This Mac's fleet node id, whose rows are this Mac's.
    pub own_node: &'a dyn Fn() -> Option<String>,
}

/// The keeper's wait at each Mac that did not answer: its failures count until it answers, so
/// its wait doubles from twice a leg to [`KEEP_BACKOFF_MAX`].
#[cfg(unix)]
#[derive(Debug, Default)]
struct Backoff(std::collections::HashMap<String, (u32, std::time::Instant)>);

#[cfg(unix)]
impl Backoff {
    /// Whether `node` is asked at `at`: it never failed, or its wait is over.
    fn asks(&self, node: &str, at: std::time::Instant) -> bool {
        self.0.get(node).is_none_or(|(_, until)| *until <= at)
    }

    fn answered(&mut self, node: &str) {
        self.0.remove(node);
    }

    /// `node` did not answer at `now`: how long it is not asked.
    fn unanswered(&mut self, node: String, now: std::time::Instant) -> Duration {
        let fails = self
            .0
            .get(&node)
            .map_or(0, |(fails, _)| *fails)
            .saturating_add(1);
        let wait = KEEP_LEG
            .saturating_mul(1 << fails.min(5))
            .min(KEEP_BACKOFF_MAX);
        self.0.insert(node, (fails, now + wait));
        wait
    }

    /// Forget each Mac `on_board` no longer names.
    fn keep(&mut self, on_board: impl Fn(&str) -> bool) {
        self.0.retain(|node, _| on_board(node));
    }
}

/// Keep the board of the state folder `state` current for the life of the process: while it holds the board's keeper lock (one keeper per board, whichever aterm took it first; the
/// others ask for it again every [`KEEP_ELSEWHERE`]), wait on every open row its Macs were
/// heard of within [`UNHEARD_ASKED`] and every row closed within [`CLOSED_ASKED`] (every
/// [`CLOSED_ASKED_EVERY`]), one round of legs ([`crate::await_task`]'s keeper round) after
/// another, and merge what they read. Each row's leg starts from the newest revision read of
/// it, so news the board could not write is not read again at once. A Mac that does not answer
/// is asked again later, backing off to [`KEEP_BACKOFF_MAX`]. Between rounds it sleeps the whole
/// period, a thread asleep until it is due: nothing ends a keeper but its process, so there is no
/// stop to look at (ATERM-KI-1105-20: a look every 250 ms was 80 wakes in 20 s at rest).
pub fn keep(state: &Path, keeper: &Keeper<'_>) {
    #[cfg(unix)]
    {
        use std::collections::HashMap;
        use std::time::Instant;
        let board = Board::in_state(state);
        let mut held = None;
        let mut backoff = Backoff::default();
        let mut edges: HashMap<String, u64> = HashMap::new();
        let mut closed_asked: HashMap<String, Instant> = HashMap::new();
        let mut pruned: Option<Instant> = None;
        loop {
            if held.is_none() {
                held = board.keeper_lock();
                if held.is_none() {
                    std::thread::sleep(KEEP_ELSEWHERE);
                    continue;
                }
            }
            if pruned.is_none_or(|at| at.elapsed() >= KEEP_PRUNE_EVERY) {
                board.prune();
                pruned = Some(Instant::now());
            }
            let now = now_ms();
            let at = Instant::now();
            closed_asked.retain(|_, next| *next > at);
            let rows = board.scan().rows;
            edges.retain(|sid, _| rows.iter().any(|row| row.sid == *sid));
            backoff.keep(|node| rows.iter().any(|row| row.node.as_deref() == Some(node)));
            let mut asked: Vec<Row> = rows
                .into_iter()
                .filter(|row| {
                    let heard = row.heard_ms.unwrap_or(0).max(row.started_ms);
                    let since = now.saturating_sub(heard);
                    if row.is_open() {
                        since < millis(UNHEARD_ASKED)
                    } else {
                        since < millis(CLOSED_ASKED) && !closed_asked.contains_key(&row.sid)
                    }
                })
                .filter(|row| {
                    row.node
                        .as_deref()
                        .is_none_or(|node| backoff.asks(node, at))
                })
                .collect();
            if asked.is_empty() {
                std::thread::sleep(KEEP_IDLE);
                continue;
            }
            for row in &mut asked {
                if !row.is_open() {
                    closed_asked.insert(row.sid.clone(), at + CLOSED_ASKED_EVERY);
                }
                if let Some(&edge) = edges.get(&row.sid) {
                    row.rev = row.rev.max(edge);
                }
            }
            let own = (keeper.own_node)();
            let round = crate::await_task::keep_round(
                &asked,
                keeper.drive,
                ((keeper.here)().as_deref(), own.as_deref()),
                KEEP_LEG,
            );
            let changed = board.apply(&round.seen, now_ms()).unwrap_or(0);
            for row in &round.seen {
                let Some(sid) = seen_sid(row) else { continue };
                if let Some(rev) = crate::await_task::row_field(row, "rev").and_then(parse_u64) {
                    let edge = edges.entry(sid.to_owned()).or_insert(0);
                    *edge = (*edge).max(rev);
                }
                if let Some(node) = asked
                    .iter()
                    .find(|r| r.sid == sid)
                    .and_then(|row| row.node.as_ref())
                {
                    backoff.answered(node);
                }
            }
            for node in round.unanswered {
                backoff.unanswered(node, Instant::now());
            }
            // A round no Mac waited in (every leg answered or refused at once) waits before the
            // next.
            if changed == 0 && at.elapsed() < Duration::from_secs(1) {
                std::thread::sleep(KEEP_IDLE);
            }
        }
    }
    #[cfg(not(unix))]
    let _ = (state, keeper);
}

/// The header row of a read of a board: this Mac's host, its rows, those shown, those open, and
/// those it could not read.
#[must_use]
pub fn header(scan: &Scan, shown: usize, host: Option<&str>) -> String {
    format!(
        "board host={} rows={} shown={shown} open={} unread={}",
        host.map_or_else(|| "-".to_owned(), encode),
        scan.rows.len(),
        scan.rows.iter().filter(|row| row.is_open()).count(),
        scan.unread
    )
}

/// The rows a read of `scan` shows, oldest first: every open row before any closed one (a resume
/// waits on the open ones), the newest first of each, at most [`SHOWN_MAX`] within
/// [`SHOWN_MAX_BYTES`].
#[must_use]
pub fn shown(scan: &Scan) -> Vec<&Row> {
    let mut bytes = 0;
    let mut rows: Vec<&Row> = Vec::new();
    let newest_open_first = scan
        .rows
        .iter()
        .rev()
        .filter(|row| row.is_open())
        .chain(scan.rows.iter().rev().filter(|row| !row.is_open()));
    for row in newest_open_first.take(SHOWN_MAX) {
        bytes += row.line().len() + "worker \n".len();
        if bytes > SHOWN_MAX_BYTES {
            break;
        }
        rows.push(row);
    }
    rows.sort_by(|a, b| (a.started_ms, &a.sid).cmp(&(b.started_ms, &b.sid)));
    rows
}

/// Which rows a read or a wait takes: those a dispatcher session started (`from=<sid>`), and
/// those of a label (`label=<word>`, or `label=<prefix>*`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filter {
    /// The starting session's sid, as the row's `from=` names it.
    pub from: Option<String>,
    /// A label, or a label's prefix and `*`.
    pub label: Option<String>,
}

impl Filter {
    /// Take `from=` and `label=` off `words`: the words left, and the filter.
    /// # Errors
    /// Either named twice, or a value that is no word (`from=` no sid).
    pub fn take(words: &[String]) -> io::Result<(Vec<String>, Self)> {
        let invalid = |why: String| io::Error::new(io::ErrorKind::InvalidInput, why);
        let mut filter = Self::default();
        let mut kept = Vec::with_capacity(words.len());
        for word in words {
            if let Some(from) = word.strip_prefix("from=") {
                let from = from.trim_start_matches('@');
                if !is_sid(from) || filter.from.replace(from.to_owned()).is_some() {
                    return Err(invalid(format!(
                        "board: from= names one starting session's sid, not `{}`",
                        one_line(word)
                    )));
                }
            } else if let Some(label) = word.strip_prefix("label=") {
                let bare = label.strip_suffix('*').unwrap_or(label);
                if !valid_name(bare) || filter.label.replace(label.to_owned()).is_some() {
                    return Err(invalid(format!(
                        "board: label= names one label, or a prefix and `*`, not `{}`",
                        one_line(word)
                    )));
                }
            } else {
                kept.push(word.clone());
            }
        }
        Ok((kept, filter))
    }
}

/// A read of `scan` as a control reply: `OK <n>`, the [`header`] row, then a `worker <row>` row
/// per row shown ([`shown`]); `host` is this Mac's.
#[must_use]
pub fn reply(scan: &Scan, host: Option<&str>) -> String {
    let rows = shown(scan);
    let mut out = format!(
        "OK {}\n{}\n",
        rows.len() + 1,
        header(scan, rows.len(), host)
    );
    for row in rows {
        out.push_str("worker ");
        out.push_str(&row.line());
        out.push('\n');
    }
    out
}

/// The reply of the board of `state` (the server's `board`), with this Mac's host name.
#[must_use]
pub fn serve(state: &Path) -> String {
    reply(&Board::in_state(state).scan(), host_name().as_deref())
}

/// A read of a board, as a reply's rows give it ([`reply`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Read {
    /// The header row's words.
    pub header: Vec<(String, String)>,
    /// The rows.
    pub rows: Vec<Row>,
}

impl Read {
    /// The board's own Mac, as its header names it.
    #[must_use]
    pub fn host(&self) -> Option<&str> {
        self.header
            .iter()
            .find(|(key, _)| key == "host")
            .map(|(_, value)| value.as_str())
            .filter(|host| *host != "-")
    }
}

/// Read a reply's rows ([`reply`]): a `board …` header, then `worker …` rows.
/// # Errors
/// The first row is no `board` header, or a row is no `worker` row a board writes.
pub fn parse_reply(lines: &[String]) -> Result<Read, String> {
    let mut lines = lines.iter();
    let head = lines
        .next()
        .and_then(|line| line.strip_prefix("board "))
        .ok_or("the reply names no board")?;
    // A key that is no word is no header field: it would reach a terminal as sent.
    let header = head
        .split_whitespace()
        .filter_map(|word| word.split_once('='))
        .filter(|(key, _)| valid_word(key))
        .map(|(key, value)| (key.to_owned(), decode(value)))
        .collect();
    let rows = lines
        .map(|line| {
            line.strip_prefix("worker ")
                .ok_or_else(|| format!("`{}` is no worker row", one_line(line)))
                .and_then(Row::parse)
        })
        .collect::<Result<_, _>>()?;
    Ok(Read { header, rows })
}

/// A read of a board as one line of JSON: `{"board":{…},"workers":[{…},…]}`, every value a
/// string as the rows carry it (`-` for none), decoded.
#[must_use]
pub fn json_of(read: &Read) -> String {
    let object = |pairs: &[(String, String)]| {
        let members: Vec<String> = pairs
            .iter()
            .map(|(k, v)| {
                format!(
                    "\"{}\":\"{}\"",
                    aterm_codec::json::escape(k),
                    aterm_codec::json::escape(v)
                )
            })
            .collect();
        format!("{{{}}}", members.join(","))
    };
    let workers: Vec<String> = read
        .rows
        .iter()
        .map(|row| {
            let pairs: Vec<(String, String)> = row
                .line()
                .split_whitespace()
                .filter_map(|word| word.split_once('='))
                .map(|(k, v)| (k.to_owned(), decode(v)))
                .collect();
            object(&pairs)
        })
        .collect();
    format!(
        "{{\"board\":{},\"workers\":[{}]}}\n",
        object(&read.header),
        workers.join(",")
    )
}

/// Whether `parts` (a request's words) read or mark a board: `board …`, with no selector.
#[must_use]
pub(crate) fn is_board_verb(parts: &[String]) -> bool {
    parts.first().is_some_and(|word| word == "board")
}

/// How `board` is used.
const USAGE: &str = "aterm ctl [--host <h>] board [--json] [from=<sid>] [label=<word>|<prefix>*] \
                     | aterm ctl board accept <@sid|label> <word>";

/// Where a board read goes: another Mac's host, the call's `--timeout`, and the drive dial.
pub(crate) type Remote<'a> = (&'a str, Option<Option<Duration>>, crate::FleetDriveFn);

/// `aterm ctl [--host <h>] board [--json]` and `aterm ctl board accept <@sid|label> <word>`:
/// read this Mac's board from its state folder (or `remote`'s over a drive) and print it, a
/// header row then a `worker` row per worker, or one line of JSON; or record an accept word on
/// a row of this Mac's.
/// # Errors
/// A form `board` does not take, an accept over a drive, a board this Mac keeps none of, or as
/// [`crate::remote::read_board`] and [`Board::accept`].
pub(crate) fn run_read(parts: &[String], remote: Option<Remote<'_>>) -> io::Result<ExitCode> {
    let usage = |why: &str| io::Error::new(io::ErrorKind::InvalidInput, format!("{why}: {USAGE}"));
    let (words, filter) = Filter::take(&parts[1..])?;
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    let json = match words.as_slice() {
        [] => false,
        ["--json"] => true,
        ["accept", who, word] => {
            if filter != Filter::default() {
                return Err(usage("board accept names its row itself"));
            }
            if remote.is_some() {
                return Err(usage(
                    "board accept records on this Mac's own board, never over a drive",
                ));
            }
            let board = Board::here().ok_or_else(|| {
                io::Error::new(io::ErrorKind::Unsupported, "this Mac keeps no board")
            })?;
            let row = board.accept(who, word, now_ms())?;
            crate::print_stdout_line(&format!("OK board accept @{} accept={word}", row.sid))?;
            return Ok(ExitCode::SUCCESS);
        }
        ["accept", ..] => return Err(usage("board accept names a worker and a word")),
        _ => return Err(usage("board takes --json or accept")),
    };
    let mut read = match remote {
        Some((host, total, drive)) => crate::remote::read_board(host, total, drive)?,
        None => {
            let scan = Board::here().map(|board| board.scan()).unwrap_or_default();
            let text = reply(&scan, host_name().as_deref());
            let lines: Vec<String> = text.lines().skip(1).map(str::to_owned).collect();
            parse_reply(&lines).map_err(io::Error::other)?
        }
    };
    read.rows.retain(|row| row.matches(&filter));
    let out = if json {
        json_of(&read)
    } else {
        lines_of(&read)
    };
    // Another Mac's board is its text: shown to a terminal as ctl shows any peer's.
    let stdout = crate::stdout_handle();
    let mut sink = crate::StdoutSink::new(stdout.lock());
    io::Write::write_all(&mut sink, out.as_bytes())?;
    io::Write::flush(&mut sink)?;
    Ok(ExitCode::SUCCESS)
}

/// A read of a board as its rows: the header row, then a `worker` row per worker.
#[must_use]
pub fn lines_of(read: &Read) -> String {
    let header: Vec<String> = read
        .header
        .iter()
        .map(|(key, value)| {
            format!(
                "{key}={}",
                if value == "-" {
                    "-".to_owned()
                } else {
                    encode(value)
                }
            )
        })
        .collect();
    let mut out = format!("board {}\n", header.join(" "));
    for row in &read.rows {
        out.push_str("worker ");
        out.push_str(&row.line());
        out.push('\n');
    }
    out
}

/// Whether `word` is a row's key or state word: 1-64 of `A-Z a-z 0-9 . _ -`.
#[must_use]
pub fn valid_word(word: &str) -> bool {
    (1..=WORD_MAX).contains(&word.len())
        && word
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Whether `word` is a label or an accept word: a [`valid_word`] other than `-`, which a row
/// reads as none.
#[must_use]
pub fn valid_name(word: &str) -> bool {
    valid_word(word) && word != "-"
}

/// Take a spawn's `label=` off its words (`args`, after `spawn`): the words that go to the
/// worker's Mac, and the label. The label is the dispatcher's alone, so it never crosses the
/// wire, and a Mac that does not know the word never sees it.
/// # Errors
/// More than one `label=`, or one that is no [`valid_name`].
pub(crate) fn take_label(args: &[String]) -> io::Result<(Vec<String>, Option<String>)> {
    let mut label = None;
    let mut kept = Vec::with_capacity(args.len());
    for arg in args {
        let Some(word) = arg.strip_prefix("label=") else {
            kept.push(arg.clone());
            continue;
        };
        let word = word
            .strip_prefix('"')
            .and_then(|w| w.strip_suffix('"'))
            .unwrap_or(word);
        if label.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "spawn: label= at most once",
            ));
        }
        if !valid_name(word) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "spawn: label= takes 1-{WORD_MAX} of A-Z a-z 0-9 . _ - (not `-` \
                     alone), not `{}`",
                    one_line(word)
                ),
            ));
        }
        label = Some(word.to_owned());
    }
    Ok((kept, label))
}

/// Where a worker a spawn started runs, as the board records it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Placed {
    /// The node id its Mac proved; `None` for this Mac.
    pub(crate) node: Option<String>,
    /// How a person names its Mac.
    pub(crate) host: Option<String>,
    /// The account row `--host any` chose its Mac by, masked.
    pub(crate) account: Option<String>,
}

/// The row of a worker a spawn started: `sid`, `placed` where, its `label`, the spawn's
/// words `args` (after `spawn`, `label=` taken off) and its task, started by `from` at `now_ms`.
pub(crate) fn spawned(
    sid: &str,
    placed: Placed,
    label: Option<String>,
    (args, task): (&[String], &[u8]),
    from: Option<&str>,
    now_ms: u64,
) -> Row {
    let arg = |key: &str| {
        args.iter()
            .rev()
            .find_map(|a| a.strip_prefix(key)?.strip_prefix('='))
            .map(|v| {
                v.strip_prefix('"')
                    .and_then(|v| v.strip_suffix('"'))
                    .unwrap_or(v)
                    .to_owned()
            })
            .filter(|v| !v.is_empty())
    };
    let from = arg("from").or_else(|| from.map(str::to_owned));
    Row {
        label,
        node: placed.node,
        host: placed.host,
        program: arg("agent").or_else(|| arg("program")),
        model: arg("model"),
        account: placed.account,
        // A program worker runs under no Autopilot: no harness of its reads it.
        autopilot: arg("autopilot").or_else(|| arg("agent").map(|_| "land".to_owned())),
        cwd: arg("cwd"),
        brief: Some(brief_digest(task)),
        brief_bytes: task.len() as u64,
        from,
        ..Row::started(sid, now_ms)
    }
}

/// Record the row of a worker a spawn started on this Mac's board, saying on stderr why where
/// it could not: the spawn itself stands.
pub(crate) fn record_spawned(row: &Row) {
    let Some(board) = Board::here() else {
        return;
    };
    if let Err(e) = board.record(row) {
        let _ = crate::stderr_line(&format!("the board did not record @{}: {e}", row.sid));
    }
}

/// Merge each row a wait read (`await task`'s rows) into this Mac's board, for the workers it
/// holds and `writes` lets it change; a board it cannot write is left as it was.
pub(crate) fn apply_seen(rows: &[String], writes: Writes) {
    if writes == Writes::Nothing {
        return;
    }
    let Some(board) = Board::here() else {
        return;
    };
    let live: Vec<String> = rows
        .iter()
        .filter(|row| !row.split_whitespace().any(|w| w == "read=board"))
        .filter(|row| {
            writes == Writes::Rows
                || seen_sid(row)
                    .and_then(|sid| board.row(sid))
                    .is_some_and(|known| known.node.is_none())
        })
        .cloned()
        .collect();
    let _ = board.apply(&live, now_ms());
}

/// Which of this Mac's board rows a wait's answers may change: only the Mac a row says its
/// worker runs on answers for it, so a Mac a wait drives writes nothing about another's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Writes {
    /// None: a wait at a Mac a person named (`--host <h>`), or on another Mac's board, whose
    /// answers this board cannot bind to its rows. The keeper keeps those rows.
    Nothing,
    /// The rows of workers on this Mac (`node=-`): a wait over this Mac's own socket.
    Here,
    /// Every row the wait answered: a wait that asked each worker at the Mac its row on this
    /// board names, and took only an answer naming the sid it asked.
    Rows,
}

/// `sha256:<64 hex>` of a task's bytes.
#[must_use]
pub fn brief_digest(task: &[u8]) -> String {
    let digest = aterm_digest::Sha256::digest(task);
    let mut out = String::with_capacity(7 + 64);
    out.push_str("sha256:");
    aterm_codec::hex::push_lower(&mut out, &digest);
    out
}

/// The sid an `await task` row (`@<sid> …`) is of.
fn seen_sid(row: &str) -> Option<&str> {
    row.split_whitespace()
        .next()?
        .strip_prefix('@')
        .filter(|sid| is_sid(sid))
}

/// Whether `word` is a global session id: `s-` and 1-62 hex digits.
#[must_use]
pub fn is_sid(word: &str) -> bool {
    word.len() <= 64
        && word
            .strip_prefix("s-")
            .is_some_and(|hex| !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// This Mac's host name, as a board names its own Mac.
#[must_use]
pub fn host_name() -> Option<String> {
    aterm_uds::process::host_name()
}

/// Now, in Unix milliseconds on this Mac's clock.
#[must_use]
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

fn parse_u64(v: &str) -> Option<u64> {
    (!v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
        .then(|| v.parse().ok())
        .flatten()
}

fn encode(s: &str) -> String {
    aterm_codec::pct::encode(s)
}

fn decode(s: &str) -> String {
    aterm_codec::pct::decode(s)
}

fn one_line(s: &str) -> String {
    aterm_codec::tty::one_line(s, 40)
}

#[cfg(test)]
#[path = "board_tests.rs"]
mod tests;
