// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Restore a dead window's session from its journal region.

use std::ops::Range;

use astream_wire::steps::{StepEnd, StepFault, scan_steps_from};
use aterm_core::grid::{HistoryFence, MAX_GRID_COLS, MAX_GRID_ROWS};
use aterm_core::terminal::{Terminal, TerminalCheckpoint};

use crate::layout::{
    ERASED_COUNT_V1, Geometry, LAYOUT_VERSION, LayoutError, MAGIC, MAX_REGION_LEN, OFF_CELL_PX,
    OFF_CONFIG_FP, OFF_CTL_COMMIT, OFF_CTL_COUNT, OFF_CTL_LEN, OFF_ENGINE_RESIZE_LIFETIME,
    OFF_ENGINE_RESIZE_ORDINAL, OFF_HIST_CAP, OFF_HIST_CAP_PHYS, OFF_HIST_END_ROW, OFF_HIST_HI,
    OFF_HIST_IDENTITY, OFF_HIST_LO, OFF_HIST_LO_SEQ, OFF_HIST_OFF, OFF_KF_CAP, OFF_KF_OFF,
    OFF_LOCAL_ID, OFF_MAGIC, OFF_REGION_LEN, OFF_SID, OFF_STREAM_GATHERED, OFF_STREAM_PROCESSED,
    OFF_TAIL_CAP, OFF_TAIL_COMMIT, OFF_TAIL_FLOOR, OFF_TAIL_MARKS, OFF_TAIL_OFF, OFF_WRITER_BIRTH,
    OFF_WRITER_BUILD, OFF_WRITER_PID, SID_LEN, cell_px_of, kf_commit_word, kf_len_word, slot_of,
};
use crate::records::{
    ChunkFault, ControlFault, HistoryChunk, HistoryIdentity, KIND_PAD, Keyframe, KeyframeFault,
    KeyframeHead, TAIL_RECORD_HEADER, TailFault, TailHeader, TailRecord, decode_control,
    decode_history_chunk, decode_keyframe, decode_tail_payload,
};
use crate::store::RegionView;

/// How a restore runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecoverPolicy {
    /// Replay the tail past the keyframe.
    pub tail: bool,
    /// The restorer's config fingerprint.
    pub config_fp: u64,
    /// Scratch shell-integration nonce authorized during replay when the live engine had one.
    pub scratch_nonce: [u8; 32],
}

/// Why a restore fell to the META rung.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetaWhy {
    /// The header is not a region this build reads.
    Header(HeaderFault),
    /// Neither slot holds a keyframe this build restores.
    NoKeyframe([SlotVerdict; 2]),
}

/// Why a header was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderFault {
    /// A header word could not be read.
    Unreadable,
    /// Not a journal region.
    Magic,
    /// A layout version this build does not read.
    Version(u8),
    /// The length word is not the view's, or is past [`MAX_REGION_LEN`].
    RegionLen(u64),
    /// The section words do not fit.
    Layout(LayoutError),
}

/// What one keyframe slot holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotVerdict {
    /// Commit word 0: empty, or being written when the writer died.
    Empty,
    /// A commit word whose parity is the other slot's.
    WrongSlot(u64),
    /// A length of zero or past the slot.
    Length(u64),
    /// The slot could not be read.
    Unreadable,
    /// The record is not a keyframe this build restores.
    Keyframe(KeyframeFault),
    /// A valid keyframe with this sequence number.
    Valid(u64),
}

/// The header's facts: reported, never trusted for anything but bounds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderFacts {
    /// The writer's pid.
    pub writer_pid: u32,
    /// The writer's birth, microseconds.
    pub writer_birth_us: u64,
    /// The session's local id in the writer.
    pub local_id: u64,
    /// The writer's build number.
    pub writer_build: u64,
    /// The session id the writer recorded.
    pub sid: String,
    /// The validated layout.
    pub geometry: Geometry,
    /// The writer's config fingerprint as of its last config change.
    pub config_fp: u64,
    /// The writer's cell size as of its last cell-size change ([`OFF_CELL_PX`]).
    pub cell_px: Option<(u16, u16)>,
    /// The window's control count ([`OFF_CTL_COUNT`]).
    pub ctl_count: u64,
}

/// Everything a restore could not reproduce.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Losses {
    /// PTY bytes the writer read and never committed to the tail.
    pub in_flight_lost: u64,
    /// PTY bytes the tail held past the keyframe that the restore does not reflect.
    pub tail_unreplayed: u64,
    /// The floor had passed the keyframe's `tail_from`.
    pub tail_overflow: bool,
    /// The first invalid tail record, which ended the walk.
    pub tail_fault: Option<TailFault>,
    /// Verified marks the keyframe budget left in the tail.
    pub marks_lost: u64,
    /// Resizes the engine's journal overran before the tail caught up.
    pub resizes_lost: u64,
    /// Resizes the host applied that neither the keyframe nor a RESIZE record past it reached.
    pub resizes_unjournaled: u64,
    /// Resizes the replay applied whose outcome depends on engine state no checkpoint carries.
    pub regrids_replayed: u64,
    /// The replay crossed an ENGINE_REPLACED record.
    pub engine_replaced: bool,
    /// The tail ends in a SUPERSEDED record.
    pub superseded: bool,
    /// Kitty-graphics commands the replay ran.
    pub graphics_replayed: u64,
    /// Kitty unscrolls (`CSI n + T`) the replay ran on the main screen.
    pub unscrolls_replayed: u64,
    /// The host changed engine-relevant config or cell size after the newest keyframe.
    pub config_moved: bool,
    /// Sequence kind the replay ended inside that the result can only swallow.
    pub swallowed: Option<&'static str>,
}

/// Why a history ring's run ended before its commit word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RingFault {
    /// The ring's words are inconsistent.
    Words,
    /// A chunk is damaged or out of sequence.
    Chunk(StepEnd),
    /// A chunk's head is unreadable.
    Head(ChunkFault),
    /// A chunk names another identity than the ring's.
    Identity,
    /// Chunks whose rows do not follow one another, or end elsewhere than the end row.
    Rows,
}

/// The history ring's run of compressed chunks between its words, oldest first.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RingRun {
    /// The identity the ring's chunks were exported under.
    pub identity: HistoryIdentity,
    /// The absolute row one past the newest exported line.
    pub end_row: u64,
    /// The chunks, oldest first.
    pub chunks: Vec<HistoryChunk>,
    /// What ended the run early (its intact prefix is kept).
    pub fault: Option<RingFault>,
}

impl RingRun {
    /// The absolute row of the run's oldest line.
    #[must_use]
    pub fn first_row(&self) -> Option<u64> {
        self.chunks.first().map(|c| c.first_row)
    }

    /// How many lines the run holds.
    #[must_use]
    pub fn lines(&self) -> u64 {
        self.chunks.iter().map(|c| u64::from(c.n_lines)).sum()
    }
}

/// A restore that produced a screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Restored {
    /// The scratch engine's restored terminal checkpoint.
    pub checkpoint: TerminalCheckpoint,
    /// Whether every loss is zero and the replayed config matches the restorer's.
    pub exact: bool,
    /// The slot the keyframe came from.
    pub slot: usize,
    /// The keyframe's facts.
    pub keyframe: KeyframeHead,
    /// What the other slot held.
    pub other_slot: SlotVerdict,
    /// Tail records replayed onto the keyframe.
    pub replayed_records: u64,
    /// What it could not reproduce.
    pub losses: Losses,
    /// The scratch engine's fence right after the keyframe's restore.
    pub fence_before: HistoryFence,
    /// The scratch engine's fence after the replay.
    pub fence_after: HistoryFence,
    /// The history lines the scratch engine carries.
    pub carried_lines: u64,
    /// The history ring's run.
    pub ring: RingRun,
    /// The header's facts.
    pub header: HeaderFacts,
}

impl Restored {
    /// Whether the history ring's lines may be joined below the restored screen.
    #[must_use]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "CrashRestoreJournal",
            action = "Import",
            project = "aterm_restore::conformance_region::Rig::project"
        )
    )]
    pub fn history_admissible(&self) -> bool {
        !self.keyframe.fence.detached
            && !self.fence_before.detached
            && !self.fence_after.detached
            && HistoryIdentity::of(&self.fence_before) == HistoryIdentity::of(&self.fence_after)
            && self.ring.identity == HistoryIdentity::of(&self.keyframe.fence)
    }

    /// Compute the history join inputs for this restore, or why the head cannot be joined.
    /// # Errors
    /// Returns [`HeadLost`] if the history identity moved or detached across replay.
    pub fn join_facts(&self) -> Result<JoinFacts, HeadLost> {
        let held = !self.keyframe.fence.detached
            && !self.fence_before.detached
            && !self.fence_after.detached
            && HistoryIdentity::of(&self.fence_before) == HistoryIdentity::of(&self.fence_after);
        if !held {
            if self.fence_after.clear_gen != self.fence_before.clear_gen {
                return Err(HeadLost::Cleared);
            }
            return Err(HeadLost::Moved {
                lines: self
                    .keyframe
                    .fence
                    .lines()
                    .saturating_sub(self.fence_before.lines()),
            });
        }
        let pushed = self.fence_after.end.saturating_sub(self.fence_before.end);
        let head = HistoryFence {
            end: self.keyframe.fence.end.saturating_add(pushed),
            ..self.keyframe.fence
        };
        let export = self
            .ring
            .first_row()
            .filter(|_| self.history_admissible())
            .map(|ring_first| {
                let first = ring_first.max(head.oldest);
                let end = self.ring.end_row.min(head.end).max(first);
                RingExport {
                    fence: HistoryFence {
                        oldest: first,
                        end,
                        detached: false,
                        ..head
                    },
                    first,
                    lines: end - first,
                    skip: first - ring_first,
                }
            });
        Ok(JoinFacts {
            head,
            carried: self.carried_lines,
            export,
        })
    }

    /// Chunks in [`RingRun::chunks`] and leading-line skip to import `take` lines from `export`.
    #[must_use]
    pub fn ring_chunks_for(&self, export: &RingExport, take: u64) -> (Range<usize>, u64) {
        if take == 0 {
            return (0..0, 0);
        }
        let end_line = export.skip.saturating_add(take);
        let mut lines = 0u64;
        let mut first = None;
        let mut end = 0;
        for (i, chunk) in self.ring.chunks.iter().enumerate() {
            if lines >= end_line {
                break;
            }
            let next = lines.saturating_add(u64::from(chunk.n_lines));
            if first.is_none() && next > export.skip {
                first = Some((i, export.skip - lines));
            }
            end = i + 1;
            lines = next;
        }
        match first {
            Some((start, skip)) => (start..end, skip),
            None => (end..end, 0),
        }
    }
}

/// The ring's run fenced for the history join ([`Restored::join_facts`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RingExport {
    /// The ring's identity and row span clamped to the head.
    pub fence: HistoryFence,
    /// The absolute row of the first line the join may take.
    pub first: u64,
    /// How many lines from `first` the ring holds, up to the head's end.
    pub lines: u64,
    /// Lines in the ring older than `first` to skip at the front of the run.
    pub skip: u64,
}

impl RingExport {
    /// Clamp this export to at most its newest `max` lines.
    #[must_use]
    pub fn newest(self, max: u64) -> Self {
        let first = self.first.max(self.fence.end.saturating_sub(max));
        Self {
            fence: HistoryFence {
                oldest: first,
                ..self.fence
            },
            first,
            lines: self.fence.end - first,
            skip: self.skip + (first - self.first),
        }
    }
}

/// The history join's inputs ([`Restored::join_facts`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JoinFacts {
    /// The keyframe's history fence advanced by the lines the replay pushed.
    pub head: HistoryFence,
    /// How many of `head`'s newest lines the restored checkpoint carries.
    pub carried: u64,
    /// The ring's run, when it holds lines under the keyframe's identity.
    pub export: Option<RingExport>,
}

/// Why a restore has no head to join the ring below ([`Restored::join_facts`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadLost {
    /// The tail cleared the history.
    Cleared,
    /// The tail rewrapped or revealed the history, or a fence was taken mid-reflow.
    Moved {
        /// The keyframe's history lines the restored checkpoint does not carry.
        lines: u64,
    },
}

/// What a restore produced.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(
    clippy::large_enum_variant,
    reason = "the restored arm is boxed; the META arm carries two slot verdicts"
)]
pub enum Recovery {
    /// A screen, with its losses counted.
    Restored(Box<Restored>),
    /// No screen: the META rung.
    Meta(MetaWhy),
}

fn load<V: RegionView + ?Sized>(view: &V, at: usize) -> Option<u64> {
    view.load_u64_acquire(at).ok()
}

fn as_usize(v: u64) -> Option<usize> {
    usize::try_from(v).ok()
}

/// Read only the turn count of a privacy-erased v1 region.
#[must_use]
pub fn read_erased_turn_count<V: RegionView + ?Sized>(view: &V) -> Option<u64> {
    if view.len() > MAX_REGION_LEN || load(view, OFF_MAGIC)? != ERASED_COUNT_V1 {
        return None;
    }
    load(view, OFF_CTL_COUNT)
}

/// Read and validate a region's header without restoring the screen.
/// # Errors
/// Returns the [`HeaderFault`] that refuses the region.
pub fn read_header<V: RegionView + ?Sized>(view: &V) -> Result<HeaderFacts, HeaderFault> {
    let magic = load(view, OFF_MAGIC)
        .ok_or(HeaderFault::Unreadable)?
        .to_le_bytes();
    if magic[..4] != MAGIC || magic[5..] != [0, 0, 0] {
        return Err(HeaderFault::Magic);
    }
    if magic[4] == 0 || magic[4] > LAYOUT_VERSION {
        return Err(HeaderFault::Version(magic[4]));
    }
    let region_len = load(view, OFF_REGION_LEN).ok_or(HeaderFault::Unreadable)?;
    if as_usize(region_len) != Some(view.len()) || view.len() > MAX_REGION_LEN {
        return Err(HeaderFault::RegionLen(region_len));
    }
    let word = |at| {
        load(view, at)
            .and_then(as_usize)
            .ok_or(HeaderFault::Unreadable)
    };
    let geometry = Geometry {
        region_len: view.len(),
        kf_off: word(OFF_KF_OFF)?,
        kf_cap: word(OFF_KF_CAP)?,
        tail_off: word(OFF_TAIL_OFF)?,
        tail_cap: word(OFF_TAIL_CAP)?,
        hist_off: word(OFF_HIST_OFF)?,
        hist_cap_phys: word(OFF_HIST_CAP_PHYS)?,
    };
    geometry.validate().map_err(HeaderFault::Layout)?;
    let mut sid = [0u8; SID_LEN];
    view.read(OFF_SID, &mut sid)
        .map_err(|_| HeaderFault::Unreadable)?;
    let sid: String = sid
        .iter()
        .take_while(|b| **b != 0)
        .filter(|b| b.is_ascii_graphic())
        .map(|b| char::from(*b))
        .collect();
    let pid = load(view, OFF_WRITER_PID).ok_or(HeaderFault::Unreadable)?;
    Ok(HeaderFacts {
        writer_pid: u32::try_from(pid).unwrap_or(0),
        writer_birth_us: load(view, OFF_WRITER_BIRTH).ok_or(HeaderFault::Unreadable)?,
        local_id: load(view, OFF_LOCAL_ID).ok_or(HeaderFault::Unreadable)?,
        writer_build: load(view, OFF_WRITER_BUILD).ok_or(HeaderFault::Unreadable)?,
        sid,
        geometry,
        config_fp: load(view, OFF_CONFIG_FP).ok_or(HeaderFault::Unreadable)?,
        cell_px: cell_px_of(load(view, OFF_CELL_PX).ok_or(HeaderFault::Unreadable)?),
        ctl_count: load(view, OFF_CTL_COUNT).ok_or(HeaderFault::Unreadable)?,
    })
}

/// Judge one keyframe slot.
pub(crate) fn read_slot<V: RegionView + ?Sized>(
    view: &V,
    geometry: &Geometry,
    slot: usize,
) -> (SlotVerdict, Option<Keyframe>) {
    let Some(commit) = load(view, kf_commit_word(slot)) else {
        return (SlotVerdict::Unreadable, None);
    };
    if commit == 0 {
        return (SlotVerdict::Empty, None);
    }
    if slot_of(commit) != slot {
        return (SlotVerdict::WrongSlot(commit), None);
    }
    let Some(len) = load(view, kf_len_word(slot)) else {
        return (SlotVerdict::Unreadable, None);
    };
    let Some(n) = as_usize(len).filter(|n| *n > 0 && *n <= geometry.kf_cap) else {
        return (SlotVerdict::Length(len), None);
    };
    let mut record = Vec::new();
    if record.try_reserve_exact(n).is_err() {
        return (SlotVerdict::Unreadable, None);
    }
    record.resize(n, 0);
    if view.read(geometry.kf_slot_off(slot), &mut record).is_err() {
        return (SlotVerdict::Unreadable, None);
    }
    match decode_keyframe(&record, commit) {
        Ok(kf) => (SlotVerdict::Valid(commit), Some(kf)),
        Err(fault) => (SlotVerdict::Keyframe(fault), None),
    }
}

/// What a region's control slot holds ([`read_control`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlVerdict {
    /// Commit word 0: never written, or being written when the writer died.
    Empty,
    /// The layout has no control slot.
    NoSlot,
    /// A length of zero or past the slot.
    Length(u64),
    /// The slot could not be read.
    Unreadable,
    /// The record is invalid or does not match its commit word.
    Record(ControlFault),
    /// The newest complete record: its sequence number and the window's carry.
    Valid {
        /// The record's sequence number (its commit word).
        seq: u64,
        /// The window's carry, decompressed.
        carry: Vec<u8>,
    },
}

impl ControlVerdict {
    /// The carry, when the slot holds a valid record.
    #[must_use]
    pub fn into_carry(self) -> Option<Vec<u8>> {
        match self {
            Self::Valid { carry, .. } => Some(carry),
            _ => None,
        }
    }
}

/// Read a region's control slot (`records::decode_control`).
#[must_use]
pub fn read_control<V: RegionView + ?Sized>(view: &V, geometry: &Geometry) -> ControlVerdict {
    let cap = geometry.ctl_cap();
    if cap == 0 {
        return ControlVerdict::NoSlot;
    }
    let Some(seq) = load(view, OFF_CTL_COMMIT) else {
        return ControlVerdict::Unreadable;
    };
    if seq == 0 {
        return ControlVerdict::Empty;
    }
    let Some(len) = load(view, OFF_CTL_LEN) else {
        return ControlVerdict::Unreadable;
    };
    let Some(n) = as_usize(len).filter(|n| *n > 0 && *n <= cap) else {
        return ControlVerdict::Length(len);
    };
    let mut record = Vec::new();
    if record.try_reserve_exact(n).is_err() {
        return ControlVerdict::Unreadable;
    }
    record.resize(n, 0);
    if view.read(geometry.ctl_off(), &mut record).is_err() {
        return ControlVerdict::Unreadable;
    }
    match decode_control(&record, seq) {
        Ok(carry) => ControlVerdict::Valid { seq, carry },
        Err(fault) => ControlVerdict::Record(fault),
    }
}

/// What the tail walk did.
#[derive(Debug, Default)]
struct Walk {
    records: u64,
    /// The stream position the engine reflects.
    reached: u64,
    /// The stream position of the tail's last record, as far as the walk read.
    committed: Option<u64>,
    /// The last RESIZE past the keyframe's `tail_from`: `(lifetime, ordinal)`.
    last_resize: Option<(u64, u64)>,
    overflow: bool,
    fault: Option<TailFault>,
    resizes_lost: u64,
    regrids: u64,
    engine_replaced: bool,
    superseded: bool,
}

fn walk_tail<V: RegionView + ?Sized>(
    view: &V,
    geometry: &Geometry,
    keyframe: &KeyframeHead,
    apply: bool,
    engine: &mut Terminal,
) -> Walk {
    let mut walk = Walk {
        reached: keyframe.stream_at,
        ..Walk::default()
    };
    let (Some(commit), Some(floor)) = (load(view, OFF_TAIL_COMMIT), load(view, OFF_TAIL_FLOOR))
    else {
        walk.fault = Some(TailFault::Unreadable);
        return walk;
    };
    let cap = geometry.tail_cap as u64;
    if floor > commit && floor - commit <= cap {
        walk.overflow = true;
        return walk;
    }
    if floor > commit || commit - floor > cap {
        walk.fault = Some(TailFault::PastCommit);
        return walk;
    }
    let from = keyframe.tail_from;
    if from > commit {
        walk.fault = Some(TailFault::Misplaced);
        return walk;
    }
    walk.overflow = from < floor;
    let replay = apply && !walk.overflow;
    let mut pos = floor;
    let mut prev_end: Option<u64> = None;
    let mut started = from == floor;
    while pos < commit {
        if pos == from {
            started = true;
        } else if pos > from && !started && !walk.overflow {
            walk.fault = Some(TailFault::Misplaced);
            return walk;
        }
        let off = pos % cap;
        let room = cap - off;
        if room < TAIL_RECORD_HEADER as u64 {
            let Some(end) = pos.checked_add(room).filter(|end| *end <= commit) else {
                walk.fault = Some(TailFault::PastCommit);
                return walk;
            };
            pos = end;
            continue;
        }
        if pos
            .checked_add(TAIL_RECORD_HEADER as u64)
            .is_none_or(|end| end > commit)
        {
            walk.fault = Some(TailFault::PastCommit);
            return walk;
        }
        let at = geometry.tail_off + usize::try_from(off).unwrap_or(0);
        let mut head = [0u8; TAIL_RECORD_HEADER];
        if view.read(at, &mut head).is_err() {
            walk.fault = Some(TailFault::Unreadable);
            return walk;
        }
        let header = TailHeader::read(&head);
        if header.kind == KIND_PAD {
            if header.record_len() as u64 != room || header.flags != 0 || header.reserved != 0 {
                walk.fault = Some(TailFault::BadPad);
                return walk;
            }
            let Some(end) = pos.checked_add(room).filter(|end| *end <= commit) else {
                walk.fault = Some(TailFault::PastCommit);
                return walk;
            };
            pos = end;
            continue;
        }
        let len = header.record_len() as u64;
        if len > room {
            walk.fault = Some(TailFault::BadLength);
            return walk;
        }
        let Some(end) = pos.checked_add(len).filter(|end| *end <= commit) else {
            walk.fault = Some(TailFault::PastCommit);
            return walk;
        };
        let payload_len = usize::try_from(header.payload_len).unwrap_or(usize::MAX);
        let mut payload = vec![0u8; payload_len];
        if view.read(at + TAIL_RECORD_HEADER, &mut payload).is_err() {
            walk.fault = Some(TailFault::Unreadable);
            return walk;
        }
        let record = match decode_tail_payload(&header, &payload) {
            Ok(record) => record,
            Err(fault) => {
                walk.fault = Some(fault);
                return walk;
            }
        };
        let behind = prev_end.is_some_and(|prev| header.stream_end < prev)
            || (started && header.stream_end < keyframe.stream_at);
        if behind {
            walk.fault = Some(TailFault::StreamBackwards);
            return walk;
        }
        prev_end = Some(header.stream_end);
        walk.committed = Some(header.stream_end);
        if let TailRecord::Resize {
            lifetime, ordinal, ..
        } = record
            && started
        {
            walk.last_resize = Some((lifetime, ordinal));
        }
        if record == TailRecord::Superseded {
            walk.superseded = true;
        }
        if replay && started {
            match record {
                TailRecord::Bytes { bytes, .. } => engine.process(bytes),
                TailRecord::Resize {
                    rows, cols, policy, ..
                } => {
                    if rows == 0 || cols == 0 || rows > MAX_GRID_ROWS || cols > MAX_GRID_COLS {
                        walk.fault = Some(TailFault::BadLength);
                        return walk;
                    }
                    if cols != engine.cols() || rows > engine.rows() {
                        walk.regrids = walk.regrids.saturating_add(1);
                    }
                    engine.resize_with_policy(rows, cols, policy);
                }
                TailRecord::ResizesLost { n } => {
                    walk.resizes_lost = walk.resizes_lost.saturating_add(u64::from(n));
                }
                TailRecord::EngineReplaced => walk.engine_replaced = true,
                TailRecord::Superseded => {}
            }
            walk.records += 1;
            walk.reached = header.stream_end;
        }
        pos = end;
    }
    if !started && !walk.overflow && pos != from {
        walk.fault = Some(TailFault::Misplaced);
    }
    walk
}

fn read_ring<V: RegionView + ?Sized>(view: &V, geometry: &Geometry) -> RingRun {
    let mut run = RingRun::default();
    let words = (
        load(view, OFF_HIST_LO),
        load(view, OFF_HIST_HI),
        load(view, OFF_HIST_LO_SEQ),
        load(view, OFF_HIST_END_ROW),
        load(view, OFF_HIST_CAP).and_then(as_usize),
    );
    let identity = (0..4)
        .map(|i| load(view, OFF_HIST_IDENTITY + 8 * i))
        .collect::<Option<Vec<u64>>>();
    let (
        (Some(lo), Some(hi), Some(lo_seq), Some(end_row), Some(cap)),
        Some(&[renumber_epoch, clear_gen, reveal_gen, cols]),
    ) = (words, identity.as_deref())
    else {
        run.fault = Some(RingFault::Words);
        return run;
    };
    let Ok(cols) = u16::try_from(cols) else {
        run.fault = Some(RingFault::Words);
        return run;
    };
    run.identity = HistoryIdentity {
        renumber_epoch,
        clear_gen,
        reveal_gen,
        cols,
    };
    run.end_row = end_row;
    if lo >= hi {
        return run;
    }
    if cap == 0 || cap > geometry.hist_cap_phys || hi - lo > cap as u64 {
        run.fault = Some(RingFault::Words);
        return run;
    }
    let cap64 = cap as u64;
    let mut pos = lo;
    let mut expected = lo_seq;
    let mut accepted_end = None;
    while pos < hi {
        let off = pos % cap64;
        let Some(ring_end) = pos.checked_add(cap64 - off) else {
            run.fault = Some(RingFault::Words);
            return run;
        };
        let seg_end = hi.min(ring_end);
        let (Some(off_usize), Some(seg_len)) = (as_usize(off), as_usize(seg_end - pos)) else {
            run.fault = Some(RingFault::Words);
            return run;
        };
        let mut bytes = vec![0u8; seg_len];
        if view
            .read(geometry.hist_off + off_usize, &mut bytes)
            .is_err()
        {
            run.fault = Some(RingFault::Words);
            return run;
        }
        let mut scan = scan_steps_from(&bytes, expected);
        if pos == lo
            && let StepEnd::Fault {
                at: 0,
                kind: StepFault::OutOfOrder { found, .. },
            } = scan.end
            && found < expected
        {
            expected = found;
            scan = scan_steps_from(&bytes, expected);
        }
        for (step, range) in &scan.steps {
            let Some(body) = bytes.get(range.clone()) else {
                run.fault = Some(RingFault::Words);
                return run;
            };
            match decode_history_chunk(*step, body) {
                Ok(chunk) => {
                    if chunk.identity != run.identity {
                        run.fault = Some(RingFault::Identity);
                        return run;
                    }
                    let Some(chunk_end) = chunk
                        .first_row
                        .checked_add(u64::from(chunk.n_lines))
                        .filter(|end| *end <= end_row)
                    else {
                        run.fault = Some(RingFault::Rows);
                        return run;
                    };
                    if accepted_end.is_some_and(|end| end != chunk.first_row) {
                        run.fault = Some(RingFault::Rows);
                        return run;
                    }
                    accepted_end = Some(chunk_end);
                    run.end_row = chunk_end;
                    run.chunks.push(chunk);
                }
                Err(fault) => {
                    run.fault = Some(RingFault::Head(fault));
                    return run;
                }
            }
        }
        expected = expected.saturating_add(scan.steps.len() as u64);
        let valid = scan.valid_len as u64;
        if scan.end == StepEnd::Clean {
            pos += valid;
            continue;
        }
        let gap_is_zero = bytes
            .get(scan.valid_len..)
            .is_some_and(|rest| rest.iter().all(|b| *b == 0));
        if seg_end == ring_end && ring_end < hi && gap_is_zero {
            pos = ring_end;
            continue;
        }
        run.fault = Some(RingFault::Chunk(scan.end));
        return run;
    }
    run
}

/// Restore a dead window's session from its region.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CrashRestoreJournal",
        action = "Die",
        reason = "the environment: W dies without a word (a SIGKILL, a jetsam kill, a Force \
                  Quit, a crash) and no code performs it; Tier-1 kills the real writer's \
                  `CountdownStore` after each of its primitive stores and restores what it \
                  left with this fn"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CrashRestoreJournal",
        action = "Poison",
        reason = "a ghost of the restorer: some tail slice kills a restorer that replays it, \
                  and no code makes one; Tier-1 plays it as a full try that dies, with the \
                  keeper's `attempts` brake"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CrashRestoreJournal",
        action = "PoisonKf",
        reason = "a ghost of the restorer: the keyframe itself kills every restorer, and no \
                  code makes one; Tier-1 plays it as every try dying, down to the META rung"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CrashRestoreJournal",
        action = "RestoreFullA",
        project = "aterm_restore::conformance_region::Rig::project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CrashRestoreJournal",
        action = "RestoreFullB",
        project = "aterm_restore::conformance_region::Rig::project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CrashRestoreJournal",
        action = "RestoreScreenA",
        project = "aterm_restore::conformance_region::Rig::project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CrashRestoreJournal",
        action = "RestoreScreenB",
        project = "aterm_restore::conformance_region::Rig::project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CrashRestoreJournal",
        action = "RestoreMeta",
        project = "aterm_restore::conformance_region::Rig::project"
    )
)]
pub fn recover<V: RegionView + ?Sized>(
    view: &V,
    policy: RecoverPolicy,
    make_engine: impl FnOnce(u16, u16) -> Terminal,
) -> Recovery {
    let header = match read_header(view) {
        Ok(header) => header,
        Err(fault) => return Recovery::Meta(MetaWhy::Header(fault)),
    };
    let geometry = header.geometry;
    let (v0, k0) = read_slot(view, &geometry, 0);
    let (v1, k1) = read_slot(view, &geometry, 1);
    let (slot, keyframe, other_slot) = match (k0, k1) {
        (Some(a), Some(b)) if b.head.seq > a.head.seq => (1, b, v0),
        (Some(a), _) => (0, a, v1),
        (None, Some(b)) => (1, b, v0),
        (None, None) => return Recovery::Meta(MetaWhy::NoKeyframe([v0, v1])),
    };

    let mut engine = make_engine(keyframe.checkpoint.rows, keyframe.checkpoint.cols);
    engine.restore_checkpoint(&keyframe.checkpoint);
    if keyframe.head.integration_on {
        engine.authorize_shell_integration(policy.scratch_nonce);
    }
    let fence_before = engine.history_fence();
    let kitty_before = engine.kitty_graphics_commands();
    let unscrolls_before = engine.kitty_unscrolls();

    let walk = walk_tail(view, &geometry, &keyframe.head, policy.tail, &mut engine);
    let graphics_replayed = engine.kitty_graphics_commands().wrapping_sub(kitty_before);
    let unscrolls_replayed = engine.kitty_unscrolls().wrapping_sub(unscrolls_before);

    let fence_after = engine.history_fence();
    let swallowed = engine
        .partial_sequence_uncarried()
        .or_else(|| engine.partial_sequence_swallowed());
    let checkpoint = engine.checkpoint_continuing_swallowing();

    let gathered = load(view, OFF_STREAM_GATHERED).unwrap_or(0);
    let processed_word = load(view, OFF_STREAM_PROCESSED).unwrap_or(0);
    let committed = match (walk.fault, walk.committed) {
        (None, Some(end)) => end,
        (None, None) if walk.overflow => processed_word,
        (None, None) => walk.reached,
        (Some(_), end) => end.unwrap_or(walk.reached).max(processed_word),
    };
    let committed = committed.max(walk.reached);
    let (journaled_lifetime, journaled_ordinal) = walk
        .last_resize
        .unwrap_or((keyframe.head.resize_lifetime, keyframe.head.resize_ordinal));
    let host_lifetime = load(view, OFF_ENGINE_RESIZE_LIFETIME).unwrap_or(0);
    let host_ordinal = load(view, OFF_ENGINE_RESIZE_ORDINAL).unwrap_or(0);
    let resizes_unjournaled = if host_lifetime == 0 {
        0
    } else if host_lifetime == journaled_lifetime {
        host_ordinal.saturating_sub(journaled_ordinal)
    } else {
        host_ordinal.max(1)
    };
    let losses = Losses {
        in_flight_lost: gathered.saturating_sub(committed),
        tail_unreplayed: committed.saturating_sub(walk.reached),
        tail_overflow: walk.overflow,
        tail_fault: walk.fault,
        marks_lost: load(view, OFF_TAIL_MARKS).unwrap_or(0),
        resizes_lost: walk.resizes_lost,
        resizes_unjournaled,
        regrids_replayed: walk.regrids,
        engine_replaced: walk.engine_replaced,
        superseded: walk.superseded,
        graphics_replayed,
        unscrolls_replayed,
        config_moved: header.config_fp != 0 && header.config_fp != keyframe.head.config_fp,
        swallowed,
    };
    let config_ok = keyframe.head.config_fp == policy.config_fp || walk.records == 0;
    let exact = policy.tail && losses == Losses::default() && config_ok;
    let ring = read_ring(view, &geometry);
    Recovery::Restored(Box::new(Restored {
        checkpoint,
        exact,
        slot,
        keyframe: keyframe.head,
        other_slot,
        replayed_records: walk.records,
        losses,
        fence_before,
        carried_lines: fence_after.lines(),
        fence_after,
        ring,
        header,
    }))
}

#[cfg(test)]
#[path = "recover_tests.rs"]
mod tests;
