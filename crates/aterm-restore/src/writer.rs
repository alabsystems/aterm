// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The journal's single-writer protocols (the crash-restore plan §2.4).

use std::collections::VecDeque;

use aterm_core::grid::{HistoryFence, ResizePolicy};
use aterm_core::scrollback::Line;
use aterm_core::terminal::TerminalCheckpoint;

use crate::layout::{
    ERASED_COUNT_V1, Geometry, LAYOUT_VERSION, LayoutError, MAGIC, OFF_CELL_PX, OFF_CONFIG_FP,
    OFF_CTL_COMMIT, OFF_CTL_COUNT, OFF_CTL_LEN, OFF_ENGINE_RESIZE_LIFETIME,
    OFF_ENGINE_RESIZE_ORDINAL, OFF_HIST_CAP, OFF_HIST_CAP_PHYS, OFF_HIST_END_ROW, OFF_HIST_HI,
    OFF_HIST_IDENTITY, OFF_HIST_LO, OFF_HIST_LO_SEQ, OFF_HIST_OFF, OFF_KF_CAP, OFF_KF_OFF,
    OFF_LOCAL_ID, OFF_MAGIC, OFF_REGION_LEN, OFF_SID, OFF_STREAM_GATHERED, OFF_STREAM_PROCESSED,
    OFF_TAIL_CAP, OFF_TAIL_COMMIT, OFF_TAIL_FLOOR, OFF_TAIL_MARKS, OFF_TAIL_OFF, OFF_WRITER_BIRTH,
    OFF_WRITER_BUILD, OFF_WRITER_PID, SID_LEN, cell_px_word, kf_commit_word, kf_len_word, slot_of,
};
use crate::records::{
    EncodeError, HistoryIdentity, KeyframeHead, MAX_BYTES_PAYLOAD, TAIL_RECORD_HEADER, TailRecord,
    encode_control, encode_history_chunk, encode_keyframe, pad_header,
};
use crate::store::{RegionStore, RegionView, StoreError, Touched};

/// Who wrote a region: header facts a restore reports, never trusts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriterIdentity<'a> {
    /// The writer's pid.
    pub pid: u32,
    /// The writer's birth, microseconds since the epoch.
    pub birth_us: u64,
    /// The session's local id in the writer.
    pub local_id: u64,
    /// The writer's build number.
    pub build: u64,
    /// The session id (at most 64 ASCII bytes are kept; informational).
    pub sid: &'a str,
}

/// Why a write was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteError {
    /// The store refused an access.
    Store(StoreError),
    /// The geometry is not one a reader accepts, or not the store's.
    Layout(LayoutError),
    /// A record larger than its ring or slot.
    TooLarge {
        /// The record's length.
        len: usize,
        /// The space it must fit.
        cap: usize,
    },
    /// [`JournalWriter::finish_keyframe`] with no keyframe begun, or a begin while one is in progress.
    Sequence,
}

impl From<StoreError> for WriteError {
    fn from(e: StoreError) -> Self {
        Self::Store(e)
    }
}

impl std::fmt::Display for WriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Store(e) => write!(f, "the region refused a store: {e}"),
            Self::Layout(e) => write!(f, "a region layout no reader accepts: {e}"),
            Self::TooLarge { len, cap } => write!(f, "a {len}-byte record for a {cap}-byte space"),
            Self::Sequence => f.write_str("a keyframe step out of sequence"),
        }
    }
}

/// Why a keyframe was not begun.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyframeError {
    /// The record did not encode.
    Encode(EncodeError),
    /// The record is larger than a slot.
    TooLarge {
        /// The record's length.
        len: usize,
        /// The slot's capacity.
        cap: usize,
    },
    /// A keyframe is already in progress.
    InProgress,
    /// The store refused the commit word's store.
    Store(StoreError),
}

impl std::fmt::Display for KeyframeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Encode(e) => write!(f, "{e}"),
            Self::TooLarge { len, cap } => {
                write!(f, "a {len}-byte keyframe for a {cap}-byte slot")
            }
            Self::InProgress => f.write_str("a keyframe is already being written"),
            Self::Store(e) => write!(f, "the region refused a store: {e}"),
        }
    }
}

/// What one keyframe captures, beside the writer's own positions.
#[derive(Debug, Clone, Copy)]
pub struct KeyframeInput<'a> {
    /// The projection.
    pub checkpoint: &'a TerminalCheckpoint,
    /// `Terminal::history_fence()`.
    pub fence: HistoryFence,
    /// `Terminal::resize_lifetime()`.
    pub resize_lifetime: u64,
    /// `Terminal::resize_ordinal()`.
    pub resize_ordinal: u64,
    /// The bridge the projection was bounded by: the decoder's bound on the history it may hold.
    pub bridge_lines: u32,
    /// The engine-relevant config's fingerprint.
    pub config_fp: u64,
    /// The engine required a shell-integration nonce and had one in use (`Terminal::shell_integration_posture() == ShellIntegrationPosture::On`).
    pub integration_on: bool,
}

/// A keyframe begun and not yet finished.
#[derive(Debug)]
struct Pending {
    seq: u64,
    slot: usize,
    record: Vec<u8>,
    tail_from: u64,
}

/// A writer with a planted defect: the negative controls of the Tier-1 sweep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WriterFault {
    /// The keyframe's commit word is stored at the begin, before the copy (the model's `Buggy = 1`).
    PublishEarly,
    /// The tail's floor is never moved before an overwrite (the model's `Buggy = 2`).
    NoFloor,
    /// The tail's floor is moved after the copy that overwrites what it guards, not before.
    FloorAfterCopy,
    /// The tail's stream count and commit word are stored before the copy, not after.
    CommitBeforeCopy,
    /// The control slot's commit word is stored with the new record's sequence number before its copy, never zeroed first ([`ControlWriter`]).
    ControlPublishEarly,
}

/// THE HISTORY RING'S WRITER (the plan's §2.4 "History append" and "Fence reset").
#[derive(Debug)]
pub struct HistoryWriter {
    /// The ring's offset in the region.
    hist_off: usize,
    /// The ring's physical capacity: no modulus is larger.
    hist_cap_phys: usize,
    /// The ring's modulus (`hist_cap`), changed only by a reset.
    pub(crate) cap: usize,
    pub(crate) lo: u64,
    pub(crate) hi: u64,
    next_seq: u64,
    identity: HistoryIdentity,
    /// `(start, seq, first_row)` of each chunk in `[lo, hi)`, oldest first.
    chunks: VecDeque<(u64, u64, u64)>,
    /// The absolute row one past the newest exported line (`hist_end_row`): the reset's
    /// `oldest` until a chunk lands.
    end_row: u64,
    /// One past the highest ring offset ever written, under any modulus: what a clear zeroes.
    pub(crate) touched: usize,
    /// Encoded bytes of the chunks in `[lo, hi)` and the lines they hold, for the adaptive
    /// cap's bytes-per-line ([`Self::bytes_per_line`]).
    run_bytes: u64,
    run_lines: u64,
}

/// The least logical cap the history ring is given (the plan's §2.6 floor).
pub const MIN_HIST_CAP: usize = 256 * 1024;
/// The least bytes per line the adaptive cap assumes (§2.6's `max(32, ewma)`).
pub const MIN_BYTES_PER_LINE: u64 = 32;

/// The history ring's logical cap (the plan's §2.6, judges 1 and 3: the 24 B/line cap the first design assumed rested on one corpus, and the designs measured 7.5 to 30.9 LZ4 bytes a line across theirs).
#[must_use]
pub fn adaptive_hist_cap(line_limit: Option<usize>, bytes_per_line: u64, phys: usize) -> usize {
    let cap = match line_limit {
        None => phys,
        Some(limit) => {
            let per_line = u128::from(bytes_per_line.max(MIN_BYTES_PER_LINE));
            let want = (limit as u128).saturating_mul(per_line).saturating_mul(5) / 4;
            usize::try_from(want)
                .unwrap_or(usize::MAX)
                .clamp(MIN_HIST_CAP.min(phys), phys)
        }
    };
    cap / 8 * 8
}

/// Zeros for [`JournalWriter::zero`] to copy from, so zeroing a span allocates nothing.
const ZEROS: [u8; 4096] = [0; 4096];

/// The journal's writer for one session's region.
#[derive(Debug)]
pub struct JournalWriter<S: RegionStore> {
    store: S,
    geometry: Geometry,
    /// The next keyframe's sequence number (the first is 1; a commit word of 0 is empty).
    next_seq: u64,
    pending: Option<Pending>,
    /// The record length each slot holds.
    slot_len: [usize; 2],
    /// One past the highest byte ever written in each slot: what
    /// [`Self::settle_clear`] zeroes.
    slot_high: [usize; 2],
    /// The newest committed keyframe's tail position.
    keyframe_tail_from: u64,
    tail_commit: u64,
    tail_floor: u64,
    stream_processed: u64,
    /// Start positions of the records in `[tail_floor, tail_commit)`, oldest first: the
    /// boundaries a floor may land on.
    tail_starts: VecDeque<u64>,
    tail_marks: u64,
    /// The ring's writer, for a single-threaded writer of the whole region ([`HistoryWriter`]: a window's worker keeps its own).
    pub(crate) history: HistoryWriter,
    /// The tail record being encoded, kept between appends so the tap allocates nothing
    /// per slice.
    scratch: Vec<u8>,
    #[cfg(test)]
    fault: Option<WriterFault>,
}

/// Store the gather thread's count of PTY bytes read.
///
/// # Errors
/// The store's refusal.
pub fn store_gathered<S: RegionStore + ?Sized>(store: &S, gathered: u64) -> Result<(), StoreError> {
    store.store_u64_release(OFF_STREAM_GATHERED, gathered)
}

/// Store the engine's resize lifetime and ordinal from the host's resize path, in the same terminal-lock hold as the resize.
///
/// # Errors
/// The store's refusal.
pub fn store_resize_word<S: RegionStore + ?Sized>(
    store: &S,
    lifetime: u64,
    ordinal: u64,
) -> Result<(), StoreError> {
    store.store_u64_release(OFF_ENGINE_RESIZE_LIFETIME, lifetime)?;
    store.store_u64_release(OFF_ENGINE_RESIZE_ORDINAL, ordinal)
}

/// Store the fingerprint of the engine-relevant config from the host's config, colour-scheme and cell-size paths, in the same hold as the engine mutation.
///
/// # Errors
/// The store's refusal.
pub fn store_config_fp<S: RegionStore + ?Sized>(
    store: &S,
    config_fp: u64,
) -> Result<(), StoreError> {
    store.store_u64_release(OFF_CONFIG_FP, config_fp)
}

/// Store the window's cell size ([`OFF_CELL_PX`]) from the host's cell-size path, in the same hold as the engine's `set_cell_pixel_size` and BEFORE the `config_fp` that folds it in.
///
/// # Errors
/// The store's refusal.
pub fn store_cell_px<S: RegionStore + ?Sized>(
    store: &S,
    px: Option<(u16, u16)>,
) -> Result<(), StoreError> {
    store.store_u64_release(OFF_CELL_PX, cell_px_word(px))
}

/// Store the window's control count ([`OFF_CTL_COUNT`]: `aterm-gui`'s turn-id count) from the thread that raised it.
///
/// # Errors
/// The store's refusal.
pub fn store_ctl_count<S: RegionStore + ?Sized>(store: &S, count: u64) -> Result<(), StoreError> {
    store.store_u64_release(OFF_CTL_COUNT, count)
}

/// ERASE a region no writer journals into any more.
///
/// # Errors
/// The store's refusal.
pub fn erase_region<S: RegionStore>(region: &Touched<S>) -> Result<(), StoreError> {
    region.store_u64_release(OFF_MAGIC, ERASED_COUNT_V1)?;
    for page in region.written_pages() {
        let mut at = page.start;
        for keep in [OFF_MAGIC..OFF_MAGIC + 8, OFF_CTL_COUNT..OFF_CTL_COUNT + 8] {
            let end = keep.start.min(page.end);
            if at < end {
                zero(region, at, end - at)?;
            }
            at = at.max(keep.end.min(page.end));
        }
        if at < page.end {
            zero(region, at, page.end - at)?;
        }
    }
    Ok(())
}

fn word(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

/// `pos mod cap` as a ring offset (`cap > 0`).
fn ring_offset(pos: u64, cap: usize) -> usize {
    usize::try_from(pos % word(cap.max(1))).unwrap_or(0)
}

/// Zero `len` bytes of `store` at `at`, a page of zeros at a time: one primitive store
/// (and one hold of a shared store's lock) per 4 KiB, so zeroing the whole history ring
/// never holds the region from the parse thread for more than a page's copy.
fn zero<S: RegionStore + ?Sized>(store: &S, at: usize, len: usize) -> Result<(), StoreError> {
    let mut done = 0;
    while done < len {
        let n = (len - done).min(ZEROS.len());
        store.write(at + done, &ZEROS[..n])?;
        done += n;
    }
    Ok(())
}

impl<S: RegionStore> JournalWriter<S> {
    /// Lay a fresh region out.
    ///
    /// # Errors
    /// [`WriteError::Layout`] for a geometry no reader accepts or one that is not the store's length; the store's refusal.
    pub fn create(
        store: S,
        geometry: Geometry,
        writer: &WriterIdentity<'_>,
    ) -> Result<Self, WriteError> {
        geometry.validate().map_err(WriteError::Layout)?;
        if store.len() != geometry.region_len {
            return Err(WriteError::Layout(LayoutError::RegionLen(store.len())));
        }
        store.store_u64_release(OFF_REGION_LEN, word(geometry.region_len))?;
        store.store_u64_release(OFF_WRITER_PID, u64::from(writer.pid))?;
        store.store_u64_release(OFF_WRITER_BIRTH, writer.birth_us)?;
        store.store_u64_release(OFF_LOCAL_ID, writer.local_id)?;
        store.store_u64_release(OFF_WRITER_BUILD, writer.build)?;
        let mut sid = [0u8; SID_LEN];
        for (dst, src) in sid
            .iter_mut()
            .zip(writer.sid.bytes().filter(u8::is_ascii_graphic))
        {
            *dst = src;
        }
        store.write(OFF_SID, &sid)?;
        for (at, value) in [
            (OFF_KF_OFF, geometry.kf_off),
            (OFF_KF_CAP, geometry.kf_cap),
            (OFF_TAIL_OFF, geometry.tail_off),
            (OFF_TAIL_CAP, geometry.tail_cap),
            (OFF_HIST_OFF, geometry.hist_off),
            (OFF_HIST_CAP_PHYS, geometry.hist_cap_phys),
            (OFF_HIST_CAP, geometry.hist_cap_phys),
        ] {
            store.store_u64_release(at, word(value))?;
        }
        let mut magic = [0u8; 8];
        magic[..4].copy_from_slice(&MAGIC);
        magic[4] = LAYOUT_VERSION;
        store.store_u64_release(OFF_MAGIC, u64::from_le_bytes(magic))?;
        Ok(Self {
            store,
            geometry,
            next_seq: 1,
            pending: None,
            slot_len: [0; 2],
            slot_high: [0; 2],
            keyframe_tail_from: 0,
            tail_commit: 0,
            tail_floor: 0,
            stream_processed: 0,
            tail_starts: VecDeque::new(),
            tail_marks: 0,
            scratch: Vec::new(),
            history: HistoryWriter::new(&geometry),
            #[cfg(test)]
            fault: None,
        })
    }

    /// Plant a defect (the Tier-1 negative controls).
    #[cfg(test)]
    pub(crate) fn plant(&mut self, fault: WriterFault) {
        self.fault = Some(fault);
    }

    /// Whether `fault` was planted: always `false` outside a test build.
    fn planted(&self, fault: WriterFault) -> bool {
        #[cfg(test)]
        {
            self.fault == Some(fault)
        }
        #[cfg(not(test))]
        {
            let _ = (self, fault);
            false
        }
    }

    /// The region's store.
    pub const fn store(&self) -> &S {
        &self.store
    }

    /// The region's layout.
    pub const fn geometry(&self) -> &Geometry {
        &self.geometry
    }

    /// The tail position one past the last complete record.
    pub const fn tail_commit(&self) -> u64 {
        self.tail_commit
    }

    /// The tail position below which bytes may be overwritten.
    pub const fn tail_floor(&self) -> u64 {
        self.tail_floor
    }

    /// PTY bytes processed, as of the tail's commit.
    pub const fn stream_processed(&self) -> u64 {
        self.stream_processed
    }

    /// Tail bytes written since the newest committed keyframe: what the policy's K3 (tail pressure) weighs against the ring.
    pub const fn tail_since_keyframe(&self) -> u64 {
        self.tail_commit.saturating_sub(self.keyframe_tail_from)
    }

    /// The next keyframe's sequence number.
    pub const fn next_keyframe_seq(&self) -> u64 {
        self.next_seq
    }

    /// Whether a keyframe was begun and not finished.
    pub const fn keyframe_in_progress(&self) -> bool {
        self.pending.is_some()
    }

    /// Zero `len` bytes at `at`, a page of zeros at a time.
    fn zero(&self, at: usize, len: usize) -> Result<(), StoreError> {
        zero(&self.store, at, len)
    }

    /// The most payload one BYTES record carries in this ring: [`MAX_BYTES_PAYLOAD`],
    /// or what fits beside a header in a small test ring.
    fn max_payload(&self) -> usize {
        let room = self.geometry.tail_cap.saturating_sub(TAIL_RECORD_HEADER) / 8 * 8;
        MAX_BYTES_PAYLOAD.min(room).max(1)
    }

    /// Append PTY bytes the engine just processed (a slice, after `Terminal::process`):
    /// # Errors
    ///
    /// The store's refusal.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "CrashRestoreJournal",
            action = "Commit",
            project = "aterm_restore::conformance_region::Rig::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "CrashRestoreJournal",
            action = "Slice",
            reason = "the engine's step, not the journal's: W's parse thread runs \
                      `Terminal::process` (aterm-core) on the slice before this append, \
                      which is `Commit`; Tier-1 (conformance_region.rs) runs the real \
                      `Terminal::process` for it and projects the units the engine shows"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "CrashRestoreJournal",
            action = "SliceClear",
            reason = "a `Slice` holding an ED3 or RIS: the clear generation moves inside \
                      `Terminal::process` (aterm-core); Tier-1 processes a real `ESC [ 3 J` \
                      unit and projects the engine's clear generation"
        )
    )]
    pub fn append_slice(&mut self, bytes: &[u8]) -> Result<(), WriteError> {
        self.append_bytes(bytes, false)
    }

    /// Append bytes the engine processed that did not come from the PTY (a foreground handback, a manual reset).
    ///
    /// # Errors
    /// The store's refusal.
    pub fn append_synthesized(&mut self, bytes: &[u8]) -> Result<(), WriteError> {
        self.append_bytes(bytes, true)
    }

    fn append_bytes(&mut self, bytes: &[u8], synthesized: bool) -> Result<(), WriteError> {
        let max = self.max_payload();
        for chunk in bytes.chunks(max) {
            let stream_end = if synthesized {
                self.stream_processed
            } else {
                self.stream_processed.saturating_add(word(chunk.len()))
            };
            self.append_record(
                &TailRecord::Bytes {
                    synthesized,
                    bytes: chunk,
                },
                stream_end,
            )?;
        }
        Ok(())
    }

    /// Append a resize the engine applied, in engine order (the reader's spine catch-up walks `Terminal::resize_journal_since` before each slice).
    ///
    /// # Errors
    /// The store's refusal.
    pub fn append_resize(
        &mut self,
        rows: u16,
        cols: u16,
        policy: ResizePolicy,
        lifetime: u64,
        ordinal: u64,
    ) -> Result<(), WriteError> {
        self.append_record(
            &TailRecord::Resize {
                rows,
                cols,
                policy,
                lifetime,
                ordinal,
            },
            self.stream_processed,
        )
    }

    /// Record that the engine's resize journal overran.
    ///
    /// # Errors
    /// The store's refusal.
    pub fn append_resizes_lost(&mut self, n: u32) -> Result<(), WriteError> {
        self.append_record(&TailRecord::ResizesLost { n }, self.stream_processed)
    }

    /// Record that the session's engine was replaced; what follows is another lifetime's, and the session needs a keyframe of it.
    ///
    /// # Errors
    /// The store's refusal.
    pub fn append_engine_replaced(&mut self) -> Result<(), WriteError> {
        self.append_record(&TailRecord::EngineReplaced, self.stream_processed)
    }

    /// Close the region: its last record says the session's journal goes on in another region ([`crate::records::KIND_SUPERSEDED`]), so a restore from it is never exact.
    ///
    /// # Errors
    /// The store's refusal.
    pub fn append_superseded(&mut self) -> Result<(), WriteError> {
        self.append_record(&TailRecord::Superseded, self.stream_processed)
    }

    /// Continue another region's stream count in this freshly laid-out one.
    ///
    /// # Errors
    /// [`WriteError::Sequence`] once anything was written past the layout (a keyframe or a tail record: the count they were stamped with stands).
    pub fn resume_stream(&mut self, processed: u64) -> Result<(), WriteError> {
        if self.next_seq != 1 || self.pending.is_some() || self.tail_commit != 0 {
            return Err(WriteError::Sequence);
        }
        self.stream_processed = processed;
        self.store
            .store_u64_release(OFF_STREAM_PROCESSED, processed)
            .map_err(WriteError::from)
    }

    /// Mark the region's history ring SHED.
    ///
    /// # Errors
    /// The store's refusal.
    pub fn shed_history(&mut self) -> Result<(), WriteError> {
        self.history.cap = 0;
        self.store
            .store_u64_release(OFF_HIST_CAP, 0)
            .map_err(WriteError::from)
    }

    /// The tail append protocol (module doc): floor, copy, stream count, commit.
    fn append_record(
        &mut self,
        record: &TailRecord<'_>,
        stream_end: u64,
    ) -> Result<(), WriteError> {
        let cap = self.geometry.tail_cap;
        let len = record.encoded_len();
        if len > cap {
            return Err(WriteError::TooLarge { len, cap });
        }
        let mut head = std::mem::take(&mut self.scratch);
        head.clear();
        let payload: &[u8] = match record {
            TailRecord::Bytes { bytes, .. } => {
                head.extend_from_slice(&record.header(stream_end));
                bytes
            }
            _ => {
                record.encode(stream_end, &mut head);
                &[]
            }
        };

        let off = ring_offset(self.tail_commit, cap);
        let room = cap - off;
        let gap = if len > room { room } else { 0 };
        let start = self.tail_commit.saturating_add(word(gap));
        let end = start.saturating_add(word(len));

        let need = end.saturating_sub(word(cap));
        let floor_moves = need > self.tail_floor && !self.planted(WriterFault::NoFloor);
        if floor_moves && !self.planted(WriterFault::FloorAfterCopy) {
            self.move_floor(need, start)?;
        }
        let commit_early = self.planted(WriterFault::CommitBeforeCopy);
        if commit_early {
            self.store
                .store_u64_release(OFF_STREAM_PROCESSED, stream_end)?;
            self.store.store_u64_release(OFF_TAIL_COMMIT, end)?;
        }

        if gap >= TAIL_RECORD_HEADER {
            self.store.write(
                self.geometry.tail_off + off,
                &pad_header(gap, self.stream_processed),
            )?;
        }
        let at = self.geometry.tail_off + ring_offset(start, cap);
        self.store.write(at, &head)?;
        if !payload.is_empty() {
            self.store.write(at + head.len(), payload)?;
        }
        let padding = len - head.len() - payload.len();
        if padding > 0 {
            self.store
                .write(at + head.len() + payload.len(), &[0u8; 8][..padding.min(8)])?;
        }

        if floor_moves && self.planted(WriterFault::FloorAfterCopy) {
            self.move_floor(need, start)?;
        }

        self.stream_processed = stream_end;
        self.tail_commit = end;
        if !commit_early {
            self.store
                .store_u64_release(OFF_STREAM_PROCESSED, self.stream_processed)?;
            self.store
                .store_u64_release(OFF_TAIL_COMMIT, self.tail_commit)?;
        }
        self.tail_starts.push_back(start);
        self.scratch = head;
        Ok(())
    }

    /// Move the floor onto the first record boundary at or past `need` (or onto `start`,
    /// the record being written, when the window holds none) and store it.
    fn move_floor(&mut self, need: u64, start: u64) -> Result<(), StoreError> {
        while self.tail_starts.front().is_some_and(|&s| s < need) {
            self.tail_starts.pop_front();
        }
        self.tail_floor = self.tail_starts.front().copied().unwrap_or(start);
        self.store
            .store_u64_release(OFF_TAIL_FLOOR, self.tail_floor)
    }

    /// Count the verified OSC 133/633 marks of the bytes about to be appended (how far processing them moved `Terminal::shell_marks_verified`).
    ///
    /// # Errors
    /// The store's refusal.
    pub fn note_marks(&mut self, delta: u64) -> Result<(), WriteError> {
        if delta == 0 {
            return Ok(());
        }
        self.tail_marks = self.tail_marks.saturating_add(delta);
        self.store
            .store_u64_release(OFF_TAIL_MARKS, self.tail_marks)?;
        Ok(())
    }

    /// [`store_gathered`] through the writer.
    ///
    /// # Errors
    /// The store's refusal.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "CrashRestoreJournal",
            action = "Gather",
            project = "aterm_restore::conformance_region::Rig::project"
        )
    )]
    pub fn set_gathered(&self, gathered: u64) -> Result<(), WriteError> {
        store_gathered(&self.store, gathered).map_err(WriteError::from)
    }

    /// [`store_resize_word`] through the writer.
    ///
    /// # Errors
    /// The store's refusal.
    pub fn set_resize_word(&self, lifetime: u64, ordinal: u64) -> Result<(), WriteError> {
        store_resize_word(&self.store, lifetime, ordinal).map_err(WriteError::from)
    }

    /// [`store_config_fp`] through the writer.
    ///
    /// # Errors
    /// The store's refusal.
    pub fn set_config_fp(&self, config_fp: u64) -> Result<(), WriteError> {
        store_config_fp(&self.store, config_fp).map_err(WriteError::from)
    }

    /// Capture a keyframe and begin its write.
    ///
    /// # Errors
    /// [`KeyframeError::TooLarge`] for a record over the slot (nothing is stored: the caller retries with a smaller bridge), an encoding failure, a keyframe already in progress, or the store's refusal.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "CrashRestoreJournal",
            action = "BeginKf",
            project = "aterm_restore::conformance_region::Rig::project"
        )
    )]
    pub fn begin_keyframe(&mut self, input: &KeyframeInput<'_>) -> Result<u64, KeyframeError> {
        if self.pending.is_some() {
            return Err(KeyframeError::InProgress);
        }
        let seq = self.next_seq;
        let head = KeyframeHead {
            seq,
            tail_from: self.tail_commit,
            stream_at: self.stream_processed,
            resize_lifetime: input.resize_lifetime,
            resize_ordinal: input.resize_ordinal,
            fence: input.fence,
            bridge_lines: input.bridge_lines,
            config_fp: input.config_fp,
            integration_on: input.integration_on,
        };
        let record = encode_keyframe(&head, input.checkpoint).map_err(KeyframeError::Encode)?;
        if record.len() > self.geometry.kf_cap {
            return Err(KeyframeError::TooLarge {
                len: record.len(),
                cap: self.geometry.kf_cap,
            });
        }
        let slot = slot_of(seq);
        let commit = if self.planted(WriterFault::PublishEarly) {
            seq
        } else {
            0
        };
        self.store
            .store_u64_release(kf_commit_word(slot), commit)
            .map_err(KeyframeError::Store)?;
        self.pending = Some(Pending {
            seq,
            slot,
            record,
            tail_from: head.tail_from,
        });
        Ok(seq)
    }

    /// Finish the keyframe [`Self::begin_keyframe`] began.
    ///
    /// # Errors
    /// [`WriteError::Sequence`] with none begun; the store's refusal.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "CrashRestoreJournal",
            action = "FinishKf",
            project = "aterm_restore::conformance_region::Rig::project"
        )
    )]
    pub fn finish_keyframe(&mut self) -> Result<u64, WriteError> {
        let pending = self.pending.take().ok_or(WriteError::Sequence)?;
        if let Some(high) = self.slot_high.get_mut(pending.slot) {
            *high = (*high).max(pending.record.len());
        }
        self.store
            .write(self.geometry.kf_slot_off(pending.slot), &pending.record)?;
        self.store
            .store_u64_release(kf_len_word(pending.slot), word(pending.record.len()))?;
        if !self.planted(WriterFault::PublishEarly) {
            self.store
                .store_u64_release(kf_commit_word(pending.slot), pending.seq)?;
        }
        if let Some(len) = self.slot_len.get_mut(pending.slot) {
            *len = pending.record.len();
        }
        self.next_seq = pending.seq.saturating_add(1);
        self.keyframe_tail_from = pending.tail_from;
        self.tail_marks = 0;
        self.store.store_u64_release(OFF_TAIL_MARKS, 0)?;
        Ok(pending.seq)
    }

    /// [`Self::begin_keyframe`] and [`Self::finish_keyframe`] in one call.
    ///
    /// # Errors
    /// As theirs.
    pub fn write_keyframe(&mut self, input: &KeyframeInput<'_>) -> Result<u64, KeyframeError> {
        self.begin_keyframe(input)?;
        self.finish_keyframe().map_err(|e| match e {
            WriteError::Store(e) => KeyframeError::Store(e),
            _ => KeyframeError::InProgress,
        })
    }

    /// Settle a clear (ED3, RIS) in the parse thread's sections.
    ///
    /// # Errors
    /// The store's refusal.
    pub fn settle_clear(&mut self) -> Result<(), WriteError> {
        if self.pending.is_some() || self.next_seq < 2 {
            return Ok(());
        }
        let newest = slot_of(self.next_seq - 1);
        let other = 1 - newest;

        let high = self.slot_high[other];
        if high > 0 {
            self.store.store_u64_release(kf_commit_word(other), 0)?;
            self.zero(self.geometry.kf_slot_off(other), high)?;
            self.store.store_u64_release(kf_len_word(other), 0)?;
            self.slot_len[other] = 0;
            self.slot_high[other] = 0;
        }

        let (len, high) = (self.slot_len[newest], self.slot_high[newest]);
        if high > len {
            self.zero(self.geometry.kf_slot_off(newest) + len, high - len)?;
            self.slot_high[newest] = len;
        }

        let floor = self.tail_floor.max(self.keyframe_tail_from);
        if floor > self.tail_floor {
            self.tail_floor = floor;
            while self.tail_starts.front().is_some_and(|&s| s < floor) {
                self.tail_starts.pop_front();
            }
            self.store
                .store_u64_release(OFF_TAIL_FLOOR, self.tail_floor)?;
        }
        let cap = self.geometry.tail_cap;
        let live = usize::try_from(self.tail_commit.saturating_sub(self.tail_floor))
            .unwrap_or(cap)
            .min(cap);
        let dead = cap - live;
        let from = ring_offset(self.tail_commit, cap);
        let to_end = dead.min(cap - from);
        self.zero(self.geometry.tail_off + from, to_end)?;
        self.zero(self.geometry.tail_off, dead - to_end)?;
        Ok(())
    }

    /// [`HistoryWriter::reset_ring`] over this writer's store.
    ///
    /// # Errors
    /// The store's refusal.
    pub fn reset_history(
        &mut self,
        identity: HistoryIdentity,
        oldest: u64,
        cap: usize,
        clear: bool,
    ) -> Result<(), WriteError> {
        self.history
            .reset_ring(&self.store, identity, oldest, cap, clear)
    }

    /// [`HistoryWriter::append_chunk`] over this writer's store.
    ///
    /// # Errors
    /// As [`HistoryWriter::append_chunk`]'s.
    pub fn append_history(&mut self, first_row: u64, lines: &[Line]) -> Result<u64, WriteError> {
        self.history.append_chunk(&self.store, first_row, lines)
    }
}

impl HistoryWriter {
    /// The ring of a freshly laid-out region of `geometry` ([`JournalWriter::create`] stores its words).
    #[must_use]
    pub fn new(geometry: &Geometry) -> Self {
        Self {
            hist_off: geometry.hist_off,
            hist_cap_phys: geometry.hist_cap_phys,
            cap: geometry.hist_cap_phys,
            lo: 0,
            hi: 0,
            next_seq: 0,
            identity: HistoryIdentity::default(),
            chunks: VecDeque::new(),
            end_row: 0,
            touched: 0,
            run_bytes: 0,
            run_lines: 0,
        }
    }

    /// The identity the ring's chunks are exported under (the default, whose width is 0, before the first reset: no engine's).
    #[must_use]
    pub const fn identity(&self) -> HistoryIdentity {
        self.identity
    }

    /// The absolute row one past the newest exported line: where the next chunk starts.
    #[must_use]
    pub const fn end_row(&self) -> u64 {
        self.end_row
    }

    /// The absolute row of the oldest line the ring still holds, `None` when it is empty.
    #[must_use]
    pub fn first_row(&self) -> Option<u64> {
        self.chunks.front().map(|&(_, _, row)| row)
    }

    /// The ring's logical cap (its modulus).
    #[must_use]
    pub const fn cap(&self) -> usize {
        self.cap
    }

    /// One past the highest ring offset ever written: the ring bytes this region has touched, and so keeps resident.
    #[must_use]
    pub const fn touched(&self) -> usize {
        self.touched
    }

    /// The encoded bytes per line of the chunks the ring holds, rounded up.
    #[must_use]
    pub fn bytes_per_line(&self) -> Option<u64> {
        (self.run_lines > 0).then(|| self.run_bytes.div_ceil(self.run_lines))
    }

    /// Start the ring over under `identity`, whose oldest line is absolute row `oldest`, with modulus `cap` (clamped to the physical ring, a multiple of 8).
    ///
    /// # Errors
    /// The store's refusal.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "CrashRestoreJournal",
            action = "RingReset",
            project = "aterm_restore::conformance_region::Rig::project"
        )
    )]
    pub fn reset_ring<S: RegionStore + ?Sized>(
        &mut self,
        store: &S,
        identity: HistoryIdentity,
        oldest: u64,
        cap: usize,
        clear: bool,
    ) -> Result<(), WriteError> {
        let cap = cap.min(self.hist_cap_phys) / 8 * 8;
        self.lo = self.hi;
        self.chunks.clear();
        self.run_bytes = 0;
        self.run_lines = 0;
        store.store_u64_release(OFF_HIST_LO, self.lo)?;
        store.store_u64_release(OFF_HIST_LO_SEQ, self.next_seq)?;
        for (i, value) in [
            identity.renumber_epoch,
            identity.clear_gen,
            identity.reveal_gen,
            u64::from(identity.cols),
        ]
        .into_iter()
        .enumerate()
        {
            store.store_u64_release(OFF_HIST_IDENTITY + 8 * i, value)?;
        }
        store.store_u64_release(OFF_HIST_END_ROW, oldest)?;
        store.store_u64_release(OFF_HIST_CAP, word(cap))?;
        self.end_row = oldest;
        if clear && self.touched > 0 {
            zero(store, self.hist_off, self.touched)?;
            self.touched = 0;
        }
        self.identity = identity;
        self.cap = cap;
        Ok(())
    }

    /// Append one chunk: `lines` (at most 1024, wrapped at the ring identity's width), the first at absolute row `first_row`.
    ///
    /// # Errors
    /// [`WriteError::TooLarge`] for a chunk over the ring (nothing is stored: the caller splits it, and past one line starts the ring over after it, counted by the join).
    pub fn append_chunk<S: RegionStore + ?Sized>(
        &mut self,
        store: &S,
        first_row: u64,
        lines: &[Line],
    ) -> Result<u64, WriteError> {
        let cap = self.cap;
        let record =
            encode_history_chunk(self.next_seq, first_row, self.identity, lines).map_err(|_| {
                WriteError::TooLarge {
                    len: usize::MAX,
                    cap,
                }
            })?;
        let len = record.len();
        if len > cap {
            return Err(WriteError::TooLarge { len, cap });
        }
        let off = ring_offset(self.hi, cap);
        let room = cap - off;
        let gap = if len > room { room } else { 0 };
        let start = self.hi.saturating_add(word(gap));
        let end = start.saturating_add(word(len));
        let seq = self.next_seq;

        let need = end.saturating_sub(word(cap));
        if self.chunks.front().is_some_and(|&(s, _, _)| s < need) {
            while self.chunks.front().is_some_and(|&(s, _, _)| s < need) {
                self.chunks.pop_front();
            }
            let (lo, lo_seq) = self
                .chunks
                .front()
                .map_or((start, seq), |&(s, q, _)| (s, q));
            self.lo = lo;
            store.store_u64_release(OFF_HIST_LO_SEQ, lo_seq)?;
            store.store_u64_release(OFF_HIST_LO, lo)?;
        } else if self.chunks.is_empty() {
            self.lo = start;
            store.store_u64_release(OFF_HIST_LO_SEQ, seq)?;
            store.store_u64_release(OFF_HIST_LO, start)?;
        }

        if gap > 0 {
            self.touched = self.touched.max(cap);
            zero(store, self.hist_off + off, gap)?;
        }
        let at = ring_offset(start, cap);
        self.touched = self.touched.max(at + len);
        store.write(self.hist_off + at, &record)?;

        let n = word(lines.len());
        self.end_row = first_row.saturating_add(n);
        store.store_u64_release(OFF_HIST_END_ROW, self.end_row)?;
        self.hi = end;
        store.store_u64_release(OFF_HIST_HI, end)?;
        self.chunks.push_back((start, seq, first_row));
        self.next_seq = seq.saturating_add(1);
        self.run_lines = self
            .first_row()
            .map_or(0, |first| self.end_row.saturating_sub(first));
        self.run_bytes = self.hi.saturating_sub(self.lo);
        Ok(seq)
    }
}

/// THE CONTROL SLOT'S WRITER (the crash-restore plan's increment 7).
#[derive(Debug, Default)]
pub struct ControlWriter {
    /// The sequence number of the record the slot holds; 0 before the first write.
    seq: u64,
    /// One past the highest slot byte written in the current region: what a shorter
    /// record leaves behind to be zeroed.
    high: usize,
    #[cfg(test)]
    fault: Option<WriterFault>,
}

impl ControlWriter {
    /// A writer for a region whose slot was never written.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Plant a defect (the control slot's negative control).
    #[cfg(test)]
    pub(crate) fn plant(&mut self, fault: WriterFault) {
        self.fault = Some(fault);
    }

    /// Whether `fault` was planted: always `false` outside a test build.
    fn planted(&self, fault: WriterFault) -> bool {
        #[cfg(test)]
        {
            self.fault == Some(fault)
        }
        #[cfg(not(test))]
        {
            let _ = (self, fault);
            false
        }
    }

    /// The sequence number of the record the slot holds (0: none written).
    #[must_use]
    pub const fn seq(&self) -> u64 {
        self.seq
    }

    /// Write `carry` as the slot's next record (module doc, "Control slot").
    ///
    /// # Errors
    /// [`WriteError::TooLarge`] for a record past the slot (nothing is stored: the caller carries less); the store's refusal.
    pub fn write<S: RegionStore + ?Sized>(
        &mut self,
        store: &S,
        geometry: &Geometry,
        carry: &[u8],
    ) -> Result<u64, WriteError> {
        let seq = self.seq.saturating_add(1);
        let record = encode_control(seq, carry).map_err(|_| WriteError::TooLarge {
            len: usize::MAX,
            cap: geometry.ctl_cap(),
        })?;
        self.write_record(store, geometry, seq, &record)?;
        Ok(seq)
    }

    /// Give a fresh region (`to`, laid out by [`JournalWriter::create`]) the record the slot of `from`, the region it replaces, holds.
    ///
    /// # Errors
    /// The store's refusal, or [`WriteError::TooLarge`] for a slot `from`'s words say is longer than the slot.
    pub fn carry_into<V: RegionView + ?Sized, S: RegionStore + ?Sized>(
        &mut self,
        from: &V,
        to: &S,
        geometry: &Geometry,
    ) -> Result<(), WriteError> {
        self.high = 0;
        let seq = from.load_u64_acquire(OFF_CTL_COMMIT)?;
        if seq == 0 {
            return Ok(());
        }
        let len = usize::try_from(from.load_u64_acquire(OFF_CTL_LEN)?).unwrap_or(usize::MAX);
        let cap = geometry.ctl_cap();
        if len > cap {
            return Err(WriteError::TooLarge { len, cap });
        }
        let mut record = vec![0u8; len];
        from.read(geometry.ctl_off(), &mut record)?;
        self.write_record(to, geometry, seq, &record)
    }

    /// The slot's write protocol (module doc): commit word 0, copy, length, commit word,
    /// then the stale bytes past a shorter record zeroed.
    fn write_record<S: RegionStore + ?Sized>(
        &mut self,
        store: &S,
        geometry: &Geometry,
        seq: u64,
        record: &[u8],
    ) -> Result<(), WriteError> {
        let (len, cap) = (record.len(), geometry.ctl_cap());
        if len > cap {
            return Err(WriteError::TooLarge { len, cap });
        }
        let early = self.planted(WriterFault::ControlPublishEarly);
        store.store_u64_release(OFF_CTL_COMMIT, if early { seq } else { 0 })?;
        let stale = self.high;
        self.high = stale.max(len);
        store.write(geometry.ctl_off(), record)?;
        store.store_u64_release(OFF_CTL_LEN, word(len))?;
        if !early {
            store.store_u64_release(OFF_CTL_COMMIT, seq)?;
        }
        self.seq = seq;
        if stale > len {
            zero(store, geometry.ctl_off() + len, stale - len)?;
        }
        self.high = len;
        Ok(())
    }
}

#[cfg(test)]
#[path = "writer_tests.rs"]
mod tests;
