// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THIS SESSION'S USAGE (owner function 2, 2026-09-25): what the Claude Code
//! footer's `Σ` segment and its limit wall show ([`super::footer`]) and what
//! `aterm harness usage` prints ([`super::cli`]) — ONE implementation for
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
//! * **THE LIMIT WALL — the one account figure.** Claude Code writes a limit
//!   it hit into the transcript as a row of its own (`isApiErrorMessage`,
//!   `You've hit your session limit · resets 3pm (…)`, its own `timestamp`;
//!   [`super::usage::LimitNoticeRow`]). The newest such row in the MAIN
//!   transcript, its reset placed from the row's time in real time
//!   ([`wall_of`]), is shown while that reset is ahead and nothing has been
//!   served since — `5h limit · resets 3pm` — and not at all once it has
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
//! fixture carries a limit-notice transcript row yet.

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

/// The most subagent transcripts one session's fold follows. Every file
/// followed keeps a cursor for the fold's life; past this bound every NEW
/// file — the running subagents included — is left and counted
/// ([`Refresh::unread`]). Within the one read that crosses it, the newest
/// are admitted first.
///
/// THE CEILING, per session. A cursor at rest was MEASURED 2026-09-26 with a
/// counting allocator (release, 16 messages per file, realistic paths) at
/// about 5.4 KB while its file may still grow (its [`FINISHED_IDS_KEPT`]
/// newest ids kept) and about 2.6 KB once it has been still for
/// [`QUIET_AFTER`] (ids released). So, by arithmetic, at most about 11 MB
/// per session with every followed file recent, about 5.3 MB once they are
/// all quiet — one such fold per session the window's footer resolver
/// WATCHES, released when the session leaves that watch (ten minutes after
/// it was last asked for, unless a limit wall keeps it) or its tab closes;
/// no other bound caps how many are held. MEASURED 2026-09-25: one real
/// session had 551 subagent files, another 399.
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

/// The models the footer names; the rest are counted (`+2`).
pub const FOOTER_MODELS: usize = 3;

/// The message ids a FINISHED subagent file keeps until it has been quiet
/// [`QUIET_AFTER`]: the newest few, so a message still streaming when the
/// file paused is raised, not counted again, if it grows once more.
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
    /// Whether the file was found changed other than by an append AFTER its
    /// message ids were released ([`TranscriptCursor::release`]): nothing
    /// was read or restarted — its totals stand, and it is unread.
    pub rewritten: bool,
}

/// One transcript file folded INCREMENTALLY: the byte offset reached, the
/// running [`TranscriptUsage`], and the torn last line still waiting for its
/// newline. Each [`Self::advance`] reads only what was appended since the
/// last one.
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
    /// up to its newline, and it is counted as skipped, as
    /// [`TranscriptUsage::fold_reader`] does.
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

    /// Release a FINISHED file's message ids: its totals are all a view
    /// needs. From here only a plain append (same inode, from the size
    /// reached, the boundary byte in place) is read on; any other change
    /// cannot be told apart from what was summed, and is `rewritten` —
    /// counted unread, never re-read from its first byte. The BOUND, stated:
    /// a rewrite in place that leaves the file at least as long and the
    /// byte the fold stopped after where it was cannot be seen at all, by
    /// this rule or the boundary check before it.
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

    /// Read what the file has appended since the last call, at most `budget`
    /// bytes of it, and fold every line it completes.
    ///
    /// A file whose size, mtime and inode are what the last read left, read
    /// to its end, is not opened at all (one `stat`). Otherwise only a
    /// REGULAR file is read, opened non-blocking (a FIFO planted where a
    /// transcript should be cannot park the caller). The fold starts over
    /// when the file is shorter than the offset reached (truncated), is a
    /// different file (rotated: another inode at the path), or no longer
    /// holds the byte the fold stopped after (rewritten in place — the one
    /// byte of that check is read, and counted, on every read that opens the
    /// file past its start) — unless its ids were RELEASED, when such a change
    /// is `rewritten` instead: nothing read, nothing restarted. What is held
    /// between reads is the fold and the bytes of a torn last line — no line
    /// buffer.
    ///
    /// # Errors
    ///
    /// The file cannot be opened or read; what was folded before the error
    /// is kept, and the offset is exactly what was consumed.
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
    /// Subagent transcripts that exist with rows NOT in the totals, each
    /// counted once: not readable now; new past [`MAX_SUBAGENT_FILES`]
    /// followed; or changed other than by an append after its ids were
    /// released (its earlier totals stand; what it holds now is not read).
    pub unread: usize,
    /// The subagent walk stopped at its entry bound: more files may exist
    /// than were seen, and none of those is counted.
    pub walk_cut: bool,
    /// How much of the budget this refresh used: all of it when the budget,
    /// not the files, ended the read.
    pub spent: u64,
}

impl Refresh {
    /// Whether the totals are the WHOLE session's: caught up, nothing
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

    /// Whether a refresh now could find anything the last one did not: the
    /// last one did not catch up and did read something (a fold that could
    /// not move on is left to a change or the slower clock); the transcript
    /// moved (size, mtime or inode); a directory the walk visited changed —
    /// a new subagent file, at ANY depth (`subagents/workflows/<run>/`), or a
    /// new run's directory; or any followed subagent file changed since its
    /// last read — EVERY one, not the most recently written few: an agent in
    /// a long tool call is the least recently written, and its final message
    /// is what would be missed. `stat`s only, one per followed file (at most
    /// [`MAX_SUBAGENT_FILES`]; a frozen, rewritten file is not asked again,
    /// and one no longer there has nothing to read): nothing is opened and
    /// nothing is walked.
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

    /// Read what every file of the session appended since the last call —
    /// the transcript first, then each subagent transcript — at most
    /// `budget` bytes over all of them. A file whose size, mtime and inode
    /// have not moved is not opened, and once the budget is spent no file is
    /// opened at all; one walk of the subagent directory, one `lstat` per
    /// entry.
    ///
    /// A subagent file seen for the first time is admitted — newest first
    /// among this read's new files — while fewer than [`MAX_SUBAGENT_FILES`]
    /// are followed, and only if it can be read now; a followed file is never
    /// dropped. What is not in the totals is [`Refresh::unread`]: a file past
    /// the bound (from then on every new one), one that cannot be read, one
    /// rewritten after its ids were released. A file still new when the
    /// budget ran out is not admitted, and the read is not caught up.
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

    /// Unix seconds of the newest response the API served in ANY of the
    /// session's files ([`TranscriptUsage::served_at`]): the limit is the
    /// account's, so a background workflow's subagent served after a notice
    /// in the main transcript is as much a response served since as the main
    /// thread's own.
    #[must_use]
    pub fn served_at(&self) -> Option<i64> {
        std::iter::once(&self.main)
            .chain(self.subagents.values())
            .filter_map(|cursor| cursor.fold().served_at())
            .max()
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
    /// `(zone)` left off only where that zone's clock IS the local one at
    /// the reset (`10pm (UTC)` stays so on a Pacific machine); a span
    /// (`in 3h`, counted from the notice) as the local clock time it names.
    pub resets: String,
}

/// The wall `rows` (the newest limit-notice row per window, from the MAIN
/// transcript's fold) shows at `now`: the NEWEST row's, while its reset is
/// still ahead and nothing has been served since.
///
/// * PLACED from the row's own time by the one reset parser
///   (`supervise::limit::reset_in_zone`) in REAL time: `offset_at(zone, t)`
///   is the notice's zone's offset at the instant `t` (`None`: the local
///   zone's), so a daylight-saving jump between the notice and its reset
///   lands where the zone's clock shows it. The five-hour window's bare
///   clock time is at most five real hours ahead; any other window's is its
///   NEXT occurrence — the vendor prints no date only when the reset is
///   under a day away (MEASURED, 2.1.283's time formatter prints the date
///   past 24 hours), so a weekly `9am` written at 14:00 is the next day's.
///   A reset that cannot be placed — its zone unknown here, an offset that
///   cannot be read — shows nothing: never a guess.
/// * SERVED SINCE: `served_at` ([`TranscriptUsage::served_at`]) after the
///   row is a response the API served after the wall — `/limit-reset`,
///   extra usage, another account — so the wall no longer blocks and is not
///   shown. It can only hide a wall, never show one.
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
// The footer's facts and its one segment
// ---------------------------------------------------------------------------

/// One model's tokens in this session, as the footer names them.
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

/// What the footer's `Σ` segment says.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct UsageFacts {
    /// This session's tokens per model, most first, at most
    /// [`FOOTER_MODELS`] of them. LIVE: folded from its transcripts.
    pub models: Vec<ModelTokens>,
    /// Models past [`FOOTER_MODELS`], counted.
    pub more_models: usize,
    /// Subagent transcripts NOT in these totals ([`Refresh::unread`]).
    pub unread: usize,
    /// More files may exist than were seen ([`Refresh::walk_cut`]).
    pub unread_more: bool,
    /// The limit wall still standing, from the transcript ([`wall_of`]).
    pub wall: Option<Wall>,
}

impl UsageFacts {
    /// The facts — the tokens, the wall, and what `left_out` (the refresh
    /// that folded them) left out of the totals, said whether or not any
    /// tokens are shown yet — or `None` when there is nothing to show.
    #[must_use]
    pub fn of(
        models: (Vec<ModelTokens>, usize),
        wall: Option<Wall>,
        left_out: Option<&Refresh>,
    ) -> Option<Self> {
        let (models, more_models) = models;
        let (unread, unread_more) = left_out.map_or((0, false), |r| (r.unread, r.walk_cut));
        (!models.is_empty() || wall.is_some() || unread > 0 || unread_more).then_some(Self {
            models,
            more_models,
            unread,
            unread_more,
            wall,
        })
    }
}

/// A model id's footer name: its family (`claude-opus-5-5` → `opus`), or,
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

/// The session's spend as the footer's per-model tokens: models with no
/// tokens dropped, one label per family (the version added only where two
/// models share a family; ids that still share a label — one model reached
/// by two routes, [`model_of_route`] — are summed), most tokens first, the
/// first [`FOOTER_MODELS`] named and the rest counted.
#[must_use]
pub fn model_tokens(per_model: &BTreeMap<String, ModelSpend>) -> (Vec<ModelTokens>, usize) {
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
    let more = all.len().saturating_sub(FOOTER_MODELS);
    all.truncate(FOOTER_MODELS);
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

/// The footer's usage segment text (after its `Σ` mark):
/// `opus 48M in 310k out · haiku 1.1M in 20k out · +2`. Files left out of
/// the totals are said after the models (`+3 unread`; `+3+ unread`, or `+?
/// unread` alone, when the walk stopped short). The limit wall is NOT here:
/// it is its own segment ([`wall_text`]), placed where a narrow row keeps it
/// longest. `None` when there is nothing to say.
#[must_use]
pub fn usage_text(facts: &UsageFacts) -> Option<String> {
    let mut parts: Vec<String> = facts
        .models
        .iter()
        .map(|m| {
            format!(
                "{} {} in {} out",
                m.label,
                tokens_short(m.input),
                tokens_short(m.output)
            )
        })
        .collect();
    if facts.more_models > 0 {
        parts.push(format!("+{}", facts.more_models));
    }
    match (facts.unread, facts.unread_more) {
        (0, false) => {}
        // The walk stopped short with nothing else left out: how many were
        // not seen is unknown, and a `0` would read as none.
        (0, true) => parts.push("+? unread".to_owned()),
        (n, more) => parts.push(format!("+{n}{} unread", if more { "+" } else { "" })),
    }
    (!parts.is_empty()).then(|| parts.join(" \u{00B7} "))
}

/// The limit wall's segment text (after its mark): `5h limit · resets 3pm`.
#[must_use]
pub fn wall_text(wall: &Wall) -> String {
    format!("{} limit \u{00B7} resets {}", wall.label, wall.resets)
}

#[cfg(test)]
#[path = "session_usage_tests.rs"]
mod tests;
