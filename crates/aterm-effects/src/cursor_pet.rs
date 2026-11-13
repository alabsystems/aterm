// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `cursor_pet` table: which animal keeps the cursor company, and whether it walks or flies.
//!
//! The pet is its own setting. It names an animal by a row of [`PETS`], whatever the cursor
//! trail style is, the plain cursor (`cursor_trail_style = "off"`) included. A pet is a name in
//! one table, so another pet is another row and not another key. How a pet moves is part of
//! its row: a walker lives beside the cursor, and a flier is a head that flies in at the
//! cursor once typing gathers speed.

use crate::cursor_glow::spellings::{eq_ascii_ci, trim_whitespace_bytes};
use crate::kitty_pet::PetSpecies;

/// One `cursor_pet` value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PetRow {
    /// The lower-case spelling aterm.toml holds.
    pub name: &'static str,
    /// The animal that walks beside the cursor, or `None` for `off` and for a flier.
    pub species: Option<PetSpecies>,
    /// The pet is the flying head ([`crate::kitty_cursor`]): nothing walks, and the rainbow
    /// trail's momentum flies the head in at the cursor.
    pub flies: bool,
    /// Display label shown in Settings.
    pub label: &'static str,
    /// One-line description for Settings and accessibility.
    pub blurb: &'static str,
}

/// The pets, in the order Settings lists them.
pub const PETS: &[PetRow] = &[
    PetRow {
        name: "off",
        species: None,
        flies: false,
        label: "Off",
        blurb: "No pet",
    },
    PetRow {
        name: "cat",
        species: Some(PetSpecies::Cat),
        flies: false,
        label: "Cat",
        blurb: "A cat walks beside your cursor",
    },
    PetRow {
        name: "dog",
        species: Some(PetSpecies::Dog),
        flies: false,
        label: "Dog",
        blurb: "A dog walks beside your cursor",
    },
    PetRow {
        name: FLYING_CAT,
        species: None,
        flies: true,
        label: "Flying cat",
        blurb: "A cat flies in at the head of the rainbow once your typing gathers speed",
    },
];

#[inline]
const fn pet_names<const N: usize>() -> [&'static str; N] {
    let mut out = [""; N];
    let mut i = 0;
    while i < N {
        out[i] = PETS[i].name;
        i += 1;
    }
    out
}

/// The [`PETS`] names in table order.
pub const PET_NAMES: [&str; PETS.len()] = pet_names();

/// The pet a new aterm.toml keeps.
pub const DEFAULT_PET: &str = "cat";

/// The flier's name: the pet the retired flying styles became.
pub const FLYING_CAT: &str = "flying cat";

/// The [`PETS`] row `raw` names (trimmed, ASCII case-insensitive).
#[inline]
#[must_use]
pub const fn row(raw: &str) -> Option<&'static PetRow> {
    let raw = trim_whitespace_bytes(raw);
    let mut i = 0;
    while i < PETS.len() {
        if eq_ascii_ci(raw, PETS[i].name) {
            return Some(&PETS[i]);
        }
        i += 1;
    }
    None
}

/// The [`PETS`] name `raw` resolves to.
#[inline]
#[must_use]
pub const fn canonical(raw: &str) -> Option<&'static str> {
    match row(raw) {
        Some(r) => Some(r.name),
        None => None,
    }
}

/// The animal `raw` keeps walking beside the cursor: `None` for `off`, for a flier and for a
/// name no row has.
#[inline]
#[must_use]
pub const fn species(raw: &str) -> Option<PetSpecies> {
    match row(raw) {
        Some(r) => r.species,
        None => None,
    }
}

/// Whether `raw` names a flier.
#[inline]
#[must_use]
pub const fn flies(raw: &str) -> bool {
    match row(raw) {
        Some(r) => r.flies,
        None => false,
    }
}

#[cfg(test)]
#[path = "cursor_pet_tests.rs"]
mod tests;
