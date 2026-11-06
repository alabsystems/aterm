// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-window presence view: the story watermarks.

use std::collections::HashMap;

use super::ChipLevel;

/// One window's presence state.
#[derive(Clone, Debug, Default)]
pub struct View {
    /// Newest story sequence seen per session in this window.
    pub watermarks: HashMap<u64, u64>,
}

impl View {
    /// The story watermark this window holds for `session` (`0` if never read).
    #[must_use]
    pub fn watermark(&self, session: u64) -> u64 {
        self.watermarks.get(&session).copied().unwrap_or(0)
    }
}

/// Map a tab's attention indicator bit to [`ChipLevel::Note`] or [`ChipLevel::Off`].
#[must_use]
pub const fn chip_of_attention(attention: bool) -> ChipLevel {
    if attention {
        ChipLevel::Note
    } else {
        ChipLevel::Off
    }
}
