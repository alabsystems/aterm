// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The one typography resolver, sibling of [`crate::resolved_colors`]:
//! [`Config::resolved_typography`] picks the theme an appearance draws (by
//! the same selection as the colours, [`Config::theme_selection`]) and works
//! out which of the theme's type settings apply.

use std::borrow::Cow;

use aterm_types::Appearance;
use aterm_types::scheme::{THEME_KEYS, ThemeKey, ThemeKeyGroup, ThemeTypography};

use crate::app_config::{Config, FontList, ThemeCatalog};

/// Who decides one type setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TypeOrigin {
    /// Nobody set it: aterm's default.
    Default,
    /// The theme's file.
    Theme,
    /// A key in aterm.toml.
    Config,
    /// A launch flag (`--font`).
    Flag,
}

impl TypeOrigin {
    /// The word `--show-config`, `ctl theme` and Settings use.
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Theme => "theme",
            Self::Config => "config",
            Self::Flag => "flag",
        }
    }
}

/// One theme type setting a higher rung beat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TypeShadow {
    /// The theme key (`font_family`).
    pub(crate) key: &'static str,
    /// Who beat it.
    pub(crate) origin: TypeOrigin,
    /// What beat it, as a person would look for it: the aterm.toml key
    /// (`font_family`) or the flag (`--font`).
    pub(crate) by: &'static str,
}

/// The typography one appearance draws: the theme's type settings and which
/// of them apply over the config.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResolvedTypography {
    /// The theme drawn (the same id the colours report).
    pub(crate) id: String,
    /// The appearance it was resolved for.
    pub(crate) side: Appearance,
    /// What the theme file says.
    pub(crate) declared: ThemeTypography,
    /// The subset no config key or flag beats: what [`Self::overlay`] fills.
    pub(crate) applied: ThemeTypography,
    /// The theme's keys a config key or a flag beat, in table order.
    pub(crate) shadowed: Vec<TypeShadow>,
}

/// Whether `cfg` sets the typography key `key` itself (a whole value: an
/// empty list or a blank family counts). A non-finite `line_height` or
/// `font_size_scale` sets nothing.
pub(crate) fn config_sets(cfg: &Config, key: ThemeKey) -> bool {
    match key {
        ThemeKey::FontFamily => cfg.font_family.is_some(),
        ThemeKey::FontFamilyBold => cfg.font_family_bold.is_some(),
        ThemeKey::FontFamilyItalic => cfg.font_family_italic.is_some(),
        ThemeKey::FontFamilyBoldItalic => cfg.font_family_bold_italic.is_some(),
        ThemeKey::FallbackFonts => cfg.fallback_fonts.is_some(),
        ThemeKey::SymbolFont => cfg.symbol_font.is_some(),
        ThemeKey::EmojiFont => cfg.emoji_font.is_some(),
        ThemeKey::LineHeight => cfg.line_height.is_some_and(f32::is_finite),
        ThemeKey::Ligatures => cfg.ligatures.is_some(),
        ThemeKey::FontFeatures => cfg.font_features.is_some(),
        ThemeKey::FontSyntheticStyle => cfg.font_synthetic_style.is_some(),
        ThemeKey::FontWeight => cfg.font_weight.is_some(),
        ThemeKey::FontSizeScale => cfg.font_size_scale.is_some_and(f32::is_finite),
        _ => false,
    }
}

/// A thousandths value as the `f32` aterm.toml would hold for the same
/// decimal: the decimal's `f64`, narrowed.
#[allow(
    clippy::cast_possible_truncation,
    reason = "the TOML f64 -> f32 narrowing, on purpose, so the bits match a config value"
)]
pub(crate) fn milli_f32(milli: u16) -> f32 {
    (f64::from(milli) / 1000.0) as f32
}

impl Config {
    /// The typography `appearance` draws against `themes`, for the launch's
    /// own `--font`.
    pub(crate) fn resolved_typography(
        &self,
        appearance: Appearance,
        themes: &ThemeCatalog,
    ) -> ResolvedTypography {
        self.resolved_typography_with_flag(
            appearance,
            themes,
            crate::launch::flags().font_family.as_deref(),
        )
    }

    /// [`Self::resolved_typography`] with `flag` standing in for `--font`.
    pub(crate) fn resolved_typography_with_flag(
        &self,
        appearance: Appearance,
        themes: &ThemeCatalog,
        flag: Option<&str>,
    ) -> ResolvedTypography {
        let selection = self.theme_selection(appearance, themes, None);
        let declared = selection.package.typography.clone();
        let mut applied = declared.clone();
        let mut shadowed = Vec::new();
        for spec in THEME_KEYS
            .iter()
            .filter(|spec| spec.group == ThemeKeyGroup::Typography && declared.is_set(spec.key))
        {
            let beaten = if spec.key == ThemeKey::FontFamily && flag.is_some() {
                Some((TypeOrigin::Flag, "--font"))
            } else if config_sets(self, spec.key) {
                Some((TypeOrigin::Config, spec.name))
            } else {
                None
            };
            if let Some((origin, by)) = beaten {
                applied.clear(spec.key);
                shadowed.push(TypeShadow {
                    key: spec.name,
                    origin,
                    by,
                });
            }
        }
        ResolvedTypography {
            id: selection.id,
            side: appearance,
            declared,
            applied,
            shadowed,
        }
    }
}

impl ResolvedTypography {
    /// No theme typography: what a config without a typography theme draws.
    #[cfg(any(test, feature = "bench-support"))]
    pub(crate) fn none(appearance: Appearance) -> Self {
        Self {
            id: String::new(),
            side: appearance,
            declared: ThemeTypography::NONE,
            applied: ThemeTypography::NONE,
            shadowed: Vec::new(),
        }
    }

    /// The config every font reader should see: `cfg` with the applied theme
    /// settings filled into the keys it leaves unset (flag > config > theme >
    /// default).
    pub(crate) fn overlay<'a>(&self, cfg: &'a Config) -> Cow<'a, Config> {
        let t = &self.applied;
        if t.is_empty() {
            return Cow::Borrowed(cfg);
        }
        let mut view = cfg.clone();
        let fill = |slot: &mut Option<String>, value: &Option<String>| {
            if slot.is_none() {
                slot.clone_from(value);
            }
        };
        fill(&mut view.font_family, &t.font_family);
        fill(&mut view.font_family_bold, &t.font_family_bold);
        fill(&mut view.font_family_italic, &t.font_family_italic);
        fill(
            &mut view.font_family_bold_italic,
            &t.font_family_bold_italic,
        );
        fill(&mut view.symbol_font, &t.symbol_font);
        fill(&mut view.emoji_font, &t.emoji_font);
        if view.fallback_fonts.is_none() {
            view.fallback_fonts = t.fallback_fonts.clone().map(FontList);
        }
        if !view.line_height.is_some_and(f32::is_finite) && t.line_height_milli.is_some() {
            view.line_height = t.line_height_milli.map(milli_f32);
        }
        if view.ligatures.is_none() {
            view.ligatures = t.ligatures;
        }
        if view.font_features.is_none() {
            view.font_features.clone_from(&t.font_features);
        }
        if view.font_synthetic_style.is_none() {
            view.font_synthetic_style = t.font_synthetic_style;
        }
        if view.font_weight.is_none() {
            view.font_weight = t.font_weight.map(u32::from);
        }
        if !view.font_size_scale.is_some_and(f32::is_finite) && t.font_size_scale_milli.is_some() {
            view.font_size_scale = t.font_size_scale_milli.map(milli_f32);
        }
        Cow::Owned(view)
    }

    /// Who decides the typography key `key` (canonical spelling) in this
    /// resolution, for `cfg` (the authored config it was resolved from).
    pub(crate) fn origin(&self, key: &str, cfg: &Config) -> TypeOrigin {
        if let Some(shadow) = self.shadowed.iter().find(|shadow| shadow.key == key) {
            return shadow.origin;
        }
        let Some(spec) = THEME_KEYS.iter().find(|spec| spec.name == key) else {
            return TypeOrigin::Default;
        };
        if spec.key == ThemeKey::FontFamily && crate::launch::flags().font_family.is_some() {
            TypeOrigin::Flag
        } else if config_sets(cfg, spec.key) {
            TypeOrigin::Config
        } else if self.applied.is_set(spec.key) {
            TypeOrigin::Theme
        } else {
            TypeOrigin::Default
        }
    }

    /// The face-relevant fingerprint: two resolutions need no font generation
    /// between them iff their keys are equal.
    pub(crate) const fn face_key(&self) -> &ThemeTypography {
        &self.applied
    }

    /// The applied keys, in table order, comma-joined (`-` for none): what
    /// `ctl theme` reports.
    pub(crate) fn applied_keys(&self) -> Vec<&'static str> {
        self.applied.keys()
    }

    /// The shadowed keys as `key:config|flag`, for `ctl theme`.
    pub(crate) fn shadowed_words(&self) -> Vec<String> {
        self.shadowed
            .iter()
            .map(|shadow| format!("{}:{}", shadow.key, shadow.origin.word()))
            .collect()
    }
}

#[cfg(test)]
#[path = "resolved_typography_tests.rs"]
mod tests;
