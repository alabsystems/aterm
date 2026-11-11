// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

//! Web terminal named themes.

use super::*;

use aterm_types::scheme::{
    builtin_theme, builtin_theme_list, parse_theme_str, validate_user_theme_name, AppearanceSource,
    CatalogTheme, ColorScheme, SelectorDraw, ThemeMeta, ThemeSelector, MAX_USER_THEME_FILES,
    MAX_USER_THEME_FILE_BYTES, UNSET_THEME,
};
use aterm_types::Appearance;

/// How many themes a host may register: the native catalog's `MAX_USER_THEME_FILES`.
pub(crate) const MAX_REGISTERED_THEMES: usize = MAX_USER_THEME_FILES;

/// Where an installed theme came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WebThemeSource {
    /// One of aterm's built-ins.
    Builtin,
    /// A theme the host registered with `register_theme`.
    Registered,
}

/// One resolved side of a selector: the theme a terminal draws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WebDraw {
    /// The theme's canonical id.
    pub(crate) id: String,
    /// The appearance it was drawn for (the host's last `set_color_scheme`).
    pub(crate) side: Appearance,
    /// Built-in or registered.
    pub(crate) source: WebThemeSource,
    /// Its colours.
    pub(crate) scheme: ColorScheme,
}

/// The web terminal's theme state.
#[derive(Debug)]
pub(crate) struct WebThemeState {
    /// The selector `set_theme_by_name` last installed.
    pub(crate) selector: Option<String>,
    /// The host's last `set_color_scheme` appearance.
    pub(crate) appearance: Appearance,
    /// Registered themes, in insertion order, at most [`MAX_REGISTERED_THEMES`].
    registered: Vec<CatalogTheme>,
    /// What the selector put on screen.
    pub(crate) installed: Option<WebDraw>,
    /// Whether the host's `set_theme` set the colours after construction.
    pub(crate) host_set: bool,
}

#[inline]
const fn appearance_word(a: Appearance) -> &'static str {
    match a {
        Appearance::Dark => "dark",
        Appearance::Light => "light",
    }
}

#[inline]
const fn source_word(source: AppearanceSource) -> &'static str {
    match source {
        AppearanceSource::Declared => "declared",
        AppearanceSource::Measured => "measured",
    }
}

#[inline]
fn json_opt(s: Option<&str>) -> String {
    s.map_or_else(|| "null".to_string(), json_string)
}

fn json_warnings(meta: &ThemeMeta) -> String {
    let rows: Vec<String> = meta
        .warnings
        .iter()
        .map(|w| json_string(&w.to_string()))
        .collect();
    format!("[{}]", rows.join(","))
}

/// Pack an engine colour as `0x00RRGGBB`.
#[inline]
pub(crate) fn pack(c: Rgb) -> u32 {
    (u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b)
}

/// Unpack `0x00RRGGBB` into an engine colour.
#[inline]
pub(crate) const fn unpack(c: u32) -> Rgb {
    let [_, r, g, b] = c.to_be_bytes();
    Rgb { r, g, b }
}

impl WebThemeState {
    #[inline]
    pub(crate) const fn new(appearance: Appearance) -> Self {
        Self {
            selector: None,
            appearance,
            registered: Vec::new(),
            installed: None,
            host_set: false,
        }
    }

    #[inline]
    fn find_registered(&self, name: &str) -> Option<&CatalogTheme> {
        self.registered
            .iter()
            .find(|t| t.id.eq_ignore_ascii_case(name.trim()))
    }

    /// Resolve the side of `raw` that `appearance` draws, without mutating.
    pub(crate) fn resolve(&self, raw: &str, appearance: Appearance) -> Result<WebDraw, String> {
        let (selector, issues) = ThemeSelector::parse(raw);
        if !issues.is_empty() {
            let text: Vec<String> = issues.iter().map(ToString::to_string).collect();
            return Err(text.join("; "));
        }
        for (_, name) in selector.named_sides() {
            if builtin_theme(name).is_none() && self.find_registered(name).is_none() {
                return Err(format!("no built-in or registered theme named {name:?}"));
            }
        }
        match selector.draw_builtin(appearance) {
            SelectorDraw::Builtin(scheme) | SelectorDraw::Unset(scheme) => Ok(WebDraw {
                id: scheme.name.clone(),
                side: appearance,
                source: WebThemeSource::Builtin,
                scheme,
            }),
            SelectorDraw::Other(name) => {
                let theme = self
                    .find_registered(&name)
                    .ok_or_else(|| format!("no built-in or registered theme named {name:?}"))?;
                Ok(WebDraw {
                    id: theme.id.clone(),
                    side: appearance,
                    source: WebThemeSource::Registered,
                    scheme: theme.theme.scheme.clone(),
                })
            }
        }
    }

    /// Admit `text` (a theme file) as a registered theme under its `name =`.
    pub(crate) fn register(&mut self, text: &str) -> Result<&CatalogTheme, String> {
        if text.len() > MAX_USER_THEME_FILE_BYTES {
            return Err(format!(
                "theme text is larger than {} KiB",
                MAX_USER_THEME_FILE_BYTES / 1024
            ));
        }
        let mut parsed = parse_theme_str(text).map_err(|e| e.to_string())?;
        let id = parsed.scheme.name.trim().to_string();
        if id.is_empty() {
            return Err("theme text needs a name = line".to_string());
        }
        validate_user_theme_name(&id).map_err(str::to_string)?;
        if builtin_theme(&id).is_some() {
            return Err(format!(
                "{id:?} is a built-in theme name; built-ins always win"
            ));
        }
        let measured = if aterm_render::theme_is_dark(pack(parsed.scheme.background)) {
            Appearance::Dark
        } else {
            Appearance::Light
        };
        let meta = ThemeMeta::admit(&mut parsed, &id, measured);
        let entry = CatalogTheme {
            id,
            theme: parsed,
            meta,
        };
        let at = match self
            .registered
            .iter()
            .position(|t| t.id.eq_ignore_ascii_case(&entry.id))
        {
            Some(at) => {
                self.registered[at] = entry;
                at
            }
            None if self.registered.len() >= MAX_REGISTERED_THEMES => {
                return Err(format!(
                    "already {MAX_REGISTERED_THEMES} registered themes; unregister one first"
                ));
            }
            None => {
                self.registered.push(entry);
                self.registered.len() - 1
            }
        };
        Ok(&self.registered[at])
    }

    /// Whether the installed selector writes `id`.
    pub(crate) fn selector_names(&self, id: &str) -> bool {
        self.selector.as_deref().is_some_and(|raw| {
            ThemeSelector::parse(raw)
                .0
                .named_sides()
                .iter()
                .any(|(_, name)| name.trim().eq_ignore_ascii_case(id.trim()))
        })
    }

    /// Drop a registered theme.
    pub(crate) fn unregister(&mut self, id: &str) -> Result<(), String> {
        if self.selector_names(id) {
            let selector = self.selector.as_deref().unwrap_or_default();
            return Err(format!(
                "{id:?} is drawn by the installed theme {selector:?}"
            ));
        }
        let before = self.registered.len();
        self.registered
            .retain(|t| !t.id.eq_ignore_ascii_case(id.trim()));
        if self.registered.len() == before {
            return Err(format!("no registered theme named {id:?}"));
        }
        Ok(())
    }

    /// `{"unset", "themes": [...]}`: built-ins in catalog order, then the
    /// registered themes in insertion order.
    pub(crate) fn catalog_json(&self) -> String {
        let builtins = builtin_theme_list().into_iter().filter_map(|identity| {
            let theme = CatalogTheme::builtin(identity.name)?;
            Some((theme, identity.signature, "builtin"))
        });
        let registered = self
            .registered
            .iter()
            .map(|t| (t.clone(), false, "registered"));
        let rows: Vec<String> = builtins
            .chain(registered)
            .map(|(t, signature, source)| {
                format!(
                    "{{\"id\":{},\"display_name\":{},\"appearance\":\"{}\",\"appearance_source\":\"{}\",\"pair\":{},\"description\":{},\"signature\":{},\"source\":\"{}\",\"warnings\":{}}}",
                    json_string(&t.id),
                    json_string(&t.meta.display_name),
                    appearance_word(t.meta.appearance),
                    source_word(t.meta.appearance_source),
                    json_opt(t.meta.pair.as_deref()),
                    json_opt(t.meta.description.as_deref()),
                    signature,
                    source,
                    json_warnings(&t.meta),
                )
            })
            .collect();
        format!(
            "{{\"unset\":{},\"themes\":[{}]}}",
            json_string(UNSET_THEME),
            rows.join(",")
        )
    }

    /// JSON snapshot `{"selector", "appearance", "id", "side", "reported", "source"}`.
    pub(crate) fn state_json(&self, reported: Appearance) -> String {
        let (id, side, source) = match (&self.installed, self.host_set) {
            (Some(d), _) => (
                json_string(&d.id),
                format!("\"{}\"", appearance_word(d.side)),
                match d.source {
                    WebThemeSource::Builtin => "\"builtin\"",
                    WebThemeSource::Registered => "\"registered\"",
                },
            ),
            (None, true) => ("null".into(), "null".into(), "\"host\""),
            (None, false) => ("null".into(), "null".into(), "null"),
        };
        format!(
            "{{\"selector\":{},\"appearance\":\"{}\",\"id\":{},\"side\":{},\"reported\":\"{}\",\"source\":{}}}",
            json_opt(self.selector.as_deref()),
            appearance_word(self.appearance),
            id,
            side,
            appearance_word(reported),
            source,
        )
    }
}

impl WebTerminal {
    /// Put `draw` on screen.
    pub(crate) fn install_draw(&mut self, draw: WebDraw) {
        let scheme = &draw.scheme;
        let parts = scheme.to_theme_parts();
        let mut base = self.term.color_baseline();
        base.foreground = scheme.foreground;
        base.background = scheme.background;
        base.cursor = Some(unpack(parts.cursor));
        base.selection_background = Some(unpack(parts.selection));
        base.selection_foreground = scheme.selection_fg;
        base.palette = Some(scheme.to_color_palette());
        base.color_scheme = Some(scheme.appearance);
        self.term.rebase_colors(&base);
        for (i, c) in (0u8..).zip(scheme.ansi.iter()) {
            self.band.set_palette_color(i, [c.r, c.g, c.b]);
        }
        self.install_renderer_theme(parts);
        self.install_renderer_selection_fg(scheme.selection_fg.map(pack));
        self.web_theme.installed = Some(draw);
        self.web_theme.host_set = false;
    }

    /// The host's own colours (`set_theme`, 0x00RRGGBB).
    pub(crate) fn install_host_colors(&mut self, parts: aterm_types::scheme::ThemeParts) {
        let mut base = self.term.color_baseline();
        base.foreground = unpack(parts.fg);
        base.background = unpack(parts.bg);
        base.cursor = Some(unpack(parts.cursor));
        base.selection_background = Some(unpack(parts.selection));
        base.color_scheme = Some(self.web_theme.appearance);
        self.term.rebase_colors(&base);
        self.install_renderer_theme(parts);
        self.web_theme.selector = None;
        self.web_theme.installed = None;
        self.web_theme.host_set = true;
    }

    /// Resolve `raw` for the current appearance and install it.
    pub(crate) fn install_selector(&mut self, raw: String) -> Result<(), String> {
        let draw = self.web_theme.resolve(&raw, self.web_theme.appearance)?;
        self.install_draw(draw);
        self.web_theme.selector = Some(raw.trim().to_string());
        Ok(())
    }

    /// The host's appearance changed: a selector picks its side, else the legacy behaviour.
    pub(crate) fn follow_appearance(&mut self, scheme: Appearance) {
        self.web_theme.appearance = scheme;
        match self.web_theme.selector.clone() {
            Some(raw) => {
                let _ = self.install_selector(raw);
            }
            None => self.term.set_color_scheme(scheme),
        }
    }
}

macro_rules! bind {
    ($terminal:ident, $export:ident) => {
        #[cfg_attr(target_arch = "wasm32", $export)]
        impl $terminal {
            /// The themes `set_theme_by_name` can draw, as JSON.
            pub fn theme_catalog(&self) -> String {
                self.web_theme.catalog_json()
            }

            /// What is on screen, as JSON.
            pub fn theme_state(&self) -> String {
                self.web_theme.state_json(self.term.color_scheme())
            }

            /// Register a theme from `.conf` text.
            pub fn register_theme(&mut self, text: &str) -> Result<String, String> {
                let theme = self.web_theme.register(text)?;
                let out = format!(
                    "{{\"id\":{},\"appearance\":\"{}\",\"appearance_source\":\"{}\",\"warnings\":{}}}",
                    json_string(&theme.id),
                    appearance_word(theme.meta.appearance),
                    source_word(theme.meta.appearance_source),
                    json_warnings(&theme.meta),
                );
                let id = theme.id.clone();
                if self.web_theme.selector_names(&id) {
                    if let Some(raw) = self.web_theme.selector.clone() {
                        let _ = self.install_selector(raw);
                    }
                }
                Ok(out)
            }

            /// Drop a registered theme.
            pub fn unregister_theme(&mut self, id: &str) -> Result<(), String> {
                self.web_theme.unregister(id)
            }

            /// Install a theme by name.
            pub fn set_theme_by_name(&mut self, selector: &str) -> Result<String, String> {
                self.install_selector(selector.to_string())?;
                Ok(self.theme_state())
            }

            /// The per-cell minimum contrast the renderer actually floors glyphs to.
            pub fn effective_minimum_contrast(&self) -> f32 {
                self.render_face().effective_minimum_contrast()
            }
        }
    };
}

web_terminal_impl!(bind);

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "theme_api_tests.rs"]
mod tests;
