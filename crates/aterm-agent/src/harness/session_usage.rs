// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! This session's usage: the Claude Code footer's `Σ` segment and limit wall ([`super::footer`])
//! and `aterm harness usage`'s per-model rows ([`super::cli`]), read from the session's own files,
//! never off the screen. The footer keeps one [`SessionUsage`] per watched session
//! (`footer::FooterCache`); model and effort come from its per-process transcript tail
//! (`footer::TailCache`), while the tokens here are the conversation's, a resumed conversation's
//! earlier processes included.
//!
//! * **Tokens**, per model, from `<project>/<session>.jsonl` and the subagent transcripts beside it
//!   (`<project>/<session>/subagents/…/agent-*.jsonl`; the main transcript carries none of a
//!   subagent's turns). Each file is folded incrementally ([`TranscriptCursor`]). Each `message.id`
//!   counts once, at the largest counts its rows carry (a streamed message's rows grow), and a line
//!   is folded only once its newline is written. Files that cannot be counted are said
//!   ([`Refresh::unread`]), never silently left out of a total.
//! * **Runtime**, the time Claude was working: the union of the main transcript's finished turns,
//!   each `[timestamp − durationMs, timestamp]` of its `turn_duration` row
//!   ([`SessionUsage::busy_ms`]). Not the durations' sum (a workflow's turn overlaps the turns
//!   after it), not the first-to-last span (mostly idle), not the `cost-state` API time (written
//!   only at exit). It undercounts: no row is written for a turn interrupted with Esc or in flight
//!   at exit, and a turn's paused time (a permission dialog) is not in its `durationMs`.
//! * **The limit wall**: the newest limit-notice row in the main transcript
//!   ([`super::usage::LimitNoticeRow`]), its reset placed from the row's own time ([`wall_of`]),
//!   shown while that reset is ahead and nothing has been served since in any of the session's
//!   files ([`SessionUsage::served_at`]: the limit is the account's). The band's `limited → 3pm`
//!   reads the screen's notice instead, so the two can differ.
//!
//! Nothing here writes anything or installs anything into the agent: no hook, status line or
//! plugin. The one exception is [`super::tap`]'s opt-in `--settings` status line, written to no
//! file. The notice row, the subagent layout and the streamed usage growth are as Claude Code
//! 2.1.283 writes them, the `turn_duration` row as 2.1.282 to 2.1.286 do; no vendor-corpus fixture
//! carries a limit-notice row yet.

use std::collections::BTreeMap;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::flight::{FileFlight, FlightFold, FlightTable, Flyer};
use super::steps::StepFold;
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

/// A subagent file unchanged this long (by its mtime) releases its message ids
/// ([`TranscriptCursor::release`]): only its totals are kept, a later append is read on, and any
/// other change is counted unread.
pub const QUIET_AFTER: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// The most directory entries the subagent walk looks at, over all levels
/// (the 551-file session used 1,111). A walk cut short makes the fold
/// incomplete ([`Refresh::walk_cut`]).
const MAX_SUBAGENT_ENTRIES: usize = 16_384;

/// How deep under `<session>/subagents/` the walk goes: the vendor nests a
/// subagent's transcript under a subdirectory (`agentTranscriptSubdirs`) and
/// a workflow's under `workflows/<run>/`.
const MAX_SUBAGENT_DEPTH: usize = 4;

/// How recently a directory may have changed, at the walk that stamped it, for
/// its stamp to be RACY ([`DirSeen::racy`]): an entry made in the same tick of
/// the file system's clock as the walk leaves the directory's stamp as the walk
/// recorded it. Linux stamps a directory from its coarse clock (one jiffy, up
/// to 10 ms; ext4 with small inodes and HFS+, a whole second), and there an
/// ext4 directory's size and link count do not move for a new file either:
/// measured on the Linux lane (Ubuntu 24.04, kernel 6.8), a subagent file made
/// just after a refresh left `moved` false. Two seconds covers every one of
/// those clocks (git's "racily clean" rule is the same idea).
const RACY_DIR: std::time::Duration = std::time::Duration::from_secs(2);

/// The models `aterm harness usage` names; the rest are counted (`+2`).
/// The footer names none: its `Σ` sums every model ([`UsageGrain::Total`]).
pub const NAMED_MODELS: usize = 3;

/// The message ids a finished subagent file keeps until quiet for
/// [`QUIET_AFTER`]: the newest few, so a message still streaming when the file
/// paused is raised, not counted again, if it grows.
pub const FINISHED_IDS_KEPT: usize = 16;

// One transcript, folded incrementally

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

impl FileId {
    /// As the step fold keys a file ([`StepFold::read_at`]).
    const fn pair(self) -> (u64, u64) {
        (self.dev, self.ino)
    }
}

/// A transcript path's fingerprint, as the step fold keys a file
/// (FNV-1a/64 of its bytes).
pub(crate) fn path_fp(path: &Path) -> u64 {
    aterm_hash::fnv1a_64(path.as_os_str().as_encoded_bytes())
}

/// The identity of the open `file` whose metadata is `meta`, `(device, file
/// number)`: what tells a file replaced at its path (a rotation, a rename over
/// it) from the one a fold read. Unix: `meta`'s device and inode. Windows
/// keeps a file's identity, its volume serial and file index, on the open
/// handle alone (std reads them only behind an unstable feature), so it is
/// asked of `file`; without it a file renamed over the one read, no shorter
/// and with the same byte before the fold's offset, was read on as an append.
pub(crate) fn open_identity(file: &std::fs::File, meta: &std::fs::Metadata) -> Option<(u64, u64)> {
    #[cfg(windows)]
    let id = {
        let _ = meta;
        handle_id(file)
    };
    #[cfg(not(windows))]
    let id = {
        let _ = file;
        file_id(meta)
    };
    id.map(|FileId { dev, ino }| (dev, ino))
}

/// `GetFileInformationByHandle`'s volume serial and file index of `file`.
#[cfg(windows)]
fn handle_id(file: &std::fs::File) -> Option<FileId> {
    use std::os::windows::io::AsRawHandle as _;
    /// `BY_HANDLE_FILE_INFORMATION` (fileapi.h); each `FILETIME` is two
    /// `DWORD`s.
    #[repr(C)]
    #[derive(Default)]
    struct Info {
        attributes: u32,
        created: [u32; 2],
        accessed: [u32; 2],
        written: [u32; 2],
        volume_serial: u32,
        size_high: u32,
        size_low: u32,
        links: u32,
        index_high: u32,
        index_low: u32,
    }
    unsafe extern "system" {
        fn GetFileInformationByHandle(file: *mut core::ffi::c_void, info: *mut Info) -> i32;
    }
    let mut info = Info::default();
    // SAFETY: the handle is `file`'s own, open for the whole call, and `info`
    // is a writable structure of the layout the call fills.
    let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), &raw mut info) };
    (ok != 0).then(|| FileId {
        dev: u64::from(info.volume_serial),
        ino: (u64::from(info.index_high) << 32) | u64::from(info.index_low),
    })
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
    /// Its message ids were released ([`Self::release`]): only an append can be read on; any
    /// other change leaves it `rewritten`.
    released: bool,
    /// Found rewritten after its ids were released: frozen, its totals
    /// standing, counted unread from then on.
    rewritten: bool,
    /// A subagent's calls in flight ([`super::flight`]), fed the same lines
    /// as `fold`; `None` for the main transcript.
    flight: Option<FlightFold>,
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
            flight: None,
        }
    }

    /// A subagent's cursor: [`Self::new`], its calls in flight folded too.
    fn subagent(path: impl Into<PathBuf>) -> Self {
        Self {
            flight: Some(FlightFold::new()),
            ..Self::new(path)
        }
    }

    /// What its calls in flight say ([`FileFlight::Unknown`] for the main
    /// transcript, a file that could not be read at its last look, or one
    /// found rewritten after its ids were released).
    fn file_flight(&self) -> FileFlight {
        match &self.flight {
            Some(fold) if self.readable && !self.rewritten => fold.flight(),
            _ => FileFlight::Unknown,
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
        if let Some(flight) = self.flight.as_mut() {
            flight.restart();
        }
        self.fold = TranscriptUsage::following(&self.fold);
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
        self.advance_tapped(budget, None)
    }

    /// [`Self::advance`], handing every line it completes to the step fold
    /// `tap` as well, with the line's end offset (addendum A20): the main
    /// transcript's ([`SessionUsage::refresh_tapped`]). The tap is bound
    /// ([`StepFold::read_at`]) before the first line of a read, with the
    /// file's identity, its length and whether the fold started over, so a
    /// restart drops the tap's in-flight state and keeps its seed. What the
    /// usage fold does is unchanged.
    ///
    /// # Errors
    ///
    /// As [`Self::advance`].
    pub fn advance_tapped(
        &mut self,
        budget: u64,
        mut tap: Option<&mut StepFold>,
    ) -> io::Result<Advance> {
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
        let id = open_identity(&file, &meta).map(|(dev, ino)| FileId { dev, ino });
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
        if let Some(tap) = tap.as_deref_mut() {
            tap.read_at(path_fp(&self.path), id.map(FileId::pair), len, restarted);
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
                self.consume(&chunk[..n], tap.as_deref_mut());
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

    /// Fold every line `bytes` completes; keep the torn tail pending. The
    /// bytes start at [`Self::offset`]; each completed line goes to `tap`
    /// too, with the offset just past its newline.
    fn consume(&mut self, mut bytes: &[u8], mut tap: Option<&mut StepFold>) {
        let mut end = self.offset;
        while let Some(nl) = bytes.iter().position(|&b| b == b'\n') {
            let line = &bytes[..nl];
            end += nl as u64 + 1;
            if self.overflow {
                self.fold.note_long_line();
                if let Some(flight) = self.flight.as_mut() {
                    flight.long_line();
                }
                if let Some(tap) = tap.as_deref_mut() {
                    tap.long_line(end);
                }
                self.overflow = false;
            } else if self.pending.is_empty() {
                self.fold.fold_bytes(line);
                if let Some(flight) = self.flight.as_mut() {
                    flight.fold_bytes(line);
                }
                if let Some(tap) = tap.as_deref_mut() {
                    tap.push(line, end);
                }
            } else if self.pending.len().saturating_add(line.len()) > MAX_LINE_BYTES {
                self.fold.note_long_line();
                if let Some(flight) = self.flight.as_mut() {
                    flight.long_line();
                }
                if let Some(tap) = tap.as_deref_mut() {
                    tap.long_line(end);
                }
            } else {
                self.pending.extend_from_slice(line);
                let whole = std::mem::take(&mut self.pending);
                self.fold.fold_bytes(&whole);
                if let Some(flight) = self.flight.as_mut() {
                    flight.fold_bytes(&whole);
                }
                if let Some(tap) = tap.as_deref_mut() {
                    tap.push(&whole, end);
                }
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

// One session: its transcript and its subagents' transcripts

/// What one [`SessionUsage::refresh`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Refresh {
    /// Whether every file followed has been read to its end, so the totals are no mid-read
    /// prefix. What is not followed is [`Self::unread`].
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
    dirs_seen: Vec<DirSeen>,
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
    /// What each followed agent's `meta.json` names it ([`AgentMeta`]): read
    /// once, when its file is admitted, and again only while it is missing
    /// (at most [`META_TRIES`] times, each when the file moved).
    metas: BTreeMap<PathBuf, AgentMeta>,
    /// Each workflow run's script ([`RunMeta`]), keyed by its run id.
    runs: BTreeMap<String, RunMeta>,
    /// Moved with every meta or script learned: [`Self::flight_generation`].
    learned: u64,
    /// Every `meta.json` opened and every scripts listing (the tests' meter).
    #[cfg(test)]
    meta_reads: u64,
}

/// The most times an agent's `meta.json` or a run's script is looked for.
pub const META_TRIES: u8 = 4;

/// The most bytes of an agent's `meta.json` read (measured: 200 bytes).
const META_CAP: u64 = 16 * 1024;

/// What a subagent's `meta.json` names it, as its panel row's name and
/// activity would read it ([`Flyer::agent`]); `None` until read.
#[derive(Debug, Clone, Default)]
struct AgentMeta {
    flyer: Option<Flyer>,
    tries: u8,
}

/// A workflow run's script, `<session>/workflows/scripts/<slug>-<run>.js`:
/// its slug and its mtime (the run's start, within 8 ms of the run's own
/// `startTime`, measured); `None` until found.
#[derive(Debug, Clone, Default)]
struct RunMeta {
    found: Option<(String, i64)>,
    tries: u8,
}

/// The workflow run a subagent file under `subagents` belongs to:
/// `workflows/<run>/…`.
fn run_of<'a>(subagents: &Path, path: &'a Path) -> Option<&'a str> {
    let mut parts = path.strip_prefix(subagents).ok()?.components();
    if parts.next()?.as_os_str() != "workflows" {
        return None;
    }
    let run = parts.next()?.as_os_str().to_str()?;
    parts.next().is_some().then_some(run)
}

/// An agent's `meta.json` beside its transcript (`agent-<id>.jsonl` →
/// `agent-<id>.meta.json`), read into what its panel row would say:
/// `name ?? agentType`, and `description`. `None` when absent or unreadable.
fn read_meta(path: &Path) -> Option<Flyer> {
    let stem = path.file_stem()?.to_str()?;
    let meta = path.with_file_name(format!("{stem}.meta.json"));
    let text = super::footer::read_small(&meta, META_CAP)?;
    let value: aterm_json::Value = aterm_json::from_str(&text).ok()?;
    let word = |key: &str| {
        value
            .get(key)
            .and_then(aterm_json::Value::as_str)
            .filter(|s| !s.trim().is_empty())
    };
    let name = word("name").or_else(|| word("agentType"))?;
    Some(Flyer::agent(
        name,
        word("description"),
        0,
        FileFlight::Unknown,
    ))
}

/// The script of workflow run `run` in `scripts`: its slug and its mtime
/// (unix ms). `None` when there is none, or more than one.
fn find_script(scripts: &Path, run: &str) -> Option<(String, i64)> {
    let suffix = format!("-{run}.js");
    let mut found = None;
    for entry in std::fs::read_dir(scripts).ok()?.flatten() {
        let name = entry.file_name();
        let Some(slug) = name.to_str().and_then(|n| n.strip_suffix(suffix.as_str())) else {
            continue;
        };
        if found.is_some() || slug.is_empty() {
            return None;
        }
        let mtime = entry.metadata().ok()?.modified().ok()?;
        let ms = mtime
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_millis();
        found = Some((slug.to_owned(), i64::try_from(ms).ok()?));
    }
    found
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
            metas: BTreeMap::new(),
            runs: BTreeMap::new(),
            learned: 0,
            #[cfg(test)]
            meta_reads: 0,
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

    /// Bind the step fold `tap` to the transcript as it is now (its length,
    /// its identity, asked of the open file as the cursor asks it,
    /// [`open_identity`]): a tap armed after this fold's first read seeds at
    /// the file's length then, never at its next read's (S1, A20). Nothing
    /// when the file cannot be opened; the next read binds it.
    pub fn bind_tap(&self, tap: &mut StepFold) {
        let path = self.main.path();
        let Some(file) = super::footer::open_regular(path) else {
            return;
        };
        if let Ok(meta) = file.metadata() {
            tap.read_at(
                path_fp(path),
                open_identity(&file, &meta),
                meta.len(),
                false,
            );
        }
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
    /// (at most [`MAX_SUBAGENT_FILES`]), and a listing of each directory whose
    /// stamp was racy at the walk ([`DirSeen::racy`]); nothing is opened or
    /// walked.
    #[must_use]
    pub fn moved(&self) -> bool {
        (!self.caught_up && self.progressed)
            || std::fs::metadata(self.main.path()).map_or(true, |m| !self.main.unchanged(&m))
            || self.dirs_seen.iter().any(DirSeen::moved)
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
        self.refresh_tapped(budget, None)
    }

    /// [`Self::refresh`], the main transcript's lines handed to the step
    /// fold `tap` as well ([`TranscriptCursor::advance_tapped`]); no
    /// subagent file is tapped.
    pub fn refresh_tapped(&mut self, budget: u64, tap: Option<&mut StepFold>) -> Refresh {
        let mut left = budget;
        let mut caught_up = match self.main.advance_tapped(left, tap) {
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
                let mut cursor = TranscriptCursor::subagent(&path);
                if !advance_subagent(&mut cursor, &mut left, &mut caught_up, &mut unread) {
                    continue;
                }
                self.subagents.insert(path, cursor);
            }
            self.learn(&dir);
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

    /// Look for what names each followed agent and workflow run
    /// ([`AgentMeta`], [`RunMeta`]) that is not yet known and has tries left:
    /// one small read per agent, one listing of the scripts per run — a run
    /// once a pass however many of its agents' files are followed, so a run
    /// of many agents spends one of its tries a pass, as a lone agent does.
    /// A key already known is found without making one.
    fn learn(&mut self, subagents: &Path) {
        let scripts = subagents
            .parent()
            .map(|session| session.join("workflows").join("scripts"));
        let mut looked: Vec<&str> = Vec::new();
        for path in self.subagents.keys() {
            match run_of(subagents, path) {
                Some(run) => {
                    if looked.contains(&run) {
                        continue;
                    }
                    if !self.runs.contains_key(run) {
                        self.runs.insert(run.to_owned(), RunMeta::default());
                    }
                    let Some(meta) = self.runs.get_mut(run) else {
                        continue;
                    };
                    if meta.found.is_some() || meta.tries >= META_TRIES {
                        continue;
                    }
                    looked.push(run);
                    meta.tries += 1;
                    #[cfg(test)]
                    {
                        self.meta_reads += 1;
                    }
                    meta.found = scripts.as_deref().and_then(|dir| find_script(dir, run));
                    if meta.found.is_some() {
                        self.learned = self.learned.wrapping_add(1);
                    }
                }
                None => {
                    if !self.metas.contains_key(path) {
                        self.metas.insert(path.clone(), AgentMeta::default());
                    }
                    let Some(meta) = self.metas.get_mut(path) else {
                        continue;
                    };
                    if meta.flyer.is_some() || meta.tries >= META_TRIES {
                        continue;
                    }
                    meta.tries += 1;
                    #[cfg(test)]
                    {
                        self.meta_reads += 1;
                    }
                    meta.flyer = read_meta(path);
                    if meta.flyer.is_some() {
                        self.learned = self.learned.wrapping_add(1);
                    }
                }
            }
        }
    }

    /// The session's agents and workflow runs as last read
    /// ([`super::flight`]): each agent its meta names and whose start is
    /// read, with what its file says; each run whose script is found, with
    /// its agents' flights combined ([`super::flight::combine`]).
    #[must_use]
    pub fn flights(&self) -> FlightTable {
        let mut flyers = Vec::new();
        let Some(dir) = self.subagent_dir() else {
            return FlightTable { flyers };
        };
        let mut runs: BTreeMap<&str, Vec<FileFlight>> = BTreeMap::new();
        for (path, cursor) in &self.subagents {
            if let Some(run) = run_of(&dir, path) {
                runs.entry(run).or_default().push(cursor.file_flight());
                continue;
            }
            let (Some(flyer), Some(start)) = (
                self.metas.get(path).and_then(|m| m.flyer.as_ref()),
                cursor.flight.as_ref().and_then(FlightFold::start_ms),
            ) else {
                continue;
            };
            flyers.push(Flyer {
                start_ms: start,
                flight: cursor.file_flight(),
                ..flyer.clone()
            });
        }
        for (run, flights) in runs {
            if let Some((slug, start)) = self.runs.get(run).and_then(|r| r.found.as_ref()) {
                flyers.push(Flyer::workflow(
                    slug,
                    *start,
                    super::flight::combine(flights),
                ));
            }
        }
        FlightTable { flyers }
    }

    /// What moves whenever [`Self::flights`] would: every subagent file's
    /// [`FlightFold::generation`] and readability, and what was learned of
    /// their names.
    #[must_use]
    pub fn flight_generation(&self) -> u64 {
        self.subagents
            .values()
            .fold(self.learned, |sum, cursor| {
                let fold = cursor.flight.as_ref().map_or(0, FlightFold::generation);
                let state = u64::from(cursor.readable) | u64::from(cursor.rewritten) << 1;
                sum.wrapping_mul(31)
                    .wrapping_add(fold.wrapping_mul(4) | state)
            })
            .wrapping_add(self.subagents.len() as u64)
    }

    /// Every `meta.json` read and scripts listing looked at, over the
    /// session's life.
    #[cfg(test)]
    pub(crate) fn meta_reads(&self) -> u64 {
        self.meta_reads
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

    /// Milliseconds Claude was working in this conversation: its main transcript's finished turns
    /// ([`TranscriptUsage::busy_ms`]). Subagent files are not read for it: their turns run inside
    /// the main chain's (measured: 0.00h and 0.13h outside them in two long sessions).
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

    /// Every file's [`TranscriptUsage::generation`] summed: it moves whenever
    /// what [`Self::traffic`] draws from does.
    #[must_use]
    pub fn traffic_generation(&self) -> u64 {
        std::iter::once(&self.main)
            .chain(self.subagents.values())
            .fold(0u64, |sum, cursor| {
                sum.wrapping_add(cursor.fold().generation())
            })
    }

    /// The session's traffic over time ([`TrafficView`]): the series of the
    /// transcript and every subagent file merged, as [`Self::total_fold`]
    /// sums their spend, and the main chain's working time (subagents' turns
    /// run inside it, [`Self::busy_ms`]).
    #[must_use]
    pub fn traffic(&self) -> TrafficView {
        let mut series = super::usage::TokenSeries::default();
        let mut unread = 0u64;
        let mut totals = (0u64, 0u64);
        for cursor in std::iter::once(&self.main).chain(self.subagents.values()) {
            let fold = cursor.fold();
            series.absorb(fold.series());
            unread = unread.saturating_add(fold.unstamped());
            for spend in fold.per_model().values() {
                let tokens = spend.tokens();
                totals = (
                    totals.0.saturating_add(tokens.prompt()),
                    totals.1.saturating_add(tokens.output),
                );
            }
        }
        let spans = self.main.fold().busy_spans();
        let busy = spans
            .iter()
            .skip(spans.len().saturating_sub(TRAFFIC_BUSY_SPANS))
            .map(|&(start, end)| {
                (
                    start.div_euclid(1000),
                    end.saturating_add(999).div_euclid(1000),
                )
            })
            .collect();
        let busy_ms = self.busy_ms();
        TrafficView {
            start_s: series.start_s(),
            width_s: series.width_s(),
            buckets: series.buckets().iter().copied().collect(),
            busy,
            runtime_s: (busy_ms > 0).then_some(busy_ms / 1000),
            unread: usize::try_from(unread).unwrap_or(usize::MAX),
            totals,
        }
    }
}

/// The newest working spans a [`TrafficView`] carries.
pub const TRAFFIC_BUSY_SPANS: usize = 512;

/// A session's tokens over time, for the footer's traffic card: the merged
/// [`super::usage::TokenSeries`] of its files, flat, with the turns Claude
/// worked through. Built on the resolver thread ([`SessionUsage::traffic`])
/// and shared read-only.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrafficView {
    /// Unix seconds of the first bucket's start.
    pub start_s: i64,
    /// Each bucket's width in seconds ([`super::usage::SERIES_BASE_S`]·2ᵏ;
    /// 0 with no traffic).
    pub width_s: i64,
    /// `(in, out)` tokens per bucket, oldest first: in is the prompt (input,
    /// cache writes and reads, the `Σ ↑`), out the output (the `Σ ↓`).
    pub buckets: Box<[(u64, u64)]>,
    /// The newest [`TRAFFIC_BUSY_SPANS`] spans Claude was working, `[start,
    /// end)` in unix seconds (outward to whole seconds), in time order.
    pub busy: Box<[(i64, i64)]>,
    /// The working time in seconds ([`SessionUsage::busy_ms`]), once a turn
    /// ended.
    pub runtime_s: Option<u64>,
    /// Summed rows not graphed: their row carried no `timestamp`
    /// ([`super::usage::TranscriptUsage::unstamped`]).
    pub unread: usize,
    /// Every file's tokens, `(in, out)`, graphed or not.
    pub totals: (u64, u64),
}

impl TrafficView {
    /// Whether no row was graphed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buckets.iter().all(|&(i, o)| i == 0 && o == 0)
    }

    /// The buckets spread over `n` columns of `from_s..to_s` (unix seconds),
    /// exactly as the series it was taken from spreads them
    /// ([`super::usage::TokenSeries::columns`]). Pure.
    #[must_use]
    pub fn columns(&self, from_s: i64, to_s: i64, n: usize) -> Vec<(u64, u64)> {
        super::usage::spread_columns(
            self.start_s.checked_div(self.width_s).unwrap_or(0),
            self.width_s,
            self.buckets.iter().copied(),
            (from_s, to_s),
            n,
        )
    }
}

/// A directory the subagent walk looked at ([`SessionUsage::moved`]).
#[derive(Debug, Clone, PartialEq, Eq)]
struct DirSeen {
    path: PathBuf,
    /// What `stat` said of it at the walk (`None`: not there).
    stamp: Option<Stamp>,
    /// The entries the walk read in it, kept only where its stamp was RACY: it
    /// changed less than [`RACY_DIR`] before the walk (or after it, or its
    /// time cannot be read), so an entry made since in the same clock tick
    /// leaves the stamp as it is. Such a directory is listed again by `moved`.
    racy: Option<usize>,
}

impl DirSeen {
    /// `path` as the walk that began at `walked` found it, holding `entries`.
    fn at(path: PathBuf, stamp: Option<Stamp>, entries: usize, walked: SystemTime) -> Self {
        let racy = stamp
            .is_some_and(|s| {
                s.mtime
                    .is_none_or(|m| walked.duration_since(m).map_or(true, |age| age < RACY_DIR))
            })
            .then_some(entries);
        Self { path, stamp, racy }
    }

    /// Whether this directory changed since the walk: its stamp now, and where
    /// the stamp was racy, whether its entry count changed. One entry past
    /// the recorded count is enough to know it grew; never finish listing a
    /// larger directory just to establish the same difference.
    fn moved(&self) -> bool {
        let cap = self.racy.map_or(0, |n| n.saturating_add(1));
        dir_moved(self, stamp_of(&self.path), || {
            std::fs::read_dir(&self.path)
                .ok()
                .map(|entries| entries.take(cap).count())
        })
    }
}

/// Whether `seen` moved, given its stamp `now` and a count of its entries now
/// (asked only for a racy stamp): a stamp that differs, or a racy directory
/// whose entries are not the walk's.
fn dir_moved(seen: &DirSeen, now: Option<Stamp>, entries: impl FnOnce() -> Option<usize>) -> bool {
    now != seen.stamp || seen.racy.is_some_and(|n| entries() != Some(n))
}

/// What the subagent walk found.
struct Listing {
    /// Every `agent-*.jsonl` regular file, with what `lstat` said of it.
    files: Vec<(PathBuf, std::fs::Metadata)>,
    /// Every directory looked at, with what `stat` said of it — the walk's
    /// root among them even when it is not there (`None`).
    dirs: Vec<DirSeen>,
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
    let walked = SystemTime::now();
    while let Some((here, depth)) = stack.pop() {
        let stamp = stamp_of(&here);
        let Ok(entries) = std::fs::read_dir(&here) else {
            dirs.push(DirSeen::at(here, stamp, 0, walked));
            continue;
        };
        let mut held = 0usize;
        for entry in entries {
            held += 1;
            let Ok(entry) = entry else {
                continue;
            };
            seen += 1;
            if seen > max_entries {
                dirs.push(DirSeen::at(here, stamp, held, walked));
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
        dirs.push(DirSeen::at(here, stamp, held, walked));
    }
    Listing {
        files,
        dirs,
        cut: false,
    }
}

// The limit wall, from the transcript

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

/// The wall `rows` (the newest limit-notice row per window, from the main transcript's fold) shows
/// at `now`: the newest row's, while its reset is ahead and nothing has been served since.
///
/// The reset is placed from the row's own time by `supervise::limit::reset_in_zone`, with
/// `offset_at(zone, t)` giving the zone's offset (`None`: local); the five-hour window's bare clock
/// time is at most five hours ahead, any other window's is its next occurrence. A reset that cannot
/// be placed shows nothing.
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

// The usage facts: the footer's merged `Σ`, `aterm harness usage`'s per model

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
    /// This session's tokens per model, most first, at most [`NAMED_MODELS`] of them, folded live
    /// from its transcripts.
    pub models: Vec<ModelTokens>,
    /// The models past [`NAMED_MODELS`].
    pub more: More,
    /// Subagent transcripts not in these totals ([`Refresh::unread`]).
    pub unread: usize,
    /// More files may exist than were seen ([`Refresh::walk_cut`]).
    pub unread_more: bool,
    /// The refresh that folded these totals read the whole session ([`Refresh::complete`]):
    /// a rate taken between two facts is a rate only where both are complete.
    pub complete: bool,
    /// The limit wall still standing, from the transcript ([`wall_of`]).
    pub wall: Option<Wall>,
    /// Seconds Claude was working in this conversation: the union of its main transcript's
    /// finished turns ([`TranscriptUsage::busy_ms`]); `None` before a turn ends, or before the
    /// fold caught up ([`Self::with_runtime`]).
    pub runtime: Option<u64>,
}

impl UsageFacts {
    /// The facts — the tokens, the wall, and what `left_out` (the refresh
    /// that folded them) left out of the totals, said whether or not any
    /// tokens are shown yet — or `None` when there is nothing to show. No
    /// runtime: [`Self::with_runtime`] adds it. Complete only where that
    /// refresh says so ([`Refresh::complete`]); with no refresh, not.
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
            complete: left_out.is_some_and(Refresh::complete),
            wall,
            runtime: None,
        })
    }

    /// Every output token these facts count: the named models' and [`Self::more`]'s.
    #[must_use]
    pub fn output_total(&self) -> u64 {
        self.models
            .iter()
            .fold(self.more.output, |sum, m| sum.saturating_add(m.output))
    }

    /// Every input token these facts count, as [`ModelTokens::input`] counts them: the
    /// named models' and [`Self::more`]'s.
    #[must_use]
    pub fn input_total(&self) -> u64 {
        self.models
            .iter()
            .fold(self.more.input, |sum, m| sum.saturating_add(m.input))
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

/// A token count at the footer's grain, never rounded up: `940`, `1.2k`, `40k`, `310k`, `1.2M`,
/// `48M`, `3.1B`.
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
    /// was left out still said: the footer's `Σ` segment, one total rather
    /// than a stat per model.
    Total,
    /// Runtime alone: the footer's `Σ` where the rule is too narrow for the
    /// total.
    Runtime,
}

/// A working time at the footer's grain, never rounded up: `42s`, `12m03s`, `7h52m`, `130h`
/// (seconds dropped past an hour, minutes past 100 hours). Every form carries an `s` or an `h`, so
/// none reads as a token count (`12m` beside `12M`) where the segment narrows to the runtime alone.
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

/// The usage text (after its `Σ` mark) at `grain`. Merged: `<runtime> ↑<in> ↓<out>[ +N unread]`
/// (`7h52m ↑395M ↓2.0M`). Per model: `<runtime> [<model> ]↑<in> ↓<out>[ · …][ +N][ +N unread]`
/// (`18h46m opus ↑395M ↓2.0M · haiku ↑12M ↓88k +1`). ↑ is input, cache reads and writes included,
/// and ↓ output: Claude Code's own spinner arrows. Files left out of the totals are said after them
/// (`+3 unread`; `+3+ unread`, or `+? unread` alone, when the walk stopped short). The limit wall
/// is its own segment ([`wall_text`]). `None` when the grain has nothing to say.
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
        _ => pair(facts.input_total(), facts.output_total()),
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
