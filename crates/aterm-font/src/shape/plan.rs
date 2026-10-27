// Copyright (c) HarfBuzz developers
// Copyright (c) 2020 Yevhenii Reizner
// SPDX-License-Identifier: MIT
//
// Derived from rustybuzz 0.20.1 src/hb/ot_shape_plan.rs, src/hb/ot_shape.rs
// (`hb_ot_shape_planner_t`) and src/hb/ot_shaper.rs and modified by the aterm
// project in 2026.
// See ../../LICENSE-RUSTYBUZZ for the upstream license.

//! Compiled OpenType feature shaping plan for one face, direction, and script.

use super::ShapeFace;
use super::arabic;
use super::common::{Direction, script_tags};
use super::map::{
    F_GLOBAL, F_GLOBAL_HAS_FALLBACK, F_GLOBAL_MANUAL_JOINERS, F_GLOBAL_SEARCH, F_NONE, F_RANDOM,
    MAX_VALUE, Map, MapBuilder,
};
use crate::Tag;
use aterm_grapheme::Script;

/// A feature over the whole run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Feature {
    /// The OpenType feature tag.
    pub tag: Tag,
    /// Its value: 0 turns it off, 1 on, higher values pick an alternate.
    pub value: u32,
}

impl Feature {
    /// `tag` at `value` over the whole run.
    #[inline]
    #[must_use]
    pub const fn new(tag: Tag, value: u32) -> Self {
        Feature { tag, value }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Shaper {
    Default,
    Arabic,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ZeroWidthMarks {
    None,
    ByGdefEarly,
    ByGdefLate,
}

/// A reusable plan for shaping runs of one direction and script on one face.
pub struct ShapePlan {
    pub(crate) direction: Direction,
    pub(crate) script: Option<Script>,
    pub(crate) shaper: Shaper,
    pub(crate) map: Map,
    pub(crate) arabic: Option<arabic::ArabicPlan>,
    pub(crate) frac_mask: u32,
    pub(crate) numr_mask: u32,
    pub(crate) dnom_mask: u32,
    pub(crate) rtlm_mask: u32,
    pub(crate) kern_mask: u32,
    pub(crate) requested_kerning: bool,
    pub(crate) has_frac: bool,
    pub(crate) fallback_glyph_classes: bool,
    pub(crate) zero_marks: bool,
    pub(crate) zero_width_marks: ZeroWidthMarks,
    pub(crate) fallback_mark_positioning: bool,
    pub(crate) adjust_mark_positioning_when_zeroing: bool,
    pub(crate) apply_gpos: bool,
    pub(crate) apply_kerx: bool,
    pub(crate) apply_kern: bool,
}

impl ShapePlan {
    /// A plan for `direction` and `script` (`None` when the run has none but Common and Inherited
    /// characters) on `face`, with `features` applied over every run after the default ones (a
    /// later feature overrides an earlier one with the same tag).
    pub fn new(
        face: &ShapeFace,
        direction: Direction,
        script: Option<Script>,
        features: &[Feature],
    ) -> Self {
        let direction = if direction == Direction::Invalid {
            Direction::LeftToRight
        } else {
            direction
        };
        let tags = script_tags(script);
        let mut map = MapBuilder::new(face, &tags);
        let chosen_gsub = map.chosen_script_gsub();
        let shaper = match script {
            Some(s) => categorize(s, direction, chosen_gsub),
            None => Shaper::Default,
        };
        let (zero_width_marks, script_fallback_mark_positioning) =
            script_mark_policy(script, chosen_gsub);
        let script_zero_marks = zero_width_marks != ZeroWidthMarks::None;
        let chosen_gpos = map.chosen_script_gpos();
        collect_features(&mut map, shaper, direction, script, features);
        let map = map.compile();
        let frac_mask = map.get_1_mask(Tag::from_bytes(b"frac"));
        let numr_mask = map.get_1_mask(Tag::from_bytes(b"numr"));
        let dnom_mask = map.get_1_mask(Tag::from_bytes(b"dnom"));
        let has_frac = frac_mask != 0 || (numr_mask != 0 && dnom_mask != 0);
        let rtlm_mask = map.get_1_mask(Tag::from_bytes(b"rtlm"));
        let kern_tag = if direction.is_horizontal() {
            Tag::from_bytes(b"kern")
        } else {
            Tag::from_bytes(b"vkrn")
        };
        let kern_mask = map.get_mask(kern_tag);
        let requested_kerning = kern_mask != 0;
        let has_gpos_kern = map.get_feature_index(super::map::GPOS, kern_tag).is_some();
        let arabic = (shaper == Shaper::Arabic).then(|| arabic::ArabicPlan::new(&map));
        let disable_gpos = script.is_some_and(|s| &s.0 == b"Hebr")
            && chosen_gpos != Some(Tag::from_bytes(b"hebr"));
        let has_kerx = face.face.tables().kerx.is_some();
        let has_gsub = face.face.tables().gsub.is_some();
        let has_gpos = !disable_gpos && face.face.tables().gpos.is_some();

        let mut apply_gpos = false;
        let mut apply_kerx = false;
        let mut apply_kern = false;
        if has_kerx && !(has_gsub && has_gpos) {
            apply_kerx = true;
        } else if has_gpos {
            apply_gpos = true;
        }
        if !apply_kerx && (!has_gpos_kern || !apply_gpos) {
            if has_kerx {
                apply_kerx = true;
            } else if face.face.tables().kern.is_some() {
                apply_kern = true;
            }
        }

        let has_machine_kerning = face
            .face
            .tables()
            .kern
            .is_some_and(|k| k.subtables.into_iter().any(|s| s.has_state_machine));
        let has_cross_kerning = face
            .face
            .tables()
            .kern
            .is_some_and(|k| k.subtables.into_iter().any(|s| s.has_cross_stream));
        let zero_marks = script_zero_marks && !apply_kerx && (!apply_kern || !has_machine_kerning);
        let adjust_mark_positioning_when_zeroing =
            !apply_gpos && !apply_kerx && (!apply_kern || !has_cross_kerning);
        let fallback_mark_positioning =
            adjust_mark_positioning_when_zeroing && script_fallback_mark_positioning;

        ShapePlan {
            direction,
            script,
            shaper,
            map,
            arabic,
            frac_mask,
            numr_mask,
            dnom_mask,
            rtlm_mask,
            kern_mask,
            requested_kerning,
            has_frac,
            fallback_glyph_classes: !face.has_glyph_classes(),
            zero_marks,
            zero_width_marks,
            fallback_mark_positioning,
            adjust_mark_positioning_when_zeroing,
            apply_gpos,
            apply_kerx,
            apply_kern,
        }
    }

    /// The direction the plan shapes in.
    #[inline]
    #[must_use]
    pub const fn direction(&self) -> Direction {
        self.direction
    }

    /// The script the plan shapes, if any.
    #[inline]
    #[must_use]
    pub const fn script(&self) -> Option<Script> {
        self.script
    }
}

/// Which shaper a script uses.
#[inline]
fn categorize(script: Script, direction: Direction, gsub_script: Option<Tag>) -> Shaper {
    match &script.0 {
        b"Arab" | b"Syrc" => {
            if (gsub_script != Some(Tag::from_bytes(b"DFLT")) || script == Script::ARABIC)
                && direction.is_horizontal()
            {
                Shaper::Arabic
            } else {
                Shaper::Default
            }
        }
        _ => Shaper::Default,
    }
}

/// Mark advance zeroing and fallback mark positioning policy for `script` and `gsub_script`,
/// matching `hb_ot_shape_complex_categorize` in `rustybuzz`.
fn script_mark_policy(script: Option<Script>, gsub_script: Option<Tag>) -> (ZeroWidthMarks, bool) {
    let Some(script) = script else {
        return (ZeroWidthMarks::ByGdefLate, true);
    };
    let is_dflt_or_latn = gsub_script == Some(Tag::from_bytes(b"DFLT"))
        || gsub_script == Some(Tag::from_bytes(b"latn"));
    match &script.0 {
        b"Thai" | b"Laoo" => (ZeroWidthMarks::ByGdefLate, false),
        b"Hang" | b"Khmr" | b"Qaag" => (ZeroWidthMarks::None, false),
        b"Beng" | b"Deva" | b"Gujr" | b"Guru" | b"Knda" | b"Mlym" | b"Orya" | b"Taml" | b"Telu" => {
            if is_dflt_or_latn {
                (ZeroWidthMarks::ByGdefLate, true)
            } else if gsub_script.is_some_and(|t| t.to_bytes()[3] == b'3') {
                (ZeroWidthMarks::ByGdefEarly, false)
            } else {
                (ZeroWidthMarks::None, false)
            }
        }
        b"Mymr" => {
            if is_dflt_or_latn || gsub_script == Some(Tag::from_bytes(b"mymr")) {
                (ZeroWidthMarks::ByGdefLate, true)
            } else {
                (ZeroWidthMarks::ByGdefEarly, false)
            }
        }
        b"Tibt" | b"Mong" | b"Sinh" | b"Buhd" | b"Hano" | b"Tglg" | b"Tagb" | b"Limb" | b"Tale"
        | b"Bugi" | b"Khar" | b"Sylo" | b"Tfng" | b"Bali" | b"Nkoo" | b"Phag" | b"Cham"
        | b"Kali" | b"Lepc" | b"Rjng" | b"Saur" | b"Sund" | b"Egyp" | b"Java" | b"Kthi"
        | b"Mtei" | b"Lana" | b"Tavt" | b"Batk" | b"Brah" | b"Mand" | b"Cakm" | b"Plrd"
        | b"Shrd" | b"Takr" | b"Dupl" | b"Gran" | b"Khoj" | b"Sind" | b"Mahj" | b"Mani"
        | b"Modi" | b"Hmng" | b"Phlp" | b"Sidd" | b"Tirh" | b"Ahom" | b"Mult" | b"Adlm"
        | b"Bhks" | b"Marc" | b"Newa" | b"Gonm" | b"Soyo" | b"Zanb" | b"Dogr" | b"Gong"
        | b"Rohg" | b"Maka" | b"Medf" | b"Sogo" | b"Sogd" | b"Elym" | b"Nand" | b"Hmnp"
        | b"Wcho" | b"Chrs" | b"Diak" | b"Kits" | b"Yezi" | b"Cpmn" | b"Ougr" | b"Tnsa"
        | b"Toto" | b"Vith" | b"Kawi" | b"Nagm" | b"Gara" | b"Gukh" | b"Krai" | b"Onao"
        | b"Sunu" | b"Todr" | b"Tutg" => {
            if is_dflt_or_latn {
                (ZeroWidthMarks::ByGdefLate, true)
            } else {
                (ZeroWidthMarks::ByGdefEarly, false)
            }
        }
        _ => (ZeroWidthMarks::ByGdefLate, true),
    }
}

fn collect_features(
    map: &mut MapBuilder,
    shaper: Shaper,
    direction: Direction,
    script: Option<Script>,
    user_features: &[Feature],
) {
    const COMMON_FEATURES: &[(&[u8; 4], u32)] = &[
        (b"abvm", F_GLOBAL),
        (b"blwm", F_GLOBAL),
        (b"ccmp", F_GLOBAL),
        (b"locl", F_GLOBAL),
        (b"mark", F_GLOBAL_MANUAL_JOINERS),
        (b"mkmk", F_GLOBAL_MANUAL_JOINERS),
        (b"rlig", F_GLOBAL),
    ];
    const HORIZONTAL_FEATURES: &[(&[u8; 4], u32)] = &[
        (b"calt", F_GLOBAL),
        (b"clig", F_GLOBAL),
        (b"curs", F_GLOBAL),
        (b"dist", F_GLOBAL),
        (b"kern", F_GLOBAL_HAS_FALLBACK),
        (b"liga", F_GLOBAL),
        (b"rclt", F_GLOBAL),
    ];
    let tag = |t: &[u8; 4]| Tag::from_bytes(t);

    map.is_simple = true;
    map.enable_feature(tag(b"rvrn"), F_NONE, 1);
    map.add_gsub_pause(None);
    match direction {
        Direction::LeftToRight => {
            map.enable_feature(tag(b"ltra"), F_NONE, 1);
            map.enable_feature(tag(b"ltrm"), F_NONE, 1);
        }
        Direction::RightToLeft => {
            map.enable_feature(tag(b"rtla"), F_NONE, 1);
            map.add_feature(tag(b"rtlm"), F_NONE, 1);
        }
        _ => {}
    }
    map.add_feature(tag(b"frac"), F_NONE, 1);
    map.add_feature(tag(b"numr"), F_NONE, 1);
    map.add_feature(tag(b"dnom"), F_NONE, 1);
    map.enable_feature(tag(b"rand"), F_RANDOM, MAX_VALUE);
    map.enable_feature(tag(b"trak"), super::map::F_HAS_FALLBACK, 1);
    map.enable_feature(tag(b"Harf"), F_NONE, 1);
    map.enable_feature(tag(b"HARF"), F_NONE, 1);
    if shaper == Shaper::Arabic {
        map.is_simple = false;
        arabic::collect_features(map, script);
    }
    map.enable_feature(tag(b"Buzz"), F_NONE, 1);
    map.enable_feature(tag(b"BUZZ"), F_NONE, 1);
    for &(t, flags) in COMMON_FEATURES {
        map.add_feature(tag(t), flags, 1);
    }
    if direction.is_horizontal() {
        for &(t, flags) in HORIZONTAL_FEATURES {
            map.add_feature(tag(t), flags, 1);
        }
    } else {
        map.enable_feature(tag(b"vert"), F_GLOBAL_SEARCH, 1);
    }
    if !user_features.is_empty() {
        map.is_simple = false;
    }
    for feature in user_features {
        map.add_feature(feature.tag, F_GLOBAL, feature.value);
    }
}
