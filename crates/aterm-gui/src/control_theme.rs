// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The live theme on the control socket, and the transient preview lane.

use std::time::{Duration, Instant};

use aterm_types::{Appearance, Rgb};

use crate::App;
use crate::app_colorscheme::ThemeChange;
use crate::resolved_colors::ThemeSource;

/// How long a preview stands when `theme preview` names no `ttl=`.
pub(crate) const THEME_PREVIEW_DEFAULT_TTL: Duration = Duration::from_secs(120);
/// The longest `ttl=` a preview takes: a preview is a trial, not a setting.
pub(crate) const THEME_PREVIEW_MAX_TTL: Duration = Duration::from_secs(3600);

/// What keeps a theme preview standing, and so what ends it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PreviewHold {
    /// `theme preview [ttl=]`: reverts by itself at this instant.
    Until(Instant),
    /// The command palette's highlighted Theme row: no TTL; it stands while a palette highlights it and reverts when the palette closes or the highlight leaves the Theme section.
    Palette { window: crate::WindowId },
    /// Chosen in the palette (Enter): its `theme` write is in flight.
    Committing,
    /// Settings ▸ Themes' trial ([`App::sync_settings_theme_trial`]): it stands while a Settings view tries it, with no TTL, and reverts when none does (Revert, Esc, leaving the page, closing Settings).
    Settings { keeping: bool },
}

/// What a Settings view asks of the live theme ([`crate::native_settings::SettingsViewState::theme_trial_intent`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ThemeTrialIntent {
    /// Draw this theme selector everywhere in place of the configured one.
    Show(String),
    /// Keep drawing it: the page's Keep write for it is in flight.
    Keeping(String),
}

impl PreviewHold {
    /// The word the `theme` status line prints for the hold.
    const fn word(self) -> &'static str {
        match self {
            Self::Until(_) => "ttl",
            Self::Palette { .. } => "palette",
            Self::Committing => "commit",
            Self::Settings { .. } => "settings",
        }
    }
}

/// A standing theme preview.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ThemePreview {
    /// The selector drawn in place of the configured `theme` (what `theme commit` writes): a name or a `dark:…,light:…` split.
    pub(crate) id: String,
    /// The theme on screen before the first of a run of previews began.
    pub(crate) restore: String,
    /// What keeps it standing: a TTL, the palette's highlight, or a write in flight.
    pub(crate) hold: PreviewHold,
    /// The admitted save that owns a Committing hold.
    pub(crate) commit_request: Option<u64>,
    /// This trial or its latest commit's place in the config request sequence.
    pub(crate) generation: u64,
}

/// One `theme` control operation ([`crate::Wake::ThemeControl`]), parsed on the control thread (`control_media::parse_theme_form`) and run on the main thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ThemeCtlOp {
    /// `theme` / `theme status`: one status line, no mutation.
    Status,
    /// `theme list`: one row per theme the instance can name.
    List,
    /// `theme preview <id> [ttl=]`: draw `id` until `ttl` passes.
    Preview { id: String, ttl: Duration },
    /// `theme revert`: end the preview, if one stands.
    Revert,
    /// `theme commit`: write the preview's name to `aterm.toml`.
    Commit,
    /// `theme palette [session]`: the 256 palette rows the instance (or, with `session`, the aimed session) draws.
    Palette { session: bool },
    /// `theme session [status]`: the aimed session's own theme and what it draws.
    SessionStatus,
    /// `theme session set <selector>`: give the aimed session a theme of its own.
    SessionSet { selector: String },
    /// `theme session clear`: the aimed session draws the instance's theme again.
    SessionClear,
}

fn hex(c: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)
}

fn opt_hex(c: Option<Rgb>) -> String {
    c.map_or_else(|| "-".to_string(), hex)
}

const fn side_word(appearance: Appearance) -> &'static str {
    match appearance {
        Appearance::Dark => "dark",
        Appearance::Light => "light",
    }
}

/// The `theme palette` rows of one resolution: 256 lines `slot index=<n> rgb=#rrggbb origin=theme|config|default|generated|xterm`, what the engine is configured with (a program's OSC 4 is not in them).
pub(crate) fn theme_palette_rows(colors: &crate::resolved_colors::ResolvedColors) -> Vec<String> {
    let palette = colors.custom_palette().unwrap_or_default();
    (0..=255u8)
        .map(|index| {
            format!(
                "slot index={index} rgb={} origin={}",
                hex(palette.get(index)),
                colors.extended_origin(index).word()
            )
        })
        .collect()
}

/// Who set a chrome colour, as `theme` reports it: `theme`, `config`, or
/// `default` (nobody: the chrome derives it).
const fn origin_word(origin: crate::resolved_colors::ColorOrigin) -> &'static str {
    use crate::resolved_colors::ColorOrigin;
    match origin {
        ColorOrigin::Theme => "theme",
        ColorOrigin::Config => "config",
        ColorOrigin::Default => "default",
    }
}

impl App {
    /// Run one `theme` operation and answer `reply`.
    pub(crate) fn theme_control(
        &mut self,
        op: ThemeCtlOp,
        session: Option<u64>,
        now: Instant,
        reply: std::sync::mpsc::Sender<Result<String, String>>,
    ) {
        let answer = match op {
            ThemeCtlOp::Status => Ok(self.theme_status_line(now)),
            ThemeCtlOp::List => Ok(self.theme_list_wire_rows().join("\n")),
            ThemeCtlOp::Preview { id, ttl } => self.preview_theme(&id, ttl, now),
            ThemeCtlOp::Revert => Ok(self.revert_theme_preview(now)),
            ThemeCtlOp::Commit => return self.commit_theme_preview(reply),
            ThemeCtlOp::Palette { session: false } => {
                Ok(theme_palette_rows(&self.installed_colors).join("\n"))
            }
            ThemeCtlOp::Palette { session: true } => self
                .theme_session_target(session)
                .map(|sid| theme_palette_rows(&self.resolved_colors_for_session(sid)).join("\n")),
            ThemeCtlOp::SessionStatus => self
                .theme_session_target(session)
                .map(|sid| self.session_theme_status_line(sid)),
            ThemeCtlOp::SessionSet { selector } => self
                .theme_session_target(session)
                .and_then(|sid| self.set_session_theme_checked(sid, &selector)),
            ThemeCtlOp::SessionClear => self.theme_session_target(session).map(|sid| {
                if self.pool.theme(sid).is_some() {
                    self.set_session_theme(sid, None);
                }
                self.session_theme_status_line(sid)
            }),
        };
        let _ = reply.send(answer);
    }

    /// The session a session-scoped `theme` form acts on: `session` when the
    /// request names one, else the front window's front terminal.
    fn theme_session_target(&self, session: Option<u64>) -> Result<u64, String> {
        match session {
            Some(sid) if self.pool.get(sid).is_some() => Ok(sid),
            Some(sid) => Err(format!("no such session {sid}")),
            None => {
                let wid = self
                    .frontmost_window
                    .ok_or_else(|| "no focused window".to_string())?;
                self.front_terminal(wid)
                    .map(|t| t.session)
                    .ok_or_else(|| "front content is not a terminal".to_string())
            }
        }
    }

    /// `theme session set`: refused, changing nothing, when the selector has grammar issues or a side would draw Default for want of a theme as the session resolves it ([`Self::session_theme_unavailable`]: a community pack theme the themes folder lacks is refused, though a preview draws it); else the session's own theme, installed.
    pub(crate) fn set_session_theme_checked(
        &mut self,
        sid: u64,
        selector: &str,
    ) -> Result<String, String> {
        if let Some(why) = self.session_theme_unavailable(selector) {
            return Err(format!("theme session {selector:?}: {why}"));
        }
        self.set_session_theme(
            sid,
            Some(crate::session_theme::SessionTheme {
                selector: selector.to_string(),
                origin: crate::session_theme::SessionThemeOrigin::Ctl,
            }),
        );
        Ok(self.session_theme_status_line(sid))
    }

    /// The `theme` status line: the installed resolution and where it came from, every quoted field escaped so a hostile file name cannot split it.
    pub(crate) fn theme_status_line(&self, now: Instant) -> String {
        let colors = &self.installed_colors;
        let themes = &self.config_assets.themes;
        let label = themes
            .meta(&colors.id)
            .map_or_else(|| colors.id.clone(), |meta| meta.display_name);
        let configured = self
            .config
            .theme
            .as_deref()
            .map_or_else(|| "-".to_string(), |raw| format!("{raw:?}"));
        let source = match &colors.source {
            ThemeSource::Builtin => "builtin".to_string(),
            ThemeSource::File(id) => match themes.source_path(id) {
                Some(path) => format!("file path={path:?}"),
                None => "file".to_string(),
            },
            ThemeSource::Pack(_) => "pack".to_string(),
            ThemeSource::DefaultFallback { .. } => "default".to_string(),
        };
        let misses = crate::app_config::theme_misses(&self.config, themes);
        let miss = if misses.is_empty() {
            "-".to_string()
        } else {
            format!("{:?}", misses.join("; "))
        };
        let preview = match &self.theme_preview {
            None => "preview=-".to_string(),
            Some(preview) => format!(
                "preview={:?} restore={:?} hold={} ttl_ms={}",
                preview.id,
                preview.restore,
                preview.hold.word(),
                match preview.hold {
                    PreviewHold::Until(expires) => expires
                        .saturating_duration_since(now)
                        .as_millis()
                        .to_string(),
                    PreviewHold::Palette { .. }
                    | PreviewHold::Committing
                    | PreviewHold::Settings { .. } => "-".to_string(),
                }
            ),
        };
        let scheme = &colors.scheme;
        let palette = scheme
            .ansi
            .iter()
            .map(|c| hex(*c))
            .collect::<Vec<_>>()
            .join(",");
        let header = self
            .chrome_palette(crate::app_render::ChromeSurface::Header)
            .tokens();
        let grid = self.chrome_palette(crate::app_render::ChromeSurface::Grid);
        let grid_tokens = grid.tokens();
        let bytes_hex = |c: [u8; 3]| hex(aterm_types::Rgb::new(c[0], c[1], c[2]));
        let chrome = format!(
            "accent={} accent_source={} accent_from={} border={} border_from={} \
             search_match={} search_match_from={} active_tab={} active_tab_from={}",
            bytes_hex(header.accent),
            header.accent_source.label(),
            origin_word(colors.provenance.chrome.accent),
            bytes_hex(grid_tokens.seam_on(crate::settings::u32_rgb(grid.theme.bg))),
            origin_word(colors.provenance.chrome.border),
            bytes_hex(grid_tokens.search_match(crate::settings::u32_rgb(grid.theme.selection))),
            origin_word(colors.provenance.chrome.search_match),
            opt_hex(scheme.chrome.active_tab),
            origin_word(colors.provenance.chrome.active_tab),
        );
        let quoted_or_dash = |items: Vec<String>| {
            if items.is_empty() {
                "-".to_string()
            } else {
                format!("{:?}", items.join(","))
            }
        };
        let typography_fields = format!(
            "typography={} typography_keys={} typography_shadowed={} font_size_scale={}",
            self.typography
                .applied
                .summary()
                .map_or_else(|| "-".to_string(), |summary| format!("{summary:?}")),
            quoted_or_dash(
                self.typography
                    .applied_keys()
                    .into_iter()
                    .map(str::to_string)
                    .collect()
            ),
            quoted_or_dash(self.typography.shadowed_words()),
            aterm_types::scheme_typography::milli_text(self.font_size_scale_milli),
        );
        format!(
            "id={:?} label={label:?} configured={configured} side={} source={source} \
             reported={} miss={miss} os_appearance={} {preview} fg={} bg={} cursor={} \
             selection={} selection_fg={} {chrome} {typography_fields} generate={} \
             generate_from={} pinned={} palette={palette}",
            colors.id,
            side_word(colors.side),
            side_word(colors.reported),
            side_word(self.os_appearance),
            hex(scheme.foreground),
            hex(scheme.background),
            opt_hex(scheme.cursor),
            opt_hex(scheme.selection),
            opt_hex(scheme.selection_fg),
            if colors.palette_generate { "on" } else { "off" },
            origin_word(colors.palette_generate_origin),
            self.pinned_session_count(),
        )
    }

    /// The `theme list` rows over the admitted catalog, the drawn theme marked.
    pub(crate) fn theme_list_wire_rows(&self) -> Vec<String> {
        crate::diagnostics::theme_list_wire_rows(
            &self.config_assets.themes,
            aterm_types::scheme::user_theme_dir().as_deref(),
            &self.installed_colors.id,
        )
    }

    /// Draw `id` in place of the configured `theme` until `now + ttl`, writing nothing.
    pub(crate) fn preview_theme(
        &mut self,
        id: &str,
        ttl: Duration,
        now: Instant,
    ) -> Result<String, String> {
        self.begin_theme_preview(id, PreviewHold::Until(now + ttl))
            .map_err(|why| format!("theme preview {id:?}: {why}"))?;
        Ok(self.theme_status_line(now))
    }

    /// Draw `id` in place of the configured `theme`, held by `hold`, through the one install path.
    pub(crate) fn begin_theme_preview(
        &mut self,
        id: &str,
        hold: PreviewHold,
    ) -> Result<(), String> {
        if let Some(why) = self.theme_preview_unavailable(id) {
            return Err(why);
        }
        let restore = self
            .theme_preview
            .as_ref()
            .map_or_else(|| self.installed_colors.id.clone(), |p| p.restore.clone());
        self.theme_preview = Some(ThemePreview {
            id: id.to_string(),
            restore,
            hold,
            commit_request: None,
            generation: crate::app_native::next_control_settings_request(),
        });
        self.install_resolved_colors(ThemeChange::Preview);
        Ok(())
    }

    /// Why selector `id` cannot be previewed under the config and catalog now held: grammar issues, or a side that would draw Default for want of a theme.
    pub(crate) fn theme_preview_unavailable(&self, id: &str) -> Option<String> {
        if let Some(notice) = crate::app_config::theme_selector_notice(Some(id)) {
            return Some(notice);
        }
        [Appearance::Dark, Appearance::Light]
            .into_iter()
            .find_map(|side| {
                match self
                    .config
                    .resolved_colors_previewing(side, &self.config_assets.themes, Some(id))
                    .source
                {
                    ThemeSource::DefaultFallback { reason } => {
                        Some(format!("unavailable ({reason}); nothing changed"))
                    }
                    ThemeSource::Builtin | ThemeSource::File(_) | ThemeSource::Pack(_) => None,
                }
            })
    }

    /// Why selector `id` cannot be a session's own theme under the config and catalog now held: its grammar issues, else a side, dark or light, that [`Self::resolve_session_selector_on`] (what the session draws with) does not draw, so a community pack theme the themes folder lacks is refused though a preview draws it.
    pub(crate) fn session_theme_unavailable(&self, id: &str) -> Option<String> {
        if let Some(notice) = crate::app_config::theme_selector_notice(Some(id)) {
            return Some(notice);
        }
        [Appearance::Dark, Appearance::Light]
            .into_iter()
            .find_map(|side| self.resolve_session_selector_on(side, id).err())
            .map(|reason| format!("unavailable ({reason}); nothing changed"))
    }

    /// End the preview if `held` says it is the one standing, and install what the config draws.
    pub(crate) fn end_theme_preview_if(&mut self, held: impl Fn(&ThemePreview) -> bool) -> bool {
        if !self.theme_preview.as_ref().is_some_and(held) {
            return false;
        }
        self.theme_preview = None;
        self.install_resolved_colors(ThemeChange::Preview);
        true
    }

    /// End the preview, if one stands, and install what the config draws.
    pub(crate) fn revert_theme_preview(&mut self, now: Instant) -> String {
        if self.theme_preview.take().is_some() {
            self.install_resolved_colors(ThemeChange::Preview);
        }
        self.theme_status_line(now)
    }

    /// Revert the preview when its TTL has run out by `now`.
    pub(crate) fn expire_theme_preview(&mut self, now: Instant) -> bool {
        self.end_theme_preview_if(
            |preview| matches!(preview.hold, PreviewHold::Until(expires) if now >= expires),
        )
    }

    /// The instant a standing preview reverts by itself; `None` for one the palette or a write in flight holds, which no clock ends.
    pub(crate) fn next_theme_preview_wake(&self) -> Option<Instant> {
        match self.theme_preview.as_ref()?.hold {
            PreviewHold::Until(expires) => Some(expires),
            PreviewHold::Palette { .. }
            | PreviewHold::Committing
            | PreviewHold::Settings { .. } => None,
        }
    }

    /// Follow Settings ▸ Themes' trials: the theme a Settings view tries is drawn everywhere ([`PreviewHold::Settings`], no TTL), and a Settings trial nobody asks for any more is reverted.
    pub(crate) fn sync_settings_theme_trial(&mut self, _now: Instant) {
        let intent = self.settings_theme_trial_intent();
        let settings_held = |preview: &ThemePreview| {
            matches!(preview.hold, PreviewHold::Settings { keeping: false })
        };
        match intent {
            None => {
                self.settings_trial_refused = None;
                self.end_theme_preview_if(settings_held);
            }
            Some(ThemeTrialIntent::Keeping(_)) => {}
            Some(ThemeTrialIntent::Show(id)) => {
                let standing = self.theme_preview.as_ref();
                let ours_already = standing.is_some_and(|preview| {
                    matches!(preview.hold, PreviewHold::Settings { .. })
                        && preview.id.eq_ignore_ascii_case(&id)
                });
                let held_elsewhere = standing
                    .is_some_and(|preview| !matches!(preview.hold, PreviewHold::Settings { .. }));
                let refused = self
                    .settings_trial_refused
                    .as_deref()
                    .is_some_and(|refused| refused.eq_ignore_ascii_case(&id));
                let blocker = standing
                    .filter(|_| held_elsewhere)
                    .map(|preview| preview.id.clone());
                if ours_already {
                    if let Some(preview) = self.theme_preview.as_mut() {
                        if preview.hold == (PreviewHold::Settings { keeping: true }) {
                            preview.commit_request = None;
                            preview.generation = crate::app_native::next_control_settings_request();
                        }
                        preview.hold = PreviewHold::Settings { keeping: false };
                    }
                } else if !refused {
                    let drawn = match blocker {
                        Some(blocker) => Err(format!(
                            "another preview is showing ({blocker}); revert it or wait for it to end"
                        )),
                        None => {
                            self.begin_theme_preview(&id, PreviewHold::Settings { keeping: false })
                        }
                    };
                    if let Err(why) = drawn {
                        self.settings_trial_refused = Some(id.clone());
                        self.refuse_settings_theme_trial(&id, &why);
                    }
                }
            }
        }
    }

    /// What the Settings views ask of the live theme: the front window's
    /// first, then any other window's, so one person's trial is what shows.
    fn settings_theme_trial_intent(&self) -> Option<ThemeTrialIntent> {
        self.native_runtime
            .instance_by_kind(crate::native_app::AppKind::Settings)?;
        let mut front = None;
        let mut other = None;
        for &wid in self.windows.keys() {
            let mut found = None;
            self.visit_settings_tabs_in_window(wid, |_, _, _, view| {
                if found.is_none()
                    && let Some(crate::native_app::AppViewState::Settings(state)) =
                        self.native_runtime.view_state(view)
                {
                    found = state.theme_trial_intent();
                }
            });
            if found.is_some() {
                if Some(wid) == self.frontmost_window {
                    front = found;
                } else if other.is_none() {
                    other = found;
                }
            }
        }
        front.or(other)
    }

    /// Tell every Settings view trying `id` that it cannot be drawn.
    fn refuse_settings_theme_trial(&mut self, id: &str, why: &str) {
        let mut views = Vec::new();
        for &wid in self.windows.keys() {
            self.visit_settings_tabs_in_window(wid, |_, _, _, view| views.push(view));
        }
        for view in views {
            if let Some(crate::native_app::AppViewState::Settings(state)) =
                self.native_runtime.view_state_mut(view)
            {
                state.refuse_theme_trial(id, why);
            }
        }
        self.request_redraw_all_windows();
    }

    /// Write the preview's name to `aterm.toml` through the `settings set
    /// theme` lane; `reply` gets that lane's answer (misses included).
    fn commit_theme_preview(&mut self, reply: std::sync::mpsc::Sender<Result<String, String>>) {
        let dir = aterm_types::scheme::user_theme_dir();
        self.commit_theme_preview_into(dir.as_deref(), reply, |app, id, request_id, reply| {
            app.queue_control_settings_field_admitted(
                crate::prefs::EDIT_THEME.to_string(),
                Some(id),
                reply,
                request_id,
            )
        });
    }

    /// Commit the standing preview, first installing into `dir` the community pack file a pack-only preview draws ([`Self::install_pack_preview`]), so the `theme` written names a theme the catalog holds.
    pub(crate) fn commit_theme_preview_into(
        &mut self,
        dir: Option<&std::path::Path>,
        reply: std::sync::mpsc::Sender<Result<String, String>>,
        queue: impl FnOnce(
            &mut Self,
            String,
            u64,
            std::sync::mpsc::Sender<Result<String, String>>,
        ) -> Option<u64>,
    ) {
        if let Some(id) = self.theme_preview.as_ref().map(|p| p.id.clone())
            && let Err(why) = self.install_pack_preview(&id, dir)
        {
            let _ = reply.send(Err(format!("theme commit {id:?}: {why}")));
            return;
        }
        self.commit_theme_preview_with(reply, queue);
    }

    /// Install the community pack file each side of selector `id` draws from the embedded pack ([`ThemeSource::Pack`]) into `dir`, then read the folder into the catalog at once (not on the watcher's next poll).
    pub(crate) fn install_pack_preview(
        &mut self,
        id: &str,
        dir: Option<&std::path::Path>,
    ) -> Result<(), String> {
        let mut pack_ids = Vec::new();
        for side in [Appearance::Dark, Appearance::Light] {
            if let ThemeSource::Pack(pack_id) = self
                .config
                .resolved_colors_previewing(side, &self.config_assets.themes, Some(id))
                .source
                && !pack_ids.contains(&pack_id)
            {
                pack_ids.push(pack_id);
            }
        }
        if pack_ids.is_empty() {
            return Ok(());
        }
        let dir = dir.ok_or_else(|| {
            "no themes folder: neither XDG_CONFIG_HOME, APPDATA nor HOME is set".to_string()
        })?;
        for pack_id in &pack_ids {
            match crate::settings_themes_host::add_pack_into(dir, pack_id) {
                crate::native_app::ThemesOutcome::Added { .. } => {}
                crate::native_app::ThemesOutcome::Failed { message } => return Err(message),
                other => return Err(format!("could not add {pack_id}: {other:?}")),
            }
        }
        let catalog = crate::app_config::ThemeCatalog::try_discover_in(dir)
            .map_err(|_| "the themes folder could not be read after the install".to_string())?;
        self.reload_theme_catalog(std::sync::Arc::new(catalog));
        Ok(())
    }

    /// The same preview commit with the queue handoff injected: tests can keep a real config request pending without constructing a platform event loop.
    pub(crate) fn commit_theme_preview_with(
        &mut self,
        reply: std::sync::mpsc::Sender<Result<String, String>>,
        queue: impl FnOnce(
            &mut Self,
            String,
            u64,
            std::sync::mpsc::Sender<Result<String, String>>,
        ) -> Option<u64>,
    ) {
        let Some(id) = self.theme_preview.as_ref().map(|p| p.id.clone()) else {
            let _ = reply.send(Err("no theme preview to commit".to_string()));
            return;
        };
        let request_id = crate::app_native::next_control_settings_request();
        self.bind_theme_preview_commit(&id, request_id);
        if queue(self, id, request_id, reply).is_none() {
            self.end_theme_preview_if(|p| p.commit_request == Some(request_id));
        }
    }

    pub(crate) fn bind_theme_preview_commit(&mut self, id: &str, request_id: u64) {
        if let Some(preview) = self
            .theme_preview
            .as_mut()
            .filter(|preview| preview.id.eq_ignore_ascii_case(id))
        {
            preview.hold = if matches!(preview.hold, PreviewHold::Settings { .. }) {
                PreviewHold::Settings { keeping: true }
            } else {
                PreviewHold::Committing
            };
            preview.commit_request = Some(request_id);
            preview.generation = request_id;
        }
    }
}

#[cfg(test)]
#[path = "control_theme_tests.rs"]
mod tests;
