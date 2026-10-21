// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE JOURNAL (design §5.3, §5.4; phase 2B): the lines a host's journal
//! writer appends to its segments, their names, and the READER'S VIEW of
//! them, every rule of which is a pure function here, so its model
//! (`aterm_models_session::session_log_journal_model`) binds to exactly the
//! code a `log @<sid>` runs. The host owns the files, the locks and the
//! threads; this crate reads no file and keeps no clock.
//!
//! ```text
//! s1→*→#seg→v=1→inst=<inst>→n=<n>→t=<ms>→build=<ver>→end=↵
//! s1→<sid>→#snap→head=<seq>→ep=<e>→base=<b>→first=<f>→open=<k>→t=<ms>→end=↵
//! s1→<sid>→<open record>→re=1→end=↵                       (k of them, outermost first)
//! s1→<sid>→<record>→end=↵
//! s1→<sid>→#gap→why=evicted|handoff|reset|dropped→lost=<n>→after=<seq>→through=<seq>→end=↵
//! s1→<sid>→#hole→lo=<seq>→hi=<seq>→why=<w>→end=↵
//! s1→<sid>→#handed→fence=<seq>→end=↵
//! s1→*→#fin→why=quit|handoff→t=<ms>→end=↵                 (a chain's last line)
//! ```
//!
//! (`→` is TAB, `↵` LF.) A record is [`crate::encode`] at [`Tier::Host`]: the
//! content tier (the OSC title) never reaches a journal. Only ASCII appears.
//! A reader takes complete lines only ([`complete_lines`]: a partial last
//! line is dropped), refuses a line that is not `s1 … end=` with every field
//! after the body a `key=value` ([`decode_line`]: a tear glued onto the next
//! line fails), skips unknown keys and kinds, and treats a segment whose
//! `#seg` names a version it does not know as opaque.
//!
//! THE VIEW ([`view_of`]). Lines are grouped by the session's seq space,
//! `(sid, base)` (a keeper's crash adoption starts a new one under the same
//! sid), and the newest space answers. Within it:
//! - each `(sid, base, seq)` is shown at most once (a line journaled twice,
//!   by an update's overlap or a stopped writer's successor, is one record),
//!   and gap frames are deduplicated by their range;
//! - every seq the journal accounts for and holds no record of is a loss a
//!   read says, and every range nobody holds says why: an update's overlap
//!   (`why=handoff`), segments pruned (`why=pruned`) or left unread by the
//!   read's budget (`why=unread`);
//! - a node the journal opened, never closed, and a later snap of the space
//!   does not list as open, closed while the writer was behind: it is shown
//!   closed `out=unknown` at that snap's time;
//! - CRASH CLOSES ARE DERIVED AT READ TIME, never written: when the chain
//!   that last wrote the space ends with no `#fin` and no `#handed` for the
//!   sid, and its instance is not live (the host's lock probe), the view adds
//!   `gap why=crash after=<the journal's head>` and closes everything still
//!   open `out=gone by=aterm`, innermost first. A misjudged liveness changes
//!   one reply, never the disk;
//! - the records a reply derives (the crash gap and closes, `out=unknown`
//!   closes, a `gap why=unread`) are numbered past the journal's head in the
//!   journal's own epoch, and DERIVED AGAIN BY EVERY READ: a reply hands out
//!   the journal's own cursor, its epoch and head ([`View::reply_cursor`]),
//!   never one into what it derived, and [`View::cursor`] places a reader's
//!   cursor only within the journal's own numbering, resetting (said) every
//!   other, so following one reply into the next never skips a record the
//!   journal holds and no cursor of another epoch is read as this one's.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use crate::Close;
use crate::field::Tier;
use crate::line::{DecodeError, decode, encode};
use crate::log::{GapWhy, JournalImage, Log};
use crate::rec::{Rec, Stamp};
use crate::vocab::{Actor, Conf, Lvl, Node, Op, ShortId, Src};
use crate::words::{Out, RunOut, SessionOut, StepOut, TurnOut};

/// The line format's version (`#seg v=`).
pub const FORMAT: u32 = 1;
/// A segment's size past which its writer starts the next.
pub const SEGMENT_BYTES: u64 = 8 << 20;
/// The largest segment a reader reads; a larger one is foreign.
pub const SEGMENT_READ_MAX: u64 = 16 << 20;
/// The journal directory's budget: its own segments, oldest deleted first.
pub const BUDGET_BYTES: u64 = 64 << 20;
/// The journal directory's count budget: its own segments and lock files,
/// oldest segments deleted first past it. Well under [`LIST_MAX`], so one
/// listing sees all of a journal however small its segments are.
pub const FILES_MAX: usize = 2048;
/// The most names one retention pass visits (it holds only the journal's
/// own among them): a directory past it is pruned by what was visited.
pub const RETENTION_SCAN_MAX: usize = 1 << 16;
/// The longest line a reader decodes.
pub const LINE_MAX: usize = 2048;
/// The most bytes one journal read decodes past its newest
/// [`READ_MIN_SEGMENTS`] segments that hold the session.
pub const READ_BUDGET: u64 = 16 << 20;
/// The segments holding the session that one read decodes whatever their
/// size (each at most [`SEGMENT_READ_MAX`]): a budget measured in whole
/// segments, so a session's newest lines are always reached.
pub const READ_MIN_SEGMENTS: usize = 2;
/// The most bytes one read's index refresh reads (no decoding: which
/// sessions each segment names, whether it ends its chain). Twice the
/// directory's budget, so a journal within its budgets is indexed whole and
/// whether it holds a session is known, never guessed from a budget.
pub const INDEX_BUDGET: u64 = 2 * BUDGET_BYTES;
/// The most names one listing of the directory takes.
pub const LIST_MAX: usize = 4096;
/// The retention pass's lock file (`flock LOCK_EX|LOCK_NB`; busy: skip).
pub const PRUNE_LOCK: &str = "prune.lock";

/// A process instance's name (`control_auth::process_instance_id`):
/// `p<pid>-<nonce>`, the pid 1 to 10 digits, the nonce 1 to 40 lower hex
/// digits (or a decimal nanosecond count, which is hex digits too).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Inst(String);

impl Inst {
    /// `s` as an instance name, or `None`.
    #[must_use]
    pub fn new(s: &str) -> Option<Inst> {
        let (pid, nonce) = s.strip_prefix('p')?.split_once('-')?;
        let ok = (1..=10).contains(&pid.len())
            && pid.bytes().all(|b| b.is_ascii_digit())
            && (1..=40).contains(&nonce.len())
            && nonce
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        ok.then(|| Inst(s.to_string()))
    }

    /// The name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Inst {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// `<inst>.<n>.slog`: segment `n` (1, 2, …) of `inst`'s chain.
#[must_use]
pub fn segment_name(inst: &Inst, n: u32) -> String {
    format!("{inst}.{n}.slog")
}

/// The instance and number a segment's name gives, or `None` for any other
/// name: exactly [`segment_name`]'s spelling (no leading zero, no 0, no
/// suffix after `.slog`, an instance [`Inst::new`] reads).
#[must_use]
pub fn parse_segment_name(name: &str) -> Option<(Inst, u32)> {
    let (inst, n) = name.strip_suffix(".slog")?.rsplit_once('.')?;
    if n.is_empty() || n.starts_with('0') || !n.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((Inst::new(inst)?, n.parse().ok()?))
}

/// `<inst>.lock`: the instance's liveness lock, `flock`ed `LOCK_EX` for its
/// journal's whole life.
#[must_use]
pub fn lock_name(inst: &Inst) -> String {
    format!("{inst}.lock")
}

/// The instance a lock file's name gives, or `None`.
#[must_use]
pub fn parse_lock_name(name: &str) -> Option<Inst> {
    Inst::new(name.strip_suffix(".lock")?)
}

/// `.<inst>.lock.pending`: where the lock is created and locked before it is
/// renamed into place, so no reader ever sees an unlocked live lock.
#[must_use]
pub fn pending_lock_name(inst: &Inst) -> String {
    format!(".{inst}.lock.pending")
}

/// Why a writer says records went unjournaled (`#gap why=`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LossWhy {
    /// The ring evicted them before the writer reached them.
    Evicted,
    /// They lie across an update's overlap.
    Handoff,
    /// The writer's cursor did not place (never expected).
    Reset,
    /// The writer let a closed session's timeline go unwritten (its
    /// backstop).
    Dropped,
}

impl LossWhy {
    /// The `why=` word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            LossWhy::Evicted => "evicted",
            LossWhy::Handoff => "handoff",
            LossWhy::Reset => "reset",
            LossWhy::Dropped => "dropped",
        }
    }

    fn from_word(w: &str) -> Option<LossWhy> {
        [
            LossWhy::Evicted,
            LossWhy::Handoff,
            LossWhy::Reset,
            LossWhy::Dropped,
        ]
        .into_iter()
        .find(|x| x.word() == w)
    }
}

/// How a chain ended (`#fin why=`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndWhy {
    /// A clean quit: every session closed `out=app-quit`.
    Quit,
    /// A seamless update: the successor journals what comes after.
    Handoff,
}

impl EndWhy {
    const fn word(self) -> &'static str {
        match self {
            EndWhy::Quit => "quit",
            EndWhy::Handoff => "handoff",
        }
    }
}

/// One journal line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Frame {
    /// A segment's first line.
    Seg {
        /// The format version.
        v: u32,
        /// The writer.
        inst: Inst,
        /// The segment's number.
        n: u32,
        /// When it began (wall ms).
        t: u64,
        /// The writer's build.
        build: String,
    },
    /// A session's restatement: the seq space it writes in, the writer's
    /// cursor (`head`: everything at or below it is already accounted for),
    /// and the count of open records that follow (`re=1`).
    Snap {
        /// The session.
        sid: ShortId,
        /// The writer's cursor.
        head: u64,
        /// The log's epoch.
        ep: u32,
        /// The space's base epoch.
        base: u32,
        /// The log's first accounted seq.
        first: u64,
        /// How many restated opens follow.
        open: u32,
        /// When (wall ms).
        t: u64,
    },
    /// A record.
    Rec {
        /// The session.
        sid: ShortId,
        /// The record.
        rec: Rec,
        /// A snap's restatement (`re=1`) of an open record.
        restated: bool,
    },
    /// Records the writer did not journal, and how many.
    Gap {
        /// The session.
        sid: ShortId,
        /// Why.
        why: LossWhy,
        /// How many (across an overlap, a lower bound).
        lost: u64,
        /// The writer's cursor before (exclusive).
        after: u64,
        /// After (inclusive).
        through: u64,
    },
    /// A seq range the log never held (an update's overlap).
    Hole {
        /// The session.
        sid: ShortId,
        /// From.
        lo: u64,
        /// To (inclusive).
        hi: u64,
        /// Why.
        why: GapWhy,
    },
    /// The outgoing process journaled the session to its carry's fence: a
    /// successor continues the space above it.
    Handed {
        /// The session.
        sid: ShortId,
        /// The fence.
        fence: u64,
    },
    /// The chain's end.
    End {
        /// Why.
        why: EndWhy,
        /// When (wall ms).
        t: u64,
    },
    /// A kind this build does not know (skipped).
    Unknown,
}

/// Why a line did not decode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineError {
    /// Longer than [`LINE_MAX`].
    TooLong,
    /// A byte that is not printable ASCII or TAB.
    NotAscii,
    /// Not `s1→<subject>→<body>…`.
    NotS1,
    /// Its last field is not `end=`.
    NoEnd,
    /// A field after the body that is no `key=value` (a torn line glued on).
    Glued,
    /// A frame's field missing or of the wrong shape.
    Field,
    /// A record that does not decode.
    Record(DecodeError),
}

/// A token for `build=`: `[A-Za-z0-9.+_-]`, at most 64 bytes.
fn build_token(build: &str) -> String {
    build
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '_' | '-'))
        .take(64)
        .collect()
}

/// `frame` as one line, without its LF; `None` for a record over the byte
/// bound (the ring refuses those) or [`Frame::Unknown`].
#[must_use]
pub fn encode_frame(frame: &Frame) -> Option<String> {
    let mut s = String::with_capacity(128);
    match frame {
        Frame::Seg {
            v,
            inst,
            n,
            t,
            build,
        } => {
            let _ = write!(
                s,
                "s1\t*\t#seg\tv={v}\tinst={inst}\tn={n}\tt={t}\tbuild={}",
                build_token(build)
            );
        }
        Frame::Snap {
            sid,
            head,
            ep,
            base,
            first,
            open,
            t,
        } => {
            let _ = write!(
                s,
                "s1\t{}\t#snap\thead={head}\tep={ep}\tbase={base}\tfirst={first}\topen={open}\tt={t}",
                sid.as_str()
            );
        }
        Frame::Rec { sid, rec, restated } => {
            let body = encode(rec, Tier::Host).ok()?;
            let _ = write!(s, "s1\t{}\t{body}", sid.as_str());
            if *restated {
                s.push_str("\tre=1");
            }
        }
        Frame::Gap {
            sid,
            why,
            lost,
            after,
            through,
        } => {
            let _ = write!(
                s,
                "s1\t{}\t#gap\twhy={}\tlost={lost}\tafter={after}\tthrough={through}",
                sid.as_str(),
                why.word()
            );
        }
        Frame::Hole { sid, lo, hi, why } => {
            let _ = write!(
                s,
                "s1\t{}\t#hole\tlo={lo}\thi={hi}\twhy={}",
                sid.as_str(),
                why.word()
            );
        }
        Frame::Handed { sid, fence } => {
            let _ = write!(s, "s1\t{}\t#handed\tfence={fence}", sid.as_str());
        }
        Frame::End { why, t } => {
            let _ = write!(s, "s1\t*\t#fin\twhy={}\tt={t}", why.word());
        }
        Frame::Unknown => return None,
    }
    s.push_str("\tend=");
    Some(s)
}

/// The complete lines of `bytes` (each without its LF), in order: a last
/// line with no LF (a writer still appending it, or a torn tail) is not one.
pub fn complete_lines(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    let end = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    bytes[..end]
        .split(|&b| b == b'\n')
        .take(bytes[..end].iter().filter(|&&b| b == b'\n').count())
}

/// One complete line (without its LF) back to its frame.
///
/// # Errors
///
/// A [`LineError`] for a line too long, not ASCII, not `s1 … end=`, with a
/// field after the body that is no `key=value`, a frame missing a field, or
/// a record that does not decode.
pub fn decode_line(line: &[u8]) -> Result<Frame, LineError> {
    if line.len() > LINE_MAX {
        return Err(LineError::TooLong);
    }
    if !line
        .iter()
        .all(|&b| b == b'\t' || (0x20..0x7f).contains(&b))
    {
        return Err(LineError::NotAscii);
    }
    let s = std::str::from_utf8(line).map_err(|_| LineError::NotAscii)?;
    let fields: Vec<&str> = s.split('\t').collect();
    if fields.len() < 4 || fields[0] != "s1" {
        return Err(LineError::NotS1);
    }
    if fields.last() != Some(&"end=") {
        return Err(LineError::NoEnd);
    }
    let mut kv: BTreeMap<&str, &str> = BTreeMap::new();
    for f in &fields[3..fields.len() - 1] {
        let (k, v) = f.split_once('=').ok_or(LineError::Glued)?;
        if k.is_empty() || k == "end" || !k.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') {
            return Err(LineError::Glued);
        }
        kv.entry(k).or_insert(v);
    }
    let (subj, body) = (fields[1], fields[2]);
    let num = |k: &str| -> Result<u64, LineError> {
        kv.get(k)
            .and_then(|v| v.parse().ok())
            .ok_or(LineError::Field)
    };
    let num32 = |k: &str| -> Result<u32, LineError> {
        u32::try_from(num(k)?).map_err(|_| LineError::Field)
    };
    let sid = || ShortId::new(subj).ok_or(LineError::Field);
    let Some(kind) = body.strip_prefix('#') else {
        let rec = decode(body).map_err(LineError::Record)?;
        return Ok(Frame::Rec {
            sid: sid()?,
            rec,
            restated: kv.get("re") == Some(&"1"),
        });
    };
    Ok(match (subj, kind) {
        ("*", "seg") => Frame::Seg {
            v: num32("v")?,
            inst: kv
                .get("inst")
                .and_then(|i| Inst::new(i))
                .ok_or(LineError::Field)?,
            n: num32("n")?,
            t: num("t")?,
            build: kv.get("build").map_or_else(String::new, |b| build_token(b)),
        },
        ("*", "fin") => Frame::End {
            why: match kv.get("why") {
                Some(&"handoff") => EndWhy::Handoff,
                _ => EndWhy::Quit,
            },
            t: num("t").unwrap_or(0),
        },
        ("*", _) => Frame::Unknown,
        (_, "snap") => Frame::Snap {
            sid: sid()?,
            head: num("head")?,
            ep: num32("ep")?,
            base: num32("base")?,
            first: num("first")?,
            open: num32("open")?,
            t: num("t").unwrap_or(0),
        },
        (_, "gap") => Frame::Gap {
            sid: sid()?,
            why: kv
                .get("why")
                .and_then(|w| LossWhy::from_word(w))
                .unwrap_or(LossWhy::Evicted),
            lost: num("lost").unwrap_or(0),
            after: num("after")?,
            through: num("through")?,
        },
        (_, "hole") => Frame::Hole {
            sid: sid()?,
            lo: num("lo")?,
            hi: num("hi")?,
            why: kv
                .get("why")
                .and_then(|w| GapWhy::from_word(w))
                .unwrap_or(GapWhy::Handoff),
        },
        (_, "handed") => Frame::Handed {
            sid: sid()?,
            fence: num("fence")?,
        },
        _ => Frame::Unknown,
    })
}

/// The session a line names, without decoding it: its second field (`*` for
/// the instance's own lines), or `None` for a line that is not `s1→…`. What
/// a reader's index reads of every line.
#[must_use]
pub fn line_subject(line: &[u8]) -> Option<&[u8]> {
    let rest = line.strip_prefix(b"s1\t")?;
    let end = rest.iter().position(|&b| b == b'\t')?;
    Some(&rest[..end])
}

/// Whether a complete, valid line is a chain's end (`s1→*→#fin→…`).
#[must_use]
pub fn is_end_line(line: &[u8]) -> bool {
    matches!(decode_line(line), Ok(Frame::End { .. }))
}

// ---- the writer's take ----------------------------------------------

/// What a journal writer copies out of a log under its lock for one pass
/// ([`take`]): everything [`take_frames`] turns into lines, so the lock is
/// held only for the read and the copies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Take {
    /// The log's epoch, base epoch and first accounted seq: its seq space.
    pub ep: u32,
    /// See [`Take::ep`].
    pub base: u32,
    /// See [`Take::ep`].
    pub first: u64,
    /// The writer's cursor before the read.
    pub cursor: u64,
    /// Where the read reached (the writer's next cursor).
    pub next: u64,
    /// The gap the read crossed.
    pub gap: Option<crate::log::Gap>,
    /// The records, oldest first.
    pub recs: Vec<Rec>,
    /// The overlaps the read crossed.
    pub holes: Vec<(u64, u64, GapWhy)>,
    /// The open path (the session's open, then each open node's), when the
    /// writer asked for a snap or the read lost records.
    pub opens: Option<Vec<Rec>>,
    /// The readers' fence, while an update's carry stands.
    pub fence: Option<u64>,
    /// The session is closed and the read reached the log's last record (its
    /// close among them): never while a fence hides records past the read.
    pub done: bool,
    /// More records wait past `max`.
    pub more: bool,
}

/// The writer's take of `log` from `cursor`: at most `max` records, the gap
/// and overlaps crossed, and the open path when `snap` asks for it or the
/// read lost records (ruling R8: after any loss a fresh snap restates the
/// open path, so a reader can tell a close that went unjournaled).
#[must_use]
pub fn take(log: &Log, cursor: u64, max: usize, snap: bool) -> Take {
    let read = log.read(crate::Cursor::new(log.ep(), cursor), max);
    let next = read.next.seq;
    let lost = read
        .gap
        .is_some_and(|g| g.why != GapWhy::Handoff || g.lost > 0);
    let holes: Vec<(u64, u64, GapWhy)> = log
        .holes_why()
        .filter(|&(lo, hi, _)| lo <= next && hi > cursor)
        .collect();
    let more = read.recs.len() == max && next < log.reader_last_seq();
    Take {
        ep: log.ep(),
        base: log.base_ep(),
        first: log.first_seq(),
        cursor,
        next,
        gap: read.gap,
        recs: read.recs.into_iter().cloned().collect(),
        holes,
        opens: (snap || lost).then(|| log.open_path_records()),
        fence: log.read_fence(),
        // Against the log's own head, never the readers' fence: a session
        // that closes during an update's overlap is fenced past its close, and
        // its track must outlive a rollback to write it.
        done: !log.is_open(Node::SESSION) && next >= log.last_seq(),
        more,
    }
}

/// A snap of `take`'s space at `head`: the `#snap` frame, then the open
/// path restated AS OF THE HEAD: the nodes open at the take that opened at or
/// below `head` (a node opened past it is not yet journaled, and a snap that
/// named it would claim a seq the writer never accounted for).
fn snap_frames(sid: ShortId, take: &Take, head: u64, t: u64, out: &mut Vec<Frame>) {
    let opens: Vec<&Rec> = take
        .opens
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .filter(|r| r.seq <= head)
        .collect();
    out.push(Frame::Snap {
        sid,
        head,
        ep: take.ep,
        base: take.base,
        first: take.first,
        open: u32::try_from(opens.len()).unwrap_or(u32::MAX),
        t,
    });
    for r in opens {
        out.push(Frame::Rec {
            sid,
            rec: r.clone(),
            restated: true,
        });
    }
}

/// The lines of one take for session `sid`, at wall time `t`: a snap first
/// when `snap` (a new segment, or a new seq space), then the gap frame and
/// the overlaps, then the records, then, after a loss, a fresh snap. Every
/// snap restates the open path as of its own head ([`snap_frames`]).
#[must_use]
pub fn take_frames(sid: ShortId, take: &Take, snap: bool, t: u64) -> Vec<Frame> {
    let mut out = Vec::with_capacity(take.recs.len() + 4);
    let snap_at = |head: u64, out: &mut Vec<Frame>| snap_frames(sid, take, head, t, out);
    if snap {
        snap_at(take.cursor, &mut out);
    }
    let mut lost = false;
    if let Some(g) = take.gap {
        let why = match g.why {
            GapWhy::Handoff => LossWhy::Handoff,
            GapWhy::Reset => LossWhy::Reset,
            _ => LossWhy::Evicted,
        };
        lost = why != LossWhy::Handoff || g.lost > 0;
        out.push(Frame::Gap {
            sid,
            why,
            lost: g.lost,
            after: take.cursor,
            through: take.next,
        });
    }
    for &(lo, hi, why) in &take.holes {
        out.push(Frame::Hole { sid, lo, hi, why });
    }
    for r in &take.recs {
        out.push(Frame::Rec {
            sid,
            rec: r.clone(),
            restated: false,
        });
    }
    if lost && take.opens.is_some() {
        snap_at(take.next, &mut out);
    }
    out
}

/// The lines that say a session the writer let go unwritten (its backstop):
/// a snap first when `snap` (none of the session's in this segment yet, so
/// the reader places the frame in its space), then `#gap why=dropped` over
/// `(take.cursor, through]`, `lost` of them. `take` is a probe of the log
/// (`take(log, cursor, 0, true)`) made when the writer let it go.
#[must_use]
pub fn dropped_frames(
    sid: ShortId,
    take: &Take,
    through: u64,
    lost: u64,
    snap: bool,
    t: u64,
) -> Vec<Frame> {
    let mut out = Vec::with_capacity(4);
    if snap {
        snap_frames(sid, take, take.cursor, t, &mut out);
    }
    if through > take.cursor {
        out.push(Frame::Gap {
            sid,
            why: LossWhy::Dropped,
            lost,
            after: take.cursor,
            through,
        });
    }
    out
}

/// The format version a segment's first complete line names, read from its
/// raw `v=` field without decoding the rest: `None` when that line is no
/// `s1→*→#seg` or names no version. A segment whose first line does not name
/// [`FORMAT`] is opaque to this build, however much of it decodes.
#[must_use]
pub fn segment_version(first_line: &[u8]) -> Option<u32> {
    let rest = first_line.strip_prefix(b"s1\t*\t#seg\t")?;
    rest.split(|&b| b == b'\t')
        .find_map(|f| f.strip_prefix(b"v="))
        .and_then(|v| std::str::from_utf8(v).ok())
        .and_then(|v| v.parse().ok())
}

// ---- the view -------------------------------------------------------

/// Whether a chain's instance lives, as the host's lock probe says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Liveness {
    /// Its lock is held: the instance runs.
    Live,
    /// Its lock is free, or gone: the instance is not running.
    Dead,
    /// The probe could not say: never read as a crash.
    Unknown,
}

/// One segment a journal read gathered: its frames in file order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SegmentRead {
    /// Its number in its chain.
    pub n: u32,
    /// Its complete lines' frames, in order.
    pub frames: Vec<Frame>,
}

/// One instance's chain as a journal read found it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChainRead {
    /// The writer.
    pub inst: Inst,
    /// The segments that hold the session's lines, in any order.
    pub segments: Vec<SegmentRead>,
    /// Whether the chain's newest segment (of all of it) holds its end
    /// (`#fin`).
    pub ended: bool,
    /// Whether the writer lives.
    pub live: Liveness,
    /// When the chain began (its first segment's `#seg t=`, else the
    /// earliest the read saw): chains are ordered by it.
    pub start_t: u64,
}

/// The view's rules: [`ViewRules::READER`] is the reader's. Each switch
/// exists for the conformance test's negative controls, which turn one off
/// and watch the model refuse the view (the way `encode_with_table` is the
/// tier test's).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewRules {
    /// Each `(sid, base, seq)` once.
    pub dedupe: bool,
    /// A dead chain's open nodes closed at read time.
    pub crash_closes: bool,
    /// Every seq accounted for and not held said as a loss.
    pub say_losses: bool,
    /// A restatement counts only right after its snap and at or below the
    /// snap's head, and never toward the journal's head (off: the reader
    /// before phase 2B's fixes, which took a restated open past the head for
    /// an accounted seq).
    pub restated_heads: bool,
    /// A later snap of the session voids its `#handed` (an update that stood
    /// down; off: a `#handed` voided only by a record or a gap frame).
    pub snap_voids_handed: bool,
    /// Two chains that no `#handed` links and that hold different records
    /// at one `(base, seq)` are two seq spaces, ordered by when each chain
    /// began (session log phase 6, 6A-2; off: the reader before it, which
    /// keyed a space by its base alone and let a relaunch's records hide
    /// behind a dead process's at the same seqs).
    pub split_unlinked: bool,
}

impl ViewRules {
    /// The reader's.
    pub const READER: ViewRules = ViewRules {
        dedupe: true,
        crash_closes: true,
        say_losses: true,
        restated_heads: true,
        snap_voids_handed: true,
        split_unlinked: true,
    };
}

/// What a journal read answers for a session.
#[derive(Clone, Debug)]
pub struct View {
    /// The rebuilt log, synthetic closes included.
    pub log: Log,
    /// The journal's own epoch for the space, the rebuilt log's: the records
    /// the reply derives are numbered in it, past the head.
    pub ep: u32,
    /// The journal's own head for the space, before any record the reply
    /// derives (those are numbered past it): a cursor of the dead process
    /// past it names records nobody journaled.
    pub head: u64,
    /// Whether the view added crash closes.
    pub crashed: bool,
    /// How many nodes it closed `out=unknown`.
    pub unknown_closed: usize,
}

#[derive(Clone, Debug, Default)]
struct SnapSeen {
    head: u64,
    t: u64,
    want: u32,
    open: BTreeSet<Node>,
}

#[derive(Clone, Debug, Default)]
struct Space {
    ep: u32,
    first: u64,
    /// seq → (record, restated): one per seq.
    recs: BTreeMap<u64, (Rec, bool)>,
    /// Every line journaled again (the dedupe's work), for the switch.
    dups: Vec<Rec>,
    gaps: BTreeSet<(u64, u64, LossWhy)>,
    holes: BTreeSet<(u64, u64, GapWhy)>,
    snaps: Vec<SnapSeen>,
    /// The chains that wrote it: (start_t, inst, chain index).
    chains: BTreeSet<(u64, Inst, usize)>,
    last_t: u64,
}

/// One maximal run of seqs `[lo, hi]`.
type Run = (u64, u64);

/// The runs of `[lo, hi]` no run of `covered` (sorted, may overlap) meets.
fn uncovered(lo: u64, hi: u64, covered: &[Run]) -> Vec<Run> {
    let mut out = Vec::new();
    let mut at = lo;
    for &(a, b) in covered {
        if b < at {
            continue;
        }
        if a > hi {
            break;
        }
        if a > at {
            out.push((at, a - 1));
        }
        at = at.max(b.saturating_add(1));
        if at > hi {
            return out;
        }
    }
    if at <= hi {
        out.push((at, hi));
    }
    out
}

/// What one chain wrote of session `sid` in one base: each seq's first
/// admitted record (an ordinary one ahead of a restatement), and whether its
/// last word on the session was
/// `#handed` (as [`view_of`] reads it: a later record, gap or snap voids
/// it).
#[derive(Default)]
struct ChainSpace<'a> {
    recs: BTreeMap<u64, (&'a Rec, bool)>,
    handed: bool,
}

/// THE SEQ SPACES THE CHAINS WROTE (session log phase 6, 6A-2, ruling U1):
/// for each `(chain, base)`, the number of its space among that base's,
/// counted in the order the chains began (`start_t`, then instance). A
/// chain continues the base's newest space when the newest chain of it
/// handed the session on (`#handed`: a seamless update's successor), or
/// when every seq both hold is the same record (an overlap journaled
/// twice, a carry's restatement); otherwise it holds another process's
/// records at the same seqs (a relaunch under the same sid by a producer
/// that reused the epoch) and starts a space of its own, newer, so its
/// records are never hidden behind the dead one's. Every chain is space 0
/// with the rule off.
fn space_numbers(
    sid: &ShortId,
    chains: &[ChainRead],
    rules: ViewRules,
) -> BTreeMap<(usize, u32), u32> {
    let mut wrote: BTreeMap<(usize, u32), ChainSpace<'_>> = BTreeMap::new();
    for (ci, chain) in chains.iter().enumerate() {
        let mut segs: Vec<&SegmentRead> = chain.segments.iter().collect();
        segs.sort_by_key(|s| s.n);
        for seg in segs {
            let mut space: Option<u32> = None;
            let mut snap_open = false;
            let mut snap_head = 0;
            for f in &seg.frames {
                match f {
                    Frame::Seg { v, .. } if *v != FORMAT => break,
                    Frame::Snap {
                        sid: s, base, head, ..
                    } if s == sid => {
                        space = Some(*base);
                        snap_open = true;
                        snap_head = *head;
                        let cs = wrote.entry((ci, *base)).or_default();
                        if rules.snap_voids_handed {
                            cs.handed = false;
                        }
                    }
                    Frame::Rec {
                        sid: s,
                        rec,
                        restated,
                    } if s == sid => {
                        if let Some(b) = space {
                            // Only a row the view admits may distinguish spaces.
                            // An ignored restatement is no conflicting evidence.
                            if *restated
                                && (!snap_open || (rules.restated_heads && rec.seq > snap_head))
                            {
                                continue;
                            }
                            if !*restated {
                                snap_open = false;
                            }
                            let cs = wrote.entry((ci, b)).or_default();
                            match cs.recs.get(&rec.seq) {
                                Some((_, true)) if !*restated => {
                                    cs.recs.insert(rec.seq, (rec, false));
                                }
                                Some(_) => {}
                                None => {
                                    cs.recs.insert(rec.seq, (rec, *restated));
                                }
                            }
                            if !*restated {
                                cs.handed = false;
                            }
                        }
                    }
                    Frame::Gap { sid: s, .. } if s == sid => {
                        snap_open = false;
                        if let Some(b) = space {
                            wrote.entry((ci, b)).or_default().handed = false;
                        }
                    }
                    Frame::Hole { sid: s, .. } if s == sid => snap_open = false,
                    Frame::Handed { sid: s, .. } if s == sid => {
                        snap_open = false;
                        if let Some(b) = space {
                            wrote.entry((ci, b)).or_default().handed = true;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    let mut bases: BTreeMap<u32, Vec<(u64, &Inst, usize)>> = BTreeMap::new();
    for &(ci, base) in wrote.keys() {
        let chain = &chains[ci];
        bases
            .entry(base)
            .or_default()
            .push((chain.start_t, &chain.inst, ci));
    }
    let mut out = BTreeMap::new();
    for (base, mut order) in bases {
        order.sort();
        // The newest space's records so far, and whether its newest chain
        // handed the session on.
        let mut held: BTreeMap<u64, &Rec> = BTreeMap::new();
        let mut handed = false;
        let mut n = 0u32;
        for (i, &(.., ci)) in order.iter().enumerate() {
            let cs = &wrote[&(ci, base)];
            let conflict = cs
                .recs
                .iter()
                .any(|(seq, (r, _))| held.get(seq).is_some_and(|h| h != r));
            if i > 0 && rules.split_unlinked && !handed && conflict {
                n = n.saturating_add(1);
                held.clear();
            }
            for (seq, (r, _)) in &cs.recs {
                held.entry(*seq).or_insert(r);
            }
            handed = cs.handed;
            out.insert((ci, base), n);
        }
    }
    out
}

/// The view of session `sid` over the chains a read gathered (module
/// docs); `unread`: the read stopped at its budget, so a range nobody
/// accounts for may still be on disk. `None` when no chain holds a line of
/// the session.
#[must_use]
pub fn view_of(
    sid: &ShortId,
    chains: &[ChainRead],
    unread: bool,
    rules: ViewRules,
) -> Option<View> {
    let numbers = space_numbers(sid, chains, rules);
    let number = |ci: usize, base: u32| numbers.get(&(ci, base)).copied().unwrap_or(0);
    // Keyed by base, then by its space's number among that base's.
    let mut spaces: BTreeMap<(u32, u32), Space> = BTreeMap::new();
    // Per chain: whether the session's last line in it is `#handed`.
    let mut handed: BTreeMap<usize, bool> = BTreeMap::new();
    for (ci, chain) in chains.iter().enumerate() {
        let mut segs: Vec<&SegmentRead> = chain.segments.iter().collect();
        segs.sort_by_key(|s| s.n);
        for seg in segs {
            let mut space: Option<(u32, u32)> = None;
            let mut snap_open = false;
            // The head of the snap whose restatement is being read.
            let mut snap_head = 0u64;
            for f in &seg.frames {
                match f {
                    Frame::Seg { v, .. } if *v != FORMAT => break,
                    Frame::Snap {
                        sid: s,
                        head,
                        ep,
                        base,
                        first,
                        open,
                        t,
                    } if s == sid => {
                        let key = (*base, number(ci, *base));
                        let sp = spaces.entry(key).or_default();
                        sp.ep = sp.ep.max(*ep);
                        sp.first = sp.first.max(*first);
                        sp.last_t = sp.last_t.max(*t);
                        sp.chains.insert((chain.start_t, chain.inst.clone(), ci));
                        sp.snaps.push(SnapSeen {
                            head: *head,
                            t: *t,
                            want: *open,
                            open: BTreeSet::new(),
                        });
                        space = Some(key);
                        snap_open = true;
                        snap_head = *head;
                        if rules.snap_voids_handed {
                            handed.insert(ci, false);
                        }
                    }
                    Frame::Rec {
                        sid: s,
                        rec,
                        restated,
                    } if s == sid => {
                        let Some(sp) = space.and_then(|b| spaces.get_mut(&b)) else {
                            continue;
                        };
                        if *restated {
                            // A restatement is its snap's alone, as of the
                            // snap's head: one elsewhere, or of a seq past the
                            // head, accounts for nothing (and leaves the snap
                            // short of its count, so it closes nothing).
                            if !snap_open || (rules.restated_heads && rec.seq > snap_head) {
                                continue;
                            }
                            if let Some(last) = sp.snaps.last_mut() {
                                last.open.insert(rec.node);
                            }
                        } else {
                            snap_open = false;
                        }
                        sp.last_t = sp.last_t.max(rec.t);
                        match sp.recs.get(&rec.seq) {
                            Some((_, true)) if !*restated => {
                                sp.recs.insert(rec.seq, (rec.clone(), false));
                            }
                            Some(_) => {
                                if !*restated {
                                    sp.dups.push(rec.clone());
                                }
                            }
                            None => {
                                sp.recs.insert(rec.seq, (rec.clone(), *restated));
                            }
                        }
                        // A restatement belongs to its snap, which voided any `#handed`.
                        if !*restated {
                            handed.insert(ci, false);
                        }
                    }
                    Frame::Gap {
                        sid: s,
                        why,
                        after,
                        through,
                        ..
                    } if s == sid => {
                        snap_open = false;
                        if let Some(sp) = space.and_then(|b| spaces.get_mut(&b))
                            && through > after
                        {
                            sp.gaps.insert((*after, *through, *why));
                        }
                        handed.insert(ci, false);
                    }
                    Frame::Hole {
                        sid: s,
                        lo,
                        hi,
                        why,
                    } if s == sid => {
                        snap_open = false;
                        if let Some(sp) = space.and_then(|b| spaces.get_mut(&b))
                            && lo <= hi
                            && *lo > 0
                        {
                            sp.holes.insert((*lo, *hi, *why));
                        }
                    }
                    Frame::Handed { sid: s, .. } if s == sid => {
                        snap_open = false;
                        if space.is_some() {
                            handed.insert(ci, true);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    let ((base, _), sp) = spaces.into_iter().next_back()?;
    build(base, sp, chains, &handed, unread, rules)
}

fn build(
    base: u32,
    sp: Space,
    chains: &[ChainRead],
    handed: &BTreeMap<usize, bool>,
    unread: bool,
    rules: ViewRules,
) -> Option<View> {
    // The newest chain that wrote the space decides how it ended.
    let last_chain = sp.chains.iter().next_back().map(|(.., ci)| *ci)?;
    let chain = chains.get(last_chain)?;
    let dead = !chain.ended
        && chain.live == Liveness::Dead
        && !handed.get(&last_chain).copied().unwrap_or(false);
    // The newest seq the journal accounts for: its records', its gap and
    // hole frames' and its snaps' heads, never a restatement's (a restated
    // open is accounted where it was first written).
    let head = sp
        .recs
        .iter()
        .filter(|(_, (_, restated))| !restated || !rules.restated_heads)
        .map(|(s, _)| *s)
        .chain(sp.gaps.iter().map(|g| g.1))
        .chain(sp.holes.iter().map(|h| h.1))
        .chain(sp.snaps.iter().map(|s| s.head))
        .max()
        .unwrap_or(0);
    // Validate decoded seqs before range arithmetic. A first past the head
    // can describe a writer behind its log, but no log counts past this bound.
    if head > crate::log::SEQ_LIMIT || sp.first > crate::log::SEQ_LIMIT + 1 {
        return None;
    }
    let first = sp.first.max(1);
    // What the journal accounts for: its own records, its gap frames' ranges
    // and the overlaps; a snap's restatement covers nothing (its seqs were
    // accounted where they were first written).
    let mut covered: Vec<Run> = sp
        .recs
        .iter()
        .filter(|(_, (_, restated))| !restated)
        .map(|(s, _)| (*s, *s))
        .chain(sp.gaps.iter().map(|&(a, t, _)| (a.saturating_add(1), t)))
        .chain(sp.holes.iter().map(|&(lo, hi, _)| (lo, hi)))
        .collect();
    covered.sort_unstable();
    // A range nobody accounts for, ending where a successor's handoff gap
    // begins, is the outgoing process's shortfall: a loss, said across the
    // overlap. Any other was pruned (or is unread past the budget).
    let handoff_after: BTreeSet<u64> = sp
        .gaps
        .iter()
        .filter(|g| g.2 == LossWhy::Handoff)
        .map(|g| g.0)
        .collect();
    let restated: BTreeSet<u64> = sp
        .recs
        .iter()
        .filter(|(_, (_, r))| *r)
        .map(|(s, _)| *s)
        .collect();
    let mut holes: BTreeSet<(u64, u64, GapWhy)> =
        sp.holes.iter().copied().filter(|h| h.0 >= first).collect();
    if head >= first {
        for (a, b) in uncovered(first, head, &covered) {
            if handoff_after.contains(&b) && a > first {
                continue;
            }
            let why = if unread {
                GapWhy::Unread
            } else {
                GapWhy::Pruned
            };
            // Carved around the restated opens it holds: a range never held
            // holds no record.
            let mut lo = a;
            for s in restated.range(a..=b) {
                if *s > lo {
                    holes.insert((lo, s - 1, why));
                }
                lo = s + 1;
            }
            if lo <= b {
                holes.insert((lo, b, why));
            }
        }
    }
    // Disjoint: overlapping overlaps from two writers merge.
    let mut merged: Vec<(u64, u64, GapWhy)> = Vec::new();
    for h in holes {
        match merged.last_mut() {
            Some(m) if h.0 <= m.1.saturating_add(1) => m.1 = m.1.max(h.1),
            _ => merged.push(h),
        }
    }
    let mut recs: Vec<Rec> = sp
        .recs
        .values()
        .map(|(r, _)| r.clone())
        .filter(|r| !merged.iter().any(|&(lo, hi, _)| lo <= r.seq && r.seq <= hi))
        .collect();
    let mut img_head = head;
    if !rules.say_losses {
        // NEGATIVE CONTROL: a reader blind to seqs, numbering what it holds
        // densely, says no loss.
        for (i, r) in recs.iter_mut().enumerate() {
            r.seq = first + i as u64;
        }
        merged.clear();
        img_head = (first + recs.len() as u64).saturating_sub(1);
    }
    if !rules.dedupe {
        // NEGATIVE CONTROL: every line it read, each a record of its own.
        for r in &sp.dups {
            img_head += 1;
            let mut r = r.clone();
            r.seq = img_head;
            recs.push(r);
        }
    }
    let ep = sp.ep.max(base);
    let img = JournalImage {
        ep,
        base,
        first,
        head: img_head,
        recs,
        holes: merged,
    };
    let mut log = Log::from_journal(&img).ok()?;
    let journal_head = log.last_seq();
    let victims = unknown_victims(&log, &sp, &img);
    let crash = rules.crash_closes && dead && log.is_open(Node::SESSION);
    // A read that did not reach all of the journal says so: by its holes
    // when what it missed lies within what it accounts for, else by a
    // `gap why=unread` record of its own past the head (what it missed may
    // be newer: a cut listing, a segment past the reader's limit).
    let said_unread = unread && !img.holes.iter().any(|h| h.2 == GapWhy::Unread);
    // The records the reply derives are numbered past the head in the
    // journal's own epoch: no cursor a reply hands out names one
    // ([`View::reply_cursor`]), so each read derives them again. Both gaps
    // say `after=` the seq the journal accounted for last (its head).
    if said_unread {
        log.gap_after(GapWhy::Unread, head, Stamp::new(sp.last_t, 0));
    }
    let unknown_closed = close_unknown(&mut log, victims);
    let mut crashed = false;
    if crash {
        let at = Stamp::new(sp.last_t, 0);
        log.gap_after(GapWhy::Crash, head, at);
        let close =
            Close::new(Out::Session(SessionOut::Gone), Src::Aterm, Conf::Exact).by(Actor::Aterm);
        let _ = log.close_session(close, at);
        crashed = true;
    }
    Some(View {
        log,
        ep,
        head: journal_head,
        crashed,
        unknown_closed,
    })
}

impl View {
    /// THE CURSOR A REPLY OVER THIS VIEW HANDS OUT (its `as_of=` and `ep=`):
    /// the journal's own epoch and head, never one into the records the
    /// reply derives past the head. Those are derived again by the next
    /// read: a later read over more of the journal (it grew, or a read cut
    /// by its budget is followed by a whole one) may hold journaled records
    /// at the same seqs, and a cursor that named the derived ones would read
    /// on past them. No reply's cursor names an epoch the journal never
    /// numbered, so [`View::cursor`] needs no epoch of its own to tell one.
    #[must_use]
    pub fn reply_cursor(&self) -> crate::Cursor {
        crate::Cursor::new(self.ep, self.head)
    }

    /// Where a reader's cursor `c` reads on in this view: one of the
    /// journal's own numbering (an epoch of the space, its base to its
    /// epoch) at or below the head reads on, the replies' own among them
    /// ([`View::reply_cursor`]); every other is reset, and the read says so
    /// (`why=reset`):
    /// - past the head: the dead process's records nobody journaled, which
    ///   the records this reply derives are numbered over;
    /// - an epoch after the journal's: a carried successor's (the journal
    ///   lags it), a later crash recovery's or adoption's (Unix seconds: a
    ///   space the journal does not hold), or a reply's from a reader that
    ///   numbered what it derived in an epoch past the journal's. A
    ///   successor's `+1` and a recovery's seconds share one range of
    ///   numbers, so no epoch above the journal's is read as naming a place
    ///   in it;
    /// - before the space's base: another, earlier space's.
    #[must_use]
    pub fn cursor(&self, c: crate::Cursor) -> crate::Cursor {
        let own = (self.log.base_ep()..=self.ep).contains(&c.ep);
        if own && c.seq <= self.head {
            c
        } else {
            crate::Cursor::new(self.ep, u64::MAX)
        }
    }
}

/// Every node the journal opened and never closed that a later complete
/// snap does not list as open (its close went unjournaled while the writer
/// was behind): (its open's seq, the node, that snap's time).
fn unknown_victims(log: &Log, sp: &Space, img: &JournalImage) -> Vec<(u64, Node, u64)> {
    let opened: BTreeMap<Node, u64> = img
        .recs
        .iter()
        .filter(|r| r.op == Op::Open && r.node != Node::SESSION)
        .map(|r| (r.node, r.seq))
        .collect();
    let mut victims: Vec<(u64, Node, u64)> = Vec::new();
    for e in log.open_nodes() {
        let Some(&at) = opened.get(&e.node) else {
            continue;
        };
        let later = sp
            .snaps
            .iter()
            .find(|s| s.head >= at && s.open.len() as u32 >= s.want && !s.open.contains(&e.node));
        if let Some(s) = later {
            victims.push((at, e.node, s.t));
        }
    }
    victims
}

/// Close `out=unknown`, at its snap's time, each of `victims`
/// ([`unknown_victims`]). Innermost first.
fn close_unknown(log: &mut Log, mut victims: Vec<(u64, Node, u64)>) -> usize {
    // Innermost (latest opened) first.
    victims.sort_by_key(|v| std::cmp::Reverse(v.0));
    let mut n = 0;
    for (_, node, t) in victims {
        if !log.is_open(node) {
            continue;
        }
        let out = match node.lvl {
            Lvl::Run => Out::Run(RunOut::Unknown),
            Lvl::Turn => Out::Turn(TurnOut::Unknown),
            Lvl::Step => Out::Step(StepOut::Unknown),
            Lvl::Session | Lvl::Instance => continue,
        };
        let close = Close::new(out, Src::Aterm, Conf::Strong).by(Actor::Aterm);
        if log.close(node, close, Stamp::new(t, 0)).is_ok() {
            n += 1;
        }
    }
    n
}
