// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THIS SESSION'S USAGE (owner function 2, 2026-09-25): what the Claude Code
//! footer's `Σ` segment (one merged total, shown unless `[claude_footer] usage
//! = false`) and its limit wall show ([`super::footer`]) and what `aterm
//! harness usage` prints, per model ([`super::cli`]) — ONE implementation for
//! both, read from the session's own FILES and nothing else. Nothing is read
//! off the screen. The footer keeps one [`SessionUsage`] per watched session
//! (`footer::FooterCache`), beside the transcript-tail memo it keeps per
//! PROCESS (`footer::TailCache`); model and effort still come from that
//! tail, floored at the process's start, while the
//! tokens here are the CONVERSATION's — every row of its transcript, a
//! resumed conversation's earlier processes included.
//!
//! * **TOKENS — LIVE.** Per model, folded from this session's OWN transcript
//!   files: `<project>/<session>.jsonl`, the one `sessions/<pid>.json` names,
//!   and the subagent transcripts Claude Code keeps beside it
//!   (`<project>/<session>/subagents/…/agent-*.jsonl`, most of them under
//!   `workflows/<run>/`; MEASURED in the 2.1.283 bundle, where a subagent's
//!   turns are written — the main transcript carries none of them). Each
//!   file is folded INCREMENTALLY ([`TranscriptCursor`]): the byte offset and
//!   the running fold are kept, a re-read reads only the bytes appended since
//!   the last one, and a file whose size, mtime and inode have not moved is
//!   not even opened. Each `message.id` counts once, at the largest counts
//!   its rows carry — a subagent's message is STREAMED over several rows
//!   whose output grows ([`super::usage::TranscriptUsage`]'s rule, and its
//!   measurement) — and a line is folded only once its newline is written.
//!   Files that cannot be counted are said ([`Refresh::unread`]), never
//!   silently left out of a total that calls itself complete.
//! * **RUNTIME — the time Claude was working.** The union of the MAIN
//!   transcript's finished turns, each `[timestamp − durationMs, timestamp]`
//!   of the `turn_duration` row Claude Code writes at its end
//!   ([`TranscriptUsage::busy_ms`], [`SessionUsage::busy_ms`]), carried as
//!   [`UsageFacts::runtime`] once the fold caught up — `Σ 7h52m ↑395M ↓2.0M`.
//!   Not the durations' sum (a workflow's turn overlaps the turns after it),
//!   not the first-to-last span (mostly idle), not the `cost-state` API time
//!   (written only at exit, with no timestamp). Subagent files add nothing
//!   to it (measured: 0.00h and 0.13h outside the main turns in two long
//!   sessions).
//! * **THE LIMIT WALL — the one account figure.** Claude Code writes a limit
//!   it hit into the transcript as a row of its own (`isApiErrorMessage`,
//!   `You've hit your session limit · resets 3pm (…)`, its own `timestamp`;
//!   [`super::usage::LimitNoticeRow`]). The newest such row in the MAIN
//!   transcript, its reset placed from the row's time in real time
//!   ([`wall_of`]), is shown while that reset is ahead and nothing has been
//!   served since — `5h ♻ 3pm` — and not at all once it has
//!   passed, once a response is served after it in ANY of the session's
//!   files (`/limit-reset`; the limit is the account's, so a workflow's
//!   subagent served counts — [`SessionUsage::served_at`]), or when it
//!   cannot be placed. A resumed conversation, a restart or an aterm update
//!   does not move a row's time. The band's `limited → 3pm` reads the
//!   SCREEN's notice and counts down from now; this reads the transcript's
//!   row, so the two can differ (a notice scrolled away, or one on the
//!   screen but older than a served response).
//!
//! Nothing here writes anything and nothing is installed into the agent:
//! decision "B" (2026-09-22) stands — no hook, no statusLine, no plugin, no
//! `--settings`. The files are the vendor's own, read from outside.
//!
//! STATUS (docs/README.md honesty ratchet): unit-tested against temp-file
//! fixtures; the footer and `aterm harness usage` both read through it. The
//! notice row's shape (`isApiErrorMessage`, `<synthetic>`, the text, the
//! `timestamp`), the `subagents/**/agent-*.jsonl` layout and the streamed
//! usage growth were MEASURED on Claude Code 2.1.283; no vendor-corpus
//! fixture carries a limit-notice transcript row yet. The `turn_duration`
//! row (`durationMs`, `timestamp`, `isSidechain`) was MEASURED in real
//! transcripts of 2.1.282, 2.1.283, 2.1.284 and 2.1.286 (528 of 548 prompted
//! turns in the 150 newest here carry one). KNOWN UNDERCOUNT, read in the
//! 2.1.286 bundle: the row is appended only `if (… && !signal.aborted)`, so a
//! turn interrupted with Esc writes none, and a turn in flight when the
//! process ends (a relaunch, a crash) never reaches its end — both turns'
//! time is lost. Its paused time (a permission dialog) is subtracted from
//! `durationMs` by the vendor, so a turn's span starts that much late.

use std::collections::BTreeMap;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::usage::{LimitNoticeRow, MAX_LINE_BYTES, ModelSpend, TranscriptUsage, model_short};

/// How much one read call asks the kernel for.
const READ_CHUNK: usize = 64 * 1024;

/// A `pending` line buffer larger than this is released once the line it
/// held is folded, so one long tool-result row does not pin its allocation
/// through the rest of a read. Between reads nothing is kept but the bytes
/// of a torn last line ([`TranscriptCursor::advance`]).
const PENDING_KEEP: usize = 1024 * 1024;

/// The most subagent transcripts one session's fold follows; past it every new
/// file is left and counted ([`Refresh::unread`]), newest admitted first. A
/// cursor costs about 5 KB while its file grows and 2.6 KB once quiet, so at
/// most about 11 MB per session.
pub const MAX_SUBAGENT_FILES: usize = 2048;

/// A subagent file unchanged this long (by its mtime) RELEASES its message
/// ids ([`TranscriptCursor::release`]): only its totals are kept, a later
/// append is read on, and any other change is counted unread.
pub const QUIET_AFTER: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// The most directory entries the subagent walk looks at, over all levels
/// (the 551-file session used 1,111). A walk cut short makes the fold
/// incomplete ([`Refresh::walk_cut`]).
const MAX_SUBAGENT_ENTRIES: usize = 16_384;

/// How deep under `<session>/subagents/` the walk goes: the vendor nests a
/// subagent's transcript under a subdirectory (`agentTranscriptSubdirs`) and
/// a workflow's under `workflows/<run>/`.
const MAX_SUBAGENT_DEPTH: usize = 4;

/// The models `aterm harness usage` names; the rest are counted (`+2`).
/// The footer names none: its `Σ` sums every model ([`UsageGrain::Total`]).
pub const NAMED_MODELS: usize = 3;

/// The message ids a finished subagent file keeps until quiet for
/// [`QUIET_AFTER`]: the newest few, so a message still streaming when the file
/// paused is raised, not counted again, if it grows.
pub const FINISHED_IDS_KEPT: usize = 16;

// ---------------------------------------------------------------------------
// One transcript, folded incrementally
// ---------------------------------------------------------------------------

/// The file a cursor has been folding, as the kernel names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileId {
    dev: u64,
    ino: u64,
}

#[cfg(unix)]
fn file_id(meta: &std::fs::Metadata) -> Option<FileId> {
    use std::os::unix::fs::MetadataExt as _;
    Some(FileId {
        dev: meta.dev(),
        ino: meta.ino(),
    })
}

#[cfg(not(unix))]
fn file_id(_meta: &std::fs::Metadata) -> Option<FileId> {
    None
}

/// What `stat` says about a file: enough to know it has not moved since a
/// read, without opening it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    len: u64,
    mtime: Option<SystemTime>,
    id: Option<FileId>,
}

/// What `stat` says about the file at `path` now, if it can be read — the
/// one stat helper.
pub(crate) fn stamp_of(path: &Path) -> Option<Stamp> {
    std::fs::metadata(path).ok().map(|m| Stamp::of(&m))
}

impl Stamp {
    fn of(meta: &std::fs::Metadata) -> Self {
        Self {
            len: meta.len(),
            mtime: meta.modified().ok(),
            id: file_id(meta),
        }
    }
}

/// What one [`TranscriptCursor::advance`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Advance {
    /// Bytes read from the file by this call — the appended bytes, plus the
    /// one byte of the boundary check once the fold is past the start; `0`
    /// for a file whose size, mtime and inode have not moved (it is not
    /// opened).
    pub read: u64,
    /// Whether the fold now reaches the end the file had when this call
    /// looked.
    pub at_end: bool,
    /// Whether the file was found shrunk, replaced or rewritten, and the
    /// fold started over from its first byte.
    pub restarted: bool,
    /// Whether the file changed other than by an append after its message ids
    /// were released ([`TranscriptCursor::release`]): nothing was read or
    /// restarted; its totals stand, and it is unread.
    pub rewritten: bool,
}

/// One transcript file folded incrementally: the byte offset reached, the
/// running [`TranscriptUsage`], and a torn last line awaiting its newline.
/// Each [`Self::advance`] reads only what was appended since the last.
#[derive(Debug, Clone)]
pub struct TranscriptCursor {
    path: PathBuf,
    /// The file folded so far — a different one at the same path is a
    /// rotation, and the fold starts over.
    identity: Option<FileId>,
    /// Bytes consumed: every complete line folded, plus `pending`.
    offset: u64,
    /// The byte at `offset - 1` when it was read: a file that no longer holds
    /// it there was rewritten in place.
    boundary: Option<u8>,
    /// The torn last line: bytes after the last newline, not folded until
    /// their newline is written. Only those bytes are held between reads.
    pending: Vec<u8>,
    /// The pending line passed [`MAX_LINE_BYTES`]: its bytes are discarded
    /// up to its newline, and it is counted as skipped, never held.
    overflow: bool,
    fold: TranscriptUsage,
    /// The file as the last read found it: unchanged, it is not opened.
    seen: Option<Stamp>,
    /// Every byte this cursor has read from disk (the tests' meter).
    bytes_read: u64,
    /// How many times the fold started over.
    restarts: u64,
    /// Whether the last [`Self::advance`] could read the file.
    readable: bool,
    /// Its message ids were released ([`Self::release`]): only an APPEND can
    /// be read on; any other change leaves it `rewritten`.
    released: bool,
    /// Found rewritten after its ids were released: frozen, its totals
    /// standing, counted unread from then on.
    rewritten: bool,
}

impl TranscriptCursor {
    /// A cursor at the start of `path`; nothing is read until
    /// [`Self::advance`].
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            identity: None,
            offset: 0,
            boundary: None,
            pending: Vec::new(),
            overflow: false,
            fold: TranscriptUsage::new(),
            seen: None,
            bytes_read: 0,
            restarts: 0,
            readable: true,
            released: false,
            rewritten: false,
        }
    }

    /// Release a finished file's message ids (its totals are all a view
    /// needs). From here only a plain append (same inode, from the size
    /// reached, the boundary byte in place) is read; any other change is
    /// `rewritten`, counted unread, never re-read from the start. A rewrite in
    /// place that keeps the file at least as long and the boundary byte where
    /// it was cannot be seen.
    fn release(&mut self) {
        self.fold.shed_seen(0);
        self.released = true;
    }

    /// The file this cursor folds.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The fold so far.
    pub fn fold(&self) -> &TranscriptUsage {
        &self.fold
    }

    /// Bytes of the file consumed so far (complete lines and the torn tail).
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// How many times the fold started over.
    pub fn restarts(&self) -> u64 {
        self.restarts
    }

    /// Every byte this cursor has read from disk, over its whole life.
    #[cfg(test)]
    pub(crate) fn bytes_read(&self) -> u64 {
        self.bytes_read
    }

    /// Whether the file `meta` describes is exactly the one the last read
    /// left, read to its end: same size, mtime and inode.
    fn unchanged(&self, meta: &std::fs::Metadata) -> bool {
        let now = Stamp::of(meta);
        self.seen == Some(now) && self.offset >= now.len
    }

    fn restart(&mut self) {
        self.fold = TranscriptUsage::new();
        self.offset = 0;
        self.boundary = None;
        self.pending = Vec::new();
        self.overflow = false;
        self.seen = None;
        self.restarts = self.restarts.saturating_add(1);
    }

    /// Read what the file appended since the last call, at most `budget` bytes,
    /// and fold every line it completes.
    ///
    /// An unchanged file (size, mtime, inode) is not opened; only a regular
    /// file is read, non-blocking. The fold starts over when the file was
    /// truncated, rotated or rewritten (the byte before the offset changed),
    /// unless its ids were released, when the change is `rewritten` instead.
    ///
    /// # Errors
    ///
    /// The file cannot be opened or read; what was folded before the error is
    /// kept, and the offset is exactly what was consumed.
    pub fn advance(&mut self, budget: u64) -> io::Result<Advance> {
        if let Ok(meta) = std::fs::metadata(&self.path)
            && self.unchanged(&meta)
        {
            return Ok(Advance {
                read: 0,
                at_end: true,
                restarted: false,
                rewritten: false,
            });
        }
        self.readable = false;
        let mut file = super::footer::open_regular(&self.path).ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "not a readable regular file")
        })?;
        self.readable = true;
        let meta = file.metadata()?;
        let len = meta.len();
        let id = file_id(&meta);
        let mut read: u64 = 0;
        let mut restarted = false;
        let frozen = |read: u64| Advance {
            read,
            at_end: true,
            restarted: false,
            rewritten: true,
        };
        if (self.identity.is_some() && self.identity != id) || len < self.offset {
            if self.released {
                self.rewritten = true;
                return Ok(frozen(0));
            }
            self.restart();
            restarted = true;
        }
        self.identity = id;
        // (A file shorter than the offset was caught above; the byte under
        // the boundary exists.)
        if let (Some(expect), Some(at)) = (self.boundary, self.offset.checked_sub(1))
            && self.offset <= len
        {
            file.seek(SeekFrom::Start(at))?;
            let mut byte = [0u8; 1];
            let n = file.read(&mut byte)?;
            read += n as u64;
            self.bytes_read = self.bytes_read.saturating_add(n as u64);
            if n != 1 || byte[0] != expect {
                if self.released {
                    self.rewritten = true;
                    return Ok(frozen(read));
                }
                self.restart();
                restarted = true;
            }
        }
        let want = len.saturating_sub(self.offset).min(budget);
        if want > 0 {
            file.seek(SeekFrom::Start(self.offset))?;
            let mut chunk = vec![0u8; READ_CHUNK.min(usize::try_from(want).unwrap_or(READ_CHUNK))];
            let mut left = want;
            while left > 0 {
                let ask = chunk
                    .len()
                    .min(usize::try_from(left).unwrap_or(chunk.len()));
                let n = file.read(&mut chunk[..ask])?;
                if n == 0 {
                    break;
                }
                self.consume(&chunk[..n]);
                let n64 = n as u64;
                left -= n64.min(left);
                read += n64;
                self.bytes_read = self.bytes_read.saturating_add(n64);
                self.offset += n64;
                self.boundary = Some(chunk[n - 1]);
            }
        }
        // Between reads, keep the torn tail's bytes and nothing more: a
        // finished file (most subagents') never needs a line buffer again,
        // and hundreds of cursors each pinning one add up to tens of MB.
        self.pending.shrink_to_fit();
        self.seen = Some(Stamp::of(&meta));
        Ok(Advance {
            read,
            at_end: self.offset >= len,
            restarted,
            rewritten: false,
        })
    }

    /// Fold every line `bytes` completes; keep the torn tail pending.
    fn consume(&mut self, mut bytes: &[u8]) {
        while let Some(nl) = bytes.iter().position(|&b| b == b'\n') {
            let line = &bytes[..nl];
            if self.overflow {
                self.fold.note_long_line();
                self.overflow = false;
            } else if self.pending.is_empty() {
                self.fold.fold_bytes(line);
            } else if self.pending.len().saturating_add(line.len()) > MAX_LINE_BYTES {
                self.fold.note_long_line();
            } else {
                self.pending.extend_from_slice(line);
                let whole = std::mem::take(&mut self.pending);
                self.fold.fold_bytes(&whole);
                if whole.capacity() <= PENDING_KEEP {
                    self.pending = whole;
                }
            }
            self.pending.clear();
            bytes = &bytes[nl + 1..];
        }
        if bytes.is_empty() || self.overflow {
            return;
        }
        if self.pending.len().saturating_add(bytes.len()) > MAX_LINE_BYTES {
            self.overflow = true;
            self.pending = Vec::new();
        } else {
            self.pending.extend_from_slice(bytes);
        }
    }
}

// ---------------------------------------------------------------------------
// One session: its transcript and its subagents' transcripts
// ---------------------------------------------------------------------------

/// What one [`SessionUsage::refresh`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Refresh {
    /// Whether every file FOLLOWED has been read to its end — the totals are
    /// not a mid-read prefix. What is not followed is [`Self::unread`].
    pub caught_up: bool,
    /// Subagent transcripts that exist with rows not in the totals, each
    /// counted once: unreadable, past [`MAX_SUBAGENT_FILES`], or changed other
    /// than by an append after its ids were released.
    pub unread: usize,
    /// The subagent walk stopped at its entry bound: more files may exist
    /// than were seen, and none of those is counted.
    pub walk_cut: bool,
    /// How much of the budget this refresh used: all of it when the budget,
    /// not the files, ended the read.
    pub spent: u64,
}

impl Refresh {
    /// Whether the totals are the whole session's: caught up, nothing
    /// unread, the walk whole.
    #[must_use]
    pub fn complete(&self) -> bool {
        self.caught_up && self.unread == 0 && !self.walk_cut
    }
}

/// One Claude Code session's token usage: its transcript and every subagent
/// transcript beside it, each folded incrementally, summed per model.
#[derive(Debug, Clone)]
pub struct SessionUsage {
    main: TranscriptCursor,
    /// Every subagent file followed, at most [`MAX_SUBAGENT_FILES`]; each
    /// has been read at least once.
    subagents: BTreeMap<PathBuf, TranscriptCursor>,
    /// Every directory the last walk looked at, with what `stat` said of it
    /// (`None`: not there yet): a new subagent's file, a new workflow run's
    /// directory, or `subagents/` itself appearing changes one.
    dirs_seen: Vec<(PathBuf, Option<Stamp>)>,
    /// Whether the last refresh caught up (see [`Refresh::caught_up`]), and
    /// whether it read anything: a fold behind that did not move on its last
    /// read is not re-read until a file changes.
    caught_up: bool,
    progressed: bool,
    /// The most files followed ([`MAX_SUBAGENT_FILES`]; a test sets it low).
    cap: usize,
    /// The most directory entries one walk looks at
    /// ([`MAX_SUBAGENT_ENTRIES`]; a test sets it low).
    walk_max: usize,
}

/// Advance one subagent cursor within the budget `left`: a file read to its
/// end sheds its ids to [`FINISHED_IDS_KEPT`]; one that cannot be read now,
/// or is found rewritten after its ids were released, is `unread`. Whether
/// it could be read.
fn advance_subagent(
    cursor: &mut TranscriptCursor,
    left: &mut u64,
    caught_up: &mut bool,
    unread: &mut usize,
) -> bool {
    match cursor.advance(*left) {
        Ok(a) if a.rewritten => {
            *left = left.saturating_sub(a.read);
            *unread += 1;
            true
        }
        Ok(a) => {
            *left = left.saturating_sub(a.read);
            *caught_up &= a.at_end;
            if a.at_end {
                cursor.fold.shed_seen(FINISHED_IDS_KEPT);
            }
            true
        }
        Err(_) => {
            *unread += 1;
            false
        }
    }
}

impl SessionUsage {
    /// The session whose transcript is `transcript`.
    pub fn new(transcript: impl Into<PathBuf>) -> Self {
        Self {
            main: TranscriptCursor::new(transcript),
            subagents: BTreeMap::new(),
            dirs_seen: Vec::new(),
            caught_up: false,
            progressed: false,
            cap: MAX_SUBAGENT_FILES,
            walk_max: MAX_SUBAGENT_ENTRIES,
        }
    }

    /// The session's transcript.
    pub fn transcript(&self) -> &Path {
        self.main.path()
    }

    /// The transcript's own cursor.
    pub fn main(&self) -> &TranscriptCursor {
        &self.main
    }

    /// Where the vendor keeps this session's subagent transcripts:
    /// `<project>/<session>/subagents` beside `<project>/<session>.jsonl`.
    pub fn subagent_dir(&self) -> Option<PathBuf> {
        let path = self.main.path();
        Some(path.parent()?.join(path.file_stem()?).join("subagents"))
    }

    /// Whether a refresh now could find anything the last one did not: the last
    /// one did not catch up and did read something; the transcript moved (size,
    /// mtime or inode); a directory the walk visited changed (a new subagent
    /// file at any depth, or a new run's directory); or any followed subagent
    /// file changed since its last read (every one: an agent in a long tool
    /// call is the least recently written). `stat`s only, one per followed file
    /// (at most [`MAX_SUBAGENT_FILES`]); nothing is opened or walked.
    #[must_use]
    pub fn moved(&self) -> bool {
        (!self.caught_up && self.progressed)
            || std::fs::metadata(self.main.path()).map_or(true, |m| !self.main.unchanged(&m))
            || self
                .dirs_seen
                .iter()
                .any(|(dir, seen)| stamp_of(dir) != *seen)
            || self
                .subagents
                .values()
                .filter(|cursor| !cursor.rewritten)
                .any(|cursor| stamp_of(cursor.path()).is_some_and(|now| Some(now) != cursor.seen))
    }

    /// Read what every file of the session appended since the last call (the
    /// transcript first, then each subagent transcript), at most `budget` bytes
    /// over all of them. Unchanged files are not opened; one directory walk.
    ///
    /// A new subagent file is admitted (newest first) while fewer than
    /// [`MAX_SUBAGENT_FILES`] are followed and it is readable; a followed file
    /// is never dropped. A file still new when the budget ran out is not
    /// admitted, and the read is not caught up.
    pub fn refresh(&mut self, budget: u64) -> Refresh {
        let mut left = budget;
        let mut caught_up = match self.main.advance(left) {
            Ok(a) => {
                left = left.saturating_sub(a.read);
                a.at_end
            }
            Err(_) => false,
        };
        let mut unread = 0usize;
        let mut walk_cut = false;
        if let Some(dir) = self.subagent_dir() {
            let listing = list_subagents(&dir, self.walk_max);
            walk_cut = listing.cut;
            self.dirs_seen = listing.dirs;
            let quiet_since = SystemTime::now().checked_sub(QUIET_AFTER);
            let mut fresh: Vec<(PathBuf, std::fs::Metadata)> = Vec::new();
            for (path, meta) in listing.files {
                let Some(cursor) = self.subagents.get_mut(&path) else {
                    fresh.push((path, meta));
                    continue;
                };
                if cursor.rewritten {
                    unread += 1;
                    continue;
                }
                if !cursor.unchanged(&meta) {
                    if left == 0 {
                        // Nothing can be read now: the next read with
                        // budget opens it.
                        caught_up = false;
                        continue;
                    }
                    advance_subagent(cursor, &mut left, &mut caught_up, &mut unread);
                }
                if !cursor.released
                    && cursor.unchanged(&meta)
                    && quiet_since.is_some_and(|q| meta.modified().is_ok_and(|m| m <= q))
                {
                    cursor.release();
                }
            }
            fresh.sort_by_key(|(_, meta)| std::cmp::Reverse(meta.modified().ok()));
            for (path, _) in fresh {
                if self.subagents.len() >= self.cap {
                    unread += 1;
                    continue;
                }
                if left == 0 {
                    // Not opened, so not admitted: nothing of it is in the
                    // totals, and the next read with budget finds it new.
                    caught_up = false;
                    continue;
                }
                let mut cursor = TranscriptCursor::new(&path);
                if !advance_subagent(&mut cursor, &mut left, &mut caught_up, &mut unread) {
                    continue;
                }
                self.subagents.insert(path, cursor);
            }
        }
        self.progressed = left < budget;
        self.caught_up = caught_up;
        Refresh {
            caught_up,
            unread,
            walk_cut,
            spent: budget - left,
        }
    }

    /// Subagent transcripts in the totals, whole: followed, readable at the
    /// last read, and not rewritten after their ids were released.
    pub fn subagent_files(&self) -> usize {
        self.subagents
            .values()
            .filter(|c| c.readable && !c.rewritten)
            .count()
    }

    /// Every byte read from disk over the session's life, all files.
    #[cfg(test)]
    pub(crate) fn bytes_read(&self) -> u64 {
        std::iter::once(&self.main)
            .chain(self.subagents.values())
            .map(TranscriptCursor::bytes_read)
            .sum()
    }

    /// Unix seconds of the newest response the API served in any of the
    /// session's files ([`TranscriptUsage::served_at`]); the limit is the
    /// account's, so a subagent's response counts as much as the main thread's.
    #[must_use]
    pub fn served_at(&self) -> Option<i64> {
        std::iter::once(&self.main)
            .chain(self.subagents.values())
            .filter_map(|cursor| cursor.fold().served_at())
            .max()
    }

    /// Milliseconds Claude was working in this conversation: its MAIN
    /// transcript's finished turns ([`TranscriptUsage::busy_ms`]). Subagent
    /// files are not read for it: their turns run inside the main chain's
    /// (MEASURED on two long sessions, 0.00h and 0.13h outside them).
    #[must_use]
    pub fn busy_ms(&self) -> u64 {
        self.main.fold().busy_ms()
    }

    /// The session's fold: every file's spend per model and its row counters,
    /// summed — each file's own `message.id` dedupe applied first, bounded at
    /// [`super::usage::MAX_MODELS`] models like any fold. The one sum the
    /// footer and `harness usage` both read.
    pub fn total_fold(&self) -> TranscriptUsage {
        let mut all = TranscriptUsage::new();
        for cursor in std::iter::once(&self.main).chain(self.subagents.values()) {
            all.absorb(cursor.fold());
        }
        all
    }
}

/// What the subagent walk found.
struct Listing {
    /// Every `agent-*.jsonl` regular file, with what `lstat` said of it.
    files: Vec<(PathBuf, std::fs::Metadata)>,
    /// Every directory looked at, with what `stat` said of it — the walk's
    /// root among them even when it is not there (`None`).
    dirs: Vec<(PathBuf, Option<Stamp>)>,
    /// The walk stopped at its entry bound.
    cut: bool,
}

/// The `agent-*.jsonl` files under `dir`, at most [`MAX_SUBAGENT_DEPTH`]
/// levels down, each with its metadata, and the directories looked at.
/// Symbolic links are not followed, and the walk looks at no more than
/// `max_entries` entries — and says so when it stops there. A missing
/// directory holds none.
fn list_subagents(dir: &Path, max_entries: usize) -> Listing {
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    let mut seen = 0usize;
    let mut stack = vec![(dir.to_path_buf(), 0usize)];
    while let Some((here, depth)) = stack.pop() {
        dirs.push((here.clone(), stamp_of(&here)));
        let Ok(entries) = std::fs::read_dir(&here) else {
            continue;
        };
        for entry in entries.flatten() {
            seen += 1;
            if seen > max_entries {
                return Listing {
                    files,
                    dirs,
                    cut: true,
                };
            }
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            let path = entry.path();
            if meta.is_dir() {
                if depth + 1 < MAX_SUBAGENT_DEPTH {
                    stack.push((path, depth + 1));
                }
            } else if meta.is_file()
                && entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with("agent-") && name.ends_with(".jsonl"))
            {
                files.push((path, meta));
            }
        }
    }
    Listing {
        files,
        dirs,
        cut: false,
    }
}

// ---------------------------------------------------------------------------
// The limit wall, from the transcript
// ---------------------------------------------------------------------------

/// The limit wall the footer shows: the window of the newest limit notice
/// written into this session's transcript, and its reset.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Wall {
    /// `5h` or `7d`.
    pub label: &'static str,
    /// The reset as the notice printed it — `3pm`, `Oct 1 at 9am` — its
    /// `(zone)` left off only where that zone's clock is the local one at
    /// the reset (`10pm (UTC)` stays so on a Pacific machine); a span
    /// (`in 3h`, counted from the notice) as the local clock time it names.
    pub resets: String,
}

/// The wall `rows` (the newest limit-notice row per window, from the main
/// transcript's fold) shows at `now`: the newest row's, while its reset is
/// ahead and nothing has been served since.
///
/// The reset is placed from the row's own time by
/// `supervise::limit::reset_in_zone`, with `offset_at(zone, t)` giving the
/// zone's offset (`None`: local); the five-hour window's bare clock time is at
/// most five hours ahead, any other window's is its next occurrence. A reset
/// that cannot be placed shows nothing. A `served_at` after the row hides it.
#[must_use]
pub fn wall_of(
    rows: &BTreeMap<String, LimitNoticeRow>,
    served_at: Option<i64>,
    now: i64,
    offset_at: &dyn Fn(Option<&str>, i64) -> Option<i64>,
) -> Option<Wall> {
    use crate::supervise::limit::{self, ResetSpec};
    let (key, row) = rows.iter().max_by_key(|(_, row)| row.at)?;
    if served_at.is_some_and(|served| served > row.at) {
        return None;
    }
    let (label, ahead_max) = match key.as_str() {
        "five_hour" => ("5h", limit::SESSION_AHEAD_MAX),
        "seven_day" => ("7d", i64::MAX),
        _ => return None,
    };
    let text = row.reset_text.as_deref()?.trim();
    let spec = limit::parse_reset(text)?;
    let zone = match &spec {
        ResetSpec::At { zone, .. } => zone.as_deref(),
        ResetSpec::In(_) => None,
    };
    let resets_at = limit::reset_in_zone(&spec, row.at, ahead_max, &|t| offset_at(zone, t))?;
    if resets_at <= now {
        return None;
    }
    let local = offset_at(None, resets_at);
    let resets = match zone {
        _ if matches!(spec, ResetSpec::In(_)) => local_clock(resets_at, local?, now),
        Some(zone) if local.is_none() || offset_at(Some(zone), resets_at) != local => {
            text.to_owned()
        }
        _ => text.split(" (").next().unwrap_or(text).trim().to_owned(),
    };
    Some(Wall { label, resets })
}

/// `unix` on the local clock (`offset` east of UTC), as the vendor prints a
/// reset: `3pm`, `3:30pm`, and the date first when it is a day or more past
/// `now` (`Oct 1 at 9am`).
fn local_clock(unix: i64, offset: i64, now: i64) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let local = unix + offset;
    let secs = local.rem_euclid(86_400);
    let (hour, minute) = (secs / 3600, secs % 3600 / 60);
    let twelve = if hour % 12 == 0 { 12 } else { hour % 12 };
    let half = if hour < 12 { "am" } else { "pm" };
    let clock = if minute == 0 {
        format!("{twelve}{half}")
    } else {
        format!("{twelve}:{minute:02}{half}")
    };
    if unix - now < 86_400 {
        return clock;
    }
    let (_, month, day) = aterm_types::rfc3339::civil_from_days(local.div_euclid(86_400));
    let name = MONTHS
        .get(usize::try_from(month).unwrap_or(1).saturating_sub(1))
        .unwrap_or(&"?");
    format!("{name} {day} at {clock}")
}

// ---------------------------------------------------------------------------
// The usage facts: the footer's merged `Σ`, `aterm harness usage`'s per model
// ---------------------------------------------------------------------------

/// One model's tokens in this session, as `aterm harness usage` names them.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ModelTokens {
    /// The model's short name (`opus`; `opus5.5` beside an `opus5`).
    pub label: String,
    /// Every input token the model read: fresh input, cache writes and cache
    /// reads.
    pub input: u64,
    /// Output tokens.
    pub output: u64,
}

/// The models past [`NAMED_MODELS`]: how many, and their tokens — counted
/// (`+2`) where the models are named ([`UsageGrain::PerModel`]), summed in
/// where they are merged ([`UsageGrain::Total`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct More {
    /// How many models.
    pub models: usize,
    /// Their input tokens, as [`ModelTokens::input`] counts them.
    pub input: u64,
    /// Their output tokens.
    pub output: u64,
}

/// What a `Σ` says: the footer's segment, `aterm harness usage`'s line.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct UsageFacts {
    /// This session's tokens per model, most first, at most
    /// [`NAMED_MODELS`] of them. LIVE: folded from its transcripts.
    pub models: Vec<ModelTokens>,
    /// The models past [`NAMED_MODELS`].
    pub more: More,
    /// Subagent transcripts NOT in these totals ([`Refresh::unread`]).
    pub unread: usize,
    /// More files may exist than were seen ([`Refresh::walk_cut`]).
    pub unread_more: bool,
    /// The limit wall still standing, from the transcript ([`wall_of`]).
    pub wall: Option<Wall>,
    /// Seconds Claude was WORKING in this conversation: the union of its main
    /// transcript's finished turns ([`TranscriptUsage::busy_ms`]); `None`
    /// before a turn ends, or before the fold caught up ([`Self::with_runtime`]).
    pub runtime: Option<u64>,
}

impl UsageFacts {
    /// The facts — the tokens, the wall, and what `left_out` (the refresh
    /// that folded them) left out of the totals, said whether or not any
    /// tokens are shown yet — or `None` when there is nothing to show. No
    /// runtime: [`Self::with_runtime`] adds it.
    #[must_use]
    pub fn of(
        models: (Vec<ModelTokens>, More),
        wall: Option<Wall>,
        left_out: Option<&Refresh>,
    ) -> Option<Self> {
        let (models, more) = models;
        let (unread, unread_more) = left_out.map_or((0, false), |r| (r.unread, r.walk_cut));
        (!models.is_empty() || wall.is_some() || unread > 0 || unread_more).then_some(Self {
            models,
            more,
            unread,
            unread_more,
            wall,
            runtime: None,
        })
    }

    /// These facts with Claude's working time: `busy_ms` (the main
    /// transcript's [`SessionUsage::busy_ms`]) in whole seconds, never
    /// rounded up; none for `0` (no turn has ended).
    #[must_use]
    pub fn with_runtime(mut self, busy_ms: u64) -> Self {
        self.runtime = (busy_ms >= 1000).then_some(busy_ms / 1000);
        self
    }

    /// Whether these facts carry anything of the `Σ` segment's — what
    /// [`Self::wall_only`] takes out.
    #[must_use]
    pub fn says_usage(&self) -> bool {
        !self.models.is_empty()
            || self.more.models > 0
            || self.unread > 0
            || self.unread_more
            || self.runtime.is_some()
    }

    /// These facts with the `Σ` segment's part taken out — no tokens, no
    /// unread count — and only the limit wall kept: what the footer shows
    /// with `[claude_footer] usage = false`. `None` when no wall stands. The
    /// fold behind them keeps running: the wall is read from it.
    #[must_use]
    pub fn wall_only(self) -> Option<Self> {
        if !self.says_usage() {
            return Some(self);
        }
        Self::of((Vec::new(), More::default()), self.wall, None)
    }
}

/// A model id's short name in `aterm harness usage`: its family (`claude-opus-5-5` → `opus`), or,
/// when `family_shared`, the family with its version (`opus5.5`). Letters,
/// digits and `.` only, at most 16 of them — a transcript is text another
/// program wrote, and this ends up in a chrome row.
fn model_label(id: &str, family_shared: bool) -> String {
    let family = model_short(id);
    let raw = if family_shared {
        super::footer::model_display(id).replace(' ', "")
    } else {
        family
    };
    let label: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '.')
        .take(16)
        .collect::<String>()
        .to_ascii_lowercase();
    if label.is_empty() {
        "model".to_owned()
    } else {
        label
    }
}

/// A model id as the API names the model, whatever route served it: a
/// Bedrock id (`us.anthropic.claude-opus-4-1-20250805-v1:0`, or an ARN
/// ending in one) loses everything up to `anthropic.` and its `-v1:0`
/// revision, a Vertex id (`claude-opus-4-1@20250805`) its `@` date. Any
/// other id is itself.
fn model_of_route(id: &str) -> &str {
    const PROVIDER: &str = "anthropic.";
    let id = id
        .rfind(PROVIDER)
        .map_or(id, |at| &id[at + PROVIDER.len()..]);
    let id = id.split('@').next().unwrap_or(id);
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    match id.rfind("-v") {
        Some(at)
            if id[at + 2..]
                .split_once(':')
                .is_some_and(|(major, minor)| digits(major) && digits(minor)) =>
        {
            &id[..at]
        }
        _ => id,
    }
}

/// The session's spend per model, what every [`UsageGrain`] reads (the
/// footer's merges it): models with no tokens dropped, one label per family
/// (the version added only where two models share a family; ids that still
/// share a label — one model reached by two routes, [`model_of_route`] —
/// are summed), most tokens first, the first [`NAMED_MODELS`] named and the
/// rest gathered in [`More`].
#[must_use]
pub fn model_tokens(per_model: &BTreeMap<String, ModelSpend>) -> (Vec<ModelTokens>, More) {
    let live: Vec<(&str, &ModelSpend)> = per_model
        .iter()
        .filter(|(_, s)| (s.input | s.output | s.cache_write | s.cache_read) != 0)
        .map(|(id, s)| (model_of_route(id), s))
        .collect();
    let families: Vec<String> = live.iter().map(|(id, _)| model_short(id)).collect();
    let mut by_label: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    for (i, (id, s)) in live.iter().enumerate() {
        let shared = families
            .iter()
            .zip(&live)
            .any(|(f, (other, _))| other != id && *f == families[i]);
        let input = s
            .input
            .saturating_add(s.cache_write)
            .saturating_add(s.cache_read);
        let entry = by_label.entry(model_label(id, shared)).or_default();
        entry.0 = entry.0.saturating_add(input);
        entry.1 = entry.1.saturating_add(s.output);
    }
    let mut all: Vec<ModelTokens> = by_label
        .into_iter()
        .map(|(label, (input, output))| ModelTokens {
            label,
            input,
            output,
        })
        .collect();
    all.sort_by(|a, b| {
        (b.input.saturating_add(b.output))
            .cmp(&a.input.saturating_add(a.output))
            .then_with(|| a.label.cmp(&b.label))
    });
    let rest = all.split_off(NAMED_MODELS.min(all.len()));
    let more = rest.iter().fold(More::default(), |more, m| More {
        models: more.models + 1,
        input: more.input.saturating_add(m.input),
        output: more.output.saturating_add(m.output),
    });
    (all, more)
}

/// A token count at the footer's grain, never rounded UP: `940`, `1.2k`,
/// `40k`, `310k`, `1.2M`, `48M`, `3.1B`.
#[must_use]
pub fn tokens_short(n: u64) -> String {
    const UNITS: [(u64, &str); 3] = [(1_000_000_000, "B"), (1_000_000, "M"), (1_000, "k")];
    for (unit, suffix) in UNITS {
        if n >= unit {
            let whole = n / unit;
            return if whole < 10 {
                format!("{whole}.{}{suffix}", (n % unit) / (unit / 10))
            } else {
                format!("{whole}{suffix}")
            };
        }
    }
    n.to_string()
}

/// How much of the usage a `Σ` line says ([`usage_text_at`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageGrain {
    /// Runtime, then each named model's tokens (the name only where two or
    /// more models spent), the models past them counted, and what was left
    /// out of the totals: `aterm harness usage`'s `Σ` line ([`usage_text`]).
    PerModel,
    /// Runtime and every model's tokens merged into one pair, no names; what
    /// was left out still said: the footer's `Σ` segment (owner, 2026-09-30:
    /// "I just can't have so many different model stats").
    Total,
    /// Runtime alone: the footer's `Σ` where the rule is too narrow for the
    /// total.
    Runtime,
}

/// A working time at the footer's grain, never rounded up: `42s`, `12m03s`,
/// `7h52m`, `130h` — the seconds dropped past an hour, the minutes past 100
/// hours. Every form carries an `s` or an `h`, so none reads as a token
/// count (`12m` beside `12M`) where the `Σ` segment narrows to the runtime
/// alone, with no arrow to mark tokens.
#[must_use]
pub fn runtime_short(secs: u64) -> String {
    let (h, m) = (secs / 3600, secs % 3600 / 60);
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{m}m{:02}s", secs % 60),
        _ if h < 100 => format!("{h}h{m:02}m"),
        _ => format!("{h}h"),
    }
}

/// `aterm harness usage`'s `Σ` line, per model ([`UsageGrain::PerModel`]).
/// `None` when there is nothing to say.
#[must_use]
pub fn usage_text(facts: &UsageFacts) -> Option<String> {
    usage_text_at(facts, UsageGrain::PerModel)
}

/// The usage text (after its `Σ` mark) at `grain`: `<runtime> ↑<in> ↓<out>[
/// +N unread]` merged — `7h52m ↑395M ↓2.0M`, the footer's — or per model,
/// `<runtime> [<model> ]↑<in> ↓<out>[ · …][ +N][ +N unread]` — `18h46m opus
/// ↑395M ↓2.0M · haiku ↑12M ↓88k +1` (↑ in, ↓ out: Claude Code's own spinner
/// arrows; input counts cache reads and writes). Files left out of the
/// totals are said after them (`+3 unread`; `+3+ unread`, or `+? unread`
/// alone, when the walk stopped short). The limit wall is NOT here: it is
/// its own segment ([`wall_text`]), placed where a narrow row keeps it
/// longest. `None` when the grain has nothing to say.
#[must_use]
pub fn usage_text_at(facts: &UsageFacts, grain: UsageGrain) -> Option<String> {
    let pair = |input: u64, output: u64| {
        format!(
            "\u{2191}{} \u{2193}{}",
            tokens_short(input),
            tokens_short(output)
        )
    };
    let mut parts: Vec<String> = facts.runtime.map(runtime_short).into_iter().collect();
    if grain == UsageGrain::Runtime {
        return parts.pop();
    }
    let named = facts.models.len() + facts.more.models > 1;
    let tokens = match grain {
        UsageGrain::PerModel => {
            let models: Vec<String> = facts
                .models
                .iter()
                .map(|m| {
                    if named {
                        format!("{} {}", m.label, pair(m.input, m.output))
                    } else {
                        pair(m.input, m.output)
                    }
                })
                .collect();
            let mut text = models.join(" \u{00B7} ");
            if facts.more.models > 0 {
                text.push_str(&format!(" +{}", facts.more.models));
            }
            text
        }
        _ if facts.models.is_empty() => String::new(),
        _ => {
            let (input, output) = facts
                .models
                .iter()
                .fold((facts.more.input, facts.more.output), |(i, o), m| {
                    (i.saturating_add(m.input), o.saturating_add(m.output))
                });
            pair(input, output)
        }
    };
    parts.extend((!tokens.is_empty()).then_some(tokens));
    match (facts.unread, facts.unread_more) {
        (0, false) => {}
        // The walk stopped short with nothing else left out: how many were
        // not seen is unknown, and a `0` would read as none.
        (0, true) => parts.push("+? unread".to_owned()),
        (n, more) => parts.push(format!("+{n}{} unread", if more { "+" } else { "" })),
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

/// The limit wall's segment text (after its `⧗` mark, which says "limit"):
/// the window, then [`super::footer::RESET_MARK`] for "resets" and the reset
/// as the notice printed it — `5h ♻ 3pm`, `7d ♻ Oct 1 at 9am`, `5h ♻ 10pm
/// (UTC)`.
#[must_use]
pub fn wall_text(wall: &Wall) -> String {
    format!(
        "{} {} {}",
        wall.label,
        super::footer::RESET_MARK,
        wall.resets
    )
}

#[cfg(test)]
#[path = "session_usage_tests.rs"]
mod tests;
