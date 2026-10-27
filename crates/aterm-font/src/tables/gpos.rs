// Copyright (c) 2018 Yevhenii Reizner and the ttf-parser authors
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from ttf-parser 0.25.1 src/tables/gpos.rs and modified by the aterm
// project in 2026.
// See ../../LICENSE-MIT and ../../LICENSE-APACHE for the upstream licenses.

//! OpenType Glyph Positioning (`GPOS`) table implementation.

use core::convert::TryFrom;

use crate::GlyphId;
use crate::ggg::{
    ChainedContextLookup, Class, ClassDefinition, ContextLookup, Coverage, LookupSubtable,
};
use crate::parser::{
    FromData, FromSlice, LazyArray16, LazyArray32, NumFrom, Offset, Offset16, Stream,
};

/// OpenType Device Table hinting values.
#[derive(Clone, Copy)]
pub struct HintingDevice<'a> {
    start_size: u16,
    end_size: u16,
    delta_format: u16,
    delta_values: LazyArray16<'a, u16>,
}

impl HintingDevice<'_> {
    /// Returns X-axis delta.
    #[inline]
    pub fn x_delta(&self, units_per_em: u16, pixels_per_em: Option<(u16, u16)>) -> Option<i32> {
        let ppem = pixels_per_em.map(|(x, _)| x)?;
        self.get_delta(ppem, units_per_em)
    }

    /// Returns Y-axis delta.
    #[inline]
    pub fn y_delta(&self, units_per_em: u16, pixels_per_em: Option<(u16, u16)>) -> Option<i32> {
        let ppem = pixels_per_em.map(|(_, y)| y)?;
        self.get_delta(ppem, units_per_em)
    }

    fn get_delta(&self, ppem: u16, scale: u16) -> Option<i32> {
        let f = self.delta_format;
        debug_assert!(matches!(f, 1..=3));

        if ppem == 0 || ppem < self.start_size || ppem > self.end_size {
            return None;
        }

        let s = ppem - self.start_size;
        let byte = self.delta_values.get(s >> (4 - f))?;
        let bits = byte >> (16 - (((s & ((1 << (4 - f)) - 1)) + 1) << f));
        let mask = 0xFFFF >> (16 - (1 << f));

        let mut delta = i64::from(bits & mask);
        if delta >= i64::from((mask + 1) >> 1) {
            delta -= i64::from(mask + 1);
        }

        i32::try_from(delta * i64::from(scale) / i64::from(ppem)).ok()
    }
}

impl core::fmt::Debug for HintingDevice<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "HintingDevice {{ ... }}")
    }
}

/// VariationIndex table indexing into `GDEF`'s `ItemVariationStore`.
#[allow(missing_docs)]
#[derive(Clone, Copy, Debug)]
pub struct VariationDevice {
    pub outer_index: u16,
    pub inner_index: u16,
}

/// OpenType Device or VariationIndex table.
#[allow(missing_docs)]
#[derive(Clone, Copy, Debug)]
pub enum Device<'a> {
    Hinting(HintingDevice<'a>),
    Variation(VariationDevice),
}

impl<'a> Device<'a> {
    pub(crate) fn parse(data: &'a [u8]) -> Option<Self> {
        let mut s = Stream::new(data);
        let first = s.read::<u16>()?;
        let second = s.read::<u16>()?;
        let format = s.read::<u16>()?;
        match format {
            1..=3 => {
                let start_size = first;
                let end_size = second;
                let span = end_size.checked_sub(start_size)?;
                let count = (span >> (4 - format)) + 1;
                let delta_values = s.read_array16(count)?;
                Some(Self::Hinting(HintingDevice {
                    start_size,
                    end_size,
                    delta_format: format,
                    delta_values,
                }))
            }
            0x8000 => Some(Self::Variation(VariationDevice {
                outer_index: first,
                inner_index: second,
            })),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Default, Debug)]
struct ValueFormatFlags(u8);

impl ValueFormatFlags {
    #[inline]
    const fn x_placement(self) -> bool {
        self.0 & 0x01 != 0
    }
    #[inline]
    const fn y_placement(self) -> bool {
        self.0 & 0x02 != 0
    }
    #[inline]
    const fn x_advance(self) -> bool {
        self.0 & 0x04 != 0
    }
    #[inline]
    const fn y_advance(self) -> bool {
        self.0 & 0x08 != 0
    }
    #[inline]
    const fn x_placement_device(self) -> bool {
        self.0 & 0x10 != 0
    }
    #[inline]
    const fn y_placement_device(self) -> bool {
        self.0 & 0x20 != 0
    }
    #[inline]
    const fn x_advance_device(self) -> bool {
        self.0 & 0x40 != 0
    }
    #[inline]
    const fn y_advance_device(self) -> bool {
        self.0 & 0x80 != 0
    }

    fn size(self) -> usize {
        u16::SIZE * usize::num_from(self.0.count_ones())
    }
}

impl FromData for ValueFormatFlags {
    const SIZE: usize = 2;

    #[inline]
    fn parse(data: &[u8]) -> Option<Self> {
        // The high 8 bits of ValueFormat are reserved/unused in GPOS 1.0/1.1.
        data.get(1).copied().map(Self)
    }
}

/// OpenType `GPOS` ValueRecord.
#[derive(Clone, Copy, Default, Debug)]
pub struct ValueRecord<'a> {
    /// Horizontal adjustment for placement, in design units.
    pub x_placement: i16,
    /// Vertical adjustment for placement, in design units.
    pub y_placement: i16,
    /// Horizontal adjustment for advance, in design units — only used for horizontal layout.
    pub x_advance: i16,
    /// Vertical adjustment for advance, in design units — only used for vertical layout.
    pub y_advance: i16,

    /// A [`Device`] table with horizontal adjustment for placement.
    pub x_placement_device: Option<Device<'a>>,
    /// A [`Device`] table with vertical adjustment for placement.
    pub y_placement_device: Option<Device<'a>>,
    /// A [`Device`] table with horizontal adjustment for advance.
    pub x_advance_device: Option<Device<'a>>,
    /// A [`Device`] table with vertical adjustment for advance.
    pub y_advance_device: Option<Device<'a>>,
}

impl<'a> ValueRecord<'a> {
    fn parse(
        table_data: &'a [u8],
        s: &mut Stream,
        flags: ValueFormatFlags,
    ) -> Option<ValueRecord<'a>> {
        let mut record = ValueRecord::default();

        if flags.x_placement() {
            record.x_placement = s.read::<i16>()?;
        }
        if flags.y_placement() {
            record.y_placement = s.read::<i16>()?;
        }
        if flags.x_advance() {
            record.x_advance = s.read::<i16>()?;
        }
        if flags.y_advance() {
            record.y_advance = s.read::<i16>()?;
        }
        if flags.x_placement_device() {
            if let Some(offset) = s.read::<Option<Offset16>>()? {
                record.x_placement_device =
                    table_data.get(offset.to_usize()..).and_then(Device::parse);
            }
        }
        if flags.y_placement_device() {
            if let Some(offset) = s.read::<Option<Offset16>>()? {
                record.y_placement_device =
                    table_data.get(offset.to_usize()..).and_then(Device::parse);
            }
        }
        if flags.x_advance_device() {
            if let Some(offset) = s.read::<Option<Offset16>>()? {
                record.x_advance_device =
                    table_data.get(offset.to_usize()..).and_then(Device::parse);
            }
        }
        if flags.y_advance_device() {
            if let Some(offset) = s.read::<Option<Offset16>>()? {
                record.y_advance_device =
                    table_data.get(offset.to_usize()..).and_then(Device::parse);
            }
        }

        Some(record)
    }
}

/// Packed array of [`ValueRecord`] entries.
#[derive(Clone, Copy)]
pub struct ValueRecordsArray<'a> {
    table_data: &'a [u8],
    data: &'a [u8],
    len: u16,
    value_len: usize,
    flags: ValueFormatFlags,
}

impl<'a> ValueRecordsArray<'a> {
    fn parse(
        table_data: &'a [u8],
        count: u16,
        flags: ValueFormatFlags,
        s: &mut Stream<'a>,
    ) -> Option<Self> {
        let value_len = flags.size();
        let bytes = usize::from(count).checked_mul(value_len)?;
        Some(Self {
            table_data,
            flags,
            len: count,
            value_len,
            data: s.read_bytes(bytes)?,
        })
    }

    /// Returns array's length.
    #[inline]
    pub const fn len(&self) -> u16 {
        self.len
    }

    /// Checks if the array is empty.
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns a [`ValueRecord`] at index.
    pub fn get(&self, index: u16) -> Option<ValueRecord<'a>> {
        if index >= self.len {
            return None;
        }
        let start = usize::from(index).checked_mul(self.value_len)?;
        let end = start.checked_add(self.value_len)?;
        let data = self.data.get(start..end)?;
        let mut s = Stream::new(data);
        ValueRecord::parse(self.table_data, &mut s, self.flags)
    }
}

impl core::fmt::Debug for ValueRecordsArray<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "ValueRecordsArray {{ ... }}")
    }
}

/// OpenType Single Adjustment Positioning subtable (`GPOS` LookupType 1).
#[allow(missing_docs)]
#[derive(Clone, Copy, Debug)]
pub enum SingleAdjustment<'a> {
    Format1 {
        coverage: Coverage<'a>,
        value: ValueRecord<'a>,
    },
    Format2 {
        coverage: Coverage<'a>,
        values: ValueRecordsArray<'a>,
    },
}

impl<'a> SingleAdjustment<'a> {
    fn parse(data: &'a [u8]) -> Option<Self> {
        let mut s = Stream::new(data);
        match s.read::<u16>()? {
            1 => {
                let coverage = Coverage::parse(s.read_at_offset16(data)?)?;
                let flags = s.read::<ValueFormatFlags>()?;
                let value = ValueRecord::parse(data, &mut s, flags)?;
                Some(Self::Format1 { coverage, value })
            }
            2 => {
                let coverage = Coverage::parse(s.read_at_offset16(data)?)?;
                let flags = s.read::<ValueFormatFlags>()?;
                let count = s.read::<u16>()?;
                let values = ValueRecordsArray::parse(data, count, flags, &mut s)?;
                Some(Self::Format2 { coverage, values })
            }
            _ => None,
        }
    }

    /// Returns the subtable coverage.
    #[inline]
    pub fn coverage(&self) -> Coverage<'a> {
        match self {
            Self::Format1 { coverage, .. } => *coverage,
            Self::Format2 { coverage, .. } => *coverage,
        }
    }
}

/// A [`ValueRecord`] pairs set used by [`PairAdjustment`].
#[derive(Clone, Copy)]
pub struct PairSet<'a> {
    table_data: &'a [u8],
    records: &'a [u8],
    flags: (ValueFormatFlags, ValueFormatFlags),
    record_len: u8,
}

impl<'a> PairSet<'a> {
    fn parse(data: &'a [u8], flags: (ValueFormatFlags, ValueFormatFlags)) -> Option<Self> {
        let mut s = Stream::new(data);
        let count = s.read::<u16>()?;
        // Max len is 34, so u8 is enough.
        let record_len = (GlyphId::SIZE + flags.0.size() + flags.1.size()) as u8;
        let bytes = usize::from(count).checked_mul(usize::from(record_len))?;
        let records = s.read_bytes(bytes)?;
        Some(Self {
            table_data: data,
            records,
            flags,
            record_len,
        })
    }

    #[inline]
    fn binary_search(&self, second: GlyphId) -> Option<&'a [u8]> {
        let step = usize::from(self.record_len);
        let mut size = self.records.len() / step;
        if size == 0 {
            return None;
        }

        let get_record = |index: usize| {
            let start = index.checked_mul(step)?;
            let end = start.checked_add(step)?;
            self.records.get(start..end)
        };

        let get_glyph = |record: &[u8]| GlyphId::parse(record.get(..GlyphId::SIZE)?);

        let mut base = 0;
        while size > 1 {
            let half = size / 2;
            let mid = base + half;
            let cmp = get_glyph(get_record(mid)?)?.cmp(&second);
            if cmp != core::cmp::Ordering::Greater {
                base = mid;
            }
            size -= half;
        }

        let value = get_record(base)?;
        if get_glyph(value)? == second {
            Some(value)
        } else {
            None
        }
    }

    /// Returns a [`ValueRecord`] pair using the second glyph.
    pub fn get(&self, second: GlyphId) -> Option<(ValueRecord<'a>, ValueRecord<'a>)> {
        let record_data = self.binary_search(second)?;
        let mut s = Stream::new(record_data);
        s.skip::<GlyphId>();
        Some((
            ValueRecord::parse(self.table_data, &mut s, self.flags.0)?,
            ValueRecord::parse(self.table_data, &mut s, self.flags.1)?,
        ))
    }
}

impl core::fmt::Debug for PairSet<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "PairSet {{ ... }}")
    }
}

/// A list of [`PairSet`] tables.
#[derive(Clone, Copy)]
pub struct PairSets<'a> {
    data: &'a [u8],
    offsets: LazyArray16<'a, Option<Offset16>>,
    flags: (ValueFormatFlags, ValueFormatFlags),
}

impl<'a> PairSets<'a> {
    const fn new(
        data: &'a [u8],
        offsets: LazyArray16<'a, Option<Offset16>>,
        flags: (ValueFormatFlags, ValueFormatFlags),
    ) -> Self {
        Self {
            data,
            offsets,
            flags,
        }
    }

    /// Returns a value at `index`.
    #[inline]
    pub fn get(&self, index: u16) -> Option<PairSet<'a>> {
        let offset = self.offsets.get(index)??.to_usize();
        self.data
            .get(offset..)
            .and_then(|data| PairSet::parse(data, self.flags))
    }

    /// Returns array's length.
    #[inline]
    pub const fn len(&self) -> u16 {
        self.offsets.len()
    }

    /// Checks if the array is empty.
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.offsets.is_empty()
    }

    /// Checks whether pair records in this subtable carry [`Device`] or variation-index offsets.
    #[inline]
    pub const fn has_device_tables(&self) -> bool {
        (self.flags.0.0 | self.flags.1.0) & 0xF0 != 0
    }
}

impl core::fmt::Debug for PairSets<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "PairSets {{ ... }}")
    }
}

/// A [`ValueRecord`] pairs matrix used by [`PairAdjustment`].
#[derive(Clone, Copy)]
pub struct ClassMatrix<'a> {
    table_data: &'a [u8],
    matrix: &'a [u8],
    counts: (u16, u16),
    flags: (ValueFormatFlags, ValueFormatFlags),
    record_len: u8,
}

impl<'a> ClassMatrix<'a> {
    fn parse(
        table_data: &'a [u8],
        counts: (u16, u16),
        flags: (ValueFormatFlags, ValueFormatFlags),
        s: &mut Stream<'a>,
    ) -> Option<Self> {
        let count = usize::num_from(u32::from(counts.0) * u32::from(counts.1));
        // Max len is 32, so u8 is enough.
        let record_len = (flags.0.size() + flags.1.size()) as u8;
        let bytes = count.checked_mul(usize::from(record_len))?;
        let matrix = s.read_bytes(bytes)?;
        Some(Self {
            table_data,
            matrix,
            counts,
            flags,
            record_len,
        })
    }

    /// Returns a [`ValueRecord`] pair using specified classes.
    pub fn get(&self, classes: (u16, u16)) -> Option<(ValueRecord<'a>, ValueRecord<'a>)> {
        if classes.0 >= self.counts.0 || classes.1 >= self.counts.1 {
            return None;
        }

        let idx =
            usize::num_from(u32::from(classes.0) * u32::from(self.counts.1) + u32::from(classes.1));
        let start = idx.checked_mul(usize::from(self.record_len))?;
        let end = start.checked_add(usize::from(self.record_len))?;
        let record = self.matrix.get(start..end)?;

        let mut s = Stream::new(record);
        Some((
            ValueRecord::parse(self.table_data, &mut s, self.flags.0)?,
            ValueRecord::parse(self.table_data, &mut s, self.flags.1)?,
        ))
    }
}

impl core::fmt::Debug for ClassMatrix<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "ClassMatrix {{ ... }}")
    }
}

/// OpenType Pair Adjustment Positioning subtable (`GPOS` LookupType 2).
#[allow(missing_docs)]
#[derive(Clone, Copy, Debug)]
pub enum PairAdjustment<'a> {
    Format1 {
        coverage: Coverage<'a>,
        sets: PairSets<'a>,
    },
    Format2 {
        coverage: Coverage<'a>,
        classes: (ClassDefinition<'a>, ClassDefinition<'a>),
        matrix: ClassMatrix<'a>,
    },
}

impl<'a> PairAdjustment<'a> {
    fn parse(data: &'a [u8]) -> Option<Self> {
        let mut s = Stream::new(data);
        match s.read::<u16>()? {
            1 => {
                let coverage = Coverage::parse(s.read_at_offset16(data)?)?;
                let flags = (s.read::<ValueFormatFlags>()?, s.read::<ValueFormatFlags>()?);
                let count = s.read::<u16>()?;
                let offsets = s.read_array16(count)?;
                Some(Self::Format1 {
                    coverage,
                    sets: PairSets::new(data, offsets, flags),
                })
            }
            2 => {
                let coverage = Coverage::parse(s.read_at_offset16(data)?)?;
                let flags = (s.read::<ValueFormatFlags>()?, s.read::<ValueFormatFlags>()?);
                let classes = (
                    ClassDefinition::parse(s.read_at_offset16(data)?)?,
                    ClassDefinition::parse(s.read_at_offset16(data)?)?,
                );
                let counts = (s.read::<u16>()?, s.read::<u16>()?);
                Some(Self::Format2 {
                    coverage,
                    classes,
                    matrix: ClassMatrix::parse(data, counts, flags, &mut s)?,
                })
            }
            _ => None,
        }
    }

    /// Returns the subtable coverage.
    #[inline]
    pub fn coverage(&self) -> Coverage<'a> {
        match self {
            Self::Format1 { coverage, .. } => *coverage,
            Self::Format2 { coverage, .. } => *coverage,
        }
    }
}

#[derive(Clone, Copy)]
struct EntryExitRecord {
    entry_anchor_offset: Option<Offset16>,
    exit_anchor_offset: Option<Offset16>,
}

impl FromData for EntryExitRecord {
    const SIZE: usize = 4;

    #[inline]
    fn parse(data: &[u8]) -> Option<Self> {
        let mut s = Stream::new(data);
        Some(Self {
            entry_anchor_offset: s.read::<Option<Offset16>>()?,
            exit_anchor_offset: s.read::<Option<Offset16>>()?,
        })
    }
}

/// A list of entry and exit [`Anchor`] pairs.
#[derive(Clone, Copy)]
pub struct CursiveAnchorSet<'a> {
    data: &'a [u8],
    records: LazyArray16<'a, EntryExitRecord>,
}

impl<'a> CursiveAnchorSet<'a> {
    /// Returns an entry [`Anchor`] at index.
    pub fn entry(&self, index: u16) -> Option<Anchor<'a>> {
        let offset = self.records.get(index)?.entry_anchor_offset?.to_usize();
        self.data.get(offset..).and_then(Anchor::parse)
    }

    /// Returns an exit [`Anchor`] at index.
    pub fn exit(&self, index: u16) -> Option<Anchor<'a>> {
        let offset = self.records.get(index)?.exit_anchor_offset?.to_usize();
        self.data.get(offset..).and_then(Anchor::parse)
    }

    /// Returns the number of items.
    #[inline]
    pub const fn len(&self) -> u16 {
        self.records.len()
    }

    /// Checks if the set is empty.
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

impl core::fmt::Debug for CursiveAnchorSet<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "CursiveAnchorSet {{ ... }}")
    }
}

/// OpenType Cursive Attachment Positioning subtable (`GPOS` LookupType 3).
#[allow(missing_docs)]
#[derive(Clone, Copy, Debug)]
pub struct CursiveAdjustment<'a> {
    pub coverage: Coverage<'a>,
    pub sets: CursiveAnchorSet<'a>,
}

impl<'a> CursiveAdjustment<'a> {
    fn parse(data: &'a [u8]) -> Option<Self> {
        let mut s = Stream::new(data);
        match s.read::<u16>()? {
            1 => {
                let coverage = Coverage::parse(s.read_at_offset16(data)?)?;
                let count = s.read::<u16>()?;
                let records = s.read_array16(count)?;
                Some(Self {
                    coverage,
                    sets: CursiveAnchorSet { data, records },
                })
            }
            _ => None,
        }
    }
}

/// OpenType Mark-to-Base Attachment Positioning subtable (`GPOS` LookupType 4).
#[derive(Clone, Copy, Debug)]
pub struct MarkToBaseAdjustment<'a> {
    /// A mark coverage.
    pub mark_coverage: Coverage<'a>,
    /// A base coverage.
    pub base_coverage: Coverage<'a>,
    /// A list of mark anchors.
    pub marks: MarkArray<'a>,
    /// An anchors matrix.
    pub anchors: AnchorMatrix<'a>,
}

impl<'a> MarkToBaseAdjustment<'a> {
    fn parse(data: &'a [u8]) -> Option<Self> {
        let mut s = Stream::new(data);
        match s.read::<u16>()? {
            1 => {
                let mark_coverage = Coverage::parse(s.read_at_offset16(data)?)?;
                let base_coverage = Coverage::parse(s.read_at_offset16(data)?)?;
                let class_count = s.read::<u16>()?;
                let marks = MarkArray::parse(s.read_at_offset16(data)?)?;
                let anchors = AnchorMatrix::parse(s.read_at_offset16(data)?, class_count)?;
                Some(Self {
                    mark_coverage,
                    base_coverage,
                    marks,
                    anchors,
                })
            }
            _ => None,
        }
    }
}

/// OpenType Mark-to-Ligature Attachment Positioning subtable (`GPOS` LookupType 5).
#[allow(missing_docs)]
#[derive(Clone, Copy, Debug)]
pub struct MarkToLigatureAdjustment<'a> {
    pub mark_coverage: Coverage<'a>,
    pub ligature_coverage: Coverage<'a>,
    pub marks: MarkArray<'a>,
    pub ligature_array: LigatureArray<'a>,
}

impl<'a> MarkToLigatureAdjustment<'a> {
    fn parse(data: &'a [u8]) -> Option<Self> {
        let mut s = Stream::new(data);
        match s.read::<u16>()? {
            1 => {
                let mark_coverage = Coverage::parse(s.read_at_offset16(data)?)?;
                let ligature_coverage = Coverage::parse(s.read_at_offset16(data)?)?;
                let class_count = s.read::<u16>()?;
                let marks = MarkArray::parse(s.read_at_offset16(data)?)?;
                let ligature_array = LigatureArray::parse(s.read_at_offset16(data)?, class_count)?;
                Some(Self {
                    mark_coverage,
                    ligature_coverage,
                    marks,
                    ligature_array,
                })
            }
            _ => None,
        }
    }
}

/// An array of ligature anchor matrices.
#[derive(Clone, Copy)]
pub struct LigatureArray<'a> {
    data: &'a [u8],
    class_count: u16,
    offsets: LazyArray16<'a, Offset16>,
}

impl<'a> LigatureArray<'a> {
    fn parse(data: &'a [u8], class_count: u16) -> Option<Self> {
        let mut s = Stream::new(data);
        let count = s.read::<u16>()?;
        let offsets = s.read_array16(count)?;
        Some(Self {
            data,
            class_count,
            offsets,
        })
    }

    /// Returns an [`AnchorMatrix`] at index.
    pub fn get(&self, index: u16) -> Option<AnchorMatrix<'a>> {
        let offset = self.offsets.get(index)?.to_usize();
        let data = self.data.get(offset..)?;
        AnchorMatrix::parse(data, self.class_count)
    }

    /// Returns the array length.
    #[inline]
    pub const fn len(&self) -> u16 {
        self.offsets.len()
    }

    /// Checks if the array is empty.
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.offsets.is_empty()
    }
}

impl core::fmt::Debug for LigatureArray<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "LigatureArray {{ ... }}")
    }
}

#[derive(Clone, Copy)]
struct MarkRecord {
    class: Class,
    mark_anchor: Offset16,
}

impl FromData for MarkRecord {
    const SIZE: usize = 4;

    #[inline]
    fn parse(data: &[u8]) -> Option<Self> {
        let mut s = Stream::new(data);
        Some(Self {
            class: s.read::<Class>()?,
            mark_anchor: s.read::<Offset16>()?,
        })
    }
}

/// OpenType `GPOS` MarkArray table.
#[derive(Clone, Copy)]
pub struct MarkArray<'a> {
    data: &'a [u8],
    array: LazyArray16<'a, MarkRecord>,
}

impl<'a> MarkArray<'a> {
    fn parse(data: &'a [u8]) -> Option<Self> {
        let mut s = Stream::new(data);
        let count = s.read::<u16>()?;
        let array = s.read_array16(count)?;
        Some(Self { data, array })
    }

    /// Returns contained data at index.
    pub fn get(&self, index: u16) -> Option<(Class, Anchor<'a>)> {
        let record = self.array.get(index)?;
        let anchor = self
            .data
            .get(record.mark_anchor.to_usize()..)
            .and_then(Anchor::parse)?;
        Some((record.class, anchor))
    }

    /// Returns the array length.
    #[inline]
    pub const fn len(&self) -> u16 {
        self.array.len()
    }

    /// Checks if the array is empty.
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.array.is_empty()
    }
}

impl core::fmt::Debug for MarkArray<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "MarkArray {{ ... }}")
    }
}

/// OpenType `GPOS` Anchor table (Format 1 and Format 3; Format 2 contour point index is ignored).
#[derive(Clone, Copy, Debug)]
pub struct Anchor<'a> {
    /// Horizontal value, in design units.
    pub x: i16,
    /// Vertical value, in design units.
    pub y: i16,
    /// A [`Device`] table with horizontal value.
    pub x_device: Option<Device<'a>>,
    /// A [`Device`] table with vertical value.
    pub y_device: Option<Device<'a>>,
}

impl<'a> Anchor<'a> {
    fn parse(data: &'a [u8]) -> Option<Self> {
        let mut s = Stream::new(data);
        let format = s.read::<u16>()?;
        if !matches!(format, 1..=3) {
            return None;
        }

        let mut table = Anchor {
            x: s.read::<i16>()?,
            y: s.read::<i16>()?,
            x_device: None,
            y_device: None,
        };

        if format == 3 {
            table.x_device = s
                .read::<Option<Offset16>>()?
                .and_then(|offset| data.get(offset.to_usize()..))
                .and_then(Device::parse);

            table.y_device = s
                .read::<Option<Offset16>>()?
                .and_then(|offset| data.get(offset.to_usize()..))
                .and_then(Device::parse);
        }

        Some(table)
    }
}

/// An [`Anchor`] matrix used by mark attachment subtables.
#[derive(Clone, Copy)]
pub struct AnchorMatrix<'a> {
    data: &'a [u8],
    /// Number of rows in the matrix.
    pub rows: u16,
    /// Number of columns in the matrix.
    pub cols: u16,
    matrix: LazyArray32<'a, Option<Offset16>>,
}

impl<'a> AnchorMatrix<'a> {
    fn parse(data: &'a [u8], cols: u16) -> Option<Self> {
        let mut s = Stream::new(data);
        let rows = s.read::<u16>()?;
        let count = u32::from(rows) * u32::from(cols);
        let matrix = s.read_array32(count)?;
        Some(Self {
            data,
            rows,
            cols,
            matrix,
        })
    }

    /// Returns an [`Anchor`] at `(row, col)`.
    pub fn get(&self, row: u16, col: u16) -> Option<Anchor<'a>> {
        if row >= self.rows || col >= self.cols {
            return None;
        }
        let idx = u32::from(row) * u32::from(self.cols) + u32::from(col);
        let offset = self.matrix.get(idx)??.to_usize();
        Anchor::parse(self.data.get(offset..)?)
    }
}

impl core::fmt::Debug for AnchorMatrix<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "AnchorMatrix {{ ... }}")
    }
}

/// OpenType Mark-to-Mark Attachment Positioning subtable (`GPOS` LookupType 6).
#[allow(missing_docs)]
#[derive(Clone, Copy, Debug)]
pub struct MarkToMarkAdjustment<'a> {
    pub mark1_coverage: Coverage<'a>,
    pub mark2_coverage: Coverage<'a>,
    pub marks: MarkArray<'a>,
    pub mark2_matrix: AnchorMatrix<'a>,
}

impl<'a> MarkToMarkAdjustment<'a> {
    fn parse(data: &'a [u8]) -> Option<Self> {
        let mut s = Stream::new(data);
        match s.read::<u16>()? {
            1 => {
                let mark1_coverage = Coverage::parse(s.read_at_offset16(data)?)?;
                let mark2_coverage = Coverage::parse(s.read_at_offset16(data)?)?;
                let class_count = s.read::<u16>()?;
                let marks = MarkArray::parse(s.read_at_offset16(data)?)?;
                let mark2_matrix = AnchorMatrix::parse(s.read_at_offset16(data)?, class_count)?;
                Some(Self {
                    mark1_coverage,
                    mark2_coverage,
                    marks,
                    mark2_matrix,
                })
            }
            _ => None,
        }
    }
}

/// OpenType `GPOS` lookup subtable enumeration.
#[allow(missing_docs)]
#[derive(Clone, Copy, Debug)]
pub enum PositioningSubtable<'a> {
    Single(SingleAdjustment<'a>),
    Pair(PairAdjustment<'a>),
    Cursive(CursiveAdjustment<'a>),
    MarkToBase(MarkToBaseAdjustment<'a>),
    MarkToLigature(MarkToLigatureAdjustment<'a>),
    MarkToMark(MarkToMarkAdjustment<'a>),
    Context(ContextLookup<'a>),
    ChainContext(ChainedContextLookup<'a>),
}

impl<'a> LookupSubtable<'a> for PositioningSubtable<'a> {
    fn parse(data: &'a [u8], kind: u16) -> Option<Self> {
        match kind {
            1 => SingleAdjustment::parse(data).map(Self::Single),
            2 => PairAdjustment::parse(data).map(Self::Pair),
            3 => CursiveAdjustment::parse(data).map(Self::Cursive),
            4 => MarkToBaseAdjustment::parse(data).map(Self::MarkToBase),
            5 => MarkToLigatureAdjustment::parse(data).map(Self::MarkToLigature),
            6 => MarkToMarkAdjustment::parse(data).map(Self::MarkToMark),
            7 => ContextLookup::parse(data).map(Self::Context),
            8 => ChainedContextLookup::parse(data).map(Self::ChainContext),
            9 => crate::ggg::parse_extension_lookup(data, |data, kind| {
                if kind == 9 {
                    None
                } else {
                    Self::parse(data, kind)
                }
            }),
            _ => None,
        }
    }
}

impl<'a> PositioningSubtable<'a> {
    /// Returns the subtable coverage.
    #[inline]
    pub fn coverage(&self) -> Coverage<'a> {
        match self {
            Self::Single(t) => t.coverage(),
            Self::Pair(t) => t.coverage(),
            Self::Cursive(t) => t.coverage,
            Self::MarkToBase(t) => t.mark_coverage,
            Self::MarkToLigature(t) => t.mark_coverage,
            Self::MarkToMark(t) => t.mark1_coverage,
            Self::Context(t) => t.coverage(),
            Self::ChainContext(t) => t.coverage(),
        }
    }
}
