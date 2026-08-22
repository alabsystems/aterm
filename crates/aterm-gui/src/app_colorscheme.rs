// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! OS color-scheme source: feed the REAL desktop light/dark appearance into every
//! session's engine via [`Terminal::set_color_scheme`].
//!
//! The engine already REPORTS the host color scheme to apps (DEC private mode 2031
//! plus DSR `CSI ? 996 n` → `CSI ? 997 ; Ps n`); it just needs the GUI to TELL it
//! what the OS appearance actually is. winit exposes that platform-neutrally as
//! [`winit::window::Theme`] (per-window `Window::theme()` at attach time, and a
//! `WindowEvent::ThemeChanged` on live switches), so this is the single seam that
//! maps that winit theme onto [`aterm_types::Appearance`] and pushes it to the
//! engine.
//!
//! When mode 2031 is set and the scheme CHANGES, `set_color_scheme` queues an
//! unsolicited `CSI ? 997 ; Ps n` in the engine's response buffer; we drain that via
//! [`Terminal::take_response`] and write it to the owning session's PTY sink so an
//! app that subscribed live-updates its own theme. The first call after spawn (when
//! the engine still holds its `Dark` default) is a real change iff the OS is Light,
//! which is exactly when an app should be told.

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

impl App {
    /// Push the OS color scheme `appearance` into EVERY session of window `wid`
    /// (each pane of each tab — the engine state is per-session) and flush any
    /// unsolicited DEC-2031 report the engine queued to that session's PTY.
    ///
    /// Called at window attach (from the real `window.theme()`) and on every
    /// `WindowEvent::ThemeChanged`. A no-op for a stale/unknown `wid`; a no-op INSIDE
    /// the engine when the scheme is unchanged (so a redundant ThemeChanged costs at
    /// most a lock per session and writes nothing to the PTY).
    pub(crate) fn apply_os_color_scheme(
        &mut self,
        wid: WindowId,
        appearance: aterm_types::Appearance,
    ) {
        // Every session across every tab/pane of this window. A split tab has >1
        // session; canonical trees include heterogeneous terminal leaves. Dedup so
        // a session shared by two views is poked once.
        let mut ids = self.window_terminal_sessions(wid);
        ids.sort_unstable();
        ids.dedup();
        for id in ids {
            let Some(session) = self.pool.get(id) else {
                continue;
            };
            // Take the per-session sink BEFORE locking the engine so we can flush the
            // engine's queued report (if any) without holding the term lock across the
            // PTY write.
            let sink = session.ctx.sink.clone();
            let response = {
                let mut term = term_lock(&session.term);
                term.set_color_scheme(appearance);
                // Drain the unsolicited `CSI ? 997 ; Ps n` the engine queued IFF the
                // scheme actually changed AND the app enabled DEC mode 2031. `None`
                // when unchanged or unsubscribed — the common steady-state path.
                term.take_response()
            };
            if let Some(resp) = response {
                // Best-effort: a closed/half-open PTY just drops the report. The OS
                // appearance is advisory; we never fail the GUI over it.
                let _ = sink.write_frame(&resp);
            }
        }
    }

    /// Switch aterm's OWN rendered theme to the side of a `dark:…,light:…` split
    /// `theme` config that matches the live OS `appearance`. The rendered-theme
    /// companion to [`Self::apply_os_color_scheme`] (which only feeds the engine's
    /// REPORTED scheme for DEC 2031).
    ///
    /// A NO-OP when the appearance is unchanged, and (for a plain, non-split `theme`)
    /// the re-resolved scheme is identical, so a single theme never re-themes on an OS
    /// toggle and the renderer is rebuilt ONLY when the chrome actually changes. Re-
    /// resolves from the retained live [`Config`](crate::Config) and immutable
    /// admitted theme catalog — no disk read — and
    /// re-applies the engine palette + rebuilds the backend exactly as a live
    /// `reload_config` does, so the switch is seamless.
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

        // Engine config (default fg/bg + ANSI palette) for the new appearance, applied
        // to every live session and pinned into the factory so future tabs inherit it.
        let applied_tc = self
            .config
            .applied_terminal_config_for_with_assets(appearance, &self.config_assets.themes);
        for s in self.pool.iter() {
            term_lock(&s.term).apply_config(&applied_tc);
        }
        self.session_factory.terminal_config = Some(applied_tc);
        // BROKEN-2: keep the factory's reported scheme current so a tab/split spawned
        // AFTER this flip starts its engine at the live OS appearance (not `Dark`), in
        // agreement with the palette pinned just above.
        self.session_factory.appearance = appearance;

        // Renderer chrome. `Theme` is a 4×u32 POD without `PartialEq`; compare fields
        // (the renderer bakes these in, so any change needs a backend rebuild).
        let new_theme = self
            .config
            .theme_for_with_assets(appearance, &self.config_assets.themes);
        let theme_changed = (
            new_theme.fg,
            new_theme.bg,
            new_theme.cursor,
            new_theme.selection,
        ) != (
            self.theme.fg,
            self.theme.bg,
            self.theme.cursor,
            self.theme.selection,
        );
        if theme_changed {
            // Colour-only swap: the face/atlas are untouched by a light/dark flip,
            // so push the theme onto the live backend instead of rebuilding it —
            // the shared fast path also retints the strip cache + titlebar bg and
            // drops every present cache. Mirrors `reload_config`'s theme-only path.
            self.apply_theme_live(new_theme);
        } else {
            // Chrome unchanged, but the engine palette and native app previews may
            // have re-coloured — nudge every window; each repaint key keeps an
            // untouched presentation cheap.
            for wid in self.appearance_redraw_targets() {
                if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
                    w.request_redraw();
                }
            }
        }
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

        app.apply_os_color_scheme(wid, Appearance::Light);
        assert_eq!(
            crate::term_lock(&app.pool.get(0).unwrap().term).color_scheme(),
            Appearance::Light,
            "ordinary background terminal tabs still update"
        );
        assert_eq!(
            crate::term_lock(&app.pool.get(mixed_session).unwrap().term).color_scheme(),
            Appearance::Light,
            "a heterogeneous terminal leaf receives the same OS appearance"
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

    /// BROKEN-2 — an OS light/dark flip updates the SESSION FACTORY's appearance, so a
    /// tab/split spawned AFTER the flip inherits the live scheme (its engine is built
    /// with `set_color_scheme(factory.appearance)`) instead of the `Dark` default. This
    /// pins the wiring source that `spawn_session` reads.
    #[test]
    fn os_flip_updates_the_session_factory_appearance() {
        let mut app = crate::App::headless_for_test();
        assert_eq!(
            app.session_factory.appearance,
            Appearance::Dark,
            "the factory is seeded to the engine default"
        );
        app.sync_app_theme_to_appearance(Appearance::Light);
        assert_eq!(
            app.session_factory.appearance,
            Appearance::Light,
            "BROKEN-2: the factory tracks the live OS appearance for future tabs"
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
