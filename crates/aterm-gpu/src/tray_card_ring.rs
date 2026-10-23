// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The armed tray card's ring bookkeeping, apart from its textures.

/// How many slots the ring rotates through.
pub const TRAY_CARD_SLOTS: usize = 2;

/// One slot's bookkeeping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrayCardSlot {
    /// Rows `y0, y1)` that differ from the card's bytes: what changed while another slot was shown.
    pub stale: Option<(u32, u32)>,
    /// The present that last sampled it: its number in the arm's present count.
    pub last_read: Option<u64>,
}

/// A change's write, planned by [`TrayCardRing::plan_change`] before anything is touched and landed
/// by [`TrayCardRing::land_change`] once it is done.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrayCardWrite {
    /// The slot to write: a new one while the ring is short, else the one shown least recently.
    pub slot: usize,
    /// Whether [`Self::slot`] is a texture still to mint.
    pub mint: bool,
    /// Settle every in-flight present numbered up to this one (the slot's last reader) before the
    /// write: the presents [`present_may_read`] names.
    pub settle_through: Option<u64>,
    /// The rows `y0, y1)` to upload: the change, and what the slot missed while another was shown.
    pub rows: (u32, u32),
}

/// The ring's bookkeeping: one entry per slot, and the shown one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrayCardRing {
    slots: Vec<TrayCardSlot>,
    shown: usize,
    height: u32,
}

/// Whether the in-flight present numbered `seq` may sample a slot whose last reader was present
/// `last_read`: only one numbered no later, since each present samples the slot shown when it is
/// encoded and a slot records its reader before that present takes its number.
#[must_use]
#[inline]
pub const fn present_may_read(seq: u64, last_read: u64) -> bool {
    seq <= last_read
}

/// The rows `y0, y1)` covering both `a` (if any) and `b`.
#[must_use]
#[inline]
pub fn union_band(a: Option<(u32, u32)>, b: (u32, u32)) -> (u32, u32) {
    a.map_or(b, |(y0, y1)| (y0.min(b.0), y1.max(b.1)))
}

impl TrayCardRing {
    /// A freshly minted ring of one slot, `height` rows, holding the card and shown, read by
    /// `reader` as [`Self::read_unchanged`] says.
    #[must_use]
    #[inline]
    pub fn fresh(height: u32, reader: Option<u64>) -> Self {
        Self {
            slots: vec![TrayCardSlot {
                stale: None,
                last_read: reader,
            }],
            shown: 0,
            height,
        }
    }

    /// The slot the card's draw samples.
    #[must_use]
    #[inline]
    pub const fn shown(&self) -> usize {
        self.shown
    }

    /// Slot `index`'s bookkeeping.
    #[must_use]
    #[inline]
    pub fn slot(&self, index: usize) -> Option<TrayCardSlot> {
        self.slots.get(index).copied()
    }

    /// The card is unchanged and the shown slot is drawn again.
    #[inline]
    pub fn read_unchanged(&mut self, reader: Option<u64>) {
        if let Some(reader) = reader {
            self.slots[self.shown].last_read = Some(reader);
        }
    }

    /// The write a change of rows `band` needs, deciding everything and changing nothing.
    #[must_use]
    #[inline]
    pub fn plan_change(&self, band: (u32, u32)) -> TrayCardWrite {
        if self.slots.len() < TRAY_CARD_SLOTS {
            return TrayCardWrite {
                slot: self.slots.len(),
                mint: true,
                settle_through: None,
                rows: (0, self.height),
            };
        }
        let slot = (self.shown + 1) % self.slots.len();
        let target = self.slots[slot];
        TrayCardWrite {
            slot,
            mint: false,
            settle_through: target.last_read,
            rows: union_band(target.stale, band),
        }
    }

    /// The planned `write` of rows `band` landed.
    #[inline]
    pub fn land_change(&mut self, write: TrayCardWrite, band: (u32, u32), reader: Option<u64>) {
        if write.mint {
            debug_assert_eq!(write.slot, self.slots.len(), "a mint is the next slot");
            self.slots.push(TrayCardSlot {
                stale: None,
                last_read: None,
            });
        }
        for (index, slot) in self.slots.iter_mut().enumerate() {
            slot.stale = if index == write.slot {
                None
            } else {
                Some(union_band(slot.stale, band))
            };
        }
        self.shown = write.slot;
        self.slots[write.slot].last_read = reader;
    }
}
