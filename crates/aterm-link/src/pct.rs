// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The percent escape the control protocol and the fabric body share: [`encode`] writes every
//! byte that is not ASCII-graphic, and `%`, as uppercase `%XX`; [`decode`] is total, so it is
//! safe on hostile bytes. The implementation is `aterm_codec::pct`.

pub use aterm_codec::pct::{decode, encode};
