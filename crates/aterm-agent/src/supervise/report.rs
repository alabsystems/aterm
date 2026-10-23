// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The worker's report, as `aterm drive ledger` and the loop's report count read it.

use std::fmt;

use aterm_phase::{Program, ScreenReader};

use super::blocks::{View, view_rows};
use super::phase::transcript_end;
use super::run::{Ctl, CtlReply, Fail, Session};

/// [`ReportOpts::max_rows`]' default.
pub const DEFAULT_MAX_ROWS: usize = 8000;
/// How many `history` records a report reads first to find the newest
/// SUBMITTED turn; when they are all the window harness's own or unsubmitted
/// it reads the whole ledger ([`Session::ledger_turn`]).
const HISTORY_DEPTH: usize = 8;
/// How many characters of the turn's text (whitespace aside) its `❯` row must begin with.
pub const MARKER_CHARS: usize = 60;
/// The caret Claude Code's user rows open with, and any reader's without one.
const CLAUDE_CARET: char = '❯';
/// How Claude Code shows a long paste in its transcript.
pub(super) const PASTED: &str = "[Pasted text";
/// A single-line turn at least this long (chars) may be shown as a paste.
const PASTE_CHARS: usize = 200;
/// The ledger keeps up to 512 bytes of a turn's text: one this long may have
/// been cut, so its newlines are a lower bound of the paste's.
const LEDGER_CUT_BYTES: usize = 500;

/// A position in a session's alt-screen archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mark {
    /// The archive's host-assigned origin (unique per aterm process that started the archive, and kept by a self-update that carries it).
    pub origin: Option<u64>,
    /// The archived row index (0 = before the first row).
    pub index: u64,
}

impl Mark {
    /// `<origin>:<index>` or `<index>`, decimal digits only.
    pub fn parse(s: &str) -> Option<Self> {
        match s.split_once(':') {
            Some((origin, index)) => Some(Self {
                origin: Some(decimal(origin)?),
                index: decimal(index)?,
            }),
            None => Some(Self {
                origin: None,
                index: decimal(s)?,
            }),
        }
    }
}

impl fmt::Display for Mark {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.origin {
            Some(origin) => write!(f, "{origin}:{}", self.index),
            None => write!(f, "{}", self.index),
        }
    }
}

/// ASCII digits and nothing else (no sign, no space).
pub(super) fn decimal(s: &str) -> Option<u64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// `report`'s knobs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReportOpts {
    /// Start right after this archived row, not at your turn.
    pub since: Option<Mark>,
    /// The most archived rows read.
    pub max_rows: usize,
    /// Which of the rows are printed ([`Report::render_view`]).
    pub view: View,
}

impl Default for ReportOpts {
    fn default() -> Self {
        Self {
            since: None,
            max_rows: DEFAULT_MAX_ROWS,
            view: View::All,
        }
    }
}

/// Where a report starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Marker {
    /// Your newest submitted `turn`.
    Ledger,
    /// No turn in the ledger.
    UserRow,
    /// `--since`: right after that archived row.
    Since,
}

impl Marker {
    pub const fn name(self) -> &'static str {
        match self {
            Marker::Ledger => "ledger",
            Marker::UserRow => "user-row",
            Marker::Since => "since",
        }
    }
}

/// Why a report is not complete, in the order the header lists them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Reason {
    /// The host has no `offscreen`, its archive is off (`enabled=0`), or the client relays no header.
    NoArchive,
    /// The worker is not on the alternate screen.
    MainScreen,
    /// The archive is not the one the mark named.
    ArchiveReset,
    /// Rows after the start were evicted, or a gap (a redraw with no overlap, a resize, a reset) lies after it.
    ArchiveGap,
    /// More archived rows than `--max-rows`.
    MaxRows,
    /// Your turn's `❯` row was not found (nor a last `❯` row).
    MarkerNotFound,
}

impl Reason {
    pub const fn name(self) -> &'static str {
        match self {
            Reason::NoArchive => "no-archive",
            Reason::MainScreen => "main-screen",
            Reason::ArchiveReset => "archive-reset",
            Reason::ArchiveGap => "archive-gap",
            Reason::MaxRows => "max-rows",
            Reason::MarkerNotFound => "marker-not-found",
        }
    }
}

/// What the worker said since your turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// Why it is not complete.
    pub reasons: Vec<Reason>,
    pub marker: Marker,
    /// The ledger turn it starts from (`marker=ledger`).
    pub turn: Option<u64>,
    /// The rows, from the start to where the transcript ends, verbatim.
    pub rows: Vec<String>,
    /// How many of `rows`, from the top, came from the archive.
    pub archived: usize,
    /// How many of `rows`, after those, came from the screen.
    pub screen: usize,
    /// The newest archived row read, to hand to `--since` next time (`None` without an archive).
    pub last: Option<Mark>,
}

impl Report {
    /// Nothing was lost between the start and the end.
    pub const fn complete(&self) -> bool {
        self.reasons.is_empty()
    }

    /// `report complete=<0|1> [reason=<r>[,<r>…]] marker=<m> turn=<id|-> rows=<n> archived=<a> screen=<s> last=<origin:i|->`.
    pub fn header(&self) -> String {
        let mut out = format!("report complete={}", u8::from(self.complete()));
        if !self.reasons.is_empty() {
            let names: Vec<&str> = self.reasons.iter().map(|r| r.name()).collect();
            out.push_str(&format!(" reason={}", names.join(",")));
        }
        out.push_str(&format!(
            " marker={} turn={} rows={} archived={} screen={} last={}",
            self.marker.name(),
            self.turn.map_or_else(|| "-".to_string(), |t| t.to_string()),
            self.rows.len(),
            self.archived,
            self.screen,
            self.last.map_or_else(|| "-".to_string(), |m| m.to_string()),
        ));
        out
    }

    /// The header, a `--` line, then the rows, one per line.
    pub fn render(&self) -> String {
        let mut out = self.header();
        out.push_str("\n--\n");
        for row in &self.rows {
            out.push_str(row);
            out.push('\n');
        }
        out
    }

    /// [`Self::render`] for `view`.
    pub fn render_view(&self, view: View) -> String {
        if view == View::All {
            return self.render();
        }
        let kept = view_rows(&self.rows, view);
        let mut out = format!("{} view={} kept={}", self.header(), view.name(), kept.len());
        out.push_str("\n--\n");
        for row in &kept {
            out.push_str(row);
            out.push('\n');
        }
        out
    }
}

/// One `history` record, as a report reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerTurn {
    pub id: u64,
    /// The submit verifiably landed.
    pub submitted: bool,
    /// Who typed it (`by=`: `harness`, `ctl`, `bridge`, a sid, `-`).
    pub by: String,
    /// Where the archive stood when the turn started (a host that predates the archive prints none).
    pub arch: Option<Mark>,
    /// What was typed (pct-decoded; the ledger keeps up to 512 bytes).
    pub text: String,
}

/// The records of a `history` reply (`turn <id> submitted=<0|1> … arch=<o>:<l> text=<pct>` per line).
pub fn parse_history(body: &str) -> Vec<LedgerTurn> {
    body.lines().filter_map(parse_turn_line).collect()
}

fn parse_turn_line(line: &str) -> Option<LedgerTurn> {
    let rest = line.strip_prefix("turn ")?;
    let (head, text) = rest.split_once(" text=").unwrap_or((rest, ""));
    let mut words = head.split(' ');
    let id = decimal(words.next()?)?;
    let mut turn = LedgerTurn {
        id,
        submitted: false,
        by: "-".to_string(),
        arch: None,
        text: pct_decode(text),
    };
    for word in words {
        match word.split_once('=') {
            Some(("submitted", v)) => turn.submitted = v == "1",
            Some(("by", v)) => turn.by = pct_decode(v),
            Some(("arch", v)) => turn.arch = Mark::parse(v),
            _ => {}
        }
    }
    Some(turn)
}

/// The turn a report starts from: the newest one whose submit landed, else
/// the newest (a turn that never landed may sit in the composer; its mark
/// still says where the archive stood when it was typed).
fn newest_turn(turns: Vec<LedgerTurn>) -> Option<LedgerTurn> {
    let mut turns: Vec<LedgerTurn> = turns.into_iter().filter(|t| t.by != "harness").collect();
    let at = turns
        .iter()
        .rposition(|t| t.submitted)
        .or_else(|| turns.len().checked_sub(1))?;
    Some(turns.swap_remove(at))
}

/// Decode the control protocol's `%XX` escape, under the name this crate's parsers import.
pub(crate) use aterm_codec::pct::decode as pct_decode;

/// One `offscreen … screen=1` reply.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Offscreen {
    /// Index of `archived[0]` (`last + 1` when nothing came).
    pub first: u64,
    /// The newest archived index at the read.
    pub last: u64,
    /// Rows after `since` evicted before the read.
    pub lost: u64,
    /// Gaps with `after >= since`.
    pub breaks: u64,
    /// How many rows the screen shows again from the archive.
    pub back: usize,
    pub back_at: u64,
    pub pin: usize,
    pub origin: u64,
    /// The host's archive is recording (`enabled=0` says it is off: nothing that scrolled away was kept).
    pub enabled: bool,
    /// The worker is on the alternate screen.
    pub alt: bool,
    /// More archived rows than the page held.
    pub more: bool,
    /// The screen half caught the program's DEC 2026 frame unfinished (`frame=open`, read under the rows' lock, as `text --json`'s `"frame":"open"`).
    pub frame_open: bool,
    pub archived: Vec<String>,
    pub screen: Vec<String>,
}

/// Why an `offscreen` reply could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OffscreenError {
    /// Missing `OK <n> first=… last=…` header, or header alone with its rows dropped.
    NoHeader,
    /// A header that does not describe the rows that came.
    Bad(String),
}

/// Read an `offscreen` reply.
pub fn parse_offscreen(reply: &CtlReply) -> Result<Offscreen, OffscreenError> {
    let stdout: Vec<&str> = reply.stdout.lines().collect();
    let from_stderr = reply.stderr.lines().find_map(|l| {
        let l = l.trim();
        header(l.strip_prefix("aterm-ctl:").unwrap_or(l))
    });
    let header_on_stderr = from_stderr.is_some();
    let (head, payload) = match from_stderr {
        Some(h) => (h, &stdout[..]),
        None => match stdout.first().and_then(|l| header(l)) {
            Some(h) => (h, &stdout[1..]),
            None => return Err(OffscreenError::NoHeader),
        },
    };
    let (n, fields) = head;
    let bad = |what: String| OffscreenError::Bad(what);
    let field = |key: &str| -> Result<u64, OffscreenError> {
        fields
            .iter()
            .find(|(k, _)| *k == key)
            .and_then(|(_, v)| decimal(v))
            .ok_or_else(|| bad(format!("no {key}= in the header")))
    };
    let optional = |key: &str| -> Result<u64, OffscreenError> {
        if fields.iter().any(|(k, _)| *k == key) {
            field(key)
        } else {
            Ok(0)
        }
    };
    if payload.len() != n {
        // this reader, not malformed.
        if !header_on_stderr && payload.is_empty() {
            return Err(OffscreenError::NoHeader);
        }
        return Err(bad(format!(
            "the header counts {n} rows, {} came",
            payload.len()
        )));
    }
    let screen_rows = usize::try_from(optional("screen_rows")?).unwrap_or(usize::MAX);
    let Some(split) = n.checked_sub(screen_rows) else {
        return Err(bad(format!("screen_rows={screen_rows} is more than {n}")));
    };
    Ok(Offscreen {
        first: field("first")?,
        last: field("last")?,
        lost: field("lost")?,
        breaks: field("breaks")?,
        back: usize::try_from(field("back")?).unwrap_or(usize::MAX),
        back_at: optional("back_at")?,
        pin: usize::try_from(optional("pin")?).unwrap_or(usize::MAX),
        origin: field("origin")?,
        enabled: fields.iter().all(|(k, v)| *k != "enabled" || *v != "0"),
        alt: field("alt")? == 1,
        more: optional("more")? == 1,
        frame_open: fields.iter().any(|(k, v)| *k == "frame" && *v == "open"),
        archived: payload[..split].iter().map(|r| r.to_string()).collect(),
        screen: payload[split..].iter().map(|r| r.to_string()).collect(),
    })
}

/// `OK <n> key=value…` with a `last=` field: the count and the fields. Words
/// without `=` (`aterm ctl`'s `(offscreen: no results)` note) are ignored.
fn header(line: &str) -> Option<(usize, Vec<(&str, &str)>)> {
    let rest = line.trim().strip_prefix("OK ")?;
    let mut words = rest.split_whitespace();
    let n = usize::try_from(decimal(words.next()?)?).ok()?;
    let fields: Vec<(&str, &str)> = words.filter_map(|w| w.split_once('=')).collect();
    fields
        .iter()
        .any(|(k, _)| *k == "last")
        .then_some((n, fields))
}

/// The archived rows and the screen's transcript rows, joined at the seam.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct Joined {
    pub(super) rows: Vec<String>,
    /// How many of `rows`, from the top, came from the archive.
    pub(super) archived: usize,
    /// The archive index of `rows[0]` (when `archived > 0`).
    pub(super) first: u64,
    /// The newest archived index read (the page's `last`).
    pub(super) upto: u64,
}

/// Join an `offscreen … screen=1` read.
pub(super) fn join(off: &Offscreen, reader: &dyn ScreenReader) -> Joined {
    let count = u64::try_from(off.archived.len()).unwrap_or(u64::MAX);
    let upto = if count > 0 {
        off.first.saturating_add(count - 1)
    } else {
        off.last
    };
    let end = reader.transcript_end(&off.screen).min(off.screen.len());
    let got = |index: u64| count > 0 && index >= off.first && index <= upto;
    let reshown = |r: usize| {
        off.back_at > 0
            && r >= off.pin
            && r - off.pin < off.back
            && got(off
                .back_at
                .saturating_add(u64::try_from(r - off.pin).unwrap_or(u64::MAX)))
    };
    let mut rows = off.archived.clone();
    rows.extend(
        (0..end)
            .filter(|&r| !reshown(r))
            .map(|r| off.screen[r].clone()),
    );
    Joined {
        rows,
        archived: off.archived.len(),
        first: off.first,
        upto,
    }
}

/// A user row: `❯` in column 0, then whitespace.
pub(super) fn is_user_row(row: &str) -> bool {
    user_row(row, CLAUDE_CARET)
}

/// A user row of a program whose user rows open with `caret` ([`is_user_row`]
/// for Claude Code's).
fn user_row(row: &str, caret: char) -> bool {
    row.strip_prefix(caret)
        .is_some_and(|rest| rest.starts_with(char::is_whitespace))
}

/// The first `need` non-whitespace characters of the user block at `at`.
pub(super) fn user_text(rows: &[String], at: usize, need: usize) -> String {
    let mut chars = rows[at].chars();
    chars.next();
    let head = chars.as_str();
    let tail = rows[at + 1..]
        .iter()
        .map(String::as_str)
        .take_while(|r| r.is_empty() || r.starts_with(char::is_whitespace));
    std::iter::once(head)
        .chain(tail)
        .flat_map(str::chars)
        .filter(|c| !c.is_whitespace())
        .take(need)
        .collect()
}

/// Whether `text` may show as a `[Pasted text …]` row.
pub(super) fn paste_like(text: &str) -> bool {
    text.contains('\n') || text.chars().count() >= PASTE_CHARS
}

/// Whether the paste row `row` can be `text`.
pub(super) fn paste_fits(row: &str, text: &str) -> bool {
    let Some(n) = row
        .split_once(" +")
        .and_then(|(_, rest)| rest.split_once(" line"))
        .and_then(|(n, _)| decimal(n))
    else {
        return true;
    };
    let breaks = u64::try_from(text.matches('\n').count()).unwrap_or(u64::MAX);
    if text.len() >= LEDGER_CUT_BYTES {
        breaks <= n + 1
    } else {
        n.abs_diff(breaks) <= 1
    }
}

/// Where the report starts in `rows`: for the ledger, the last user row (one
/// that opens with `caret`) whose text begins with the turn's first
/// [`MARKER_CHARS`] characters (whitespace aside), else — for a paste-like
/// turn — the last `[Pasted text …]` row when it fits the text; without a
/// ledger turn, the last user row; after `--since`, the top.
fn find_start(rows: &[String], marker: Marker, text: Option<&str>, caret: char) -> Option<usize> {
    let users: Vec<usize> = (0..rows.len())
        .filter(|&i| user_row(&rows[i], caret))
        .collect();
    match marker {
        Marker::Since => Some(0),
        Marker::UserRow => users.last().copied(),
        Marker::Ledger => {
            let want: String = text
                .unwrap_or("")
                .chars()
                .filter(|c| !c.is_whitespace())
                .take(MARKER_CHARS)
                .collect();
            let need = want.chars().count();
            let by_text = if need == 0 {
                None
            } else {
                users
                    .iter()
                    .rev()
                    .copied()
                    .find(|&i| user_text(rows, i, need) == want)
            };
            let text = text.unwrap_or("");
            by_text.or_else(|| {
                paste_like(text).then_some(())?;
                let at = users.iter().rev().copied().find(|&i| {
                    rows[i]
                        .trim_start_matches(caret)
                        .trim_start()
                        .starts_with(PASTED)
                })?;
                paste_fits(rows[at].trim_start_matches(caret).trim_start(), text).then_some(at)
            })
        }
    }
}

/// The report of `rows` from `start`: its blank rows at either end dropped,
/// and counted by where they came from (the first `archived` of `rows` are
/// the archive's).
#[allow(
    clippy::too_many_arguments,
    reason = "every field of the report but its rows' own trim: a struct would only rename \
              the list"
)]
fn finish(
    marker: Marker,
    turn: Option<u64>,
    rows: &[String],
    archived: usize,
    start: usize,
    mut reasons: Vec<Reason>,
    last: Option<Mark>,
) -> Report {
    let start = (start..rows.len())
        .find(|&i| !rows[i].trim().is_empty())
        .unwrap_or(rows.len());
    let end = rows[start..]
        .iter()
        .rposition(|r| !r.trim().is_empty())
        .map_or(start, |i| start + i + 1);
    let out = rows[start..end].to_vec();
    let from_archive = archived.saturating_sub(start).min(out.len());
    reasons.sort_unstable();
    reasons.dedup();
    Report {
        reasons,
        marker,
        turn,
        screen: out.len() - from_archive,
        archived: from_archive,
        rows: out,
        last,
    }
}

/// A host without the archive: the screen's transcript alone.
fn screen_only(marker: Marker, turn: Option<u64>, text: Option<&str>, screen: &[String]) -> Report {
    let rows = &screen[..transcript_end(screen)];
    let start = find_start(rows, marker, text, CLAUDE_CARET);
    let mut reasons = vec![Reason::NoArchive];
    if start.is_none() {
        reasons.push(Reason::MarkerNotFound);
    }
    finish(marker, turn, rows, 0, start.unwrap_or(0), reasons, None)
}

/// What makes a report from one read incomplete, given where it starts; and,
/// when the read counted a gap after the mark and the start is an archived
/// row, that row's index — a second, one-row read places the gap against it.
fn assess(
    off: &Offscreen,
    joined: &Joined,
    marker: Marker,
    mark: Option<Mark>,
    start: Option<usize>,
) -> (Vec<Reason>, Option<u64>) {
    let mut reasons = Vec::new();
    if !off.enabled {
        reasons.push(Reason::NoArchive);
    }
    if !off.alt {
        reasons.push(Reason::MainScreen);
    }
    if mark
        .and_then(|m| m.origin)
        .is_some_and(|origin| origin != off.origin)
    {
        reasons.push(Reason::ArchiveReset);
    }
    let gaps = off.lost > 0 || off.breaks > 0;
    let mut recheck = None;
    match (marker, start) {
        (Marker::Since, _) => {
            if gaps {
                reasons.push(Reason::ArchiveGap);
            }
            if off.more {
                reasons.push(Reason::MaxRows);
            }
        }
        (_, None) => {
            if gaps {
                reasons.push(Reason::ArchiveGap);
            }
            if off.more {
                reasons.push(Reason::MaxRows);
            }
            reasons.push(Reason::MarkerNotFound);
        }
        (_, Some(s)) if s < joined.archived => {
            if off.breaks > 0 {
                recheck = Some(joined.first + u64::try_from(s).unwrap_or(u64::MAX));
            }
            if off.more && marker == Marker::Ledger {
                reasons.push(Reason::MaxRows);
            }
        }
        (_, Some(_)) => {}
    }
    (reasons, recheck)
}

impl<C: Ctl> Session<'_, C> {
    /// Worker output since the newest submitted turn (or [`ReportOpts`] `since`), joining archive and screen rows.
    pub fn report(&mut self, opts: &ReportOpts) -> Result<Report, String> {
        self.gather_report(opts).map_err(String::from)
    }

    /// [`Self::report`], a request not served told apart.
    pub(super) fn gather_report(&mut self, opts: &ReportOpts) -> Result<Report, Fail> {
        let (marker, turn, text, mark) = match opts.since {
            Some(since) => (Marker::Since, None, None, Some(since)),
            None => match self.ledger_turn()? {
                Some(t) => (Marker::Ledger, Some(t.id), Some(t.text), t.arch),
                None => (Marker::UserRow, None, None, None),
            },
        };
        let Some(off) = self.offscreen_snapshot(marker, mark, opts.max_rows.max(1))? else {
            let screen = self.full_screen()?;
            return Ok(screen_only(marker, turn, text.as_deref(), &screen.rows));
        };
        let joined = join(&off, Program::Claude.reader());
        let start = find_start(&joined.rows, marker, text.as_deref(), CLAUDE_CARET);
        let (mut reasons, recheck) = assess(&off, &joined, marker, mark, start);
        if let Some(k) = recheck
            && self.gap_after(off.origin, k)?
        {
            reasons.push(Reason::ArchiveGap);
        }
        let last = Mark {
            origin: Some(off.origin),
            index: joined.upto,
        };
        Ok(finish(
            marker,
            turn,
            &joined.rows,
            joined.archived,
            start.unwrap_or(0),
            reasons,
            Some(last),
        ))
    }

    /// The turn a report starts from ([`newest_turn`]); `None` when there is
    /// none, or the host has no `history`.
    fn ledger_turn(&mut self) -> Result<Option<LedgerTurn>, Fail> {
        let depth = HISTORY_DEPTH.to_string();
        let Some(near) = self.history_read(&["history", &depth])? else {
            return Ok(None);
        };
        if near.len() < HISTORY_DEPTH || near.iter().any(|t| t.submitted && t.by != "harness") {
            return Ok(newest_turn(near));
        }
        Ok(self.history_read(&["history"])?.and_then(newest_turn))
    }

    /// One `history` read's records; `None` from a host without the verb.
    fn history_read(&mut self, args: &[&str]) -> Result<Option<Vec<LedgerTurn>>, Fail> {
        let r = self.call(args)?;
        if r.ok() {
            return Ok(Some(parse_history(&r.stdout)));
        }
        if !self.unserved(&r) && without_verb(&r) {
            return Ok(None);
        }
        Err(self.fault(&r, format!("history failed: {}", r.stderr.trim())))
    }

    /// The one read: `offscreen since=<mark> max=<n> screen=1` — oldest first
    /// from the mark — or, with no mark to start from, the newest `<n>`
    /// (`tail=`).
    fn offscreen_snapshot(
        &mut self,
        marker: Marker,
        mark: Option<Mark>,
        max: usize,
    ) -> Result<Option<Offscreen>, Fail> {
        let max_arg = format!("max={max}");
        let from = match (marker, mark) {
            (Marker::UserRow, _) => Some(format!("tail={max}")),
            (_, Some(m)) => Some(format!("since={m}")),
            (_, None) => None,
        };
        let mut args = vec!["offscreen"];
        args.extend(from.as_deref());
        args.push(&max_arg);
        args.push("screen=1");
        self.whole_frame(|s| {
            let r = s.call(&args)?;
            if r.ok() {
                return match parse_offscreen(&r) {
                    Ok(off) => {
                        let open = off.frame_open;
                        Ok((Some(off), open))
                    }
                    Err(OffscreenError::NoHeader) => Ok((None, false)),
                    Err(OffscreenError::Bad(why)) => Err(Fail::Hard(format!("offscreen: {why}"))),
                };
            }
            if !s.unserved(&r) && without_verb(&r) {
                return Ok((None, false));
            }
            Err(s.fault(&r, format!("offscreen failed: {}", r.stderr.trim())))
        })
    }

    /// Whether a gap lies after archived row `k` (`offscreen since=<origin>:<k>
    /// max=1`).
    fn gap_after(&mut self, origin: u64, k: u64) -> Result<bool, Fail> {
        let since = format!("since={origin}:{k}");
        let r = self.call(&["offscreen", &since, "max=1"])?;
        if r.ok() {
            return Ok(parse_offscreen(&r).map_or(true, |o| o.breaks > 0 || o.origin != origin));
        }
        if self.unserved(&r) {
            return Err(Fail::Lost(format!("offscreen failed: {}", r.stderr.trim())));
        }
        Ok(true)
    }

    /// The whole screen (`text --json`, never `tail=`), a whole frame's
    /// ([`Self::text_json`]): a host without the archive reports from it alone.
    fn full_screen(&mut self) -> Result<super::screen::Screen, Fail> {
        self.text_json(&["text", "--json"])
    }
}

/// The host does not know the verb.
pub(super) fn without_verb(r: &CtlReply) -> bool {
    r.unknown_form() || r.is_err("unknown verb") || r.is_err("denied")
}

#[cfg(test)]
#[path = "report_tests.rs"]
mod tests;
