// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The step fold: tool calls, turn ends and walls from Claude Code's transcript, for the session log, with no text kept.

use std::collections::VecDeque;

use aterm_json::{Map, Value};
use aterm_phase::WallKind;
use aterm_phase::anchors::anchor_text;

use super::transcript;
use super::usage::{Seen, SeenIds, TokenUsage};
use crate::supervise::run::wall_confirm::{self, Class, Entry, Row, TranscriptEnd};

/// The Claude Code builds whose transcript rows the lab read, each with its key-only reduction committed (`harness/testdata/steps/<build>.jsonl`).
pub const MEASURED_BUILDS: &[&str] = &["2.1.303"];

/// The most steps the fold holds between two takes; past it the oldest is dropped and counted ([`StepBatch::overflow`]).
pub const STEP_QUEUE: usize = 1024;

/// The most message ids a vendor turn's token count dedupes.
pub const TURN_IDS_MAX: usize = 64;

/// How many decoded bytes of a user row's first text the shape test reads.
const LEAD_BYTES: usize = 63;

/// A background task's wake (`<task-notification>…`): Claude Code's own, no prompt (`upgrade::asked_by` reads it as nobody's).
pub(crate) const TASK_NOTIFICATION: &str = "<task-notification>";
/// A shell-mode command (`! cmd`), which asks the model nothing.
pub(crate) const BASH_INPUT: &str = "<bash-input>";
/// A shell-mode command's output.
pub(crate) const BASH_STDOUT: &str = "<bash-stdout>";
/// A shell-mode command's error output.
pub(crate) const BASH_STDERR: &str = "<bash-stderr>";
/// Esc's row (`[Request interrupted by user]`, `… for tool use]`).
pub(crate) const INTERRUPTED: &str = "[Request interrupted by user";

/// A LOCAL COMMAND's own rows, as `wall_confirm`'s `local_command` reads a
/// user row's string content: no turn of the conversation's.
const LOCAL_COMMAND: [&str; 4] = [
    "<command-name>",
    "<command-message>",
    "<local-command-stdout>",
    "<local-command-stderr>",
];

/// A compaction's own command row.
pub const COMPACT: &str = "/compact";

/// The words a user row opens with that the shape test reads.
pub const USER_MARKERS: [&str; 10] = [
    TASK_NOTIFICATION,
    BASH_INPUT,
    BASH_STDOUT,
    BASH_STDERR,
    INTERRUPTED,
    LOCAL_COMMAND[0],
    LOCAL_COMMAND[1],
    LOCAL_COMMAND[2],
    LOCAL_COMMAND[3],
    super::upgrade::MISSED_TASK_NOTICE,
];

const KEY_TOOL_USE: &str = anchor_text("transcript.tool_use");
const KEY_TOOL_RESULT: &str = anchor_text("transcript.tool_result");
const KEY_TOOL_USE_ID: &str = anchor_text("transcript.tool_use_id");
const KEY_IS_ERROR: &str = anchor_text("transcript.is_error");
const KEY_TURN_DURATION: &str = anchor_text("transcript.turn_duration");
const KEY_DURATION_MS: &str = anchor_text("transcript.durationMs");
const KEY_COMPACT_BOUNDARY: &str = anchor_text("transcript.compact_boundary");
const KEY_COMPACT_SUMMARY: &str = anchor_text("transcript.isCompactSummary");
const KEY_QUEUE_TRANSCRIPT_ONLY: &str = anchor_text("transcript.queueTranscriptOnly");
const KEY_API_ERROR: &str = anchor_text("transcript.isApiErrorMessage");
const KEY_SIDECHAIN: &str = anchor_text("transcript.isSidechain");
const KEY_META: &str = anchor_text("transcript.isMeta");
const KEY_PROMPT_ID: &str = anchor_text("transcript.promptId");
const KEY_PARENT: &str = anchor_text("transcript.parentUuid");
const KEY_LOCAL_COMMAND: &str = anchor_text("transcript.local_command");

/// An id as the session log takes one (`aterm_slog::ShortId`'s rule): 1 to 40 bytes of `[A-Za-z0-9._:@-]`.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct StepId {
    bytes: [u8; StepId::MAX],
    len: u8,
}

impl StepId {
    /// The longest id.
    pub const MAX: usize = 40;

    /// `s` as an id, or `None` when it is empty, too long or holds a byte outside the alphabet.
    #[must_use]
    pub fn new(s: &str) -> Option<StepId> {
        let raw = s.as_bytes();
        if raw.is_empty()
            || raw.len() > Self::MAX
            || !raw.iter().all(|&b| {
                b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'@' | b'-')
            })
        {
            return None;
        }
        let mut bytes = [0u8; Self::MAX];
        bytes[..raw.len()].copy_from_slice(raw);
        Some(StepId {
            bytes,
            len: u8::try_from(raw.len()).ok()?,
        })
    }

    /// The id's text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..usize::from(self.len)]).unwrap_or("-")
    }
}

impl std::fmt::Debug for StepId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "StepId({:?})", self.as_str())
    }
}

/// A vendor tool's name (`Bash`, `mcp__<server>__<tool>`), bounded at the fold.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ToolName {
    bytes: [u8; ToolName::MAX],
    len: u8,
}

impl ToolName {
    /// The longest name kept.
    pub const MAX: usize = 64;

    /// `s`, bounded.
    #[must_use]
    pub fn new(s: &str) -> ToolName {
        let mut bytes = [0u8; Self::MAX];
        let mut len = 0usize;
        for c in s.chars().filter(|c| !c.is_control()) {
            let w = c.len_utf8();
            if len + w > Self::MAX {
                break;
            }
            c.encode_utf8(&mut bytes[len..len + w]);
            len += w;
        }
        ToolName {
            bytes,
            len: u8::try_from(len).unwrap_or(0),
        }
    }

    /// The name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..usize::from(self.len)]).unwrap_or("")
    }
}

impl std::fmt::Debug for ToolName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ToolName({:?})", self.as_str())
    }
}

/// The class of a vendor tool by its name, and whether it starts a subagent.
#[must_use]
pub fn tool_class(name: &str) -> (&'static str, bool) {
    match name {
        "Bash" | "BashOutput" | "KillShell" => ("shell", false),
        "Edit" | "MultiEdit" | "NotebookEdit" => ("edit", false),
        "Write" => ("write", false),
        "Read" | "NotebookRead" => ("read", false),
        "Grep" | "Glob" | "LS" => ("search", false),
        "WebFetch" | "WebSearch" => ("web", false),
        "Task" | "Agent" => ("agent", true),
        "TodoWrite" | "ExitPlanMode" | "EnterPlanMode" => ("plan", false),
        n if n.starts_with("mcp__") => ("mcp", false),
        _ => ("other", false),
    }
}

/// Where a step was read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StepPos {
    /// The path's fingerprint (FNV-1a/64 of its bytes).
    pub fp: u64,
    /// The file's `(dev, ino)`, when known.
    pub id: Option<(u64, u64)>,
    /// The offset.
    pub off: u64,
    /// The row's time (wall ms); for a [`StepEvent::Bind`], the floor.
    pub at_ms: u64,
}

/// A watermark the log holds for a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StepMark {
    /// The path's fingerprint.
    pub fp: u64,
    /// The file's identity, when known.
    pub id: Option<(u64, u64)>,
    /// The offset applied through.
    pub off: u64,
}

/// Whether a step's build was measured.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StepState {
    /// A build [`MEASURED_BUILDS`] names: exact.
    On,
    /// Any other: strong, additive only.
    Partial,
}

/// One transcript fact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepEvent {
    /// The fold bound a file: its first `len` bytes are history.
    Bind {
        /// The file's length at the bind; for the file the registry named
        /// while it did not exist ([`StepFold::note_vacant`]), its seed
        /// (none of it, but a watermark the log held).
        len: u64,
    },
    /// A user row that starts a vendor turn.
    Prompt {
        /// The vendor turn's id.
        vref: StepId,
    },
    /// Esc's row: the open turn was interrupted.
    Interrupt,
    /// A `tool_use` part of an assistant row.
    ToolUse {
        /// The call's id.
        id: StepId,
        /// The tool's name.
        tool: ToolName,
        /// Its class ([`tool_class`]).
        cls: &'static str,
        /// A subagent's call.
        subagent: bool,
    },
    /// A `tool_result` part of a user row.
    ToolResult {
        /// The call's id.
        id: StepId,
        /// Its `is_error`.
        error: bool,
    },
    /// `system subtype=turn_duration`: the vendor turn ended.
    TurnEnd {
        /// Its `durationMs`.
        vdur_ms: u64,
        /// Input tokens over its rows (input, cache writes and reads), each
        /// message counted once at its largest ([`TURN_IDS_MAX`]).
        tok_in: u64,
        /// Output tokens over its rows.
        tok_out: u64,
    },
    /// The turn the row just ended ended on Claude Code's own notice of this wall (the harness's `transcript_wall`).
    EndWall {
        /// The notice's wall.
        kind: WallKind,
    },
    /// `system subtype=compact_boundary`.
    Compact,
    /// A main-chain assistant row whose model or effort is not the last one this fold said (the first after a bind included).
    Model {
        /// `message.model`.
        model: StepId,
        /// The row's `effort`.
        effort: Option<StepId>,
        /// The build that wrote the row.
        ver: Option<StepId>,
    },
    /// The fold latched off for this path.
    Off {
        /// Why (`unknown`: a discriminant of the wrong shape).
        why: &'static str,
    },
}

/// One step, where it was read and at which state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Step {
    /// Where.
    pub pos: StepPos,
    /// Measured or not.
    pub state: StepState,
    /// What.
    pub ev: StepEvent,
}

/// What a take hands the glue.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StepBatch {
    /// The steps, oldest first.
    pub steps: Vec<Step>,
    /// Steps dropped from a full queue (the oldest): the join is desynced until the next prompt (`TxEvent::Overflow`).
    pub overflow: u64,
    /// Rows past the seed the fold could not read (not UTF-8, not JSON, longer than the line bound): `TxEvent::Lost`.
    pub lost: u64,
    /// The main chain's end after the last row read.
    pub end: End,
}

/// What the newest main-chain entry is (the harness's `Entry`, without its text, and the opaque local command).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum EndClass {
    /// Nothing of the conversation's yet.
    #[default]
    Nothing,
    /// Claude Code's own notice.
    Notice,
    /// A tool's result.
    ToolResult,
    /// A turn's input: a prompt, a shell-mode command and its output.
    Input,
    /// The model's words or call.
    Reply,
    /// A `system` `local_command` row, whose content is not read.
    Opaque,
}

/// The main chain's end, as the wall judge reads it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct End {
    /// The newest entry.
    pub last: EndClass,
    /// A `turn_duration` closed the turn after it.
    pub closed: bool,
    /// When the newest entry's row was written (wall ms).
    pub at_ms: Option<u64>,
    /// A notice's wall once its turn is closed (`transcript_wall`).
    wall: Option<WallKind>,
    /// The classes a notice confirms (`confirms`), one bit each.
    classes: u8,
}

/// One screen wall of each class, for asking `confirms` once per class.
const CLASSES: [WallKind; 5] = [
    WallKind::UsageSession,
    WallKind::Context,
    WallKind::Auth,
    WallKind::Overloaded,
    WallKind::Memory,
];

const fn bit(c: Class) -> u8 {
    match c {
        Class::Limit => 1,
        Class::Context => 2,
        Class::Auth => 4,
        Class::Api => 8,
        Class::Memory => 16,
    }
}

impl End {
    /// The transcript has not caught up with the turn the screen ended.
    #[must_use]
    pub fn lagging(&self) -> bool {
        !self.closed && self.last != EndClass::Notice
    }

    /// The end confirms a screen wall of kind `screen`.
    #[must_use]
    pub fn confirms(&self, screen: WallKind) -> bool {
        self.last == EndClass::Notice && self.classes & bit(wall_confirm::class(screen)) != 0
    }

    /// The wall a closed turn's notice is (the harness's `transcript_wall`).
    #[must_use]
    pub fn wall(&self) -> Option<WallKind> {
        if self.closed && self.last == EndClass::Notice {
            self.wall
        } else {
            None
        }
    }
}

/// The wall judge's answer for a screen wall.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Judge {
    /// The end is Claude Code's own notice of the wall's class.
    Confirm,
    /// The end is newer than the wall's turn and is no such notice.
    Refute,
    /// Not now: the transcript lags, its end is opaque or older than the turn, or the wall is one the transcript never writes.
    Wait,
}

/// The wall judge: the harness's own rule, so the log and the harness never disagree on a wall.
#[must_use]
pub fn judge_wall(screen: WallKind, end: &End, turn_opened_ms: u64) -> Judge {
    if screen == WallKind::Memory || end.last == EndClass::Opaque {
        return Judge::Wait;
    }
    if end.confirms(screen) {
        return Judge::Confirm;
    }
    if end.lagging() || end.at_ms.is_none_or(|t| t < turn_opened_ms) {
        return Judge::Wait;
    }
    Judge::Refute
}

/// The incremental `transcript_ends_on`: the same loop, row by row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct EndFold {
    end: End,
}

/// What the end reads of a main-chain row.
enum EndRow {
    Entry(EndClass, Option<(Option<WallKind>, u8)>),
    Closed,
    None,
}

impl EndFold {
    fn push(&mut self, row: EndRow, opens: bool, at_ms: Option<u64>) {
        match row {
            EndRow::Closed => self.end.closed = true,
            EndRow::Entry(class, notice) => {
                if class == EndClass::Input && opens && self.end.last != EndClass::Nothing {
                    return;
                }
                let (wall, classes) = notice.unwrap_or((None, 0));
                self.end = End {
                    last: class,
                    closed: false,
                    at_ms,
                    wall,
                    classes,
                };
            }
            EndRow::None => {}
        }
    }
}

/// A notice row (`isApiErrorMessage`) read as the harness reads it, its
/// classes and wall kept and its text dropped.
fn notice_of(line: &str) -> (Option<WallKind>, u8) {
    notice_read(line).map_or((None, 0), |n| {
        let classes = CLASSES
            .iter()
            .zip(n.confirms)
            .filter(|(_, confirms)| *confirms)
            .fold(0, |c, (&k, _)| c | bit(wall_confirm::class(k)));
        (n.wall, classes)
    })
}

/// What the harness reads of one notice row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NoticeRead {
    /// Its words: every text part joined, or the command's output in its tags.
    pub text: String,
    /// Its `error` word.
    pub error: Option<String>,
    /// Its typed `apiError` (since 2.1.295).
    pub api_error: Option<String>,
    /// The status it answered (`apiErrorStatus`).
    pub status: Option<u16>,
    /// Which of the account's limits it was written for (`quotaLimits.rateLimitType`).
    pub limit: Option<String>,
    /// The wall it is once its turn is closed (`transcript_wall`).
    pub wall: Option<WallKind>,
    /// Whether it confirms a screen wall of each class, in the order of a usage limit, a full context, the lost login, an API error and the memory wall (`confirms`, which reads the screen's class alone).
    pub confirms: [bool; 5],
    /// Which limit it names (`limit_named`: by its words, else by its `rateLimitType`).
    pub named: Option<WallKind>,
}

/// [`NoticeRead`] of one transcript line; `None` for a line that is no notice, and one that is no JSON.
#[must_use]
pub fn notice_read(line: &str) -> Option<NoticeRead> {
    let v = transcript::conversation(line).ok()?;
    let Some(Row::Entry(entry @ Entry::Notice { .. })) = wall_confirm::row_of(&v) else {
        return None;
    };
    let end = TranscriptEnd {
        last: entry,
        closed: true,
        row: None,
        at: None,
        prompted: false,
    };
    let wall = wall_confirm::transcript_wall(&end);
    let confirms = CLASSES.map(|k| wall_confirm::confirms(k, &end));
    let Entry::Notice {
        text,
        error,
        api_error,
        status,
        limit,
    } = end.last
    else {
        return None;
    };
    let named = wall_confirm::limit_named(&text, limit.as_deref());
    Some(NoticeRead {
        text,
        error,
        api_error,
        status,
        limit,
        wall,
        confirms,
        named,
    })
}

/// Whether a row's content parts are needed.
pub(super) fn wants_parts(line: &str) -> bool {
    [KEY_TOOL_USE_ID, KEY_TOOL_RESULT, KEY_TOOL_USE]
        .iter()
        .any(|k| quoted(line, k))
        || line.contains("\\u")
}

/// Whether `"<word>"` occurs in `line`.
pub(super) fn quoted(line: &str, word: &str) -> bool {
    let b = line.as_bytes();
    let w = word.as_bytes();
    let mut from = 0;
    while let Some(i) = line[from..].find(word) {
        let at = from + i;
        if at > 0 && b[at - 1] == b'"' && b.get(at + w.len()) == Some(&b'"') {
            return true;
        }
        from = at + 1;
    }
    false
}

/// The first decoded bytes of a user row's first text: its string content,
/// or its content's first `text` part, with whether it was the string and
/// whether it was read whole.
struct Lead {
    bytes: [u8; LEAD_BYTES],
    len: usize,
    string: bool,
    whole: bool,
}

impl Lead {
    fn text(&self) -> &str {
        let b = &self.bytes[..self.len];
        match std::str::from_utf8(b) {
            Ok(s) => s,
            Err(e) => std::str::from_utf8(&b[..e.valid_up_to()]).unwrap_or(""),
        }
    }

    /// Whether this is a local command's own row (`wall_confirm`'s
    /// `local_command`, on a string content only, as `user_entry` asks).
    fn local(&self) -> bool {
        if !self.string {
            return false;
        }
        let t = self.text().trim_start();
        (self.whole && t.trim_end() == COMPACT)
            || t.strip_prefix(COMPACT).is_some_and(|r| r.starts_with(' '))
            || LOCAL_COMMAND.iter().any(|m| t.starts_with(m))
    }

    fn starts(&self, marker: &str) -> bool {
        self.text().trim_start().starts_with(marker)
    }
}

/// [`Lead`] of `line`, found after its `"message":` key; `None` where the
/// content is neither a string nor holds a text part.
fn lead(line: &str) -> Option<Lead> {
    let m = line.find("\"message\":")?;
    let rest = &line[m..];
    let c = rest.find("\"content\":")?;
    let after = rest[c + "\"content\":".len()..].trim_start();
    let (string, body) = if let Some(s) = after.strip_prefix('"') {
        (true, s)
    } else if after.starts_with('[') {
        let t = after.find("\"text\":")?;
        let s = after[t + "\"text\":".len()..]
            .trim_start()
            .strip_prefix('"')?;
        (false, s)
    } else {
        return None;
    };
    let mut lead = Lead {
        bytes: [0u8; LEAD_BYTES],
        len: 0,
        string,
        whole: false,
    };
    let b = body.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let (c, used) = match b[i] {
            b'"' => {
                lead.whole = true;
                break;
            }
            b'\\' => match b.get(i + 1) {
                Some(b'n') => ('\n', 2),
                Some(b't') => ('\t', 2),
                Some(b'r') => ('\r', 2),
                Some(b'b') => ('\u{8}', 2),
                Some(b'f') => ('\u{c}', 2),
                Some(b'"') => ('"', 2),
                Some(b'\\') => ('\\', 2),
                Some(b'/') => ('/', 2),
                Some(b'u') => match unicode_escape(&b[i..]) {
                    Some(pair) => pair,
                    None => break,
                },
                _ => break,
            },
            _ => {
                let n = b[i..]
                    .iter()
                    .position(|&x| x == b'"' || x == b'\\')
                    .unwrap_or(b.len() - i);
                let room = LEAD_BYTES - lead.len;
                let take = n.min(room);
                lead.bytes[lead.len..lead.len + take].copy_from_slice(&b[i..i + take]);
                lead.len += take;
                if take < n {
                    return Some(lead);
                }
                i += n;
                continue;
            }
        };
        let w = c.len_utf8();
        if lead.len + w > LEAD_BYTES {
            return Some(lead);
        }
        c.encode_utf8(&mut lead.bytes[lead.len..lead.len + w]);
        lead.len += w;
        i += used;
    }
    Some(lead)
}

/// A `\uXXXX` escape at the start of `b` (a surrogate pair as one), and how
/// many bytes it spans.
fn unicode_escape(b: &[u8]) -> Option<(char, usize)> {
    let hex = |s: &[u8]| -> Option<u32> {
        let t = std::str::from_utf8(s.get(2..6)?).ok()?;
        u32::from_str_radix(t, 16).ok()
    };
    let hi = hex(b)?;
    if (0xD800..0xDC00).contains(&hi) {
        let rest = b.get(6..)?;
        if rest.first() != Some(&b'\\') || rest.get(1) != Some(&b'u') {
            return None;
        }
        let lo = hex(rest)?;
        if !(0xDC00..0xE000).contains(&lo) {
            return None;
        }
        let c = char::from_u32(0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00))?;
        return Some((c, 12));
    }
    Some((char::from_u32(hi)?, 6))
}

/// A row's `timestamp` as wall ms: whole seconds (`parse_utc_fractional`'s
/// rule) and the first three digits of its fraction.
fn stamp_ms(s: &str) -> Option<u64> {
    let secs = u64::try_from(aterm_types::rfc3339::parse_utc_fractional(s)?).ok()?;
    let ms = s
        .get(19..)
        .and_then(|f| f.strip_prefix('.'))
        .map_or(0, |f| {
            f.bytes()
                .take_while(u8::is_ascii_digit)
                .chain(std::iter::repeat(b'0'))
                .take(3)
                .fold(0u64, |a, d| a * 10 + u64::from(d - b'0'))
        });
    secs.checked_mul(1000)?.checked_add(ms)
}

fn flag(obj: &Map, key: &str) -> bool {
    obj.get(key).and_then(Value::as_bool) == Some(true)
}

/// A count as a row writes it: a whole number, else a finite non-negative one cut to whole.
pub(super) fn number(v: Option<&Value>) -> Option<u64> {
    let v = v?;
    v.as_u64().or_else(|| {
        v.as_f64()
            .filter(|f| f.is_finite() && *f >= 0.0)
            .map(|f| f as u64)
    })
}

/// A vendor turn's tokens, by the usage fold's dedupe.
#[derive(Clone, Debug, Default)]
struct TurnTokens {
    seen: SeenIds,
    tok_in: u64,
    tok_out: u64,
}

impl TurnTokens {
    const fn add(&mut self, u: TokenUsage) {
        self.tok_in = self
            .tok_in
            .saturating_add(u.input)
            .saturating_add(u.cache_write)
            .saturating_add(u.cache_read);
        self.tok_out = self.tok_out.saturating_add(u.output);
    }

    fn note(&mut self, id: Option<&str>, u: TokenUsage) {
        let Some(id) = id else {
            self.add(u);
            return;
        };
        match self.seen.remember(id, u) {
            Seen::New => self.add(u),
            Seen::Same => {}
            Seen::Differs(growth) => self.add(growth),
        }
        self.seen.shed(TURN_IDS_MAX);
    }
}

/// The session the registry named with no transcript, and the file a
/// later read found for it ([`StepFold::note_vacant`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Vacancy {
    /// The session id's fingerprint.
    sid: u64,
    /// Its file's fingerprint, once a read found it.
    fp: Option<u64>,
}

/// A row that asks the model a turn though its words are no prompt, read
/// and waiting for the assistant row that says the turn ran.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Pending {
    /// Its `promptId`, else its `uuid` (none: counted lost if a turn ran).
    vref: Option<StepId>,
    /// Its `timestamp`.
    at_ms: Option<u64>,
    /// Past the seed.
    fresh: bool,
    /// A background task's wake, not a command: joins a running turn.
    wake: bool,
}

/// A local command's output, which says the command before it ran no turn.
const LOCAL_OUTPUT: &str = "<local-command-";

/// What a user row that is no local command's asks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ask {
    /// A vendor turn's prompt.
    Prompt,
    /// Esc's row.
    Interrupt,
    /// A background task's wake: a turn waits for the model's row.
    Wake,
    /// A `!` shell command: the turn it may start waits, as a prompt
    /// command's does.
    Command,
    /// A shell-mode command's output: nothing.
    Nothing,
}

/// A vendor turn's name on a user row: its `promptId`, else its `uuid`.
fn vref_of(obj: &Map) -> Option<StepId> {
    [KEY_PROMPT_ID, "uuid"]
        .iter()
        .find_map(|k| obj.get(*k).and_then(Value::as_str).and_then(StepId::new))
}

/// A `promptId` as the fold compares it (any string, never kept).
const fn fingerprint(id: &str) -> u64 {
    aterm_hash::fnv1a_64(id.as_bytes())
}

/// The steps of the bound file's first conversation rows, read before any prompt of theirs.
#[derive(Clone, Debug)]
struct Hold {
    /// Where the held steps start in the queue (those before are not held).
    at: usize,
    /// The log position before the first held row's steps.
    off: u64,
    /// The first held row's time: the prompt is dated no later.
    at_ms: u64,
    /// A take has passed over them once.
    taken: bool,
}

/// The step fold of one session's main transcript.
#[derive(Clone, Debug)]
pub struct StepFold {
    /// The process's start (wall ms): older rows are history.
    floor_ms: u64,
    /// The glue's watermark, from the log.
    given: Option<StepMark>,
    /// The file bound: its fingerprint and identity.
    bound: Option<(u64, Option<(u64, u64)>)>,
    /// Rows ending at or below this are history.
    seed: u64,
    /// The session the registry named while no transcript existed for it ([`Self::note_vacant`], [`Self::note_found`]).
    vacant: Option<Vacancy>,
    /// The highest offset queued.
    through: u64,
    end: EndFold,
    turn: TurnTokens,
    model: Option<(StepId, Option<StepId>)>,
    version: Option<StepId>,
    /// Latched off for the bound file ([`Self::latch`]).
    broken: bool,
    /// A vendor turn is running: a prompt or a model's row was read and no
    /// turn end or interrupt since (`None`: not known, the bind's history
    /// unread).
    in_turn: Option<bool>,
    /// A prompt command's row or a wake waiting for the assistant row that
    /// says a turn ran.
    pending: Option<Pending>,
    /// The `promptId` of the last main-chain user row that named the
    /// running turn ([`fingerprint`]; `upgrade.rs`'s `FinishedScan::turn`).
    vturn: Option<u64>,
    /// The opening hold ([`Hold`]).
    hold: Option<Hold>,
    queue: VecDeque<Step>,
    overflow: u64,
    lost: u64,
}

impl StepFold {
    /// A fold whose rows older than `floor_ms` (wall ms) are history, and whose log holds the watermark `given` (the glue's read of `TxView::through`; `None` for none).
    #[must_use]
    pub fn new(floor_ms: u64, given: Option<StepMark>) -> StepFold {
        StepFold {
            floor_ms,
            given,
            bound: None,
            seed: 0,
            vacant: None,
            through: 0,
            end: EndFold::default(),
            turn: TurnTokens::default(),
            model: None,
            version: None,
            broken: false,
            in_turn: None,
            pending: None,
            vturn: None,
            hold: None,
            queue: VecDeque::new(),
            overflow: 0,
            lost: 0,
        }
    }

    /// The floor and the log's watermark again (a later process, a fold the glue re-read the log for): the floor only rises.
    pub fn set_floor(&mut self, floor_ms: u64, given: Option<StepMark>) {
        self.floor_ms = self.floor_ms.max(floor_ms);
        self.given = given;
    }

    /// Raise the floor to `floor_ms` (wall ms) when that is later.
    pub fn raise_floor(&mut self, floor_ms: u64) {
        self.floor_ms = self.floor_ms.max(floor_ms);
    }

    /// The registry read named session `sid` (its id's fingerprint) and no transcript for it.
    pub const fn note_vacant(&mut self, sid: u64) {
        self.vacant = Some(Vacancy { sid, fp: None });
    }

    /// The registry read named session `sid` and its transcript, the file at fingerprint `fp`.
    pub fn note_found(&mut self, sid: u64, fp: u64) {
        let bound = self.bound.map(|(b, _)| b);
        self.vacant = self
            .vacant
            .filter(|v| v.sid == sid && bound != Some(fp))
            .map(|v| Vacancy { fp: Some(fp), ..v });
    }

    /// Whether a file is bound.
    #[must_use]
    pub const fn is_bound(&self) -> bool {
        self.bound.is_some()
    }

    /// The main chain's end.
    #[must_use]
    pub const fn end(&self) -> End {
        self.end.end
    }

    /// Whether the fold latched off.
    #[must_use]
    pub const fn broken(&self) -> bool {
        self.broken
    }

    /// Everything queued since the last take.
    pub fn take(&mut self) -> StepBatch {
        let upto = match self.hold.as_mut() {
            Some(h) if !h.taken => {
                h.taken = true;
                std::mem::take(&mut h.at)
            }
            Some(_) => {
                self.release();
                self.queue.len()
            }
            None => self.queue.len(),
        };
        StepBatch {
            steps: self.queue.drain(..upto).collect(),
            overflow: std::mem::take(&mut self.overflow),
            lost: std::mem::take(&mut self.lost),
            end: self.end.end,
        }
    }

    /// The cursor's read of the file at `fp` (its identity `id`, `len` bytes long) is about to fold lines.
    pub fn read_at(&mut self, fp: u64, id: Option<(u64, u64)>, len: u64, restarted: bool) {
        let key = (fp, id);
        let vacant = self.vacant.take().is_some_and(|v| v.fp == Some(fp));
        if self.bound == Some(key) && !restarted {
            return;
        }
        self.release();
        let same = self.bound == Some(key);
        if !same {
            self.broken = false;
        }
        self.bound = Some(key);
        self.end = EndFold::default();
        self.turn = TurnTokens::default();
        self.model = None;
        self.in_turn = None;
        self.pending = None;
        self.vturn = None;
        let vacant = vacant && !same;
        self.seed = match self.given {
            Some(m) if m.fp == fp && m.id == id && m.off <= len => m.off,
            _ if same && self.through <= len => self.through,
            _ if vacant => 0,
            _ => len,
        };
        self.through = self.seed;
        if self.broken {
            return;
        }
        self.enqueue(
            StepPos {
                fp,
                id,
                off: len,
                at_ms: self.floor_ms,
            },
            StepEvent::Bind {
                len: if vacant { self.seed } else { len },
            },
        );
    }

    /// A line of the main transcript longer than the line bound, ending at `end`: counted lost when it is past the seed.
    pub const fn long_line(&mut self, end: u64) {
        if end > self.seed {
            self.lost = self.lost.saturating_add(1);
        }
    }

    /// Fold one completed line of the main transcript, ending at offset `end` (its newline's).
    pub fn push(&mut self, line: &[u8], end: u64) {
        let Some((fp, id)) = self.bound else {
            return;
        };
        let fresh = end > self.seed;
        let Ok(text) = std::str::from_utf8(line) else {
            self.lost = self.lost.saturating_add(u64::from(fresh));
            return;
        };
        let t = text.trim();
        if t.is_empty()
            || !["\"user\"", "\"assistant\"", "\"system\"", "\\u"]
                .iter()
                .any(|k| t.contains(k))
        {
            return;
        }
        let parsed = if wants_parts(t) {
            transcript::steps(t)
        } else {
            transcript::steps_bare(t)
        };
        let Ok(value) = parsed else {
            self.lost = self.lost.saturating_add(u64::from(fresh));
            return;
        };
        let Some(obj) = value.as_object() else {
            return;
        };
        if flag(obj, KEY_SIDECHAIN) {
            return;
        }
        if let Some(v) = obj
            .get("version")
            .and_then(Value::as_str)
            .and_then(StepId::new)
        {
            self.version = Some(v);
        }
        let at_ms = obj
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(stamp_ms);
        let opens = obj.get(KEY_PARENT).is_some_and(Value::is_null);
        let mut out: Vec<StepEvent> = Vec::new();
        let mut bad = false;
        let kind = obj.get("type").and_then(Value::as_str);
        let unasked = kind == Some("assistant")
            && self.end.end.last == EndClass::Nothing
            && self.pending.is_none()
            && self.in_turn != Some(true);
        let waited = match kind {
            Some("assistant") => self.ran(obj),
            _ => None,
        };
        let mut late = None;
        let live = fresh && at_ms.is_some_and(|t| t >= self.floor_ms);
        let row = match kind {
            Some("assistant") => self.assistant(t, obj, live, &mut out, &mut bad),
            Some("user") => {
                let (row, first) = self.user(t, obj, opens, (fresh, at_ms), &mut out, &mut bad);
                late = first;
                row
            }
            Some("system") => self.system(obj, &mut out, &mut bad),
            _ => EndRow::None,
        };
        let ended = matches!(row, EndRow::Closed);
        self.end.push(row, opens, at_ms);
        if ended && let Some(kind) = self.end.end.wall() {
            out.push(StepEvent::EndWall { kind });
        }
        for ev in &out {
            match ev {
                StepEvent::Prompt { .. } => self.in_turn = Some(true),
                StepEvent::TurnEnd { .. } | StepEvent::Interrupt => self.in_turn = Some(false),
                _ => {}
            }
            if matches!(
                ev,
                StepEvent::Prompt { .. }
                    | StepEvent::TurnEnd { .. }
                    | StepEvent::Interrupt
                    | StepEvent::Compact
            ) {
                self.pending = None;
            }
        }
        if self.broken || !fresh {
            return;
        }
        let waited = waited.map(|(vref, when)| (vref, when.or(at_ms)));
        if bad
            || (!out.is_empty() && at_ms.is_none())
            || waited.is_some_and(|(_, when)| when.is_none())
        {
            self.latch(fp, id, end);
            return;
        }
        let state = self.state();
        let k = out.len() as u64;
        if kind == Some("user") {
            // THE HELD TURN'S PROMPT, written late: dated before its rows,
            // it is queued before their steps; any other user row says no
            // such prompt comes, and they go on as they are.
            if let Some(h) = self.hold.take()
                && let (Some(vref), Some(when)) = (late, at_ms)
                && when <= h.at_ms
                && when >= self.floor_ms
            {
                let pos = StepPos {
                    fp,
                    id,
                    off: h.off,
                    at_ms: when,
                };
                let ev = StepEvent::Prompt { vref };
                self.queue.insert(h.at, Step { pos, state, ev });
                self.trim();
                self.in_turn = self.in_turn.or(Some(true));
            }
        }
        if let Some((vref, Some(when))) = waited
            && when >= self.floor_ms
        {
            let off = end.saturating_sub(k);
            let pos = StepPos {
                fp,
                id,
                off,
                at_ms: when,
            };
            self.enqueue_as(pos, state, StepEvent::Prompt { vref });
            self.through = self.through.max(off);
        }
        let Some(at_ms) = at_ms.filter(|&t| t >= self.floor_ms) else {
            return;
        };
        if unasked && waited.is_none() && self.hold.is_none() {
            self.hold = Some(Hold {
                at: self.queue.len(),
                off: end.saturating_sub(k),
                at_ms,
                taken: false,
            });
        }
        for (i, ev) in out.into_iter().enumerate() {
            let off = end.saturating_sub(k - 1 - i as u64);
            self.enqueue_as(StepPos { fp, id, off, at_ms }, state, ev);
        }
        self.through = self.through.max(end);
    }

    /// End the opening hold: what it held goes on in the queue as it was
    /// read.
    const fn release(&mut self) {
        self.hold = None;
    }

    /// A main-chain assistant row of a model's (never Claude Code's own
    /// `<synthetic>` notice) with a row waiting for it: the turn it asked
    /// ran.
    fn ran(&mut self, obj: &Map) -> Option<(StepId, Option<u64>)> {
        let model = obj
            .get("message")
            .and_then(|m| m.get("model"))
            .and_then(Value::as_str)
            .is_some_and(super::upgrade::is_model_id);
        if !model {
            return None;
        }
        let running = self.in_turn.replace(true) == Some(true);
        let p = self.pending.take()?;
        if p.wake && running {
            return None;
        }
        self.turn = TurnTokens::default();
        let Some(vref) = p.vref else {
            self.lost = self.lost.saturating_add(u64::from(p.fresh));
            return None;
        };
        p.fresh.then_some((vref, p.at_ms))
    }

    fn latch(&mut self, fp: u64, id: Option<(u64, u64)>, end: u64) {
        self.release();
        self.broken = true;
        let at_ms = self.floor_ms;
        self.enqueue(
            StepPos {
                fp,
                id,
                off: end,
                at_ms,
            },
            StepEvent::Off { why: "unknown" },
        );
        self.through = self.through.max(end);
    }

    /// The state of the rows now.
    fn state(&self) -> StepState {
        state_for(MEASURED_BUILDS, self.version.as_ref().map(StepId::as_str))
    }

    fn enqueue(&mut self, pos: StepPos, ev: StepEvent) {
        let state = self.state();
        self.enqueue_as(pos, state, ev);
    }

    /// Queue one step, dropping the oldest non-`Bind`/non-`Off` step when full.
    fn enqueue_as(&mut self, pos: StepPos, state: StepState, ev: StepEvent) {
        self.queue.push_back(Step { pos, state, ev });
        self.trim();
    }

    /// Bound the queue to [`STEP_QUEUE`] as [`Self::enqueue_as`] says.
    fn trim(&mut self) {
        while self.queue.len() > STEP_QUEUE {
            let pinned = |s: &Step| matches!(s.ev, StepEvent::Bind { .. } | StepEvent::Off { .. });
            let i = self.queue.iter().position(|s| !pinned(s)).unwrap_or(0);
            self.queue.remove(i);
            if let Some(h) = self.hold.as_mut()
                && i < h.at
            {
                h.at -= 1;
            }
            self.overflow = self.overflow.saturating_add(1);
        }
    }

    /// `live`: a row the log takes, past the seed and no older than the
    /// floor (the model is said only on one).
    fn assistant(
        &mut self,
        line: &str,
        obj: &Map,
        live: bool,
        out: &mut Vec<StepEvent>,
        bad: &mut bool,
    ) -> EndRow {
        let message = obj.get("message").and_then(Value::as_object);
        if let Some(usage) = message
            .and_then(|m| m.get("usage"))
            .and_then(Value::as_object)
            .and_then(TokenUsage::from_object)
        {
            let id = message
                .and_then(|m| m.get("id"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty());
            self.turn.note(id, usage);
        }
        if flag(obj, KEY_API_ERROR) {
            return EndRow::Entry(EndClass::Notice, Some(notice_of(line)));
        }
        if let Some(model) = message
            .and_then(|m| m.get("model"))
            .and_then(Value::as_str)
            .filter(|m| live && super::upgrade::is_model_id(m))
            .and_then(StepId::new)
        {
            let effort = obj
                .get("effort")
                .and_then(Value::as_str)
                .and_then(StepId::new);
            if self.model != Some((model, effort)) {
                self.model = Some((model, effort));
                out.push(StepEvent::Model {
                    model,
                    effort,
                    ver: self.version,
                });
            }
        }
        out.reserve(parts(message).count());
        for part in parts(message) {
            if part.get("type").and_then(Value::as_str) != Some(KEY_TOOL_USE) {
                continue;
            }
            let Some(id) = part.get("id").and_then(Value::as_str) else {
                *bad = true;
                continue;
            };
            let Some(id) = StepId::new(id) else {
                self.lost = self.lost.saturating_add(1);
                continue;
            };
            let name = part.get("name").and_then(Value::as_str).unwrap_or("");
            let (cls, subagent) = tool_class(name);
            out.push(StepEvent::ToolUse {
                id,
                tool: ToolName::new(name),
                cls,
                subagent,
            });
        }
        EndRow::Entry(EndClass::Reply, None)
    }

    /// A user row: its end entry, and the vendor turn's name when it is the
    /// conversation's first prompt written after newer rows (the harness's
    /// exception), for [`Self::push`] to hand to the opening hold.
    fn user(
        &mut self,
        line: &str,
        obj: &Map,
        opens: bool,
        (fresh, at_ms): (bool, Option<u64>),
        out: &mut Vec<StepEvent>,
        bad: &mut bool,
    ) -> (EndRow, Option<StepId>) {
        let someone = obj
            .get("origin")
            .and_then(|o| o.get("kind"))
            .and_then(Value::as_str)
            .is_some_and(|k| !matches!(k, "human" | "task-notification" | "auto-continuation"));
        let meta = flag(obj, KEY_META);
        let id = obj
            .get(KEY_PROMPT_ID)
            .and_then(Value::as_str)
            .map(fingerprint);
        let known = self.vturn.is_some() || self.in_turn == Some(false);
        let anew = super::upgrade::numbered_anew(id.filter(|_| known), self.vturn, || {
            super::upgrade::begins_unnumbered_by(
                |k| obj.get(k).is_some_and(|v| !v.is_null()),
                meta,
                lead(line).as_ref().map(Lead::text),
            )
        });
        if anew || (!known && id.is_some()) {
            self.vturn = id;
        }
        let waits = |wake: bool| Pending {
            vref: vref_of(obj),
            at_ms,
            fresh,
            wake,
        };
        if flag(obj, KEY_COMPACT_SUMMARY) || flag(obj, KEY_QUEUE_TRANSCRIPT_ONLY) {
            return (EndRow::None, None);
        }
        if meta {
            if someone {
                self.prompt(obj, out);
            } else if anew {
                self.pending = Some(waits(true));
            }
            return (EndRow::None, None);
        }
        let message = obj.get("message").and_then(Value::as_object);
        let mut results = false;
        for part in parts(message) {
            if part.get("type").and_then(Value::as_str) != Some(KEY_TOOL_RESULT) {
                continue;
            }
            results = true;
            let Some(id) = part.get(KEY_TOOL_USE_ID).and_then(Value::as_str) else {
                *bad = true;
                continue;
            };
            let Some(id) = StepId::new(id) else {
                self.lost = self.lost.saturating_add(1);
                continue;
            };
            out.push(StepEvent::ToolResult {
                id,
                error: flag(part, KEY_IS_ERROR),
            });
        }
        if results {
            return (EndRow::Entry(EndClass::ToolResult, None), None);
        }
        let lead = lead(line);
        if let Some(l) = lead.as_ref().filter(|l| l.local()) {
            // A command's row waits for the turn it may start (a prompt
            // command's), its output says it started none (a local one's).
            if l.starts(LOCAL_COMMAND[0]) || l.starts(LOCAL_COMMAND[1]) {
                self.pending = Some(waits(false));
            } else if l.starts(LOCAL_OUTPUT) {
                self.pending = None;
            }
            return (EndRow::None, None);
        }
        let asks = obj
            .get("origin")
            .and_then(|o| o.get("kind"))
            .and_then(Value::as_str)
            != Some("task-notification");
        let ask = match lead {
            _ if someone => Ask::Prompt,
            Some(l) if l.starts(INTERRUPTED) => Ask::Interrupt,
            Some(l) if l.starts(TASK_NOTIFICATION) => Ask::Wake,
            Some(l) if l.starts(BASH_INPUT) => Ask::Command,
            Some(l) if [BASH_STDOUT, BASH_STDERR].iter().any(|m| l.starts(m)) => Ask::Nothing,
            _ if !asks => Ask::Wake,
            _ => Ask::Prompt,
        };
        if opens && self.end.end.last != EndClass::Nothing {
            let late = (ask == Ask::Prompt).then(|| vref_of(obj)).flatten();
            return (EndRow::Entry(EndClass::Input, None), late);
        }
        match ask {
            Ask::Prompt => self.prompt(obj, out),
            Ask::Interrupt => out.push(StepEvent::Interrupt),
            Ask::Wake => self.pending = Some(waits(true)),
            Ask::Command => self.pending = Some(waits(false)),
            Ask::Nothing => {}
        }
        (EndRow::Entry(EndClass::Input, None), None)
    }

    /// A vendor turn's prompt: its tokens start over, and it is named by
    /// its `promptId`, else its row's `uuid` (none: counted lost).
    fn prompt(&mut self, obj: &Map, out: &mut Vec<StepEvent>) {
        self.turn = TurnTokens::default();
        match vref_of(obj) {
            Some(vref) => out.push(StepEvent::Prompt { vref }),
            None => self.lost = self.lost.saturating_add(1),
        }
    }

    fn system(&mut self, obj: &Map, out: &mut Vec<StepEvent>, bad: &mut bool) -> EndRow {
        match obj.get("subtype").and_then(Value::as_str) {
            Some(KEY_TURN_DURATION) => {
                match number(obj.get(KEY_DURATION_MS)) {
                    Some(vdur_ms) => out.push(StepEvent::TurnEnd {
                        vdur_ms,
                        tok_in: self.turn.tok_in,
                        tok_out: self.turn.tok_out,
                    }),
                    None => *bad = true,
                }
                EndRow::Closed
            }
            Some(KEY_COMPACT_BOUNDARY) => {
                out.push(StepEvent::Compact);
                EndRow::None
            }
            Some(KEY_LOCAL_COMMAND) => {
                // A local command's output in the newer shape: the command
                // before it started no turn (`upgrade.rs`'s `TaskScan`).
                if obj
                    .get("content")
                    .and_then(Value::as_str)
                    .is_some_and(|t| t.trim_start().starts_with(LOCAL_OUTPUT))
                {
                    self.pending = None;
                }
                EndRow::Entry(EndClass::Opaque, None)
            }
            _ => EndRow::None,
        }
    }
}

/// The state of a row written by build `version` when the builds the lab measured are `measured`.
pub(super) fn state_for(measured: &[&str], version: Option<&str>) -> StepState {
    match version {
        Some(v) if measured.contains(&v) => StepState::On,
        _ => StepState::Partial,
    }
}

/// A message's content parts (the `steps` projection's), as objects.
fn parts(message: Option<&Map>) -> impl Iterator<Item = &Map> {
    message
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
}

#[cfg(test)]
#[path = "steps_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "steps_corpus_tests.rs"]
mod corpus_tests;
