// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-session themes: one tab (or pane) drawn in a theme of its own.

use std::collections::HashMap;
use std::sync::Arc;

use aterm_core::config::TerminalConfig;

use crate::App;
use crate::resolved_colors::{ResolvedColors, ThemeSource};

/// A session's own theme, as the pool holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SessionTheme {
    /// A theme name or a `dark:…,light:…` split.
    pub(crate) selector: String,
    /// How the session came by it.
    pub(crate) origin: SessionThemeOrigin,
}

/// How a session came by its own theme.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionThemeOrigin {
    /// `theme session set`, or the palette's Choose Tab Theme.
    Ctl,
    /// `spawn theme=`.
    Spawn,
    /// A split from a pane with its own theme.
    Split,
    /// Carried by a seamless update or a restore.
    Restored,
}

impl SessionThemeOrigin {
    /// The `origin=` word.
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::Ctl => "ctl",
            Self::Spawn => "spawn",
            Self::Split => "split",
            Self::Restored => "restored",
        }
    }
}

/// Who chose the theme a session asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionDrawOrigin {
    /// The session's own theme.
    Own(SessionThemeOrigin),
}

impl SessionDrawOrigin {
    /// The `origin=` word.
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::Own(origin) => origin.word(),
        }
    }
}

/// The theme a session asks for, before it is resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SessionChoice {
    pub(crate) selector: String,
    pub(crate) origin: SessionDrawOrigin,
}

/// What a session draws.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SessionDraw {
    /// The instance's theme: the session asks for none.
    Instance,
    /// Its own resolution.
    Own {
        colors: Box<ResolvedColors>,
        choice: SessionChoice,
    },
    /// It asks for a theme that does not draw, so it draws the instance's.
    Missed { choice: SessionChoice, why: String },
}

impl SessionDraw {
    /// The theme it asks for, drawn or not.
    pub(crate) const fn choice(&self) -> Option<&SessionChoice> {
        match self {
            Self::Instance => None,
            Self::Own { choice, .. } | Self::Missed { choice, .. } => Some(choice),
        }
    }
}

/// The palette's Choose Tab Theme… while it is open: the window whose palette drives it, the session it draws on, and the theme it had before, which any close but Enter puts back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TabThemeTrial {
    pub(crate) window: crate::WindowId,
    pub(crate) session: u64,
    pub(crate) prior: Option<SessionTheme>,
}

/// One session's part of an install, decided before any terminal lock.
struct SessionApply {
    sid: u64,
    /// The projection to `apply_config`, when it moved.
    apply: Option<Arc<TerminalConfig>>,
    /// What the pool records as its own projection afterwards (`None`: it
    /// draws the instance's).
    own: Option<Arc<TerminalConfig>>,
}

impl App {
    /// Session `sid`'s own theme as it would be kept: under an open Choose Tab Theme… trial on it, the theme it had before the trial (the highlight is tentative until Enter), else the pool's.
    pub(crate) fn kept_session_theme(&self, sid: u64) -> Option<&SessionTheme> {
        match &self.palette_tab_theme {
            Some(trial) if trial.session == sid => trial.prior.as_ref(),
            _ => self.pool.theme(sid),
        }
    }

    /// The theme session `sid` asks for, or none for the instance's theme.
    pub(crate) fn session_choice(&self, sid: u64) -> Option<SessionChoice> {
        let theme = self.pool.theme(sid)?;
        Some(SessionChoice {
            selector: theme.selector.clone(),
            origin: SessionDrawOrigin::Own(theme.origin),
        })
    }

    /// Resolve `selector` as a session theme for the live appearance: the colours, or why it does not draw.
    pub(crate) fn resolve_session_selector(
        &self,
        selector: &str,
    ) -> Result<ResolvedColors, String> {
        self.resolve_session_selector_on(self.os_appearance, selector)
    }

    /// Resolve `selector` as a session theme on `side`, through the catalog alone (never the community pack): the colours, or why it does not draw.
    pub(crate) fn resolve_session_selector_on(
        &self,
        side: aterm_types::Appearance,
        selector: &str,
    ) -> Result<ResolvedColors, String> {
        if let Some(notice) = crate::app_config::theme_selector_notice(Some(selector)) {
            return Err(notice);
        }
        let colors = self
            .config
            .resolved_colors(side, &self.config_assets.themes, Some(selector));
        match &colors.source {
            ThemeSource::DefaultFallback { reason } => Err(reason.clone()),
            ThemeSource::Builtin | ThemeSource::File(_) | ThemeSource::Pack(_) => Ok(colors),
        }
    }

    /// The light/dark side a new session's shell is told it runs on (its `COLORFGBG`, Windows): that of the theme the session will draw, by [`Self::session_draw`]'s precedence, its own `theme` (asked, inherited by a split, restored).
    #[cfg(any(windows, test))]
    pub(crate) fn spawn_shell_appearance(
        &self,
        theme: Option<&str>,
    ) -> Option<aterm_types::Appearance> {
        let selector = theme?;
        self.resolve_session_selector(selector)
            .ok()
            .map(|colors| colors.reported)
    }

    /// The instance's session factory for a fresh shell whose session will draw `theme`: told that theme's side ([`Self::spawn_shell_appearance`]).
    #[cfg(windows)]
    pub(crate) fn shell_factory(&self, theme: Option<&str>) -> crate::spawn::SessionFactory {
        crate::spawn::SessionFactory {
            shell_appearance: self.spawn_shell_appearance(theme),
            ..self.session_factory.clone()
        }
    }

    /// What session `sid` draws: precedence own > instance; a choice that does not draw falls back to the instance.
    pub(crate) fn session_draw(&self, sid: u64) -> SessionDraw {
        let Some(choice) = self.session_choice(sid) else {
            return SessionDraw::Instance;
        };
        match self.resolve_session_selector(&choice.selector) {
            Ok(colors) => SessionDraw::Own {
                colors: Box::new(colors),
                choice,
            },
            Err(why) => SessionDraw::Missed { choice, why },
        }
    }

    /// The resolution session `sid` draws: its own, else the instance's.
    pub(crate) fn resolved_colors_for_session(&self, sid: u64) -> ResolvedColors {
        match self.session_draw(sid) {
            SessionDraw::Own { colors, .. } => *colors,
            SessionDraw::Instance | SessionDraw::Missed { .. } => self.installed_colors.clone(),
        }
    }

    /// How many live sessions draw a theme of their own: what an instance preview does not reach (`pinned=`).
    pub(crate) fn pinned_session_count(&self) -> usize {
        self.pool
            .ids()
            .into_iter()
            .filter(|sid| self.pool.own_colors(*sid).is_some())
            .count()
    }

    /// Decide each of `sids`' part of an install, borrowing only `&self`: a
    /// session drawing its own theme applies it when it differs from the one
    /// it last applied; one drawing the instance's applies `instance` when
    /// that moved (`instance_changed`) or when it has just left a theme of
    /// its own.
    fn session_color_plan(
        &self,
        sids: &[u64],
        instance: &Arc<TerminalConfig>,
        instance_changed: bool,
    ) -> Vec<SessionApply> {
        let mut memo: HashMap<String, Option<Arc<TerminalConfig>>> = HashMap::new();
        sids.iter()
            .map(|&sid| {
                let own = self.session_choice(sid).and_then(|choice| {
                    memo.entry(choice.selector)
                        .or_insert_with_key(|selector| {
                            self.resolve_session_selector(selector).ok().map(|colors| {
                                Arc::new(self.config.applied_terminal_config_from(&colors))
                            })
                        })
                        .clone()
                });
                let last = self.pool.own_colors(sid);
                match own {
                    Some(own) => SessionApply {
                        sid,
                        apply: (last.map(Arc::as_ref) != Some(own.as_ref()))
                            .then(|| Arc::clone(&own)),
                        own: Some(own),
                    },
                    None => SessionApply {
                        sid,
                        apply: (instance_changed || last.is_some()).then(|| Arc::clone(instance)),
                        own: None,
                    },
                }
            })
            .collect()
    }

    /// Carry out a plan: each session's `apply_config` and, in the SAME
    /// critical section, the `CSI ? 997 ; Ps n` push it queued when the
    /// session's reported scheme moved and its program set mode 2031.
    fn apply_session_colors(&mut self, plan: Vec<SessionApply>) -> (u64, u64) {
        let (mut own_applies, mut applies) = (0u64, 0u64);
        for SessionApply { sid, apply, own } in plan {
            if let Some(tc) = apply
                && let Some(session) = self.pool.get(sid)
            {
                let push = {
                    let mut term = crate::term_lock(&session.term);
                    let changes = term.apply_config(&tc);
                    session.ctx.crash_journal.host_settings(&term, Some(&tc));
                    session
                        .ctx
                        .temporal
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .set_replay_config(Some(Arc::clone(&tc)));
                    (changes.contains(&aterm_core::config::ConfigChange::ColorScheme)
                        && term.report_color_scheme_enabled())
                    .then(|| term.take_response())
                    .flatten()
                };
                if let Some(push) = push {
                    crate::app_input::write_ui_report(
                        &session.term,
                        &session.ctx.modes,
                        &session.ctx.sink,
                        &crate::input::InputEvent::KeySequence(push),
                    );
                }
                applies += 1;
                if own.is_some() {
                    own_applies += 1;
                }
            }
            self.pool.set_own_colors(sid, own);
        }
        self.session_color_installs = self.session_color_installs.saturating_add(own_applies);
        (own_applies, applies)
    }

    /// Install every session's colours for an install whose instance projection is `instance` (`instance_changed`: it differs from the one installed before).
    pub(crate) fn install_session_fanout(
        &mut self,
        instance: &Arc<TerminalConfig>,
        instance_changed: bool,
    ) -> u64 {
        let plan = self.session_color_plan(&self.pool.ids(), instance, instance_changed);
        self.apply_session_colors(plan).0
    }

    /// Bring ONE session up to what it draws now: after its own theme was set or cleared, and when it joins the pool (a spawn, a split, an adoption, a restore).
    pub(crate) fn install_session_colors(&mut self, sid: u64) {
        let instance = Arc::new(
            self.config
                .applied_terminal_config_from(&self.installed_colors),
        );
        let plan = self.session_color_plan(&[sid], &instance, false);
        let (own_applies, applies) = self.apply_session_colors(plan);
        if applies > 0 {
            self.sparkle_dirty = true;
            self.rain_dirty = true;
            self.request_redraw_all_windows();
        }
        aterm_log::debug!("session theme install: session={sid} own_applies={own_applies}");
    }

    /// Take a session into the pool and bring it up to the theme it draws ([`Self::install_session_colors`]): every production insertion goes through here, so a carried theme is on the engine before its reader runs.
    pub(crate) fn pool_insert(&mut self, session: crate::Session) -> u64 {
        let sid = self.pool.insert(session);
        self.recover_developer_tools_install(sid);
        self.install_session_colors(sid);
        sid
    }

    /// Set (or, with `None`, clear) session `sid`'s own theme and install it.
    pub(crate) fn set_session_theme(&mut self, sid: u64, theme: Option<SessionTheme>) {
        self.pool.set_theme(sid, theme);
        self.install_session_colors(sid);
    }

    /// `session_theme=<selector>|- theme_origin=<word>|instance`: the session `status` fields, the selector percent-encoded, so a name with spaces stays one field.
    pub(crate) fn session_theme_fields(&self, sid: u64) -> String {
        let choice = self.session_choice(sid);
        let theme = choice.as_ref().map_or_else(
            || "-".to_string(),
            |c| aterm_control::wire::pct_encode(&c.selector),
        );
        let origin = match &choice {
            Some(choice) if self.pool.own_colors(sid).is_some() => choice.origin.word(),
            Some(_) | None => "instance",
        };
        format!("session_theme={theme} theme_origin={origin}")
    }

    /// The `theme session` line for `sid`: what it asks for and what it draws.
    pub(crate) fn session_theme_status_line(&self, sid: u64) -> String {
        let draw = self.session_draw(sid);
        let theme = draw
            .choice()
            .map_or_else(|| "-".to_string(), |c| format!("{:?}", c.selector));
        let origin = match &draw {
            SessionDraw::Own { choice, .. } => choice.origin.word(),
            SessionDraw::Instance | SessionDraw::Missed { .. } => "instance",
        };
        let asked = draw.choice().map_or("-", |c| c.origin.word());
        let miss = match &draw {
            SessionDraw::Missed { why, .. } => format!("{why:?}"),
            SessionDraw::Instance | SessionDraw::Own { .. } => "-".to_string(),
        };
        let colors = match draw {
            SessionDraw::Own { colors, .. } => *colors,
            SessionDraw::Instance | SessionDraw::Missed { .. } => self.installed_colors.clone(),
        };
        let source = match colors.source {
            ThemeSource::Builtin => "builtin",
            ThemeSource::File(_) => "file",
            ThemeSource::Pack(_) => "pack",
            ThemeSource::DefaultFallback { .. } => "default",
        };
        let hex = |c: aterm_types::Rgb| format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b);
        format!(
            "session={sid} theme={theme} origin={origin} asked={asked} id={:?} source={source} \
             side={} reported={} miss={miss} generate={} fg={} bg={}",
            colors.id,
            crate::os_appearance::appearance_word(colors.side),
            crate::os_appearance::appearance_word(colors.reported),
            if colors.palette_generate { "on" } else { "off" },
            hex(colors.scheme.foreground),
            hex(colors.scheme.background),
        )
    }
}

#[cfg(test)]
#[path = "session_theme_tests.rs"]
mod tests;
