// Copyright (c) Erick Tryzelaar, David Tolnay et al. (the serde authors)
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from serde_core 1.0.228 src/private/size_hint.rs and modified by the
// aterm project in 2026.
// See ../../LICENSE-MIT and ../../LICENSE-APACHE for the upstream licenses.

use crate::lib::*;

pub fn from_bounds<I>(iter: &I) -> Option<usize>
where
    I: Iterator,
{
    helper(iter.size_hint())
}

pub fn cautious<Element>(hint: Option<usize>) -> usize {
    const MAX_PREALLOC_BYTES: usize = 1024 * 1024;

    if mem::size_of::<Element>() == 0 {
        0
    } else {
        cmp::min(
            hint.unwrap_or(0),
            MAX_PREALLOC_BYTES / mem::size_of::<Element>(),
        )
    }
}

fn helper(bounds: (usize, Option<usize>)) -> Option<usize> {
    match bounds {
        (lower, Some(upper)) if lower == upper => Some(upper),
        _ => None,
    }
}
