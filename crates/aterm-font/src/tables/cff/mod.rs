// Copyright (c) 2018 Yevhenii Reizner and the ttf-parser authors
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from ttf-parser 0.25.1 src/tables/cff/mod.rs and modified by the aterm
// project in 2026.
// See ../../../LICENSE-MIT and ../../../LICENSE-APACHE for the upstream licenses.

mod argstack;
pub mod cff1;
pub mod cff2;
mod charset;
mod charstring;
mod dict;
mod encoding;
mod index;

use core::convert::TryFrom;

use crate::parser::{FromData, TryNumFrom};
use crate::{OutlineSink, RectF};

/// A list of errors that can occur during a CFF glyph outlining.
#[allow(missing_docs)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CFFError {
    NoGlyph,
    ReadOutOfBounds,
    ZeroBBox,
    InvalidOperator,
    UnsupportedOperator,
    MissingEndChar,
    DataAfterEndChar,
    NestingLimitReached,
    OutlineWorkLimitReached,
    ArgumentsStackLimitReached,
    InvalidArgumentsStackLength,
    BboxOverflow,
    MissingMoveTo,
    InvalidSubroutineIndex,
    NoLocalSubroutines,
    InvalidSeacCode,
    InvalidItemVariationDataIndex,
    InvalidNumberOfBlendOperands,
    BlendRegionsLimitReached,
}

pub(crate) struct Builder<'a> {
    builder: &'a mut dyn OutlineSink,
    bbox: RectF,
}

impl<'a> Builder<'a> {
    #[inline]
    fn move_to(&mut self, x: f32, y: f32) {
        self.bbox.extend_by(x, y);
        self.builder.move_to(x, y);
    }

    #[inline]
    fn line_to(&mut self, x: f32, y: f32) {
        self.bbox.extend_by(x, y);
        self.builder.line_to(x, y);
    }

    #[inline]
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.bbox.extend_by(x1, y1);
        self.bbox.extend_by(x2, y2);
        self.bbox.extend_by(x, y);
        self.builder.curve_to(x1, y1, x2, y2, x, y);
    }

    #[inline]
    fn close(&mut self) {
        self.builder.close();
    }
}

/// A type-safe wrapper for string ID.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Debug)]
pub struct StringId(u16);

impl FromData for StringId {
    const SIZE: usize = 2;

    #[inline]
    fn parse(data: &[u8]) -> Option<Self> {
        u16::parse(data).map(StringId)
    }
}

pub trait IsEven {
    fn is_even(&self) -> bool;
    fn is_odd(&self) -> bool;
}

impl IsEven for usize {
    #[inline]
    fn is_even(&self) -> bool {
        (*self) & 1 == 0
    }

    #[inline]
    fn is_odd(&self) -> bool {
        !self.is_even()
    }
}

#[inline]
pub fn f32_abs(n: f32) -> f32 {
    n.abs()
}

#[inline]
pub fn conv_subroutine_index(index: f32, bias: u16) -> Result<u32, CFFError> {
    conv_subroutine_index_impl(index, bias).ok_or(CFFError::InvalidSubroutineIndex)
}

#[inline]
fn conv_subroutine_index_impl(index: f32, bias: u16) -> Option<u32> {
    let index = i32::try_num_from(index)?;
    let bias = i32::from(bias);

    let index = index.checked_add(bias)?;
    u32::try_from(index).ok()
}

// Adobe Technical Note #5176, Chapter 16 "Local / Global Subrs INDEXes"
//
// The `ensures` states the bias a caller adds; prover 321aaeda7 does not
// lower the `cfg_attr` contract form, so it counts as unknown (see
// `parser::i16_bound`).
#[inline]
#[cfg_attr(trust_verify, trust::ensures(result == 107 || result == 1131 || result == 32768))]
pub fn calc_subroutine_bias(len: u32) -> u16 {
    if len < 1240 {
        107
    } else if len < 33900 {
        1131
    } else {
        32768
    }
}
