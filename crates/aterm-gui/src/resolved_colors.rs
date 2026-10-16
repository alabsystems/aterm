// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The one colour resolver: [`Config::resolved_colors`] picks the theme for an
//! appearance, applies every colour override in `aterm.toml` once (fg, bg,
//! cursor, selection, selection foreground, `palette[]`), and records where each
//! slot came from. The renderer [`Theme`] and the engine `TerminalConfig`
//! colours are projections of that one result, so "config beats theme" holds in
//! one place, and a bad hex value is reported once, by one reader
//! ([`aterm_types::scheme::parse_hex`]).

use aterm_core::terminal::{ColorPalette, Rgb};
use aterm_render::Theme;
use aterm_types::{Appearance, ColorScheme};

use crate::app_config::{Config, ThemeCatalog, resolve_theme_name_value, unset_theme_name};

/// Where the theme a resolution draws came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ThemeSource {
    /// A built-in scheme; also what an unset `theme` draws (Default, or
    /// Default Light on a light appearance: [`crate::app_config::UNSET_THEME`]).
    Builtin,
    /// A loaded custom theme file, by its catalog id (the file stem).
    File(String),
    /// The selected name does not resolve, so Default is drawn. `reason` says
    /// why, in the words [`crate::app_config::theme_catalog_misses`] uses.
    DefaultFallback { reason: String },
}

/// Who chose one colour slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ColorOrigin {
    /// The selected theme (built-in or file).
    Theme,
    /// A colour key in `aterm.toml`.
    Config,
    /// Nobody: the unset default (no theme selected: Default, or Default Light
    /// on a light appearance), the Default fallback (the selected theme does
    /// not resolve), or for `selection_fg` / a missing cursor the renderer's
    /// own rule.
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
}

/// One colour value in `aterm.toml` that is not a hex colour. The slot keeps
/// the theme's colour.
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

/// The colours one appearance draws: the selected theme with every
/// `aterm.toml` colour override applied, plus where each slot came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResolvedColors {
    /// The theme drawn: a built-in's canonical name, a file's catalog id, or
    /// `Default` on a fallback.
    pub(crate) id: String,
    /// The appearance this resolution is for; it picked the side of a
    /// `dark:…,light:…` selector.
    pub(crate) side: Appearance,
    pub(crate) source: ThemeSource,
    /// The final colours, overrides included: `ansi` carries `palette[0..16]`
    /// and `selection_fg` the theme's or the config's selection foreground.
    pub(crate) scheme: ColorScheme,
    /// `palette[]` entries past the 16 ANSI slots, as (index, colour). The
    /// xterm cube and grey ramp fill the rest.
    pub(crate) extended_palette: Vec<(u8, Rgb)>,
    /// The `palette[i]` and `palette_overrides.i` keys that landed, as slot
    /// indices, in key order: what [`Self::config_keys`] names.
    palette_keys: PaletteKeys,
    /// Whether bold text brightens ANSI 0-7 (`bold_is_bright`, else the
    /// engine default).
    pub(crate) bold_is_bright: bool,
    pub(crate) provenance: ColorProvenance,
    /// The appearance the palette on screen reads as, which DSR 996 and mode
    /// 2031 report: the scheme's appearance (declared or measured at
    /// admission) unless `background` is overridden, then the final
    /// background's polarity.
    pub(crate) reported: Appearance,
    /// Every colour value that did not parse, once each, in key order.
    pub(crate) issues: Vec<ColorIssue>,
    /// Whether the engine gets an explicit palette: a theme was selected, the
    /// unset default drew slots other than the xterm table (Default Light), or
    /// a `palette[]` entry landed. Otherwise the engine keeps its xterm table.
    custom_palette: bool,
}

impl ResolvedColors {
    /// The renderer's theme: the final scheme's chrome.
    pub(crate) fn theme(&self) -> Theme {
        Theme::from(&self.scheme)
    }

    /// The selected-text foreground, when the theme or the config sets one;
    /// `None` keeps the renderer's contrast floor.
    pub(crate) fn selection_fg(&self) -> Option<Rgb> {
        self.scheme.selection_fg
    }

    /// The engine's indexed palette: the scheme's 16 ANSI slots plus the
    /// extended `palette[]` entries, or `None` for the engine's own table.
    pub(crate) fn custom_palette(&self) -> Option<ColorPalette> {
        self.custom_palette.then(|| {
            let mut palette = self.scheme.to_color_palette();
            for &(index, rgb) in &self.extended_palette {
                palette.set(index, rgb);
            }
            palette
        })
    }

    /// The `aterm.toml` keys that set a slot here, in slot order (`palette[i]`
    /// per index).
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

/// The `palette_overrides` key for slot `index`, as `aterm.toml` and the
/// Settings patch lane address it.
pub(crate) fn palette_override_key(index: usize) -> String {
    format!("{}.{index}", crate::prefs::EDIT_PALETTE_OVERRIDES)
}

/// The slot a `palette_overrides` table key names: a decimal index 0–255
/// with no sign or leading zero, so each slot has one spelling.
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

/// The lowest text contrast a side of a `dark:…,light:…` split may be left
/// at by the colour keys in `aterm.toml` before validation warns
/// ([`Config::split_override_warnings`]).
pub(crate) const SPLIT_OVERRIDE_MIN_CONTRAST: f64 = 3.0;

/// The theme a selection draws for one appearance, before any `aterm.toml`
/// colour key: its id, where it came from, its colours, whether a theme was
/// named at all, and whether its ANSI slots differ from the engine's own
/// xterm table (so the engine needs them as a custom palette).
struct ThemeDraw {
    id: String,
    source: ThemeSource,
    scheme: ColorScheme,
    named: bool,
    custom_palette: bool,
}

impl Config {
    /// The theme `appearance` draws, before the colour keys apply.
    fn theme_draw(
        &self,
        appearance: Appearance,
        themes: &ThemeCatalog,
        candidate: Option<&str>,
    ) -> ThemeDraw {
        let selector = candidate.or(self.theme.as_deref());
        let named = resolve_theme_name_value(selector, appearance);
        let theme_named = named.is_some();
        let (id, source, scheme) = match named {
            // No name: the unset default follows the OS. Its dark side is
            // `ColorScheme::default()` itself, so a dark appearance draws
            // exactly what it always did.
            None => {
                let scheme =
                    aterm_types::scheme::builtin(unset_theme_name(appearance)).unwrap_or_default();
                (scheme.name.clone(), ThemeSource::Builtin, scheme)
            }
            Some(name) => {
                if let Some(scheme) = aterm_types::scheme::builtin(&name) {
                    (scheme.name.clone(), ThemeSource::Builtin, scheme)
                } else {
                    match themes.resolve(&name) {
                        Ok(scheme) => {
                            let id = themes
                                .ready_names()
                                .find(|ready| ready.eq_ignore_ascii_case(&name))
                                .map_or(name, str::to_string);
                            (id.clone(), ThemeSource::File(id), scheme)
                        }
                        Err(reason) => (
                            "Default".to_string(),
                            ThemeSource::DefaultFallback { reason },
                            ColorScheme::default(),
                        ),
                    }
                }
            }
        };
        // Default's ANSI slots ARE the engine's xterm table, so an unset
        // theme on a dark appearance keeps the engine's own palette; any other
        // draw hands its slots over.
        let custom_palette = theme_named || scheme.ansi != ColorScheme::default().ansi;
        ThemeDraw {
            id,
            source,
            scheme,
            named: theme_named,
            custom_palette,
        }
    }

    /// Resolve the colours `appearance` draws against `themes`, with every
    /// colour override applied once. `candidate`, when given, stands in for the
    /// configured `theme` value (a preview); the config's colour keys still
    /// apply over it.
    pub(crate) fn resolved_colors(
        &self,
        appearance: Appearance,
        themes: &ThemeCatalog,
        candidate: Option<&str>,
    ) -> ResolvedColors {
        let ThemeDraw {
            id,
            source,
            mut scheme,
            named: theme_named,
            custom_palette: theme_palette,
        } = self.theme_draw(appearance, themes, candidate);
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
        };

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
        let mut extended_palette: Vec<(u8, Rgb)> = Vec::new();
        let mut palette_keys = PaletteKeys::default();
        let mut set_slot = |index: usize, rgb: Rgb| {
            match (scheme.ansi.get_mut(index), provenance.ansi.get_mut(index)) {
                (Some(slot), Some(origin)) => {
                    *slot = rgb;
                    *origin = ColorOrigin::Config;
                }
                // Both callers bound the index below 256.
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
        // The sparse slots land over the list: a slot the table does not name
        // keeps the list's colour, else the theme's.
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
        // The table's keys are text, so `10` sorts before `2`: by slot.
        palette_keys.overrides.sort_unstable();
        let palette_landed = !palette_keys.list.is_empty() || !palette_keys.overrides.is_empty();

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
            extended_palette,
            palette_keys,
            bold_is_bright: self
                .bold_is_bright
                .unwrap_or(aterm_core::config::TerminalConfig::default().bold_is_bright),
            provenance,
            reported,
            issues,
            custom_palette: theme_palette || palette_landed,
        }
    }

    /// What the colour keys in `aterm.toml` do to a `dark:…,light:…` split.
    /// They apply to both sides, so a `background` chosen for one theme can
    /// leave the other side's text illegible. One `(key, message)` per key
    /// and side that leaves a side below [`SPLIT_OVERRIDE_MIN_CONTRAST`]
    /// where the side's theme alone clears it: `foreground`/`background` for
    /// the text, `palette` for the overridden ANSI slots. A single theme gets
    /// none (an override then has one theme to suit, and the author sees it).
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
            let theme = self.theme_draw(side, themes, None).scheme;
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
    /// They do not depend on the theme or the appearance.
    pub(crate) fn color_issues(&self) -> Vec<ColorIssue> {
        self.resolved_colors(Appearance::Dark, &ThemeCatalog::default(), None)
            .issues
    }

    /// The engine config to apply: the non-colour keys
    /// ([`Self::engine_behaviour_config`]) with every colour from `resolved`.
    /// The default fg/bg are the window's, so a default cell paints exactly
    /// the window colour.
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
                    // Renderer and engine draw the same final colours.
                    assert_eq!(unpack(t.fg), tc.default_foreground, "fg: {label}");
                    assert_eq!(unpack(t.bg), tc.default_background, "bg: {label}");
                    assert_eq!(Some(unpack(t.cursor)), tc.cursor_color, "cursor: {label}");
                    assert_eq!(
                        Some(unpack(t.selection)),
                        tc.selection_background,
                        "selection: {label}"
                    );
                    // …and they are the resolution's.
                    assert_eq!(tc.default_foreground, r.scheme.foreground, "{label}");
                    assert_eq!(tc.default_background, r.scheme.background, "{label}");
                    assert_eq!(tc.selection_foreground, r.selection_fg(), "{label}");
                    assert_eq!(tc.bold_is_bright, r.bold_is_bright, "{label}");
                    let engine = engine_view(tc.clone()).1;
                    let resolved = r
                        .custom_palette()
                        .map(|p| (0..=255u8).map(|i| p.get(i)).collect::<Vec<_>>());
                    assert_eq!(engine, resolved, "palette: {label}");
                    // Every override that parses is drawn.
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

    /// An unset `theme` follows the OS: a pristine config on a light
    /// appearance draws Default Light (its colours, its ANSI slots handed to
    /// the engine, and a light report for DSR 996), and on a dark one it is
    /// Default with no custom palette: the engine keeps its xterm table.
    #[test]
    fn resolved_colors_unset_theme_follows_the_os() {
        let themes = ThemeCatalog::default();
        let pristine = Config::default();

        let light = resolve(&pristine, Appearance::Light);
        let default_light = aterm_types::scheme::builtin("Default Light").expect("a built-in");
        assert_eq!(light.id, "Default Light");
        assert_eq!(light.source, ThemeSource::Builtin);
        assert_eq!(light.scheme, default_light);
        assert_eq!(light.reported, Appearance::Light);
        assert_eq!(light.provenance.background, ColorOrigin::Default);
        assert!(light.config_keys().is_empty());
        let palette = light.custom_palette().expect("Default Light's slots");
        for (i, slot) in default_light.ansi.iter().enumerate() {
            assert_eq!(palette.get(i as u8), *slot, "ansi {i}");
        }
        // The same pixels as writing the unset rule out.
        let written = cfg(&format!("theme = {:?}", crate::app_config::UNSET_THEME));
        assert_eq!(
            engine_view(
                pristine.applied_terminal_config_for_with_assets(Appearance::Light, &themes)
            ),
            engine_view(
                written.applied_terminal_config_for_with_assets(Appearance::Light, &themes)
            ),
        );
        assert_eq!(
            pristine.theme_for_with_assets(Appearance::Light, &themes),
            written.theme_for_with_assets(Appearance::Light, &themes),
        );

        let dark = resolve(&pristine, Appearance::Dark);
        assert_eq!(dark.id, "Default");
        assert_eq!(dark.scheme, ColorScheme::default());
        assert_eq!(dark.reported, Appearance::Dark);
        assert_eq!(dark.custom_palette(), None, "the engine keeps its table");
        let default = ColorScheme::default();
        let applied = pristine.applied_terminal_config_for_with_assets(Appearance::Dark, &themes);
        assert_eq!(applied.default_foreground, default.foreground);
        assert_eq!(applied.default_background, default.background);
        assert_eq!(applied.custom_palette, None);
        assert_eq!(
            pristine.theme_for_with_assets(Appearance::Dark, &themes),
            Theme::from(&default),
        );

        // Negative controls: a named theme is not replaced on a light
        // appearance, and `theme = "Default"` pins the dark Default there.
        let nord = resolve(&cfg("theme = \"Nord\""), Appearance::Light);
        assert_eq!(nord.id, "Nord");
        let pinned = resolve(&cfg("theme = \"Default\""), Appearance::Light);
        assert_eq!(pinned.id, "Default");
        assert_eq!(pinned.scheme.background, ColorScheme::default().background);
        assert_ne!(light.scheme.background, pinned.scheme.background);
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
        // `--validate-config` says each one once, from the same reader.
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
        // A clean config has no issues (negative control).
        assert!(
            resolve(&cfg("foreground = \"#102030\""), Appearance::Dark)
                .issues
                .is_empty()
        );
    }

    /// `#RGB` is a colour in aterm.toml as in theme files and OSC: each digit
    /// widens to a byte. Four digits are not a colour.
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
        // A foreground override does not move it (negative control).
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
        // A background value that does not parse leaves the theme's answer.
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
    /// side's text illegible, and validation says which side and why. Negative
    /// controls: the same override on a single theme, a split whose override
    /// suits both sides, and a split with no colour keys say nothing.
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

        // A palette entry chosen for the dark side fades on the light one.
        let palette = cfg(
            "theme = \"dark:Dracula,light:GitHub Light\"\npalette = [\"#000000\", \"#ffb0b0\"]",
        )
        .split_override_warnings(&themes);
        assert_eq!(palette.len(), 1, "{palette:?}");
        assert_eq!(palette[0].0, "palette");
        assert!(palette[0].1.contains("palette[1]"), "{palette:?}");
        // palette[0] is black, which Dracula's own black already is below
        // 3:1 on: the override changes nothing a reader relied on.
        assert!(!palette[0].1.contains("palette[0]"), "{palette:?}");

        for quiet in [
            "theme = \"GitHub Light\"\nbackground = \"#000\"",
            "theme = \"Dracula\"\nbackground = \"#000\"",
            "theme = \"dark:Dracula,light:GitHub Light\"",
            "theme = \"dark:Dracula,light:GitHub Light\"\nbackground = \"#808080\"\nforeground = \"#000000\"",
        ] {
            // (The last: black text on a mid grey is 5.3:1 on both sides.)
            let found = cfg(quiet).split_override_warnings(&themes);
            assert!(found.is_empty(), "{quiet}: {found:?}");
        }
    }

    /// `palette_overrides` sets single slots over the theme (and over the
    /// `palette` list): a slot it does not name follows whichever theme is
    /// drawn, so one tuned slot never pins the others. Negative control: the
    /// positional list that reaches the same slot pins every slot before it.
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
}
