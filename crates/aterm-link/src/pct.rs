// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The one escape the control protocol and the fabric body share
//! (`aterm_control::wire` re-exports it as `pct_encode`/`pct_decode`).
//! [`encode`] writes every byte that is not an ASCII graphic, and `%` itself,
//! as UPPERCASE `%XX`; [`decode`] is TOTAL (a malformed escape passes through
//! verbatim, invalid UTF-8 decodes lossily), so it is safe on bytes a hostile
//! peer authored.

pub use aterm_codec::pct::{decode, encode};
