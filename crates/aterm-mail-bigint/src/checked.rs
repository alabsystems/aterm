// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Checked arithmetic.

use subtle::{Choice, ConditionallySelectable, ConstantTimeEq, CtOption};

#[cfg(feature = "serde")]
use serdect::serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Provides intentionally-checked arithmetic on `T`.
///
/// Internally this leverages the [`CtOption`] type from the [`subtle`] crate
/// in order to handle overflows.
#[derive(Copy, Clone, Debug)]
pub struct Checked<T>(pub CtOption<T>);

impl<T> Checked<T> {
    /// Create a new checked arithmetic wrapper for the given value.
    pub fn new(val: T) -> Self {
        Self(CtOption::new(val, Choice::from(1)))
    }
}

impl<T> Default for Checked<T>
where
    T: Default,
{
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T: ConditionallySelectable> ConditionallySelectable for Checked<T> {
    #[inline]
    fn conditional_select(a: &Self, b: &Self, choice: Choice) -> Self {
        Self(CtOption::conditional_select(&a.0, &b.0, choice))
    }
}

impl<T: ConstantTimeEq> ConstantTimeEq for Checked<T> {
    #[inline]
    fn ct_eq(&self, rhs: &Self) -> Choice {
        self.0.ct_eq(&rhs.0)
    }
}

impl<T> From<Checked<T>> for CtOption<T> {
    fn from(checked: Checked<T>) -> CtOption<T> {
        checked.0
    }
}

impl<T> From<CtOption<T>> for Checked<T> {
    fn from(ct_option: CtOption<T>) -> Checked<T> {
        Checked(ct_option)
    }
}

impl<T> From<Checked<T>> for Option<T> {
    fn from(checked: Checked<T>) -> Option<T> {
        checked.0.into()
    }
}

#[cfg(feature = "serde")]
impl<'de, T: Default + Deserialize<'de>> Deserialize<'de> for Checked<T> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Option::<T>::deserialize(deserializer)?;
        let choice = Choice::from(value.is_some() as u8);
        Ok(Self(CtOption::new(value.unwrap_or_default(), choice)))
    }
}

#[cfg(feature = "serde")]
impl<T: Copy + Serialize> Serialize for Checked<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        Option::<T>::from(self.0).serialize(serializer)
    }
}

#[cfg(all(test, feature = "serde"))]
#[allow(clippy::unwrap_used)]
#[path = "checked_tests.rs"]
mod tests;
