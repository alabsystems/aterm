// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The region's v1 layout (the crash-restore plan §2.3).
//!
//! A region is [`REGION_LEN`] (8 MiB) of virtual address space, a multiple of the
//! 16 KiB page: macOS rounds a shared-memory object's size up to the page, and only
//! the pages a writer touches are resident. It opens with a 4 KiB header of
//! naturally aligned little-endian `u64` words, then the sections:
//!
//! | offset | size | section | writer |
//! |---|---|---|---|
//! | 0 | 4 KiB | header words | below |
//! | 4,096 | 2 × 384 KiB | keyframe slots 0 and 1 | parse thread |
//! | 790,528 | 256 KiB | tail ring | parse thread |
//! | 1,052,672 | 64 KiB | control slot (increment 7) | main thread |
//! | 1,118,208 | to the end (≈ 6.9 MiB) | history ring | journal worker |
//!
//! The header names every section's offset and size ([`Geometry`]), so a reader
//! never assumes the v1 numbers: it validates what the header says against the
//! region's length. Tests build smaller regions the same way
//! ([`Geometry::custom`]), so the Tier-1 sweep can fill a tail ring with two
//! records.
//!
//! Words written by different threads sit on different 64-byte lines, with one
//! exception the plan's table fixes: `hist_cap` (the worker's, rewritten only at a
//! history reset) shares the line of the host's `config_fp` and resize words, all
//! of which change at human rates. Each word is an atomic `u64`, so the sharing
//! costs a cache line's traffic, never a torn value.
//!
//! The layout version is byte 4 of the magic. Fields only grow, into the reserved
//! words: a reader refuses a version above the one it knows, and ignores reserved
//! words it does not.

/// The magic's first four bytes; byte 4 is the layout version, bytes 5..8 are zero.
pub const MAGIC: [u8; 4] = *b"ATRJ";
/// The layout this build writes and the newest it reads.
pub const LAYOUT_VERSION: u8 = 1;
/// The page every region size is a multiple of (macOS rounds a shm size up to it).
pub const PAGE: usize = 16 * 1024;
/// A v1 region's length: 8 MiB of virtual address space, resident only where touched.
pub const REGION_LEN: usize = 8 * 1024 * 1024;
/// The largest region a reader maps (and the keeper accepts): eight v1 regions.
pub const MAX_REGION_LEN: usize = 64 * 1024 * 1024;
/// The header's length: the words below and reserved zeros to the first section.
pub const HEADER_LEN: usize = 4096;

/// v1 keyframe slot capacity: a 4,096-column keyframe with heavy attributes can
/// overflow it, and the writer then retries with no bridge (the plan's "giant
/// screens" risk); the slot size is a layout constant v2 can raise.
pub const KF_SLOT_CAP_V1: usize = 384 * 1024;
/// v1 tail ring capacity. At ≤ 10 keyframes a second under tail pressure, a
/// program writing under ~2.5 MB/s never overflows it between keyframes.
pub const TAIL_CAP_V1: usize = 256 * 1024;
/// v1 control slot capacity (increment 7's turn-ledger carry).
pub const CTL_CAP_V1: usize = 64 * 1024;

// ---------------------------------------------------------------------------
// Header words: byte offsets of naturally aligned little-endian u64 words.
// ---------------------------------------------------------------------------

/// `b"ATRJ"`, the layout version, three zero bytes. Written last by the creator.
pub const OFF_MAGIC: usize = 0;
/// The region's length in bytes.
pub const OFF_REGION_LEN: usize = 8;
/// The writer's pid (low 32 bits; the high half is zero).
pub const OFF_WRITER_PID: usize = 16;
/// The writer's birth in microseconds (`aterm_uds::process::read_process_birth`).
pub const OFF_WRITER_BIRTH: usize = 24;
/// The session's local id in the writer.
pub const OFF_LOCAL_ID: usize = 32;
/// The writer's build number.
pub const OFF_WRITER_BUILD: usize = 40;
/// The session id, ASCII, NUL-padded: informational (the keeper's offer is
/// authoritative).
pub const OFF_SID: usize = 48;
/// The session id field's length.
pub const SID_LEN: usize = 64;
/// Keyframe slots' offset (slot 1 follows slot 0).
pub const OFF_KF_OFF: usize = 112;
/// One keyframe slot's capacity.
pub const OFF_KF_CAP: usize = 120;
/// Tail ring offset.
pub const OFF_TAIL_OFF: usize = 128;
/// Tail ring capacity.
pub const OFF_TAIL_CAP: usize = 136;
/// History ring offset.
pub const OFF_HIST_OFF: usize = 144;
/// History ring physical capacity.
pub const OFF_HIST_CAP_PHYS: usize = 152;
/// The history ring's logical capacity (its modulus), set at each history reset.
pub const OFF_HIST_CAP: usize = 160;
/// The fingerprint of the engine-relevant config the session runs under.
pub const OFF_CONFIG_FP: usize = 168;
/// The engine's resize lifetime as of the host's last resize (0: none since
/// creation).
pub const OFF_ENGINE_RESIZE_LIFETIME: usize = 176;
/// The engine's resize ordinal as of the host's last resize.
pub const OFF_ENGINE_RESIZE_ORDINAL: usize = 184;
/// The window's cell size in device pixels as of the host's last cell-size change,
/// `width << 16 | height` (0: never set, the engine's 8x16 placeholder). Written by
/// the same host path as `config_fp`, which folds it in. The restorer builds its
/// scratch engine with it: an inline image in the tail advances the cursor by rows
/// its pixel height spans, so a replay under another cell size lays the text out
/// differently, and the restorer's own window is not yet built when it restores.
/// A word the plan's v1 table left reserved: a reader that predates it ignores it.
pub const OFF_CELL_PX: usize = 192;
// The cell size is the host's, in a word no other header field claims: past the host's
// resize words and before the parse thread's first line.
const _: () = assert!(OFF_CELL_PX > OFF_ENGINE_RESIZE_ORDINAL && OFF_CELL_PX + 8 <= OFF_KF_COMMIT);
/// `kf_commit[0]`, `kf_commit[1]`: each slot's keyframe sequence number, 0 when the
/// slot is empty or being written.
pub const OFF_KF_COMMIT: usize = 256;
/// `kf_len[0]`, `kf_len[1]`: each slot's record length.
pub const OFF_KF_LEN: usize = 272;
/// Absolute tail position one past the last complete record.
pub const OFF_TAIL_COMMIT: usize = 320;
/// Absolute tail position below which bytes may already be overwritten (always a
/// record boundary).
pub const OFF_TAIL_FLOOR: usize = 328;
/// PTY bytes processed, as of `tail_commit` (stored just before it).
pub const OFF_STREAM_PROCESSED: usize = 336;
/// Verified OSC 133/633 marks inside the tail since the newest keyframe.
pub const OFF_TAIL_MARKS: usize = 344;
/// PTY bytes the gather thread has read (its own line).
pub const OFF_STREAM_GATHERED: usize = 384;
/// Absolute history-ring position of the oldest chunk.
pub const OFF_HIST_LO: usize = 448;
/// Absolute history-ring position one past the newest chunk: the ring's commit.
pub const OFF_HIST_HI: usize = 456;
/// The step sequence number of the chunk at `hist_lo`.
pub const OFF_HIST_LO_SEQ: usize = 464;
/// The absolute row one past the newest exported line.
pub const OFF_HIST_END_ROW: usize = 472;
/// The ring's identity: `renumber_epoch`, `clear_gen`, `reveal_gen`, `cols`
/// (`aterm_grid::HistoryFence`'s identity half), four words.
pub const OFF_HIST_IDENTITY: usize = 480;
/// `ctl_commit`, `ctl_len` (increment 7).
pub const OFF_CTL_COMMIT: usize = 512;
/// The control slot's record length.
pub const OFF_CTL_LEN: usize = 520;

/// The header word holding slot `slot`'s commit word (`slot` is 0 or 1).
#[must_use]
pub const fn kf_commit_word(slot: usize) -> usize {
    OFF_KF_COMMIT + 8 * (slot & 1)
}

/// The header word holding slot `slot`'s record length.
#[must_use]
pub const fn kf_len_word(slot: usize) -> usize {
    OFF_KF_LEN + 8 * (slot & 1)
}

/// The [`OFF_CELL_PX`] word for a cell size: `width << 16 | height`, 0 for none (a zero
/// axis reads as none too, as the engine clamps one to 1 and a window never reports one).
#[must_use]
pub const fn cell_px_word(px: Option<(u16, u16)>) -> u64 {
    match px {
        Some((w, h)) if w > 0 && h > 0 => (w as u64) << 16 | h as u64,
        _ => 0,
    }
}

/// The inverse of [`cell_px_word`]: `None` for 0, for a zero axis, or for bits above
/// the two halves (a word this build never writes).
#[must_use]
pub const fn cell_px_of(word: u64) -> Option<(u16, u16)> {
    let (w, h) = ((word >> 16) & 0xffff, word & 0xffff);
    if word >> 32 != 0 || w == 0 || h == 0 {
        None
    } else {
        Some((w as u16, h as u16))
    }
}

/// The keyframe slot a sequence number goes to: `seq & 1`. Sequence numbers start
/// at 1 (a commit word of 0 means empty), so the first keyframe lands in slot 1.
#[must_use]
pub const fn slot_of(seq: u64) -> usize {
    (seq & 1) as usize
}

/// Where each section of a region lives: what the header's offset words say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    /// The region's length.
    pub region_len: usize,
    /// Slot 0's offset; slot 1 is at `kf_off + kf_cap`.
    pub kf_off: usize,
    /// One slot's capacity.
    pub kf_cap: usize,
    /// The tail ring's offset.
    pub tail_off: usize,
    /// The tail ring's capacity (its modulus).
    pub tail_cap: usize,
    /// The history ring's offset. The control slot is `[tail_off + tail_cap,
    /// hist_off)`.
    pub hist_off: usize,
    /// The history ring's physical capacity; its logical modulus (`hist_cap`) is at
    /// most this.
    pub hist_cap_phys: usize,
}

/// Why a geometry is not one this build reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutError {
    /// The region length is zero, not a page multiple, or past [`MAX_REGION_LEN`].
    RegionLen(usize),
    /// A section overlaps the header or another section, or runs past the region.
    Overlap,
    /// A section offset or capacity is not a multiple of 8.
    Misaligned,
    /// A section is too small to hold one record.
    TooSmall,
}

impl std::fmt::Display for LayoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RegionLen(len) => write!(f, "a region length no reader maps ({len} bytes)"),
            Self::Overlap => f.write_str("sections that overlap or run past the region"),
            Self::Misaligned => f.write_str("a section that is not 8-byte aligned"),
            Self::TooSmall => f.write_str("a section too small to hold a record"),
        }
    }
}

/// The smallest tail ring: one 16-byte record header and eight payload bytes.
const MIN_TAIL_CAP: usize = 24;
/// The smallest keyframe slot or history ring: a frame header and a little body.
const MIN_SECTION: usize = 64;

impl Geometry {
    /// The v1 layout of a [`REGION_LEN`] region.
    #[must_use]
    pub const fn v1() -> Self {
        let kf_off = HEADER_LEN;
        let tail_off = kf_off + 2 * KF_SLOT_CAP_V1;
        let hist_off = tail_off + TAIL_CAP_V1 + CTL_CAP_V1;
        Self {
            region_len: REGION_LEN,
            kf_off,
            kf_cap: KF_SLOT_CAP_V1,
            tail_off,
            tail_cap: TAIL_CAP_V1,
            hist_off,
            hist_cap_phys: REGION_LEN - hist_off,
        }
    }

    /// A layout with the given section sizes, packed after the header with no
    /// control slot, and the region rounded up to the page: what tests use to fill a
    /// ring with a handful of records.
    ///
    /// # Errors
    ///
    /// When the result does not [`validate`](Self::validate).
    pub fn custom(
        kf_cap: usize,
        tail_cap: usize,
        hist_cap_phys: usize,
    ) -> Result<Self, LayoutError> {
        let kf_off = HEADER_LEN;
        let tail_off = kf_off
            .checked_add(kf_cap.checked_mul(2).ok_or(LayoutError::Overlap)?)
            .ok_or(LayoutError::Overlap)?;
        let hist_off = tail_off.checked_add(tail_cap).ok_or(LayoutError::Overlap)?;
        let end = hist_off
            .checked_add(hist_cap_phys)
            .ok_or(LayoutError::Overlap)?;
        let region_len = end
            .div_ceil(PAGE)
            .checked_mul(PAGE)
            .ok_or(LayoutError::Overlap)?;
        let geometry = Self {
            region_len,
            kf_off,
            kf_cap,
            tail_off,
            tail_cap,
            hist_off,
            hist_cap_phys,
        };
        geometry.validate()?;
        Ok(geometry)
    }

    /// Slot `slot`'s offset.
    #[must_use]
    pub const fn kf_slot_off(&self, slot: usize) -> usize {
        self.kf_off + (slot & 1) * self.kf_cap
    }

    /// The control slot's offset (increment 7).
    #[must_use]
    pub const fn ctl_off(&self) -> usize {
        self.tail_off + self.tail_cap
    }

    /// The control slot's capacity (increment 7).
    #[must_use]
    pub const fn ctl_cap(&self) -> usize {
        self.hist_off.saturating_sub(self.ctl_off())
    }

    /// Check what a header says against the region it is in: every section 8-byte
    /// aligned, after the header, in order, disjoint and inside the region, and
    /// each big enough for one record.
    ///
    /// # Errors
    ///
    /// The first rule broken.
    pub fn validate(&self) -> Result<(), LayoutError> {
        if self.region_len == 0
            || !self.region_len.is_multiple_of(PAGE)
            || self.region_len > MAX_REGION_LEN
        {
            return Err(LayoutError::RegionLen(self.region_len));
        }
        let words = [
            self.kf_off,
            self.kf_cap,
            self.tail_off,
            self.tail_cap,
            self.hist_off,
            self.hist_cap_phys,
        ];
        if words.iter().any(|w| !w.is_multiple_of(8)) {
            return Err(LayoutError::Misaligned);
        }
        if self.kf_cap < MIN_SECTION
            || self.tail_cap < MIN_TAIL_CAP
            || self.hist_cap_phys < MIN_SECTION
        {
            return Err(LayoutError::TooSmall);
        }
        let slots_end = self
            .kf_cap
            .checked_mul(2)
            .and_then(|slots| self.kf_off.checked_add(slots));
        let tail_end = self.tail_off.checked_add(self.tail_cap);
        let hist_end = self.hist_off.checked_add(self.hist_cap_phys);
        match (slots_end, tail_end, hist_end) {
            (Some(slots_end), Some(tail_end), Some(hist_end))
                if self.kf_off >= HEADER_LEN
                    && slots_end <= self.tail_off
                    && tail_end <= self.hist_off
                    && hist_end <= self.region_len =>
            {
                Ok(())
            }
            _ => Err(LayoutError::Overlap),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The v1 numbers are the plan's table, and they validate.
    #[test]
    fn the_v1_layout_is_the_plans_table() {
        let g = Geometry::v1();
        assert_eq!(g.validate(), Ok(()));
        assert_eq!(REGION_LEN % PAGE, 0, "a page multiple");
        assert_eq!(g.kf_off, 4_096);
        assert_eq!(g.tail_off, 790_528);
        assert_eq!(g.ctl_off(), 1_052_672);
        assert_eq!(g.ctl_cap(), 64 * 1024);
        assert_eq!(g.hist_off, 1_118_208);
        assert_eq!(g.hist_cap_phys, REGION_LEN - 1_118_208);
        // Every header word sits inside the header and is aligned.
        for word in [
            OFF_MAGIC,
            OFF_REGION_LEN,
            OFF_WRITER_PID,
            OFF_WRITER_BIRTH,
            OFF_LOCAL_ID,
            OFF_WRITER_BUILD,
            OFF_SID,
            OFF_KF_OFF,
            OFF_KF_CAP,
            OFF_TAIL_OFF,
            OFF_TAIL_CAP,
            OFF_HIST_OFF,
            OFF_HIST_CAP_PHYS,
            OFF_HIST_CAP,
            OFF_CONFIG_FP,
            OFF_ENGINE_RESIZE_LIFETIME,
            OFF_ENGINE_RESIZE_ORDINAL,
            OFF_CELL_PX,
            kf_commit_word(0),
            kf_commit_word(1),
            kf_len_word(0),
            kf_len_word(1),
            OFF_TAIL_COMMIT,
            OFF_TAIL_FLOOR,
            OFF_STREAM_PROCESSED,
            OFF_TAIL_MARKS,
            OFF_STREAM_GATHERED,
            OFF_HIST_LO,
            OFF_HIST_HI,
            OFF_HIST_LO_SEQ,
            OFF_HIST_END_ROW,
            OFF_HIST_IDENTITY + 24,
            OFF_CTL_COMMIT,
            OFF_CTL_LEN,
        ] {
            assert_eq!(word % 8, 0, "{word} is aligned");
            assert!(word + 8 <= HEADER_LEN, "{word} is inside the header");
        }
        assert_eq!(OFF_SID + SID_LEN, OFF_KF_OFF);
        // The parse thread's words, the gather thread's and the worker's are on
        // lines of their own (the 64-byte rule).
        assert_ne!(OFF_TAIL_COMMIT / 64, OFF_STREAM_GATHERED / 64);
        assert_ne!(OFF_STREAM_GATHERED / 64, OFF_HIST_HI / 64);
        assert_ne!(OFF_TAIL_COMMIT / 64, OFF_HIST_HI / 64);
        assert_ne!(OFF_HIST_HI / 64, OFF_CTL_COMMIT / 64);
    }

    /// A cell size round-trips through its word; none, a zero axis and bits this build
    /// never writes read as none.
    #[test]
    fn a_cell_size_round_trips_through_its_word() {
        for px in [(1, 1), (9, 18), (18, 36), (u16::MAX, u16::MAX)] {
            assert_eq!(cell_px_of(cell_px_word(Some(px))), Some(px), "{px:?}");
        }
        assert_eq!(cell_px_word(None), 0);
        assert_eq!(cell_px_word(Some((0, 18))), 0);
        assert_eq!(cell_px_word(Some((9, 0))), 0);
        assert_eq!(cell_px_of(0), None);
        assert_eq!(cell_px_of(9 << 16), None, "a zero height");
        assert_eq!(cell_px_of(18), None, "a zero width");
        assert_eq!(cell_px_of(1 << 32 | 9 << 16 | 18), None, "high bits");
    }

    /// A header naming sections that overlap, run past the region or are not
    /// aligned is refused.
    #[test]
    fn a_geometry_that_does_not_fit_is_refused() {
        let v1 = Geometry::v1();
        let cases = [
            (
                Geometry {
                    region_len: REGION_LEN + 1,
                    ..v1
                },
                LayoutError::RegionLen(REGION_LEN + 1),
            ),
            (
                Geometry {
                    region_len: MAX_REGION_LEN + PAGE,
                    ..v1
                },
                LayoutError::RegionLen(MAX_REGION_LEN + PAGE),
            ),
            (Geometry { kf_off: 8, ..v1 }, LayoutError::Overlap),
            (
                Geometry {
                    tail_off: v1.tail_off - 8,
                    ..v1
                },
                LayoutError::Overlap,
            ),
            (
                Geometry {
                    hist_cap_phys: v1.hist_cap_phys + 8,
                    ..v1
                },
                LayoutError::Overlap,
            ),
            (
                Geometry {
                    tail_cap: TAIL_CAP_V1 + 4,
                    ..v1
                },
                LayoutError::Misaligned,
            ),
            (Geometry { tail_cap: 16, ..v1 }, LayoutError::TooSmall),
            (
                Geometry {
                    hist_off: usize::MAX - 7,
                    ..v1
                },
                LayoutError::Overlap,
            ),
        ];
        for (geometry, refusal) in cases {
            assert_eq!(geometry.validate(), Err(refusal), "{geometry:?}");
        }
        let small = Geometry::custom(2048, 48, 4096).expect("a test region");
        assert_eq!(small.region_len, PAGE);
        assert_eq!(small.ctl_cap(), 0);
    }
}
