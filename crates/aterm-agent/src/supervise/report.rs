// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm drive report`: what the worker said since your turn, in full — not
//! just what is still on its screen. Claude Code runs on the ALTERNATE screen
//! and repaints in place, so a row that leaves the top of the screen is gone
//! from it: in one measured worker turn the worker displayed 35 message blocks
//! and 7 were still readable at the end. The host keeps those rows (the
//! alt-screen archive, read with `offscreen`), and every `turn` records where
//! the archive stood when it started (`history`'s `arch=<origin>:<last>`).
//! This joins the two.
//!
//! ONE read carries both halves: `offscreen since=<mark> screen=1` returns the
//! rows archived after the mark AND the screen, taken under the same lock, with
//! `back=` — how many rows the screen shows again from the archive (screen rows
//! `pin..pin+back` are archived rows `back_at..`). The join is the archived
//! rows, then the screen's rows down to where the transcript ends
//! ([`transcript_end`]: the status row and everything under it, found by
//! POSITION), less the re-shown rows this read already got. The rows are kept
//! verbatim: a done row, a table, a todo item or indented code is what the
//! worker said, and the filter behind "the last thing said"
//! ([`super::phase::last_said_index`]) would drop them.
//!
//! The report opens at YOUR turn's `❯` row: the last one whose text begins
//! with the first [`MARKER_CHARS`] characters of the ledger's text, compared
//! without whitespace so a wrap never matters — or, since Claude Code can show
//! a long paste as `[Pasted text #N +L lines]`, when the turn's text is a paste
//! (it has a newline, or is long), the last such row if its line count fits.
//! The rows above it are the tail of what the screen showed when the turn
//! began. Without a turn in the ledger (`aterm drive prompt` keeps none) it
//! opens at the last `❯` row; `--since <origin:i>` opens right after an
//! archived row instead.
//!
//! `complete=1` says nothing was lost between that start and the end: the
//! start was found, the archive is the one the mark named (a restart starts a
//! new one; an aterm self-update carries it, marks and turn ledger included,
//! unless it could not), the worker is on the alternate screen, no gap (a
//! redraw with no overlap, a resize, a reset) and no eviction lies after the
//! start, and `--max-rows` held every row. Otherwise `reason=` says why.
//! A host without `offscreen` — or whose archive is off (`enabled=0`), or an
//! `aterm ctl` too old to relay the rows — gets the screen alone
//! (`reason=no-archive`).

use std::fmt;

use super::blocks::{View, view_rows};
use super::phase::transcript_end;
use super::run::{Ctl, CtlReply, Fail, Session};
use super::screen::parse_text_json;

/// `--max-rows`' default: the most archived rows one report reads.
pub const DEFAULT_MAX_ROWS: usize = 8000;
/// How many `history` records a report reads to find the newest SUBMITTED turn.
const HISTORY_DEPTH: &str = "8";
/// How many characters of the turn's text (whitespace aside) its `❯` row must
/// begin with. The ledger keeps up to 512 bytes of it.
pub const MARKER_CHARS: usize = 60;
/// How Claude Code shows a long paste in its transcript.
pub(super) const PASTED: &str = "[Pasted text";
/// A single-line turn at least this long (chars) may be shown as a paste.
const PASTE_CHARS: usize = 200;
/// The ledger keeps up to 512 bytes of a turn's text: one this long may have
/// been cut, so its newlines are a lower bound of the paste's.
const LEDGER_CUT_BYTES: usize = 500;

/// A position in a session's alt-screen archive: `<origin>:<index>` as
/// `history`'s `arch=` and a report's `last=` print it and `offscreen since=`
/// takes it; a bare `<index>` names no origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mark {
    /// The archive's host-assigned origin (unique per aterm process that
    /// started the archive, and kept by a self-update that carries it).
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
    /// `--since`: start right after this archived row, not at your turn.
    pub since: Option<Mark>,
    /// `--max-rows`: the most archived rows read.
    pub max_rows: usize,
    /// `--final` / `--messages`: which of the rows are printed
    /// ([`Report::render_view`]); the rows read are the same.
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
    /// Your newest submitted `turn`: its `❯` row, found by its text.
    Ledger,
    /// No turn in the ledger: the last `❯` row.
    UserRow,
    /// `--since`: right after that archived row.
    Since,
}

impl Marker {
    pub fn name(self) -> &'static str {
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
    /// The host has no `offscreen`, its archive is off (`enabled=0`), or the
    /// client relays no header: the screen alone.
    NoArchive,
    /// The worker is not on the alternate screen: not a fullscreen app, or it
    /// left; the archive holds nothing of what it prints now.
    MainScreen,
    /// The archive is not the one the mark named: the host restarted since,
    /// or handed the session to a new instance that could not carry the
    /// archive (one that can keeps it and its origin).
    ArchiveReset,
    /// Rows after the start were evicted, or a gap (a redraw with no overlap,
    /// a resize, a reset) lies after it. A resize as a self-update's new
    /// instance takes over is one that loses nothing (rows may repeat).
    ArchiveGap,
    /// More archived rows than `--max-rows`: the middle is missing.
    MaxRows,
    /// Your turn's `❯` row was not found (nor a last `❯` row): the report is
    /// everything read, from the top.
    MarkerNotFound,
}

impl Reason {
    pub fn name(self) -> &'static str {
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
    /// Why it is not complete; empty when it is.
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
    /// The newest archived row read, to hand to `--since` next time (`None`
    /// without an archive).
    pub last: Option<Mark>,
}

impl Report {
    /// Nothing was lost between the start and the end.
    pub fn complete(&self) -> bool {
        self.reasons.is_empty()
    }

    /// `report complete=<0|1> [reason=<r>[,<r>…]] marker=<m> turn=<id|-> rows=<n>
    /// archived=<a> screen=<s> last=<origin:i|->`.
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

    /// [`Self::render`] for `view`: `View::All` is exactly it; `--final` and
    /// `--messages` print the same header with ` view=<final|messages>
    /// kept=<n>` after it — `rows=` still counts every row the report holds —
    /// a `--` line, then only the rows the view keeps
    /// ([`super::blocks::view_rows`]).
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
    /// Where the archive stood when the turn started (a host that predates
    /// the archive prints none).
    pub arch: Option<Mark>,
    /// What was typed (pct-decoded; the ledger keeps up to 512 bytes).
    pub text: String,
}

/// The records of a `history` reply (`turn <id> submitted=<0|1> … arch=<o>:<l>
/// text=<pct>` per line); any other line (a raw reply's `OK <n>`) is skipped.
/// `text=` is the free-text tail, so the line is cut there first.
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
        arch: None,
        text: pct_decode(text),
    };
    for word in words {
        match word.split_once('=') {
            Some(("submitted", v)) => turn.submitted = v == "1",
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
    let at = turns
        .iter()
        .rposition(|t| t.submitted)
        .or_else(|| turns.len().checked_sub(1))?;
    turns.into_iter().nth(at)
}

/// Decode the control protocol's `%XX` escape (every byte that is not ASCII
/// graphic, and `%`). TOTAL: a malformed escape passes through verbatim and
/// invalid UTF-8 decodes lossily. The same rule as `aterm_control::wire::
/// pct_decode`, which this crate does not depend on.
pub(super) fn pct_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let (Some(hi), Some(lo)) = (
                bytes.get(i + 1).and_then(|b| char::from(*b).to_digit(16)),
                bytes.get(i + 2).and_then(|b| char::from(*b).to_digit(16)),
            )
            && let Ok(byte) = u8::try_from(hi * 16 + lo)
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// One `offscreen … screen=1` reply: the header's fields and the two halves.
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
    /// How many rows the screen shows again from the archive: screen rows
    /// `pin..pin+back` are archived rows `back_at..` (`back_at`/`pin` 0 when
    /// the host does not say).
    pub back: usize,
    pub back_at: u64,
    pub pin: usize,
    pub origin: u64,
    /// The host's archive is recording (`enabled=0` says it is off: nothing
    /// that scrolled away was kept).
    pub enabled: bool,
    /// The worker is on the alternate screen.
    pub alt: bool,
    /// More archived rows than the page held.
    pub more: bool,
    pub archived: Vec<String>,
    pub screen: Vec<String>,
}

/// Why an `offscreen` reply could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OffscreenError {
    /// No `OK <n> first=… last=…` header anywhere (an `aterm ctl` that prints
    /// only a line-framed verb's rows), or the header alone with rows it counts
    /// dropped (an `aterm ctl` that predates `offscreen` frames it like
    /// `lines`, a status line). Either way the rows cannot be placed.
    NoHeader,
    /// A header that does not describe the rows that came.
    Bad(String),
}

/// Read an `offscreen` reply. `aterm ctl` prints the rows on stdout and the
/// header on stderr (`aterm-ctl: OK <n> first=…`, as it does for `inbox`); a
/// raw client carries the header as stdout's first line. `<n>` counts every
/// line that follows, the screen's `screen_rows=` included: the archived rows
/// are the first `n - screen_rows`.
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
        // The header on stdout and nothing after it: a client that framed the
        // reply as a status line and dropped every row — the rows are lost to
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

/// Join an `offscreen … screen=1` read: every archived row, then the screen's
/// rows to [`transcript_end`], less the ones it re-shows from the archive that
/// this read got — screen row `pin + m` is archived row `back_at + m` for
/// `m < back`, so a pinned header above them stays, and a re-shown row the
/// read did not get (a page cut short by `--max-rows`, a row from before the
/// mark) is kept from the screen: a duplicate, never a loss.
pub(super) fn join(off: &Offscreen) -> Joined {
    let count = u64::try_from(off.archived.len()).unwrap_or(u64::MAX);
    let upto = if count > 0 {
        off.first.saturating_add(count - 1)
    } else {
        off.last
    };
    let end = transcript_end(&off.screen);
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

/// A user row: `❯` in column 0, then whitespace. Claude Code's own `❯` rows —
/// a prompt's options (`❯ 1. Yes`) — are indented, and its composer is in the
/// live zone, which [`transcript_end`] already cut; so a message that starts
/// with `1.` is still a user row.
pub(super) fn is_user_row(row: &str) -> bool {
    row.strip_prefix('❯')
        .is_some_and(|rest| rest.starts_with(char::is_whitespace))
}

/// The first `need` non-whitespace characters of the user block at `at`: its
/// `❯` row, then the rows it wrapped onto (indented, or blank), up to the
/// first row back in column 0 (the worker's `⏺`, a done row).
pub(super) fn user_text(rows: &[String], at: usize, need: usize) -> String {
    let head = rows[at].strip_prefix('❯').unwrap_or(&rows[at]);
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

/// Whether `text` may show as a `[Pasted text …]` row: it has a line break,
/// or is long.
pub(super) fn paste_like(text: &str) -> bool {
    text.contains('\n') || text.chars().count() >= PASTE_CHARS
}

/// Whether the paste row `row` can be `text`: its `+N lines` (when it says)
/// counts the text's line breaks, give or take one (a trailing break, lines
/// against breaks) — and at most N+1 of them when the ledger may have cut the
/// text short.
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

/// Where the report starts in `rows`: for the ledger, the last user row whose
/// text begins with the turn's first [`MARKER_CHARS`] characters (whitespace
/// aside), else — for a paste-like turn — the last `[Pasted text …]` row when
/// it fits the text; without a ledger turn, the last user row; after
/// `--since`, the top.
fn find_start(rows: &[String], marker: Marker, text: Option<&str>) -> Option<usize> {
    let users: Vec<usize> = (0..rows.len()).filter(|&i| is_user_row(&rows[i])).collect();
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
            // A short turn is never a paste: an earlier turn's paste row is
            // not where it starts (it may be queued under the status row).
            let text = text.unwrap_or("");
            // Only the LAST paste row can be the newest turn's; one that does
            // not fit it is another paste, and an older one further up is
            // never reached for.
            by_text.or_else(|| {
                paste_like(text).then_some(())?;
                let at = users.iter().rev().copied().find(|&i| {
                    rows[i]
                        .trim_start_matches('❯')
                        .trim_start()
                        .starts_with(PASTED)
                })?;
                paste_fits(rows[at].trim_start_matches('❯').trim_start(), text).then_some(at)
            })
        }
    }
}

/// The report of `rows` from `start`: its blank rows at either end dropped,
/// and counted by where they came from (the first `archived` of `rows` are
/// the archive's).
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
    let start = find_start(rows, marker, text);
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
        // Everything after the given row counts, up to the gap at it.
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
        // The start is an archived row k: the rows after it came oldest first
        // from k on, so none of them was evicted at the read (eviction takes
        // the oldest). A gap after the mark may still lie before k — the one
        // a resize or a redraw left just before the turn began — so a gap
        // counted here is placed against k by a second read.
        (_, Some(s)) if s < joined.archived => {
            if off.breaks > 0 {
                recheck = Some(joined.first + u64::try_from(s).unwrap_or(u64::MAX));
            }
            // A tail read (no ledger turn) left out OLDER rows: nothing after
            // the start. An oldest-first page left out newer ones.
            if off.more && marker == Marker::Ledger {
                reasons.push(Reason::MaxRows);
            }
        }
        // The start is on the screen: every row reported came from the one
        // snapshot.
        (_, Some(_)) => {}
    }
    (reasons, recheck)
}

impl<C: Ctl> Session<'_, C> {
    /// `aterm drive report`: what the worker said since your newest submitted
    /// turn (or `--since`), the archive's rows and the screen's joined (see
    /// the module doc).
    pub fn report(&mut self, opts: &ReportOpts) -> Result<Report, String> {
        self.gather_report(opts).map_err(String::from)
    }

    /// [`Self::report`], a request not served told apart (`watch --report`
    /// rides it out).
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
        let joined = join(&off);
        let start = find_start(&joined.rows, marker, text.as_deref());
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

    /// The newest submitted turn in the ledger (`history 8`); `None` when there
    /// is none, or the host has no `history`.
    fn ledger_turn(&mut self) -> Result<Option<LedgerTurn>, Fail> {
        let r = self.call(&["history", HISTORY_DEPTH])?;
        if r.ok() {
            return Ok(newest_turn(parse_history(&r.stdout)));
        }
        if !self.unserved(&r) && without_verb(&r) {
            return Ok(None);
        }
        Err(self.fault(&r, format!("history failed: {}", r.stderr.trim())))
    }

    /// The one read: `offscreen since=<mark> max=<n> screen=1` — oldest first
    /// from the mark — or, with no mark to start from, the newest `<n>`
    /// (`tail=`). `None`: the host has no `offscreen`, or the client relayed
    /// no header to place the rows by.
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
        let r = self.call(&args)?;
        if r.ok() {
            return match parse_offscreen(&r) {
                Ok(off) => Ok(Some(off)),
                Err(OffscreenError::NoHeader) => Ok(None),
                Err(OffscreenError::Bad(why)) => Err(Fail::Hard(format!("offscreen: {why}"))),
            };
        }
        if !self.unserved(&r) && without_verb(&r) {
            return Ok(None);
        }
        Err(self.fault(&r, format!("offscreen failed: {}", r.stderr.trim())))
    }

    /// Whether a gap lies after archived row `k` (`offscreen since=<origin>:<k>
    /// max=1`). Read after the snapshot, so a gap that came since counts too:
    /// never a gap missed. A reply that says nothing certain is a gap.
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

    /// The whole screen (`text --json`, never `tail=`): a host without the
    /// archive reports from it alone.
    fn full_screen(&mut self) -> Result<super::screen::Screen, Fail> {
        let r = self.call(&["text", "--json"])?;
        if !r.ok() {
            return Err(self.fault(&r, format!("text --json failed: {}", r.stderr.trim())));
        }
        parse_text_json(&r.stdout).map_err(Fail::Hard)
    }
}

/// The host does not know the verb: an older build answers `ERR unknown verb`
/// (or, for a verb aimed at another session, `ERR denied` — an unknown verb
/// has no op class to authorize), a usage line or a bare `ERR`.
pub(super) fn without_verb(r: &CtlReply) -> bool {
    r.unknown_form() || r.is_err("unknown verb") || r.is_err("denied")
}

#[cfg(test)]
#[path = "report_tests.rs"]
mod tests;
