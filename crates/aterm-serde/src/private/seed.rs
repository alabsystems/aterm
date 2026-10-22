// Copyright (c) Erick Tryzelaar, David Tolnay et al. (the serde authors)
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from serde_core 1.0.228 src/private/seed.rs and modified by the
// aterm project in 2026.
// See ../../LICENSE-MIT and ../../LICENSE-APACHE for the upstream licenses.

use crate::de::{Deserialize, DeserializeSeed, Deserializer};

/// A DeserializeSeed helper for implementing deserialize_in_place Visitors.
///
/// Wraps a mutable reference and calls deserialize_in_place on it.
pub struct InPlaceSeed<'a, T: 'a>(pub &'a mut T);

#[diagnostic::do_not_recommend]
impl<'a, 'de, T> DeserializeSeed<'de> for InPlaceSeed<'a, T>
where
    T: Deserialize<'de>,
{
    type Value = ();
    #[inline]
    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        T::deserialize_in_place(deserializer, self.0)
    }
}
