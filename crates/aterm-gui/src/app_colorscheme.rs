// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The one theme-install path, and the OS appearance that feeds it.

use crate::{App, WindowId};

/// The `scheme_from=` word of a session's `status`.
pub(crate) const fn scheme_from_word(
    source: Option<aterm_core::terminal::ColorSchemeSource>,
) -> &'static str {
    use aterm_core::terminal::ColorSchemeSource;
    match source {
        Some(ColorSchemeSource::Configured) => "theme",
        Some(ColorSchemeSource::Program) => "program",
        None => "-",
    }
}

/// Map a winit window [`Theme`](aterm_window::window::Theme) onto the engine's
/// [`Appearance`](aterm_types::Appearance).
#[must_use]
pub(crate) const fn theme_to_appearance(
    theme: Option<aterm_window::window::Theme>,
) -> aterm_types::Appearance {
    match theme {
        Some(aterm_window::window::Theme::Light) => aterm_types::Appearance::Light,
        Some(aterm_window::window::Theme::Dark) | None => aterm_types::Appearance::Dark,
    }
}

/// Why the colours are being installed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ThemeChange {
    /// `aterm.toml` changed.
    ConfigReload,
    /// A theme file arrived, changed or left.
    CatalogReload,
    /// The OS appearance flipped, which can pick the other side of a split.
    OsFlip,
    /// A rejected renderer rebuild restored the previous render config.
    Rollback,
    /// Transient preview lane (`theme preview|revert`, or its TTL expiry).
    Preview,
}

impl ThemeChange {
    /// Whether this install keeps a preview held by `hold` standing, provided the preview's
    /// selector still resolves ([`App::settle_theme_preview`]).
    pub(crate) fn keeps_preview(self, hold: crate::control_theme::PreviewHold) -> bool {
        match self {
            Self::OsFlip | Self::Rollback | Self::Preview | Self::CatalogReload => true,
            Self::ConfigReload => {
                hold == crate::control_theme::PreviewHold::Settings { keeping: false }
            }
        }
    }

    /// Combine two install triggers into a single reason.
    pub(crate) fn merged(self, other: Option<Self>) -> Self {
        match other {
            None => self,
            Some(Self::ConfigReload) => Self::ConfigReload,
            Some(_) if self == Self::ConfigReload => Self::ConfigReload,
            Some(other) => other,
        }
    }

    /// Whether this install reloads what a selector draws, so a kept preview
    /// must be re-checked against the new config and catalog.
    const fn reloads(self) -> bool {
        matches!(self, Self::ConfigReload | Self::CatalogReload)
    }
}

/// What one [`App::install_resolved_colors`] changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ThemeInstall {
    /// The engine projection (`TerminalConfig`) differed.
    pub(crate) engine_changed: bool,
    /// The renderer [`Theme`](aterm_render::Theme) differed.
    pub(crate) theme_changed: bool,
    /// The authored chrome colours (`accent_color` and its kin) differed.
    pub(crate) chrome_changed: bool,
}

impl App {
    /// The one theme-install path.
    pub(crate) fn install_resolved_colors(&mut self, reason: ThemeChange) -> ThemeInstall {
        self.settle_theme_preview(reason);
        let candidate = self.theme_preview.as_ref().map(|p| p.id.as_str());
        let colors = self.config.resolved_colors_previewing(
            self.os_appearance,
            &self.config_assets.themes,
            candidate,
        );
        self.install_colors(colors, reason)
    }

    /// Install one resolution everywhere it is drawn or reported, in order.
    pub(crate) fn install_colors(
        &mut self,
        colors: crate::resolved_colors::ResolvedColors,
        reason: ThemeChange,
    ) -> ThemeInstall {
        self.settle_theme_preview(reason);
        let tc = std::sync::Arc::new(self.config.applied_terminal_config_from(&colors));
        let engine_changed = self.session_factory.terminal_config.as_ref() != Some(tc.as_ref());
        let reported = colors.reported;
        if engine_changed {
            self.spare_colors_changed();
        }
        let own_applies = self.install_session_fanout(&tc, engine_changed);
        if engine_changed {
            self.session_factory.terminal_config = Some(std::sync::Arc::unwrap_or_clone(tc));
            self.engine_color_fanouts = self.engine_color_fanouts.saturating_add(1);
        }

        let theme = colors.theme();
        let theme_changed = theme != self.theme;
        let chrome_changed = colors.scheme.chrome != self.installed_colors.scheme.chrome;
        let hues_changed = colors.scheme.ansi != self.installed_colors.scheme.ansi;
        let selection_fg = colors.selection_fg().map(crate::app_config::rgb_u32);
        let (id, side) = (colors.id.clone(), colors.side);
        self.installed_colors = colors;
        if theme_changed {
            self.apply_theme_live(theme);
        } else if chrome_changed || hues_changed {
            self.publish_theme_chrome();
            self.rain_dirty = true;
            self.introspect_gpu.invalidate_present();
        }
        if self.render_knobs.selection_fg != selection_fg {
            self.render_knobs.selection_fg = selection_fg;
            self.apply_render_knob(crate::app_config::KnobChange::SelectionFg(selection_fg));
        }

        if engine_changed || theme_changed || own_applies > 0 {
            self.sparkle_dirty = true;
            self.rain_dirty = true;
            for ws in self.windows.values_mut() {
                ws.fx.companion_mut().invalidate_colors();
            }
        }
        if (engine_changed || own_applies > 0) && !theme_changed {
            self.request_redraw_all_windows();
        }
        aterm_log::debug!(
            "theme install ({reason:?}): {id} side={side:?} reported={reported:?} engine_changed={engine_changed} theme_changed={theme_changed} chrome_changed={chrome_changed} session_applies={own_applies}",
        );
        ThemeInstall {
            engine_changed,
            theme_changed,
            chrome_changed,
        }
    }

    /// End the standing preview unless [`ThemeChange::keeps_preview`] holds and its selector still
    /// resolves.
    pub(crate) fn settle_theme_preview(&mut self, reason: ThemeChange) {
        let Some(preview) = self.theme_preview.as_ref() else {
            return;
        };
        let keep = reason.keeps_preview(preview.hold)
            && !(reason.reloads() && self.theme_preview_unavailable(&preview.id).is_some());
        if !keep {
            self.theme_preview = None;
        }
    }

    /// Record the live OS `appearance` and install the resolved colours
    /// ([`Self::install_resolved_colors`]).
    pub(crate) fn sync_app_theme_to_appearance(&mut self, appearance: aterm_types::Appearance) {
        #[cfg(windows)]
        {
            for wid in self.appearance_redraw_targets() {
                if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
                    crate::platform_win::resync_chrome_appearance(w);
                }
            }
        }

        if self.os_appearance == appearance {
            return;
        }
        self.os_appearance = appearance;

        if self.request_theme_typography_refresh(ThemeChange::OsFlip) {
            return;
        }
        let installed = self.install_resolved_colors(ThemeChange::OsFlip);
        if !installed.theme_changed {
            for wid in self.appearance_redraw_targets() {
                if let Some(ws) = self.windows.get(&wid) {
                    ws.request_redraw();
                }
            }
        }
    }

    /// Handle an OS appearance reading ([`crate::Wake::OsAppearance`]) or the `appearance` verb.
    pub(crate) fn on_os_appearance(&mut self, appearance: aterm_types::Appearance) {
        self.sync_app_theme_to_appearance(appearance);
    }

    /// The `theme_side=` word.
    pub(crate) fn theme_side_word(&self) -> &'static str {
        use aterm_types::scheme::ThemeSelector;
        let (selector, _) = ThemeSelector::parse(self.config.theme.as_deref().unwrap_or(""));
        let follows = matches!(selector, ThemeSelector::Split { .. })
            && selector.name_for(aterm_types::Appearance::Dark).is_some();
        if follows {
            crate::os_appearance::appearance_word(self.installed_colors.side)
        } else {
            "-"
        }
    }

    /// Format the appearance status fields (`os_appearance= theme_side= reported_scheme=
    /// [scheme_from=] bg_dark=`).
    pub(crate) fn appearance_status_fields(
        &self,
        reported: Option<aterm_types::Appearance>,
        scheme_from: Option<&str>,
        bg: Option<aterm_types::Rgb>,
    ) -> String {
        let reported = reported.map_or("-", crate::os_appearance::appearance_word);
        let scheme_from = scheme_from.map_or_else(String::new, |w| format!(" scheme_from={w}"));
        let bg_dark = bg.map_or("-", |bg| {
            if aterm_messages::ink::bg_is_light([bg.r, bg.g, bg.b]) {
                "0"
            } else {
                "1"
            }
        });
        format!(
            "os_appearance={} theme_side={} reported_scheme={reported}{scheme_from} bg_dark={bg_dark}",
            crate::os_appearance::appearance_word(self.os_appearance),
            self.theme_side_word(),
        )
    }

    /// The `appearance` verb's line.
    pub(crate) fn appearance_status_line(&self) -> String {
        let fields = self.appearance_status_fields(
            Some(self.installed_colors.reported),
            None,
            Some(self.installed_colors.scheme.background),
        );
        let mut fields = fields.splitn(2, ' ');
        let os = fields.next().unwrap_or_default();
        let rest = fields.next().unwrap_or_default();
        format!(
            "{os} source={} {rest} theme={} installs={} session_installs={}",
            self.os_appearance_source.wire_name(),
            aterm_control::wire::pct_encode(&self.installed_colors.id),
            self.engine_color_fanouts,
            self.session_color_installs,
        )
    }

    /// The windows an OS appearance flip nudges when the rendered theme is unchanged.
    pub(crate) fn appearance_redraw_targets(&self) -> Vec<WindowId> {
        self.windows.keys().copied().collect()
    }
}

#[cfg(test)]
#[path = "app_colorscheme_tests.rs"]
mod tests;

/// The one install path.
#[cfg(test)]
#[path = "app_colorscheme_install_resolved_colors_tests.rs"]
pub(crate) mod install_resolved_colors;

/// Tests for OS appearance source decoupling, launch appearance seeding, and status fields.
#[cfg(test)]
#[path = "app_colorscheme_appearance_source_tests.rs"]
mod appearance_source;
