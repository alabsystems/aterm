// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE JOURNAL'S READER (`docs/DESIGN-session-log-2026-10-15.md` §5.4, §7.2;
//! phase 2B): `log @<sid>` and `brief @<sid>` of a session from an earlier
//! process, answered from the disk journal on the control worker that serves
//! the connection, never the UI thread.
//!
//! - OWNER ONLY. No read grant survives its process, so an edge gets `ERR no
//!   such session`, byte for byte what an unknown sid gets
//!   (`control::closed_session_reply`).
//! - OFF WITH THE JOURNAL (ruling R10): the reader exists only when this
//!   launch keeps a journal ([`install`]); it only opens an existing
//!   directory and never creates one.
//! - BOUNDED (ruling R11), in two steps. THE INDEX REFRESH reads, without
//!   decoding, what segments it has not indexed (or the tail a segment grew
//!   by), at most `journal::INDEX_BUDGET` bytes, so which segments hold the
//!   session is known for a journal within its budgets, never guessed from a
//!   decode budget; it keeps only each segment's sessions, start and end,
//!   one segment's bytes at a time. THE DECODE then reads the segments that
//!   hold the session, newest first: always the newest
//!   `journal::READ_MIN_SEGMENTS`, then more within `journal::READ_BUDGET`
//!   bytes, the same bytes on a cold index as on a warm one. A listing is at most
//!   `journal::LIST_MAX` names. Whatever a read could not reach (a cut
//!   listing, a budget, a segment past `journal::SEGMENT_READ_MAX`) reads
//!   `gap why=unread`; when it found no line of the session at all, the reply
//!   says the read was cut ([`JOURNAL_CUT`]), never that the session does not
//!   exist.
//! - A TRUNCATED LISTING DERIVES NO CRASH: a chain's newest segment may be
//!   among the names it did not list, so its liveness is unknown.
//! - Reads are one at a time (a second waits on the first, whose budgets
//!   bound the wait); the index's lock is a leaf held only to look up, copy
//!   out or store an entry, never across I/O.
//! - The view itself (dedupe, crash closes derived at read time, losses said)
//!   is `aterm_slog::journal::view_of`.

#[cfg(unix)]
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use aterm_slog::journal::View;
#[cfg(unix)]
use aterm_slog::journal::{
    self, ChainRead, FORMAT, Frame, Inst, Liveness, SegmentRead, ViewRules, complete_lines,
    decode_line, is_end_line, line_subject, parse_segment_name, segment_version,
};
use aterm_slog::{Cursor, ShortId};

#[cfg(unix)]
use super::fs::{JournalFs, Meta, list};

/// The reply when a read found no line of the session in what it reached and
/// could not reach all of the journal.
pub(crate) const JOURNAL_CUT: &str = "ERR the session journal was not read whole (too many files or bytes); no line of the session was reached\n";

/// What the index knows of one segment.
#[cfg(unix)]
#[derive(Clone, Debug, Default)]
pub(crate) struct SegIndex {
    dev: u64,
    ino: u64,
    /// The bytes indexed: up to the end of the last complete line.
    indexed_to: u64,
    /// Its `#seg t=`.
    start_t: u64,
    /// It holds a `#fin`.
    ended: bool,
    /// Its first line names no format, or one this build does not read.
    opaque: bool,
    /// The sessions its lines name.
    sids: BTreeSet<String>,
}

/// The index of every segment a read has looked at, by name.
#[derive(Default)]
pub(crate) struct Index {
    #[cfg(unix)]
    segs: BTreeMap<String, SegIndex>,
}

/// A read's bounds ([`Budgets::READER`]; tests shrink them).
#[cfg(unix)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct Budgets {
    /// The index refresh's bytes.
    pub(crate) index: u64,
    /// The decode's bytes past its first `min_segments`.
    pub(crate) decode: u64,
    /// The segments holding the session decoded whatever their size.
    pub(crate) min_segments: usize,
}

#[cfg(unix)]
impl Budgets {
    /// The reader's.
    pub(crate) const READER: Budgets = Budgets {
        index: journal::INDEX_BUDGET,
        decode: journal::READ_BUDGET,
        min_segments: journal::READ_MIN_SEGMENTS,
    };
}

/// What one journal read answers for a session.
#[derive(Debug)]
#[cfg_attr(
    not(unix),
    expect(dead_code, reason = "only the unix reader reads a journal")
)]
pub(crate) enum JournalRead {
    /// The session's view.
    View(Box<View>),
    /// No line of it in a journal read whole (or no journal).
    Absent,
    /// No line of it in what the read reached, which was not all: whether
    /// the journal holds it is not known ([`JOURNAL_CUT`]).
    Cut,
}

/// This launch's reader (a journal-keeping launch's only).
pub(crate) struct Reader {
    logs: PathBuf,
    /// One read at a time.
    flight: Mutex<()>,
    index: Mutex<Index>,
}

static READER: OnceLock<Reader> = OnceLock::new();

/// Make this launch's reader over `<logs>/session-log` (the lane does, when
/// the gate passed).
pub(crate) fn install(logs: PathBuf) {
    let _ = READER.set(Reader::at(logs));
}

/// This launch's reader: `None` while it keeps no journal, so nothing is
/// read and nothing created.
pub(crate) fn reader() -> Option<&'static Reader> {
    READER.get()
}

impl Reader {
    /// A reader over `<logs>/session-log`.
    pub(crate) fn at(logs: PathBuf) -> Reader {
        Reader {
            logs,
            flight: Mutex::new(()),
            index: Mutex::new(Index::default()),
        }
    }

    /// What the journal answers for `sid` (no directory: [`JournalRead::Absent`]).
    pub(crate) fn view(&self, sid: &ShortId) -> JournalRead {
        let _one = self.flight.lock().unwrap_or_else(|p| p.into_inner());
        #[cfg(unix)]
        {
            let dir = match super::fs::PinnedJournalFs::open(&self.logs, false) {
                Ok(Some(dir)) => dir,
                Ok(None) => return JournalRead::Absent,
                Err(error) => {
                    aterm_log::warn!("session journal: not read ({error})");
                    return JournalRead::Cut;
                }
            };
            view_with(&dir, &self.index, sid, Budgets::READER)
        }
        #[cfg(not(unix))]
        {
            let _ = (&self.logs, &self.index, sid);
            JournalRead::Absent
        }
    }
}

/// One listed segment of the journal.
#[cfg(unix)]
struct Listed {
    mtime_ms: u64,
    name: String,
    inst: Inst,
    n: u32,
    meta: Meta,
}

#[cfg(unix)]
fn index_get(index: &Mutex<Index>, name: &str) -> Option<SegIndex> {
    index
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .segs
        .get(name)
        .cloned()
}

#[cfg(unix)]
fn index_put(index: &Mutex<Index>, name: &str, entry: SegIndex) {
    index
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .segs
        .insert(name.to_string(), entry);
}

/// The frames of `want` (and the instance's own `*` lines) in a segment read
/// whole: none for a segment whose first line names another format. Every
/// byte it is given counts as decoded in `cost`.
#[cfg(unix)]
fn decode_whole(bytes: &[u8], want: &str, cost: &mut ReadCost) -> Option<Vec<Frame>> {
    cost.decoded = cost.decoded.saturating_add(bytes.len() as u64);
    let mut lines = complete_lines(bytes);
    let first = lines.next()?;
    if segment_version(first) != Some(FORMAT) {
        return None;
    }
    let mut out: Vec<Frame> = decode_line(first).ok().into_iter().collect();
    for line in lines {
        match line_subject(line) {
            Some(b"*") => out.extend(decode_line(line).ok()),
            Some(s) if s == want.as_bytes() => out.extend(decode_line(line).ok()),
            _ => {}
        }
    }
    Some(out)
}

/// What one read spent: the bytes its index refresh read and the bytes it
/// decoded.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ReadCost {
    pub(crate) indexed: u64,
    pub(crate) decoded: u64,
}

/// One read of `sid` over `fs` within `b` (module docs).
#[cfg(unix)]
pub(crate) fn view_with(
    fs: &dyn JournalFs,
    index: &Mutex<Index>,
    sid: &ShortId,
    b: Budgets,
) -> JournalRead {
    view_costed(fs, index, sid, b, &mut ReadCost::default())
}

/// [`view_with`], adding what it spent to `cost`.
#[cfg(unix)]
pub(crate) fn view_costed(
    fs: &dyn JournalFs,
    index: &Mutex<Index>,
    sid: &ShortId,
    b: Budgets,
    cost: &mut ReadCost,
) -> JournalRead {
    let Ok(listing) = list(fs) else {
        return JournalRead::Cut;
    };
    let mut unread = listing.truncated;
    let mut segs: Vec<Listed> = listing
        .names
        .iter()
        .filter_map(|name| {
            let (inst, n) = parse_segment_name(name)?;
            let meta = fs.meta(name).ok().filter(|m| m.private)?;
            Some(Listed {
                mtime_ms: meta.mtime_ms,
                name: name.clone(),
                inst,
                n,
                meta,
            })
        })
        .collect();
    // Newest first, so a budget cut leaves the oldest unread.
    segs.sort_by(|a, b| b.mtime_ms.cmp(&a.mtime_ms).then(b.n.cmp(&a.n)));
    if !listing.truncated {
        // Only a whole listing says a name is gone.
        let listed: BTreeSet<&str> = segs.iter().map(|s| s.name.as_str()).collect();
        index
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .segs
            .retain(|name, _| listed.contains(name.as_str()));
    }
    let want = sid.as_str();
    // THE INDEX REFRESH: what is new, and the tail a segment grew by.
    let mut spent_index = 0u64;
    for s in &segs {
        let known =
            index_get(index, &s.name).filter(|k| k.dev == s.meta.dev && k.ino == s.meta.ino);
        let from = known.as_ref().map_or(0, |k| {
            if k.indexed_to <= s.meta.size {
                k.indexed_to
            } else {
                0
            }
        });
        if known.is_some() && from == s.meta.size {
            continue;
        }
        if s.meta.size > journal::SEGMENT_READ_MAX {
            // Past what any writer of this journal writes: not read, and
            // what it may hold is unread.
            unread = true;
            continue;
        }
        let len = s.meta.size - from;
        if spent_index.saturating_add(len) > b.index {
            unread = true;
            continue;
        }
        let Ok((bytes, _)) = fs.read(&s.name, from, len) else {
            unread = true;
            continue;
        };
        spent_index = spent_index.saturating_add(bytes.len() as u64);
        cost.indexed = cost.indexed.saturating_add(bytes.len() as u64);
        let mut entry = match known.filter(|_| from > 0) {
            Some(k) => k,
            None => SegIndex {
                dev: s.meta.dev,
                ino: s.meta.ino,
                ..SegIndex::default()
            },
        };
        let end = bytes.iter().rposition(|&c| c == b'\n').map_or(0, |i| i + 1);
        for (i, line) in complete_lines(&bytes).enumerate() {
            if from == 0 && i == 0 {
                entry.opaque = segment_version(line) != Some(FORMAT);
                if let Ok(Frame::Seg { t, .. }) = decode_line(line) {
                    entry.start_t = t;
                }
            }
            if entry.opaque {
                break;
            }
            match line_subject(line) {
                Some(b"*") => entry.ended |= is_end_line(line),
                Some(subject) => {
                    if let Ok(name) = std::str::from_utf8(subject) {
                        entry.sids.insert(name.to_string());
                    }
                }
                None => {}
            }
        }
        if from == 0 && end == 0 {
            // No complete line yet: its version is not known either.
            entry.opaque = false;
        }
        entry.indexed_to = from + end as u64;
        index_put(index, &s.name, entry);
    }
    // THE DECODE: the segments that hold the session, newest first.
    let mut spent = 0u64;
    let mut decoded = 0usize;
    let mut held = 0usize;
    let mut frames: BTreeMap<Inst, Vec<SegmentRead>> = BTreeMap::new();
    for s in &segs {
        let Some(k) = index_get(index, &s.name) else {
            continue;
        };
        if k.opaque || !k.sids.contains(want) {
            continue;
        }
        if s.meta.size > journal::SEGMENT_READ_MAX {
            // Grown past what any writer of this journal writes since it was
            // indexed: not read.
            unread = true;
            continue;
        }
        // Past half of what a rebuilt log takes, no more segments: one more
        // (at most `SEGMENT_READ_MAX` of lines) still fits.
        if held >= aterm_slog::JOURNAL_IMAGE_MAX_RECORDS / 2 {
            unread = true;
            continue;
        }
        if decoded >= b.min_segments && spent.saturating_add(s.meta.size) > b.decode {
            unread = true;
            continue;
        }
        let got = fs
            .read(&s.name, 0, s.meta.size)
            .ok()
            .and_then(|(bytes, _)| decode_whole(&bytes, want, cost));
        let Some(got) = got else {
            unread = true;
            continue;
        };
        spent = spent.saturating_add(s.meta.size);
        decoded += 1;
        held += got.len();
        frames.entry(s.inst.clone()).or_default().push(SegmentRead {
            n: s.n,
            frames: got,
        });
    }
    if frames.is_empty() {
        return if unread {
            JournalRead::Cut
        } else {
            JournalRead::Absent
        };
    }
    // Each chain's facts, copied out under the index's lock; the probes run
    // after it is let go.
    let facts: Vec<(Inst, Vec<SegmentRead>, Option<bool>, u64)> = {
        let idx = index.lock().unwrap_or_else(|p| p.into_inner());
        frames
            .into_iter()
            .map(|(inst, segments)| {
                let mine = || segs.iter().filter(|s| s.inst == inst);
                // The chain's newest listed segment decides whether it
                // ended; one the index does not hold is not known.
                let ended = mine()
                    .max_by_key(|s| s.n)
                    .and_then(|s| idx.segs.get(&s.name))
                    .map(|e| e.ended);
                let start_t = mine()
                    .filter_map(|s| idx.segs.get(&s.name))
                    .map(|e| e.start_t)
                    .filter(|t| *t > 0)
                    .min()
                    .unwrap_or(0);
                (inst, segments, ended, start_t)
            })
            .collect()
    };
    let chains: Vec<ChainRead> = facts
        .into_iter()
        .map(|(inst, segments, ended, start_t)| {
            let live = match ended {
                Some(true) => Liveness::Dead,
                // A cut listing may not show the chain's newest segment.
                Some(false) if !listing.truncated => fs.probe(&journal::lock_name(&inst)),
                _ => Liveness::Unknown,
            };
            ChainRead {
                ended: ended.unwrap_or(false),
                live,
                start_t,
                inst,
                segments,
            }
        })
        .collect();
    match journal::view_of(sid, &chains, unread, ViewRules::READER) {
        Some(view) => JournalRead::View(Box::new(view)),
        None if unread => JournalRead::Cut,
        None => JournalRead::Absent,
    }
}

/// A reader's cursor against a journal's view ([`View::cursor`]): one of
/// the journal's own numbering at or below its head reads on (a reply's own,
/// [`View::reply_cursor`], among them); every other is reset, said: past
/// the head (the dead process's records nobody journaled), of an epoch past
/// the journal's (a successor's, another space's) or before its space.
pub(crate) fn journal_cursor(view: &View, c: Cursor) -> Cursor {
    view.cursor(c)
}

/// `log` over a journal's view: the live reply's shape, a cursor placed by
/// [`journal_cursor`], the header handing out the journal's own cursor
/// ([`View::reply_cursor`]): what the reply derives past the head is
/// derived again by the next read, never named by a cursor.
pub(crate) fn cmd_log_journal(view: &View, rest: &str) -> String {
    let rest = match crate::control::take_json_flag(rest) {
        (true, body) => format!("{body} --json"),
        (false, _) => rest.to_string(),
    };
    let mut query = match crate::session_log::LogQuery::parse(&rest) {
        Ok(q) => q,
        Err(usage) => return usage.to_string(),
    };
    query.cursor = query.cursor.map(|c| journal_cursor(view, c));
    crate::session_log::render_log_copy(
        &crate::session_log::LogCopy::copied_from(&view.log, &query).handing(view.reply_cursor()),
        &query,
    )
}
