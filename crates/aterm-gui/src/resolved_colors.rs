// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The one colour resolver: [`Config::resolved_colors`] picks the theme for an appearance, applies every colour override in `aterm.toml` once (fg, bg, cursor, selection, selection foreground, `palette[]`, and the chrome keys `accent_color`, `active_tab_color`, `border_color`, `search_match_color`), and records where each slot came from.

use aterm_core::terminal::{ColorPalette, Rgb};
use aterm_render::Theme;
use aterm_types::{Appearance, ColorScheme};

use aterm_types::scheme::{SelectorDraw, ThemeSelector};

use crate::app_config::{Config, ThemeCatalog};

/// Where the theme a resolution draws came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ThemeSource {
    /// A built-in scheme; `Default` too, which is also what an unset `theme` draws on every appearance.
    Builtin,
    /// A loaded custom theme file, by its catalog id (the file stem).
    File(String),
    /// A community pack theme drawn from the pack embedded in aterm, by its id: a preview only ([`Config::resolved_colors_previewing`]), before Keep installs the file.
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
    /// Nobody: the Default scheme (no theme selected, or the selected one does not resolve), or for `selection_fg` / a missing cursor the renderer's own rule.
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

/// The origin of each chrome colour ([`aterm_types::scheme::ChromeColors`]): the theme's file, the config's key, or [`ColorOrigin::Default`] when nobody authored it and the chrome derives it.
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

/// The colours one appearance draws: the selected theme with every `aterm.toml` colour override applied, plus where each slot came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResolvedColors {
    /// The theme drawn: a built-in's canonical name, a file's catalog id, or `Default` on a fallback.
    pub(crate) id: String,
    /// The appearance this resolution is for; it picked the side of a `dark:…,light:…` selector.
    pub(crate) side: Appearance,
    pub(crate) source: ThemeSource,
    /// The final colours, overrides included: `ansi` carries `palette[0..16]` and `selection_fg` the theme's or the config's selection foreground.
    pub(crate) scheme: ColorScheme,
    /// The colour theme's own foreground, background, cursor and selection,
    /// before the `foreground`, `background`, `cursor_color` and
    /// `selection_color` keys: what native pages (Settings, documents) draw
    /// their palette from, so a colour picked for the terminal never recolours
    /// the app's own pages.
    theme_before_overrides: Theme,
    /// The chrome colours the colour theme itself authors, before the
    /// `accent_color`, `active_tab_color`, `border_color` and
    /// `search_match_color` keys: what the strip paints while a key is unset,
    /// so Settings can name the colour an unset key leaves in force
    /// (`tab_bar::automatic_active_tab_color`).
    theme_authored_chrome: aterm_messages::tokens::AuthoredChrome,
    /// `palette[]` entries past the 16 ANSI slots, as (index, colour).
    pub(crate) extended_palette: Vec<(u8, Rgb)>,
    /// The `palette[i]` and `palette_overrides.i` keys that landed, as slot
    /// indices, in key order: what [`Self::config_keys`] names.
    palette_keys: PaletteKeys,
    /// Whether bold text brightens ANSI 0-7 (`bold_is_bright`, else the engine default).
    pub(crate) bold_is_bright: bool,
    pub(crate) provenance: ColorProvenance,
    /// The appearance of the configured palette: the scheme's appearance (declared or measured at admission) unless `background` is overridden, then the final background's polarity.
    pub(crate) reported: Appearance,
    /// Every colour value that did not parse, once each, in key order.
    pub(crate) issues: Vec<ColorIssue>,
    /// Whether slots 16-255 derive from the final colours ([`aterm_types::scheme_palette256::generated_extended_palette`]): `scheme.palette_generate`, kept equal to it.
    pub(crate) palette_generate: bool,
    /// Who decided [`Self::palette_generate`]: the theme's own key, the config's `palette_generate = "always"|"never"`, or nobody (the unset default or a fallback, which derive nothing).
    pub(crate) palette_generate_origin: ColorOrigin,
    /// Whether the engine gets an explicit palette: a theme was selected, a
    /// `palette[]` entry landed, or slots 16-255 are derived.
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

    /// The colour theme before the four colour keys that reach the renderer's theme: what native pages draw from.
    pub(crate) const fn theme_before_overrides(&self) -> Theme {
        self.theme_before_overrides
    }

    /// The chrome colours the theme authors before any chrome key ([`Self::authored_chrome`] with the keys left out).
    pub(crate) const fn theme_authored_chrome(&self) -> aterm_messages::tokens::AuthoredChrome {
        self.theme_authored_chrome
    }

    /// Flip bits of the pre-override background and foreground: a colour-only change to the installed resolution, for tests that move paint and nothing else.
    #[cfg(test)]
    pub(crate) fn tint_theme_for_test(&mut self, bg: u32, fg: u32) {
        self.theme_before_overrides.bg ^= bg;
        self.theme_before_overrides.fg ^= fg;
    }

    /// The selected-text foreground, when the theme or the config sets one; `None` keeps the renderer's contrast floor.
    pub(crate) const fn selection_fg(&self) -> Option<Rgb> {
        self.scheme.selection_fg
    }

    /// Who set palette slot `index`: an ANSI slot's colour origin, else a config entry, a derived slot or xterm's own value.
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

    /// The engine's indexed palette: the scheme's 16 ANSI slots, slots 16-255 derived from them when [`Self::palette_generate`] is on, then the extended `palette[]` entries over those (a config slot wins over a derived one); `None` for the engine's own table.
    pub(crate) fn custom_palette(&self) -> Option<ColorPalette> {
        self.custom_palette.then(|| {
            let mut palette = self.scheme.to_color_palette();
            for &(index, rgb) in &self.extended_palette {
                palette.set(index, rgb);
            }
            palette
        })
    }

    /// The chrome colours the theme or the config authored (config over theme), for the chrome tokens.
    pub(crate) fn authored_chrome(&self) -> aterm_messages::tokens::AuthoredChrome {
        crate::chrome_band::authored_of(&self.scheme.chrome)
    }

    /// The `aterm.toml` keys that set a slot here, in slot order (`palette[i]` per index, then the chrome keys).
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

/// The palette keys that set a slot: the positional `palette` entries and
/// the sparse `palette_overrides` slots, each by index.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct PaletteKeys {
    list: Vec<u8>,
    overrides: Vec<u8>,
}

/// The `palette_overrides` key for slot `index`, as `aterm.toml` and the Settings patch lane address it.
pub(crate) fn palette_override_key(index: usize) -> String {
    format!("{}.{index}", crate::prefs::EDIT_PALETTE_OVERRIDES)
}

/// The slot a `palette_overrides` table key names: a decimal index 0–255 with no sign or leading zero, so each slot has one spelling.
pub(crate) fn palette_override_index(key: &str) -> Option<usize> {
    let canonical = !key.is_empty()
        && key.bytes().all(|byte| byte.is_ascii_digit())
        && (key == "0" || !key.starts_with('0'));
    key.parse::<usize>()
        .ok()
        .filter(|index| canonical && *index < 256)
}

/// Parse one colour value; a value that does not parse is recorded once in
/// `issues` and yields `None`.
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
    /// The config's `palette_generate`, read case-insensitively; an unknown value follows the theme (the config walk warns about it once).
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

/// The lowest text contrast a side of a `dark:…,light:…` split may be left at by the colour keys in `aterm.toml` before validation warns ([`Config::split_override_warnings`]).
pub(crate) const SPLIT_OVERRIDE_MIN_CONTRAST: f64 = 3.0;

/// The theme a selection draws for one appearance, before any `aterm.toml`
/// colour key: its id, where it came from, its colours, and whether a theme
/// was named at all.
struct ThemeDraw {
    id: String,
    source: ThemeSource,
    scheme: ColorScheme,
    named: bool,
    /// The theme's `bold_is_bright`: a rung under the config's key.
    bold_is_bright: Option<bool>,
}

/// The one theme a selection picks for one appearance: its id, where it came from, the whole package (colours, identity, typography), and whether a theme was named at all.
pub(crate) struct ThemeSelection<'a> {
    pub(crate) id: String,
    pub(crate) source: ThemeSource,
    pub(crate) package: std::borrow::Cow<'a, aterm_types::scheme::ThemePackage>,
    pub(crate) named: bool,
}

/// A built-in's whole package (a built-in carries no typography), `Default`'s
/// for a name that is not one.
fn builtin_package(name: &str) -> aterm_types::scheme::ThemePackage {
    aterm_types::scheme::builtin_theme(name)
        .and_then(aterm_types::scheme::ThemePackage::of_builtin)
        .unwrap_or_else(aterm_types::scheme::ThemePackage::default_theme)
}

impl Config {
    /// The theme `appearance` selects (`candidate`, when given, standing in for the configured `theme`), as a whole package.
    pub(crate) fn theme_selection<'a>(
        &self,
        appearance: Appearance,
        themes: &'a ThemeCatalog,
        candidate: Option<&str>,
    ) -> ThemeSelection<'a> {
        self.theme_selection_with(appearance, themes, candidate, false)
    }

    /// [`Self::theme_selection`]; `pack` lets the community pack answer a
    /// name the catalog cannot ([`crate::app_config::pack_package`]): a
    /// preview's candidate only.
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

    /// The theme `appearance` draws, before the colour keys apply.
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

    /// Resolve the colours `appearance` draws against `themes`, with every colour override applied once.
    pub(crate) fn resolved_colors(
        &self,
        appearance: Appearance,
        themes: &ThemeCatalog,
        candidate: Option<&str>,
    ) -> ResolvedColors {
        self.resolved_colors_with(appearance, themes, candidate, false)
    }

    /// [`Self::resolved_colors`] for a preview: a `preview` name no built-in or loaded file answers is drawn from the community pack embedded in aterm ([`ThemeSource::Pack`]), so trying a pack theme writes nothing.
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

    /// The engine config to apply: the non-colour keys ([`Self::engine_behaviour_config`]) with every colour from `resolved`.
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
mod tests {
    use super::{ColorOrigin, ResolvedColors, ThemeSource};
    use crate::app_config::{Config, ThemeCatalog};
    use aterm_core::config::TerminalConfig;
    use aterm_core::terminal::{ColorPalette, Rgb};
    use aterm_render::Theme;
    use aterm_types::{Appearance, ColorScheme};

    fn cfg(text: &str) -> Config {
        aterm_toml::from_str(text).expect("test config parses")
    }

    fn resolve(config: &Config, appearance: Appearance) -> ResolvedColors {
        config.resolved_colors(appearance, &ThemeCatalog::default(), None)
    }

    /// A `TerminalConfig` with its palette as the 256 colours it resolves to:
    /// the palette's sparse record keeps insertion order, which two equal
    /// palettes built in a different order do not share.
    fn engine_view(mut tc: TerminalConfig) -> (TerminalConfig, Option<Vec<Rgb>>) {
        let palette = tc
            .custom_palette
            .take()
            .map(|p| (0..=255u8).map(|i| p.get(i)).collect());
        (tc, palette)
    }

    /// Projection agreement: every built-in (and no theme, a missing name and
    /// a split) × both appearances × each override set, the renderer's Theme
    /// and the engine's TerminalConfig draw the same final colours, the ones
    /// the one resolution carries, and every override that parses lands in
    /// both (a projection that dropped one fails here).
    #[test]
    fn resolved_colors_projections_agree_on_every_case() {
        let mut selectors: Vec<Option<String>> = vec![
            None,
            Some("no-such-theme".to_string()),
            Some("dark:Dracula,light:Solarized Light".to_string()),
        ];
        selectors.extend(
            aterm_types::scheme::builtin_names()
                .into_iter()
                .map(|name| Some(name.to_string())),
        );
        let overrides = [
            "",
            "foreground = \"#102030\"",
            "background = \"#f0e0d0\"",
            "cursor_color = \"#ff00ff\"",
            "selection_color = \"#334455\"",
            "selection_foreground = \"#ffffff\"",
            "palette = [\"#010203\", \"#aabbcc\"]",
            "palette = [\"#000000\", \"#800000\", \"nope\", \"#123456\"]",
            "palette = [\"bad!\"]",
            "palette = [\"#abc\"]",
            "bold_is_bright = false",
            "foreground = \"#zz0000\"\nbackground = \"oops\"",
            "foreground = \"#102030\"\nbackground = \"#203040\"\ncursor_color = \"#304050\"\n\
             selection_color = \"#405060\"\nselection_foreground = \"#506070\"\n\
             palette = [\"#111111\", \"#222222\", \"#333333\", \"#444444\", \"#555555\", \
             \"#666666\", \"#777777\", \"#888888\", \"#999999\", \"#aaaaaa\", \"#bbbbbb\", \
             \"#cccccc\", \"#dddddd\", \"#eeeeee\", \"#efefef\", \"#fefefe\", \"#121212\"]",
        ];
        let unpack = |c: u32| {
            Rgb::new(
                ((c >> 16) & 0xff) as u8,
                ((c >> 8) & 0xff) as u8,
                (c & 0xff) as u8,
            )
        };
        let parsed = |v: &Option<String>| v.as_deref().and_then(aterm_types::scheme::parse_hex);
        let themes = ThemeCatalog::default();
        let mut cases = 0;
        for selector in &selectors {
            for extra in overrides {
                let mut config = cfg(extra);
                config.theme.clone_from(selector);
                for appearance in [Appearance::Dark, Appearance::Light] {
                    let label = format!("{selector:?} / {extra:?} / {appearance:?}");
                    let r = config.resolved_colors(appearance, &themes, None);
                    let t = config.theme_for_with_assets(appearance, &themes);
                    let tc = config.applied_terminal_config_for_with_assets(appearance, &themes);
                    assert_eq!(unpack(t.fg), tc.default_foreground, "fg: {label}");
                    assert_eq!(unpack(t.bg), tc.default_background, "bg: {label}");
                    assert_eq!(Some(unpack(t.cursor)), tc.cursor_color, "cursor: {label}");
                    assert_eq!(
                        Some(unpack(t.selection)),
                        tc.selection_background,
                        "selection: {label}"
                    );
                    assert_eq!(tc.default_foreground, r.scheme.foreground, "{label}");
                    assert_eq!(tc.default_background, r.scheme.background, "{label}");
                    assert_eq!(tc.selection_foreground, r.selection_fg(), "{label}");
                    assert_eq!(tc.bold_is_bright, r.bold_is_bright, "{label}");
                    let engine = engine_view(tc.clone()).1;
                    let resolved = r
                        .custom_palette()
                        .map(|p| (0..=255u8).map(|i| p.get(i)).collect::<Vec<_>>());
                    assert_eq!(engine, resolved, "palette: {label}");
                    for (key, value, drawn) in [
                        (
                            "foreground",
                            &config.foreground,
                            Some(tc.default_foreground),
                        ),
                        (
                            "background",
                            &config.background,
                            Some(tc.default_background),
                        ),
                        ("cursor_color", &config.cursor_color, tc.cursor_color),
                        (
                            "selection_color",
                            &config.selection_color,
                            tc.selection_background,
                        ),
                        (
                            "selection_foreground",
                            &config.selection_foreground,
                            tc.selection_foreground,
                        ),
                    ] {
                        if let Some(want) = parsed(value) {
                            assert_eq!(drawn, Some(want), "{key}: {label}");
                        }
                    }
                    for (i, hex) in config.palette.iter().flatten().enumerate() {
                        if let Some(want) = aterm_types::scheme::parse_hex(hex) {
                            let got = engine.as_ref().map(|p| p[i]);
                            assert_eq!(got, Some(want), "palette[{i}]: {label}");
                        }
                    }
                    cases += 1;
                }
            }
        }
        assert!(cases > 100, "the matrix ran: {cases}");
    }

    /// An unset `theme` (absent, blank, or naming no theme) is Default on
    /// every appearance: on a light one it draws exactly what an explicit
    /// `theme = "Default"` draws, reports Dark to DSR 996, and hands the
    /// engine the very config a dark one gets (no custom palette: Default's
    /// slots are the engine's own xterm table).
    #[test]
    fn resolved_colors_unset_theme_is_the_dark_default() {
        let themes = ThemeCatalog::default();
        let explicit = cfg("theme = \"Default\"");
        let default = resolve(&explicit, Appearance::Light);
        for unset in [
            Config::default(),
            cfg("theme = \"\""),
            cfg("theme = \"dark:,light:\""),
        ] {
            let label = format!("{:?}", unset.theme);
            let light = resolve(&unset, Appearance::Light);
            assert_eq!(light.id, "Default", "{label}");
            assert_eq!(light.source, ThemeSource::Builtin, "{label}");
            assert_eq!(light.scheme, default.scheme, "{label}");
            assert_eq!(light.scheme, ColorScheme::default(), "{label}");
            assert_eq!(light.reported, Appearance::Dark, "{label}");
            assert_eq!(
                light.custom_palette(),
                None,
                "{label}: the engine keeps its table"
            );
            assert_eq!(
                unset.theme_for_with_assets(Appearance::Light, &themes),
                explicit.theme_for_with_assets(Appearance::Light, &themes),
                "{label}"
            );
            assert_eq!(
                unset.applied_terminal_config_for_with_assets(Appearance::Light, &themes),
                unset.applied_terminal_config_for_with_assets(Appearance::Dark, &themes),
                "{label}: one engine config on both appearances"
            );
        }

        let pair = resolve(
            &cfg("theme = \"dark:Default,light:Default Light\""),
            Appearance::Light,
        );
        assert_eq!(pair.id, "Default Light");
        assert_eq!(pair.reported, Appearance::Light);
        assert_ne!(pair.scheme.background, default.scheme.background);
    }

    /// Each value that is not a colour is one issue, under its own key; the
    /// slot keeps the theme's colour and the rest still apply.
    #[test]
    fn resolved_colors_reports_a_bad_hex_once() {
        let config = cfg(
            "theme = \"Dracula\"\nforeground = \"#zzzzzz\"\nbackground = \"#102030\"\n\
             palette = [\"#010203\", \"nope\", \"#040506\"]",
        );
        let resolved = resolve(&config, Appearance::Dark);
        let keys: Vec<&str> = resolved.issues.iter().map(|i| i.key.as_str()).collect();
        assert_eq!(keys, ["foreground", "palette[1]"]);
        let dracula = aterm_types::scheme::builtin("Dracula").unwrap();
        assert_eq!(resolved.scheme.foreground, dracula.foreground);
        assert_eq!(resolved.provenance.foreground, ColorOrigin::Theme);
        assert_eq!(resolved.scheme.background, Rgb::new(0x10, 0x20, 0x30));
        assert_eq!(resolved.scheme.ansi[1], dracula.ansi[1]);
        assert_eq!(resolved.scheme.ansi[2], Rgb::new(4, 5, 6));
        let warnings = crate::diagnostics::validate_config_text(
            "foreground = \"#zzzzzz\"\npalette = [\"#010203\", \"nope\"]\n",
        )
        .expect("valid TOML")
        .join("\n");
        assert_eq!(
            warnings.matches("foreground: expected").count(),
            1,
            "{warnings}"
        );
        assert_eq!(warnings.matches("palette[1]").count(), 1, "{warnings}");
        assert!(
            resolve(&cfg("foreground = \"#102030\""), Appearance::Dark)
                .issues
                .is_empty()
        );
    }

    /// `#RGB` is a colour in aterm.toml as in theme files and OSC: each digit
    /// widens to a byte.
    #[test]
    fn resolved_colors_accepts_short_hex() {
        let resolved = resolve(
            &cfg("foreground = \"#fff\"\nbackground = \"#123\"\npalette = [\"#f00\"]"),
            Appearance::Dark,
        );
        assert!(resolved.issues.is_empty(), "{:?}", resolved.issues);
        assert_eq!(resolved.scheme.foreground, Rgb::new(0xff, 0xff, 0xff));
        assert_eq!(resolved.scheme.background, Rgb::new(0x11, 0x22, 0x33));
        assert_eq!(resolved.scheme.ansi[0], Rgb::new(0xff, 0, 0));
        assert_eq!(resolved.theme().fg, 0x00ff_ffff);
        let bad = resolve(&cfg("foreground = \"#ffff\""), Appearance::Dark);
        assert_eq!(bad.issues.len(), 1);
        assert_eq!(bad.scheme.foreground, ColorScheme::default().foreground);
    }

    /// THE CHROME KEYS: a theme file's chrome colour is drawn unless
    /// `aterm.toml` sets the same key, which wins with provenance Config and
    /// is named by `config_keys`.
    #[test]
    fn resolved_colors_chrome_keys_beat_the_theme() {
        let mut scheme = aterm_types::scheme::builtin("Dracula").unwrap();
        scheme.name = "Mine".to_string();
        scheme.chrome.accent = Some(Rgb::new(0x11, 0x22, 0x33));
        scheme.chrome.border = Some(Rgb::new(0x44, 0x55, 0x66));
        let themes = ThemeCatalog::from_schemes([("Mine".to_string(), scheme)]);
        let at = |text: &str| cfg(text).resolved_colors(Appearance::Dark, &themes, None);
        let won = at("theme = \"Mine\"\naccent_color = \"#ff00aa\"");
        assert_eq!(won.scheme.chrome.accent, Some(Rgb::new(0xff, 0x00, 0xaa)));
        assert_eq!(won.provenance.chrome.accent, ColorOrigin::Config);
        assert_eq!(won.config_keys(), vec!["accent_color".to_string()]);
        assert_eq!(won.authored_chrome().accent, Some([0xff, 0x00, 0xaa]));
        assert_eq!(won.scheme.chrome.border, Some(Rgb::new(0x44, 0x55, 0x66)));
        assert_eq!(won.provenance.chrome.border, ColorOrigin::Theme);
        assert_eq!(won.provenance.chrome.search_match, ColorOrigin::Default);
        let bad = at("theme = \"Mine\"\naccent_color = \"#nope\"");
        assert_eq!(bad.issues.len(), 1, "{:?}", bad.issues);
        assert_eq!(bad.issues[0].key, "accent_color");
        assert_eq!(bad.scheme.chrome.accent, Some(Rgb::new(0x11, 0x22, 0x33)));
        assert_eq!(bad.provenance.chrome.accent, ColorOrigin::Theme);
        let theirs = at("theme = \"Mine\"");
        assert_eq!(theirs.provenance.chrome.accent, ColorOrigin::Theme);
        assert!(theirs.config_keys().is_empty());
        let all = cfg(
            "accent_color = \"#010101\"\nactive_tab_color = \"#020202\"\n\
                       border_color = \"#030303\"\nsearch_match_color = \"#040404\"",
        )
        .resolved_colors(Appearance::Dark, &ThemeCatalog::default(), None);
        assert_eq!(
            all.config_keys(),
            super::CHROME_KEYS
                .map(|(name, _)| name.to_string())
                .to_vec()
        );
        assert_eq!(
            all.authored_chrome(),
            aterm_messages::tokens::AuthoredChrome {
                accent: Some([1; 3]),
                active_tab: Some([2; 3]),
                border: Some([3; 3]),
                search_match: Some([4; 3]),
            }
        );
    }

    /// Provenance names who set each slot: the theme, a config key, or nobody.
    #[test]
    fn resolved_colors_provenance_names_each_slot() {
        let none = resolve(&Config::default(), Appearance::Dark);
        assert_eq!(none.source, ThemeSource::Builtin);
        assert_eq!(none.id, "Default");
        assert_eq!(none.provenance.foreground, ColorOrigin::Default);
        assert_eq!(none.provenance.selection_fg, ColorOrigin::Default);
        assert!(none.config_keys().is_empty());
        assert_eq!(
            none.custom_palette(),
            None,
            "no theme, no palette: engine table"
        );

        let themed = resolve(
            &cfg("theme = \"dracula\"\ncursor_color = \"#ff00ff\"\npalette = [\"#000001\"]"),
            Appearance::Dark,
        );
        assert_eq!(themed.id, "Dracula", "canonical built-in name");
        assert_eq!(themed.provenance.foreground, ColorOrigin::Theme);
        assert_eq!(themed.provenance.cursor, ColorOrigin::Config);
        assert_eq!(themed.provenance.ansi[0], ColorOrigin::Config);
        assert_eq!(themed.provenance.ansi[1], ColorOrigin::Theme);
        assert_eq!(themed.config_keys(), ["cursor_color", "palette[0]"]);

        let extended = resolve(
            &cfg(&format!(
                "palette = [{}\"#0a0b0c\"]",
                "\"#000000\", ".repeat(20)
            )),
            Appearance::Dark,
        );
        assert_eq!(
            extended.extended_palette.last(),
            Some(&(20, Rgb::new(10, 11, 12)))
        );
        let palette = extended.custom_palette().expect("a palette entry landed");
        assert_eq!(palette.get(20), Rgb::new(10, 11, 12));
        assert_eq!(palette.get(21), ColorPalette::default_color(21));

        let missing = resolve(&cfg("theme = \"no-such-theme\""), Appearance::Dark);
        assert!(matches!(
            missing.source,
            ThemeSource::DefaultFallback { .. }
        ));
        assert_eq!(missing.provenance.background, ColorOrigin::Default);
        assert_eq!(missing.scheme.background, ColorScheme::default().background);
    }

    /// A custom theme file resolves to its catalog id; the theme file's own
    /// selection foreground reaches the engine, and the config's beats it.
    #[test]
    fn resolved_colors_reads_a_file_theme_and_its_selection_foreground() {
        let scheme = ColorScheme {
            name: "Mine".into(),
            selection_fg: Some(Rgb::new(1, 2, 3)),
            ..ColorScheme::default()
        };
        let themes = ThemeCatalog::from_schemes([("Mine".to_string(), scheme)]);
        let config = cfg("theme = \"mine\"");
        let resolved = config.resolved_colors(Appearance::Dark, &themes, None);
        assert_eq!(resolved.source, ThemeSource::File("Mine".into()));
        assert_eq!(resolved.id, "Mine");
        assert_eq!(resolved.provenance.selection_fg, ColorOrigin::Theme);
        assert_eq!(
            config
                .applied_terminal_config_from(&resolved)
                .selection_foreground,
            Some(Rgb::new(1, 2, 3))
        );
        let over = cfg("theme = \"mine\"\nselection_foreground = \"#040506\"").resolved_colors(
            Appearance::Dark,
            &themes,
            None,
        );
        assert_eq!(over.selection_fg(), Some(Rgb::new(4, 5, 6)));
        assert_eq!(over.provenance.selection_fg, ColorOrigin::Config);
    }

    /// A candidate stands in for the configured theme (a preview), and the
    /// config's colour keys still apply over it.
    #[test]
    fn resolved_colors_candidate_replaces_the_configured_theme() {
        let config = cfg("theme = \"Dracula\"\nforeground = \"#102030\"");
        let preview =
            config.resolved_colors(Appearance::Dark, &ThemeCatalog::default(), Some("Nord"));
        assert_eq!(preview.id, "Nord");
        let nord = aterm_types::scheme::builtin("Nord").unwrap();
        assert_eq!(preview.scheme.background, nord.background);
        assert_eq!(preview.scheme.foreground, Rgb::new(0x10, 0x20, 0x30));
        let split = config.resolved_colors(
            Appearance::Light,
            &ThemeCatalog::default(),
            Some("dark:Nord,light:Solarized Light"),
        );
        assert_eq!(split.id, "Solarized Light");
        assert_eq!(split.side, Appearance::Light);
    }

    /// The reported appearance is the theme's own while the background is the
    /// theme's, and the final background's polarity once config overrides it.
    #[test]
    fn resolved_colors_reported_follows_the_background_on_screen() {
        let light = "Solarized Light";
        assert_eq!(
            aterm_types::scheme::builtin(light).unwrap().appearance,
            Appearance::Light
        );
        let plain = resolve(&cfg(&format!("theme = \"{light}\"")), Appearance::Dark);
        assert_eq!(plain.reported, Appearance::Light, "the theme's appearance");
        let fg_only = resolve(
            &cfg(&format!("theme = \"{light}\"\nforeground = \"#000000\"")),
            Appearance::Dark,
        );
        assert_eq!(fg_only.reported, Appearance::Light);
        let dark_bg = resolve(
            &cfg(&format!("theme = \"{light}\"\nbackground = \"#101010\"")),
            Appearance::Light,
        );
        assert_eq!(
            dark_bg.reported,
            Appearance::Dark,
            "a dark background reads dark"
        );
        let light_bg = resolve(&cfg("background = \"#f8f8f8\""), Appearance::Dark);
        assert_eq!(light_bg.reported, Appearance::Light);
        let bad_bg = resolve(&cfg("background = \"#f8f8\""), Appearance::Dark);
        assert_eq!(bad_bg.reported, Appearance::Dark);
    }

    /// A light file theme with no `selection` paints a light band, derived
    /// from the colours on screen: a `background` override moves it too, and
    /// the renderer and the engine get the same band.
    #[test]
    fn resolved_colors_derives_an_unset_selection_from_the_final_colours() {
        let light = aterm_types::scheme::parse_scheme_str(
            "appearance = light\nforeground = #24292f\nbackground = #fafafa\n",
        )
        .expect("a minimal light theme parses");
        let themes = ThemeCatalog::from_schemes([("paper".to_string(), light)]);
        let plain = cfg("theme = \"paper\"").resolved_colors(Appearance::Light, &themes, None);
        let (fg, bg) = (plain.scheme.foreground, plain.scheme.background);
        let band = plain.scheme.selection_band();
        assert!(
            band.contrast(bg) < band.contrast(fg),
            "a light band: {band:?}"
        );
        assert!(band.contrast(bg) >= aterm_types::scheme::MIN_SELECTION_CONTRAST);
        let config = cfg("theme = \"paper\"");
        let engine = config.applied_terminal_config_from(&plain);
        assert_eq!(engine.selection_background, Some(band));
        assert_eq!(
            plain.theme().selection,
            Theme::from(&plain.scheme).selection
        );

        let over = cfg("theme = \"paper\"\nbackground = \"#e0e0e0\"").resolved_colors(
            Appearance::Light,
            &themes,
            None,
        );
        assert_eq!(
            over.scheme.selection_band(),
            aterm_types::scheme::derived_selection(fg, Rgb::new(0xe0, 0xe0, 0xe0)),
            "the band follows the background on screen"
        );
    }

    /// A split with a `background` chosen for its dark side leaves the light
    /// side's text illegible, and validation says which side and why.
    #[test]
    fn split_override_warnings_name_the_side_an_override_breaks() {
        let themes = ThemeCatalog::default();
        let warnings = cfg("theme = \"dark:Dracula,light:GitHub Light\"\nbackground = \"#000\"")
            .split_override_warnings(&themes);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        let (key, message) = &warnings[0];
        assert_eq!(*key, "background");
        assert!(message.contains("light mode"), "{message}");
        assert!(message.contains("\"GitHub Light\""), "{message}");
        assert!(!message.contains("dark mode"), "{message}");

        let palette = cfg(
            "theme = \"dark:Dracula,light:GitHub Light\"\npalette = [\"#000000\", \"#ffb0b0\"]",
        )
        .split_override_warnings(&themes);
        assert_eq!(palette.len(), 1, "{palette:?}");
        assert_eq!(palette[0].0, "palette");
        assert!(palette[0].1.contains("palette[1]"), "{palette:?}");
        assert!(!palette[0].1.contains("palette[0]"), "{palette:?}");

        for quiet in [
            "theme = \"GitHub Light\"\nbackground = \"#000\"",
            "theme = \"Dracula\"\nbackground = \"#000\"",
            "theme = \"dark:Dracula,light:GitHub Light\"",
            "theme = \"dark:Dracula,light:GitHub Light\"\nbackground = \"#808080\"\nforeground = \"#000000\"",
        ] {
            let found = cfg(quiet).split_override_warnings(&themes);
            assert!(found.is_empty(), "{quiet}: {found:?}");
        }
    }

    /// `palette_overrides` sets single slots over the theme (and over the
    /// `palette` list): a slot it does not name follows whichever theme is
    /// drawn, so one tuned slot never pins the others.
    #[test]
    fn palette_overrides_set_one_slot_and_leave_the_rest_to_the_theme() {
        let sparse = cfg("palette_overrides = { 12 = \"#123456\" }");
        let themes = ThemeCatalog::default();
        for theme in ["Nord", "Dracula"] {
            let resolved = sparse.resolved_colors(Appearance::Dark, &themes, Some(theme));
            let alone = cfg("").resolved_colors(Appearance::Dark, &themes, Some(theme));
            assert_eq!(resolved.scheme.ansi[12], Rgb::new(0x12, 0x34, 0x56));
            for slot in (0..16).filter(|slot| *slot != 12) {
                assert_eq!(
                    resolved.scheme.ansi[slot], alone.scheme.ansi[slot],
                    "{theme} {slot}"
                );
                assert_eq!(resolved.provenance.ansi[slot], ColorOrigin::Theme);
            }
            assert_eq!(resolved.provenance.ansi[12], ColorOrigin::Config);
            assert_eq!(resolved.config_keys(), ["palette_overrides.12"]);
        }
        let pinned = cfg(&format!(
            "palette = [{}\"#123456\"]",
            "\"#000000\", ".repeat(12)
        ))
        .resolved_colors(Appearance::Dark, &themes, Some("Dracula"));
        let dracula = cfg("").resolved_colors(Appearance::Dark, &themes, Some("Dracula"));
        assert_ne!(
            pinned.scheme.ansi[1], dracula.scheme.ansi[1],
            "the list pins the slots before the one it reaches"
        );
    }

    /// An override lands over the list's entry for the same slot, reaches
    /// past the sixteen into the extended palette, sorts by slot, and a key
    /// that is no slot or a value that is no colour is an issue, once.
    #[test]
    fn palette_overrides_land_over_the_list_and_report_bad_entries() {
        let resolved = resolve(
            &cfg("palette = [\"#000001\", \"#000002\"]\n\
                  [palette_overrides]\n\
                  1 = \"#0000ff\"\n\
                  10 = \"#00000a\"\n\
                  2 = \"#000003\"\n\
                  200 = \"#0a0b0c\"\n\
                  07 = \"#ffffff\"\n\
                  x = \"#ffffff\"\n\
                  3 = \"nope\"\n"),
            Appearance::Dark,
        );
        assert_eq!(resolved.scheme.ansi[0], Rgb::new(0, 0, 1));
        assert_eq!(resolved.scheme.ansi[1], Rgb::new(0, 0, 0xff));
        assert_eq!(resolved.scheme.ansi[2], Rgb::new(0, 0, 3));
        assert_eq!(resolved.scheme.ansi[10], Rgb::new(0, 0, 0x0a));
        assert_eq!(resolved.extended_palette, vec![(200, Rgb::new(10, 11, 12))]);
        assert_eq!(
            resolved.custom_palette().expect("a palette").get(200),
            Rgb::new(10, 11, 12)
        );
        assert_eq!(
            resolved.config_keys(),
            [
                "palette[0]",
                "palette[1]",
                "palette_overrides.1",
                "palette_overrides.2",
                "palette_overrides.10",
                "palette_overrides.200",
            ]
        );
        let issues: Vec<&str> = resolved.issues.iter().map(|i| i.key.as_str()).collect();
        assert_eq!(
            issues,
            [
                "palette_overrides.07",
                "palette_overrides.3",
                "palette_overrides.x"
            ]
        );
    }

    /// The page palette is a by-product of the one resolution: the theme as it
    /// is before the four colour keys that reach the renderer's theme, equal to
    /// resolving the same config with those keys left out, on both sides of a
    /// split and under a preview candidate, with `palette[]`, the sparse slots
    /// and `selection_foreground` set too (none of which reach a `Theme`).
    #[test]
    fn resolved_colors_keeps_the_theme_before_the_colour_overrides() {
        let palette: Vec<String> = (0..256)
            .map(|i| format!("\"#{:02x}{:02x}{:02x}\"", i, 255 - i, i / 2))
            .collect();
        let extras = format!(
            "palette = [{}]\nselection_foreground = \"#123456\"\n[palette_overrides]\n3 = \"#ff0000\"\n",
            palette.join(", ")
        );
        let overrides = "foreground = \"#ffaa00\"\nbackground = \"#002244\"\ncursor_color = \"#00ff00\"\nselection_color = \"#440044\"\n";
        for theme in ["", "theme = \"dark:Dracula,light:Solarized Light\"\n"] {
            for candidate in [None, Some("Nord")] {
                for side in [Appearance::Dark, Appearance::Light] {
                    let bare = cfg(&format!("{theme}{extras}"));
                    let set = cfg(&format!("{theme}{overrides}{extras}"));
                    let themes = ThemeCatalog::default();
                    let bare = bare.resolved_colors(side, &themes, candidate);
                    let set = set.resolved_colors(side, &themes, candidate);
                    assert_eq!(
                        set.theme_before_overrides(),
                        bare.theme(),
                        "{theme:?} {candidate:?} {side:?}: the theme before the keys"
                    );
                    assert_eq!(
                        bare.theme_before_overrides(),
                        bare.theme(),
                        "with no colour key the two are the same"
                    );
                    assert_ne!(set.theme(), set.theme_before_overrides());
                    assert_eq!(set.theme().fg, 0x00FF_AA00);
                }
            }
        }
    }

    /// The slot-100 value the derivation gives `theme`'s colours.
    fn generated_100(config: &Config, theme: Option<&str>) -> Rgb {
        let mut scheme = config
            .resolved_colors(Appearance::Dark, &ThemeCatalog::default(), theme)
            .scheme;
        scheme.palette_generate = true;
        scheme.to_color_palette().get(100)
    }

    /// Spec `resolved-colors-generate`: a signature theme derives slots
    /// 16-255 from its own colours by its key; `palette_generate = "never"`
    /// keeps xterm's (the negative control); `"always"` derives them even for
    /// the unset theme, which otherwise hands the engine no palette.
    #[test]
    fn palette_generate_follows_the_theme_unless_the_config_says() {
        let themes = ThemeCatalog::default();
        let plain = cfg("");
        let night = plain.resolved_colors(Appearance::Dark, &themes, Some("Aterm Night"));
        assert!(night.palette_generate);
        assert_eq!(night.palette_generate_origin, ColorOrigin::Theme);
        let palette = night
            .custom_palette()
            .expect("a theme hands its palette over");
        assert_eq!(palette.get(100), generated_100(&plain, Some("Aterm Night")));
        assert_ne!(palette.get(100), ColorPalette::default_color(100));

        let never = cfg("palette_generate = \"never\"");
        let kept = never.resolved_colors(Appearance::Dark, &themes, Some("Aterm Night"));
        assert!(!kept.palette_generate);
        assert_eq!(kept.palette_generate_origin, ColorOrigin::Config);
        assert_eq!(
            kept.custom_palette().expect("palette").get(100),
            ColorPalette::default_color(100)
        );

        let nord = plain.resolved_colors(Appearance::Dark, &themes, Some("Nord"));
        assert!(!nord.palette_generate);
        assert_eq!(
            nord.custom_palette().expect("palette").get(100),
            ColorPalette::default_color(100)
        );

        assert_eq!(resolve(&plain, Appearance::Dark).custom_palette(), None);
        let always = cfg("palette_generate = \"always\"");
        let unset = resolve(&always, Appearance::Dark);
        assert!(unset.palette_generate);
        assert_eq!(unset.palette_generate_origin, ColorOrigin::Config);
        assert_eq!(
            unset.custom_palette().expect("derived").get(100),
            generated_100(&always, None)
        );
        let odd = cfg("palette_generate = \"sometimes\"");
        assert!(
            odd.resolved_colors(Appearance::Dark, &themes, Some("Aterm Night"))
                .palette_generate
        );
    }

    /// Spec `resolved-colors-generate`: a config slot past 15 wins over a
    /// derived one and leaves its neighbours derived, and the cube follows
    /// an ANSI override (slot 196 is `palette[9]`), so `theme palette` names
    /// each slot's origin.
    #[test]
    fn config_slots_win_over_derived_ones_and_the_cube_follows_ansi_overrides() {
        let themes = ThemeCatalog::default();
        let config = cfg("palette_overrides = { 100 = \"#123456\", 9 = \"#00ff00\" }");
        let resolved = config.resolved_colors(Appearance::Dark, &themes, Some("Aterm Night"));
        let palette = resolved.custom_palette().expect("palette");
        assert_eq!(palette.get(100), Rgb::new(0x12, 0x34, 0x56));
        assert_eq!(palette.get(196), Rgb::new(0x00, 0xff, 0x00));
        assert_eq!(
            palette.get(101),
            {
                let mut scheme = resolved.scheme.clone();
                scheme.palette_generate = true;
                scheme.to_color_palette().get(101)
            },
            "the neighbour stays derived"
        );
        use super::SlotOrigin;
        assert_eq!(resolved.extended_origin(100), SlotOrigin::Config);
        assert_eq!(resolved.extended_origin(101), SlotOrigin::Generated);
        assert_eq!(
            resolved.extended_origin(9),
            SlotOrigin::Ansi(ColorOrigin::Config)
        );
        assert_eq!(
            resolved.extended_origin(1),
            SlotOrigin::Ansi(ColorOrigin::Theme)
        );
        let off = cfg("palette_generate = \"never\"\npalette_overrides = { 100 = \"#123456\" }")
            .resolved_colors(Appearance::Dark, &themes, Some("Aterm Night"));
        assert_eq!(off.extended_origin(101), SlotOrigin::Xterm);
        assert_eq!(off.extended_origin(100), SlotOrigin::Config);
    }
}
