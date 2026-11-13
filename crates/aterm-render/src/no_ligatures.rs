// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A face with its programming ligatures taken out.
//!
//! The build script cuts Windows' default faces from the bundled Iosevka Term files with this
//! (`build.rs` names it by path, so it uses `std` alone), and the crate's tests hold the cut to
//! what this file says it is.

/// The `GSUB` features that turn `!=`, `->` and `>=` into other glyphs.
pub const LIGATURE_FEATURES: [[u8; 4]; 2] = [*b"calt", *b"liga"];

/// The tag a taken-out feature is left under. No shaper asks for it: a registered feature is
/// lowercase, and this is the upper-case private spelling the OpenType registry leaves to fonts.
pub const TAKEN_OUT: [u8; 4] = *b"NOLG";

/// `font` (one TrueType or OpenType face, not a collection) with every [`LIGATURE_FEATURES`]
/// record of its `GSUB` feature list renamed to [`TAKEN_OUT`], so the lookups stay and nothing
/// reaches them. Nothing else changes: same length, same glyphs, same tables. `None` where
/// `font` holds no such record or is not laid out as a face with a `GSUB` table.
#[must_use]
pub fn without_ligature_features(font: &[u8]) -> Option<Vec<u8>> {
    let u16_at = |at: usize| -> Option<usize> {
        let bytes = font.get(at..at.checked_add(2)?)?;
        Some(usize::from(u16::from_be_bytes([bytes[0], bytes[1]])))
    };
    let u32_at = |at: usize| -> Option<usize> {
        let bytes = font.get(at..at.checked_add(4)?)?;
        usize::try_from(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])).ok()
    };
    let tables = u16_at(4)?;
    let gsub = (0..tables)
        .map(|table| 12 + 16 * table)
        .find(|&record| font.get(record..record + 4) == Some(b"GSUB".as_slice()))
        .and_then(|record| u32_at(record + 8))?;
    let features = gsub.checked_add(u16_at(gsub.checked_add(6)?)?)?;
    let count = u16_at(features)?;
    let mut cut = font.to_vec();
    let mut taken = 0usize;
    for record in (0..count).map(|feature| features + 2 + 6 * feature) {
        let tag = cut.get_mut(record..record.checked_add(4)?)?;
        if LIGATURE_FEATURES.iter().any(|feature| tag == feature) {
            tag.copy_from_slice(&TAKEN_OUT);
            taken += 1;
        }
    }
    (taken > 0).then_some(cut)
}
