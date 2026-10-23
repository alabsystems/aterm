// Copyright (c) 2018 Yevhenii Reizner and the ttf-parser authors
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from ttf-parser 0.25.1 src/tables/vvar.rs and modified by the aterm
// project in 2026.
// See ../../LICENSE-MIT and ../../LICENSE-APACHE for the upstream licenses.

//! Vertical Metrics Variations (`VVAR`) table.

use crate::delta_set::DeltaSetIndexMap;
use crate::parser::{Offset, Offset32, Stream};
use crate::var_store::ItemVariationStore;
use crate::{GlyphId, NormalizedCoordinate};

/// Vertical Metrics Variations (`VVAR`) table.
#[derive(Clone, Copy)]
pub struct Table<'a> {
    data: &'a [u8],
    variation_store: ItemVariationStore<'a>,
    advance_height_mapping_offset: Option<Offset32>,
    tsb_mapping_offset: Option<Offset32>,
    bsb_mapping_offset: Option<Offset32>,
    vorg_mapping_offset: Option<Offset32>,
}

impl<'a> Table<'a> {
    /// Parses a table from raw data.
    pub fn parse(data: &'a [u8]) -> Option<Self> {
        let mut s = Stream::new(data);

        let version = s.read::<u32>()?;
        if version != 0x00010000 {
            return None;
        }

        let variation_store_offset = s.read::<Offset32>()?;
        let var_store_s = Stream::new_at(data, variation_store_offset.to_usize())?;
        let variation_store = ItemVariationStore::parse(var_store_s)?;

        Some(Table {
            data,
            variation_store,
            advance_height_mapping_offset: s.read::<Option<Offset32>>()?,
            tsb_mapping_offset: s.read::<Option<Offset32>>()?,
            bsb_mapping_offset: s.read::<Option<Offset32>>()?,
            vorg_mapping_offset: s.read::<Option<Offset32>>()?,
        })
    }

    /// Returns the advance height offset for a glyph.
    #[inline]
    pub fn advance_offset(
        &self,
        glyph_id: GlyphId,
        coordinates: &[NormalizedCoordinate],
    ) -> Option<f32> {
        let (outer_idx, inner_idx) = if let Some(offset) = self.advance_height_mapping_offset {
            DeltaSetIndexMap::new(self.data.get(offset.to_usize()..)?).map(glyph_id.0 as u32)?
        } else {
            // Without a mapping table, outer index is 0 and inner index is the glyph ID.
            (0, glyph_id.0)
        };

        self.variation_store
            .parse_delta(outer_idx, inner_idx, coordinates)
    }

    /// Returns the top side bearing offset for a glyph.
    #[inline]
    pub fn top_side_bearing_offset(
        &self,
        glyph_id: GlyphId,
        coordinates: &[NormalizedCoordinate],
    ) -> Option<f32> {
        let set_data = self.data.get(self.tsb_mapping_offset?.to_usize()..)?;
        self.side_bearing_offset(glyph_id, coordinates, set_data)
    }

    /// Returns the bottom side bearing offset for a glyph.
    #[inline]
    pub fn bottom_side_bearing_offset(
        &self,
        glyph_id: GlyphId,
        coordinates: &[NormalizedCoordinate],
    ) -> Option<f32> {
        let set_data = self.data.get(self.bsb_mapping_offset?.to_usize()..)?;
        self.side_bearing_offset(glyph_id, coordinates, set_data)
    }

    /// Returns the vertical origin offset for a glyph.
    #[inline]
    pub fn vertical_origin_offset(
        &self,
        glyph_id: GlyphId,
        coordinates: &[NormalizedCoordinate],
    ) -> Option<f32> {
        let set_data = self.data.get(self.vorg_mapping_offset?.to_usize()..)?;
        self.side_bearing_offset(glyph_id, coordinates, set_data)
    }

    fn side_bearing_offset(
        &self,
        glyph_id: GlyphId,
        coordinates: &[NormalizedCoordinate],
        set_data: &[u8],
    ) -> Option<f32> {
        let (outer_idx, inner_idx) = DeltaSetIndexMap::new(set_data).map(glyph_id.0 as u32)?;
        self.variation_store
            .parse_delta(outer_idx, inner_idx, coordinates)
    }
}

impl core::fmt::Debug for Table<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "Table {{ ... }}")
    }
}
