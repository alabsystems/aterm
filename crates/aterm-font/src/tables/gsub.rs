// Copyright (c) 2018 Yevhenii Reizner and the ttf-parser authors
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from ttf-parser 0.25.1 src/tables/gsub.rs and modified by the aterm
// project in 2026.
// See ../../LICENSE-MIT and ../../LICENSE-APACHE for the upstream licenses.

//! OpenType Glyph Substitution (`GSUB`) table implementation.

use crate::GlyphId;
use crate::ggg::{ChainedContextLookup, ContextLookup, Coverage, LookupSubtable};
use crate::parser::{FromSlice, LazyArray16, LazyOffsetArray16, Stream};

/// OpenType Single Substitution subtable (`GSUB` LookupType 1).
#[allow(missing_docs)]
#[derive(Clone, Copy, Debug)]
pub enum SingleSubstitution<'a> {
    Format1 {
        coverage: Coverage<'a>,
        delta: i16,
    },
    Format2 {
        coverage: Coverage<'a>,
        substitutes: LazyArray16<'a, GlyphId>,
    },
}

impl<'a> SingleSubstitution<'a> {
    fn parse(data: &'a [u8]) -> Option<Self> {
        let mut s = Stream::new(data);
        match s.read::<u16>()? {
            1 => {
                let coverage = Coverage::parse(s.read_at_offset16(data)?)?;
                let delta = s.read::<i16>()?;
                Some(Self::Format1 { coverage, delta })
            }
            2 => {
                let coverage = Coverage::parse(s.read_at_offset16(data)?)?;
                let count = s.read::<u16>()?;
                let substitutes = s.read_array16(count)?;
                Some(Self::Format2 {
                    coverage,
                    substitutes,
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

/// Sequence of glyphs for a `GSUB` Multiple Substitution subtable.
#[derive(Clone, Copy, Debug)]
pub struct Sequence<'a> {
    /// A list of substitute glyphs.
    pub substitutes: LazyArray16<'a, GlyphId>,
}

impl<'a> FromSlice<'a> for Sequence<'a> {
    fn parse(data: &'a [u8]) -> Option<Self> {
        let mut s = Stream::new(data);
        let count = s.read::<u16>()?;
        let substitutes = s.read_array16(count)?;
        Some(Self { substitutes })
    }
}

/// A list of [`Sequence`] tables.
pub type SequenceList<'a> = LazyOffsetArray16<'a, Sequence<'a>>;

/// OpenType Multiple Substitution subtable (`GSUB` LookupType 2).
#[allow(missing_docs)]
#[derive(Clone, Copy, Debug)]
pub struct MultipleSubstitution<'a> {
    pub coverage: Coverage<'a>,
    pub sequences: SequenceList<'a>,
}

impl<'a> MultipleSubstitution<'a> {
    fn parse(data: &'a [u8]) -> Option<Self> {
        let mut s = Stream::new(data);
        match s.read::<u16>()? {
            1 => {
                let coverage = Coverage::parse(s.read_at_offset16(data)?)?;
                let count = s.read::<u16>()?;
                let offsets = s.read_array16(count)?;
                Some(Self {
                    coverage,
                    sequences: SequenceList::new(data, offsets),
                })
            }
            _ => None,
        }
    }
}

/// List of glyphs for a `GSUB` Alternate Substitution subtable.
#[derive(Clone, Copy, Debug)]
pub struct AlternateSet<'a> {
    /// Array of alternate glyph IDs, in arbitrary order.
    pub alternates: LazyArray16<'a, GlyphId>,
}

impl<'a> FromSlice<'a> for AlternateSet<'a> {
    fn parse(data: &'a [u8]) -> Option<Self> {
        let mut s = Stream::new(data);
        let count = s.read::<u16>()?;
        let alternates = s.read_array16(count)?;
        Some(Self { alternates })
    }
}

/// A set of [`AlternateSet`].
pub type AlternateSets<'a> = LazyOffsetArray16<'a, AlternateSet<'a>>;

/// OpenType Alternate Substitution subtable (`GSUB` LookupType 3).
#[allow(missing_docs)]
#[derive(Clone, Copy, Debug)]
pub struct AlternateSubstitution<'a> {
    pub coverage: Coverage<'a>,
    pub alternate_sets: AlternateSets<'a>,
}

impl<'a> AlternateSubstitution<'a> {
    fn parse(data: &'a [u8]) -> Option<Self> {
        let mut s = Stream::new(data);
        match s.read::<u16>()? {
            1 => {
                let coverage = Coverage::parse(s.read_at_offset16(data)?)?;
                let count = s.read::<u16>()?;
                let offsets = s.read_array16(count)?;
                Some(Self {
                    coverage,
                    alternate_sets: AlternateSets::new(data, offsets),
                })
            }
            _ => None,
        }
    }
}

/// Glyph components for one ligature.
#[derive(Clone, Copy, Debug)]
pub struct Ligature<'a> {
    /// Ligature to substitute.
    pub glyph: GlyphId,
    /// Glyph components for one ligature.
    pub components: LazyArray16<'a, GlyphId>,
}

impl<'a> FromSlice<'a> for Ligature<'a> {
    fn parse(data: &'a [u8]) -> Option<Self> {
        let mut s = Stream::new(data);
        let glyph = s.read::<GlyphId>()?;
        let count = s.read::<u16>()?;
        let components = s.read_array16(count.checked_sub(1)?)?;
        Some(Self { glyph, components })
    }
}

/// A [`Ligature`] set.
pub type LigatureSet<'a> = LazyOffsetArray16<'a, Ligature<'a>>;

impl<'a> FromSlice<'a> for LigatureSet<'a> {
    fn parse(data: &'a [u8]) -> Option<Self> {
        Self::parse(data)
    }
}

/// A list of [`Ligature`] sets.
pub type LigatureSets<'a> = LazyOffsetArray16<'a, LigatureSet<'a>>;

/// OpenType Ligature Substitution subtable (`GSUB` LookupType 4).
#[allow(missing_docs)]
#[derive(Clone, Copy, Debug)]
pub struct LigatureSubstitution<'a> {
    pub coverage: Coverage<'a>,
    pub ligature_sets: LigatureSets<'a>,
}

impl<'a> LigatureSubstitution<'a> {
    fn parse(data: &'a [u8]) -> Option<Self> {
        let mut s = Stream::new(data);
        match s.read::<u16>()? {
            1 => {
                let coverage = Coverage::parse(s.read_at_offset16(data)?)?;
                let count = s.read::<u16>()?;
                let offsets = s.read_array16(count)?;
                Some(Self {
                    coverage,
                    ligature_sets: LigatureSets::new(data, offsets),
                })
            }
            _ => None,
        }
    }
}

/// OpenType Reverse Chaining Contextual Single Substitution subtable (`GSUB` LookupType 8).
#[allow(missing_docs)]
#[derive(Clone, Copy, Debug)]
pub struct ReverseChainSingleSubstitution<'a> {
    pub coverage: Coverage<'a>,
    pub backtrack_coverages: LazyOffsetArray16<'a, Coverage<'a>>,
    pub lookahead_coverages: LazyOffsetArray16<'a, Coverage<'a>>,
    pub substitutes: LazyArray16<'a, GlyphId>,
}

impl<'a> ReverseChainSingleSubstitution<'a> {
    fn parse(data: &'a [u8]) -> Option<Self> {
        let mut s = Stream::new(data);
        match s.read::<u16>()? {
            1 => {
                let coverage = Coverage::parse(s.read_at_offset16(data)?)?;
                let backtrack_count = s.read::<u16>()?;
                let backtrack_coverages = s.read_array16(backtrack_count)?;
                let lookahead_count = s.read::<u16>()?;
                let lookahead_coverages = s.read_array16(lookahead_count)?;
                let substitute_count = s.read::<u16>()?;
                let substitutes = s.read_array16(substitute_count)?;
                Some(Self {
                    coverage,
                    backtrack_coverages: LazyOffsetArray16::new(data, backtrack_coverages),
                    lookahead_coverages: LazyOffsetArray16::new(data, lookahead_coverages),
                    substitutes,
                })
            }
            _ => None,
        }
    }
}

/// Glyph substitution lookup subtable enumeration (`GSUB`).
#[allow(missing_docs)]
#[derive(Clone, Copy, Debug)]
pub enum SubstitutionSubtable<'a> {
    Single(SingleSubstitution<'a>),
    Multiple(MultipleSubstitution<'a>),
    Alternate(AlternateSubstitution<'a>),
    Ligature(LigatureSubstitution<'a>),
    Context(ContextLookup<'a>),
    ChainContext(ChainedContextLookup<'a>),
    ReverseChainSingle(ReverseChainSingleSubstitution<'a>),
}

impl<'a> LookupSubtable<'a> for SubstitutionSubtable<'a> {
    fn parse(data: &'a [u8], kind: u16) -> Option<Self> {
        match kind {
            1 => SingleSubstitution::parse(data).map(Self::Single),
            2 => MultipleSubstitution::parse(data).map(Self::Multiple),
            3 => AlternateSubstitution::parse(data).map(Self::Alternate),
            4 => LigatureSubstitution::parse(data).map(Self::Ligature),
            5 => ContextLookup::parse(data).map(Self::Context),
            6 => ChainedContextLookup::parse(data).map(Self::ChainContext),
            7 => crate::ggg::parse_extension_lookup(data, |data, kind| {
                if kind == 7 {
                    None
                } else {
                    Self::parse(data, kind)
                }
            }),
            8 => ReverseChainSingleSubstitution::parse(data).map(Self::ReverseChainSingle),
            _ => None,
        }
    }
}

impl<'a> SubstitutionSubtable<'a> {
    /// Returns the subtable coverage.
    #[inline]
    pub fn coverage(&self) -> Coverage<'a> {
        match self {
            Self::Single(t) => t.coverage(),
            Self::Multiple(t) => t.coverage,
            Self::Alternate(t) => t.coverage,
            Self::Ligature(t) => t.coverage,
            Self::Context(t) => t.coverage(),
            Self::ChainContext(t) => t.coverage(),
            Self::ReverseChainSingle(t) => t.coverage,
        }
    }

    /// Checks that the current subtable is *Reverse Chaining Contextual Single*.
    #[inline]
    pub fn is_reverse(&self) -> bool {
        matches!(self, Self::ReverseChainSingle(_))
    }
}
