// Copyright (c) 2018 Yevhenii Reizner and the ttf-parser authors
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from ttf-parser 0.25.1 src/var_store.rs and modified by the aterm
// project in 2026.
// See ../LICENSE-MIT and ../LICENSE-APACHE for the upstream licenses.

//! Item Variation Store implementation.

use crate::NormalizedCoordinate;
use crate::parser::{FromData, LazyArray16, NumFrom, Stream};

/// Region-axis evaluations one [`ItemVariationStore::parse_delta`] may make:
/// real faces name tens of regions of a handful of axes per delta set.
const DELTA_STEPS: u32 = 65_536;

#[derive(Clone, Copy, Debug)]
pub(crate) struct ItemVariationStore<'a> {
    data: &'a [u8],
    data_offsets: LazyArray16<'a, u32>,
    pub regions: VariationRegionList<'a>,
}

impl<'a> Default for ItemVariationStore<'a> {
    #[inline]
    fn default() -> Self {
        ItemVariationStore {
            data: &[],
            data_offsets: LazyArray16::new(&[]),
            regions: VariationRegionList {
                axis_count: 0,
                regions: LazyArray16::new(&[]),
            },
        }
    }
}

impl<'a> ItemVariationStore<'a> {
    #[inline]
    pub fn parse(mut s: Stream<'_>) -> Option<ItemVariationStore<'_>> {
        let data = s.tail()?;

        let mut regions_s = s.clone();
        let format = s.read::<u16>()?;
        if format != 1 {
            return None;
        }

        let region_list_offset = s.read::<u32>()?;
        let count = s.read::<u16>()?;
        let offsets = s.read_array16::<u32>(count)?;

        let regions = {
            regions_s.advance(usize::num_from(region_list_offset));
            let axis_count = regions_s.read::<u16>()?;
            let count = regions_s.read::<u16>()?;
            let total = count.checked_mul(axis_count)?;
            VariationRegionList {
                axis_count,
                regions: regions_s.read_array16::<RegionAxisCoordinatesRecord>(total)?,
            }
        };

        Some(ItemVariationStore {
            data,
            data_offsets: offsets,
            regions,
        })
    }

    /// The region-axis evaluations [`Self::parse_delta`] of an item of the
    /// `outer_index` set may make at `coordinates`, read from the set's header
    /// alone: each region it names over every axis, to at most
    /// [`DELTA_STEPS`]. A set that is not there costs nothing.
    pub(crate) fn delta_steps(
        &self,
        outer_index: u16,
        coordinates: &[NormalizedCoordinate],
    ) -> usize {
        let regions = self
            .data_offsets
            .get(outer_index)
            .and_then(|offset| {
                let mut s = Stream::new_at(self.data, usize::num_from(offset))?;
                s.skip::<u16>(); // item_count
                s.skip::<u16>(); // word_delta_count
                s.read::<u16>()
            })
            .unwrap_or(0);
        let steps = u32::from(regions)
            .saturating_mul(per_region(coordinates))
            .min(DELTA_STEPS);
        usize::num_from(steps)
    }

    pub fn region_indices(&self, index: u16) -> Option<LazyArray16<'_, u16>> {
        let offset = self.data_offsets.get(index)?;
        let mut s = Stream::new_at(self.data, usize::num_from(offset))?;
        s.skip::<u16>(); // item_count
        s.skip::<u16>(); // short_delta_count
        let count = s.read::<u16>()?;
        s.read_array16::<u16>(count)
    }

    /// The delta one item of the store adds at `coordinates`, or `None` when
    /// it is malformed or would cost more than [`DELTA_STEPS`]: each region
    /// its delta set names is evaluated over every axis, so a set naming tens
    /// of thousands of regions in a face of many axes would otherwise cost
    /// millions of steps for one glyph's advance.
    pub fn parse_delta(
        &self,
        outer_index: u16,
        inner_index: u16,
        coordinates: &[NormalizedCoordinate],
    ) -> Option<f32> {
        let per_region = per_region(coordinates);
        let mut steps = DELTA_STEPS;
        let mut evaluate = |region: u16| {
            steps = steps.checked_sub(per_region)?;
            #[cfg(all(any(test, debug_assertions), not(trust_verify)))]
            crate::work::note(|s| s.delta_steps += u64::from(per_region));
            Some(self.regions.evaluate_region(region, coordinates))
        };

        let offset = self.data_offsets.get(outer_index)?;
        let mut s = Stream::new_at(self.data, usize::num_from(offset))?;
        let item_count = s.read::<u16>()?;
        let word_delta_count = s.read::<u16>()?;
        let region_index_count = s.read::<u16>()?;
        let region_indices = s.read_array16::<u16>(region_index_count)?;

        if inner_index >= item_count {
            return None;
        }

        let has_long_words = (word_delta_count & 0x8000) != 0;
        let word_delta_count = word_delta_count & 0x7FFF;

        let mut delta_set_len = usize::from(word_delta_count) + usize::from(region_index_count);
        if has_long_words {
            delta_set_len *= 2;
        }

        s.advance(usize::from(inner_index).checked_mul(delta_set_len)?);

        let mut delta = 0.0;
        let mut i = 0;
        while i < word_delta_count {
            let idx = region_indices.get(i)?;
            let num = if has_long_words {
                s.read::<i32>()? as f32
            } else {
                f32::from(s.read::<i16>()?)
            };
            delta += num * evaluate(idx)?;
            i += 1;
        }

        while i < region_index_count {
            let idx = region_indices.get(i)?;
            let num = if has_long_words {
                f32::from(s.read::<i16>()?)
            } else {
                f32::from(s.read::<i8>()?)
            };
            delta += num * evaluate(idx)?;
            i += 1;
        }

        Some(delta)
    }
}

/// What evaluating one region at `coordinates` costs: a step per axis.
#[inline]
fn per_region(coordinates: &[NormalizedCoordinate]) -> u32 {
    u32::try_from(coordinates.len()).map_or(u32::MAX, |axes| axes.max(1))
}

#[derive(Clone, Copy, Debug)]
pub struct VariationRegionList<'a> {
    axis_count: u16,
    regions: LazyArray16<'a, RegionAxisCoordinatesRecord>,
}

impl<'a> VariationRegionList<'a> {
    #[inline]
    pub(crate) fn evaluate_region(&self, index: u16, coordinates: &[NormalizedCoordinate]) -> f32 {
        let mut v = 1.0;
        for (i, coord) in coordinates.iter().enumerate() {
            let record = u16::try_from(i)
                .ok()
                .and_then(|i| index.checked_mul(self.axis_count)?.checked_add(i));
            let region = match record.and_then(|r| self.regions.get(r)) {
                Some(r) => r,
                None => return 0.0,
            };

            let factor = region.evaluate_axis(coord.get());
            if factor == 0.0 {
                return 0.0;
            }

            v *= factor;
        }

        v
    }
}

#[derive(Clone, Copy, Debug)]
struct RegionAxisCoordinatesRecord {
    start_coord: i16,
    peak_coord: i16,
    end_coord: i16,
}

impl RegionAxisCoordinatesRecord {
    #[inline]
    pub fn evaluate_axis(&self, coord: i16) -> f32 {
        let start = self.start_coord;
        let peak = self.peak_coord;
        let end = self.end_coord;

        if start > peak || peak > end {
            return 1.0;
        }

        if start < 0 && end > 0 && peak != 0 {
            return 1.0;
        }

        if peak == 0 || coord == peak {
            return 1.0;
        }

        if coord <= start || end <= coord {
            return 0.0;
        }

        let (coord, start, peak, end) = (
            i32::from(coord),
            i32::from(start),
            i32::from(peak),
            i32::from(end),
        );
        if coord < peak {
            (coord - start) as f32 / (peak - start) as f32
        } else {
            (end - coord) as f32 / (end - peak) as f32
        }
    }
}

impl FromData for RegionAxisCoordinatesRecord {
    const SIZE: usize = 6;

    #[inline]
    fn parse(data: &[u8]) -> Option<Self> {
        let mut s = Stream::new(data);
        Some(RegionAxisCoordinatesRecord {
            start_coord: s.read::<i16>()?,
            peak_coord: s.read::<i16>()?,
            end_coord: s.read::<i16>()?,
        })
    }
}
