// Copyright (c) 2018 Yevhenii Reizner and the ttf-parser authors
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from ttf-parser 0.25.1 src/tables/mod.rs, and `PlatformId` from
// src/tables/name.rs, and modified by the aterm project in 2026: the tables
// aterm reads (TF.3 and TF.5 of docs/AUDIT-third-party-2026-10-15.md). Not
// carved: `ankr`, `feat`, `kerx`, `morx`, `trak` (the AAT layout tables),
// `GPOS` subtables, `MATH`, `STAT` and `SVG `.
// See ../../LICENSE-MIT and ../../LICENSE-APACHE for the upstream licenses.

pub mod cbdt;
pub mod cblc;
mod cff;
pub mod cmap;
pub mod colr;
pub mod cpal;
pub mod gdef;
pub mod glyf;
pub mod gsub;
pub mod head;
pub mod hhea;
pub mod hmtx;
pub mod kern;
pub mod loca;
pub mod maxp;
pub mod name;
pub mod os2;
pub mod post;
pub mod sbix;
pub mod vhea;
pub mod vorg;

pub mod avar;
pub mod fvar;
pub mod gvar;
pub mod hvar;
pub mod mvar;
pub mod vvar;

pub use cff::CFFError;
pub use cff::cff1;
pub use cff::cff2;

use crate::parser::FromData;

/// A [platform ID](https://docs.microsoft.com/en-us/typography/opentype/spec/name#platform-ids).
#[allow(missing_docs)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PlatformId {
    Unicode,
    Macintosh,
    Iso,
    Windows,
    Custom,
}

impl FromData for PlatformId {
    const SIZE: usize = 2;

    #[inline]
    fn parse(data: &[u8]) -> Option<Self> {
        match u16::parse(data)? {
            0 => Some(PlatformId::Unicode),
            1 => Some(PlatformId::Macintosh),
            2 => Some(PlatformId::Iso),
            3 => Some(PlatformId::Windows),
            4 => Some(PlatformId::Custom),
            _ => None,
        }
    }
}
