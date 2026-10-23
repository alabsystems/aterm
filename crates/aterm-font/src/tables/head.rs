// Copyright (c) 2018 Yevhenii Reizner and the ttf-parser authors
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from ttf-parser 0.25.1 src/tables/head.rs and modified by the aterm
// project in 2026.
// See ../../LICENSE-MIT and ../../LICENSE-APACHE for the upstream licenses.

//! A [Font Header Table](
//! https://docs.microsoft.com/en-us/typography/opentype/spec/head) implementation.

use crate::Rect;
use crate::parser::{Fixed, Stream};

/// An index format used by the [Index to Location Table](
/// https://docs.microsoft.com/en-us/typography/opentype/spec/loca).
#[allow(missing_docs)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IndexToLocationFormat {
    Short,
    Long,
}

/// A [Font Header Table](https://docs.microsoft.com/en-us/typography/opentype/spec/head).
#[derive(Clone, Copy, Debug)]
pub struct Table {
    /// Units per EM (`16..=16384`).
    pub units_per_em: u16,
    /// A bounding box that large enough to enclose any glyph from the face.
    pub global_bbox: Rect,
    /// An index format used by the [Index to Location Table](
    /// https://docs.microsoft.com/en-us/typography/opentype/spec/loca).
    pub index_to_location_format: IndexToLocationFormat,
    /// `macStyle` bits: bit 0 bold, bit 1 italic.
    pub mac_style: u16,
}

impl Table {
    /// `macStyle` bit 0.
    #[inline]
    pub const fn mac_bold(&self) -> bool {
        self.mac_style & 1 != 0
    }

    /// `macStyle` bit 1.
    #[inline]
    pub const fn mac_italic(&self) -> bool {
        self.mac_style & 2 != 0
    }
}

impl Table {
    /// Parses a table from raw data.
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < 54 {
            return None;
        }

        let mut s = Stream::new(data);
        s.skip::<u32>(); // version
        s.skip::<Fixed>(); // font revision
        s.skip::<u32>(); // checksum adjustment
        s.skip::<u32>(); // magic number
        s.skip::<u16>(); // flags
        let units_per_em = s.read::<u16>()?;
        s.skip::<u64>(); // created time
        s.skip::<u64>(); // modified time
        let x_min = s.read::<i16>()?;
        let y_min = s.read::<i16>()?;
        let x_max = s.read::<i16>()?;
        let y_max = s.read::<i16>()?;
        let mac_style = s.read::<u16>()?;
        s.skip::<u16>(); // lowest PPEM
        s.skip::<i16>(); // font direction hint
        let index_to_location_format = s.read::<u16>()?;

        if !(16..=16384).contains(&units_per_em) {
            return None;
        }

        let index_to_location_format = match index_to_location_format {
            0 => IndexToLocationFormat::Short,
            1 => IndexToLocationFormat::Long,
            _ => return None,
        };

        Some(Table {
            units_per_em,
            global_bbox: Rect {
                x_min,
                y_min,
                x_max,
                y_max,
            },
            index_to_location_format,
            mac_style,
        })
    }
}
