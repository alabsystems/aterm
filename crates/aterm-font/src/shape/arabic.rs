// Copyright (c) HarfBuzz developers
// Copyright (c) 2020 Yevhenii Reizner
// SPDX-License-Identifier: MIT
//
// Derived from rustybuzz 0.20.1 src/hb/ot_shaper_arabic.rs and modified by
// the aterm project in 2026.
// See ../../LICENSE-RUSTYBUZZ for the upstream license.

//! The Arabic shaper: joining forms (`isol`, `fina`, `fin2`, `fin3`, `medi`,
//! `med2`, `init`) chosen by the Unicode joining state machine, applied in
//! the feature stages the Arabic spec orders.
//!
//! Joining is computed in buffer order, which is logical order: a caller
//! with visual-order text reverses it first (aterm-render's run planner does,
//! for the right-to-left letters a reordered terminal row paints backwards).
//! Left out:
//! `stch` (Syriac stretching), the mark reordering of modifier combining
//! marks (marks never reach the run shaper), and the presentation-form
//! fallback, which upstream does not implement either.

use super::ShapeFace;
use super::buffer::Buffer;
use super::map::{F_HAS_FALLBACK, F_MANUAL_ZWJ, F_NONE, Map, MapBuilder};
use super::plan::ShapePlan;
use crate::Tag;
use aterm_grapheme::{GeneralCategory, Script};

#[derive(Clone, Copy, PartialEq, PartialOrd, Debug)]
pub(crate) enum JoiningType {
    U = 0,
    L = 1,
    R = 2,
    D = 3,
    GroupAlaph = 4,
    GroupDalathRish = 5,
    T = 7,
    /// Not in the table: the general category decides between U and T.
    X = 8,
}

fn joining_type(u: char, gc: GeneralCategory) -> JoiningType {
    let j = super::arabic_table::joining_type(u);
    if j != JoiningType::X {
        return j;
    }
    match gc {
        GeneralCategory::NonspacingMark
        | GeneralCategory::EnclosingMark
        | GeneralCategory::Format => JoiningType::T,
        _ => JoiningType::U,
    }
}

const ARABIC_FEATURES: [&[u8; 4]; 7] = [
    b"isol", b"fina", b"fin2", b"fin3", b"medi", b"med2", b"init",
];

mod action {
    pub(super) const ISOL: u8 = 0;
    pub(super) const FINA: u8 = 1;
    pub(super) const FIN2: u8 = 2;
    pub(super) const FIN3: u8 = 3;
    pub(super) const MEDI: u8 = 4;
    pub(super) const MED2: u8 = 5;
    pub(super) const INIT: u8 = 6;
    pub(super) const NONE: u8 = 7;
}

use action::{FIN2, FIN3, FINA, INIT, ISOL, MED2, MEDI, NONE};

/// `(previous glyph's action, this glyph's action, next state)`, by state
/// and by joining type U, L, R, D, Alaph, Dalath/Rish.
const STATE_TABLE: [[(u8, u8, u8); 6]; 7] = [
    // 0: the previous was U, not willing to join.
    [
        (NONE, NONE, 0),
        (NONE, ISOL, 2),
        (NONE, ISOL, 1),
        (NONE, ISOL, 2),
        (NONE, ISOL, 1),
        (NONE, ISOL, 6),
    ],
    // 1: the previous was R, or ISOL/ALAPH, not willing to join.
    [
        (NONE, NONE, 0),
        (NONE, ISOL, 2),
        (NONE, ISOL, 1),
        (NONE, ISOL, 2),
        (NONE, FIN2, 5),
        (NONE, ISOL, 6),
    ],
    // 2: the previous was D/L in ISOL form, willing to join.
    [
        (NONE, NONE, 0),
        (NONE, ISOL, 2),
        (INIT, FINA, 1),
        (INIT, FINA, 3),
        (INIT, FINA, 4),
        (INIT, FINA, 6),
    ],
    // 3: the previous was D in FINA form, willing to join.
    [
        (NONE, NONE, 0),
        (NONE, ISOL, 2),
        (MEDI, FINA, 1),
        (MEDI, FINA, 3),
        (MEDI, FINA, 4),
        (MEDI, FINA, 6),
    ],
    // 4: the previous was FINA ALAPH, not willing to join.
    [
        (NONE, NONE, 0),
        (NONE, ISOL, 2),
        (MED2, ISOL, 1),
        (MED2, ISOL, 2),
        (MED2, FIN2, 5),
        (MED2, ISOL, 6),
    ],
    // 5: the previous was FIN2/FIN3 ALAPH, not willing to join.
    [
        (NONE, NONE, 0),
        (NONE, ISOL, 2),
        (ISOL, ISOL, 1),
        (ISOL, ISOL, 2),
        (ISOL, FIN2, 5),
        (ISOL, ISOL, 6),
    ],
    // 6: the previous was DALATH/RISH, not willing to join.
    [
        (NONE, NONE, 0),
        (NONE, ISOL, 2),
        (NONE, ISOL, 1),
        (NONE, ISOL, 2),
        (NONE, FIN3, 5),
        (NONE, ISOL, 6),
    ],
];

/// The joining-form masks, one per action (`NONE`'s is 0).
pub(crate) struct ArabicPlan {
    mask_array: [u32; ARABIC_FEATURES.len() + 1],
}

impl ArabicPlan {
    pub(crate) fn new(map: &Map) -> Self {
        let mut mask_array = [0; ARABIC_FEATURES.len() + 1];
        for (mask, tag) in mask_array.iter_mut().zip(ARABIC_FEATURES) {
            *mask = map.get_1_mask(Tag::from_bytes(tag));
        }
        ArabicPlan { mask_array }
    }
}

/// The Arabic spec's feature order, with a pause after most stages: between
/// the joining forms and `rlig` (required, bugzilla 644184), after `rlig`
/// (IranNastaliq's ALLAH), and after `calt` unless an `rclt` follows
/// (harfbuzz#505, harfbuzz#1573). ZWJ means "do not ligate" in Arabic, so
/// the ligating features run with manual ZWJ.
pub(crate) fn collect_features(map: &mut MapBuilder, script: Option<Script>) {
    let tag = |t: &[u8; 4]| Tag::from_bytes(t);
    map.enable_feature(tag(b"stch"), F_NONE, 1);
    map.add_gsub_pause(None);
    map.enable_feature(tag(b"ccmp"), F_MANUAL_ZWJ, 1);
    map.enable_feature(tag(b"locl"), F_MANUAL_ZWJ, 1);
    map.add_gsub_pause(None);
    for feature in ARABIC_FEATURES {
        // `fin2`, `fin3` and `med2` are Syriac's.
        let syriac = matches!(feature[3], b'2' | b'3');
        let has_fallback = script == Some(Script::ARABIC) && !syriac;
        let flags = if has_fallback { F_HAS_FALLBACK } else { F_NONE };
        map.add_feature(tag(feature), F_MANUAL_ZWJ | flags, 1);
        map.add_gsub_pause(None);
    }
    map.enable_feature(tag(b"rlig"), F_MANUAL_ZWJ | F_HAS_FALLBACK, 1);
    if script == Some(Script::ARABIC) {
        // Upstream's fallback-shaping pause, which shapes nothing.
        map.add_gsub_pause(None);
    }
    map.enable_feature(tag(b"calt"), F_MANUAL_ZWJ, 1);
    if !map.has_feature(tag(b"rclt")) {
        map.add_gsub_pause(None);
    }
    map.enable_feature(tag(b"liga"), F_MANUAL_ZWJ, 1);
    map.enable_feature(tag(b"clig"), F_MANUAL_ZWJ, 1);
    map.enable_feature(tag(b"mset"), F_MANUAL_ZWJ, 1);
}

fn arabic_joining(buffer: &mut Buffer) {
    let mut prev: Option<usize> = None;
    let mut state = 0usize;
    for i in 0..buffer.len {
        let info = &buffer.info[i];
        let this_type = joining_type(info.as_char(), info.gc);
        if this_type == JoiningType::T {
            buffer.info[i].shaper_action = NONE;
            continue;
        }
        let Some(&entry) = STATE_TABLE
            .get(state)
            .and_then(|row| row.get(this_type as usize))
        else {
            break;
        };
        if entry.0 != NONE
            && let Some(p) = prev
        {
            buffer.info[p].shaper_action = entry.0;
        }
        buffer.info[i].shaper_action = entry.1;
        prev = Some(i);
        state = usize::from(entry.2);
    }
}

fn mongolian_variation_selectors(buffer: &mut Buffer) {
    // A Mongolian free variation selector takes its base's joining action.
    for i in 1..buffer.len {
        if (0x180B..=0x180D).contains(&buffer.info[i].glyph_id) || buffer.info[i].glyph_id == 0x180F
        {
            buffer.info[i].shaper_action = buffer.info[i - 1].shaper_action;
        }
    }
}

pub(crate) fn setup_masks(plan: &ShapePlan, _face: &ShapeFace, buffer: &mut Buffer) {
    let Some(arabic) = &plan.arabic else { return };
    arabic_joining(buffer);
    if plan.script == Some(Script::MONGOLIAN) {
        mongolian_variation_selectors(buffer);
    }
    for info in &mut buffer.info[..buffer.len] {
        info.mask |= arabic
            .mask_array
            .get(usize::from(info.shaper_action))
            .copied()
            .unwrap_or(0);
    }
}
