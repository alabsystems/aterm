// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

//! Named themes for both web terminals: the built-in catalog, `dark:A,light:B`
//! splits that follow `set_color_scheme`, and themes a host registers from the
//! same `.conf` text a native themes folder holds.
//!
//! What a selector draws is [`ThemeSelector::draw_builtin`], the one draw
//! aterm-gui resolves through too, so native and web cannot disagree about it.
//! Registered text is admitted the way a native theme file is
//! ([`parse_theme_str`] under [`MAX_USER_THEME_FILE_BYTES`], then
//! [`ThemeMeta::admit`] with the renderer's own dark/light classifier), and an
//! install moves the engine's colour baseline with
//! [`Terminal::rebase_colors`](aterm_core::terminal::Terminal::rebase_colors),
//! so a program's OSC 4/10/11/12/17/19 colours survive a theme switch exactly
//! as they do natively. Unlike the native config path, which falls back to
//! Default with a notice, an API call that names something it cannot draw
//! fails loudly and changes nothing.
//!
//! Shared with `aterm-gpu-web`, which mounts this file by `#[path]`; names
//! resolve against the crate root of whichever crate compiles it. Each crate
//! supplies `render_face`, `install_renderer_theme` and
//! `install_renderer_selection_fg` in its `lib.rs`.

// The methods read the crate root's items, which differ per crate.
use super::*;

use aterm_types::scheme::{
    builtin_theme, builtin_theme_list, parse_theme_str, validate_user_theme_name, AppearanceSource,
    CatalogTheme, ColorScheme, SelectorDraw, ThemeMeta, ThemeSelector, MAX_USER_THEME_FILES,
    MAX_USER_THEME_FILE_BYTES, UNSET_THEME,
};
use aterm_types::Appearance;

/// How many themes a host may register: the native catalog's
/// `MAX_USER_THEME_FILES`.
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

/// The web terminal's theme state. Holds no font or terminal, so its rules are
/// unit-testable on their own.
#[derive(Debug)]
pub(crate) struct WebThemeState {
    /// The selector `set_theme_by_name` last installed; `None` while the host
    /// drives the colours (`set_theme`) or nothing was installed.
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

fn appearance_word(a: Appearance) -> &'static str {
    match a {
        Appearance::Dark => "dark",
        Appearance::Light => "light",
    }
}

fn source_word(source: AppearanceSource) -> &'static str {
    match source {
        AppearanceSource::Declared => "declared",
        AppearanceSource::Measured => "measured",
    }
}

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
pub(crate) fn pack(c: Rgb) -> u32 {
    (u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b)
}

/// Unpack `0x00RRGGBB` into an engine colour.
pub(crate) fn unpack(c: u32) -> Rgb {
    let [_, r, g, b] = c.to_be_bytes();
    Rgb { r, g, b }
}

impl WebThemeState {
    pub(crate) fn new(appearance: Appearance) -> Self {
        Self {
            selector: None,
            appearance,
            registered: Vec::new(),
            installed: None,
            host_set: false,
        }
    }

    fn find_registered(&self, name: &str) -> Option<&CatalogTheme> {
        self.registered
            .iter()
            .find(|t| t.id.eq_ignore_ascii_case(name.trim()))
    }

    /// Resolve the side of `raw` that `appearance` draws, without mutating.
    /// `Err` (nothing changes) when the selector has any issue (blank, `dark:`,
    /// an unknown mode, a junk segment, a repeated side), or when any name it
    /// writes is neither a built-in nor registered.
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
            // Blank is an issue above, so `Unset` cannot reach here; drawing
            // the unset default would still be the right answer.
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
    /// A theme already registered under that name (case-insensitively) is
    /// replaced in place.
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

    /// Drop a registered theme. Refused while the installed selector names it.
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

    /// `{"selector", "appearance", "id", "side", "reported", "source"}`, with
    /// `reported` the scheme DSR 996 answers now.
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
    /// Put `draw` on screen: the engine baseline moves (program colours ride
    /// on top), the band and renderer take the theme, and the reported scheme
    /// becomes the drawn theme's appearance, reconciled once after the colours
    /// (a `CSI ? 997` push only when it moved under DEC 2031).
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
        // The band borrows slots 1, 3, 4 and 6 (its error, warn and meter hues).
        for (i, c) in (0u8..).zip(scheme.ansi.iter()) {
            self.band.set_palette_color(i, [c.r, c.g, c.b]);
        }
        self.install_renderer_theme(parts);
        self.install_renderer_selection_fg(scheme.selection_fg.map(pack));
        self.web_theme.installed = Some(draw);
        self.web_theme.host_set = false;
    }

    /// The host's own colours (`set_theme`, 0x00RRGGBB): the engine baseline
    /// moves (a program's OSC 10/11/12/17 colours survive), the renderer
    /// takes them, and the host owns the theme again. The reported scheme's
    /// baseline returns to the host's last declared appearance
    /// (`set_color_scheme`), not a named theme's it replaces, in the same
    /// rebase, so DSR 996 and a DEC 2031 push describe the host's theme.
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

    /// Resolve `raw` for the current appearance and install it. On `Err`
    /// nothing changes.
    pub(crate) fn install_selector(&mut self, raw: String) -> Result<(), String> {
        let draw = self.web_theme.resolve(&raw, self.web_theme.appearance)?;
        self.install_draw(draw);
        self.web_theme.selector = Some(raw.trim().to_string());
        Ok(())
    }

    /// The host's appearance changed: a selector picks its side, else the
    /// legacy behaviour (the host owns the colours, only the report moves).
    pub(crate) fn follow_appearance(&mut self, scheme: Appearance) {
        self.web_theme.appearance = scheme;
        match self.web_theme.selector.clone() {
            // The selector resolved when it was installed and every name it
            // writes is still known (unregister refuses them), so this succeeds.
            Some(raw) => {
                let _ = self.install_selector(raw);
            }
            None => self.term.set_color_scheme(scheme),
        }
    }
}

/// The JS-visible theme surface. Each crate expands it onto its own terminal
/// type with `web_terminal_impl!`, as `facade_api` does.
macro_rules! bind {
    ($terminal:ident, $export:ident) => {
        #[cfg_attr(target_arch = "wasm32", $export)]
        impl $terminal {
            /// The themes `set_theme_by_name` can draw, as JSON:
            /// `{"unset": "Default", "themes": [{"id",
            /// "display_name", "appearance": "dark"|"light", "appearance_source":
            /// "declared"|"measured", "pair", "description", "signature", "source":
            /// "builtin"|"registered", "warnings": [..]}]}`, built-ins first.
            pub fn theme_catalog(&self) -> String {
                self.web_theme.catalog_json()
            }

            /// What is on screen, as JSON: `{"selector", "appearance", "id",
            /// "side", "reported", "source": "builtin"|"registered"|"host"|null}`.
            /// `reported` is the scheme DSR 996 answers (a program's own OSC 11
            /// background reports its own polarity).
            pub fn theme_state(&self) -> String {
                self.web_theme.state_json(self.term.color_scheme())
            }

            /// Register a theme from `.conf` text (the format of a native themes
            /// folder; `contrib/themes/TEMPLATE.conf`), under its `name =`. At most
            /// 256 KiB and 128 themes; a built-in's name is refused (built-ins
            /// always win); registering a name again replaces it, and re-installs
            /// it when the current theme draws it. Returns `{"id", "appearance",
            /// "appearance_source", "warnings"}`.
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
                        // The theme was just registered, so the selector resolves.
                        let _ = self.install_selector(raw);
                    }
                }
                Ok(out)
            }

            /// Drop a registered theme. Refused while the installed theme names it.
            pub fn unregister_theme(&mut self, id: &str) -> Result<(), String> {
                self.web_theme.unregister(id)
            }

            /// Install a theme by name: a built-in (case-insensitive), a registered
            /// theme, or `dark:A,light:B`, whose side follows `set_color_scheme`. An
            /// omitted side draws the named side's built-in pair (`dark:Gruvbox
            /// Dark` is Gruvbox Light in light mode), else the named side. On error
            /// nothing changes. Program-set colours (OSC 4/10/11/12/17/19) survive,
            /// as on native. Host `set_palette_color` overrides are replaced by the
            /// theme's slots 0-15 (re-apply them after); slots 16-255 return to the
            /// xterm table. Returns `theme_state()`.
            pub fn set_theme_by_name(&mut self, selector: &str) -> Result<String, String> {
                self.install_selector(selector.to_string())?;
                Ok(self.theme_state())
            }

            /// The per-cell minimum contrast the renderer actually floors glyphs
            /// to: the configured ratio, raised to WCAG AA (4.5:1) while
            /// `set_background_opacity` is below 1, so a host can show what glass
            /// forces.
            pub fn effective_minimum_contrast(&self) -> f32 {
                self.render_face().effective_minimum_contrast()
            }
        }
    };
}

web_terminal_impl!(bind);

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    const MINE: &str = "name = Mine\nbackground = #101820\nforeground = #e0e0e0\n";

    /// A complete minimal theme text (foreground and background are required).
    fn conf(name: &str, bg: &str) -> String {
        format!("name = {name}\nbackground = {bg}\nforeground = #e0e0e0\n")
    }

    #[test]
    fn resolve_is_atomic_and_loud() {
        let state = WebThemeState::new(Appearance::Dark);
        let nord = state.resolve("Nord", Appearance::Dark).expect("built-in");
        assert_eq!(
            (nord.id.as_str(), nord.source),
            ("Nord", WebThemeSource::Builtin)
        );
        let paired = state
            .resolve("dark:Gruvbox Dark", Appearance::Light)
            .expect("an omitted side draws the pair");
        assert_eq!(paired.id, "Gruvbox Light");
        assert_eq!(paired.side, Appearance::Light);
        assert_eq!(
            state
                .resolve("nord", Appearance::Light)
                .expect("any case")
                .id,
            "Nord"
        );
        for bad in [
            "",
            "  ",
            "dark:",
            "dark:Nord,dusk:X",
            "Nope",
            "dark:Nord,light:Nope",
        ] {
            assert!(state.resolve(bad, Appearance::Dark).is_err(), "{bad:?}");
        }
        // Negative control: the permissive native reading draws something for
        // the half-bad split, where the API refuses it.
        let (native, _) = ThemeSelector::parse("dark:Nord,light:Nope");
        assert_eq!(native.name_for(Appearance::Dark), Some("Nord"));
    }

    #[test]
    fn register_bounds_and_shadowing() {
        let mut state = WebThemeState::new(Appearance::Dark);
        let mine = state.register(MINE).expect("a minimal theme");
        assert_eq!(mine.id, "Mine");
        assert_eq!(mine.meta.appearance, Appearance::Dark);
        assert_eq!(mine.meta.appearance_source, AppearanceSource::Measured);
        state
            .register(&conf("mine", "#202020"))
            .expect("re-register");
        assert_eq!(state.registered.len(), 1, "replaced, not added");
        assert_eq!(state.registered[0].id, "mine");

        let err = state
            .register(&conf("nord", "#000000"))
            .expect_err("built-in");
        assert!(err.contains("built-in"), "{err}");
        let err = state
            .register(&conf("dark:x", "#000000"))
            .expect_err("not an id");
        assert!(!err.contains("built-in"), "{err}");
        let err = state
            .register("background = #000000\nforeground = #ffffff\n")
            .expect_err("no name");
        assert!(err.contains("name"), "{err}");
        // Non-vacuity: the same text with a free name registers.
        state
            .register(&conf("Mine Too", "#000000"))
            .expect("a free name");
        // Over the cap is refused before parsing (the text is not a theme).
        let huge = "x".repeat(MAX_USER_THEME_FILE_BYTES + 1);
        let err = state.register(&huge).expect_err("too large");
        assert!(err.contains("256 KiB"), "{err}");

        let light = state.register(&conf("Paper", "#F5F5F5")).expect("light");
        assert_eq!(light.meta.appearance, Appearance::Light);
        assert_eq!(light.meta.appearance_source, AppearanceSource::Measured);

        let mut full = WebThemeState::new(Appearance::Dark);
        for i in 0..MAX_REGISTERED_THEMES {
            full.register(&conf(&format!("T{i}"), "#000000"))
                .unwrap_or_else(|e| panic!("theme {i}: {e}"));
        }
        assert_eq!(
            full.registered.len(),
            MAX_REGISTERED_THEMES,
            "the cap is reachable"
        );
        assert!(
            full.register(&conf("One More", "#000000")).is_err(),
            "past the cap"
        );
        full.register(&conf("t0", "#111111"))
            .expect("replacing at the cap is fine");
    }

    #[test]
    fn catalog_json_lists_builtins_then_registered() {
        let mut state = WebThemeState::new(Appearance::Dark);
        state.register(MINE).expect("registered");
        let json = state.catalog_json();
        let ids: Vec<&str> = json
            .split("{\"id\":\"")
            .skip(1)
            .map(|row| row.split('"').next().unwrap_or_default())
            .collect();
        let builtins = builtin_theme_list();
        assert_eq!(ids.len(), builtins.len() + 1);
        assert_eq!(ids.first(), Some(&"Default"));
        for (id, b) in ids.iter().zip(&builtins) {
            assert_eq!(*id, b.name);
        }
        assert_eq!(ids.last(), Some(&"Mine"));
        assert!(json.starts_with("{\"unset\":\"Default\","));
        assert!(json.contains(
            "\"id\":\"Mine\",\"display_name\":\"Mine\",\"appearance\":\"dark\",\"appearance_source\":\"measured\""
        ));
        assert!(json.contains(
            "\"id\":\"Default Light\",\"display_name\":\"Default Light\",\"appearance\":\"light\""
        ));
    }

    #[test]
    fn unregister_refuses_the_drawn_theme() {
        let mut state = WebThemeState::new(Appearance::Dark);
        state.register(MINE).expect("registered");
        state.selector = Some("dark:Mine,light:Hearth".into());
        assert!(state.unregister("mine").is_err(), "drawn by the selector");
        state.selector = Some("Nord".into());
        state.unregister("mine").expect("no longer drawn");
        assert!(state.unregister("Mine").is_err(), "already gone");
        assert!(state.resolve("Mine", Appearance::Dark).is_err());
    }

    const FACE: &[u8] = include_bytes!("../../aterm-render/assets/DejaVuSansMono.ttf");

    fn terminal() -> WebTerminal {
        WebTerminal::new(
            4,
            20,
            FACE,
            14.0,
            0x00d0_d0d0,
            0x0010_1010,
            0x00ff_ffff,
            0x0040_4040,
        )
        .expect("the bundled face parses")
    }

    fn assert_draws(t: &WebTerminal, scheme: &ColorScheme, at: &str) {
        let parts = scheme.to_theme_parts();
        assert_eq!(
            t.render_face().theme(),
            aterm_render::Theme::from(parts),
            "{at}"
        );
        let base = t.term.color_baseline();
        assert_eq!(base.foreground, scheme.foreground, "{at}");
        assert_eq!(t.term.default_background(), scheme.background, "{at}");
        assert_eq!(t.term.default_foreground(), scheme.foreground, "{at}");
        assert_eq!(base.cursor, Some(unpack(parts.cursor)), "{at}");
        assert_eq!(
            base.selection_background,
            Some(unpack(parts.selection)),
            "{at}"
        );
        for i in 0..16u8 {
            assert_eq!(
                t.term.palette_color(i),
                scheme.ansi[usize::from(i)],
                "{at} slot {i}"
            );
        }
        // Slots 16-255: xterm's table, or what the theme derives from its own
        // colours (`palette_generate`), as native draws them.
        let palette = scheme.to_color_palette();
        for i in [16u8, 100, 231, 255] {
            assert_eq!(t.term.palette_color(i), palette.get(i), "{at} slot {i}");
        }
        assert_eq!(t.term.color_scheme(), scheme.appearance, "{at}");
    }

    #[test]
    fn set_theme_by_name_matches_the_shared_projection_for_every_builtin() {
        let mut t = terminal();
        for name in aterm_types::scheme::builtin_names() {
            let scheme = aterm_types::scheme::builtin(name).expect("a built-in");
            for dark in [true, false] {
                t.set_color_scheme(dark);
                t.set_theme_by_name(name).expect("a built-in installs");
                assert_draws(&t, &scheme, &format!("{name} dark={dark}"));
            }
        }
        // Both kinds are drawn above: a derived extended palette (Aterm Night)
        // and xterm's table (Default), so the slot checks compare each.
        let derived = aterm_types::scheme::builtin("Aterm Night").expect("a built-in");
        assert!(derived.palette_generate);
        assert_ne!(
            derived.to_color_palette().get(16),
            aterm_core::terminal::ColorPalette::default_color(16)
        );
        let xterm = aterm_types::scheme::builtin("Default").expect("a built-in");
        assert!(!xterm.palette_generate);
        assert_eq!(
            xterm.to_color_palette().get(16),
            aterm_core::terminal::ColorPalette::default_color(16)
        );
    }

    #[test]
    fn split_follows_set_color_scheme() {
        let mut t = terminal();
        t.process(b"\x1b[?2031h");
        while t.term.take_response().is_some() {}
        t.set_color_scheme(true);
        t.set_theme_by_name("dark:Aterm Night,light:Aterm Day")
            .expect("signature split");
        assert_eq!(
            t.web_theme.installed.as_ref().map(|d| d.id.as_str()),
            Some("Aterm Night")
        );
        while t.term.take_response().is_some() {}
        t.set_color_scheme(false);
        let day = aterm_types::scheme::builtin("Aterm Day").expect("built-in");
        assert_draws(&t, &day, "Aterm Day on light");
        assert_eq!(
            t.term.take_response().as_deref(),
            Some(&b"\x1b[?997;2n"[..])
        );
        let state = t.theme_state();
        assert!(state.contains("\"id\":\"Aterm Day\",\"side\":\"light\",\"reported\":\"light\",\"source\":\"builtin\""), "{state}");

        // A plain theme draws itself on both sides and its report never moves.
        t.set_theme_by_name("Nord").expect("plain");
        while t.term.take_response().is_some() {}
        let nord = aterm_types::scheme::builtin("Nord").expect("built-in");
        for dark in [true, false] {
            t.set_color_scheme(dark);
            assert_draws(&t, &nord, "Nord");
            assert_eq!(
                t.term.take_response(),
                None,
                "no 997 push for a plain theme"
            );
        }

        // Negative control: after the host's raw set_theme, an appearance flip
        // leaves the renderer theme alone.
        t.set_theme(0x0011_2233, 0x00ee_eeee, 0x0000_0000, 0x0080_8080);
        let host = t.render_face().theme();
        t.set_color_scheme(true);
        assert_eq!(t.render_face().theme(), host);
        assert!(t.theme_state().contains("\"source\":\"host\""));
    }

    #[test]
    fn named_install_keeps_program_colours() {
        let linen = aterm_types::scheme::builtin("Linen").expect("built-in");
        let program = Rgb {
            r: 0x10,
            g: 0x11,
            b: 0x12,
        };
        let mut t = terminal();
        t.process(b"\x1b]11;rgb:10/11/12\x07");
        t.set_theme_by_name("Linen").expect("installs");
        assert_eq!(t.term.default_background(), program, "OSC 11 survives");
        assert_eq!(t.term.default_foreground(), linen.foreground, "fg follows");
        t.process(b"\x1b]111\x07");
        assert_eq!(
            t.term.default_background(),
            linen.background,
            "OSC 111 → Linen"
        );

        // Negative control: the pre-rebase path (live = baseline) loses it.
        let mut legacy = terminal();
        legacy.process(b"\x1b]11;rgb:10/11/12\x07");
        legacy.term.set_default_background(linen.background);
        assert_ne!(legacy.term.default_background(), program);
    }

    /// After a named light theme, the host's raw dark `set_theme` hands the
    /// report back to the host's declared appearance: DSR 996 answers dark.
    /// Negative control: under the named theme it answered light.
    #[test]
    fn set_theme_after_a_named_theme_reports_the_host_appearance() {
        let mut t = terminal();
        t.set_color_scheme(true);
        t.set_theme_by_name("Linen").expect("installs");
        let _ = t.term.take_response();
        t.process(b"\x1b[?996n");
        assert_eq!(
            t.term.take_response().as_deref(),
            Some(&b"\x1b[?997;2n"[..]),
            "NEG: the named light theme reports light"
        );
        t.set_theme(0x00d0_d0d0, 0x0010_1010, 0x00ff_ffff, 0x0040_4040);
        let _ = t.term.take_response();
        t.process(b"\x1b[?996n");
        assert_eq!(
            t.term.take_response().as_deref(),
            Some(&b"\x1b[?997;1n"[..]),
            "the host's dark appearance"
        );
    }

    #[test]
    fn set_theme_keeps_program_colours() {
        let program = Rgb {
            r: 0x10,
            g: 0x11,
            b: 0x12,
        };
        let mut t = terminal();
        t.process(b"\x1b]11;rgb:10/11/12\x07");
        t.set_theme(0x00ee_eeee, 0x00fa_fafa, 0x0000_0000, 0x00c0_c0c0);
        assert_eq!(
            t.term.default_background(),
            program,
            "OSC 11 survives set_theme"
        );
        assert_eq!(t.term.default_foreground(), unpack(0x00ee_eeee));
        t.process(b"\x1b]111\x07");
        assert_eq!(
            t.term.default_background(),
            unpack(0x00fa_fafa),
            "OSC 111 → host bg"
        );
    }

    #[test]
    fn web_translucency_raises_the_effective_floor() {
        let mut t = terminal();
        t.set_background_opacity(0.6);
        assert_eq!(t.effective_minimum_contrast(), 4.5);
        t.set_minimum_contrast(7.0);
        assert_eq!(
            t.effective_minimum_contrast(),
            7.0,
            "a stronger floor stays"
        );
        t.set_minimum_contrast(1.0);
        t.set_background_opacity(1.0);
        assert_eq!(t.effective_minimum_contrast(), 1.0, "opaque: no raise");
    }

    #[test]
    fn register_then_select_follows_a_re_registration() {
        let mut t = terminal();
        let out = t.register_theme(MINE).expect("registers");
        assert!(
            out.starts_with(
                "{\"id\":\"Mine\",\"appearance\":\"dark\",\"appearance_source\":\"measured\""
            ),
            "{out}"
        );
        t.set_theme_by_name("mine").expect("selects any case");
        assert_eq!(t.render_face().theme().bg, 0x0010_1820);
        assert!(t.unregister_theme("Mine").is_err(), "drawn");
        t.register_theme(&conf("Mine", "#203040"))
            .expect("re-registers");
        assert_eq!(
            t.render_face().theme().bg,
            0x0020_3040,
            "the drawn theme re-installs"
        );
        assert!(t.set_theme_by_name("Nope").is_err());
        assert_eq!(
            t.render_face().theme().bg,
            0x0020_3040,
            "an error changes nothing"
        );
    }

    #[test]
    fn theme_installs_reopen_the_frame_gate() {
        let mut t = terminal();
        t.register_theme(MINE).expect("registers");
        t.set_theme_by_name("Mine").expect("installs");
        t.settle_frame();
        assert!(!t.needs_frame(), "settled");
        t.set_theme_by_name("Nord").expect("installs");
        assert!(t.needs_frame(), "a named install repaints");
        t.settle_frame();
        t.set_theme_by_name("dark:Mine,light:Hearth")
            .expect("installs");
        t.settle_frame();
        t.register_theme(&conf("Mine", "#304050"))
            .expect("re-registers");
        assert!(t.needs_frame(), "re-registering the drawn theme repaints");
        // Negative control: registering a theme nothing draws does not.
        t.settle_frame();
        t.register_theme(&conf("Other", "#000000"))
            .expect("registers");
        assert!(!t.needs_frame());
    }
}
