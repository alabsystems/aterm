// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The one colour resolver.

use aterm_core::terminal::{ColorPalette, Rgb};
use aterm_render::Theme;
use aterm_types::{Appearance, ColorScheme};

use aterm_types::scheme::{SelectorDraw, ThemeSelector};

use crate::app_config::{Config, ThemeCatalog};

/// Where the theme a resolution draws came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ThemeSource {
    /// A built-in scheme.
    Builtin,
    /// A loaded custom theme file, by its catalog id (the file stem).
    File(String),
    /// A community pack theme drawn from the pack embedded in aterm.
    Pack(String),
    /// The selected name does not resolve, so Default is drawn.
    DefaultFallback { reason: String },
}

/// Who chose one colour slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ColorOrigin {
    /// The selected theme (built-in or file).
    Theme,
    /// A colour key in `aterm.toml`.
    Config,
    /// Nobody: the Default scheme (no theme selected, or the selected one does not resolve), or a
    /// rule that derives a slot no theme or key sets (cursor, selection, `selection_fg`, chrome).
    Default,
}

/// The origin of every colour slot [`ResolvedColors::scheme`] carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ColorProvenance {
    pub(crate) foreground: ColorOrigin,
    pub(crate) background: ColorOrigin,
    pub(crate) cursor: ColorOrigin,
    pub(crate) selection: ColorOrigin,
    pub(crate) selection_fg: ColorOrigin,
    pub(crate) ansi: [ColorOrigin; 16],
    pub(crate) chrome: ChromeProvenance,
}

/// The origin of each chrome colour ([`aterm_types::scheme::ChromeColors`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ChromeProvenance {
    pub(crate) accent: ColorOrigin,
    pub(crate) active_tab: ColorOrigin,
    pub(crate) border: ColorOrigin,
    pub(crate) search_match: ColorOrigin,
}

/// The chrome keys, as `aterm.toml` and a theme file spell them, with the slot each sets.
pub(crate) const CHROME_KEYS: [(&str, aterm_types::scheme::ThemeKey); 4] = [
    ("accent_color", aterm_types::scheme::ThemeKey::AccentColor),
    (
        "active_tab_color",
        aterm_types::scheme::ThemeKey::ActiveTabColor,
    ),
    ("border_color", aterm_types::scheme::ThemeKey::BorderColor),
    (
        "search_match_color",
        aterm_types::scheme::ThemeKey::SearchMatchColor,
    ),
];

impl ChromeProvenance {
    /// The origin of the slot `key` sets (a chrome key of [`CHROME_KEYS`]).
    pub(crate) const fn of(&self, key: aterm_types::scheme::ThemeKey) -> ColorOrigin {
        use aterm_types::scheme::ThemeKey;
        match key {
            ThemeKey::AccentColor => self.accent,
            ThemeKey::ActiveTabColor => self.active_tab,
            ThemeKey::BorderColor => self.border,
            _ => self.search_match,
        }
    }

    const fn of_mut(&mut self, key: aterm_types::scheme::ThemeKey) -> &mut ColorOrigin {
        use aterm_types::scheme::ThemeKey;
        match key {
            ThemeKey::AccentColor => &mut self.accent,
            ThemeKey::ActiveTabColor => &mut self.active_tab,
            ThemeKey::BorderColor => &mut self.border,
            _ => &mut self.search_match,
        }
    }
}

/// One colour value in `aterm.toml` that is not a hex colour.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ColorIssue {
    /// The key as written: `foreground`, `palette[3]`, ...
    pub(crate) key: String,
    pub(crate) value: String,
}

impl std::fmt::Display for ColorIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: expected #RRGGBB or #RGB, got {:?}",
            self.key, self.value
        )
    }
}

/// The colours one appearance draws.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResolvedColors {
    /// The theme drawn.
    pub(crate) id: String,
    /// The appearance this resolution is for; it picked the side of a `dark:…,light:…` selector.
    pub(crate) side: Appearance,
    pub(crate) source: ThemeSource,
    /// The final colours.
    pub(crate) scheme: ColorScheme,
    /// The colour theme's own foreground, background, cursor and selection, before the colour keys.
    theme_before_overrides: Theme,
    /// The chrome colours the colour theme itself authors.
    theme_authored_chrome: aterm_messages::tokens::AuthoredChrome,
    /// `palette[]` entries past the 16 ANSI slots, as (index, colour).
    pub(crate) extended_palette: Vec<(u8, Rgb)>,
    /// The `palette[i]` and `palette_overrides.i` keys that landed.
    palette_keys: PaletteKeys,
    /// Whether bold text brightens ANSI 0-7 (`bold_is_bright`, else the engine default).
    pub(crate) bold_is_bright: bool,
    pub(crate) provenance: ColorProvenance,
    /// The appearance of the configured palette.
    pub(crate) reported: Appearance,
    /// Every colour value that did not parse, once each, in key order.
    pub(crate) issues: Vec<ColorIssue>,
    /// Whether slots 16-255 derive from the final colours.
    pub(crate) palette_generate: bool,
    /// Who decided [`Self::palette_generate`].
    pub(crate) palette_generate_origin: ColorOrigin,
    /// Whether the engine gets an explicit palette.
    custom_palette: bool,
}

/// Who set one slot of the 256-colour palette, for `theme palette`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SlotOrigin {
    /// One of the 16 ANSI slots, by the origin of its colour.
    Ansi(ColorOrigin),
    /// Slot 16-255 set by `palette[N]` or `palette_overrides.N`.
    Config,
    /// Slot 16-255 derived from the theme's colours.
    Generated,
    /// Slot 16-255 at xterm's own value.
    Xterm,
}

impl SlotOrigin {
    /// The word `theme palette` prints for this origin.
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::Ansi(ColorOrigin::Theme) => "theme",
            Self::Ansi(ColorOrigin::Config) | Self::Config => "config",
            Self::Ansi(ColorOrigin::Default) => "default",
            Self::Generated => "generated",
            Self::Xterm => "xterm",
        }
    }
}

impl ResolvedColors {
    /// The renderer's theme: the final scheme's chrome.
    pub(crate) fn theme(&self) -> Theme {
        Theme::from(&self.scheme)
    }

    /// The colour theme before the four colour keys that reach the renderer's theme.
    pub(crate) const fn theme_before_overrides(&self) -> Theme {
        self.theme_before_overrides
    }

    /// The chrome colours the theme authors before any chrome key.
    pub(crate) const fn theme_authored_chrome(&self) -> aterm_messages::tokens::AuthoredChrome {
        self.theme_authored_chrome
    }

    /// Flip bits of the pre-override background and foreground.
    #[cfg(test)]
    pub(crate) fn tint_theme_for_test(&mut self, bg: u32, fg: u32) {
        self.theme_before_overrides.bg ^= bg;
        self.theme_before_overrides.fg ^= fg;
    }

    /// The selected-text foreground.
    pub(crate) const fn selection_fg(&self) -> Option<Rgb> {
        self.scheme.selection_fg
    }

    /// Who set palette slot `index`.
    pub(crate) fn extended_origin(&self, index: u8) -> SlotOrigin {
        if let Some(origin) = self.provenance.ansi.get(usize::from(index)) {
            return SlotOrigin::Ansi(*origin);
        }
        if self.extended_palette.iter().any(|(at, _)| *at == index) {
            SlotOrigin::Config
        } else if self.palette_generate {
            SlotOrigin::Generated
        } else {
            SlotOrigin::Xterm
        }
    }

    /// The engine's indexed palette.
    pub(crate) fn custom_palette(&self) -> Option<ColorPalette> {
        self.custom_palette.then(|| {
            let mut palette = self.scheme.to_color_palette();
            for &(index, rgb) in &self.extended_palette {
                palette.set(index, rgb);
            }
            palette
        })
    }

    /// The chrome colours the theme or the config authored (config over theme).
    pub(crate) fn authored_chrome(&self) -> aterm_messages::tokens::AuthoredChrome {
        crate::chrome_band::authored_of(&self.scheme.chrome)
    }

    /// The `aterm.toml` keys that set a slot here.
    pub(crate) fn config_keys(&self) -> Vec<String> {
        let p = &self.provenance;
        let mut keys: Vec<String> = [
            ("foreground", p.foreground),
            ("background", p.background),
            ("cursor_color", p.cursor),
            ("selection_color", p.selection),
            ("selection_foreground", p.selection_fg),
        ]
        .into_iter()
        .filter(|(_, origin)| *origin == ColorOrigin::Config)
        .map(|(key, _)| key.to_string())
        .collect();
        keys.extend(
            self.palette_keys
                .list
                .iter()
                .map(|i| format!("palette[{i}]")),
        );
        keys.extend(
            self.palette_keys
                .overrides
                .iter()
                .map(|&i| palette_override_key(usize::from(i))),
        );
        keys.extend(
            CHROME_KEYS
                .iter()
                .filter(|(_, key)| p.chrome.of(*key) == ColorOrigin::Config)
                .map(|(name, _)| (*name).to_string()),
        );
        keys
    }
}

/// The palette keys that set a slot.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct PaletteKeys {
    list: Vec<u8>,
    overrides: Vec<u8>,
}

/// The `palette_overrides` key for slot `index`.
pub(crate) fn palette_override_key(index: usize) -> String {
    format!("{}.{index}", crate::prefs::EDIT_PALETTE_OVERRIDES)
}

/// The slot a `palette_overrides` table key names.
pub(crate) fn palette_override_index(key: &str) -> Option<usize> {
    let canonical = !key.is_empty()
        && key.bytes().all(|byte| byte.is_ascii_digit())
        && (key == "0" || !key.starts_with('0'));
    key.parse::<usize>()
        .ok()
        .filter(|index| canonical && *index < 256)
}

fn override_color(key: &str, raw: Option<&str>, issues: &mut Vec<ColorIssue>) -> Option<Rgb> {
    let raw = raw?;
    let parsed = aterm_types::scheme::parse_hex(raw);
    if parsed.is_none() {
        issues.push(ColorIssue {
            key: key.to_string(),
            value: raw.to_string(),
        });
    }
    parsed
}

/// What `palette_generate` in `aterm.toml` says ([`crate::prefs::PALETTE_GENERATE_MODES`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PaletteGenerateMode {
    /// Absent, `"theme"` or a value it does not know: the theme's own key.
    Theme,
    /// `"always"`: derive slots 16-255 for every theme.
    Always,
    /// `"never"`: keep xterm's slots 16-255.
    Never,
}

impl Config {
    /// The config's `palette_generate`.
    pub(crate) fn palette_generate_mode(&self) -> PaletteGenerateMode {
        match self
            .palette_generate
            .as_deref()
            .map(|v| v.trim().to_ascii_lowercase())
            .as_deref()
        {
            Some("always") => PaletteGenerateMode::Always,
            Some("never") => PaletteGenerateMode::Never,
            _ => PaletteGenerateMode::Theme,
        }
    }
}

/// The lowest text contrast a side of a `dark:…,light:…` split may be left at by the colour keys in
/// `aterm.toml` before validation warns.
pub(crate) const SPLIT_OVERRIDE_MIN_CONTRAST: f64 = 3.0;

struct ThemeDraw {
    id: String,
    source: ThemeSource,
    scheme: ColorScheme,
    named: bool,
    /// The theme's `bold_is_bright`: a rung under the config's key.
    bold_is_bright: Option<bool>,
}

/// The one theme a selection picks for one appearance.
pub(crate) struct ThemeSelection<'a> {
    pub(crate) id: String,
    pub(crate) source: ThemeSource,
    pub(crate) package: std::borrow::Cow<'a, aterm_types::scheme::ThemePackage>,
    pub(crate) named: bool,
}

fn builtin_package(name: &str) -> aterm_types::scheme::ThemePackage {
    aterm_types::scheme::builtin_theme(name)
        .and_then(aterm_types::scheme::ThemePackage::of_builtin)
        .unwrap_or_else(aterm_types::scheme::ThemePackage::default_theme)
}

impl Config {
    /// The theme `appearance` selects.
    pub(crate) fn theme_selection<'a>(
        &self,
        appearance: Appearance,
        themes: &'a ThemeCatalog,
        candidate: Option<&str>,
    ) -> ThemeSelection<'a> {
        self.theme_selection_with(appearance, themes, candidate, false)
    }

    fn theme_selection_with<'a>(
        &self,
        appearance: Appearance,
        themes: &'a ThemeCatalog,
        candidate: Option<&str>,
        pack: bool,
    ) -> ThemeSelection<'a> {
        let draw = match candidate.or(self.theme.as_deref()) {
            None => SelectorDraw::unset(),
            Some(raw) => ThemeSelector::parse(raw).0.draw_builtin(appearance),
        };
        let theme_named = !matches!(draw, SelectorDraw::Unset(_));
        let (id, source, package) = match draw {
            SelectorDraw::Unset(scheme) | SelectorDraw::Builtin(scheme) => {
                let package = builtin_package(&scheme.name);
                (
                    package.colors.name.clone(),
                    ThemeSource::Builtin,
                    std::borrow::Cow::Owned(package),
                )
            }
            SelectorDraw::Other(name) => match themes.resolve_package(&name) {
                Ok(package) => {
                    let id = themes
                        .ready_names()
                        .find(|ready| ready.eq_ignore_ascii_case(&name))
                        .map_or(name, str::to_string);
                    (id.clone(), ThemeSource::File(id), package)
                }
                Err(_) if pack && let Some((id, package)) = themes.pack_fallback(&name) => (
                    id.to_string(),
                    ThemeSource::Pack(id.to_string()),
                    std::borrow::Cow::Borrowed(package),
                ),
                Err(reason) => (
                    "Default".to_string(),
                    ThemeSource::DefaultFallback { reason },
                    std::borrow::Cow::Owned(builtin_package("Default")),
                ),
            },
        };
        ThemeSelection {
            id,
            source,
            package,
            named: theme_named,
        }
    }

    fn theme_draw(
        &self,
        appearance: Appearance,
        themes: &ThemeCatalog,
        candidate: Option<&str>,
        pack: bool,
    ) -> ThemeDraw {
        let ThemeSelection {
            id,
            source,
            package,
            named: theme_named,
        } = self.theme_selection_with(appearance, themes, candidate, pack);
        let bold_is_bright = package.bold_is_bright;
        let scheme = package.into_owned().colors;
        ThemeDraw {
            id,
            source,
            scheme,
            named: theme_named,
            bold_is_bright,
        }
    }

    /// Resolve the colours `appearance` draws against `themes`.
    pub(crate) fn resolved_colors(
        &self,
        appearance: Appearance,
        themes: &ThemeCatalog,
        candidate: Option<&str>,
    ) -> ResolvedColors {
        self.resolved_colors_with(appearance, themes, candidate, false)
    }

    /// [`Self::resolved_colors`] for a preview.
    pub(crate) fn resolved_colors_previewing(
        &self,
        appearance: Appearance,
        themes: &ThemeCatalog,
        preview: Option<&str>,
    ) -> ResolvedColors {
        self.resolved_colors_with(appearance, themes, preview, preview.is_some())
    }

    fn resolved_colors_with(
        &self,
        appearance: Appearance,
        themes: &ThemeCatalog,
        candidate: Option<&str>,
        pack: bool,
    ) -> ResolvedColors {
        let ThemeDraw {
            id,
            source,
            mut scheme,
            named: theme_named,
            bold_is_bright: theme_bold_is_bright,
        } = self.theme_draw(appearance, themes, candidate, pack);
        let theme_before_overrides = Theme::from(&scheme);
        let theme_authored_chrome = crate::chrome_band::authored_of(&scheme.chrome);
        let base = if matches!(source, ThemeSource::DefaultFallback { .. }) || !theme_named {
            ColorOrigin::Default
        } else {
            ColorOrigin::Theme
        };
        let mut provenance = ColorProvenance {
            foreground: base,
            background: base,
            cursor: if scheme.cursor.is_some() {
                base
            } else {
                ColorOrigin::Default
            },
            selection: if scheme.selection.is_some() {
                base
            } else {
                ColorOrigin::Default
            },
            selection_fg: if scheme.selection_fg.is_some() {
                base
            } else {
                ColorOrigin::Default
            },
            ansi: [base; 16],
            chrome: ChromeProvenance {
                accent: ColorOrigin::Default,
                active_tab: ColorOrigin::Default,
                border: ColorOrigin::Default,
                search_match: ColorOrigin::Default,
            },
        };
        for (_, key) in CHROME_KEYS {
            if scheme.chrome.slot(key).flatten().is_some() {
                *provenance.chrome.of_mut(key) = base;
            }
        }

        let mut issues = Vec::new();
        if let Some(rgb) = override_color("foreground", self.foreground.as_deref(), &mut issues) {
            scheme.foreground = rgb;
            provenance.foreground = ColorOrigin::Config;
        }
        let bg_override = override_color("background", self.background.as_deref(), &mut issues);
        if let Some(rgb) = bg_override {
            scheme.background = rgb;
            provenance.background = ColorOrigin::Config;
        }
        if let Some(rgb) = override_color("cursor_color", self.cursor_color.as_deref(), &mut issues)
        {
            scheme.cursor = Some(rgb);
            provenance.cursor = ColorOrigin::Config;
        }
        if let Some(rgb) = override_color(
            "selection_color",
            self.selection_color.as_deref(),
            &mut issues,
        ) {
            scheme.selection = Some(rgb);
            provenance.selection = ColorOrigin::Config;
        }
        if let Some(rgb) = override_color(
            "selection_foreground",
            self.selection_foreground.as_deref(),
            &mut issues,
        ) {
            scheme.selection_fg = Some(rgb);
            provenance.selection_fg = ColorOrigin::Config;
        }
        for (name, key) in CHROME_KEYS {
            let raw = match key {
                aterm_types::scheme::ThemeKey::AccentColor => self.accent_color.as_deref(),
                aterm_types::scheme::ThemeKey::ActiveTabColor => self.active_tab_color.as_deref(),
                aterm_types::scheme::ThemeKey::BorderColor => self.border_color.as_deref(),
                _ => self.search_match_color.as_deref(),
            };
            if let Some(rgb) = override_color(name, raw, &mut issues)
                && let Some(slot) = scheme.chrome.slot_mut(key)
            {
                *slot = Some(rgb);
                *provenance.chrome.of_mut(key) = ColorOrigin::Config;
            }
        }
        let mut extended_palette: Vec<(u8, Rgb)> = Vec::new();
        let mut palette_keys = PaletteKeys::default();
        let mut set_slot = |index: usize, rgb: Rgb| {
            match (scheme.ansi.get_mut(index), provenance.ansi.get_mut(index)) {
                (Some(slot), Some(origin)) => {
                    *slot = rgb;
                    *origin = ColorOrigin::Config;
                }
                _ => {
                    let index = u8::try_from(index).unwrap_or(u8::MAX);
                    match extended_palette.iter_mut().find(|(at, _)| *at == index) {
                        Some(entry) => entry.1 = rgb,
                        None => extended_palette.push((index, rgb)),
                    }
                }
            }
            u8::try_from(index).unwrap_or(u8::MAX)
        };
        for (index, hex) in self.palette.iter().flatten().take(256).enumerate() {
            let Some(rgb) = override_color(&format!("palette[{index}]"), Some(hex), &mut issues)
            else {
                continue;
            };
            palette_keys.list.push(set_slot(index, rgb));
        }
        for (key, hex) in self.palette_overrides.iter().flatten() {
            let Some(index) = palette_override_index(key) else {
                issues.push(ColorIssue {
                    key: format!("{}.{key}", crate::prefs::EDIT_PALETTE_OVERRIDES),
                    value: hex.clone(),
                });
                continue;
            };
            let Some(rgb) = override_color(&palette_override_key(index), Some(hex), &mut issues)
            else {
                continue;
            };
            palette_keys.overrides.push(set_slot(index, rgb));
        }
        palette_keys.overrides.sort_unstable();
        let palette_landed = !palette_keys.list.is_empty() || !palette_keys.overrides.is_empty();

        let (palette_generate, palette_generate_origin) = match self.palette_generate_mode() {
            PaletteGenerateMode::Always => (true, ColorOrigin::Config),
            PaletteGenerateMode::Never => (false, ColorOrigin::Config),
            PaletteGenerateMode::Theme => (scheme.palette_generate, base),
        };
        scheme.palette_generate = palette_generate;

        let reported = match bg_override {
            None => scheme.appearance,
            Some(_) => {
                let bg = scheme.background;
                if aterm_messages::ink::bg_is_light([bg.r, bg.g, bg.b]) {
                    Appearance::Light
                } else {
                    Appearance::Dark
                }
            }
        };
        ResolvedColors {
            id,
            side: appearance,
            source,
            scheme,
            theme_before_overrides,
            theme_authored_chrome,
            extended_palette,
            palette_keys,
            bold_is_bright: self
                .bold_is_bright
                .or(theme_bold_is_bright)
                .unwrap_or(aterm_core::config::TerminalConfig::default().bold_is_bright),
            provenance,
            reported,
            issues,
            palette_generate,
            palette_generate_origin,
            custom_palette: theme_named || palette_landed || palette_generate,
        }
    }

    /// What the colour keys in `aterm.toml` do to a `dark:…,light:…` split.
    pub(crate) fn split_override_warnings(
        &self,
        themes: &ThemeCatalog,
    ) -> Vec<(&'static str, String)> {
        let Some(raw) = self.theme.as_deref() else {
            return Vec::new();
        };
        let (selector, _) = aterm_types::scheme::ThemeSelector::parse(raw);
        if !matches!(selector, aterm_types::scheme::ThemeSelector::Split { .. }) {
            return Vec::new();
        }
        let hex = |c: Rgb| format!("#{:02X}{:02X}{:02X}", c.r, c.g, c.b);
        let mut warnings = Vec::new();
        for side in [Appearance::Dark, Appearance::Light] {
            let mode = match side {
                Appearance::Dark => "dark",
                Appearance::Light => "light",
            };
            let theme = self.theme_draw(side, themes, None, false).scheme;
            let resolved = self.resolved_colors(side, themes, None);
            let (p, s) = (&resolved.provenance, &resolved.scheme);
            let fg_set = p.foreground == ColorOrigin::Config;
            let bg_set = p.background == ColorOrigin::Config;
            if fg_set || bg_set {
                let now = s.foreground.contrast(s.background);
                let was = theme.foreground.contrast(theme.background);
                if now < SPLIT_OVERRIDE_MIN_CONTRAST && was >= SPLIT_OVERRIDE_MIN_CONTRAST {
                    let (key, keys) = match (fg_set, bg_set) {
                        (true, true) => ("background", "foreground and background apply"),
                        (true, false) => ("foreground", "foreground applies"),
                        _ => ("background", "background applies"),
                    };
                    warnings.push((
                        key,
                        format!(
                            "{key}: {keys} to both modes of theme {raw:?}, so {mode} mode                              ({:?}) draws text at {now:.2}:1 (foreground {} on background {}),                              below 3:1; {:?} alone draws {was:.2}:1",
                            resolved.id,
                            hex(s.foreground),
                            hex(s.background),
                            resolved.id,
                        ),
                    ));
                }
            }
            let faded: Vec<String> = (0..16)
                .filter(|&i| {
                    p.ansi[i] == ColorOrigin::Config
                        && s.ansi[i].contrast(s.background) < SPLIT_OVERRIDE_MIN_CONTRAST
                        && theme.ansi[i].contrast(theme.background) >= SPLIT_OVERRIDE_MIN_CONTRAST
                })
                .map(|i| format!("palette[{i}] {:.2}:1", s.ansi[i].contrast(s.background)))
                .collect();
            if !faded.is_empty() {
                warnings.push((
                    "palette",
                    format!(
                        "palette: applies to both modes of theme {raw:?}, so {mode} mode ({:?})                          draws {} on background {}, below 3:1 where the theme's own colours                          clear it",
                        resolved.id,
                        faded.join(", "),
                        hex(s.background),
                    ),
                ));
            }
        }
        warnings
    }

    /// Every colour value in this config that is not a hex colour, once each.
    pub(crate) fn color_issues(&self) -> Vec<ColorIssue> {
        self.resolved_colors(Appearance::Dark, &ThemeCatalog::default(), None)
            .issues
    }

    /// The engine config to apply.
    pub(crate) fn applied_terminal_config_from(
        &self,
        resolved: &ResolvedColors,
    ) -> aterm_core::config::TerminalConfig {
        let mut tc = self.engine_behaviour_config();
        let unpack = |c: u32| {
            Rgb::new(
                ((c >> 16) & 0xff) as u8,
                ((c >> 8) & 0xff) as u8,
                (c & 0xff) as u8,
            )
        };
        let parts = resolved.scheme.to_theme_parts();
        tc.default_foreground = resolved.scheme.foreground;
        tc.default_background = resolved.scheme.background;
        tc.cursor_color = Some(unpack(parts.cursor));
        tc.selection_background = Some(unpack(parts.selection));
        tc.selection_foreground = resolved.selection_fg();
        tc.custom_palette = resolved.custom_palette();
        tc.bold_is_bright = resolved.bold_is_bright;
        tc.color_scheme = Some(resolved.reported);
        tc
    }
}

#[cfg(test)]
#[path = "resolved_colors_tests.rs"]
mod tests;
