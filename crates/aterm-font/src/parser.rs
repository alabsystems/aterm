// Copyright (c) 2018 Yevhenii Reizner and the ttf-parser authors
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from ttf-parser 0.25.1 src/parser.rs and modified by the aterm
// project in 2026.
// See ../LICENSE-MIT and ../LICENSE-APACHE for the upstream licenses.

//! Binary parsing utilities.

use core::convert::{TryFrom, TryInto};
use core::ops::Range;

/// Parses raw binary data of fixed size.
pub trait FromData: Sized {
    /// Raw data size in bytes.
    const SIZE: usize;

    /// Parses an object from a raw data.
    fn parse(data: &[u8]) -> Option<Self>;
}

/// Parses raw binary data of variable size.
pub trait FromSlice<'a>: Sized {
    /// Parses an object from a raw data.
    fn parse(data: &'a [u8]) -> Option<Self>;
}

impl FromData for () {
    const SIZE: usize = 0;

    #[inline]
    fn parse(_: &[u8]) -> Option<Self> {
        Some(())
    }
}

impl FromData for u8 {
    const SIZE: usize = 1;

    #[inline]
    fn parse(data: &[u8]) -> Option<Self> {
        data.get(0).copied()
    }
}

impl FromData for i8 {
    const SIZE: usize = 1;

    #[inline]
    fn parse(data: &[u8]) -> Option<Self> {
        data.get(0).copied().map(|n| n as i8)
    }
}

impl FromData for u16 {
    const SIZE: usize = 2;

    #[inline]
    fn parse(data: &[u8]) -> Option<Self> {
        data.try_into().ok().map(u16::from_be_bytes)
    }
}

impl FromData for i16 {
    const SIZE: usize = 2;

    #[inline]
    fn parse(data: &[u8]) -> Option<Self> {
        data.try_into().ok().map(i16::from_be_bytes)
    }
}

impl FromData for u32 {
    const SIZE: usize = 4;

    #[inline]
    fn parse(data: &[u8]) -> Option<Self> {
        data.try_into().ok().map(u32::from_be_bytes)
    }
}

impl FromData for i32 {
    const SIZE: usize = 4;

    #[inline]
    fn parse(data: &[u8]) -> Option<Self> {
        data.try_into().ok().map(i32::from_be_bytes)
    }
}

impl FromData for u64 {
    const SIZE: usize = 8;

    #[inline]
    fn parse(data: &[u8]) -> Option<Self> {
        data.try_into().ok().map(u64::from_be_bytes)
    }
}

/// A 24-bit unsigned integer stored as `u32`.
#[derive(Clone, Copy, Debug)]
pub struct U24(pub u32);

impl FromData for U24 {
    const SIZE: usize = 3;

    #[inline]
    fn parse(data: &[u8]) -> Option<Self> {
        let data: [u8; 3] = data.try_into().ok()?;
        Some(U24(u32::from_be_bytes([0, data[0], data[1], data[2]])))
    }
}

/// A 16-bit signed fixed number with the low 14 bits of fraction (2.14).
#[derive(Clone, Copy, Debug)]
pub struct F2DOT14(pub i16);

impl F2DOT14 {
    /// Converts i16 to f32.
    #[inline]
    pub const fn to_f32(self) -> f32 {
        self.0 as f32 / 16384.0
    }

    /// The value with a variation delta (in F2DOT14 units) applied.
    #[inline]
    pub const fn apply_float_delta(&self, delta: f32) -> f32 {
        self.to_f32() + (delta as f64 * (1.0 / 16384.0)) as f32
    }
}

impl FromData for F2DOT14 {
    const SIZE: usize = 2;

    #[inline]
    fn parse(data: &[u8]) -> Option<Self> {
        i16::parse(data).map(F2DOT14)
    }
}

/// A 32-bit signed fixed-point number (16.16).
#[derive(Clone, Copy, Debug)]
pub struct Fixed(pub f32);

impl Fixed {
    /// The value with a variation delta (in 16.16 units) applied.
    #[inline]
    pub(crate) const fn apply_float_delta(&self, delta: f32) -> f32 {
        self.0 + (delta as f64 * (1.0 / 65536.0)) as f32
    }
}

impl FromData for Fixed {
    const SIZE: usize = 4;

    #[inline]
    fn parse(data: &[u8]) -> Option<Self> {
        i32::parse(data).map(|n| Fixed(n as f32 / 65536.0))
    }
}

/// Lossless `u32`/`char` to `usize` conversion on 32/64-bit targets.
pub trait NumFrom<T>: Sized {
    /// Converts u32 into usize.
    fn num_from(_: T) -> Self;
}

impl NumFrom<u32> for usize {
    #[inline]
    fn num_from(v: u32) -> Self {
        #[cfg(any(target_pointer_width = "32", target_pointer_width = "64"))]
        {
            v as usize
        }
    }
}

impl NumFrom<char> for usize {
    #[inline]
    fn num_from(v: char) -> Self {
        #[cfg(any(target_pointer_width = "32", target_pointer_width = "64"))]
        {
            v as usize
        }
    }
}

/// Just like `TryFrom<N>`, but for numeric types not supported by the Rust's std.
pub trait TryNumFrom<T>: Sized {
    /// Casts between numeric types.
    fn try_num_from(_: T) -> Option<Self>;
}

impl TryNumFrom<f32> for u8 {
    #[inline]
    fn try_num_from(v: f32) -> Option<Self> {
        i32::try_num_from(v).and_then(|v| u8::try_from(v).ok())
    }
}

impl TryNumFrom<f32> for i16 {
    #[inline]
    fn try_num_from(v: f32) -> Option<Self> {
        i32::try_num_from(v).and_then(|v| i16::try_from(v).ok())
    }
}

impl TryNumFrom<f32> for u16 {
    #[inline]
    fn try_num_from(v: f32) -> Option<Self> {
        i32::try_num_from(v).and_then(|v| u16::try_from(v).ok())
    }
}

#[allow(clippy::manual_range_contains)]
impl TryNumFrom<f32> for i32 {
    #[inline]
    fn try_num_from(v: f32) -> Option<Self> {
        // Truncates toward zero; allow `[MIN, MAX + 1)` (`i32::MAX as f32` rounds up to `MAX + 1`).
        const MIN: f32 = i32::MIN as f32;
        const MAX_P1: f32 = i32::MAX as f32;
        if v >= MIN && v < MAX_P1 {
            Some(v as i32)
        } else {
            None
        }
    }
}

/// A slice-like container that parses fixed-size binary elements on access.
#[derive(Clone, Copy)]
pub struct LazyArray16<'a, T> {
    data: &'a [u8],
    data_type: core::marker::PhantomData<T>,
}

impl<T> Default for LazyArray16<'_, T> {
    #[inline]
    fn default() -> Self {
        LazyArray16 {
            data: &[],
            data_type: core::marker::PhantomData,
        }
    }
}

impl<'a, T: FromData> LazyArray16<'a, T> {
    /// Creates a new `LazyArray`.
    #[inline]
    pub const fn new(data: &'a [u8]) -> Self {
        LazyArray16 {
            data,
            data_type: core::marker::PhantomData,
        }
    }

    /// Returns a value at `index`.
    #[inline]
    pub fn get(&self, index: u16) -> Option<T> {
        if index < self.len() {
            let start = usize::from(index) * T::SIZE;
            let end = start + T::SIZE;
            self.data.get(start..end).and_then(T::parse)
        } else {
            None
        }
    }

    /// Returns the last value.
    #[inline]
    pub fn last(&self) -> Option<T> {
        if !self.is_empty() {
            self.get(self.len() - 1)
        } else {
            None
        }
    }

    /// Returns sub-array.
    #[inline]
    pub fn slice(&self, range: Range<u16>) -> Option<Self> {
        let start = usize::from(range.start) * T::SIZE;
        let end = usize::from(range.end) * T::SIZE;
        Some(LazyArray16 {
            data: self.data.get(start..end)?,
            ..LazyArray16::default()
        })
    }

    /// Returns array's length.
    #[inline]
    pub const fn len(&self) -> u16 {
        (self.data.len() / T::SIZE) as u16
    }

    /// Checks if array is empty.
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Performs a binary search by specified `key`.
    #[inline]
    pub fn binary_search(&self, key: &T) -> Option<(u16, T)>
    where
        T: Ord,
    {
        self.binary_search_by(|p| p.cmp(key))
    }

    /// Performs a binary search using specified closure.
    #[inline]
    pub fn binary_search_by<F>(&self, mut f: F) -> Option<(u16, T)>
    where
        F: FnMut(&T) -> core::cmp::Ordering,
    {
        use core::cmp::Ordering;

        let mut size = self.len();
        if size == 0 {
            return None;
        }

        let mut base = 0;
        while size > 1 {
            let half = size / 2;
            let mid = base + half;
            let cmp = f(&self.get(mid)?);
            base = if cmp == Ordering::Greater { base } else { mid };
            size -= half;
        }

        let value = self.get(base)?;
        if f(&value) == Ordering::Equal {
            Some((base, value))
        } else {
            None
        }
    }
}

impl<'a, T: FromData + core::fmt::Debug + Copy> core::fmt::Debug for LazyArray16<'a, T> {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        f.debug_list().entries(*self).finish()
    }
}

impl<'a, T: FromData> IntoIterator for LazyArray16<'a, T> {
    type Item = T;
    type IntoIter = LazyArrayIter16<'a, T>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        LazyArrayIter16 {
            data: self,
            index: 0,
        }
    }
}

/// An iterator over `LazyArray16`.
#[derive(Clone, Copy)]
#[allow(missing_debug_implementations)]
pub struct LazyArrayIter16<'a, T> {
    data: LazyArray16<'a, T>,
    index: u16,
}

impl<T: FromData> Default for LazyArrayIter16<'_, T> {
    #[inline]
    fn default() -> Self {
        LazyArrayIter16 {
            data: LazyArray16::new(&[]),
            index: 0,
        }
    }
}

impl<'a, T: FromData> Iterator for LazyArrayIter16<'a, T> {
    type Item = T;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        let item = self.data.get(self.index);
        self.index = self.index.saturating_add(1);
        item
    }

    #[inline]
    fn count(self) -> usize {
        usize::from(self.data.len().saturating_sub(self.index))
    }
}

/// A slice-like container that parses fixed-size binary elements on `u32` index access.
#[derive(Clone, Copy)]
pub struct LazyArray32<'a, T> {
    data: &'a [u8],
    data_type: core::marker::PhantomData<T>,
}

impl<T> Default for LazyArray32<'_, T> {
    #[inline]
    fn default() -> Self {
        LazyArray32 {
            data: &[],
            data_type: core::marker::PhantomData,
        }
    }
}

impl<'a, T: FromData> LazyArray32<'a, T> {
    /// Creates a new `LazyArray`.
    #[inline]
    pub const fn new(data: &'a [u8]) -> Self {
        LazyArray32 {
            data,
            data_type: core::marker::PhantomData,
        }
    }

    /// Returns a value at `index`.
    #[inline]
    pub fn get(&self, index: u32) -> Option<T> {
        if index < self.len() {
            let start = usize::num_from(index) * T::SIZE;
            let end = start + T::SIZE;
            self.data.get(start..end).and_then(T::parse)
        } else {
            None
        }
    }

    /// Returns array's length.
    #[inline]
    pub const fn len(&self) -> u32 {
        (self.data.len() / T::SIZE) as u32
    }

    /// Checks if the array is empty.
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Performs a binary search by specified `key`.
    #[inline]
    pub fn binary_search(&self, key: &T) -> Option<(u32, T)>
    where
        T: Ord,
    {
        self.binary_search_by(|p| p.cmp(key))
    }

    /// Performs a binary search using specified closure.
    #[inline]
    pub fn binary_search_by<F>(&self, mut f: F) -> Option<(u32, T)>
    where
        F: FnMut(&T) -> core::cmp::Ordering,
    {
        use core::cmp::Ordering;

        let mut size = self.len();
        if size == 0 {
            return None;
        }

        let mut base = 0;
        while size > 1 {
            let half = size / 2;
            let mid = base + half;
            let cmp = f(&self.get(mid)?);
            base = if cmp == Ordering::Greater { base } else { mid };
            size -= half;
        }

        let value = self.get(base)?;
        if f(&value) == Ordering::Equal {
            Some((base, value))
        } else {
            None
        }
    }
}

impl<'a, T: FromData + core::fmt::Debug + Copy> core::fmt::Debug for LazyArray32<'a, T> {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        f.debug_list().entries(*self).finish()
    }
}

impl<'a, T: FromData> IntoIterator for LazyArray32<'a, T> {
    type Item = T;
    type IntoIter = LazyArrayIter32<'a, T>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        LazyArrayIter32 {
            data: self,
            index: 0,
        }
    }
}

/// An iterator over `LazyArray32`.
#[derive(Clone, Copy)]
#[allow(missing_debug_implementations)]
pub struct LazyArrayIter32<'a, T> {
    data: LazyArray32<'a, T>,
    index: u32,
}

impl<'a, T: FromData> Iterator for LazyArrayIter32<'a, T> {
    type Item = T;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        let item = self.data.get(self.index);
        self.index = self.index.saturating_add(1);
        item
    }

    #[inline]
    fn count(self) -> usize {
        usize::num_from(self.data.len().saturating_sub(self.index))
    }
}

/// A [`LazyArray16`]-like container accessed via `Offset16` entries.
#[derive(Clone, Copy)]
pub struct LazyOffsetArray16<'a, T: FromSlice<'a>> {
    data: &'a [u8],
    offsets: LazyArray16<'a, Option<Offset16>>,
    data_type: core::marker::PhantomData<T>,
}

impl<'a, T: FromSlice<'a>> LazyOffsetArray16<'a, T> {
    /// Creates a new `LazyOffsetArray16`.
    #[inline]
    #[allow(dead_code)]
    pub const fn new(data: &'a [u8], offsets: LazyArray16<'a, Option<Offset16>>) -> Self {
        Self {
            data,
            offsets,
            data_type: core::marker::PhantomData,
        }
    }

    /// Parses `LazyOffsetArray16` from raw data.
    #[inline]
    #[allow(dead_code)]
    pub fn parse(data: &'a [u8]) -> Option<Self> {
        let mut s = Stream::new(data);
        let count = s.read::<u16>()?;
        let offsets = s.read_array16(count)?;
        Some(Self {
            data,
            offsets,
            data_type: core::marker::PhantomData,
        })
    }

    /// Returns a value at `index`.
    #[inline]
    pub fn get(&self, index: u16) -> Option<T> {
        let offset = self.offsets.get(index)??.to_usize();
        self.data.get(offset..).and_then(T::parse)
    }

    /// Returns array's length.
    #[inline]
    pub const fn len(&self) -> u16 {
        self.offsets.len()
    }

    /// Checks if array is empty.
    #[inline]
    #[allow(dead_code)]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<'a, T: FromSlice<'a> + core::fmt::Debug + Copy> core::fmt::Debug for LazyOffsetArray16<'a, T> {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        f.debug_list().entries(*self).finish()
    }
}

/// An iterator over [`LazyOffsetArray16`] values.
#[derive(Clone, Copy)]
#[allow(missing_debug_implementations)]
pub struct LazyOffsetArrayIter16<'a, T: FromSlice<'a>> {
    array: LazyOffsetArray16<'a, T>,
    index: u16,
}

impl<'a, T: FromSlice<'a>> IntoIterator for LazyOffsetArray16<'a, T> {
    type Item = T;
    type IntoIter = LazyOffsetArrayIter16<'a, T>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        LazyOffsetArrayIter16 {
            array: self,
            index: 0,
        }
    }
}

impl<'a, T: FromSlice<'a>> Iterator for LazyOffsetArrayIter16<'a, T> {
    type Item = T;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        if self.index < self.array.len() {
            let item = self.array.get(self.index);
            self.index += 1;
            item
        } else {
            None
        }
    }

    #[inline]
    fn count(self) -> usize {
        usize::from(self.array.len().saturating_sub(self.index))
    }
}

/// A streaming binary parser.
#[derive(Clone, Default, Debug)]
pub struct Stream<'a> {
    data: &'a [u8],
    offset: usize,
}

impl<'a> Stream<'a> {
    /// Creates a new `Stream` parser.
    #[inline]
    pub const fn new(data: &'a [u8]) -> Self {
        Stream { data, offset: 0 }
    }

    /// Creates a new `Stream` parser at `offset`, or `None` if out of bounds.
    #[inline]
    pub const fn new_at(data: &'a [u8], offset: usize) -> Option<Self> {
        if offset <= data.len() {
            Some(Stream { data, offset })
        } else {
            None
        }
    }

    /// Checks that stream reached the end of the data.
    #[inline]
    pub const fn at_end(&self) -> bool {
        self.offset >= self.data.len()
    }

    /// Jumps to the end of the stream.
    #[inline]
    pub const fn jump_to_end(&mut self) {
        self.offset = self.data.len();
    }

    /// Returns the current offset.
    #[inline]
    pub const fn offset(&self) -> usize {
        self.offset
    }

    /// Returns the trailing data, or `None` past the end of the data.
    #[inline]
    pub fn tail(&self) -> Option<&'a [u8]> {
        self.data.get(self.offset..)
    }

    /// Advances by `FromData::SIZE` without checking bounds.
    #[inline]
    pub const fn skip<T: FromData>(&mut self) {
        self.advance(T::SIZE);
    }

    /// Advances by `len` without checking bounds.
    #[inline]
    pub const fn advance(&mut self, len: usize) {
        self.offset = self.offset.saturating_add(len);
    }

    /// Advances by the specified `len` and checks for bounds.
    #[inline]
    pub const fn advance_checked(&mut self, len: usize) -> Option<()> {
        let Some(end) = self.offset.checked_add(len) else {
            return None;
        };
        if end <= self.data.len() {
            self.advance(len);
            Some(())
        } else {
            None
        }
    }

    /// Parses `T` from the stream.
    #[inline]
    pub fn read<T: FromData>(&mut self) -> Option<T> {
        self.read_bytes(T::SIZE).and_then(T::parse)
    }

    /// Parses the type from the steam at offset.
    #[inline]
    pub fn read_at<T: FromData>(data: &[u8], offset: usize) -> Option<T> {
        data.get(offset..offset.checked_add(T::SIZE)?)
            .and_then(T::parse)
    }

    /// Reads N bytes from the stream.
    #[inline]
    pub fn read_bytes(&mut self, len: usize) -> Option<&'a [u8]> {
        let end = self.offset.checked_add(len)?;
        let v = self.data.get(self.offset..end)?;
        self.advance(len);
        Some(v)
    }

    #[allow(dead_code)]
    #[inline]
    pub fn read_at_offset16(&mut self, data: &'a [u8]) -> Option<&'a [u8]> {
        let offset = self.read::<Offset16>()?.to_usize();
        data.get(offset..)
    }

    /// Reads the next `count` types as a slice.
    #[inline]
    pub fn read_array16<T: FromData>(&mut self, count: u16) -> Option<LazyArray16<'a, T>> {
        let len = usize::from(count) * T::SIZE;
        self.read_bytes(len).map(LazyArray16::new)
    }

    /// Reads the next `count` types as a slice.
    #[inline]
    pub fn read_array32<T: FromData>(&mut self, count: u32) -> Option<LazyArray32<'a, T>> {
        let len = usize::num_from(count).checked_mul(T::SIZE)?;
        self.read_bytes(len).map(LazyArray32::new)
    }
}

/// A common offset methods.
pub trait Offset {
    /// Converts the offset to `usize`.
    fn to_usize(&self) -> usize;
}

/// A type-safe u16 offset.
#[derive(Clone, Copy, Debug)]
pub struct Offset16(pub u16);

impl Offset for Offset16 {
    #[inline]
    fn to_usize(&self) -> usize {
        usize::from(self.0)
    }
}

impl FromData for Offset16 {
    const SIZE: usize = 2;

    #[inline]
    fn parse(data: &[u8]) -> Option<Self> {
        u16::parse(data).map(Offset16)
    }
}

impl FromData for Option<Offset16> {
    const SIZE: usize = Offset16::SIZE;

    #[inline]
    fn parse(data: &[u8]) -> Option<Self> {
        let offset = Offset16::parse(data)?;
        if offset.0 != 0 {
            Some(Some(offset))
        } else {
            Some(None)
        }
    }
}

/// A type-safe u24 offset.
#[derive(Clone, Copy, Debug)]
pub struct Offset24(pub u32);

impl Offset for Offset24 {
    #[inline]
    fn to_usize(&self) -> usize {
        usize::num_from(self.0)
    }
}

impl FromData for Offset24 {
    const SIZE: usize = 3;

    #[inline]
    fn parse(data: &[u8]) -> Option<Self> {
        U24::parse(data).map(|n| Offset24(n.0))
    }
}

impl FromData for Option<Offset24> {
    const SIZE: usize = Offset24::SIZE;

    #[inline]
    fn parse(data: &[u8]) -> Option<Self> {
        let offset = Offset24::parse(data)?;
        if offset.0 != 0 {
            Some(Some(offset))
        } else {
            Some(None)
        }
    }
}

/// A type-safe u32 offset.
#[derive(Clone, Copy, Debug)]
pub struct Offset32(pub u32);

impl Offset for Offset32 {
    #[inline]
    fn to_usize(&self) -> usize {
        usize::num_from(self.0)
    }
}

impl FromData for Offset32 {
    const SIZE: usize = 4;

    #[inline]
    fn parse(data: &[u8]) -> Option<Self> {
        u32::parse(data).map(Offset32)
    }
}

impl FromData for Option<Offset32> {
    const SIZE: usize = Offset32::SIZE;

    #[inline]
    fn parse(data: &[u8]) -> Option<Self> {
        let offset = Offset32::parse(data)?;
        if offset.0 != 0 {
            Some(Some(offset))
        } else {
            Some(None)
        }
    }
}

/// Clamps `val` to `min..=max`.
#[inline]
#[cfg_attr(trust_verify, trust::requires(min <= max))]
#[cfg_attr(trust_verify, trust::ensures(min <= result && result <= max))]
pub const fn i16_bound(min: i16, val: i16, max: i16) -> i16 {
    let upper = if val < max { val } else { max };
    if upper > min { upper } else { min }
}

#[inline]
pub const fn f32_bound(min: f32, val: f32, max: f32) -> f32 {
    // NaN compares false both ways and passes through.
    if val > max {
        return max;
    } else if val < min {
        return min;
    }

    val
}
