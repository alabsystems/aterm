// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The one theme-install path, and the OS appearance that feeds it.
//!
//! [`App::install_resolved_colors`] is how every theme change lands: a config
//! reload, a theme-catalog reload, an OS light/dark flip and a rejected-rebuild
//! rollback all resolve the colours once ([`Config::resolved_colors`]) and
//! install them with the same steps and the same invalidations. Callers differ
//! only in the [`ThemeChange`] they name.
//!
//! What the engine REPORTS to programs (DEC private mode 2031 and DSR
//! `CSI ? 996 n` → `CSI ? 997 ; Ps n`) is the appearance of the palette on
//! screen, [`ResolvedColors::reported`]: `theme = "GitHub Light"` on a dark
//! desktop reports light. The OS appearance only picks the side of a
//! `dark:…,light:…` split. When mode 2031 is set and the reported scheme
//! CHANGES, `set_color_scheme` queues an unsolicited `CSI ? 997 ; Ps n`; it is
//! drained in the same critical section that applied the new palette and sent by
//! the route every UI-thread report takes (`app_input::write_ui_report`: queued
//! behind earlier input, never a write that can park the event loop), so a
//! program that re-queries OSC 10/11 on the push reads the new colours.
//!
//! The OS appearance comes from [`crate::os_appearance`], a source independent
//! of any window's chrome: macOS observes the application's
//! `effectiveAppearance` (a forced `window_theme` no longer masks a flip), Linux
//! reads the XDG desktop portal or gsettings, and Windows keeps winit's window
//! theme ([`theme_to_appearance`] maps it). It is read before the first session
//! is built, and every later reading arrives as [`App::on_os_appearance`].
//!
//! [`Config::resolved_colors`]: crate::app_config::Config::resolved_colors
//! [`ResolvedColors::reported`]: crate::resolved_colors::ResolvedColors::reported

use crate::{App, WindowId, term_lock};

/// Map a winit window [`Theme`](winit::window::Theme) onto the engine's
/// [`Appearance`](aterm_types::Appearance).
///
/// `Some(Light)`/`Some(Dark)` map across directly. `None` — winit could not
/// determine the OS appearance (some platforms / no theme support) — falls back to
/// [`Appearance::Dark`], which is the engine's OWN default, so an "unknown" OS leaves
/// the engine at the value it already held (no spurious change/push).
#[must_use]
pub(crate) fn theme_to_appearance(theme: Option<winit::window::Theme>) -> aterm_types::Appearance {
    match theme {
        Some(winit::window::Theme::Light) => aterm_types::Appearance::Light,
        // Dark, or an indeterminate OS appearance, both resolve to the engine default.
        Some(winit::window::Theme::Dark) | None => aterm_types::Appearance::Dark,
    }
}

/// Why the colours are being installed. Every caller takes the same path
/// ([`App::install_resolved_colors`]); the reason only names the trigger in the
/// log line.
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
    /// The transient preview lane (`theme preview|revert`, its TTL): the
    /// preview's candidate, or the configured theme once it ends.
    Preview,
}

impl ThemeChange {
    /// Whether this install keeps a preview held by `hold` standing, provided
    /// the preview's selector still draws (a reload re-checks that,
    /// [`App::settle_theme_preview`]).
    ///
    /// - An OS flip, a rollback and the preview lane itself keep every
    ///   preview: its other side, its colours under the restored render
    ///   config.
    /// - A theme-catalog reload keeps every preview, so saving the file of a
    ///   previewed theme redraws it, and a choice or a Keep in flight does not
    ///   flash the old theme back before its write lands.
    /// - A config reload ends every preview but a Settings trial nobody has
    ///   kept: the trial is still what the page shows, so the reload resolves
    ///   once with it instead of installing the config and then the trial
    ///   again. The reload a Keep write causes ends that trial, as the reload
    ///   a `theme commit` or a palette choice causes ends theirs.
    pub(crate) fn keeps_preview(self, hold: crate::control_theme::PreviewHold) -> bool {
        match self {
            Self::OsFlip | Self::Rollback | Self::Preview | Self::CatalogReload => true,
            Self::ConfigReload => {
                hold == crate::control_theme::PreviewHold::Settings { keeping: false }
            }
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
    /// The engine projection (`TerminalConfig`) differed, so every session and
    /// the factory got `apply_config`.
    pub(crate) engine_changed: bool,
    /// The renderer [`Theme`](aterm_render::Theme) differed, so it went
    /// through [`App::apply_theme_live`].
    pub(crate) theme_changed: bool,
}

impl App {
    /// The one theme-install path. Resolves the colours once for the live OS
    /// appearance and the admitted catalog, then installs them with
    /// [`Self::install_colors`].
    ///
    /// A standing preview ([`App::theme_preview`](crate::App)) stands in for
    /// the configured `theme` unless `reason` ends it
    /// ([`Self::settle_theme_preview`]).
    pub(crate) fn install_resolved_colors(&mut self, reason: ThemeChange) -> ThemeInstall {
        self.settle_theme_preview(reason);
        let candidate = self.theme_preview.as_ref().map(|p| p.id.as_str());
        let colors =
            self.config
                .resolved_colors(self.os_appearance, &self.config_assets.themes, candidate);
        self.install_colors(colors, reason)
    }

    /// Install one resolution everywhere it is drawn or reported, in order:
    ///
    /// 1. the engine projection, onto every session and the session factory,
    ///    skipped when it equals the installed one (an unrelated edit or an
    ///    unused theme file costs no `apply_config`);
    /// 2. in the SAME per-session critical section, the reported scheme
    ///    ([`ResolvedColors::reported`](crate::resolved_colors::ResolvedColors)):
    ///    DSR 996 / mode 2031 describe the palette on screen, not the OS. The
    ///    `CSI ? 997 ; Ps n` push it queues is drained only after the palette
    ///    is in, so a program that answers it with an OSC 10/11 query reads
    ///    the new colours;
    /// 3. the renderer theme, through [`Self::apply_theme_live`] when it
    ///    changed, and the selected-text foreground knob;
    /// 4. every colour-derived cache, invalidated here and nowhere else.
    ///
    /// `colors` must be resolved from `self.config`, `self.os_appearance` and
    /// `self.config_assets.themes`; callers that already hold that resolution
    /// pass it instead of resolving twice, after settling the preview for
    /// `reason` ([`Self::settle_theme_preview`]), which runs here again and
    /// then changes nothing.
    pub(crate) fn install_colors(
        &mut self,
        colors: crate::resolved_colors::ResolvedColors,
        reason: ThemeChange,
    ) -> ThemeInstall {
        self.settle_theme_preview(reason);
        let tc = self.config.applied_terminal_config_from(&colors);
        let engine_changed = self.session_factory.terminal_config.as_ref() != Some(&tc);
        let reported = colors.reported;
        for session in self.pool.iter() {
            let push = {
                let mut term = term_lock(&session.term);
                if engine_changed {
                    term.apply_config(&tc);
                }
                let scheme_changed = term.color_scheme() != reported;
                term.set_color_scheme(reported);
                // The unsolicited `CSI ? 997 ; Ps n` is queued only when the
                // scheme moved AND the program enabled mode 2031; take nothing
                // otherwise, so a pending reply the reader owns stays put.
                if scheme_changed {
                    term.take_response()
                } else {
                    None
                }
            };
            if let Some(push) = push {
                // The route every report takes (`write_ui_report`): the UI thread
                // only enqueues it behind earlier input, or writes it through the
                // egress that never parks — never the blocking write, which would
                // park this thread (and every window) on a program that has
                // stopped reading, at the spill cap or on a parked paste's fd
                // lock (2026-09-26). A refusal (a full queue) or a closed PTY
                // just drops the report: it is advisory.
                crate::app_input::write_ui_report(
                    &session.term,
                    &session.ctx.modes,
                    &session.ctx.sink,
                    &crate::input::InputEvent::KeySequence(push),
                );
            }
        }
        if engine_changed {
            self.session_factory.terminal_config = Some(tc);
            self.engine_color_fanouts = self.engine_color_fanouts.saturating_add(1);
        }
        // A tab or split spawned from here on starts at the same reported
        // scheme (`spawn::new_live_terminal`).
        self.session_factory.appearance = reported;

        let theme = colors.theme();
        let theme_changed = theme != self.theme;
        if theme_changed {
            self.apply_theme_live(theme);
        }
        // A theme file's `selection_foreground` reaches the renderer knob too,
        // not only the engine.
        let selection_fg = colors.selection_fg().map(crate::app_config::rgb_u32);
        if self.render_knobs.selection_fg != selection_fg {
            self.render_knobs.selection_fg = selection_fg;
            self.apply_render_knob(crate::app_config::KnobChange::SelectionFg(selection_fg));
        }

        if engine_changed || theme_changed {
            // Sparkle and rain read the palette; a resident pet keeps its place
            // but retires its contrast sample.
            self.sparkle_dirty = true;
            self.rain_dirty = true;
            for ws in self.windows.values_mut() {
                ws.fx.companion_mut().invalidate_colors();
            }
        }
        if engine_changed && !theme_changed {
            // Cells re-coloured under an unchanged chrome: each window's repaint
            // key keeps an untouched presentation cheap.
            self.request_redraw_all_windows();
        }
        aterm_log::debug!(
            "theme install ({reason:?}): {} side={:?} reported={reported:?} engine_changed={engine_changed} theme_changed={theme_changed}",
            colors.id,
            colors.side,
        );
        self.installed_colors = colors;
        ThemeInstall {
            engine_changed,
            theme_changed,
        }
    }

    /// End the standing preview if `reason` does not keep it
    /// ([`ThemeChange::keeps_preview`]), or if `reason` is a reload and the
    /// preview's selector no longer draws under the config and catalog now
    /// held (its file left, say). Idempotent; installs nothing.
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

    /// Record the live OS `appearance` and install the colours it draws: the
    /// OS-flip caller of [`Self::install_resolved_colors`].
    ///
    /// The appearance picks the side of a `dark:…,light:…` split `theme`; the
    /// reported scheme follows the palette that side draws, so a plain theme
    /// reports the same thing in light and dark mode. A NO-OP when the
    /// appearance is unchanged. Re-resolves from the retained live
    /// [`Config`](crate::Config) and the admitted theme catalog (no disk read),
    /// and a changed chrome goes through the colour-only
    /// [`Self::apply_theme_live`]: the backend is never rebuilt here.
    ///
    /// Callers are [`Self::on_os_appearance`] (the platform observer and the
    /// `appearance` control verb), window attach, and on Windows the window's
    /// `ThemeChanged` event.
    pub(crate) fn sync_app_theme_to_appearance(&mut self, appearance: aterm_types::Appearance) {
        // WINDOWS ONLY: re-assert the resolved title-bar appearance after the OS flip.
        // With config `window_theme = "auto"` the window's winit `preferred_theme` is
        // `None`, so winit's own `WM_SETTINGCHANGE` handler has just re-themed the
        // non-client area straight from the new OS preference — which is precisely the
        // decision the Windows arm overrides (it resolves the caption from the TERMINAL
        // background instead, so a dark grid never wears a white caption bar). That
        // clobber lands while the event is being produced, i.e. strictly before this
        // handler runs, so re-applying here is the last word.
        //
        // ABOVE the dedupe below, and that placement is load-bearing. Windows broadcasts
        // `WM_SETTINGCHANGE` to EVERY top-level window and winit's handler is PER-HWND,
        // dispatching `ThemeChanged` inline from inside each window's `WndProc`. With two
        // windows open across one OS flip the order is: W1's proc re-themes W1 and emits
        // `ThemeChanged(W1)` → we land here and re-assert both → THEN W2's proc runs,
        // re-themes W2, and emits `ThemeChanged(W2)` → we land here again with the
        // appearance already recorded, so an early return would leave W2 wearing the OS
        // caption for good. Hoisting costs one idempotent re-apply per window per event
        // and removes the hole entirely.
        //
        // Not folded into the `theme_changed` branch below either: a plain, non-split
        // `theme` keeps the SAME background across an OS flip, so `apply_theme_live` (and
        // with it `window_set_background_color`, the other re-resolution seam) never runs
        // — yet winit repainted the caption anyway. When it DOES run it re-publishes the
        // new background to every window afterwards, so the final word still carries the
        // freshly resolved colour.
        //
        // This is only the CHANGE-SIGNALLED half. winit re-themes on every
        // `WM_SETTINGCHANGE` but reports only the ones that move the OS theme, so the
        // silent broadcasts are caught on the frame path by
        // `platform_win::verify_chrome_appearance`.
        //
        // macOS/Linux are untouched: `window_theme_for_chrome` still passes `Auto`
        // through to AppKit/winit unchanged there, and this block does not compile.
        #[cfg(windows)]
        {
            for wid in self.appearance_redraw_targets() {
                if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
                    crate::platform_win::resync_chrome_appearance(w);
                }
            }
        }

        if self.os_appearance == appearance {
            return; // unchanged — nothing to re-resolve
        }
        self.os_appearance = appearance;

        // The OS appearance only picks the side of a split `theme`; what is
        // reported to programs is the palette that side draws.
        let installed = self.install_resolved_colors(ThemeChange::OsFlip);
        if !installed.theme_changed {
            // Chrome unchanged, but the `system_dark` repaint-key term and native
            // app previews follow the OS: nudge every window; each repaint key
            // keeps an untouched presentation cheap.
            for wid in self.appearance_redraw_targets() {
                if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
                    w.request_redraw();
                }
            }
        }
    }

    /// One reading from the OS appearance source ([`crate::Wake::OsAppearance`])
    /// or the development `appearance dark|light` verb: the same install.
    pub(crate) fn on_os_appearance(&mut self, appearance: aterm_types::Appearance) {
        self.sync_app_theme_to_appearance(appearance);
    }

    /// The `theme_side=` word: the side the OS appearance picked of a `theme`
    /// that follows it — a `dark:…,light:…` pair, or an unset (or nameless)
    /// `theme`, which draws [`crate::app_config::UNSET_THEME`]'s sides — and
    /// `-` for a single named theme (which no appearance moves).
    pub(crate) fn theme_side_word(&self) -> &'static str {
        use aterm_types::scheme::ThemeSelector;
        let (selector, _) = ThemeSelector::parse(self.config.theme.as_deref().unwrap_or(""));
        let follows = matches!(selector, ThemeSelector::Split { .. })
            || selector.name_for(aterm_types::Appearance::Dark).is_none();
        if follows {
            crate::os_appearance::appearance_word(self.installed_colors.side)
        } else {
            "-"
        }
    }

    /// `os_appearance= theme_side= reported_scheme= bg_dark=`, the appearance
    /// fields of a session's `status`: the OS reading and the split side are the
    /// instance's; `reported` (what DSR 996 / mode 2031 answer) and `bg` (the
    /// engine's default background) are the session's own, read by the caller
    /// under its terminal guard. `-` for a value the caller could not read.
    pub(crate) fn appearance_status_fields(
        &self,
        reported: Option<aterm_types::Appearance>,
        bg: Option<aterm_types::Rgb>,
    ) -> String {
        let reported = reported.map_or("-", crate::os_appearance::appearance_word);
        let bg_dark = bg.map_or("-", |bg| {
            if aterm_messages::ink::bg_is_light([bg.r, bg.g, bg.b]) {
                "0"
            } else {
                "1"
            }
        });
        format!(
            "os_appearance={} theme_side={} reported_scheme={reported} bg_dark={bg_dark}",
            crate::os_appearance::appearance_word(self.os_appearance),
            self.theme_side_word(),
        )
    }

    /// The `appearance` verb's line: the status fields for what the session
    /// factory installs (every session agrees after an install), plus where
    /// the reading came from, the theme drawn, and how many installs fanned
    /// `apply_config` out to every session (launch built for the right side
    /// when this stays 0 across window attach).
    pub(crate) fn appearance_status_line(&self) -> String {
        let fields = self.appearance_status_fields(
            Some(self.installed_colors.reported),
            Some(self.installed_colors.scheme.background),
        );
        let mut fields = fields.splitn(2, ' ');
        let os = fields.next().unwrap_or_default();
        let rest = fields.next().unwrap_or_default();
        format!(
            "{os} source={} {rest} theme={} installs={}",
            self.os_appearance_source.wire_name(),
            aterm_control::wire::pct_encode(&self.installed_colors.id),
            self.engine_color_fanouts,
        )
    }

    /// The windows an OS appearance flip must nudge when the rendered theme is
    /// UNCHANGED: ALL of them. The `system_dark` repaint-key term moves on every
    /// window, including background windows presenting native app previews. Split
    /// from the `request_redraw` loop so
    /// the headless suite can pin the all-window fan-out (the redraw itself needs
    /// a live `os_window`): see `appearance_flip_nudges_every_window`.
    pub(crate) fn appearance_redraw_targets(&self) -> Vec<WindowId> {
        self.windows.keys().copied().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::theme_to_appearance;
    use aterm_types::Appearance;
    use winit::window::Theme;

    #[test]
    fn os_scheme_reaches_terminal_leaves_inside_native_splits() {
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (mixed_session, _) =
            app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
        assert!(
            !app.windows[&wid]
                .layouts
                .iter()
                .any(|tree| tree.contains(mixed_session)),
            "negative control: the retired terminal-only projection cannot see this leaf"
        );

        // A split theme, so the light OS draws (and reports) a light palette.
        app.config = aterm_toml::from_str(r#"theme = "dark:Dracula,light:GitHub Light""#)
            .expect("split theme");
        app.sync_app_theme_to_appearance(Appearance::Light);
        assert_eq!(
            crate::term_lock(&app.pool.get(0).unwrap().term).color_scheme(),
            Appearance::Light,
            "ordinary background terminal tabs still update"
        );
        assert_eq!(
            crate::term_lock(&app.pool.get(mixed_session).unwrap().term).color_scheme(),
            Appearance::Light,
            "a heterogeneous terminal leaf receives the same reported scheme"
        );
    }

    /// REGRESSION (audit: vacuous `os_appearance_flip_repaints`): when the
    /// rendered theme is UNCHANGED, an OS appearance flip must nudge EVERY
    /// window — the pre-fix front-window-only nudge left an unfocused Settings
    /// window compositing its `window_theme=auto` titlebar mock stale through
    /// the flip. Headless windows carry no `os_window` (the `request_redraw`
    /// itself is unobservable here), so the pinned surface is the fan-out SET
    /// `sync_app_theme_to_appearance` draws its nudge loop from.
    #[test]
    fn appearance_flip_nudges_every_window() {
        let mut app = crate::App::headless_for_test();
        // A second mixed-tab window that is not frontmost (`install_window_state`
        // repoints frontmost, so put the first window back in front).
        let wid = crate::WindowId(9);
        app.install_window_state(wid, crate::stub_session(99), 38, 132);
        app.frontmost_window = Some(crate::WindowId(0));
        let mut targets = app.appearance_redraw_targets();
        targets.sort_unstable();
        assert_eq!(
            targets,
            vec![crate::WindowId(0), wid],
            "the appearance nudge fans out to every window, not just the front one"
        );
    }

    /// BROKEN-2 — an OS light/dark flip that moves the palette's appearance
    /// updates the SESSION FACTORY's reported scheme, so a tab/split spawned AFTER
    /// the flip starts at it (its engine is built with
    /// `set_color_scheme(factory.appearance)`). A plain dark theme keeps reporting
    /// dark on a light OS: the factory follows the palette, not the OS.
    #[test]
    fn os_flip_updates_the_session_factory_appearance() {
        let mut app = crate::App::headless_for_test();
        app.config = aterm_toml::from_str(r#"theme = "dark:Dracula,light:GitHub Light""#)
            .expect("split theme");
        assert_eq!(
            app.session_factory.appearance,
            Appearance::Dark,
            "the factory is seeded to the engine default"
        );
        app.sync_app_theme_to_appearance(Appearance::Light);
        assert_eq!(
            app.session_factory.appearance,
            Appearance::Light,
            "BROKEN-2: the factory tracks the light side the OS picked"
        );

        let mut plain = crate::App::headless_for_test();
        plain.config = aterm_toml::from_str(r#"theme = "Dracula""#).expect("plain theme");
        plain.sync_app_theme_to_appearance(Appearance::Light);
        assert_eq!(
            plain.session_factory.appearance,
            Appearance::Dark,
            "negative control: a dark theme on a light OS still reports dark"
        );
    }

    /// The winit theme → engine appearance mapping: Light/Dark map across, and an
    /// unknown (`None`) OS appearance falls back to the engine default (Dark) so it
    /// never spuriously flips the engine off its own default.
    #[test]
    fn theme_maps_light_dark_and_unknown_to_default() {
        assert_eq!(theme_to_appearance(Some(Theme::Light)), Appearance::Light);
        assert_eq!(theme_to_appearance(Some(Theme::Dark)), Appearance::Dark);
        // None == engine default == Appearance::default().
        assert_eq!(theme_to_appearance(None), Appearance::Dark);
        assert_eq!(theme_to_appearance(None), Appearance::default());
    }
}

/// The one install path: what DSR 996 / mode 2031 report, when the 997 push
/// lands, what a theme change costs, and what the image-meta fingerprint sees.
#[cfg(test)]
pub(crate) mod install_resolved_colors {
    use super::ThemeChange;
    use crate::App;
    use crate::app_config::{Config, ThemeCatalog};
    use aterm_types::{Appearance, ColorScheme, Rgb};

    /// Apply `text` as `aterm.toml` through the real admission path (the
    /// prepared generation a config watcher hands the App).
    pub(crate) fn apply_config_text(app: &mut App, dir: &std::path::Path, text: &str) {
        let path = dir.join("aterm.toml");
        std::fs::write(&path, text).expect("write config");
        let observation =
            crate::native_config_service::VersionedConfigService::observe_path(&path, false)
                .expect("observe");
        let config: Config = aterm_toml::from_str(text).expect("valid toml");
        let assets = config.resolve_asset_catalog();
        let path_feed_fps = config.path_feed_fingerprints();
        let sparkle = config.prepare_sparkle_runtime();
        app.apply_prepared_config_generation(
            crate::native_font_catalog::PreparedConfigGeneration {
                observation,
                config,
                values: std::collections::BTreeMap::new(),
                disk_faults: std::sync::Arc::default(),
                assets,
                path_feed_fps,
                sparkle,
                fonts: None,
                font_refusals: Vec::new(),
                warnings: Vec::new(),
            },
        );
    }

    fn scratch_dir(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("aterm-install-colors-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    /// The reply session 0 gives a program that asks `CSI ? 996 n`.
    fn dsr_996(app: &App) -> Vec<u8> {
        let mut term = crate::term_lock(&app.pool.get(0).expect("session 0").term);
        term.process(b"\x1b[?996n");
        term.take_response().expect("DSR 996 answers")
    }

    fn bg_of(name: &str) -> Rgb {
        aterm_types::scheme::builtin(name)
            .expect("built-in")
            .background
    }

    /// A light theme on a dark OS reports light: the palette on screen, not the
    /// desktop. Negative control: a dark theme on the same OS reports dark, and
    /// a dark theme on a LIGHT OS still reports dark.
    #[test]
    fn a_light_theme_on_a_dark_os_reports_light() {
        let dir = scratch_dir("dsr");
        let mut app = App::headless_for_test();
        assert_eq!(app.os_appearance, Appearance::Dark);
        apply_config_text(&mut app, &dir, "theme = \"GitHub Light\"\n");
        assert_eq!(
            app.theme.bg & 0x00FF_FFFF,
            0x00FF_FFFF,
            "GitHub Light is on screen"
        );
        assert_eq!(dsr_996(&app), b"\x1b[?997;2n", "GitHub Light reports light");

        apply_config_text(&mut app, &dir, "theme = \"Dracula\"\n");
        assert_eq!(dsr_996(&app), b"\x1b[?997;1n", "Dracula reports dark");

        app.sync_app_theme_to_appearance(Appearance::Light);
        assert_eq!(
            dsr_996(&app),
            b"\x1b[?997;1n",
            "a light OS does not make Dracula light"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Read whatever the program side of `fd` holds, without blocking; the push
    /// is written inline on unix, so it is there or it was never sent.
    #[cfg(unix)]
    pub(crate) fn drain(fd: i32) -> Vec<u8> {
        let mut out = Vec::new();
        let mut buf = [0u8; 256];
        loop {
            let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
            if n <= 0 {
                break;
            }
            out.extend_from_slice(&buf[..n as usize]);
        }
        out
    }

    /// A mode-2031 subscriber is pushed `?997;2n` when the config switches
    /// Dracula → Solarized Light, and by the time the push reaches the PTY the
    /// engine already answers OSC 11 with the new background (APPEAR-8: the
    /// palette and the push land in one critical section, palette first). An
    /// unrelated key change sends no push.
    #[cfg(unix)]
    #[test]
    fn a_theme_switch_pushes_997_after_the_new_palette_and_an_unrelated_edit_does_not() {
        let dir = scratch_dir("push");
        let mut fds = [0; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe");
        assert_eq!(
            unsafe { libc::fcntl(fds[0], libc::F_SETFL, libc::O_NONBLOCK) },
            0,
            "nonblocking read end"
        );
        let sink = std::sync::Arc::new(aterm_session::sink::SinkWriter::new(fds[1]));
        let mut app = App::headless_for_test_with_sink(sink);
        apply_config_text(&mut app, &dir, "theme = \"Dracula\"\n");
        crate::term_lock(&app.pool.get(0).expect("session 0").term).process(b"\x1b[?2031h");
        assert!(drain(fds[0]).is_empty(), "subscribing pushes nothing");

        apply_config_text(&mut app, &dir, "theme = \"Solarized Light\"\n");
        assert_eq!(
            drain(fds[0]),
            b"\x1b[?997;2n",
            "the subscriber is told the palette turned light"
        );
        let new_bg = bg_of("Solarized Light");
        {
            let mut term = crate::term_lock(&app.pool.get(0).expect("session 0").term);
            assert_eq!(term.default_background(), new_bg, "the palette landed");
            term.process(b"\x1b]11;?\x1b\\");
            let reply = String::from_utf8(term.take_response().expect("OSC 11 answers"))
                .expect("utf-8 reply");
            let hex = |c: u8| format!("{c:02x}{c:02x}");
            assert!(
                reply.contains(&format!(
                    "rgb:{}/{}/{}",
                    hex(new_bg.r),
                    hex(new_bg.g),
                    hex(new_bg.b)
                )),
                "a program answering the push with OSC 11 reads the new bg: {reply:?}"
            );
        }

        apply_config_text(
            &mut app,
            &dir,
            "theme = \"Solarized Light\"\ncopy_on_select = true\n",
        );
        assert!(
            drain(fds[0]).is_empty(),
            "an unrelated key change sends no push"
        );
        drop(app);
        unsafe { libc::close(fds[0]) };
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A theme file nothing draws changes no projection, so it costs no
    /// `apply_config` fan-out. Negative control: changing the file the config
    /// DOES draw fans out once.
    #[test]
    fn an_unused_theme_file_sends_no_apply_config() {
        let mut app = App::headless_for_test();
        app.config = aterm_toml::from_str("theme = \"Mine\"\n").expect("valid toml");
        let mine = ColorScheme {
            background: Rgb::new(0x10, 0x20, 0x30),
            ..ColorScheme::default()
        };
        app.reload_theme_catalog(ThemeCatalog::from_schemes([(
            "Mine".to_string(),
            mine.clone(),
        )]));
        let before = app.engine_color_fanouts;
        assert_eq!(
            app.session_factory
                .terminal_config
                .as_ref()
                .map(|tc| tc.default_background),
            Some(mine.background),
            "the configured file is installed"
        );

        app.reload_theme_catalog(ThemeCatalog::from_schemes([
            ("Mine".to_string(), mine.clone()),
            ("Unused".to_string(), ColorScheme::default()),
        ]));
        assert_eq!(
            app.engine_color_fanouts, before,
            "an unused .conf added sends no apply_config"
        );

        let edited = ColorScheme {
            background: Rgb::new(0x11, 0x20, 0x30),
            ..mine
        };
        app.reload_theme_catalog(ThemeCatalog::from_schemes([
            ("Mine".to_string(), edited),
            ("Unused".to_string(), ColorScheme::default()),
        ]));
        assert_eq!(
            app.engine_color_fanouts,
            before + 1,
            "negative control: editing the drawn file fans out once"
        );
    }

    /// Two themes that differ only in an ANSI slot draw the same chrome but give
    /// different image-meta fingerprints. Negative control: installing the same
    /// theme again gives the same fingerprint.
    #[test]
    fn themes_that_differ_only_in_ansi_fingerprint_differently() {
        let mut app = App::headless_for_test();
        app.config = aterm_toml::from_str("theme = \"Mine\"\n").expect("valid toml");
        let base = ColorScheme::default();
        let mut red = base.clone();
        red.ansi[1] = Rgb::new(0xff, 0x00, 0x00);
        let mut green = base;
        green.ansi[1] = Rgb::new(0x00, 0xff, 0x00);

        app.reload_theme_catalog(ThemeCatalog::from_schemes([(
            "Mine".to_string(),
            red.clone(),
        )]));
        let red_theme = app.theme;
        let red_fp = app.image_theme_fingerprint();
        app.reload_theme_catalog(ThemeCatalog::from_schemes([("Mine".to_string(), green)]));
        assert_eq!(app.theme, red_theme, "the chrome is identical");
        assert_ne!(
            app.image_theme_fingerprint(),
            red_fp,
            "an ANSI-only change moves the fingerprint"
        );
        app.reload_theme_catalog(ThemeCatalog::from_schemes([("Mine".to_string(), red)]));
        assert_eq!(
            app.image_theme_fingerprint(),
            red_fp,
            "the same colours fingerprint the same"
        );
    }

    /// A theme file's `selection_foreground` reaches the renderer knob, not only
    /// the engine; the config key still wins over it.
    #[test]
    fn a_theme_files_selection_foreground_reaches_the_renderer() {
        let mut app = App::headless_for_test();
        app.config = aterm_toml::from_str("theme = \"Mine\"\n").expect("valid toml");
        let mine = ColorScheme {
            selection_fg: Some(Rgb::new(0x12, 0x34, 0x56)),
            ..ColorScheme::default()
        };
        app.reload_theme_catalog(ThemeCatalog::from_schemes([(
            "Mine".to_string(),
            mine.clone(),
        )]));
        assert_eq!(app.render_knobs.selection_fg, Some(0x0012_3456));
        assert_eq!(
            app.session_factory
                .terminal_config
                .as_ref()
                .and_then(|tc| tc.selection_foreground),
            Some(Rgb::new(0x12, 0x34, 0x56)),
            "the engine and the renderer agree"
        );

        app.config = aterm_toml::from_str("theme = \"Mine\"\nselection_foreground = \"#abcdef\"\n")
            .expect("valid toml");
        app.install_resolved_colors(ThemeChange::ConfigReload);
        assert_eq!(
            app.render_knobs.selection_fg,
            Some(0x00AB_CDEF),
            "the config key wins over the theme file"
        );
    }
}

/// The OS appearance as its own source (APPEAR-1/2/10/12): a forced window
/// chrome does not mask a flip, launch builds for the side the OS shows, and
/// `status` says which side is drawn and reported.
#[cfg(test)]
mod appearance_source {
    use crate::App;
    use crate::app_config::WindowTheme;
    use aterm_types::{Appearance, Rgb};

    const SPLIT: &str = "theme = \"dark:Dracula,light:GitHub Light\"\n";

    fn bg_of(name: &str) -> Rgb {
        aterm_types::scheme::builtin(name)
            .expect("built-in")
            .background
    }

    fn rgb_u32(rgb: Rgb) -> u32 {
        crate::app_config::rgb_u32(rgb)
    }

    /// The reply session 0 gives a program that asks `CSI ? 996 n`.
    fn dsr_996(app: &App) -> Vec<u8> {
        let mut term = crate::term_lock(&app.pool.get(0).expect("session 0").term);
        term.process(b"\x1b[?996n");
        term.take_response().expect("DSR 996 answers")
    }

    /// One `key=value` field of a status record.
    fn field<'a>(record: &'a str, key: &str) -> Option<&'a str> {
        record
            .split_whitespace()
            .find_map(|kv| kv.strip_prefix(key)?.strip_prefix('='))
    }

    /// APPEAR-2: `window_theme = "dark"` forces the chrome, yet a light OS
    /// reading (the event the platform observer posts) draws and reports the
    /// light side. Negative control: the dark reading draws Dracula again.
    #[test]
    fn a_forced_dark_chrome_still_follows_an_injected_light_flip() {
        let mut app = App::headless_for_test();
        app.config =
            aterm_toml::from_str(&format!("window_theme = \"dark\"\n{SPLIT}")).expect("toml");
        app.window_theme = app.config.window_theme_or_default();
        assert_eq!(
            app.window_theme,
            WindowTheme::Dark,
            "the chrome is forced dark"
        );
        app.install_resolved_colors(super::ThemeChange::ConfigReload);
        assert_eq!(app.theme.bg & 0x00FF_FFFF, rgb_u32(bg_of("Dracula")));

        app.on_os_appearance(Appearance::Light);
        assert_eq!(
            app.theme.bg & 0x00FF_FFFF,
            rgb_u32(bg_of("GitHub Light")),
            "the light side is on screen under a forced dark chrome"
        );
        assert_eq!(dsr_996(&app), b"\x1b[?997;2n", "and reported");

        app.on_os_appearance(Appearance::Dark);
        assert_eq!(
            app.theme.bg & 0x00FF_FFFF,
            rgb_u32(bg_of("Dracula")),
            "negative control: the dark reading draws the dark side"
        );
        assert_eq!(dsr_996(&app), b"\x1b[?997;1n");
    }

    /// APPEAR-12: a launch built for the light reading (what `warm_config` and
    /// `main_entry` build when the early read says light) needs nothing at
    /// attach, which reads light again: session 0 already draws and reports
    /// the light side, and no second `apply_config` fans out. Negative
    /// control: the old cold start, built for dark, re-colours every session
    /// once at attach.
    #[test]
    fn a_launch_built_for_light_needs_no_second_apply_config_at_attach() {
        let launch = |read: Appearance| {
            let mut app = App::headless_for_test();
            app.config = aterm_toml::from_str(SPLIT).expect("toml");
            app.os_appearance = read;
            app.install_resolved_colors(super::ThemeChange::ConfigReload);
            app
        };

        let mut app = launch(Appearance::Light);
        assert_eq!(
            dsr_996(&app),
            b"\x1b[?997;2n",
            "session 0 reports light from launch"
        );
        let launched = app.engine_color_fanouts;
        app.sync_app_theme_to_appearance(Appearance::Light);
        assert_eq!(
            app.engine_color_fanouts, launched,
            "attach finds the launch already on the light side"
        );
        assert_eq!(
            crate::term_lock(&app.pool.get(0).expect("session 0").term).default_background(),
            bg_of("GitHub Light"),
        );

        let mut cold = launch(Appearance::Dark);
        let launched = cold.engine_color_fanouts;
        cold.sync_app_theme_to_appearance(Appearance::Light);
        assert_eq!(
            cold.engine_color_fanouts,
            launched + 1,
            "negative control: a launch built for dark re-applies every session at attach"
        );
    }

    /// APPEAR-10: `status` and the `appearance` line say which OS reading is
    /// in effect, which side it picked, what the session reports and whether
    /// its background is dark, and the four agree after a flip. A single
    /// theme has no side (`-`) and keeps reporting its own palette.
    #[test]
    fn status_appearance_fields_agree_after_a_flip() {
        let mut app = App::headless_for_test();
        app.config = aterm_toml::from_str(SPLIT).expect("toml");
        app.install_resolved_colors(super::ThemeChange::ConfigReload);
        app.on_os_appearance(Appearance::Light);
        let record = app.session_status_record(0).expect("status");
        assert_eq!(field(&record, "os_appearance"), Some("light"), "{record}");
        assert_eq!(field(&record, "theme_side"), Some("light"), "{record}");
        assert_eq!(field(&record, "reported_scheme"), Some("light"), "{record}");
        assert_eq!(field(&record, "bg_dark"), Some("0"), "{record}");
        let line = app.appearance_status_line();
        assert_eq!(field(&line, "os_appearance"), Some("light"), "{line}");
        assert_eq!(field(&line, "source"), Some("inert"), "{line}");
        assert_eq!(field(&line, "theme_side"), Some("light"), "{line}");
        assert_eq!(field(&line, "reported_scheme"), Some("light"), "{line}");
        assert_eq!(field(&line, "bg_dark"), Some("0"), "{line}");

        app.config = aterm_toml::from_str("theme = \"Dracula\"\n").expect("toml");
        app.install_resolved_colors(super::ThemeChange::ConfigReload);
        let record = app.session_status_record(0).expect("status");
        assert_eq!(field(&record, "os_appearance"), Some("light"), "{record}");
        assert_eq!(
            field(&record, "theme_side"),
            Some("-"),
            "a single theme has no side"
        );
        assert_eq!(
            field(&record, "reported_scheme"),
            Some("dark"),
            "negative control: Dracula reports dark on a light OS"
        );
        assert_eq!(field(&record, "bg_dark"), Some("1"), "{record}");

        // An unset theme follows the OS like a pair: on a light OS it draws
        // and names the light side.
        app.config = crate::app_config::Config::default();
        app.install_resolved_colors(super::ThemeChange::ConfigReload);
        let record = app.session_status_record(0).expect("status");
        assert_eq!(app.installed_colors.id, "Default Light");
        assert_eq!(field(&record, "theme_side"), Some("light"), "{record}");
        assert_eq!(field(&record, "reported_scheme"), Some("light"), "{record}");
    }
}
