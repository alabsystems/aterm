// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded regular-expression row matchers and wire codecs for surface watchers.

use std::sync::Arc;

use aterm_core::terminal::{RowMatch, RowVerdict};

/// Re-exports the regex compile error type for downstream crates.
pub mod regex_compile_error {
    pub use aterm_regex::Error;
}

/// Pre-compiled regular-expression row matcher.
#[derive(Debug)]
pub struct RegexRowMatch {
    re: aterm_regex::Regex,
}

impl RowMatch for RegexRowMatch {
    #[inline]
    fn verdict(&self, row: &str) -> RowVerdict {
        match self.re.try_is_match(row) {
            Ok(matched) => matched.into(),
            Err(_) => RowVerdict::Undecided,
        }
    }
}

const MAX_REGEX_PATTERN_LEN: usize = 1024;
const REGEX_SIZE_LIMIT: usize = 128 * 1024;
const REGEX_STEP_LIMIT: u64 = 1 << 22;

/// Compiles a bounded regex row matcher for `watch_rows` and `watch_rows_gone`.
/// # Errors
/// Returns an [`aterm_regex::Error`] if `pattern` exceeds length/size limits or is invalid.
pub fn row_matcher(pattern: &str) -> Result<Arc<dyn RowMatch>, aterm_regex::Error> {
    Ok(Arc::new(RegexRowMatch {
        re: bounded_regex(pattern)?,
    }))
}

fn bounded_regex(pattern: &str) -> Result<aterm_regex::Regex, aterm_regex::Error> {
    if pattern.len() > MAX_REGEX_PATTERN_LEN {
        return Err(aterm_regex::Error::Syntax(format!(
            "pattern exceeds maximum length ({MAX_REGEX_PATTERN_LEN} bytes)"
        )));
    }
    aterm_regex::RegexBuilder::new(pattern)
        .size_limit(REGEX_SIZE_LIMIT)
        .step_limit(REGEX_STEP_LIMIT)
        .build()
}

/// Wire size in bytes of an encoded watcher regex budget frame (`"OBSB"`).
pub const WATCHER_BUDGET_WIRE_BYTES: usize = 20;

/// Wire size in bytes of an encoded row-matcher verdict frame (`"OBSV"`).
pub const ROW_VERDICT_WIRE_BYTES: usize = 16;

#[inline]
const fn u16_le(b0: u8, b1: u8) -> u16 {
    (b0 as u16) | ((b1 as u16) << 8)
}

#[inline]
const fn u32_le(b0: u8, b1: u8, b2: u8, b3: u8) -> u32 {
    (b0 as u32) | ((b1 as u32) << 8) | ((b2 as u32) << 16) | ((b3 as u32) << 24)
}

#[inline]
#[expect(
    clippy::too_many_arguments,
    reason = "eight bytes as shifts, not `from_*_bytes`, so the Trust lane proves it (e9568a13f7)"
)]
const fn u64_le(b0: u8, b1: u8, b2: u8, b3: u8, b4: u8, b5: u8, b6: u8, b7: u8) -> u64 {
    (u32_le(b0, b1, b2, b3) as u64) | ((u32_le(b4, b5, b6, b7) as u64) << 32)
}

/// Encode a 20-byte watcher regex budget wire frame (`"OBSB"`).
#[inline]
#[must_use]
pub const fn encode_watcher_budget_wire(
    max_pattern_len: u32,
    size_limit: u32,
    step_limit: u64,
) -> [u8; WATCHER_BUDGET_WIRE_BYTES] {
    let pb = max_pattern_len.to_le_bytes();
    let sb = size_limit.to_le_bytes();
    let lb = step_limit.to_le_bytes();
    [
        b'O', b'B', b'S', b'B', pb[0], pb[1], pb[2], pb[3], sb[0], sb[1], sb[2], sb[3], lb[0],
        lb[1], lb[2], lb[3], lb[4], lb[5], lb[6], lb[7],
    ]
}

/// Decode a 20-byte watcher regex budget wire frame produced by [`encode_watcher_budget_wire`].
#[inline]
#[must_use]
pub const fn decode_watcher_budget_wire(bytes: &[u8]) -> Option<(u32, u32, u64)> {
    if bytes.len() != WATCHER_BUDGET_WIRE_BYTES {
        return None;
    }
    if bytes[0] != b'O' || bytes[1] != b'B' || bytes[2] != b'S' || bytes[3] != b'B' {
        return None;
    }
    let max_pattern_len = u32_le(bytes[4], bytes[5], bytes[6], bytes[7]);
    let size_limit = u32_le(bytes[8], bytes[9], bytes[10], bytes[11]);
    let step_limit = u64_le(
        bytes[12], bytes[13], bytes[14], bytes[15], bytes[16], bytes[17], bytes[18], bytes[19],
    );
    if max_pattern_len == 0
        || (max_pattern_len as usize) > MAX_REGEX_PATTERN_LEN
        || size_limit == 0
        || (size_limit as usize) > REGEX_SIZE_LIMIT
        || step_limit == 0
        || step_limit > REGEX_STEP_LIMIT
    {
        return None;
    }
    Some((max_pattern_len, size_limit, step_limit))
}

/// Encode a 16-byte row-matcher verdict wire frame (`"OBSV"`).
#[inline]
#[must_use]
pub const fn encode_row_verdict_wire(
    verdict: RowVerdict,
    row_idx: u16,
    pattern_len: u8,
    seq: u64,
) -> [u8; ROW_VERDICT_WIRE_BYTES] {
    let rb = row_idx.to_le_bytes();
    let sb = seq.to_le_bytes();
    let v_byte = match verdict {
        RowVerdict::NoMatch => 0u8,
        RowVerdict::Match => 1u8,
        RowVerdict::Undecided => 2u8,
    };
    [
        b'O',
        b'B',
        b'S',
        b'V',
        v_byte,
        pattern_len,
        rb[0],
        rb[1],
        sb[0],
        sb[1],
        sb[2],
        sb[3],
        sb[4],
        sb[5],
        sb[6],
        sb[7],
    ]
}

/// Decode a 16-byte row-matcher verdict wire frame produced by [`encode_row_verdict_wire`].
#[inline]
#[must_use]
pub const fn decode_row_verdict_wire(bytes: &[u8]) -> Option<(RowVerdict, u16, u8, u64)> {
    if bytes.len() != ROW_VERDICT_WIRE_BYTES {
        return None;
    }
    if bytes[0] != b'O' || bytes[1] != b'B' || bytes[2] != b'S' || bytes[3] != b'V' {
        return None;
    }
    let verdict = match bytes[4] {
        0 => RowVerdict::NoMatch,
        1 => RowVerdict::Match,
        2 => RowVerdict::Undecided,
        _ => return None,
    };
    let pattern_len = bytes[5];
    let row_idx = u16_le(bytes[6], bytes[7]);
    let seq = u64_le(
        bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
    );
    Some((verdict, row_idx, pattern_len, seq))
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
