// Copyright (c) HarfBuzz developers
// Copyright (c) 2020 Yevhenii Reizner
// SPDX-License-Identifier: MIT
//
// Derived from rustybuzz 0.20.1 src/hb/ot_shape_plan.rs, src/hb/ot_shape.rs
// (`hb_ot_shape_planner_t`) and src/hb/ot_shaper.rs and modified by the aterm
// project in 2026.
// See ../../LICENSE-RUSTYBUZZ for the upstream license.

//! The shape plan: the features a run is shaped with, compiled against one face for one direction
//! and script.

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
    pub(crate) has_frac: bool,
    pub(crate) fallback_glyph_classes: bool,
    /// The face has a `GPOS`: marks are then zeroed without the offset shift upstream applies when
    /// it positions them by fallback.
    pub(crate) has_gpos: bool,
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
        let shaper = match script {
            Some(s) => categorize(s, direction, map.chosen_script_gsub()),
            None => Shaper::Default,
        };
        collect_features(&mut map, shaper, direction, script, features);
        let map = map.compile();
        let frac_mask = map.get_1_mask(Tag::from_bytes(b"frac"));
        let numr_mask = map.get_1_mask(Tag::from_bytes(b"numr"));
        let dnom_mask = map.get_1_mask(Tag::from_bytes(b"dnom"));
        let has_frac = frac_mask != 0 || (numr_mask != 0 && dnom_mask != 0);
        let rtlm_mask = map.get_1_mask(Tag::from_bytes(b"rtlm"));
        let arabic = (shaper == Shaper::Arabic).then(|| arabic::ArabicPlan::new(&map));
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
            has_frac,
            fallback_glyph_classes: !face.has_glyph_classes(),
            has_gpos: face.face.tables().gpos.is_some(),
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
