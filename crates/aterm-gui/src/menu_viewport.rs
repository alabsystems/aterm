// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Logical row window shared by popup painting, navigation and hit testing.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct MenuViewport {
    first: usize,
}

impl MenuViewport {
    pub(crate) const fn first(self) -> usize {
        self.first
    }

    pub(crate) fn range(self, total: usize, shown: usize) -> std::ops::Range<usize> {
        if shown == 0 {
            return 0..0;
        }
        let first = self.first().min(total.saturating_sub(shown));
        first..first.saturating_add(shown).min(total)
    }

    pub(crate) fn absolute(self, total: usize, shown: usize, visible: usize) -> Option<usize> {
        self.range(total, shown).nth(visible)
    }

    /// Clamp after a row/geometry change, then reveal a valid selected row.
    pub(crate) fn reveal(&mut self, total: usize, shown: usize, selected: Option<usize>) -> bool {
        let before = *self;
        self.first = self.range(total, shown).start;
        if shown > 0
            && let Some(selected) = selected.filter(|row| *row < total)
        {
            if selected < self.first {
                self.first = selected;
            } else if selected - self.first >= shown {
                self.first = selected.saturating_add(1).saturating_sub(shown);
            }
        }
        *self != before
    }

    /// Move through logical rows without wrapping or leaving the available window.
    pub(crate) fn scroll(&mut self, total: usize, shown: usize, delta: isize) -> bool {
        let before = *self;
        self.first = if shown == 0 {
            0
        } else {
            self.range(total, shown)
                .start
                .saturating_add_signed(delta)
                .min(total.saturating_sub(shown))
        };
        *self != before
    }
}
