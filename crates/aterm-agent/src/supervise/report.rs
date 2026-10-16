// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm drive report`: what the worker said since your turn, in full, not
//! just what is still on its screen. Claude Code runs on the alternate screen
//! and repaints in place, so rows that scroll off are gone; the host keeps them
//! (the alt-screen archive, read with `offscreen`), and every `turn` records
//! where the archive stood (`history`'s `arch=<origin>:<last>`).
//!
//! One read, `offscreen since=<mark> screen=1`, returns the rows archived after
//! the mark and the screen under one lock, with `back=` (how many screen rows
//! repeat archived ones). The join is the archived rows, then the screen's
//! rows down to where its transcript ends ([`ScreenReader::transcript_end`]:
//! Claude Code's [`transcript_end`]), less the repeats, kept verbatim.
//!
//! The report opens at your turn's `❯` row: the last one whose text begins
//! with the first [`MARKER_CHARS`] characters of the ledger's text (whitespace
//! ignored), or a `[Pasted text #N +L lines]` row for a paste. Without a turn
//! in the ledger it opens at the last `❯` row; `--since <origin:i>` opens after
//! an archived row.
//!
//! **CODEX on the alternate screen** — its default: 0.163.0 runs there
//! (`modes` `alt_screen=true`, measured in the lab on 2026-10-16), inline only
//! under `--no-alt-screen` — is archived the same way, and read in its own
//! grammar ([`Session::archive_reader`]): its user rows open with `›`, its
//! live zone (a box, its running turn's status row, its composer and footer)
//! is cut by its reader, and its rows are read back in its blocks.
//!
//! `complete=1` says nothing was lost: the start was found, the archive is the
//! one the mark named, the worker is on the alternate screen, no gap or
//! eviction lies after the start, and `--max-rows` held every row; otherwise
//! `reason=` says why. A host without the archive gets the screen alone
//! (`reason=no-archive`).
//!
//! **A worker on the MAIN screen** — Codex drawing inline (`--no-alt-screen`,
//! [`aterm_phase::codex`]) — has no archive: what scrolls off its screen is
//! the main screen's scrollback ([`Session::main_screen_report`]). Its user
//! rows open with its own caret (`›`, [`ScreenReader::caret`]), matched as
//! above. A start on the screen is the screen's report; one scrolled off is
//! found by `search ^<caret> regex` among the scrollback's user rows, newest
//! first, each read by `line <row>`, and the rows are read by `line` from it
//! down to the screen's last row, the live zone cut by the reader
//! ([`ScreenReader::transcript_end`]: its box, its running turn's status row,
//! else its composer and footer). The search's hits are placed against the
//! live screen by the `screen_first=` its header carries (a logical-line `^`
//! match, never a count of the screen's physical caret rows). `--max-rows`
//! spent before the screen keeps the END as the archive's page does: the rows
//! read from the start, then the screen's (its last row found by a few `line`
//! probes), the middle missing (`reason=max-rows`). A `search` that says it is
//! ` incomplete` is `reason=search-incomplete` where the start may lie among
//! the rows it did not search; one whose reply cannot place the screen (no
//! `screen_first=`), or that may have left out rows anywhere when no ledger
//! turn names the start (the newest user row is then unknown), gets the
//! screen alone (`reason=search-incomplete`, `marker-not-found`). `last=-`:
//! those rows have no archive mark. A host without `search` or `line` gets the
//! screen alone (`reason=main-screen`). Unlike the archive and screen returned
//! together by `offscreen`, these `line` requests each sample the live
//! terminal separately. `complete=1` still requires the marker, retained rows
//! and read budget, but does not establish an atomic multi-row snapshot across
//! concurrent output or resize. The report's rows are read back in the
//! reader's own grammar ([`Report::program`]: `--final` and `--messages` split
//! Codex's blocks).

use std::fmt;

use aterm_ctl::SEARCH_DEPTH_NOTE;
use aterm_phase::{Program, ScreenReader};

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
/// The caret Claude Code's user rows open with, and any reader's without one.
const CLAUDE_CARET: char = '❯';
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
    /// left; the archive holds nothing of what it prints now. A Codex worker
    /// on a host without `search` or `line`: its scrollback is not read.
    MainScreen,
    /// A main-screen worker's `search` for its user rows said it was
    /// ` incomplete` where that matters: the start was not found and history
    /// older than the rows searched was not searched, or rows anywhere may
    /// have been left out (the match cap, a rewrap that searched only the
    /// screen, a long line not searched to its end, a cause this reader does
    /// not know) — a newer user row than the start, or the start itself, may
    /// be among them. Or its reply lacks the live screen's absolute position
    /// (`screen_first=`), so its hits cannot safely locate the turn.
    SearchIncomplete,
    /// The archive is not the one the mark named: the host restarted since,
    /// or handed the session to a new instance that could not carry the
    /// archive (one that can keeps it and its origin).
    ArchiveReset,
    /// Rows after the start were evicted, or a gap (a redraw with no overlap,
    /// a resize, a reset) lies after it. A resize as a self-update's new
    /// instance takes over is one that loses nothing (rows may repeat).
    ArchiveGap,
    /// More archived rows than `--max-rows` (a main-screen worker's: more
    /// scrollback rows): the middle is missing.
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
            Reason::SearchIncomplete => "search-incomplete",
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
    /// The program whose grammar the rows are read back in (`--final`,
    /// `--messages`, [`super::blocks`]): Codex for a Codex worker's report
    /// (the archive's, on the alternate screen, or the main screen's, drawn
    /// inline), Claude Code otherwise.
    pub program: Program,
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
    /// a `--` line, then only the rows the view keeps, read in the report's
    /// program's grammar ([`super::blocks::view_rows`], [`Self::program`]).
    pub fn render_view(&self, view: View) -> String {
        if view == View::All {
            return self.render();
        }
        let kept = view_rows(&self.rows, view, self.program);
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

/// Decode the control protocol's `%XX` escape: the wire's one TOTAL decoder,
/// under the name this crate's parsers (`ledger`, `claim`, `lifecycle`, the
/// approval loop, `run`) import it by.
pub(crate) use aterm_codec::pct::decode as pct_decode;

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
/// rows to where `reader` says its transcript ends
/// ([`ScreenReader::transcript_end`]; Claude Code's: [`transcript_end`]), less
/// the ones it re-shows from the archive that this read got — screen row
/// `pin + m` is archived row `back_at + m` for `m < back`, so a pinned header
/// above them stays, and a re-shown row the read did not get (a page cut short
/// by `--max-rows`, a row from before the mark) is kept from the screen: a
/// duplicate, never a loss.
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

/// Codex on the alternate screen PINS the prompt of the turn it shows on the
/// screen's top row once that prompt's own row has scrolled off (measured in
/// the lab on 0.163.0, 2026-10-16: `› Write seventy streamed paragraphs.`
/// drawn over paragraph 50 of its streamed reply, the row itself and
/// paragraphs 1 to 49 in the host's archive, `back=0`). That copy is no row of
/// the transcript: read as one it was the newest `›` row, the report opened
/// there, `complete=1`, without the reply's first 49 paragraphs, and the
/// views read the paragraphs under it as the person's message. Screen row 0,
/// kept at the join's seam, is that copy when it is a user row (`caret`)
/// whose text is the newest archived user row's; it leaves the join. A person
/// who sent the same words twice, the second row drawn on the top row, has
/// the report open one turn early: more rows, never fewer. A copy the host
/// placed against the archive (`back=`, [`join`]) has left already.
fn drop_pinned_prompt(off: &Offscreen, joined: &mut Joined, caret: char) {
    let seam = joined.archived;
    let Some(top) = off.screen.first() else {
        return;
    };
    let pinned = joined.rows.get(seam) == Some(top)
        && user_row(top, caret)
        && joined.rows[..seam]
            .iter()
            .rfind(|r| user_row(r, caret))
            .is_some_and(|r| r.trim_end() == top.trim_end());
    if pinned {
        joined.rows.remove(seam);
    }
}

/// A user row: `❯` in column 0, then whitespace. Claude Code's own `❯` rows —
/// a prompt's options (`❯ 1. Yes`) — are indented, and its composer is in the
/// live zone, which [`transcript_end`] already cut; so a message that starts
/// with `1.` is still a user row.
pub(super) fn is_user_row(row: &str) -> bool {
    user_row(row, CLAUDE_CARET)
}

/// A user row of a program whose user rows open with `caret` ([`is_user_row`]
/// for Claude Code's; `›` for Codex's, whose box options are indented too).
/// Codex's EMPTY composer (`› Ask Codex to do anything`) is no user row: the
/// archive keeps one when a reply taller than the screen replaced its frame in
/// one jump ([`aterm_phase::codex::is_empty_composer`]).
fn user_row(row: &str, caret: char) -> bool {
    row.strip_prefix(caret)
        .is_some_and(|rest| rest.starts_with(char::is_whitespace))
        && !aterm_phase::codex::is_empty_composer(row)
}

/// The first `need` non-whitespace characters of the user block at `at`: its
/// caret row (the caret dropped), then the rows it wrapped onto (indented, or
/// blank), up to the first row back in column 0 (the worker's `⏺`, a done
/// row).
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
            // A short turn is never a paste: an earlier turn's paste row is
            // not where it starts (it may be queued under the status row).
            let text = text.unwrap_or("");
            // Only the last paste row can be the newest turn's; one that does
            // not fit it is another paste, and an older one further up is
            // never reached for.
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
/// the archive's); `program` the grammar they are read back in.
#[allow(
    clippy::too_many_arguments,
    reason = "every field of the report but its rows' own trim: a struct would only rename \
              the list"
)]
fn finish(
    program: Program,
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
        program,
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
    finish(
        Program::Claude,
        marker,
        turn,
        rows,
        0,
        start.unwrap_or(0),
        reasons,
        None,
    )
}

/// What a `search` reply says it did not search, by its ` incomplete` header
/// marker (a raw client's header, the first line of stdout; `aterm ctl`'s, on
/// stderr when nothing matched) or the note `aterm ctl` prints for it
/// ([`aterm_ctl::search_incomplete_note`], on stderr); the worst one wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Unsearched {
    /// Nothing: the hits are every row that matches.
    Nothing,
    /// History OLDER than the rows searched (the depth cap: a bare `
    /// incomplete`, or ` older` alone): every newer row was searched.
    Older,
    /// Rows anywhere: the match cap, a rewrap (only the screen searched), a
    /// long line not searched to its end, the terminal changed mid-search, or
    /// a qualifier or note this reader does not know.
    Anywhere,
}

/// [`Unsearched`] of one `search` reply.
fn unsearched(r: &CtlReply) -> Unsearched {
    let mut worst = Unsearched::Nothing;
    let header = |tail: &str| {
        let mut words = tail.split_whitespace();
        let n = words.next().and_then(decimal);
        // The qualifiers end where a client's own note on the header begins
        // (`aterm ctl`'s ` (search: no results)` on a reply with no hits);
        // `screen_first=` places the screen and says nothing of what was left
        // out ([`search_screen_first`]).
        let words: Vec<&str> = words
            .take_while(|w| !w.starts_with('('))
            .filter(|w| !w.starts_with("screen_first="))
            .collect();
        match words.as_slice() {
            [] => Unsearched::Nothing,
            // An older server marks the match limit bare too.
            ["incomplete"] if n.is_some_and(|n| n >= 100_000) => Unsearched::Anywhere,
            ["incomplete"] => Unsearched::Older,
            ["incomplete", causes @ ..] if causes.iter().all(|c| *c == "older") => {
                Unsearched::Older
            }
            // Any other cause, or a qualifier this reader does not know (a
            // future server's), may have left out newer rows.
            _ => Unsearched::Anywhere,
        }
    };
    if let Some(tail) = r
        .stdout
        .lines()
        .next()
        .and_then(|l| l.trim().strip_prefix("OK "))
    {
        worst = worst.max(header(tail));
    }
    for line in r.stderr.lines() {
        let line = line.trim();
        let line = line.strip_prefix("aterm-ctl:").map_or(line, str::trim);
        if line.is_empty() {
            continue;
        }
        let said = match line.strip_prefix("OK ") {
            Some(tail) => header(tail),
            None if line.split("; ").all(|part| part == SEARCH_DEPTH_NOTE) => Unsearched::Older,
            None => Unsearched::Anywhere,
        };
        worst = worst.max(said);
    }
    worst
}

/// The user rows `search` named ([`Session::caret_rows`]), the live screen's
/// first absolute row in the frame it searched, and what it did not search.
struct UserRows {
    rows: Vec<u64>,
    screen_first: u64,
    unsearched: Unsearched,
}

/// The rows read from a user row of the main screen down
/// ([`Session::main_block`]), and whether the read reached past the screen's
/// last row.
struct MainBlock {
    rows: Vec<String>,
    bottom: bool,
}

/// One row of the main screen and its scrollback, as `line <row>` reads it.
enum MainRow {
    Text(String),
    /// Older than the oldest row kept.
    Evicted,
    /// Past the screen's last row.
    Past,
}

/// What the main-screen search for the user rows gave ([`Session::caret_rows`]).
enum MainSearch {
    /// Its hits, placed against the live screen.
    Rows(UserRows),
    /// A reply that cannot place the live-screen boundary
    /// ([`search_screen_first`]): none of its hits can safely locate the turn.
    Incomplete,
    /// The host has no `search`.
    Unavailable,
}

/// The live screen's first absolute row in the frame a `search` reply
/// searched: its header's one `screen_first=<row>` (a raw client's header, the
/// first line of stdout; `aterm ctl`'s, on stderr). `None` for a reply with no
/// header, none or a malformed one, or two: an older host or client cannot
/// tell a logical-line hit from a physical continuation row that starts with
/// the caret. What the reply left out is [`unsearched`]'s.
fn search_screen_first(reply: &CtlReply) -> Option<u64> {
    let raw = reply
        .stderr
        .lines()
        .map(|line| {
            line.trim()
                .strip_prefix("aterm-ctl:")
                .unwrap_or(line)
                .trim()
        })
        .chain(reply.stdout.lines().take(1))
        .find_map(|line| line.strip_prefix("OK "));
    let raw = raw?;
    let mut words = raw.split_whitespace();
    words.next().and_then(decimal)?;
    let mut screen_first = None;
    for word in words {
        if let Some(value) = word.strip_prefix("screen_first=") {
            let first = decimal(value)?;
            if screen_first.replace(first).is_some() {
                return None;
            }
        }
    }
    screen_first
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
            if let Some(reader) = self.main_screen_reader(marker, &screen.rows)? {
                return self.main_screen_report(
                    reader,
                    (marker, turn, text.as_deref()),
                    &screen.rows,
                    opts.max_rows,
                );
            }
            return Ok(screen_only(marker, turn, text.as_deref(), &screen.rows));
        };
        if !off.alt
            && let Some(reader) = self.main_screen_reader(marker, &off.screen)?
        {
            return self.main_screen_report(
                reader,
                (marker, turn, text.as_deref()),
                &off.screen,
                opts.max_rows,
            );
        }
        let reader = self.archive_reader(&off.screen)?;
        let mut joined = join(&off, reader);
        let caret = reader.caret().unwrap_or(CLAUDE_CARET);
        match reader.program() {
            Program::Codex => drop_pinned_prompt(&off, &mut joined, caret),
            Program::Claude | Program::Agy | Program::Muse | Program::Generic => {}
        }
        let start = find_start(&joined.rows, marker, text.as_deref(), caret);
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
            reader.program(),
            marker,
            turn,
            &joined.rows,
            joined.archived,
            start.unwrap_or(0),
            reasons,
            Some(last),
        ))
    }

    /// The grammar an ARCHIVE report — a worker on the alternate screen — is
    /// read in: Codex's for a Codex worker (0.163.0 runs on the alternate
    /// screen by default: the module doc), Claude Code's for any other (agy
    /// and Muse Code among them, as before Codex's was read). The
    /// program is what the session's reads named, else what `rows` show
    /// ([`Session::reader`]: Claude Code's frame or box, Codex's composer,
    /// status row or box), else — a screen that names no program, a Codex
    /// composer holding a draft — what `status` names
    /// ([`Session::named_reader`]).
    fn archive_reader(&mut self, rows: &[String]) -> Result<&'static dyn ScreenReader, Fail> {
        let by_screen = self.reader(rows);
        let reader = match by_screen.program() {
            Program::Claude | Program::Codex | Program::Agy | Program::Muse => by_screen,
            Program::Generic => self.named_reader(rows)?,
        };
        Ok(match reader.program() {
            Program::Codex => reader,
            Program::Claude | Program::Agy | Program::Muse | Program::Generic => {
                Program::Claude.reader()
            }
        })
    }

    /// The reader of a worker that draws its transcript on the MAIN screen —
    /// Codex drawing inline (`--no-alt-screen`) — for a report from your turn
    /// or the last user row (`--since` names an archived row): the session's
    /// program's ([`Session::named_reader`]), else what `rows` show. `None`
    /// for any other worker.
    fn main_screen_reader(
        &mut self,
        marker: Marker,
        rows: &[String],
    ) -> Result<Option<&'static dyn ScreenReader>, Fail> {
        if marker == Marker::Since {
            return Ok(None);
        }
        let reader = self.named_reader(rows)?;
        Ok(match reader.program() {
            Program::Codex => Some(reader),
            // Claude Code's inline renderer draws on the main screen too: its
            // scrollback is not read.
            // Neither agy nor Muse Code was measured drawing a turn on the
            // main screen: theirs is not read either.
            Program::Claude | Program::Agy | Program::Muse | Program::Generic => None,
        })
    }

    /// The report of a worker on the MAIN screen (module doc), read by
    /// `reader`, from `screen` (the whole grid) and, when the start has
    /// scrolled off it, the scrollback: the start found among the user rows
    /// `search` names, newest first, each read by `line` as far as the match
    /// needs, then every row from it down to the screen's last, the live zone
    /// cut ([`ScreenReader::transcript_end`]). `--max-rows` bounds the `line`
    /// reads; spent before the screen, the report keeps the rows read and
    /// closes with the screen's ([`Self::main_tail`]). `start` is the marker,
    /// the ledger turn and its text.
    fn main_screen_report(
        &mut self,
        reader: &dyn ScreenReader,
        start: (Marker, Option<u64>, Option<&str>),
        screen: &[String],
        max_rows: usize,
    ) -> Result<Report, Fail> {
        let (marker, turn, text) = start;
        let program = reader.program();
        let caret = reader.caret().unwrap_or(CLAUDE_CARET);
        let shown = &screen[..reader.transcript_end(screen).min(screen.len())];
        if let Some(at) = find_start(shown, marker, text, caret) {
            return Ok(finish(
                program,
                marker,
                turn,
                shown,
                0,
                at,
                Vec::new(),
                None,
            ));
        }
        let incomplete = |why: Vec<Reason>| finish(program, marker, turn, shown, 0, 0, why, None);
        let users = match self.caret_rows(caret)? {
            MainSearch::Rows(users) => users,
            MainSearch::Incomplete => {
                return Ok(incomplete(vec![
                    Reason::SearchIncomplete,
                    Reason::MarkerNotFound,
                ]));
            }
            MainSearch::Unavailable => {
                return Ok(incomplete(vec![Reason::MainScreen, Reason::MarkerNotFound]));
            }
        };
        // With no turn's text to read a hit by, the start is the NEWEST user
        // row above the screen: a search that may have left out rows anywhere
        // (newer ones among them) cannot name it, and an older hit is never
        // reported as the newest turn.
        if text.is_none() && users.unsearched == Unsearched::Anywhere {
            return Ok(incomplete(vec![
                Reason::SearchIncomplete,
                Reason::MarkerNotFound,
            ]));
        }
        // The newest are the screen's own (its user rows, matched above; its
        // composer): none of them is the start. Search anchors are logical
        // lines, and a wrapped physical row that starts with the caret is no
        // hit, so the search frame's absolute live-screen boundary places them,
        // never a count of the screen's caret rows.
        let mut budget = max_rows.max(1);
        let mut reasons = Vec::new();
        let mut found = None;
        for &at in users
            .rows
            .iter()
            .rev()
            .filter(|&&at| at < users.screen_first)
        {
            let Some(block) = self.main_block(at, marker, text, &mut budget, &mut reasons)? else {
                return Ok(incomplete(vec![Reason::MainScreen, Reason::MarkerNotFound]));
            };
            if find_start(&block.rows, marker, text, caret) == Some(0) {
                found = Some((at, block));
                break;
            }
            if budget == 0 {
                break;
            }
        }
        let Some((at, block)) = found else {
            if budget == 0 {
                reasons.push(Reason::MaxRows);
            }
            // The start may lie among the rows the search left out.
            if users.unsearched > Unsearched::Nothing {
                reasons.push(Reason::SearchIncomplete);
            }
            reasons.push(Reason::MarkerNotFound);
            return Ok(incomplete(reasons));
        };
        // Found: only rows left out ANYWHERE may hide a newer user row with
        // the turn's text; older history holds none newer than the start.
        if users.unsearched == Unsearched::Anywhere {
            reasons.push(Reason::SearchIncomplete);
        }
        // Every row from the start down to the screen's last: the last
        // `screen.len()` of them are the screen's.
        let (mut rows, mut bottom) = (block.rows, block.bottom);
        let mut next = at + u64::try_from(rows.len()).unwrap_or(u64::MAX);
        while !bottom {
            if budget == 0 {
                return self.main_tail(program, (marker, turn), rows, next, screen, reasons);
            }
            budget -= 1;
            match self.main_row(next)? {
                Some(MainRow::Text(row)) => rows.push(row),
                Some(MainRow::Evicted) => reasons.push(Reason::ArchiveGap),
                Some(MainRow::Past) => bottom = true,
                None => {
                    return Ok(incomplete(vec![Reason::MainScreen, Reason::MarkerNotFound]));
                }
            }
            next += 1;
        }
        // The last `screen.len()` rows read are the screen's, the rest the
        // scrollback's.
        let archived = rows.len().saturating_sub(screen.len());
        let rows = &rows[..reader.transcript_end(&rows).min(rows.len())];
        Ok(finish(
            program, marker, turn, rows, archived, 0, reasons, None,
        ))
    }

    /// The report of a main-screen worker whose `--max-rows` ran out at
    /// absolute row `next`, `read` the rows from the start up to it: it KEEPS
    /// THE END, as the archive's page cut short does — the rows read, then the
    /// screen's ([`Self::main_bottom`] finds the screen's last row, so its top
    /// is known), the live zone cut by the program's reader. Where the read
    /// stopped above the screen, the rows between are missing (max-rows);
    /// where it stopped on the screen, the screen's rows from there close it
    /// and nothing is missing. A screen that moved on since it was read shows
    /// rows the read got again: a duplicate, never a loss. `start` is the
    /// marker and the ledger turn.
    fn main_tail(
        &mut self,
        program: Program,
        start: (Marker, Option<u64>),
        read: Vec<String>,
        next: u64,
        screen: &[String],
        mut reasons: Vec<Reason>,
    ) -> Result<Report, Fail> {
        let (marker, turn) = start;
        let reader = program.reader();
        let Some(last) = self.main_bottom(next)? else {
            reasons.push(Reason::MaxRows);
            return Ok(finish(
                program,
                marker,
                turn,
                &read,
                read.len(),
                0,
                reasons,
                None,
            ));
        };
        let height = u64::try_from(screen.len()).unwrap_or(u64::MAX);
        let top = (last + 1).saturating_sub(height);
        let mut rows = read;
        if top > next {
            reasons.push(Reason::MaxRows);
            let archived = rows.len();
            rows.extend_from_slice(&screen[..reader.transcript_end(screen).min(screen.len())]);
            return Ok(finish(
                program, marker, turn, &rows, archived, 0, reasons, None,
            ));
        }
        let from = usize::try_from(next - top).unwrap_or(usize::MAX);
        rows.extend_from_slice(screen.get(from..).unwrap_or_default());
        // The last `screen.len()` rows read are the screen's, the rest the
        // scrollback's.
        let archived = rows.len().saturating_sub(screen.len());
        let rows = &rows[..reader.transcript_end(&rows).min(rows.len())];
        Ok(finish(
            program, marker, turn, rows, archived, 0, reasons, None,
        ))
    }

    /// The main screen's LAST row (the screen's bottom), at or past absolute
    /// row `from`: `line` probes at twice the step each time until one is past
    /// the screen, then halving between the last row read and that one — some
    /// 2·log₂ reads. `None`: the host has no `line`.
    fn main_bottom(&mut self, from: u64) -> Result<Option<u64>, Fail> {
        // `seen` is a row that exists (or one before `from`), `past` one past
        // the bottom.
        let mut seen = from.saturating_sub(1);
        let mut step = 1u64;
        let past = loop {
            let probe = seen.saturating_add(step);
            match self.main_row(probe)? {
                None => return Ok(None),
                Some(MainRow::Past) => break probe,
                // An evicted row still is a row: the bottom lies further on.
                Some(MainRow::Text(_) | MainRow::Evicted) => {
                    seen = probe;
                    step = step.saturating_mul(2);
                }
            }
        };
        let mut past = past;
        while past - seen > 1 {
            let mid = seen + (past - seen) / 2;
            match self.main_row(mid)? {
                None => return Ok(None),
                Some(MainRow::Past) => past = mid,
                Some(MainRow::Text(_) | MainRow::Evicted) => seen = mid,
            }
        }
        Ok(Some(seen))
    }

    /// The rows of the main screen and its scrollback that open with `caret`,
    /// as absolute rows, oldest first (`search ^<caret> regex`), with the live
    /// screen's first row in the searched frame ([`search_screen_first`]) and
    /// what the search says it left out ([`unsearched`]). A reply that cannot
    /// place the live screen is [`MainSearch::Incomplete`], refused before its
    /// rows are used; a host with no `search` is [`MainSearch::Unavailable`].
    fn caret_rows(&mut self, caret: char) -> Result<MainSearch, Fail> {
        let pattern = format!("^{caret}");
        let r = self.call(&["search", &pattern, "regex"])?;
        if r.ok() {
            let Some(screen_first) = search_screen_first(&r) else {
                return Ok(MainSearch::Incomplete);
            };
            // `<row> <col> <len>` per hit; a raw client's `OK <n>` header
            // (and an `incomplete` note) is no hit.
            let mut rows: Vec<u64> = r
                .stdout
                .lines()
                .filter_map(|l| {
                    let mut words = l.split(' ');
                    let row = decimal(words.next()?)?;
                    decimal(words.next()?)?;
                    decimal(words.next()?)?;
                    words.next().is_none().then_some(row)
                })
                .collect();
            rows.sort_unstable();
            rows.dedup();
            return Ok(MainSearch::Rows(UserRows {
                rows,
                screen_first,
                unsearched: unsearched(&r),
            }));
        }
        if !self.unserved(&r) && without_verb(&r) {
            return Ok(MainSearch::Unavailable);
        }
        Err(self.fault(&r, format!("search failed: {}", r.stderr.trim())))
    }

    /// The user block at absolute row `at`, read by `line` as far as
    /// [`find_start`] needs to judge it: its caret row, then the rows it
    /// wrapped onto until the turn's first [`MARKER_CHARS`] characters
    /// (whitespace aside) are read, up to the first row back in column 0,
    /// which is read too. Each read spends one of `budget`; an evicted row is
    /// a gap in `reasons`. `None`: the host has no `line`.
    fn main_block(
        &mut self,
        at: u64,
        marker: Marker,
        text: Option<&str>,
        budget: &mut usize,
        reasons: &mut Vec<Reason>,
    ) -> Result<Option<MainBlock>, Fail> {
        let need = match marker {
            Marker::Ledger => text
                .unwrap_or("")
                .chars()
                .filter(|c| !c.is_whitespace())
                .take(MARKER_CHARS)
                .count(),
            Marker::UserRow | Marker::Since => 0,
        };
        let mut block = MainBlock {
            rows: Vec::new(),
            bottom: false,
        };
        while *budget > 0 {
            *budget -= 1;
            let next = at + u64::try_from(block.rows.len()).unwrap_or(u64::MAX);
            let row = match self.main_row(next)? {
                Some(MainRow::Text(row)) => row,
                Some(MainRow::Evicted) => {
                    reasons.push(Reason::ArchiveGap);
                    break;
                }
                Some(MainRow::Past) => {
                    block.bottom = true;
                    break;
                }
                None => return Ok(None),
            };
            let ended =
                !block.rows.is_empty() && !row.is_empty() && !row.starts_with(char::is_whitespace);
            block.rows.push(row);
            if ended || user_text(&block.rows, 0, need).chars().count() >= need {
                break;
            }
        }
        Ok(Some(block))
    }

    /// `line <row>`: one row of the main screen or its scrollback, by its
    /// absolute row. `None`: the host has no `line`.
    fn main_row(&mut self, row: u64) -> Result<Option<MainRow>, Fail> {
        let at = row.to_string();
        let r = self.call(&["line", &at])?;
        if r.ok() {
            // `OK <text>`: the text after the one space, which a row's
            // leading blanks follow.
            let line = r.stdout.lines().next().unwrap_or("");
            let text = line.strip_prefix("OK").unwrap_or(line);
            return Ok(Some(MainRow::Text(
                text.strip_prefix(' ').unwrap_or(text).to_string(),
            )));
        }
        if r.is_err("evicted") {
            return Ok(Some(MainRow::Evicted));
        }
        if r.is_err("out of range") {
            return Ok(Some(MainRow::Past));
        }
        if !self.unserved(&r) && without_verb(&r) {
            return Ok(None);
        }
        Err(self.fault(&r, format!("line failed: {}", r.stderr.trim())))
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
