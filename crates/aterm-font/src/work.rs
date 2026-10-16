// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

/// One outline query's cumulative expansion budget. Nesting caps bound the
/// stack, but an acyclic DAG can visit its children exponentially often.
/// Charge every glyph entry, TrueType point and CFF instruction across all
/// siblings/subroutines, rather than granting each recursive call a new
/// allowance. Partial sink output is possible before refusal, as for any
/// malformed outline.
pub(crate) struct OutlineWork(Option<u32>);

impl Default for OutlineWork {
    fn default() -> Self {
        Self(Some(65_536))
    }
}

impl OutlineWork {
    pub(crate) fn take(&mut self) -> Option<()> {
        self.0 = self.0?.checked_sub(1);
        self.0.map(|_| ())
    }

    pub(crate) fn exhausted(&self) -> bool {
        self.0.is_none()
    }
}
