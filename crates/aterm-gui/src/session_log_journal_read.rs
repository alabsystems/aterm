// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Disk journal reader for `log @<sid>` and `brief @<sid>` of sessions from earlier processes,
//! run on the control worker thread within bounded index and decode budgets.

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

/// Reply when a truncated journal read reached no line of the session.
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
    /// No line of it in the reached prefix of a truncated read ([`JOURNAL_CUT`]).
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

/// Install the process-wide reader over `<logs>/session-log`.
pub(crate) fn install(logs: PathBuf) {
    let _ = READER.set(Reader::at(logs));
}

/// Process-wide reader, or `None` when this launch keeps no journal.
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

/// Decode frames for `want` (and instance `*` lines) from a whole segment, adding `bytes.len()` to `cost`.
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

/// Bytes read during index refresh and segment decode.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ReadCost {
    pub(crate) indexed: u64,
    pub(crate) decoded: u64,
}

/// Read `sid` from `fs` within `b`.
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
    segs.sort_by(|a, b| b.mtime_ms.cmp(&a.mtime_ms).then(b.n.cmp(&a.n)));
    if !listing.truncated {
        let listed: BTreeSet<&str> = segs.iter().map(|s| s.name.as_str()).collect();
        index
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .segs
            .retain(|name, _| listed.contains(name.as_str()));
    }
    let want = sid.as_str();
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
            entry.opaque = false;
        }
        entry.indexed_to = from + end as u64;
        index_put(index, &s.name, entry);
    }
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
            unread = true;
            continue;
        }
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
    let facts: Vec<(Inst, Vec<SegmentRead>, Option<bool>, u64)> = {
        let idx = index.lock().unwrap_or_else(|p| p.into_inner());
        frames
            .into_iter()
            .map(|(inst, segments)| {
                let mine = || segs.iter().filter(|s| s.inst == inst);
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

/// Resolve cursor `c` against `view` ([`View::cursor`]).
pub(crate) fn journal_cursor(view: &View, c: Cursor) -> Cursor {
    view.cursor(c)
}

/// Render `log` over `view` with its cursor resolved by [`journal_cursor`].
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
