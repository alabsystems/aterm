// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What is left of an account's window, as a person reads it: the one rule of every surface
//! (owner, 2026-11-09, asked "used or left": "left everywhere"; like a battery,
//! `docs/DESIGN-account-discovery-2026-11-09.md` decision 8).
//!
//! The store, the journal, the wire and every JSON field record *use*, in tenths of a percent or
//! whole percent, and stay so. A surface a person reads turns that use into the share left here,
//! at the moment it prints, and always says the word: no bare percentage, whose direction a
//! person would have to guess beside a context figure that counts down.
//!
//! | Stored | Shown | Rule |
//! |---|---|---|
//! | an exact reading | `37% left` | [`exact`]: 100 − the whole percent used |
//! | a lower bound of use | `≤38% left` | [`at_most`]: 100 − use rounded down, an upper bound |
//! | a projected fill short of 100 | `~1% left` | [`ahead`]: never `~0% left` |
//! | a standing wall, a projected fill | `out` | a word, never an inferred `0% left` |
//! | unread | `?` | |
//!
//! `0% left` is printed only for a reading of 100, as `100% used` was. A wall keeps the word `out`
//! because it is the vendor's refusal, not a reading: it can stand on a window last read at 97%,
//! and `0% left` there would be an inferred figure. A rate of use (`21% a day`) is not a share
//! left and has no mirror; it says `used` beside its figure.

/// The word after every share left.
pub const WORD: &str = "left";

/// Whole percent left of an exact reading of `used_x10` tenths used: 100 − the whole percent
/// used ([`super::decimal::whole_used`]: half up, 100 only for a reading of 100), so what aterm
/// says is left and what the vendor's own panel says is used add to 100, and `0` is said only
/// for a reading of 100.
#[must_use]
pub const fn exact(used_x10: u32) -> u32 {
    100 - super::decimal::whole_used(if used_x10 > 1000 { 1000 } else { used_x10 })
}

/// Whole percent left, at most, where `used_x10` tenths is a lower bound of use (an older
/// reading: use only rises within a window): 100 − the use rounded down, so the bound stays
/// true (62.5% used is `≤38% left`, never `≤37%`).
#[must_use]
pub const fn at_most(used_x10: u32) -> u32 {
    100 - (if used_x10 > 1000 { 1000 } else { used_x10 }) / 10
}

/// Whole percent left where a projection holds `used_x10` tenths used and has not filled: at
/// least 1, since a curve short of a fill is never said `0% left` (a fill says `out`).
#[must_use]
pub const fn ahead(used_x10: u32) -> u32 {
    let left = exact(used_x10);
    if left == 0 { 1 } else { left }
}

/// Whole percent left of a whole percent used, as a vendor panel or a stored window gives one.
#[must_use]
pub const fn of_whole(used_pct: u32) -> u32 {
    100 - if used_pct > 100 { 100 } else { used_pct }
}

/// What is left of `used_x10` tenths used, to the tenth, without its percent sign: `57.5`,
/// `57` where the tenth is zero.
#[must_use]
pub fn tenths(used_x10: u32) -> String {
    let left = 1000 - used_x10.min(1000);
    if left.is_multiple_of(10) {
        format!("{}", left / 10)
    } else {
        format!("{}.{}", left / 10, left % 10)
    }
}

/// An exact reading's words: `37% left`.
#[must_use]
pub fn words(used_x10: u32) -> String {
    format!("{}% {WORD}", exact(used_x10))
}

/// A lower bound of use as its words: `≤38% left`.
#[must_use]
pub fn bound_words(used_x10: u32) -> String {
    format!("\u{2264}{}% {WORD}", at_most(used_x10))
}

#[cfg(test)]
#[path = "left_tests.rs"]
mod tests;
