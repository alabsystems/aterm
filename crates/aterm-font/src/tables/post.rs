// Copyright (c) 2018 Yevhenii Reizner and the ttf-parser authors
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from ttf-parser 0.25.1 src/tables/post.rs and modified by the aterm
// project in 2026.
// See ../../LICENSE-MIT and ../../LICENSE-APACHE for the upstream licenses.

//! The [PostScript Table](https://docs.microsoft.com/en-us/typography/opentype/spec/post) header.

use crate::LineMetrics;
use crate::parser::{Fixed, Stream};

const ITALIC_ANGLE_OFFSET: usize = 4;
const UNDERLINE_POSITION_OFFSET: usize = 8;
const UNDERLINE_THICKNESS_OFFSET: usize = 10;
const IS_FIXED_PITCH_OFFSET: usize = 12;

/// A [PostScript Table](https://docs.microsoft.com/en-us/typography/opentype/spec/post) header.
#[derive(Clone, Copy, Debug)]
pub struct Table {
    /// Italic angle in counter-clockwise degrees from the vertical.
    pub italic_angle: f32,
    /// Underline metrics.
    pub underline_metrics: LineMetrics,
    /// Flag that indicates that the font is monospaced.
    pub is_monospaced: bool,
}

impl Table {
    /// Parses a table from raw data.
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < 32 {
            return None;
        }

        let version = Stream::new(data).read::<u32>()?;
        if !(version == 0x00010000
            || version == 0x00020000
            || version == 0x00025000
            || version == 0x00030000
            || version == 0x00040000)
        {
            return None;
        }

        let italic_angle = Stream::read_at::<Fixed>(data, ITALIC_ANGLE_OFFSET)?.0;

        let underline_metrics = LineMetrics {
            position: Stream::read_at::<i16>(data, UNDERLINE_POSITION_OFFSET)?,
            thickness: Stream::read_at::<i16>(data, UNDERLINE_THICKNESS_OFFSET)?,
        };

        let is_monospaced = Stream::read_at::<u32>(data, IS_FIXED_PITCH_OFFSET)? != 0;

        if version == 0x00020000 {
            let mut s = Stream::new_at(data, 32)?;
            let indexes_count = s.read::<u16>()?;
            s.read_array16::<u16>(indexes_count)?;
        }

        Some(Table {
            italic_angle,
            underline_metrics,
            is_monospaced,
        })
    }
}
