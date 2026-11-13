// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Supervisor decision journal ([`super::SuperviseOpts::journal`]): one JSON record per line a
//! loop prints ([`JournalRecord::of_line`]). A record's `kind` is one of [`Word::ALL`]'s names,
//! or `other` for a line that begins with any other word (each a loop journals is one of
//! [`Noted::ALL`]):
//!
//! ```text
//! {"t":<unix ms>,"sid":"<sid>"|null,
//!  "kind":"event|approved|declined|dismissed|reconnect|timeout|exit|mail|extend|escalated|
//!          cleared|continued|typed|restarted|waiting|skipped|unverified|limited|unproven|
//!          dialog|chose|exception|probed|reset|probe|needs|hold|occupancy|other",
//!  "phase":"idle|question|prompt|limited|survey|context|compacted|turn|idle-no-report|
//!           <rule id>|resumed|still-limited|rebriefed|rebrief-failed|point|background|-",
//!  "seq":<n>|null,"complete":0|1|null,"rows":<n>|null,"summary":"<the line's free-text tail>",
//!  "line":"<the exact line>","turn":<id>|null,"report":<id>|null[,"mark":"<16 hex>"]
//!  [,"ended":<unix ms>]}
//! ```
//!
//! A line's free-text tail is the agent's screen by construction (a reply's last line, a box's
//! command, a question's words), so it stays in the harness's drive store, 0600 files in the 0700
//! `<state>/drive` folder: this journal, the approval ledger beside it (which keeps an act's
//! command and the answer typed) and tapes when recorded (DESIGN-session-log §3.6). An app log
//! keeps [`log_words`] of a line: its words, sequence and the tail's length.

use std::io::Write;
use std::path::{Path, PathBuf};

use super::screen::Json;

/// The line that adopts a badge a previous loop left on the session, at this loop's start.
pub const ADOPTED: &str = "ADOPTED";

/// The badge a `CLEARED seq=<n> box attention=<reply>` line ended.
pub const CLEARED_BOX: &str = "box";
/// … `CLEARED seq=<n> turn-end attention=<reply>`.
pub const CLEARED_TURN_END: &str = "turn-end";
/// … `CLEARED seq=<n> goal attention=<reply>`.
pub const CLEARED_GOAL: &str = "goal";
/// … `CLEARED seq=<n> memory attention=<reply>: …`.
pub const CLEARED_MEMORY: &str = "memory";
/// … `CLEARED seq=<n> stale attention=<reply> (…)`.
pub const CLEARED_STALE: &str = "stale";
/// … `CLEARED seq=- hidden attention=<reply> (…)`.
pub const CLEARED_HIDDEN: &str = "hidden";
/// The line that puts the worker's NEEDS YOU on the person's screen.
pub const NEEDS: &str = "NEEDS";
/// … `CLEARED seq=<n> needs attention=<reply> <why>`.
pub const CLEARED_NEEDS: &str = "needs";

/// Every word a `CLEARED` line names a badge by.
pub const CLEARED_BADGES: [&str; 6] = [
    CLEARED_BOX,
    CLEARED_TURN_END,
    CLEARED_GOAL,
    CLEARED_MEMORY,
    CLEARED_STALE,
    CLEARED_HIDDEN,
];

/// The `<reply>` a badge's `CLEARED` line names where the loop's entry was not taken.
pub const ATTENTION_KEPT: &str = "kept";

/// The line a stall's hold writes (`stall.rs`'s `hold_for_stall`).
pub const FROZEN: &str = "FROZEN";

/// The words of the `NOTE seq=<n> rule=<r> … input=<w> input_bytes=<n> input_wait_ms=<ms>` a typed
/// act journals once where its screen has not moved `REACTION_WAIT` after the write
/// (`turn_end_loop.rs`'s `note_text_held`): the program has not shown the text, and the `status`
/// fields after it say whether it has read it.
pub const TEXT_HELD: &str = "the program has drawn nothing of the text written to it";

/// The words of the records of a line this loop wrote and gave up on undrawn, held in custody
/// (`turn_end_loop.rs`'s `Custody`): `SKIPPED seq=<n> rule=<r> typed, not submitted (<why>):
/// <text>; held until the worker draws it (input=<w>)` where it is taken in, and
/// `SKIPPED seq=<n> rule=<r> held until the worker draws it: <text> (input=<w>…); nothing
/// written` once at each change of what holds a write, the text in backticks in both. Records,
/// never a badge or an ask.
pub const LINE_HELD: &str = "held until the worker draws it";

/// `NOTE seq=<n> rule=<r> the worker read the line and has drawn none of it <s> s on (input=clear,
/// at rest): restarting it in place`, before a restart in place for a line held in custody; and
/// the head of the `WAITING` line where none can be made.
pub const LINE_READ_UNDRAWN: &str = "the worker read the line and has drawn none of it";

/// Where the host did not make the restart in place asked for a line held in custody (refused, or
/// no answer): the words the `WAITING` records of its wait carry after [`LINE_READ_UNDRAWN`],
/// with the host's step from the next due on. The host is asked once, never again at each due.
pub const LINE_RESTART_UNMADE: &str = "the restart in place could not be made";

/// Where the host answered the restart in place asked for a line held in custody with a word that
/// it made it, but the process the line was written to is named after it, by the restart's own
/// report or, where that names none, by the host (`custody::Restarted::Stands`): the words the
/// `WAITING` record of that ask carries after the host's step. Nothing shows the restart ended
/// that process, so the line stands held and the restart is asked again on its back-off, as a
/// deferral is.
pub const LINE_RESTART_STANDS: &str = "no evidence it ended the process the line was written to";

/// `NOTE seq=<n> rule=<r> the line held <how>: <text>`, the text in backticks: how a line held in
/// custody settled (`is drawn`, `is under a person's keys`, `went with a turn` or `ended with its
/// program`).
pub const LINE_SETTLED: &str = "the line held";

/// The head of the `WAITING` line of a line held in custody that the worker read and drew none of
/// after a restart in place was already made since the last act submitted: a second loss waits,
/// it is never restarted again.
pub const LINE_LOST_AGAIN: &str =
    "the worker read the line and has drawn none of it again since a restart in place";

/// How the line of a turn the Mac slept through in the vendor's API retry, Escaped once the API
/// was back, begins (`supervise::run::woken_turn`).
pub const UNSTUCK: &str = "UNSTUCK";

/// How the line of the rows of a screen the loop keeps begins ([`Capture`]).
pub const CAPTURED: &str = "CAPTURED";

/// The rows of a screen the loop kept, as its journal line says them: `CAPTURED seq=<n> <what>
/// program=<name> version=<v|-> grid=<cols>x<rows>|- first=<n> masked=<n> rows=<JSON array>`,
/// each row as the read gave it ([`Self::of_screen`]: every e-mail address and the composer's draft
/// masked), its trailing blanks cut; `first` is the grid row of the first, so the rows above it
/// (the transcript over a box) are none of the record's. A record, never a badge or an ask; the
/// `rows=` array, top to bottom, is a phase fixture's rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capture {
    /// The read's `seq=`.
    pub seq: u64,
    /// What the screen shows (`reset-dialog`, `reset-answer`, …), one word.
    pub what: String,
    /// The program whose screen it is (`status program=`).
    pub program: String,
    /// The program's own version, where it says one (Claude Code's transcript rows).
    pub version: Option<String>,
    /// The pane's columns and rows, where the read said them.
    pub grid: Option<(u16, usize)>,
    /// The grid row of the first row kept (`0` for a whole screen).
    pub first: usize,
    /// How many e-mail addresses [`Self::of_screen`] masked in `rows`.
    pub masked: usize,
    /// The rows, top to bottom.
    pub rows: Vec<String>,
}

/// `rows` with the ASCII letters and digits of each e-mail address in them masked
/// (`name@host.com` reads `xxxx@xxxx.xxx`), their other characters and so their widths kept, and
/// how many addresses it masked. An address is an `@` with a run of address characters on each
/// side (letters, digits and `._%+-` before it, letters, digits, `.` and `-` after it), a letter or
/// digit in the run before it, and a `.` with a letter or digit on each side in the run after it.
/// A row that ends in an address character is read on into the next row's first word where that
/// starts with one (an address the screen wrapped is masked whole). A word that opens with `@` (a
/// session's `@s-1`) is none.
#[must_use]
pub fn mask_addresses(rows: &[String]) -> (Vec<String>, usize) {
    let name_char = |c: char| host_char(c) || matches!(c, '_' | '%' | '+');
    let joins = |c: char| name_char(c) || c == '@';
    let mut rows: Vec<Vec<char>> = rows.iter().map(|row| row.chars().collect()).collect();
    // The rows as one text, each character's (row, column) beside it, a blank between two rows
    // unless the first ends in an address character and the second's first word starts with one.
    let mut text: Vec<(char, Option<(usize, usize)>)> = Vec::new();
    let mut runs_on = false;
    for (i, row) in rows.iter().enumerate() {
        let lead = row.iter().take_while(|c| c.is_whitespace()).count();
        let from = if runs_on && row.get(lead).copied().is_some_and(joins) {
            lead
        } else {
            text.push((' ', None));
            0
        };
        let end = row
            .iter()
            .rposition(|c| !c.is_whitespace())
            .map_or(0, |e| e + 1);
        text.extend((from..end.max(from)).map(|j| (row[j], Some((i, j)))));
        runs_on = end > 0 && joins(row[end - 1]);
    }
    let chars: Vec<char> = text.iter().map(|&(c, _)| c).collect();
    let spans = address_spans(&chars, name_char);
    for &(start, stop) in &spans {
        for &(_, at) in &text[start..stop] {
            if let Some((i, j)) = at
                && rows[i][j].is_ascii_alphanumeric()
            {
                rows[i][j] = 'x';
            }
        }
    }
    (
        rows.into_iter().map(String::from_iter).collect(),
        spans.len(),
    )
}

/// `rows` with the letters and digits of the composer's draft masked (`x`): the rows between the
/// composer's two rules ([`aterm_phase::phase::composer_rules`]), their other characters (the `❯`,
/// blanks, punctuation) kept. What the person typed and has not sent is none of a record's; the
/// rows under the composer (its mode and status rows, a footer notice) are kept as drawn, since a
/// reader reads a footer notice against them.
#[must_use]
pub fn mask_draft(rows: &[String]) -> Vec<String> {
    let mut rows = rows.to_vec();
    if let Some((top, bottom)) = aterm_phase::phase::composer_rules(&rows) {
        for row in rows.get_mut(top + 1..bottom).unwrap_or_default() {
            *row = row
                .chars()
                .map(|c| if c.is_alphanumeric() { 'x' } else { c })
                .collect();
        }
    }
    rows
}

/// A character an address's host may hold.
fn host_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '.' || c == '-'
}

/// The `[start, stop)` spans of the e-mail addresses in `text` ([`mask_addresses`]).
fn address_spans(text: &[char], name_char: impl Fn(char) -> bool) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    for (at, _) in text.iter().enumerate().filter(|(_, c)| **c == '@') {
        let start = text[..at]
            .iter()
            .rposition(|&c| !name_char(c))
            .map_or(0, |b| b + 1);
        let stop = at + 1 + text[at + 1..].iter().take_while(|&&c| host_char(c)).count();
        let (name, host) = (&text[start..at], &text[at + 1..stop]);
        let dotted = host
            .windows(3)
            .any(|w| w[0].is_ascii_alphanumeric() && w[1] == '.' && w[2].is_ascii_alphanumeric());
        if name.iter().any(char::is_ascii_alphanumeric) && dotted {
            spans.push((start, stop));
        }
    }
    spans
}

impl Capture {
    /// A capture of `rows`, the grid's from row `first`, with every e-mail address
    /// ([`mask_addresses`]) and the composer's draft ([`mask_draft`]) masked.
    #[must_use]
    pub fn of_screen(
        seq: u64,
        what: String,
        program: String,
        version: Option<String>,
        grid: Option<(u16, usize)>,
        first: usize,
        rows: &[String],
    ) -> Self {
        let (rows, masked) = mask_addresses(rows);
        let rows = mask_draft(&rows);
        Self {
            seq,
            what,
            program,
            version,
            grid,
            first,
            masked,
            rows,
        }
    }

    /// The journal line.
    #[must_use]
    pub fn line(&self) -> String {
        let rows = self
            .rows
            .iter()
            .map(|row| json_str(row.trim_end()))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "{CAPTURED} seq={} {} program={} version={} grid={} first={} masked={} rows=[{rows}]",
            self.seq,
            self.what,
            self.program,
            self.version.as_deref().unwrap_or("-"),
            self.grid
                .map_or_else(|| "-".to_string(), |(c, r)| format!("{c}x{r}")),
            self.first,
            self.masked,
        )
    }

    /// A [`Self::line`] read back; `None` for any other line.
    #[cfg(test)]
    #[must_use]
    pub fn of_line(line: &str) -> Option<Self> {
        fn field<'l>(words: &mut impl Iterator<Item = &'l str>, key: &str) -> Option<&'l str> {
            words.next()?.strip_prefix(key)?.strip_prefix('=')
        }
        let rest = line.strip_prefix(CAPTURED)?.strip_prefix(' ')?;
        let (head, rows) = rest.split_once(" rows=")?;
        let mut words = head.split(' ');
        let seq = field(&mut words, "seq")?.parse().ok()?;
        let what = words
            .next()
            .filter(|w| !w.is_empty() && !w.contains('='))?
            .to_string();
        let program = field(&mut words, "program")?.to_string();
        let version = Some(field(&mut words, "version")?)
            .filter(|v| *v != "-")
            .map(str::to_string);
        let grid = match field(&mut words, "grid")? {
            "-" => None,
            g => {
                let (c, r) = g.split_once('x')?;
                Some((c.parse().ok()?, r.parse().ok()?))
            }
        };
        let first = field(&mut words, "first")?.parse().ok()?;
        let masked = field(&mut words, "masked")?.parse().ok()?;
        if words.next().is_some() {
            return None;
        }
        let Json::Array(rows) = Json::parse(rows).ok()? else {
            return None;
        };
        let rows = rows
            .iter()
            .map(|row| row.as_str().map(str::to_string))
            .collect::<Option<Vec<_>>>()?;
        Some(Self {
            seq,
            what,
            program,
            version,
            grid,
            first,
            masked,
            rows,
        })
    }

    /// The phase fixture of the capture: its head line (the program and version, `CAPTURED` on
    /// `date` by the harness, the grid, the addresses masked, then `what`), then its rows.
    #[cfg(test)]
    #[must_use]
    pub fn fixture(&self, date: &str) -> String {
        let mut out = format!(
            "# {} {} · CAPTURED {date} by the harness's journal (seq={}, grid {}, first row {}, \
             {} e-mail addresses masked) · {}\n",
            if self.program == "claude" {
                "claude-code"
            } else {
                self.program.as_str()
            },
            self.version.as_deref().unwrap_or("unknown-version"),
            self.seq,
            self.grid
                .map_or_else(|| "-".to_string(), |(c, r)| format!("{c}x{r}")),
            self.first,
            self.masked,
            self.what,
        );
        for row in &self.rows {
            out.push_str(row);
            out.push('\n');
        }
        out
    }
}

/// How an `ESCALATED seq=<n> attention=<reply>` line's tail begins where no badge went.
pub const ATTENTION_SKIPPED: &str = "attention=skipped: ";
/// Why [`ATTENTION_SKIPPED`] behind another supervisor's claim.
pub const ESCALATED_BEHIND: &str = "another supervisor";
/// The `again=<why>` an `ESCALATED` line ends with where its badge is the same ask set again.
pub const AGAIN_RETRY: &str = "the retry hit the wall again";
/// The `again=<why>` of a limit episode's escalation over a badge adopted from a previous loop.
pub const AGAIN_ADOPTED: &str = "adopted from a previous watcher";
/// The `again=<why>` of the turn-end policy's badge set again as an Autopilot Off park ends.
pub const AGAIN_UNPARKED: &str = "set again after an off park";

/// Every word a journaled line is read by: the [`JournalRecord::kind`] of a line that begins
/// with it. A line that begins with any other word is `other`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Word {
    Event,
    Dismissed,
    Mail,
    Approved,
    Reconnect,
    Timeout,
    Exit,
    Extend,
    Escalated,
    Cleared,
    Continued,
    Typed,
    Restarted,
    Waiting,
    Skipped,
    Unverified,
    Limited,
    Unproven,
    Dialog,
    Declined,
    Chose,
    Exception,
    Probed,
    Reset,
    Needs,
    Hold,
    Occupancy,
    Probe,
}

impl Word {
    /// Every word, in declaration order.
    pub const ALL: [Self; 28] = [
        Self::Event,
        Self::Dismissed,
        Self::Mail,
        Self::Approved,
        Self::Reconnect,
        Self::Timeout,
        Self::Exit,
        Self::Extend,
        Self::Escalated,
        Self::Cleared,
        Self::Continued,
        Self::Typed,
        Self::Restarted,
        Self::Waiting,
        Self::Skipped,
        Self::Unverified,
        Self::Limited,
        Self::Unproven,
        Self::Dialog,
        Self::Declined,
        Self::Chose,
        Self::Exception,
        Self::Probed,
        Self::Reset,
        Self::Needs,
        Self::Hold,
        Self::Occupancy,
        Self::Probe,
    ];

    /// The record's `kind`: the printed word lowercased (`RECONNECTED` is `reconnect`).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Event => "event",
            Self::Dismissed => "dismissed",
            Self::Mail => "mail",
            Self::Approved => "approved",
            Self::Reconnect => "reconnect",
            Self::Timeout => "timeout",
            Self::Exit => "exit",
            Self::Extend => "extend",
            Self::Escalated => "escalated",
            Self::Cleared => "cleared",
            Self::Continued => "continued",
            Self::Typed => "typed",
            Self::Restarted => "restarted",
            Self::Waiting => "waiting",
            Self::Skipped => "skipped",
            Self::Unverified => "unverified",
            Self::Limited => "limited",
            Self::Unproven => "unproven",
            Self::Dialog => "dialog",
            Self::Declined => "declined",
            Self::Chose => "chose",
            Self::Exception => "exception",
            Self::Probed => "probed",
            Self::Reset => "reset",
            Self::Needs => "needs",
            Self::Hold => "hold",
            Self::Occupancy => "occupancy",
            Self::Probe => "probe",
        }
    }

    /// The word a line begins with as the loops print it ([`Self::printed`]; `RECONNECTED`
    /// is [`Self::Reconnect`] too).
    #[must_use]
    pub fn of_printed(word: &str) -> Option<Self> {
        match word {
            "RECONNECTED" => Some(Self::Reconnect),
            NEEDS => Some(Self::Needs),
            _ => Self::ALL.into_iter().find(|w| w.is_printed(word)),
        }
    }

    /// Whether `word` is [`Self::printed`], without building it.
    fn is_printed(self, word: &str) -> bool {
        let name = self.name();
        name.len() == word.len()
            && name
                .bytes()
                .zip(word.bytes())
                .all(|(n, w)| n.to_ascii_uppercase() == w)
    }

    /// How a line that is read as this word begins: its name in capitals.
    #[must_use]
    pub fn printed(self) -> String {
        self.name().to_ascii_uppercase()
    }

    /// The word whose [`Self::name`] a read record's `kind` is (`None`: `other`, or a word
    /// this build does not know).
    #[must_use]
    pub fn of_kind(kind: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|w| w.name() == kind)
    }
}

/// Every other word a loop journals a line with. Its record's `kind` stays `other` (readers
/// read these lines' first word) and a reader names it by [`Self::of_line`]; with [`Word`] it
/// covers every word a journal holds (a test build refuses any other, [`Journal::record`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Noted {
    Done,
    Left,
    Failed,
    Parked,
    Unparked,
    Halted,
    Unhalted,
    Refused,
    Held,
    HoldEnded,
    Adopted,
    Kept,
    Watching,
    Claim,
    Claimed,
    NotEscalated,
    Warn,
    Task,
    Fact,
    Launch,
    Host,
    Deferred,
    Memory,
    Stalled,
    Stuck,
    Frozen,
    Thawed,
    StallWarned,
    Signalled,
    RestartRefused,
    Note,
    Sleep,
    Abandoned,
    RepoUnread,
    Unstuck,
    Receipt,
    Captured,
}

impl Noted {
    /// Every word, in declaration order.
    pub const ALL: [Self; 37] = [
        Self::Done,
        Self::Left,
        Self::Failed,
        Self::Parked,
        Self::Unparked,
        Self::Halted,
        Self::Unhalted,
        Self::Refused,
        Self::Held,
        Self::HoldEnded,
        Self::Adopted,
        Self::Kept,
        Self::Watching,
        Self::Claim,
        Self::Claimed,
        Self::NotEscalated,
        Self::Warn,
        Self::Task,
        Self::Fact,
        Self::Launch,
        Self::Host,
        Self::Deferred,
        Self::Memory,
        Self::Stalled,
        Self::Stuck,
        Self::Frozen,
        Self::Thawed,
        Self::StallWarned,
        Self::Signalled,
        Self::RestartRefused,
        Self::Note,
        Self::Sleep,
        Self::Abandoned,
        Self::RepoUnread,
        Self::Unstuck,
        Self::Receipt,
        Self::Captured,
    ];

    /// How a line of this word begins (`NOT ESCALATED` is two words).
    #[must_use]
    pub const fn printed(self) -> &'static str {
        match self {
            Self::Done => "DONE",
            Self::Left => "LEFT",
            Self::Failed => "FAILED",
            Self::Parked => "PARKED",
            Self::Unparked => "UNPARKED",
            Self::Halted => "HALTED",
            Self::Unhalted => "UNHALTED",
            Self::Refused => "REFUSED",
            Self::Held => "HELD",
            Self::HoldEnded => "HOLD-ENDED",
            Self::Adopted => ADOPTED,
            Self::Kept => "KEPT",
            Self::Watching => "WATCHING",
            Self::Claim => "CLAIM",
            Self::Claimed => "CLAIMED",
            Self::NotEscalated => "NOT ESCALATED",
            Self::Warn => "WARN",
            Self::Task => "TASK",
            Self::Fact => "FACT",
            Self::Launch => "LAUNCH",
            Self::Host => "HOST",
            Self::Deferred => "DEFERRED",
            Self::Memory => "MEMORY",
            Self::Stalled => "STALLED",
            Self::Stuck => "STUCK",
            Self::Frozen => FROZEN,
            Self::Thawed => "THAWED",
            Self::StallWarned => "STALL-WARNED",
            Self::Signalled => "SIGNALLED",
            Self::RestartRefused => "RESTART-REFUSED",
            Self::Note => "NOTE",
            Self::Sleep => "SLEEP",
            Self::Abandoned => "ABANDONED",
            Self::RepoUnread => "REPO UNREAD",
            Self::Unstuck => UNSTUCK,
            Self::Receipt => crate::harness::land_receipt::RECEIPT,
            Self::Captured => CAPTURED,
        }
    }

    /// The word in lower case, a space as `-` (`not-escalated`): a reader's name for it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Left => "left",
            Self::Failed => "failed",
            Self::Parked => "parked",
            Self::Unparked => "unparked",
            Self::Halted => "halted",
            Self::Unhalted => "unhalted",
            Self::Refused => "refused",
            Self::Held => "held",
            Self::HoldEnded => "hold-ended",
            Self::Adopted => "adopted",
            Self::Kept => "kept",
            Self::Watching => "watching",
            Self::Claim => "claim",
            Self::Claimed => "claimed",
            Self::NotEscalated => "not-escalated",
            Self::Warn => "warn",
            Self::Task => "task",
            Self::Fact => "fact",
            Self::Launch => "launch",
            Self::Host => "host",
            Self::Deferred => "deferred",
            Self::Memory => "memory",
            Self::Stalled => "stalled",
            Self::Stuck => "stuck",
            Self::Frozen => "frozen",
            Self::Thawed => "thawed",
            Self::StallWarned => "stall-warned",
            Self::Signalled => "signalled",
            Self::RestartRefused => "restart-refused",
            Self::Note => "note",
            Self::Sleep => "sleep",
            Self::Abandoned => "abandoned",
            Self::RepoUnread => "repo-unread",
            Self::Unstuck => "unstuck",
            Self::Receipt => "receipt",
            Self::Captured => "captured",
        }
    }

    /// The word `line` begins with: its [`Self::printed`] form, then a space or the end.
    #[must_use]
    pub fn of_line(line: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|w| {
            line.strip_prefix(w.printed())
                .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
        })
    }
}

/// One journaled line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalRecord {
    /// When it was printed, Unix milliseconds.
    pub t: i64,
    /// The session the loop watched (the `@sid` it was given, without the `@`).
    pub sid: Option<String>,
    /// The line's leading word lowercased (`RECONNECTED` is `reconnect`).
    pub kind: String,
    /// The phase the line is about (an `EVENT`'s second word, `prompt` for an `APPROVED`).
    pub phase: String,
    /// The line's `seq=`.
    pub seq: Option<u64>,
    /// The line's `complete=` (an older build's `watch --report`).
    pub complete: Option<bool>,
    /// The line's `rows=` (an older build's `watch --report`).
    pub rows: Option<u64>,
    /// The line's free-text tail.
    pub summary: String,
    /// The line exactly as printed.
    pub line: String,
    /// The ledger turn the point's report counted from.
    pub turn: Option<u64>,
    /// The inbox row id of the report folded into an `EVENT turn` line.
    pub report: Option<u64>,
    /// The mark of the worker's last words at a turn end (`policy::turn_end::said_mark`).
    pub mark: Option<u64>,
    /// The time the end this note restates was first noted (unix ms).
    pub ended: Option<i64>,
}

/// The first word of `s` and the rest after one space.
fn split_word(s: &str) -> (&str, &str) {
    s.split_once(' ').unwrap_or((s, ""))
}

/// `key=<digits>` at the front of `s`: the number and the rest.
fn take_num<'s>(s: &'s str, key: &str) -> Option<(u64, &'s str)> {
    let (word, rest) = split_word(s);
    let v = word.strip_prefix(key)?.strip_prefix('=')?;
    if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((v.parse().ok()?, rest))
}

impl JournalRecord {
    /// The record of `line`, printed at `t`, read from the words the loop prints (module doc).
    pub fn of_line(t: i64, sid: Option<&str>, line: &str, turn: Option<u64>) -> Self {
        let mut rec = JournalRecord {
            t,
            sid: sid.map(|s| s.trim_start_matches('@').to_string()),
            kind: "other".to_string(),
            phase: "-".to_string(),
            seq: None,
            complete: None,
            rows: None,
            summary: String::new(),
            line: line.to_string(),
            turn,
            report: None,
            mark: None,
            ended: None,
        };
        let (word, rest) = split_word(line);
        let Some(word) = Word::of_printed(word) else {
            rec.summary = line.to_string();
            return rec;
        };
        rec.kind = word.name().to_string();
        let mut tail = rest;
        match word {
            Word::Event | Word::Dismissed => {
                let (phase, after) = split_word(rest);
                rec.phase = phase.to_string();
                tail = after;
                if let Some((seq, after)) = take_num(tail, "seq") {
                    rec.seq = Some(seq);
                    tail = after;
                }
                if let Some((c, after)) = take_num(tail, "complete")
                    && let Some((n, after)) = take_num(after, "rows")
                {
                    rec.complete = Some(c == 1);
                    rec.rows = Some(n);
                    tail = after;
                } else if let Some((id, after)) = take_num(tail, "report")
                    && let Some((n, after)) = take_num(after, "rows")
                {
                    rec.report = Some(id);
                    rec.rows = Some(n);
                    tail = after;
                }
            }
            Word::Mail | Word::Reconnect | Word::Timeout | Word::Exit | Word::Extend => {}
            Word::Approved => {
                rec.phase = "prompt".to_string();
                if let Some((seq, after)) = take_num(rest, "seq") {
                    rec.seq = Some(seq);
                    tail = after;
                }
            }
            Word::Escalated | Word::Cleared => {
                if let Some((seq, after)) = take_num(rest, "seq") {
                    rec.seq = Some(seq);
                    tail = after;
                }
            }
            Word::Continued
            | Word::Typed
            | Word::Restarted
            | Word::Waiting
            | Word::Skipped
            | Word::Unverified
            | Word::Limited
            | Word::Unproven
            | Word::Dialog
            | Word::Declined
            | Word::Chose
            | Word::Exception
            | Word::Probed
            | Word::Reset
            | Word::Needs => {
                if let Some((seq, after)) = take_num(rest, "seq") {
                    rec.seq = Some(seq);
                    tail = after;
                }
                let (first, after) = split_word(tail);
                if let Some(rule) = first.strip_prefix("rule=") {
                    rec.phase = rule.to_string();
                    tail = after;
                }
            }
            Word::Hold => {
                if let Some((seq, after)) = take_num(rest, "seq") {
                    rec.seq = Some(seq);
                    tail = after;
                }
                let (door, after) = split_word(tail);
                rec.phase = door.to_string();
                tail = after;
            }
            Word::Occupancy => {
                if let Some((seq, after)) = take_num(rest, "seq") {
                    rec.seq = Some(seq);
                    tail = after;
                }
                let (measure, after) = split_word(tail);
                rec.phase = measure.strip_prefix("rule=").unwrap_or(measure).to_string();
                tail = after;
            }
            Word::Probe => {
                let (what, after) = split_word(rest);
                rec.phase = what.to_string();
                tail = after;
                if let Some((seq, after)) = take_num(tail, "seq") {
                    rec.seq = Some(seq);
                    tail = after;
                }
            }
        }
        rec.summary = tail.to_string();
        rec
    }

    /// One JSON object, no newline, keys in the order the module doc lists.
    pub fn to_json(&self) -> String {
        let opt_str = |s: &Option<String>| s.as_deref().map_or("null".to_string(), json_str);
        let opt_num = |n: Option<u64>| n.map_or("null".to_string(), |n| n.to_string());
        let mark = self
            .mark
            .map_or_else(String::new, |m| format!(",\"mark\":\"{m:016x}\""));
        let ended = self
            .ended
            .map_or_else(String::new, |e| format!(",\"ended\":{e}"));
        format!(
            "{{\"t\":{},\"sid\":{},\"kind\":{},\"phase\":{},\"seq\":{},\"complete\":{},\
             \"rows\":{},\"summary\":{},\"line\":{},\"turn\":{},\"report\":{}{mark}{ended}}}",
            self.t,
            opt_str(&self.sid),
            json_str(&self.kind),
            json_str(&self.phase),
            opt_num(self.seq),
            self.complete.map_or("null", |c| if c { "1" } else { "0" }),
            opt_num(self.rows),
            json_str(&self.summary),
            json_str(&self.line),
            opt_num(self.turn),
            opt_num(self.report),
        )
    }

    /// Read one journal line back.
    pub fn from_json(text: &str) -> Result<Self, String> {
        let v = Json::parse(text.trim())?;
        let s = |k: &str| v.get(k).and_then(Json::as_str).map(str::to_string);
        let n = |k: &str| v.get(k).and_then(Json::as_u64);
        let t = match v.get("t") {
            Some(Json::Number(n)) => n.parse::<i64>().map_err(|_| format!("t={n} is not ms"))?,
            _ => return Err("no \"t\"".to_string()),
        };
        Ok(JournalRecord {
            t,
            sid: s("sid"),
            kind: s("kind").ok_or("no \"kind\"")?,
            phase: s("phase").unwrap_or_else(|| "-".to_string()),
            seq: n("seq"),
            complete: n("complete").map(|c| c == 1),
            rows: n("rows"),
            summary: s("summary").unwrap_or_default(),
            line: s("line").ok_or("no \"line\"")?,
            turn: n("turn"),
            report: n("report"),
            mark: s("mark").and_then(|h| {
                (!h.is_empty() && h.bytes().all(|b| b.is_ascii_hexdigit()))
                    .then(|| u64::from_str_radix(&h, 16).ok())
                    .flatten()
            }),
            ended: match v.get("ended") {
                Some(Json::Number(n)) => n.parse::<i64>().ok(),
                _ => None,
            },
        })
    }
}

/// What an app log may keep of a line a loop printed: the head [`JournalRecord::of_line`] reads
/// (the printed word; an `EVENT`'s or `DISMISSED`'s phase; `seq=`; `complete=`/`rows=` or
/// `report=`/`rows=`; a `rule=`; `HOLD`'s door, `OCCUPANCY`'s measure, `PROBE`'s what), each
/// token kept only where it is a word of `[A-Za-z0-9@._:=-]` (else `?`), then ` text=<n>B`, the
/// free-text tail's length in bytes (none for an empty tail). A line that begins with no [`Word`]
/// is its [`Noted`] word (`NOT ESCALATED`, `WATCHING`, …) or `other`, then the length of the rest.
/// The tail is never read again: for a line the loop printed, the result depends on its tail only
/// through the tail's length.
#[must_use]
pub fn log_words(line: &str) -> String {
    let rec = JournalRecord::of_line(0, None, line, None);
    let (head, tail) = if Word::of_kind(&rec.kind).is_some() {
        // `summary` is a suffix of `line` (each reader step takes a slice's rest).
        let cut = line.len().saturating_sub(rec.summary.len());
        (line.get(..cut).unwrap_or_default(), rec.summary.len())
    } else {
        match Noted::of_line(line) {
            Some(noted) => {
                let rest = line.get(noted.printed().len()..).unwrap_or_default();
                (
                    noted.printed(),
                    rest.strip_prefix(' ').unwrap_or(rest).len(),
                )
            }
            None => ("other", line.len()),
        }
    };
    let mut out = String::with_capacity(head.len().saturating_add(16));
    for token in head.split(' ').filter(|t| !t.is_empty()) {
        if !out.is_empty() {
            out.push(' ');
        }
        let word = token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"@._:=-".contains(&b));
        out.push_str(if word { token } else { "?" });
    }
    if tail > 0 {
        out.push_str(&format!(" text={tail}B"));
    }
    out
}

/// `s` as a quoted JSON string through the control path's one escape.
pub fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len().saturating_add(2));
    out.push('"');
    aterm_codec::json::escape_into(&mut out, s);
    out.push('"');
    out
}

/// The current time, Unix milliseconds.
pub fn unix_ms() -> i64 {
    aterm_time::unix_millis().map_or(0, |ms| i64::try_from(ms).unwrap_or(i64::MAX))
}

#[derive(Debug)]
enum JournalTarget {
    Off,
    Named(PathBuf),
    Append(PathBuf),
}

/// The journal of one loop (or none), reopened under its path lock for each append.
/// On failure it attempts one warning and flush. Warning metadata uses visible terminal
/// escapes; later warnings are suppressed even if delivery failed. Appends can still recover.
#[derive(Debug)]
pub struct Journal {
    target: JournalTarget,
    sid: Option<String>,
    /// The one warning was said.
    warned: bool,
}

impl Journal {
    /// No journal: [`Self::record`] does nothing.
    pub const fn off() -> Self {
        Self {
            target: JournalTarget::Off,
            sid: None,
            warned: false,
        }
    }

    /// A journal at `path` for a loop on `sid` that is named and never opened.
    #[must_use]
    pub fn named(path: Option<&Path>, sid: Option<&str>) -> Self {
        let mut j = Self::off();
        if let Some(path) = path {
            j.target = JournalTarget::Named(path.to_path_buf());
            j.sid = sid.map(str::to_string);
        }
        j
    }

    /// Open `path` append-only (created `0600` when missing) for a loop on `sid`.
    pub fn open(path: Option<&Path>, sid: Option<&str>, warn: &mut dyn Write) -> Self {
        let mut j = Self::off();
        let Some(path) = path else {
            return j;
        };
        j.target = JournalTarget::Append(path.to_path_buf());
        j.sid = sid.map(str::to_string);
        let opened = aterm_uds::line_log::ensure(path);
        if let Err(e) = opened {
            j.failed(&e, "cannot open it", warn);
        }
        j
    }

    fn path(&self) -> Option<&Path> {
        match &self.target {
            JournalTarget::Off => None,
            JournalTarget::Named(path) | JournalTarget::Append(path) => Some(path),
        }
    }

    /// Appends the record of `line` ([`JournalRecord::of_line`]), stamped `t_ms`. A test build
    /// refuses a line the ledger would read `unread` (`ledger::assert_marked`): every line a
    /// loop journals begins with a [`Word`] or a [`Noted`] word.
    pub fn record(&mut self, t_ms: i64, line: &str, warn: &mut dyn Write) -> Option<String> {
        self.record_marked(t_ms, line, None, warn)
    }

    /// [`Self::record`] of a turn end's line, with the mark of the worker's last words.
    pub fn record_marked(
        &mut self,
        t_ms: i64,
        line: &str,
        mark: Option<u64>,
        warn: &mut dyn Write,
    ) -> Option<String> {
        self.record_with(t_ms, line, mark, None, warn)
    }

    /// [`Self::record`] of a task end's note that restates an end a loop before this one noted.
    pub fn record_ended(
        &mut self,
        t_ms: i64,
        line: &str,
        ended: Option<i64>,
        warn: &mut dyn Write,
    ) -> Option<String> {
        self.record_with(t_ms, line, None, ended, warn)
    }

    /// [`Self::record`] with the record's mark and end time.
    fn record_with(
        &mut self,
        t_ms: i64,
        line: &str,
        mark: Option<u64>,
        ended: Option<i64>,
        warn: &mut dyn Write,
    ) -> Option<String> {
        #[cfg(test)]
        super::ledger::assert_marked(line);
        self.path()?;
        let mut rec = JournalRecord::of_line(t_ms, self.sid.as_deref(), line, None);
        rec.mark = mark;
        rec.ended = ended;
        let mut text = rec.to_json();
        if let JournalTarget::Append(path) = &self.target
            && let Err(e) = aterm_uds::line_log::append(path, &text)
        {
            self.failed(&e, "cannot append to it", warn);
        }
        text.push('\n');
        Some(text)
    }

    /// Whether a journal was given (a file named, opened or not).
    #[must_use]
    pub const fn is_on(&self) -> bool {
        !matches!(self.target, JournalTarget::Off)
    }

    /// Append one line that is already a JSON object.
    pub fn append_raw(&mut self, json: &str, warn: &mut dyn Write) -> bool {
        let res = match &self.target {
            JournalTarget::Append(path) => aterm_uds::line_log::append(path, json),
            JournalTarget::Off | JournalTarget::Named(_) => return false,
        };
        if let Err(e) = res {
            self.failed(&e, "cannot append to it", warn);
            return false;
        }
        true
    }

    /// A write that failed: warned once ([`Self::warn`]); an out-of-space or quota error also
    /// asks the window's disk watch to look now ([`crate::harness::disk::poke`]).
    fn failed(&mut self, e: &std::io::Error, what: &str, warn: &mut dyn Write) {
        if crate::harness::disk::poke::is_out_of_space(e) {
            let path = self
                .path()
                .map_or_else(String::new, |p| p.display().to_string());
            crate::harness::disk::poke::poke(crate::harness::disk::poke::Cause::Write(format!(
                "journal {path}: {e}"
            )));
        }
        self.warn(&format!("{what}: {e}"), warn);
    }

    fn warn(&mut self, why: &str, warn: &mut dyn Write) {
        if std::mem::replace(&mut self.warned, true) {
            return;
        }
        let path = self
            .path()
            .map_or_else(String::new, |p| p.display().to_string());
        let _ = aterm_codec::tty::write_line(
            warn,
            format_args!("aterm drive: journal {path}: {why} — the loop goes on without it"),
        );
        let _ = warn.flush();
    }
}

/// Every record in a journal file, and how many of its non-blank lines are not one.
pub fn read_journal(path: &Path) -> Result<(Vec<JournalRecord>, usize), String> {
    let text = match crate::harness::accounts::read_bounded(path, 64 * 1024 * 1024) {
        Ok(Some(text)) => text,
        Ok(None) => {
            return Err(format!(
                "{}: {}",
                path.display(),
                std::io::Error::from(std::io::ErrorKind::NotFound)
            ));
        }
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let mut out = Vec::new();
    let mut bad = 0;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        match JournalRecord::from_json(line) {
            Ok(r) => out.push(r),
            Err(_) => bad += 1,
        }
    }
    Ok((out, bad))
}

#[cfg(test)]
#[path = "journal_warning_tests.rs"]
mod warning_tests;

#[cfg(test)]
#[path = "journal_tests.rs"]
mod tests;
