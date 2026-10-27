// Copyright (c) 2018 Yevhenii Reizner and the ttf-parser authors
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from ttf-parser 0.25.1 src/aat.rs and modified by the aterm project
// in 2026.
// See ../../LICENSE-MIT and ../../LICENSE-APACHE for the upstream licenses.

//! Apple Advanced Typography state and lookup tables used by `kern`, `kerx`, and `ankr`.

use core::num::NonZeroU16;

use crate::GlyphId;
use crate::parser::{FromData, LazyArray16, NumFrom, Offset, Offset16, Offset32, Stream};

/// Predefined states.
pub(crate) mod state {
    pub(crate) const START_OF_TEXT: u16 = 0;
}

/// Predefined classes.
pub(crate) mod class {
    pub(crate) const END_OF_TEXT: u8 = 0;
    pub(crate) const OUT_OF_BOUNDS: u8 = 1;
    pub(crate) const DELETED_GLYPH: u8 = 2;
}

/// A State Table entry used by legacy and extended AAT tables.
#[derive(Clone, Copy, Debug)]
pub(crate) struct GenericStateEntry<T: FromData> {
    pub(crate) new_state: u16,
    pub(crate) flags: u16,
    pub(crate) extra: T,
}

impl<T: FromData> FromData for GenericStateEntry<T> {
    const SIZE: usize = 4 + T::SIZE;

    #[inline]
    fn parse(data: &[u8]) -> Option<Self> {
        let mut s = Stream::new(data);
        Some(GenericStateEntry {
            new_state: s.read::<u16>()?,
            flags: s.read::<u16>()?,
            extra: s.read::<T>()?,
        })
    }
}

impl<T: FromData> GenericStateEntry<T> {
    /// Checks that entry has an offset (`kern` format 1).
    #[inline]
    pub(crate) const fn has_offset(&self) -> bool {
        self.flags & 0x3FFF != 0
    }

    /// Returns a value offset (`kern` format 1).
    #[inline]
    pub(crate) const fn value_offset(&self) -> ValueOffset {
        ValueOffset(self.flags & 0x3FFF)
    }

    /// If set, reset the kerning data (clear the stack).
    #[inline]
    pub(crate) const fn has_reset(&self) -> bool {
        self.flags & 0x2000 != 0
    }

    /// If set, advance to the next glyph before going to the new state.
    #[inline]
    pub(crate) const fn has_advance(&self) -> bool {
        self.flags & 0x4000 == 0
    }

    /// If set, push this glyph on the kerning stack.
    #[inline]
    pub(crate) const fn has_push(&self) -> bool {
        self.flags & 0x8000 != 0
    }

    /// If set, remember this glyph as the marked glyph (`kerx` format 4).
    #[inline]
    pub(crate) const fn has_mark(&self) -> bool {
        self.flags & 0x8000 != 0
    }
}

/// A legacy state entry used by [`StateTable`].
pub(crate) type StateEntry = GenericStateEntry<()>;

/// A type-safe wrapper for a kerning value offset.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ValueOffset(u16);

impl ValueOffset {
    /// Returns the next offset, wrapping around `u16::MAX`.
    #[inline]
    pub(crate) const fn next(self) -> Self {
        ValueOffset(self.0.wrapping_add(u16::SIZE as u16))
    }
}

/// A legacy AAT state table (`STHeader`), used by `kern` format 1.
#[derive(Clone)]
pub(crate) struct StateTable<'a> {
    number_of_classes: u16,
    first_glyph: GlyphId,
    class_table: &'a [u8],
    state_array_offset: u16,
    state_array: &'a [u8],
    entry_table: &'a [u8],
    actions: &'a [u8],
}

impl<'a> StateTable<'a> {
    pub(crate) fn parse(data: &'a [u8]) -> Option<Self> {
        let mut s = Stream::new(data);
        let number_of_classes = s.read::<u16>()?;
        let class_table_offset = s.read::<Offset16>()?.to_usize();
        let state_array_offset = s.read::<Offset16>()?.to_usize();
        let entry_table_offset = s.read::<Offset16>()?.to_usize();

        let mut s = Stream::new_at(data, class_table_offset)?;
        let first_glyph = s.read::<GlyphId>()?;
        let number_of_glyphs = s.read::<u16>()?;
        let class_table = s.read_bytes(usize::from(number_of_glyphs))?;

        Some(StateTable {
            number_of_classes,
            first_glyph,
            class_table,
            state_array_offset: state_array_offset as u16,
            state_array: data.get(state_array_offset..)?,
            entry_table: data.get(entry_table_offset..)?,
            actions: data,
        })
    }

    /// Returns a glyph class.
    #[inline]
    pub(crate) fn class(&self, glyph_id: GlyphId) -> Option<u8> {
        if glyph_id.0 == 0xFFFF {
            return Some(class::DELETED_GLYPH);
        }
        let idx = glyph_id.0.checked_sub(self.first_glyph.0)?;
        self.class_table.get(usize::from(idx)).copied()
    }

    /// Returns a class entry.
    #[inline]
    pub(crate) fn entry(&self, state: u16, mut class: u8) -> Option<StateEntry> {
        if u16::from(class) >= self.number_of_classes {
            class = class::OUT_OF_BOUNDS;
        }
        let state_idx = usize::from(state)
            .checked_mul(usize::from(self.number_of_classes))?
            .checked_add(usize::from(class))?;
        let entry_idx = self.state_array.get(state_idx)?;
        Stream::read_at(
            self.entry_table,
            usize::from(*entry_idx).checked_mul(StateEntry::SIZE)?,
        )
    }

    /// Returns kerning at offset.
    #[inline]
    pub(crate) fn kerning(&self, offset: ValueOffset) -> Option<i16> {
        Stream::read_at(self.actions, usize::from(offset.0))
    }

    /// Produces a new state index from a byte offset into the subtable.
    #[inline]
    pub(crate) fn new_state(&self, state: u16) -> u16 {
        (i32::from(state) - i32::from(self.state_array_offset))
            .checked_div(i32::from(self.number_of_classes))
            .and_then(|n| u16::try_from(n).ok())
            .unwrap_or(0)
    }
}

impl core::fmt::Debug for StateTable<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "StateTable {{ ... }}")
    }
}

/// An Extended State Table (`STXHeader`), used by `kerx`.
#[derive(Clone)]
pub(crate) struct ExtendedStateTable<'a, T> {
    number_of_classes: u32,
    lookup: Lookup<'a>,
    state_array: &'a [u8],
    entry_table: &'a [u8],
    entry_type: core::marker::PhantomData<T>,
}

impl<'a, T: FromData> ExtendedStateTable<'a, T> {
    /// Parses an Extended State Table from a stream.
    pub(crate) fn parse(number_of_glyphs: NonZeroU16, s: &mut Stream<'a>) -> Option<Self> {
        let data = s.tail()?;
        let number_of_classes = s.read::<u32>()?;
        let lookup_table_offset = s.read::<Offset32>()?.to_usize();
        let state_array_offset = s.read::<Offset32>()?.to_usize();
        let entry_table_offset = s.read::<Offset32>()?.to_usize();

        Some(ExtendedStateTable {
            number_of_classes,
            lookup: Lookup::parse(number_of_glyphs, data.get(lookup_table_offset..)?)?,
            state_array: data.get(state_array_offset..)?,
            entry_table: data.get(entry_table_offset..)?,
            entry_type: core::marker::PhantomData,
        })
    }

    /// Returns a glyph class.
    #[inline]
    pub(crate) fn class(&self, glyph_id: GlyphId) -> Option<u16> {
        if glyph_id.0 == 0xFFFF {
            return Some(u16::from(class::DELETED_GLYPH));
        }
        self.lookup.value(glyph_id)
    }

    /// Returns a class entry.
    #[inline]
    pub(crate) fn entry(&self, state: u16, mut class: u16) -> Option<GenericStateEntry<T>> {
        if u32::from(class) >= self.number_of_classes {
            class = u16::from(class::OUT_OF_BOUNDS);
        }
        let state_idx = usize::from(state)
            .checked_mul(usize::num_from(self.number_of_classes))?
            .checked_add(usize::from(class))?;
        let entry_idx: u16 = Stream::read_at(self.state_array, state_idx.checked_mul(u16::SIZE)?)?;
        Stream::read_at(
            self.entry_table,
            usize::from(entry_idx).checked_mul(GenericStateEntry::<T>::SIZE)?,
        )
    }
}

impl<T> core::fmt::Debug for ExtendedStateTable<'_, T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "ExtendedStateTable {{ ... }}")
    }
}

/// An AAT lookup table (`u32` values in Format 10 are truncated to `u16`).
#[derive(Clone)]
pub(crate) struct Lookup<'a> {
    data: LookupInner<'a>,
}

impl<'a> Lookup<'a> {
    /// Parses a lookup table from raw data.
    #[inline]
    pub(crate) fn parse(number_of_glyphs: NonZeroU16, data: &'a [u8]) -> Option<Self> {
        LookupInner::parse(number_of_glyphs, data).map(|data| Self { data })
    }

    /// Returns a value associated with the specified glyph.
    #[inline]
    pub(crate) fn value(&self, glyph_id: GlyphId) -> Option<u16> {
        self.data.value(glyph_id)
    }
}

impl core::fmt::Debug for Lookup<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Lookup {{ ... }}")
    }
}

#[derive(Clone)]
enum LookupInner<'a> {
    Format0(LazyArray16<'a, u16>),
    Format2(BinarySearchTable<'a, LookupSegment>),
    Format4(BinarySearchTable<'a, LookupSegment>, &'a [u8]),
    Format6(BinarySearchTable<'a, LookupSingle>),
    Format8 {
        first_glyph: u16,
        values: LazyArray16<'a, u16>,
    },
    Format10 {
        value_size: u16,
        first_glyph: u16,
        glyph_count: u16,
        data: &'a [u8],
    },
}

impl<'a> LookupInner<'a> {
    fn parse(number_of_glyphs: NonZeroU16, data: &'a [u8]) -> Option<Self> {
        let mut s = Stream::new(data);
        let format = s.read::<u16>()?;
        match format {
            0 => {
                let values = s.read_array16::<u16>(number_of_glyphs.get())?;
                Some(Self::Format0(values))
            }
            2 => {
                let bsearch = BinarySearchTable::<LookupSegment>::parse(s.tail()?)?;
                Some(Self::Format2(bsearch))
            }
            4 => {
                let bsearch = BinarySearchTable::<LookupSegment>::parse(s.tail()?)?;
                Some(Self::Format4(bsearch, data))
            }
            6 => {
                let bsearch = BinarySearchTable::<LookupSingle>::parse(s.tail()?)?;
                Some(Self::Format6(bsearch))
            }
            8 => {
                let first_glyph = s.read::<u16>()?;
                let glyph_count = s.read::<u16>()?;
                let values = s.read_array16::<u16>(glyph_count)?;
                Some(Self::Format8 {
                    first_glyph,
                    values,
                })
            }
            10 => {
                let value_size = s.read::<u16>()?;
                let first_glyph = s.read::<u16>()?;
                let glyph_count = s.read::<u16>()?;
                Some(Self::Format10 {
                    value_size,
                    first_glyph,
                    glyph_count,
                    data: s.tail()?,
                })
            }
            _ => None,
        }
    }

    fn value(&self, glyph_id: GlyphId) -> Option<u16> {
        match self {
            Self::Format0(values) => values.get(glyph_id.0),
            Self::Format2(bsearch) => bsearch.get(glyph_id).map(|v| v.value),
            Self::Format4(bsearch, data) => {
                let segment = bsearch.get(glyph_id)?;
                let index = glyph_id.0.checked_sub(segment.first_glyph)?;
                let offset = usize::from(segment.value)
                    .checked_add(u16::SIZE.checked_mul(usize::from(index))?)?;
                Stream::read_at::<u16>(data, offset)
            }
            Self::Format6(bsearch) => bsearch.get(glyph_id).map(|v| v.value),
            Self::Format8 {
                first_glyph,
                values,
            } => {
                let idx = glyph_id.0.checked_sub(*first_glyph)?;
                values.get(idx)
            }
            Self::Format10 {
                value_size,
                first_glyph,
                glyph_count,
                data,
            } => {
                let idx = glyph_id.0.checked_sub(*first_glyph)?;
                let mut s = Stream::new(data);
                match value_size {
                    1 => s.read_array16::<u8>(*glyph_count)?.get(idx).map(u16::from),
                    2 => s.read_array16::<u16>(*glyph_count)?.get(idx),
                    4 => s
                        .read_array16::<u32>(*glyph_count)?
                        .get(idx)
                        .map(|n| n as u16),
                    _ => None,
                }
            }
        }
    }
}

#[derive(Clone)]
struct BinarySearchTable<'a, T: BinarySearchValue> {
    values: LazyArray16<'a, T>,
    len: NonZeroU16,
}

impl<'a, T: BinarySearchValue + core::fmt::Debug> BinarySearchTable<'a, T> {
    #[inline(never)]
    fn parse(data: &'a [u8]) -> Option<Self> {
        let mut s = Stream::new(data);
        let segment_size = s.read::<u16>()?;
        let number_of_segments = s.read::<u16>()?;
        s.advance(6);

        if usize::from(segment_size) != T::SIZE || number_of_segments == 0 {
            return None;
        }

        let values = s.read_array16::<T>(number_of_segments)?;
        let mut len = number_of_segments;
        if values.last()?.is_termination() {
            len = len.checked_sub(1)?;
        }

        Some(BinarySearchTable {
            len: NonZeroU16::new(len)?,
            values,
        })
    }

    fn get(&self, key: GlyphId) -> Option<T> {
        let mut min = 0i32;
        let mut max = i32::from(self.len.get()) - 1;
        while min <= max {
            let mid = (min + max) / 2;
            let v = self.values.get(mid as u16)?;
            match v.contains(key) {
                core::cmp::Ordering::Less => max = mid - 1,
                core::cmp::Ordering::Greater => min = mid + 1,
                core::cmp::Ordering::Equal => return Some(v),
            }
        }
        None
    }
}

trait BinarySearchValue: FromData {
    fn is_termination(&self) -> bool;
    fn contains(&self, glyph_id: GlyphId) -> core::cmp::Ordering;
}

#[derive(Clone, Copy, Debug)]
struct LookupSegment {
    last_glyph: u16,
    first_glyph: u16,
    value: u16,
}

impl FromData for LookupSegment {
    const SIZE: usize = 6;

    #[inline]
    fn parse(data: &[u8]) -> Option<Self> {
        let mut s = Stream::new(data);
        Some(LookupSegment {
            last_glyph: s.read::<u16>()?,
            first_glyph: s.read::<u16>()?,
            value: s.read::<u16>()?,
        })
    }
}

impl BinarySearchValue for LookupSegment {
    #[inline]
    fn is_termination(&self) -> bool {
        self.last_glyph == 0xFFFF && self.first_glyph == 0xFFFF
    }

    #[inline]
    fn contains(&self, id: GlyphId) -> core::cmp::Ordering {
        if id.0 < self.first_glyph {
            core::cmp::Ordering::Less
        } else if id.0 <= self.last_glyph {
            core::cmp::Ordering::Equal
        } else {
            core::cmp::Ordering::Greater
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct LookupSingle {
    glyph: u16,
    value: u16,
}

impl FromData for LookupSingle {
    const SIZE: usize = 4;

    #[inline]
    fn parse(data: &[u8]) -> Option<Self> {
        let mut s = Stream::new(data);
        Some(LookupSingle {
            glyph: s.read::<u16>()?,
            value: s.read::<u16>()?,
        })
    }
}

impl BinarySearchValue for LookupSingle {
    #[inline]
    fn is_termination(&self) -> bool {
        self.glyph == 0xFFFF
    }

    #[inline]
    fn contains(&self, id: GlyphId) -> core::cmp::Ordering {
        id.0.cmp(&self.glyph)
    }
}
