// Copyright (c) Erick Tryzelaar, David Tolnay et al. (the serde authors)
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from serde_core 1.0.228 src/private/string.rs and modified by the
// aterm project in 2026.
// See ../../LICENSE-MIT and ../../LICENSE-APACHE for the upstream licenses.

use crate::lib::*;

#[doc(hidden)]
pub fn from_utf8_lossy(bytes: &[u8]) -> Cow<'_, str> {
    String::from_utf8_lossy(bytes)
}

// The generated code calls this like:
//
//     let value = &_serde::__private::from_utf8_lossy(bytes);
//     Err(_serde::de::Error::unknown_variant(value, VARIANTS))
//
// so it is okay for the return type to be different from the std case as long
// as the above works.
