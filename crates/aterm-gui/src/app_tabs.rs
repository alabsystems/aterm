// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tab + session-view management: the windows/pool/focus_order-mutating cluster
//! moved as ONE unit so the invariant-maintaining set stays cohesive. Open/
//! switch/cycle/move/close tabs, detach/migrate a tab to a new window, the
//! tab-strip title/fingerprint/hit-test helpers, session teardown, the close
//! outcome funnel, and the live structural-invariant oracle. A verbatim
//! inherent-impl split of `App`.

use winit::event_loop::ActiveEventLoop;

use crate::platform::AppRt;
use crate::spawn::spawn_session;
use crate::{
    App, TabAction, TabIndex, WindowId, WindowState, pane, session_store, tab_bar, term_lock,
};

/// Whether a native close preflight is allowed to take over the screen when a
/// reducer answers [`crate::native_app::CloseReadiness::Blocked`].
///
/// THIS EXISTS BECAUSE A BLOCKED CLOSE HAS TWO VERY DIFFERENT AUDIENCES, and
/// until now both got the same, maximally intrusive treatment.
/// `surface_native_close_recovery` switches the active tab, moves keyboard focus,
/// re-fronts the window and REPLACES `window.overlay` with a Close Recovery
/// palette (destroying whatever overlay the user had open). For a person who just
/// pressed Cmd-W or Quit that is exactly right: they asked a question and the
/// blocker is the answer, shown where they are already looking.
///
/// For BACKGROUND POLICY it is a focus hijack. The automatic update lane re-probes
/// `prepare_all_native_shutdown` on a schedule for as long as a blocker persists
/// (`app_native.rs`, `PREFLIGHT_BLOCK_COOLDOWN`), so an Interactive probe there
/// meant a user with one unsaved Settings draft had their tab switched, their
/// focus stolen and a palette thrown over their work every couple of hours,
/// forever, for an update that is merely WAITING. Owner instruction: "a user who
/// is simply busy must not have their tabs switched, their focus stolen, or a
/// recovery panel reopened on a schedule."
///
/// The safety answer is identical under both — `Quiet` runs the same reducer and
/// returns the same `Ok(false)`/`Err` — so nothing downstream becomes more
/// permissive. Only the screen is left alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClosePreflightVisibility {
    /// A person asked for this close: surface the reducer's recovery commands on
    /// the blocking leaf.
    Interactive,
    /// Background policy asked: report the verdict, disturb nothing.
    Quiet,
}

/// Decide whether a background tab's new title epoch changes visible tab chrome.
///
/// The epoch remains the cheapest gate for ordinary PTY output. Once it changes,
/// compare the live title with the exact title last sent through
/// [`App::refresh_window_tabs`]. Busy-spinner phase changes keep that stable chrome
/// title and therefore avoid the expensive all-tab title/tooltip/metadata rebuild.
#[must_use]
pub(crate) fn background_title_refresh_needed(
    cached_epoch: Option<u64>,
    cached_title: Option<&str>,
    live_epoch: u64,
    live_title: &str,
) -> bool {
    cached_epoch != Some(live_epoch)
        && cached_title.is_none_or(|current| {
            current != live_title
                && !crate::toolbar::busy_spinner_phase_only_change(current, live_title)
        })
}

/// Resolve the terminal-owned rungs of a tab label from one captured snapshot.
///
/// The presentation/`"aterm"` fallback is window-local, so callers apply it
/// only when this returns `None`. Keeping this ordering in one pure seam makes
/// the title-drift fast path compare effective chrome with effective chrome:
/// an OSC spinner hidden beneath an operator-pinned title cannot manufacture
/// an expensive all-tab refresh.
#[must_use]
pub(crate) fn resolved_terminal_title_rung(
    user_title: Option<&str>,
    live_title: &str,
    live_cwd: Option<&str>,
) -> Option<String> {
    user_title
        .filter(|title| !title.is_empty())
        .map(str::to_owned)
        .or_else(|| (!live_title.is_empty()).then(|| live_title.to_owned()))
        .or_else(|| live_cwd.filter(|cwd| !cwd.is_empty()).map(home_abbreviated))
}

/// Make the keep-stale tab-title cache agree with the exact snapshot that
/// caused an effective chrome refresh.
///
/// `refresh_window_tabs` performs its own nonblocking terminal read. If that
/// second read contends after the drift epoch has been consumed, it must fall
/// back to this captured value rather than resurrecting the prior label
/// indefinitely. A coalesced spinner phase (`needs_refresh == false`) keeps the
/// intentionally stable cache untouched.
pub(crate) fn synchronize_captured_title_cache(
    cache: &mut std::collections::HashMap<u64, String>,
    session: u64,
    resolved_rung: Option<&str>,
    needs_refresh: bool,
) {
    if !needs_refresh {
        return;
    }
    if let Some(title) = resolved_rung {
        cache.insert(session, title.to_owned());
    } else {
        // No terminal-owned rung means `tab_titles` must use this tab's live
        // presentation fallback. Removing is more accurate than caching one
        // window/tab's fallback under the session-wide compatibility key.
        cache.remove(&session);
    }
}

#[cfg(test)]
mod background_title_tests {
    use super::{
        background_title_refresh_needed, resolved_terminal_title_rung,
        synchronize_captured_title_cache,
    };

    #[test]
    fn spinner_epochs_coalesce_before_tab_refresh() {
        let frames = [
            "⠋ aterm",
            "⠙ aterm",
            "⠹ aterm",
            "⠸ aterm",
            "⠼ aterm",
            "⠴ aterm",
            "⠦ aterm",
            "⠧ aterm",
            "⠇ aterm",
            "⠏ aterm",
        ];
        let mut epoch = None;
        let mut chrome: Option<String> = None;
        let mut refreshes = 0;

        for index in 0..100_u64 {
            let next = frames[index as usize % frames.len()];
            let live_epoch = index + 1;
            if background_title_refresh_needed(epoch, chrome.as_deref(), live_epoch, next) {
                chrome = Some(next.to_string());
                refreshes += 1;
            }
            // Production records every observed epoch, including a coalesced phase.
            epoch = Some(live_epoch);
        }

        assert_eq!(
            refreshes, 1,
            "spinner phases must not rebuild all tab chrome"
        );
        assert_eq!(chrome.as_deref(), Some("⠋ aterm"));
        assert!(!background_title_refresh_needed(
            epoch,
            chrome.as_deref(),
            epoch.unwrap(),
            "⠙ project",
        ));
        assert!(background_title_refresh_needed(
            epoch,
            chrome.as_deref(),
            epoch.unwrap() + 1,
            "⠙ project",
        ));
        assert!(background_title_refresh_needed(
            epoch,
            chrome.as_deref(),
            epoch.unwrap() + 1,
            "aterm",
        ));
    }
    #[test]
    fn pinned_user_title_masks_all_underlying_spinner_epochs() {
        let frames = ["⠋ compiling", "⠙ compiling", "⠹ compiling", "⠸ compiling"];
        let cached = "my build";
        let mut epoch = Some(1);
        let mut refreshes = 0;

        for (index, frame) in frames.into_iter().cycle().take(100).enumerate() {
            let live_epoch = index as u64 + 2;
            let effective =
                resolved_terminal_title_rung(Some(cached), frame, Some("/tmp/underlying-cwd"))
                    .expect("the pinned title is the effective chrome rung");
            if background_title_refresh_needed(epoch, Some(cached), live_epoch, &effective) {
                refreshes += 1;
            }
            epoch = Some(live_epoch);
        }

        assert_eq!(refreshes, 0, "hidden spinner phases must cost no refresh");
    }

    #[test]
    fn captured_drift_snapshot_wins_a_contended_refresh_read() {
        let mut cache = std::collections::HashMap::from([(7, "stale".to_owned())]);

        synchronize_captured_title_cache(&mut cache, 7, Some("fresh"), true);
        assert_eq!(cache.get(&7).map(String::as_str), Some("fresh"));

        // Spinner-only epochs are consumed without moving visible chrome.
        synchronize_captured_title_cache(&mut cache, 7, Some("⠙ fresh"), false);
        assert_eq!(cache.get(&7).map(String::as_str), Some("fresh"));

        // An empty terminal rung selects the current per-tab presentation
        // fallback; a stale session cache must not override it.
        synchronize_captured_title_cache(&mut cache, 7, None, true);
        assert!(!cache.contains_key(&7));
    }
}

/// Whether `tab` belongs in the legacy terminal-only compatibility projection.
/// Generic tab positions are never reused as projection positions: a native tab
/// may be inserted anywhere in [`crate::tab_model::TabSet`] without shifting a
/// `PaneTree` or changing which terminal the compatibility mirror targets.
fn is_terminal_tab(tab: &crate::tab_model::Tab, views: &crate::tab_model::ViewStore) -> bool {
    tab.root
        .leaves()
        .into_iter()
        .all(|view| matches!(views.get(view), Some(crate::tab_model::View::Terminal(_))))
}

/// Resolve a terminal-projection position to stable canonical identity.
fn terminal_tab_id_at(
    tabs: &crate::tab_model::TabSet,
    views: &crate::tab_model::ViewStore,
    projection_index: usize,
) -> Option<crate::tab_model::TabId> {
    tabs.tabs()
        .iter()
        .filter(|tab| is_terminal_tab(tab, views))
        .nth(projection_index)
        .map(|tab| tab.id)
}

/// Resolve stable canonical identity into the terminal-only compatibility
/// projection. Native identities intentionally have no result.
fn terminal_projection_index(
    tabs: &crate::tab_model::TabSet,
    views: &crate::tab_model::ViewStore,
    id: crate::tab_model::TabId,
) -> Option<usize> {
    tabs.tabs()
        .iter()
        .filter(|tab| is_terminal_tab(tab, views))
        .position(|tab| tab.id == id)
}

fn terminal_tab_ids(
    tabs: &crate::tab_model::TabSet,
    views: &crate::tab_model::ViewStore,
) -> Vec<crate::tab_model::TabId> {
    tabs.tabs()
        .iter()
        .filter(|tab| is_terminal_tab(tab, views))
        .map(|tab| tab.id)
        .collect()
}

/// If canonical focus names a terminal, point the compatibility mirror at the
/// corresponding terminal projection entry. Native focus leaves that mirror
/// parked on its previous live terminal.
fn align_terminal_projection_to_active(ws: &mut WindowState, views: &crate::tab_model::ViewStore) {
    let Some(active) = ws.tab_set.active_id() else {
        return;
    };
    if let Some(index) = terminal_projection_index(&ws.tab_set, views, active) {
        ws.tabs.switch_to(index);
    }
}

/// Remove one terminal projection entry. [`TabIndex::close`] deliberately keeps
/// the historical last-terminal behavior (it reports whole-window closure and
/// does not mutate), so a mixed window whose last terminal closes while native
/// tabs survive needs the explicit zero-entry representation here.
fn remove_terminal_projection(ws: &mut WindowState, projection_index: usize) {
    if ws.tabs.count == 1 {
        debug_assert_eq!(projection_index, 0);
        ws.tabs = TabIndex::new(0, 0);
    } else {
        let was_last = ws.tabs.close(projection_index);
        debug_assert!(!was_last);
    }
}

/// One canonical terminal-view occurrence.  `layouts` is deliberately absent:
/// heterogeneous tabs have no terminal-only projection, so session lifecycle
/// must resolve through stable tab/view identity first and consult the legacy
/// projection only after proving the whole tab is terminal-backed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TerminalViewLocation {
    tab_id: crate::tab_model::TabId,
    canonical_index: usize,
    view: crate::tab_model::ViewId,
    terminal_only: bool,
}

/// Failure to install a native app/view/tab as one host transaction.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum NativeTabInstallError {
    UnknownWindow,
    IdExhausted,
    Runtime(crate::native_app::RuntimeError),
    DuplicateTab,
}

/// The `home`-relative suffix of `path`: `Some("")` when `path` IS the home
/// directory, `Some("/sub")` (the `/`-prefixed remainder) when it lives under
/// it, `None` otherwise. Matches on a component boundary exactly like the zsh
/// integration's precmd (`aterm_shell_integration.zsh` matches `$HOME` with a
/// trailing `/`): a SIBLING like `/Users//foobar` under `home=/Users//foo` must
/// never abbreviate to `~bar`. An empty `home` never matches — an unset/empty
/// `$HOME` must not turn every path into `~<path>`. Pure (explicit `home`) so
/// the boundary cases are provable without touching process env.
pub(crate) fn home_relative_suffix<'p>(path: &'p str, home: &str) -> Option<&'p str> {
    if home.is_empty() {
        return None;
    }
    let rest = path.strip_prefix(home)?;
    (rest.is_empty() || rest.starts_with('/')).then_some(rest)
}

/// The process `$HOME`, read ONCE and cached for the process lifetime. The tab
/// label path runs per tab per repaint-fingerprint recompute; `env::var` scans
/// the whole environ block, so the hot path must not pay it repeatedly (and
/// `$HOME` legitimately never changes mid-run). `None` when unset or empty.
pub(crate) fn cached_home() -> Option<&'static str> {
    static HOME: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    HOME.get_or_init(|| std::env::var("HOME").ok().filter(|home| !home.is_empty()))
        .as_deref()
}

/// `path` with a leading `$HOME` component abbreviated to `~` — the tab-label
/// form of a session's shell-reported cwd, byte-matching what the zsh
/// integration's precmd puts in OSC 0 titles (`~` for home itself, `~/sub`
/// below it, sibling/foreign paths left verbatim).
pub(crate) fn home_abbreviated(path: &str) -> String {
    match cached_home().and_then(|home| home_relative_suffix(path, home)) {
        Some(rest) => format!("~{rest}"),
        None => path.to_string(),
    }
}

impl App {
    /// Detach one terminal view and retire all per-session auxiliary state only
    /// when the pool confirms that this was the final attachment.
    ///
    /// Every production teardown path funnels through this method so summary
    /// snapshots, retries, and queued provider work cannot outlive the PTY they
    /// describe. Shared sessions remain live until their last view closes.
    pub(crate) fn detach_session_view(&mut self, session: u64) -> bool {
        let dropped = self.pool.detach(session);
        if dropped {
            self.retire_title_summary(session);
            self.retire_session_status(session);
            // These caches can carry authored descriptions, generated activity,
            // cwd/title text, and debounce deadlines. The final view is the
            // lifecycle boundary: erase them immediately instead of waiting for
            // the lazy >64-entry reaper or a future drift sweep. A non-final
            // detach preserves every entry for the remaining shared view.
            self.session_chrome.remove(&session);
            self.session_chrome_retry.remove(&session);
            // The parked predictive-echo link estimate dies with the session too —
            // the entry describes a link that no longer exists, and without this the
            // map would accumulate one entry per closed tab for the process lifetime.
            self.link_estimates.remove(&session);
            // The parked find origin names an absolute row in a grid that is about to
            // stop existing; without this the map would grow one entry per closed tab
            // per window for the process lifetime. `retain`, not `remove`, because the
            // key carries the window a shared session's find was opened in.
            self.find_origins.retain(|(entry, _), _| *entry != session);
            self.session_chrome_expiry.cancel_session(session);
            self.title_drift.forget(session);
            for window in self.windows.values_mut() {
                window.tab_title_epochs.remove(&session);
                window.tab_title_cache.remove(&session);
                window.tab_chrome_titles.remove(&session);
                window
                    .tab_chrome_titles_by_tab
                    .retain(|_, (label_session, _)| *label_session != Some(session));
            }
        }
        dropped
    }

    fn terminal_view_location(
        &self,
        window: WindowId,
        session: u64,
    ) -> Option<TerminalViewLocation> {
        let window = self.windows.get(&window)?;
        window
            .tab_set
            .tabs()
            .iter()
            .enumerate()
            .find_map(|(canonical_index, tab)| {
                let view = tab.root.leaves().into_iter().find(|view| {
                    self.view_store
                        .get(*view)
                        .copied()
                        .and_then(crate::tab_model::View::terminal_session)
                        == Some(session)
                })?;
                Some(TerminalViewLocation {
                    tab_id: tab.id,
                    canonical_index,
                    view,
                    terminal_only: is_terminal_tab(tab, &self.view_store),
                })
            })
    }

    /// Collapse one terminal leaf out of a heterogeneous canonical tree.  Such a
    /// tab necessarily survives: by definition it also contains at least one
    /// native leaf.  Ownership is retired in view -> pool -> registry order and
    /// the terminal-only compatibility projection remains untouched.
    fn close_heterogeneous_terminal_view(
        &mut self,
        window: WindowId,
        location: TerminalViewLocation,
    ) -> bool {
        let valid = self.windows.get(&window).is_some_and(|state| {
            state.tab_set.get(location.tab_id).is_some_and(|tab| {
                tab.root.len() > 1
                    && !is_terminal_tab(tab, &self.view_store)
                    && self
                        .view_store
                        .get(location.view)
                        .is_some_and(|view| matches!(view, crate::tab_model::View::Terminal(_)))
            })
        });
        if !valid {
            return false;
        }
        if self.defer_pending_update_handoff_teardown(crate::DeferredHandoffTeardown::mutation(
            crate::DeferredHandoffMutation::CloseView {
                window,
                tab: location.tab_id,
                view: location.view,
            },
        )) {
            return false;
        }
        let removed = self.windows.get_mut(&window).and_then(|state| {
            let canonical = state
                .tab_set
                .tabs()
                .iter()
                .position(|tab| tab.id == location.tab_id)?;
            state
                .tab_set
                .tab_at_mut(canonical)
                .map(|tab| tab.remove_view(location.view))
        });
        if removed != Some(crate::tab_model::RemoveLeaf::Removed) {
            return false;
        }
        let Some(crate::tab_model::View::Terminal(terminal)) = self.remove_view_link(location.view)
        else {
            return false;
        };
        if let Some(state) = self.windows.get_mut(&window) {
            state.leaf_render_cache.remove(&location.view);
        }
        self.teardown_session(terminal.session);
        self.refresh_aggregate_tab_presentation(window, location.tab_id);
        self.resize_panes(window);
        self.resync_active_or_window(window);
        true
    }

    fn retain_closed_tab(
        &mut self,
        window: WindowId,
        original_index: usize,
        tab: crate::restore::RestoredTab,
    ) {
        let now_ms = self.lat_epoch.elapsed().as_millis() as u64;
        self.closed_recovery.tabs.push(
            crate::closed_recovery::ClosedTab {
                original_window: window,
                original_index,
                tab,
            },
            now_ms,
        );
    }

    fn closed_view_record_for_active(
        &self,
        window: WindowId,
    ) -> Option<crate::closed_recovery::ClosedView> {
        let tab = self.windows.get(&window)?.tab_set.active()?;
        if crate::closed_recovery::leaf_close_record_kind(tab.root.len())
            != crate::closed_recovery::LeafCloseRecordKind::ClosedView
        {
            return None;
        }
        let (parent, branch, axis, ratio) = tab.root.leaf_placement(tab.focus)?;
        let parent_path = parent
            .branches()
            .iter()
            .map(|branch| match branch {
                crate::tab_model::SplitBranch::First => crate::restore::RestoreBranch::First,
                crate::tab_model::SplitBranch::Second => crate::restore::RestoreBranch::Second,
            })
            .collect();
        let placement = crate::closed_recovery::ClosedViewPlacement::new(
            parent_path,
            match branch {
                crate::tab_model::SplitBranch::First => crate::restore::RestoreBranch::First,
                crate::tab_model::SplitBranch::Second => crate::restore::RestoreBranch::Second,
            },
            match axis {
                crate::tab_model::SplitAxis::Horizontal => crate::restore::SplitKind::Horizontal,
                crate::tab_model::SplitAxis::Vertical => crate::restore::SplitKind::Vertical,
            },
            ratio,
        )?;
        Some(crate::closed_recovery::ClosedView {
            original_window: window,
            original_tab: tab.id,
            view: self.view_restore_descriptor(tab.focus)?,
            placement,
        })
    }

    fn retain_closed_view(&mut self, record: crate::closed_recovery::ClosedView) {
        let now_ms = self.lat_epoch.elapsed().as_millis() as u64;
        self.closed_recovery.views.push(record, now_ms);
    }

    fn live_view_presentation(
        &self,
        view: crate::tab_model::ViewId,
    ) -> Option<crate::tab_model::TabPresentation> {
        match self.view_store.get(view).copied()? {
            crate::tab_model::View::Terminal(terminal) => {
                // The leaf carries its OWN session's status so the aggregate can
                // OR it with its siblings' — a busy background pane stays visible
                // instead of being hidden by whichever pane happens to be focused.
                let indicators = self.session_status_indicators(terminal.session);
                let session = self.pool.get(terminal.session)?;
                let title = term_lock(&session.term).title().to_string();
                let label = if title.is_empty() { "Terminal" } else { &title };
                let mut presentation = crate::tab_model::TabPresentation::terminal(label);
                presentation.indicators = indicators;
                Some(presentation)
            }
            crate::tab_model::View::Native(native) => {
                let presentation = self
                    .native_runtime
                    .presentation(native.instance, view)
                    .ok()?;
                Some(crate::tab_model::TabPresentation {
                    title: presentation.title,
                    icon: Some(match presentation.icon {
                        crate::native_app::AppIcon::Settings => {
                            crate::tab_model::TabIconKind::Settings
                        }
                        crate::native_app::AppIcon::Markdown => {
                            crate::tab_model::TabIconKind::Markdown
                        }
                        crate::native_app::AppIcon::Editor => crate::tab_model::TabIconKind::Editor,
                        crate::native_app::AppIcon::Recovery => {
                            crate::tab_model::TabIconKind::Recovery
                        }
                    }),
                    indicators: crate::tab_model::TabIndicators {
                        dirty: presentation.indicators.dirty,
                        busy: presentation.indicators.busy,
                        // A native leaf is the OUT-OF-BAND attention owner; it
                        // has no session and therefore no classified status.
                        attention: presentation.indicators.attention,
                        status_attention: false,
                    },
                    closable: presentation.closable,
                    tooltip: presentation.tooltip,
                })
            }
        }
    }

    /// Fold one tab's live leaf presentations without storing the result.
    fn aggregated_tab_presentation(
        &self,
        wid: WindowId,
        tab_id: crate::tab_model::TabId,
    ) -> Option<crate::tab_model::TabPresentation> {
        let (focus, leaves) = self.windows.get(&wid).and_then(|window| {
            let tab = window.tab_set.get(tab_id)?;
            Some((tab.focus, tab.root.leaves()))
        })?;
        let presentations = leaves
            .into_iter()
            .filter_map(|view| self.live_view_presentation(view).map(|value| (view, value)))
            .collect::<Vec<_>>();
        crate::tab_model::aggregate_presentations(focus, presentations)
    }

    pub(crate) fn refresh_aggregate_tab_presentation(
        &mut self,
        wid: WindowId,
        tab_id: crate::tab_model::TabId,
    ) {
        let Some(presentation) = self.aggregated_tab_presentation(wid, tab_id) else {
            return;
        };
        if let Some(tab) = self.windows.get_mut(&wid).and_then(|window| {
            let index = window
                .tab_set
                .tabs()
                .iter()
                .position(|tab| tab.id == tab_id)?;
            window.tab_set.tab_at_mut(index)
        }) {
            tab.presentation = presentation;
        }
    }

    /// Every tab that VIEWS this session, addressed by leaf rather than by
    /// focus: a split's background pane owns indicator state of its own.
    pub(crate) fn tabs_viewing_session(
        &self,
        session: u64,
    ) -> Vec<(WindowId, crate::tab_model::TabId)> {
        let mut tabs = Vec::new();
        for (wid, ws) in &self.windows {
            for tab in ws.tab_set.tabs() {
                if tab.root.any_leaf(&mut |view| {
                    self.view_store
                        .get(*view)
                        .copied()
                        .and_then(crate::tab_model::View::terminal_session)
                        == Some(session)
                }) {
                    tabs.push((*wid, tab.id));
                }
            }
        }
        tabs
    }

    /// Refold ONE tab's indicator bits from its leaves, leaving the rest of its
    /// presentation untouched. Returns whether the stored bits moved.
    ///
    /// The status path deliberately does not reuse
    /// [`Self::refresh_aggregate_tab_presentation`]: that replaces the whole
    /// presentation, which would drop the composed tooltip `tab_chrome_ext`
    /// writes back until the next chrome pass. Bits only, and change-gated —
    /// this runs at status-change rate, and a steady prompt must not dirty the
    /// tab model.
    pub(crate) fn refresh_tab_status_indicators(
        &mut self,
        wid: WindowId,
        tab_id: crate::tab_model::TabId,
    ) -> bool {
        // Fold the leaves' OWN status bits directly. Deliberately NOT via
        // `aggregated_tab_presentation`: that path resolves a live title through
        // a BLOCKING `term_lock` per leaf, and this runs on the output path — a
        // pane flooding inside `process()` would park the UI thread on exactly
        // the tab it is trying to light up. That regression is already fixed and
        // regression-tested for tab titles (`tab_titles_keep_stale_on_contention_
        // and_never_block`); it must not come back through the status path. No
        // lock is needed anyway: indicators come from the classifier, not the
        // grid.
        let Some(sessions) = self.windows.get(&wid).and_then(|window| {
            let tab = window.tab_set.get(tab_id)?;
            let mut sessions = Vec::new();
            tab.root.any_leaf(&mut |view| {
                if let Some(crate::tab_model::View::Terminal(terminal)) = self.view_store.get(*view)
                {
                    sessions.push(terminal.session);
                }
                false
            });
            Some(sessions)
        }) else {
            return false;
        };
        if sessions.is_empty() {
            return false;
        }
        // OR across leaves so one pane never hides another (the invariant
        // `TabIndicators` exists for).
        let folded = sessions
            .into_iter()
            .map(|session| self.session_status_indicators(session))
            .fold(crate::tab_model::TabIndicators::default(), |acc, bits| {
                crate::tab_model::TabIndicators {
                    dirty: acc.dirty,
                    busy: acc.busy || bits.busy,
                    attention: acc.attention,
                    status_attention: acc.status_attention || bits.status_attention,
                }
            });
        let Some(tab) = self.windows.get_mut(&wid).and_then(|window| {
            let index = window
                .tab_set
                .tabs()
                .iter()
                .position(|tab| tab.id == tab_id)?;
            window.tab_set.tab_at_mut(index)
        }) else {
            return false;
        };
        // The two RECOMPUTED bits are written wholesale; the two bits terminals
        // do not own pass through untouched. `dirty` means unsaved editor state,
        // and `attention` belongs to whichever native leaf raised it out of band
        // (a failed document shutdown, an update announcement) — neither is
        // derivable from a classifier, and neither may be erased by this pass.
        // Because `status_attention` is its own field, this can be a plain
        // assignment: the read-modify-write that used to latch a failure onto
        // the tab for the process lifetime is simply gone.
        let indicators = crate::tab_model::TabIndicators {
            dirty: tab.presentation.indicators.dirty,
            busy: folded.busy,
            attention: tab.presentation.indicators.attention,
            status_attention: folded.status_attention,
        };
        let changed = tab.presentation.indicators != indicators;
        tab.presentation.indicators = indicators;
        changed
    }

    fn promote_terminal_projection_if_needed(
        &mut self,
        wid: WindowId,
        tab_id: crate::tab_model::TabId,
    ) {
        let Some((root, focus, zoomed, canonical_index)) =
            self.windows.get(&wid).and_then(|window| {
                let canonical_index = window
                    .tab_set
                    .tabs()
                    .iter()
                    .position(|tab| tab.id == tab_id)?;
                let tab = window.tab_set.get(tab_id)?;
                tab.root
                    .leaves()
                    .into_iter()
                    .all(|view| {
                        matches!(
                            self.view_store.get(view),
                            Some(crate::tab_model::View::Terminal(_))
                        )
                    })
                    .then(|| (tab.root.clone(), tab.focus, tab.zoomed, canonical_index))
            })
        else {
            return;
        };
        let Some(layout) = self.restored_terminal_pane_layout(&root, focus) else {
            return;
        };
        let sessions = root
            .leaves()
            .into_iter()
            .filter_map(|view| {
                self.view_store
                    .get(view)
                    .copied()
                    .and_then(crate::tab_model::View::terminal_session)
            })
            .collect::<Vec<_>>();
        let Some(mut tree) = pane::PaneTree::rebuild(&layout, &sessions) else {
            return;
        };
        if zoomed {
            tree.toggle_zoom();
        }
        let Some(window) = self.windows.get_mut(&wid) else {
            return;
        };
        let terminal_count = window
            .tab_set
            .tabs()
            .iter()
            .filter(|tab| is_terminal_tab(tab, &self.view_store))
            .count();
        if window.layouts.len() >= terminal_count {
            return;
        }
        let projection = window
            .tab_set
            .tabs()
            .iter()
            .take(canonical_index)
            .filter(|tab| is_terminal_tab(tab, &self.view_store))
            .count();
        window.layouts.insert(projection, tree);
        let active = window
            .tabs
            .active
            .min(window.layouts.len().saturating_sub(1));
        window.tabs = TabIndex::new(active, window.layouts.len());
        if window.tab_set.active_id() == Some(tab_id) {
            window.tabs.active = projection;
        }
    }

    /// Attach one view of an existing native app instance and append it as the
    /// active stable tab in `wid`.  The terminal compatibility vectors are
    /// deliberately untouched: native leaves do not fabricate a PTY/session.
    ///
    /// Every fallible step rolls back the preceding ownership edges. Stable IDs
    /// already minted during a failed attempt remain burned, as required for
    /// stale-event safety.
    pub(crate) fn install_native_tab(
        &mut self,
        wid: WindowId,
        instance: crate::tab_model::AppInstanceId,
        state: crate::native_app::AppViewState,
        presentation: crate::tab_model::TabPresentation,
    ) -> Result<(crate::tab_model::TabId, crate::tab_model::ViewId), NativeTabInstallError> {
        if !self.windows.contains_key(&wid) {
            return Err(NativeTabInstallError::UnknownWindow);
        }

        let view = self
            .view_store
            .insert_native(instance)
            .map_err(|_| NativeTabInstallError::IdExhausted)?;
        if let Err(error) = self.native_runtime.attach_view(view, instance, state) {
            self.view_store.remove(view);
            return Err(NativeTabInstallError::Runtime(error));
        }

        let tab_id = match self.tab_ids.allocate() {
            Ok(id) => id,
            Err(_) => {
                self.native_runtime.remove_view(view);
                self.view_store.remove(view);
                return Err(NativeTabInstallError::IdExhausted);
            }
        };
        let tab = crate::tab_model::Tab::new(tab_id, view, presentation);
        let Some(ws) = self.windows.get_mut(&wid) else {
            // The event loop is single-threaded, but keep the transaction robust
            // if window ownership is ever made asynchronous.
            self.native_runtime.remove_view(view);
            self.view_store.remove(view);
            return Err(NativeTabInstallError::UnknownWindow);
        };
        if ws.tab_set.push(tab).is_err() {
            self.native_runtime.remove_view(view);
            self.view_store.remove(view);
            return Err(NativeTabInstallError::DuplicateTab);
        }
        let switched = ws.tab_set.switch_to(tab_id);
        debug_assert!(switched);
        debug_assert!(ws.tab_set.invariant_holds(&self.view_store));
        // Publish canonical front content immediately. For a front native tab
        // this clears `ActiveHandle` (there is no PTY); a background window only
        // refreshes its own chrome/render state.
        self.resync_active_or_window(wid);
        Ok((tab_id, view))
    }

    /// Create a native app instance and install its first tab view atomically.
    /// Call [`Self::install_native_tab`] directly for singleton/shared instances.
    pub(crate) fn install_new_native_tab(
        &mut self,
        wid: WindowId,
        app: crate::native_app::NativeApp,
        state: crate::native_app::AppViewState,
        presentation: crate::tab_model::TabPresentation,
    ) -> Result<
        (
            crate::tab_model::AppInstanceId,
            crate::tab_model::TabId,
            crate::tab_model::ViewId,
        ),
        NativeTabInstallError,
    > {
        if !self.windows.contains_key(&wid) {
            return Err(NativeTabInstallError::UnknownWindow);
        }
        let instance = self
            .native_runtime
            .insert_instance(app)
            .map_err(NativeTabInstallError::Runtime)?;
        match self.install_native_tab(wid, instance, state, presentation) {
            Ok((tab, view)) => Ok((instance, tab, view)),
            Err(error) => {
                self.native_runtime.remove_instance(instance);
                Err(error)
            }
        }
    }

    /// Attach another view of an existing native app as a sibling of the
    /// focused canonical leaf. This is the content-agnostic split capability
    /// used by restore, duplicate-view commands and tests; it never fabricates a
    /// terminal compatibility tree.
    #[allow(
        dead_code,
        reason = "stable restore/duplicate-view host seam; live command registration lands independently"
    )]
    pub(crate) fn split_active_with_native(
        &mut self,
        wid: WindowId,
        axis: crate::tab_model::SplitAxis,
        instance: crate::tab_model::AppInstanceId,
        state: crate::native_app::AppViewState,
    ) -> Result<crate::tab_model::ViewId, NativeTabInstallError> {
        if !self.windows.contains_key(&wid) {
            return Err(NativeTabInstallError::UnknownWindow);
        }
        let view = self
            .view_store
            .insert_native(instance)
            .map_err(|_| NativeTabInstallError::IdExhausted)?;
        if let Err(error) = self.native_runtime.attach_view(view, instance, state) {
            self.view_store.remove(view);
            return Err(NativeTabInstallError::Runtime(error));
        }
        let split = self
            .windows
            .get_mut(&wid)
            .and_then(|window| window.tab_set.active_mut())
            .is_some_and(|tab| tab.split_focused(axis, view));
        if !split {
            self.native_runtime.remove_view(view);
            self.view_store.remove(view);
            return Err(NativeTabInstallError::UnknownWindow);
        }
        self.resize_panes(wid);
        self.resync_active_or_window(wid);
        debug_assert!(self.structural_invariants_ok());
        Ok(view)
    }

    /// Re-project one terminal compatibility tree into its stable generic tab.
    /// Existing session→view associations win; a just-created terminal leaf may
    /// consume one unreferenced matching entry from the process `ViewStore`.
    pub(crate) fn sync_tab_model_from_layout(&mut self, wid: WindowId, index: usize) -> bool {
        let Some((layout, tab_id, old_views)) = self.windows.get(&wid).and_then(|ws| {
            let tab_id = terminal_tab_id_at(&ws.tab_set, &self.view_store, index)?;
            Some((
                ws.layouts.get(index)?.clone(),
                tab_id,
                ws.tab_set.get(tab_id)?.root.leaves(),
            ))
        }) else {
            return false;
        };

        let mut by_session = std::collections::HashMap::new();
        let mut used = std::collections::HashSet::new();
        for view in old_views {
            if let Some(session) = self
                .view_store
                .get(view)
                .copied()
                .and_then(crate::tab_model::View::terminal_session)
            {
                by_session.insert(session, view);
                used.insert(view);
            }
        }
        for session in layout.sessions() {
            if by_session.contains_key(&session) {
                continue;
            }
            let Some(view) = self.view_store.iter().find_map(|(id, view)| {
                (!used.contains(&id) && view.terminal_session() == Some(session)).then_some(id)
            }) else {
                return false;
            };
            by_session.insert(session, view);
            used.insert(view);
        }
        let root = layout.map_sessions(|session| by_session[&session]);
        let Some(focus) = by_session.get(&layout.focus()).copied() else {
            return false;
        };
        let Some(tab) = self.windows.get_mut(&wid).and_then(|ws| {
            let canonical = ws
                .tab_set
                .tabs()
                .iter()
                .position(|candidate| candidate.id == tab_id)?;
            ws.tab_set.tab_at_mut(canonical)
        }) else {
            return false;
        };
        tab.root = root;
        tab.focus = focus;
        tab.zoomed = layout.is_zoomed();
        true
    }

    /// Remove one stable view edge and its native reducer state, if any. The
    /// native instance is retired only when its final view disappears; shared
    /// Settings/document instances therefore survive ordinary view closes.
    pub(crate) fn remove_view_link(
        &mut self,
        view: crate::tab_model::ViewId,
    ) -> Option<crate::tab_model::View> {
        let linked = self.view_store.get(view).copied()?;
        if let crate::tab_model::View::Native(native) = linked
            && let Some(document) = self.native_runtime.document_id(native.instance)
            && self
                .document_store
                .detach_view_if_ready(document, crate::document_store::DocumentViewId(view.get()))
                .is_err()
        {
            // Fail closed. The exact view/runtime/document edges remain intact;
            // dirty final views may only cross this seam after their close plan
            // proves the requested sequence durable.
            return None;
        }
        let removed = self.view_store.remove(view)?;
        // `ViewId` is process-unique, so retiring its stable ownership edge also
        // retires every retained raster keyed by that identity.  Usually there is
        // exactly one such entry.  Scanning all windows additionally cleans a
        // source-window cache left behind by a tab migration before the next paint.
        for window in self.windows.values_mut() {
            window.leaf_render_cache.remove(&view);
        }
        if let crate::tab_model::View::Native(native) = removed {
            self.native_runtime.remove_view(view);
            let still_referenced = self.view_store.iter().any(|(_, candidate)| {
                matches!(
                    candidate,
                    crate::tab_model::View::Native(other) if other.instance == native.instance
                )
            });
            if !still_referenced {
                self.native_runtime.remove_instance(native.instance);
                self.refresh_disambiguated_document_titles();
            }
        }
        Some(removed)
    }

    pub(crate) fn remove_tab_views(&mut self, tab: &crate::tab_model::Tab) {
        for view in tab.root.leaves() {
            self.remove_view_link(view);
        }
    }

    /// Whether the visible tab strip is enabled (`tab_strip_rows > 0`). GLOBAL. The
    /// whole strip path (splice + paint + hit-test) is gated on this; `false` is the
    /// byte-identical no-strip path.
    pub(crate) fn tab_strip_enabled(&self) -> bool {
        self.tab_strip_rows > 0
    }

    /// One title per TAB (top-level) of window `wid`, for the strip labels: each
    /// tab's label is its presentation title for native content, or — for terminal
    /// content — the session's USER title (`meta set title`, the operator's name
    /// for the session, which OUTRANKS everything below), then the focused pane's
    /// live terminal title (OSC 0/2), falling back to the session's shell-reported
    /// cwd (OSC 7 / OSC 633 `P;Cwd=`, `$HOME`-abbreviated to `~…` exactly like
    /// the zsh integration's OSC 0 form) when no program has set a title. A
    /// terminal tab with none of those falls back to the tab presentation and
    /// finally `"aterm"`; native tabs keep their presentation title untouched.
    /// [`Self::refill_strip_titles`] is the in-place mirror of this fallback
    /// chain and MUST stay byte-identical whenever the try-lock wins.
    ///
    /// TRY-LOCK ONLY (the `refill_strip_titles` discipline): this runs on the main
    /// thread — from `sync_window` on every tab mutation, including the tab SWITCH
    /// itself, and from the `Wake::Output` title-drift arm during background floods.
    /// A blocking `term_lock` here parks the UI behind whichever session's reader
    /// thread is mid-`process()` — under sustained PTY flood that mutex is held
    /// back-to-back and tab switching froze until the flooding program quit. On
    /// contention the per-window `tab_title_cache` supplies the last-read label
    /// (self-correcting: the title-drift epoch handler re-runs this on the next
    /// output wake), and a never-read session falls back to the presentation title.
    /// Title and cwd are read under the ONE term try-lock per tab. (The user-title
    /// read is a separate LEAF mutex on the session ctx, taken-and-dropped BEFORE
    /// the term lock; it is contended only by an actual `meta set`, and when it
    /// hits, the term lock is skipped entirely.)
    pub(crate) fn tab_titles(&mut self, wid: WindowId) -> Vec<String> {
        let Some(ws) = self.windows.get(&wid) else {
            return Vec::new();
        };
        // Pass 1 (immutable borrows): resolve each tab to its fallback plus, for
        // terminal tabs, the session id and a freshly resolved label (`None` on
        // contention, `Some` even when empty — an empty live label must MISS the
        // cache and fall back, not resurrect a stale one). The label chain is the
        // byte-identical twin of `refill_strip_titles`: user title, then (under
        // the term try-lock) the live OSC title, then the `~`-abbreviated cwd.
        let read: Vec<_> = ws
            .tab_set
            .tabs()
            .iter()
            .map(|tab| {
                let fallback = if !tab.presentation.title.is_empty() {
                    tab.presentation.title.clone()
                } else {
                    "aterm".to_string()
                };
                let Some(crate::tab_model::View::Terminal(view)) =
                    self.view_store.get(tab.focus).copied()
                else {
                    return (fallback, None, None, None);
                };
                let Some(s) = self.pool.get(view.session) else {
                    return (fallback, None, None, None);
                };
                // TOP RUNG: the operator's `meta set title` (leaf lock, dropped
                // before the term lock is ever taken; contended only by an actual
                // `meta set`). When it hits, the term try-lock is skipped.
                let (user_title, authored_description) = {
                    let meta = s.ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
                    (
                        meta.presentation_value("title"),
                        meta.presentation_value("description"),
                    )
                };
                if user_title.is_some() {
                    return (
                        fallback,
                        Some(view.session),
                        user_title,
                        authored_description,
                    );
                }
                let term_label = |t: &aterm_core::terminal::Terminal| {
                    use crate::cwd_native::ReportedCwd as _;
                    // The cwd rung is a LABEL: it must read as a path on this
                    // platform, so it goes through the native conversion rather
                    // than showing the engine's RFC 8089 `/C:/…` URI path.
                    let cwd = t.native_working_directory();
                    resolved_terminal_title_rung(None, t.title(), cwd.as_deref())
                        .unwrap_or_default()
                };
                let fresh = match s.term.try_lock() {
                    Ok(t) => Some(term_label(&t)),
                    // Poisoned ⇒ recover the guard exactly like `term_lock`.
                    Err(std::sync::TryLockError::Poisoned(p)) => Some(term_label(&p.into_inner())),
                    Err(std::sync::TryLockError::WouldBlock) => None,
                };
                (fallback, Some(view.session), fresh, authored_description)
            })
            .collect();
        // Pass 2: fold through the per-window keep-stale cache, pruned to the
        // sessions that still label a tab so it stays bounded.
        let title_summaries = &self.title_summaries;
        let title_config = &self.config;
        let title_format = title_config.tab_title_format_or_default();
        let Some(ws) = self.windows.get_mut(&wid) else {
            return Vec::new();
        };
        let live: std::collections::HashSet<u64> =
            read.iter().filter_map(|(_, sid, _, _)| *sid).collect();
        ws.tab_title_cache.retain(|sid, _| live.contains(sid));
        read.into_iter()
            .map(|(fallback, sid, fresh, authored_description)| {
                let Some(sid) = sid else {
                    return fallback;
                };
                let raw = match fresh {
                    Some(label) => {
                        if label.is_empty() {
                            ws.tab_title_cache.remove(&sid);
                            fallback
                        } else {
                            ws.tab_title_cache.insert(sid, label.clone());
                            label
                        }
                    }
                    None => ws
                        .tab_title_cache
                        .get(&sid)
                        .filter(|cached| !cached.is_empty())
                        .cloned()
                        .unwrap_or(fallback),
                };
                title_summaries.compose(
                    Some(sid),
                    &raw,
                    authored_description.as_deref(),
                    title_format,
                    title_config,
                    " · ",
                )
            })
            .collect()
    }

    /// Canonical, non-title chrome metadata paired by index with [`Self::tab_titles`].
    /// The presentation is the stable tab model's own value, never inferred from a
    /// title, PTY, URI, or native reducer state at paint time.
    pub(crate) fn tab_strip_metadata(
        &self,
        wid: WindowId,
    ) -> Vec<crate::tab_bar::TabStripMetadata> {
        self.windows.get(&wid).map_or_else(Vec::new, |ws| {
            ws.tab_set
                .tabs()
                .iter()
                .map(|tab| crate::tab_bar::TabStripMetadata::from_presentation(&tab.presentation))
                .collect()
        })
    }

    /// The per-tab chrome EXTENSIONS (hover tooltip + right-click context-menu
    /// model, session-metadata stage 2) paired by index with
    /// [`Self::tab_titles`] — `titles` MUST be that same freshly-computed `Vec`
    /// (the composed identity line opens with the exact chip label). Terminal
    /// tabs compose from live session facts via the epoch-gated
    /// [`Self::composed_session_chrome`]; native tabs pass their presentation
    /// tooltip through unchanged (their menus stay empty — native surfaces own
    /// richer affordances of their own). The composed tooltip is ALSO written
    /// back onto the tab's [`crate::tab_model::TabPresentation::tooltip`], the
    /// one cross-platform field every presentation consumer already reads —
    /// terminal tabs stop being the only kind whose tooltip slot stays `None`.
    pub(crate) fn tab_chrome_ext(
        &mut self,
        wid: WindowId,
        titles: &[String],
    ) -> Vec<crate::session_chrome::TabChromeExt> {
        // Bound the cache: sessions close over a long process life, and nothing
        // else prunes their composed chrome. Rare (only past the bound), and the
        // live-id set is collected FIRST so the retain closure borrows no field.
        if self.session_chrome.len() > 64 {
            let live: std::collections::HashSet<u64> = self.pool.iter().map(|s| s.id).collect();
            self.session_chrome.retain(|sid, _| live.contains(sid));
        }
        // Snapshot per-tab identity (immutable borrow of the window ends here).
        let tabs: Vec<(Option<u64>, Option<String>)> =
            self.windows.get(&wid).map_or_else(Vec::new, |ws| {
                ws.tab_set
                    .tabs()
                    .iter()
                    .map(|tab| {
                        let session = self
                            .view_store
                            .get(tab.focus)
                            .copied()
                            .and_then(crate::tab_model::View::terminal_session);
                        (session, tab.presentation.tooltip.clone())
                    })
                    .collect()
            });
        let ext: Vec<crate::session_chrome::TabChromeExt> = tabs
            .iter()
            .enumerate()
            .map(|(i, (session, presentation_tooltip))| match session {
                Some(session) => {
                    let label = titles.get(i).map_or("", String::as_str);
                    self.composed_session_chrome(wid, *session, label)
                }
                None => crate::session_chrome::TabChromeExt {
                    tooltip: presentation_tooltip.clone(),
                    menu: Vec::new(),
                },
            })
            .collect();
        // Feed the composed tooltip through the presentation (terminal tabs
        // only — native tabs already own theirs). Change-gated: a steady prompt
        // must not dirty the tab model every refresh.
        if let Some(ws) = self.windows.get_mut(&wid) {
            for (i, ((session, _), ext)) in tabs.iter().zip(&ext).enumerate() {
                if session.is_some()
                    && let Some(tab) = ws.tab_set.tab_at_mut(i)
                    && tab.presentation.tooltip != ext.tooltip
                {
                    tab.presentation.tooltip = ext.tooltip.clone();
                }
            }
        }
        ext
    }

    /// Advance one expired session's fan-out by at most one real window scan.
    /// The cache entry is removed on admission; the first consuming window
    /// recomposes it, and later consuming windows reuse the fresh value while
    /// receiving it in their native toolbar. The `(session, after_window)`
    /// cursor retains all remaining fan-out work across event-loop turns.
    pub(crate) fn advance_session_chrome_expiry(
        &mut self,
        now_ms: u64,
    ) -> crate::session_chrome::ExpiryProgress {
        use crate::session_chrome as chrome;
        use std::ops::Bound::{Excluded, Unbounded};

        let mut progress = chrome::ExpiryProgress::default();
        if !self.session_chrome_expiry.pending() {
            let Some(session) = chrome::due_cache_batch(&self.session_chrome, now_ms, 1)
                .into_iter()
                .next()
            else {
                return progress;
            };
            self.session_chrome.remove(&session);
            self.session_chrome_expiry.session = Some(session);
            self.session_chrome_expiry.after_window = None;
            progress.admitted_session = Some(session);
        }

        for _ in 0..chrome::EXPIRY_WINDOW_SCAN_BUDGET {
            let Some(session) = self.session_chrome_expiry.session else {
                break;
            };
            let next_window = match self.session_chrome_expiry.after_window {
                Some(after) => self
                    .windows
                    .range((Excluded(after), Unbounded))
                    .next()
                    .map(|(window, _)| *window),
                None => self.windows.keys().next().copied(),
            };
            let Some(window) = next_window else {
                self.session_chrome_expiry.session = None;
                self.session_chrome_expiry.after_window = None;
                progress.completed_session = Some(session);
                break;
            };
            self.session_chrome_expiry.after_window = Some(window);
            progress.window_scans += 1;

            let consumes_session = self.windows.get(&window).is_some_and(|state| {
                state.tab_set.tabs().iter().any(|tab| {
                    self.view_store
                        .get(tab.focus)
                        .copied()
                        .and_then(crate::tab_model::View::terminal_session)
                        == Some(session)
                })
            });
            if consumes_session {
                self.refresh_window_tabs(window);
                progress.window_refreshes += 1;
                if let Some(os_window) = self
                    .windows
                    .get(&window)
                    .and_then(|state| state.os_window.as_ref())
                {
                    os_window.request_redraw();
                }
            }

            let has_more_windows = self
                .windows
                .range((Excluded(window), Unbounded))
                .next()
                .is_some();
            if !has_more_windows {
                self.session_chrome_expiry.session = None;
                self.session_chrome_expiry.after_window = None;
                progress.completed_session = Some(session);
            }
        }
        debug_assert!(progress.window_scans <= chrome::EXPIRY_WINDOW_SCAN_BUDGET);
        debug_assert!(progress.window_refreshes <= progress.window_scans);
        progress
    }

    /// Compose (or reuse) one terminal session's chrome extension — the impure
    /// half of [`crate::session_chrome`]: gather the facts under their LEAF
    /// locks, hand them to the pure composer, cache the result by input epoch.
    ///
    /// EPOCH GATE (the "cheap" contract): the cache entry is reused while
    /// (a) the session timeline's high-water id is unchanged — every composed
    /// fact records a timeline event (`meta-change` / `cwd-change` /
    /// `state-change` / `spawned`), so an unmoved id proves unmoved facts;
    /// (b) the resolved label is byte-identical — a BACKGROUND tab's OSC title
    /// can drift without a timeline record (only the active session's title is
    /// published to the store), so the label is its own epoch; and
    /// (c) the generated-activity revision is unchanged — summaries update
    /// independently of the durable session timeline and selected label. Age
    /// expiry is owned by the event-loop scheduler, which removes a fixed-size
    /// due batch before refreshing its windows; keeping age out of this local
    /// hit gate prevents one all-tab refresh from recomposing an unbounded
    /// synchronized backlog. On a hit this costs ONE leaf-lock read (the
    /// timeline high id); the full sweep (meta + term + store-read + timeline
    /// tail) runs only on an actual input change or scheduled expiry.
    ///
    /// LOCK ORDER: strictly SEQUENTIAL leaves — timeline, meta, term, store
    /// read, timeline again — each taken and dropped before the next, never
    /// nested, mirroring `control_session::meta_status`. The terminal leaf is a
    /// try-lock: contention serves stale cached chrome and schedules the existing
    /// title-drift tail retry, so summary completion never parks the event loop.
    fn composed_session_chrome(
        &mut self,
        retry_window: WindowId,
        session: u64,
        label: &str,
    ) -> crate::session_chrome::TabChromeExt {
        use crate::session_chrome as chrome;
        let Some((term, ctx)) = self
            .pool
            .get(session)
            .map(|session| (session.term.clone(), session.ctx.clone()))
        else {
            return chrome::TabChromeExt::default();
        };
        // Tab Subject & Status owns the live activity line when it has anything
        // honest to say; the older generated summary remains the fallback so
        // nothing regresses while the two subsystems coexist (RFC §11).
        let status_text = self.session_status_text(session);
        let activity_revision = self
            .title_summary_activity_revision(session)
            .wrapping_add(self.session_status_revision(session));
        let activity =
            status_text.or_else(|| self.title_summary_activity(session).map(str::to_owned));
        let high_id = ctx
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .high_id()
            .unwrap_or(0);
        let now = crate::turn_ledger::now_ms();
        if let Some(c) = self.session_chrome.get(&session)
            && c.high_id == high_id
            && c.label == label
            && c.activity_revision == activity_revision
        {
            return c.ext.clone();
        }
        let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner()).clone();
        // The tab TOOLTIP's `cwd: …` line, so it takes the native form — the
        // reported defect showed `cwd: /C:/Users//m6-an` here.
        let native_cwd = |t: &aterm_core::terminal::Terminal| {
            use crate::cwd_native::ReportedCwd as _;
            t.native_working_directory()
                .filter(|c| !c.is_empty())
                .map(|c| c.into_owned())
        };
        let cwd = match term.try_lock() {
            Ok(term) => native_cwd(&term),
            // Recover poison exactly like `term_lock`; poison does not imply
            // another thread currently owns the mutex.
            Err(std::sync::TryLockError::Poisoned(poisoned)) => native_cwd(&poisoned.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => {
                let stale = self
                    .session_chrome
                    .get(&session)
                    .map(|cached| cached.ext.clone())
                    .unwrap_or_default();
                // Reuse the already deadline-driven, bounded title/cwd retry lane.
                // Do not advance the chrome cache epochs: the retry must miss and
                // recompose with a consistent terminal snapshot once available.
                if let Some(window) = self.windows.get_mut(&retry_window) {
                    window.tab_title_epochs.remove(&session);
                }
                self.session_chrome_retry.insert(session);
                self.title_drift.pending.insert(session);
                return stale;
            }
        };
        let (state, has_session) = {
            let g = self.store.read().unwrap_or_else(|p| p.into_inner());
            match g.by_local(session) {
                Some(h) => (Some(h.state.as_str().to_string()), true),
                None => (None, false),
            }
        };
        let timeline: Vec<chrome::TimelineNote> = {
            let tl = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            // Walk the retained deque BACKWARDS and stop after the tail we keep:
            // `since(None)`'s filter is a no-op, so this yields exactly what
            // collecting all ~512 events and then doing `.rev().take(TAIL)` did —
            // same notes, same newest-first order the tooltip/menu contract wants
            // — while holding the leaf mutex for five steps instead of a full walk.
            // `now` stays captured outside the lock, so the ages are unchanged too.
            tl.since(None)
                .rev()
                .take(chrome::TIMELINE_TAIL)
                .map(|e| chrome::TimelineNote {
                    kind: e.kind,
                    age_ms: now.saturating_sub(e.t_ms),
                })
                .collect()
        };
        let can_rename = self.can_rename_session(retry_window);
        let input = chrome::SessionChromeInput {
            can_rename,
            label: label.to_string(),
            icon: meta.presentation_value("icon"),
            description: meta.presentation_value("description"),
            activity,
            cwd,
            home: cached_home().map(str::to_string),
            state,
            has_session,
            timeline,
        };
        let ext = chrome::TabChromeExt {
            tooltip: chrome::compose_tooltip(&input),
            menu: chrome::compose_tab_menu(&input),
        };
        self.session_chrome.insert(
            session,
            chrome::CachedChrome {
                high_id,
                label: label.to_string(),
                activity_revision,
                composed_ms: now,
                ext: ext.clone(),
            },
        );
        ext
    }

    /// Full hover/accessibility context paired by index with [`Self::tab_titles`].
    /// Settings uses this for the active route (for example
    /// `Settings · Cursor & Motion`), while documents retain their full URI/path even
    /// when the compact title chip truncates. Kept separate from `TabStripMetadata` so
    /// the every-frame semantic paint fingerprint remains allocation-free and `Copy`.
    /// Read AFTER [`Self::tab_chrome_ext`] on the refresh path, so terminal tabs
    /// report the freshly-composed session tooltip it wrote back onto the
    /// presentation.
    pub(crate) fn tab_tooltips(&self, wid: WindowId) -> Vec<Option<String>> {
        self.windows.get(&wid).map_or_else(Vec::new, |ws| {
            ws.tab_set
                .tabs()
                .iter()
                .map(|tab| tab.presentation.tooltip.clone())
                .collect()
        })
    }

    /// A cheap fingerprint of the VISIBLE tab strip — tab count, active index, and a
    /// hash of every tab's title — folded into the redraw [`RepaintKey`] so a tab
    /// switch / open / close / title change repaints the strip even when the terminal
    /// grid below is unchanged. Always `0` when the strip is disabled, keeping the
    /// key byte-identical to the pre-strip path. Computed from ALREADY-READ titles —
    /// no extra term locks: the redraw hot path reads the per-tab titles ONCE
    /// (`tab_titles`) and feeds the SAME `Vec` to both this and `splice_tab_strip_with`,
    /// instead of locking every tab's terminal twice per present (once to hash, once
    /// to paint).
    /// Byte-identical to hashing `tab_titles(wid)`: same count + active + title bytes.
    /// Full shipping fingerprint: live titles plus canonical icon/dirty/closable
    /// metadata. A save, app-kind change, or close-policy change therefore invalidates
    /// the cached strip even when its visible title stays byte-identical.
    ///
    /// It also carries `wid`'s live IN-GRID rename field. That field is painted INTO
    /// the strip, so unless its text and caret move this number, two independent
    /// caches swallow every keystroke: the RepaintKey early-out (this value) and the
    /// painted-row cache (`last_strip_fp`, which keys on this value). A NATIVE edit
    /// is deliberately absent — AppKit paints that one, and folding it in would
    /// repaint the in-grid strip for a field that is not on it.
    pub(crate) fn tab_strip_fingerprint_from_parts(
        &self,
        wid: WindowId,
        titles: &[String],
        metadata: &[crate::tab_bar::TabStripMetadata],
        active: usize,
    ) -> u64 {
        if !self.tab_strip_enabled() {
            return 0;
        }
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        titles.len().hash(&mut h);
        active.hash(&mut h);
        for t in titles {
            t.hash(&mut h);
        }
        metadata.hash(&mut h);
        if let Some(edit) = self.inline_rename_edit(wid) {
            edit.session.hash(&mut h);
            edit.tab.hash(&mut h);
            edit.text.hash(&mut h);
            edit.cursor.hash(&mut h);
            // IME-2: while the rename field owns an in-flight composition, the
            // strip's rename well paints the preedit spliced into the display
            // text (`splice_tab_strip_with`) — its bytes + caret are part of
            // what the rows show, so they must move this fingerprint or the
            // strip row cache (and the RepaintKey's strip term) would keep
            // serving pre-composition rows while a CJK user types a tab name.
            // Hashed only under a live rename edit: with no rename field the
            // preedit never reaches the strip, and the fingerprint stays
            // byte-identical to the pre-IME value.
            if let Some(ws) = self.windows.get(&wid)
                && !ws.preedit.is_empty()
            {
                ws.preedit.hash(&mut h);
                ws.preedit_caret.hash(&mut h);
            }
        }
        // Never collide with the disabled-strip sentinel (0): a real strip always
        // sets at least bit 0, so a zero-hash strip still forces the first repaint.
        h.finish() | 1
    }

    /// "Move Tab to New Window" (Cmd-Shift-N / Window ▸ Move Tab to New Window): pull
    /// the frontmost window's ACTIVE tab OUT into a brand-new in-process window. The
    /// view MOVES — the existing `Session` is never spawned, dropped, or duplicated
    /// (the pool's view-count stays 1), so there is zero PTY churn. This is the
    /// logical half: it does everything EXCEPT attach the OS surface, returning the
    /// new window's id (or `None` if the move was refused), so it is headless-testable.
    ///
    /// Refused (returns `None`) when the source window has only ONE tab — detaching
    /// the sole tab would just relocate the window, a no-op.
    pub(crate) fn detach_active_tab_logical(&mut self) -> Option<WindowId> {
        let wid_a = self.frontmost_window?;
        // Canonical identity chooses the tab. A terminal tab additionally owns one
        // compatibility projection entry; a native tab owns none.
        let (stable_tab, terminal_index, tree, rows, cols) = {
            let ws = self.windows.get(&wid_a)?;
            if ws.tab_set.len() <= 1 {
                return None;
            }
            let stable_tab = ws.tab_set.active()?.clone();
            let terminal_index =
                terminal_projection_index(&ws.tab_set, &self.view_store, stable_tab.id);
            let tree = match terminal_index {
                Some(index) => Some(ws.layouts.get(index)?.clone()),
                None => None,
            };
            (stable_tab, terminal_index, tree, ws.rows, ws.cols)
        };
        // Resolve the moved tab's canonical focused leaf independently of the
        // all-terminal compatibility projection. This preserves a terminal-
        // focused heterogeneous tab while a native-focused/native-only tab owns
        // no placeholder terminal capability.
        let focused_view = stable_tab.focus;
        let (front_content, active_terminal) = match self.view_store.get(focused_view).copied()? {
            crate::tab_model::View::Terminal(terminal) => {
                let session = self.pool.get(terminal.session)?;
                (
                    Some(crate::front_content::FrontContent::Terminal {
                        view: focused_view,
                        session: terminal.session,
                    }),
                    Some(crate::front_content::TerminalMirror {
                        session: terminal.session,
                        term: session.term.clone(),
                        master: session.master,
                        sink: session.ctx.sink.clone(),
                    }),
                )
            }
            crate::tab_model::View::Native(native) => (
                Some(crate::front_content::FrontContent::Native {
                    instance: native.instance,
                    view: focused_view,
                }),
                None,
            ),
        };
        // Remove the whole canonical tab from A and, only for terminal content,
        // its matching compatibility tree. NO `pool.detach`: every stable view
        // moves to B and every live session keeps the same view count.
        if let Some(ws) = self.windows.get_mut(&wid_a) {
            if let Some(index) = terminal_index {
                ws.layouts.remove(index);
                remove_terminal_projection(ws, index);
            }
            ws.tab_set.remove(stable_tab.id);
            align_terminal_projection_to_active(ws, &self.view_store);
        }
        // Build window B holding the EXISTING stable tab. Its terminal projection
        // is present iff the moved tab itself is terminal.
        let wid_b = WindowId(self.next_window_id);
        self.next_window_id += 1;
        let (tabs, layouts) = match tree {
            Some(tree) => (TabIndex::new(0, 1), vec![tree]),
            None => (TabIndex::new(0, 0), Vec::new()),
        };
        let metrics = self.unattached_window_metrics();
        let ws_b = WindowState::new_with_front(
            front_content,
            active_terminal,
            rows,
            cols,
            metrics,
            tabs,
            layouts,
            crate::tab_model::TabSet::new(stable_tab),
        );
        self.windows.insert(wid_b, ws_b);
        self.frontmost_window = Some(wid_b);
        // Re-mirror BOTH: A's active tab changed (it lost its old active), and B is
        // the new frontmost (also re-points the global control/notify handle to B).
        // A moved terminal's reader thread remains stamped with old window A, but
        // output routing resolves its live view and therefore finds B. Native
        // content has no reader/PTY stamp at all.
        self.sync_window(wid_a);
        self.sync_active_session(); // frontmost = B
        debug_assert!(self.structural_invariants_ok());
        Some(wid_b)
    }

    /// Full "Move Tab to New Window": the logical move + (when not headless) the
    /// winit OS-window attach for the new window. A refused move (single-tab source)
    /// is a silent no-op.
    pub(crate) fn detach_active_tab(&mut self, el: &ActiveEventLoop) {
        // Capture the SOURCE window BEFORE the move (the logical step re-points
        // frontmost to the new window), so a rollback can return the tab to it.
        let wid_a = self.frontmost_window;
        let Some(wid_b) = self.detach_active_tab_logical() else {
            return;
        };
        if !self.headless && !self.attach_os_window(el, wid_b) {
            self.detach_rollback_logical(wid_a, wid_b);
        }
    }

    /// Undo a `detach_active_tab_logical` when the new window's OS surface failed
    /// (el-free). Detach is a PURE view-move (no `pool.attach`/`detach`), so the
    /// moved session is window B's SOLE view; `close_window_logical(B)` would detach
    /// it (views 1→0) and DESTROY the live shell. Instead REVERSE the move: return
    /// the tab's pane tree to source window A (no pool churn → the session survives),
    /// then drop the empty, never-shown B. (Contrast the share/create rollbacks,
    /// where `close_window_logical` is correct: the shared view survives at 2→1, and
    /// a fresh window's brand-new session has no other home.)
    pub(crate) fn detach_rollback_logical(&mut self, wid_a: Option<WindowId>, wid_b: WindowId) {
        let returned = self.windows.remove(&wid_b).and_then(|ws_b| {
            Some((
                ws_b.layouts.into_iter().next(),
                ws_b.tab_set.active()?.clone(),
            ))
        });
        if let (Some((tree, tab)), Some(ws_a)) =
            (returned, wid_a.and_then(|a| self.windows.get_mut(&a)))
        {
            if is_terminal_tab(&tab, &self.view_store) {
                let tree = tree.expect("detached terminal tab keeps its PaneTree projection");
                ws_a.layouts.push(tree);
                ws_a.tabs.add();
            } else {
                debug_assert!(tree.is_none());
            }
            ws_a.tab_set
                .push(tab)
                .expect("returned tab id remains unique");
            align_terminal_projection_to_active(ws_a, &self.view_store);
        }
        self.winit_to_window.retain(|_, &mut v| v != wid_b);
        self.focus_order.retain(|w| *w != wid_b);
        // Drop B's native toolbar handle too (matching close_window_logical), so a
        // failed Move-Tab-to-New-Window doesn't leak a retained AppKit ToolbarHandle for
        // the never-shown window.
        self._toolbars.remove(&wid_b);
        self.frontmost_window = wid_a;
        self.sync_active_session();
    }

    /// "Move Tab to Next Window" (Cmd-Shift-M / Window ▸ Move Tab to Next Window): move
    /// the frontmost window's ACTIVE tab into the NEXT EXISTING window (BTreeMap id
    /// order, wrapping to the first), and follow it there (the destination becomes
    /// frontmost). Unlike `detach_active_tab` — which MOVES the view into a BRAND-NEW
    /// window — this targets an EXISTING window, so it never attaches a winit OS
    /// surface and needs no `ActiveEventLoop`: it is fully headless-safe and the
    /// keyboard/menu paths call it directly (no `Wake` round-trip).
    ///
    /// It is a PURE view-move: the `Session` is never spawned, dropped, or duplicated,
    /// so the pool's view-count stays unchanged (zero PTY churn). If the source window
    /// held ONLY that one tab it becomes empty and is CLOSED — a "merge the source's
    /// last tab into the next window". A no-op with fewer than two windows (nowhere to
    /// move the tab).
    pub(crate) fn migrate_active_tab_to_next_window(&mut self) {
        let Some(wid_a) = self.frontmost_window else {
            return;
        };
        // The next window after A in id order, wrapping to the first. Every window is
        // now a normal mixed-tab host; there is no Settings-only accessory kind.
        let dest = self
            .windows
            .range((std::ops::Bound::Excluded(wid_a), std::ops::Bound::Unbounded))
            .next()
            .map(|(k, _)| *k)
            .or_else(|| self.windows.keys().next().copied());
        let Some(wid_b) = dest else { return };
        if wid_b == wid_a {
            return; // never move a tab onto its own window (also: sole terminal window)
        }
        // Pull A's canonical active tab. A terminal tab additionally moves its
        // compatibility PaneTree; native content never acquires a sentinel tree.
        let (stable_tab, terminal_index, tree) = {
            let Some(ws) = self.windows.get(&wid_a) else {
                return;
            };
            let Some(stable_tab) = ws.tab_set.active().cloned() else {
                return;
            };
            let terminal_index =
                terminal_projection_index(&ws.tab_set, &self.view_store, stable_tab.id);
            let tree = terminal_index.and_then(|index| ws.layouts.get(index).cloned());
            if terminal_index.is_some() && tree.is_none() {
                return;
            }
            (stable_tab, terminal_index, tree)
        };
        // Whether A will be canonically empty after the move.
        let source_now_empty = self
            .windows
            .get(&wid_a)
            .is_some_and(|ws| ws.tab_set.len() == 1);
        if let Some(ws) = self.windows.get_mut(&wid_a) {
            if let Some(index) = terminal_index {
                ws.layouts.remove(index);
                remove_terminal_projection(ws, index);
            }
            ws.tab_set.remove(stable_tab.id);
            align_terminal_projection_to_active(ws, &self.view_store);
        }
        // Append the EXISTING stable tab to B. Only terminal content extends the
        // compatibility projection. No pool count changes: the view moved.
        if let Some(ws) = self.windows.get_mut(&wid_b) {
            if let Some(tree) = tree.as_ref() {
                ws.layouts.push(tree.clone());
                ws.tabs.add();
            }
            ws.tab_set
                .push(stable_tab)
                .expect("moved tab id is process-unique");
            align_terminal_projection_to_active(ws, &self.view_store);
        }
        // Focus follows the moved tab: the destination becomes frontmost.
        self.frontmost_window = Some(wid_b);
        // Resize the moved panes to B's grid: a migrate to a DIFFERENT-sized window
        // must SIGWINCH the moved panes' engines + PTYs to B's cell geometry, or they
        // keep A's stale grid (no reflow, no SIGWINCH). `resize_panes` no-ops per pane
        // when the dims already match (so it's free when A and B are the same size)
        // and re-lays + SIGWINCHes otherwise — mirroring how `apply_close_outcome`
        // pairs `resize_panes(wid)` with `sync_window(wid)`.
        if tree.is_some() {
            self.resize_panes(wid_b);
        }
        // Re-mirror B onto the moved canonical tab. Terminal output routing follows
        // the moved view despite its old window stamp; native content has no PTY
        // reader stamp.
        self.sync_window(wid_b);
        if source_now_empty {
            // A has no canonical tabs left. Close it before the next stable-state
            // assertion. The moved tab is already absent from A, so window teardown
            // detaches nothing and cannot double-drop its views.
            let _ = self.close_window_logical(wid_a);
        } else {
            // A survives with its remaining tabs: re-mirror its clamped active tab.
            self.sync_window(wid_a);
        }
        // Frontmost = B: re-point the global control/notify handle onto B's active tab.
        self.sync_active_session();
        // Raise B's OS surface so macOS KEY focus follows the logical frontmost. Without
        // it, when the SOURCE window survives (had >1 tab) it keeps OS key focus while B
        // is logical-frontmost — typed keys route to the still-focused A while Cmd-W /
        // Cmd-T act on the unseen B (and can even arm the foreground-job confirm in B's
        // invisible titlebar). No-op headless (no os_window); the follow-on
        // WindowEvent::Focused(true) reconciles with the already-set frontmost.
        if let Some(w) = self.windows.get(&wid_b).and_then(|ws| ws.os_window.clone()) {
            w.focus_window();
        }
        debug_assert!(
            self.structural_invariants_ok(),
            "window/session structural invariants violated after migrate_active_tab_to_next_window",
        );
    }

    /// "Open Active Session in New Window" (Cmd-Shift-O / Window ▸ Open Session in New
    /// Window): show the frontmost window's ACTIVE session in a SECOND window, so the
    /// same live terminal grid is visible in two windows at once ("watch a log in one,
    /// type in another"). Unlike `detach_active_tab` this ADDS a view rather than
    /// MOVING one: the source window keeps its tab, and a fresh window is built viewing
    /// the SAME pooled session (no spawn). The pool's view-count goes 1→2, so the PTY
    /// stays open until BOTH viewers detach (each `close_window_logical` of a viewing
    /// tab drops one view); the `pool.attach` here is paired with exactly one future
    /// `pool.detach`. This is the logical half (everything EXCEPT the OS-window attach),
    /// returning the new window's id (or `None` if no session is in view), so it is
    /// headless-testable.
    pub(crate) fn open_active_session_in_new_window_logical(&mut self) -> Option<WindowId> {
        let wid_a = self.frontmost_window?;
        // Sharing is terminal-only. Native focus has no PTY-backed capability,
        // including inside a heterogeneous tab.
        let s = self.front_terminal(wid_a)?.session;
        // Share the FOCUSED pane's session as a fresh SINGLE-PANE tab in B. A
        // shared (views>1) session is always a full single-pane tab on each side —
        // it is never split (split-spawned panes are always views=1), so B holds a
        // single-leaf pane tree on the focused session.
        let (rows, cols) = self.windows.get(&wid_a).map(|ws| (ws.rows, ws.cols))?;
        // Bump the view count: the session is now displayed by TWO windows. The PTY
        // stays open until BOTH detach (views back to 0).
        self.pool.attach(s);
        // Build window B viewing the SAME pooled session (no spawn). Clone the mirror
        // Arcs from the pool.
        let Some(sess) = self.pool.get(s) else {
            self.detach_session_view(s); // unwind the attach on the impossible miss
            return None;
        };
        let (term, master, sink) = (sess.term.clone(), sess.master, sess.ctx.sink.clone());
        let wid_b = WindowId(self.next_window_id);
        self.next_window_id += 1;
        let layout = pane::PaneTree::new(s);
        let tab =
            match crate::register_terminal_tab(&mut self.tab_ids, &mut self.view_store, &layout) {
                Ok(tab) => tab,
                Err(_) => {
                    self.detach_session_view(s);
                    return None;
                }
            };
        let metrics = self.unattached_window_metrics();
        let ws_b = WindowState::new_terminal(
            term,
            master,
            sink,
            s,
            rows,
            cols,
            metrics,
            TabIndex::new(0, 1),
            vec![layout],
            crate::tab_model::TabSet::new(tab),
        );
        self.windows.insert(wid_b, ws_b);
        self.install_window_config_assets(wid_b);
        self.frontmost_window = Some(wid_b);
        // Re-mirror BOTH viewers: B is the new frontmost (also re-points the global
        // control/notify handle to B). A is unchanged — it still displays `s`. NOTE:
        // `s`'s reader thread stamps its `Wake::Output` with ONE owning window, but the
        // `Output` arm routes via `windows_displaying(s)` — now BOTH A and B, since both
        // canonically contain `s` — so the shared session's output repaints both viewers
        // with no re-stamp (the multi-viewer fan-out is now genuinely exercised).
        self.sync_active_session(); // frontmost = B
        debug_assert!(self.structural_invariants_ok());
        Some(wid_b)
    }

    /// Full "Open Active Session in New Window": the logical attach-a-view + (when not
    /// headless) the winit OS-window attach for the new window. A no-session-in-view
    /// front window is a silent no-op.
    pub(crate) fn open_active_session_in_new_window(&mut self, el: &ActiveEventLoop) {
        let Some(wid) = self.open_active_session_in_new_window_logical() else {
            return;
        };
        if !self.headless && !self.attach_os_window(el, wid) {
            // GPU surface failed: drop the new viewer. `close_window_logical` detaches
            // its SHARED view (views N→N-1), so the session survives in the original
            // window — no black orphan, no lost session.
            self.close_window_logical(wid);
        }
    }

    /// Test-only: append a stub `session` as a NEW tab of EXISTING window `wid` and
    /// switch to it (mirrors `open_tab`'s id-list edit without a real PTY spawn). The
    /// session is pooled (one view) so `tab_ids[active]` resolves; `session.id` MUST
    /// equal `self.next_session_id` (the test builds it that way), which is then
    /// bumped. Used to stage a multi-tab front window for the detach test. Publishes
    /// the appended terminal as the window's canonical front capability.
    // bench-support: many_tabs_idle in benches/frame_latency.rs stages its
    // N-tab window through this exact helper (see src/bench_support.rs).
    #[cfg(any(test, feature = "bench-support"))]
    pub(crate) fn push_stub_tab(&mut self, wid: WindowId, session: crate::Session) {
        debug_assert_eq!(
            session.id, self.next_session_id,
            "stub tab session id must match the minted session id",
        );
        let sid = session.id;
        self.next_session_id += 1;
        Self::register_session(&self.store, &session, None);
        let layout = pane::PaneTree::new(sid);
        let tab = crate::register_terminal_tab(&mut self.tab_ids, &mut self.view_store, &layout)
            .expect("stub tab identity space");
        self.pool.insert(session);
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.layouts.push(layout);
            ws.tabs.add();
            ws.tab_set.push(tab).expect("fresh tab id");
        }
        // `tabs.add()` switched the active tab to the new one; if `wid` is frontmost
        // the global handle must follow it too (matches `open_tab_in`), so the test
        // harness mirrors production's "active-tab change re-points the handle".
        self.resync_active_or_window(wid);
    }

    /// Test-only: split window `wid`'s ACTIVE tab into a 2-pane vertical split,
    /// spawning a fresh stub session for the new (now-focused) pane. Mirrors
    /// `split_focused_pane`'s pooling/registration without a real PTY. Returns the
    /// new pane's session id. Used to exercise split-tab teardown headlessly.
    // bench-support: the flood/pet compose fixtures in benches/frame_latency.rs
    // stage their 2-pane split through this exact helper (src/bench_support.rs).
    #[cfg(any(test, feature = "bench-support"))]
    pub(crate) fn split_active_stub_tab(&mut self, wid: WindowId) -> u64 {
        let sid = self.next_session_id;
        self.next_session_id += 1;
        let stub = crate::stub_session(sid);
        Self::register_session(&self.store, &stub, None);
        self.view_store
            .insert_terminal(sid)
            .expect("stub view identity space");
        self.pool.insert(stub);
        if let Some(t) = self.active_tree_mut(wid) {
            assert!(
                t.split_focused(pane::SplitDir::Vertical, sid),
                "stub split must succeed"
            );
        }
        let active = self.windows.get(&wid).map_or(0, |ws| ws.tabs.active);
        assert!(self.sync_tab_model_from_layout(wid, active));
        // Size the split panes explicitly, mirroring the real split path —
        // `sync_window` only re-fits when a shared (views > 1) session exists.
        self.resize_panes(wid);
        self.sync_window(wid);
        sid
    }

    /// Test-only heterogeneous counterpart: insert a real stable terminal view
    /// and pool session into the canonical active tree without requiring a PTY.
    #[cfg(test)]
    pub(crate) fn split_active_with_stub_terminal(
        &mut self,
        wid: WindowId,
        axis: crate::tab_model::SplitAxis,
    ) -> (u64, crate::tab_model::ViewId) {
        let sid = self.next_session_id;
        let view = self
            .view_store
            .insert_terminal(sid)
            .expect("finite test view id");
        let split = self
            .windows
            .get_mut(&wid)
            .and_then(|window| window.tab_set.active_mut())
            .is_some_and(|tab| tab.split_focused(axis, view));
        assert!(split, "canonical heterogeneous split succeeds");
        self.next_session_id = self.next_session_id.saturating_add(1);
        self.pool.insert(crate::stub_session(sid));
        self.resize_panes(wid);
        self.resync_active_or_window(wid);
        (sid, view)
    }

    /// Cmd-T: open a new tab — a fresh shell session in the SAME window — and
    /// switch to it. Spawns the session via the factory (its own PTY/engine/policy/
    /// OSC52/reader + a FRESH shell-integration nonce) at the current grid size. A
    /// spawn failure is logged and ignored (the existing tabs survive); it does NOT
    /// take down the window, unlike a fatal session-0 failure at startup.
    pub(crate) fn open_tab(&mut self) {
        // Cmd-T / menu open in the FRONTMOST window.
        if let Some(front) = self.frontmost_window {
            self.open_tab_in(front);
        }
    }

    /// `spawn` (control socket): open one new tab session in the frontmost window
    /// and return its freshly minted sid. The newborn is found by REGISTRY DIFF —
    /// snapshot the sid set, run the exact `open_tab_in` path Cmd-T takes, and
    /// return the one sid that appeared — so this can never misattribute a
    /// concurrently exiting session, and the sid is registered (addressable)
    /// before the caller's reply is written. Headless-safe: the logical window
    /// hosts tabs exactly like a real one.
    ///
    /// `split` (`spawn split=<v|h>`, the wire half of the front door's
    /// `aterm split-pane`) divides the FOCUSED PANE instead of opening a tab.
    /// The registry diff is what makes one function serve both: a split spawns
    /// exactly one new session too, so the newborn is found the same way and the
    /// same sid contract holds. It rides the identical `App::split_focused_pane`
    /// path a Cmd-D chord takes — including its refusal to split a SHARED
    /// session, which surfaces here as "session did not spawn" rather than as a
    /// corrupted co-viewing window.
    pub(crate) fn spawn_tab_session(
        &mut self,
        cwd: Option<String>,
        split: Option<crate::pane::SplitDir>,
    ) -> Result<String, String> {
        let Some(front) = self.frontmost_window else {
            return Err("no window to host the session".to_string());
        };
        let before: std::collections::HashSet<aterm_session::SessionId> = {
            let g = self.store.read().unwrap_or_else(|p| p.into_inner());
            g.snapshot().iter().map(|h| h.sid.clone()).collect()
        };
        match split {
            Some(dir) => self.split_focused_pane_in(dir, cwd),
            None => self.open_tab_in_cwd(front, cwd.as_deref()),
        }
        let after = {
            let g = self.store.read().unwrap_or_else(|p| p.into_inner());
            g.snapshot()
        };
        match after.iter().find(|h| !before.contains(&h.sid)) {
            Some(h) => Ok(h.sid.as_str().to_string()),
            None => Err("session did not spawn (window hosts no tabs?)".to_string()),
        }
    }

    /// REGISTRY snapshot → [`crate::status_item::SessionRow`]s (effective
    /// title + typed `role`/`attention`, one leaf-lock take per row) →
    /// `classify`, then join the window rows (id order, composed chrome
    /// title, tab count, frontmost mark) and the last background fleet scan's
    /// sibling rows. Title rung: user meta title else the registry's live OSC
    /// title — deliberately NO `Terminal` lock (this runs on chrome-refresh
    /// paths). `Exited` sessions are excluded: a dead operator is not a
    /// running operator.
    pub(crate) fn operator_fleet_glance(&self) -> crate::status_item::FleetGlance {
        let rows: Vec<crate::status_item::SessionRow> = {
            let store = self.store.read().unwrap_or_else(|p| p.into_inner());
            store
                .snapshot()
                .into_iter()
                .filter(|h| !matches!(h.state, crate::session_store::SessionState::Exited))
                .map(|h| {
                    // One leaf-lock take per row: title + the typed keys.
                    let (user_title, role, attention) = {
                        let m = h.ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
                        (m.user_title.clone(), m.role.clone(), m.attention.clone())
                    };
                    crate::status_item::SessionRow {
                        id: h.local_id,
                        title: user_title.unwrap_or_else(|| h.title.clone()),
                        role,
                        attention,
                    }
                })
                .collect()
        };
        let mut glance = crate::status_item::classify(&rows);
        // Window rows: id order (the map's iteration order), composed chrome
        // title, tab count, frontmost mark — the click-to-focus section.
        glance.windows = self
            .windows
            .iter()
            .map(|(wid, ws)| {
                // Drop a leading busy-spinner frame (Codex dots / Claude moons
                // animate at up to ~10Hz) and any IME preedit suffix (churns
                // per keystroke mid-composition): the menu row reads the same
                // either way, and the fingerprint must not rebuild the native
                // menu at animation/typing rate.
                let title = crate::toolbar::busy_spinner_title_parts(&ws.current_title)
                    .map_or_else(|| ws.current_title.clone(), |(_, s)| s.trim_start().to_string());
                let title = title
                    .split(" [‹")
                    .next()
                    .map_or(title.clone(), str::to_string);
                crate::status_item::WindowRow {
                    id: wid.0,
                    title,
                    tabs: ws.tab_set.tabs().len(),
                    frontmost: self.frontmost_window == Some(*wid),
                }
            })
            .collect();
        // Sibling rows from the last background scan (already pid-sorted).
        glance.instances = self.fleet_instances.clone();
        // Start is only offered when the agent CLI is actually launchable —
        // otherwise the menu would type a command the shell cannot find.
        glance.start_available = operator_cli_on_path();
        glance
    }

    /// Recompute the fleet glance and push it to the menu-bar status item —
    /// fingerprint-gated, so per-prompt title drift does not rebuild the native
    /// menu unless the rendered state actually changed. No-op with no status
    /// item (headless, off macOS, or pre-first-window).
    pub(crate) fn refresh_operator_status_item(&mut self) {
        if self._status_item.is_none() {
            return;
        }
        let glance = self.operator_fleet_glance();
        // The operator's id is NOT part of the rendered fingerprint (same title
        // after a restart ⇒ same strings), so track it before the change gate —
        // and the typed bit with it, because Stop's confirm-suppression
        // authority follows the id (a stale menu click must judge the CURRENT
        // election, not the one the menu was drawn from).
        self.operator_local_id = glance.operator_session;
        self.operator_typed = glance.operator_typed;
        let fp = glance.fingerprint();
        if self.operator_status_fingerprint.as_deref() == Some(fp.as_str()) {
            return;
        }
        self.operator_status_fingerprint = Some(fp);
        if let Some(handle) = &self._status_item {
            crate::status_item::update(handle, &glance);
        }
    }

    /// Dispatch one click from the menu-bar status item (the
    /// `dispatch_menu_action` twin for `Wake::OperatorAction`).
    ///
    /// See [`operator_cli_on_path`] for the Start-side launchability gate.
    pub(crate) fn dispatch_operator_action(
        &mut self,
        el: &ActiveEventLoop,
        action: crate::status_item::OperatorAction,
    ) {
        match action {
            crate::status_item::OperatorAction::Start => {
                // A running operator makes Start a stale-menu no-op (the click
                // raced a state change); just re-render the truth. Same for a
                // missing agent CLI (the row renders disabled, but a stale
                // menu can still deliver the click): never spawn a session
                // just to type a command the shell will not find.
                if self.operator_local_id.is_some() || !operator_cli_on_path() {
                    self.refresh_operator_status_item();
                    return;
                }
                // A TAB, never a split: the operator row stands up its own
                // terminal, not a division of whatever pane happens to be focused.
                match self.spawn_tab_session(None, None) {
                    Ok(sid) => {
                        // Stamp identity via user meta BEFORE the agent renames
                        // itself, then type the launch line through the sink —
                        // the `send` verb's write path (whole-frame atomic;
                        // bytes buffer in the kernel while the shell boots).
                        let stamped = {
                            let store = self.store.read().unwrap_or_else(|p| p.into_inner());
                            store.by_sid(&aterm_session::SessionId::new(&sid)).map(|h| {
                                // Typed identity FIRST (the classifier keys on
                                // it); the title stays the human status line.
                                // Through the validated ladder, so the
                                // meta-change timeline records land exactly
                                // like a wire `meta set` (the raw-set stamp
                                // was invisible to `events` watchers).
                                use crate::session_timeline::{
                                    MetaEdit, MetaField, write_session_meta,
                                };
                                let _ = write_session_meta(
                                    &h.ctx,
                                    MetaField::Role,
                                    MetaEdit::Set("operator"),
                                );
                                let _ = write_session_meta(
                                    &h.ctx,
                                    MetaField::Title,
                                    MetaEdit::Set("operator: starting"),
                                );
                                (h.local_id, h.ctx.sink.clone())
                            })
                        };
                        if let Some((local, sink)) = stamped {
                            // The wire meta arm's fan-out, mirrored (the GUI
                            // rename discipline): chrome recompose + a
                            // subscriber notify so the fresh records push.
                            self.refresh_meta_dependent_chrome(local);
                            if self.subscribers.any() {
                                self.subscribers
                                    .lock()
                                    .unwrap_or_else(|p| p.into_inner())
                                    .notify(local);
                            }
                            let mut bytes =
                                crate::status_item::OPERATOR_LAUNCH_LINE.as_bytes().to_vec();
                            bytes.push(0x0d);
                            let _ = sink.write_frame(&bytes);
                        }
                        self.refresh_operator_status_item();
                    }
                    Err(e) => eprintln!("aterm-gui: start operator failed: {e}"),
                }
            }
            crate::status_item::OperatorAction::Show => {
                let Some(session) = self.operator_local_id else {
                    return;
                };
                self.focus_session_window(session);
            }
            crate::status_item::OperatorAction::FocusSession(session) => {
                // A stale menu row (session closed mid-track) misses and
                // no-ops — ids are never reused, so it can never alias.
                self.focus_session_window(session);
            }
            crate::status_item::OperatorAction::FocusWindow(id) => {
                let wid = WindowId(id);
                if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.clone()) {
                    // focus_window is a silent no-op on a miniaturized window
                    // and deminiaturization is ASYNC — arm the pending-raise
                    // so the Occluded(false)/Focused arms finish the job.
                    if w.is_minimized().unwrap_or(false) {
                        self.pending_deminiaturize_focus = Some(wid);
                    }
                    w.set_minimized(false);
                    w.focus_window();
                    w.request_redraw();
                }
                self.note_window_focused_if_present(wid);
            }
            crate::status_item::OperatorAction::RaiseInstance(pid) => {
                // Composed rows disable pid 0; the seam re-guards anyway and a
                // dead pid reads as a failed activation, silently — a stale
                // menu row is a miss, never an error dialog.
                let _ = self.apprt.activate_instance(pid);
            }
            crate::status_item::OperatorAction::Stop => {
                let Some(session) = self.operator_local_id else {
                    return;
                };
                // Confirm-suppression REQUIRES the typed role: only a session
                // that declared `role=operator` may be closed without the
                // dialog. A title-elected operator gets no Stop row, but a
                // stale menu can still deliver the click — judged here against
                // the CURRENT election, that close runs with the confirm
                // armed, so an innocent session ("operator.md" in an editor)
                // is protected by the ordinary dialog.
                let suppress = self.operator_typed;
                if suppress {
                    self.close_confirm_suppressed = true;
                }
                let result = self.close_session_by_id(session);
                if suppress {
                    self.close_confirm_suppressed = false;
                }
                if result.is_ok() {
                    self.escalate_pending_close(el);
                }
                self.refresh_operator_status_item();
            }
        }
    }

    /// Focus the tab displaying `session` and raise its window — the Show
    /// Operator body, shared by every session-focusing menu row. Returns
    /// whether the session was found in any window.
    pub(crate) fn focus_session_window(&mut self, session: u64) -> bool {
        let found = self.windows.keys().find_map(|wid| {
            self.terminal_view_location(*wid, session)
                .map(|location| (*wid, location.canonical_index))
        });
        let Some((wid, index)) = found else {
            return false;
        };
        self.switch_tab_in(wid, index);
        if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.clone()) {
            if w.is_minimized().unwrap_or(false) {
                self.pending_deminiaturize_focus = Some(wid);
            }
            w.set_minimized(false);
            w.focus_window();
        }
        true
    }

    /// MRU bookkeeping for a menu-driven raise, skipped when the id already
    /// vanished (the stale-row miss path).
    fn note_window_focused_if_present(&mut self, wid: WindowId) {
        if self.windows.contains_key(&wid) {
            self.note_window_focused(wid);
        }
    }

    /// DIFF (like spawn): if the session is gone afterward it succeeded; if it
    /// survives, the close was refused and we say so.
    pub(crate) fn close_session_by_id(&mut self, session: u64) -> Result<(), String> {
        let found = self.windows.keys().find_map(|wid| {
            self.terminal_view_location(*wid, session)
                .map(|location| (*wid, location))
        });
        let Some((wid, location)) = found else {
            return Err("no such session".to_string());
        };
        if location.terminal_only {
            if self.close_tab_at(wid, location.canonical_index)
                && let Some(ws) = self.windows.get_mut(&wid)
            {
                ws.pending_close = true; // last tab → the window closes with it
            }
        } else if !self.close_heterogeneous_terminal_view(wid, location) {
            return Err("session view changed during close".to_string());
        }
        let gone = self
            .store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .by_local(session)
            .is_none();
        if gone {
            Ok(())
        } else {
            Err("close refused (a running job armed the last-tab confirm)".to_string())
        }
    }

    /// Open a new tab in window `owner` (window-aware: the tab-strip `+` of a
    /// non-frontmost window opens there, not in the frontmost). The new session is
    /// stamped with `owner` so its output/exit/bell route back to THIS window.
    ///
    /// TRUST anchor: the `NewTab` action of the ty-proven `tab_strip` machine
    /// (`tab_strip_model()`) — appends a tab and re-syncs the owner's native strip.
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "tab_strip",
            action = "NewTab",
            project = "aterm_gui::tab_strip_conformance::project"
        )
    )]
    pub(crate) fn open_tab_in(&mut self, owner: WindowId) {
        self.open_tab_in_cwd(owner, None);
    }

    /// [`open_tab_in`] with an optional CWD override (the `spawn cwd=<path>` path).
    /// `None` keeps the default: inherit the focused pane's cwd so a Cmd-T tab opens
    /// where the user is.
    pub(crate) fn open_tab_in_cwd(&mut self, owner: WindowId, cwd_override: Option<&str>) {
        if !self.windows.contains_key(&owner) {
            return;
        }
        let id = self.next_session_id;
        let (rows, cols) = self
            .windows
            .get(&owner)
            .map_or((0, 0), |ws| (ws.rows, ws.cols));
        // A real run always has a proxy; guard rather than panic (test-only None).
        let Some(proxy) = self.proxy.clone() else {
            return;
        };
        // Inherit the focused tab's cwd so a new tab opens where the user is —
        // unless the caller (spawn cwd=<path>) overrode it.
        let cwd = cwd_override
            .map(str::to_string)
            .or_else(|| self.focused_pane_cwd(owner));
        match spawn_session(
            id,
            owner,
            rows,
            cols,
            &self.session_factory,
            &proxy,
            cwd.as_deref(),
            None, // fresh shell (not a seamless-update adoption)
        ) {
            Ok(session) => {
                self.next_session_id += 1;
                // P1.1: register in the process-wide registry (additive index) so a
                // cross-session `@<selector>` verb can reach this tab. The parent is
                // the FOCUSED pane's session of the OWNER window when the tab was
                // opened (the family tree; a user-opened tab is a child of the pane
                // it was opened from).
                let parent = self
                    .front_terminal(owner)
                    .and_then(|terminal| self.pool.get(terminal.session))
                    .map(|s| s.ctx.self_id.clone());
                Self::register_session(&self.store, &session, parent);
                let layout = pane::PaneTree::new(id);
                let tab =
                    crate::register_terminal_tab(&mut self.tab_ids, &mut self.view_store, &layout)
                        .expect("tab/view identity space");
                self.pool.insert(session);
                // Append a fresh single-pane tree (one leaf) and bump the owner
                // window's index in lockstep (keeps `layouts.len() == tabs.count`).
                if let Some(ws) = self.windows.get_mut(&owner) {
                    ws.layouts.push(layout);
                    ws.tabs.add();
                    ws.tab_set.push(tab).expect("fresh tab id");
                }
                // Mirror the owner; if it's frontmost, also re-point the globals.
                if self.frontmost_window == Some(owner) {
                    self.sync_active_session();
                } else {
                    self.sync_window(owner);
                }
            }
            Err(e) => {
                eprintln!("aterm-gui: could not open a new tab: {e}");
                self.surface_gesture_failure(&format!("✕ New tab failed: {e}"));
            }
        }
    }

    /// Cmd-1..Cmd-9: switch to tab index `i` (0-based) if it exists. No-op (and no
    /// repaint) when `i` is already active or out of range.
    pub(crate) fn switch_tab(&mut self, i: usize) {
        if let Some(front) = self.frontmost_window {
            self.switch_tab_in(front, i);
        }
    }

    /// Switch window `wid` to tab `i` (window-aware: a tab-strip CLICK targets the
    /// clicked window, which may not be the frontmost). Re-mirrors that window; when
    /// it is the frontmost, also re-points the global control/notify handles
    /// (`sync_active_session`), matching the keyboard/menu `switch_tab` behavior.
    pub(crate) fn switch_tab_in(&mut self, wid: WindowId, i: usize) {
        let Some((tab_id, terminal_index, already_active)) =
            self.windows.get(&wid).and_then(|ws| {
                let tab = ws.tab_set.tab_at(i)?;
                Some((
                    tab.id,
                    terminal_projection_index(&ws.tab_set, &self.view_store, tab.id),
                    ws.tab_set.active_id() == Some(tab.id),
                ))
            })
        else {
            return;
        };
        if already_active {
            return;
        }
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let switched = ws.tab_set.switch_to_index(i);
        debug_assert!(switched);
        debug_assert_eq!(ws.tab_set.active_id(), Some(tab_id));
        if let Some(index) = terminal_index {
            ws.tabs.switch_to(index);
        }
        if self.frontmost_window == Some(wid) {
            self.sync_active_session();
        } else {
            self.sync_window(wid);
        }
        self.palette_sync_native_scope(wid);
    }

    /// Cmd-Shift-] / Cmd-Shift-[: cycle to the next/previous tab, wrapping. No-op
    /// with a single tab.
    ///
    /// TRUST anchor: the `SelectTab` action of the ty-proven `tab_strip` machine
    /// (`tab_strip_model()`) — the DETERMINISTIC wrap the model encodes (vs the
    /// arbitrary-index `switch_tab_in`); re-syncs the strip selection in lockstep.
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "tab_strip",
            action = "SelectTab",
            project = "aterm_gui::tab_strip_conformance::project"
        )
    )]
    pub(crate) fn cycle_tab(&mut self, forward: bool) {
        let Some(wid) = self.frontmost_window else {
            return;
        };
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        if ws.tab_set.len() <= 1 {
            return;
        }
        let terminal_only = ws.tab_set.len() == ws.layouts.len();
        let Some(next) = ws.tab_set.cycle(forward) else {
            return;
        };
        if terminal_only {
            let projected = ws.tabs.cycle(forward);
            debug_assert_eq!(
                terminal_projection_index(&ws.tab_set, &self.view_store, next),
                Some(projected)
            );
        } else if let Some(index) = terminal_projection_index(&ws.tab_set, &self.view_store, next) {
            ws.tabs.switch_to(index);
        }
        self.sync_active_session();
        self.palette_sync_native_scope(wid);
    }

    /// Apply a control-socket `tab` verb ([`TabAction`]) to the FRONT window and
    /// return the resulting `(active_index, tab_count)` for the verb's reply. Driven
    /// by [`Wake::TabCmd`] on the main loop (the sole `App` mutator). Each action
    /// reuses the EXISTING command path — `New` => [`Self::open_tab`] (same as File ▸
    /// New Tab / the toolbar "+"), `Select(n)` => [`Self::switch_tab`], `Next`/`Prev`
    /// => [`Self::cycle_tab`] — so the verb adds no parallel tab logic. With no front
    /// window (impossible in a real run) it reports `(0, 0)`.
    pub(crate) fn apply_tab_cmd(&mut self, action: TabAction) -> (usize, usize) {
        match action {
            TabAction::New => self.open_tab(),
            TabAction::Select(n) => self.switch_tab(n),
            TabAction::Next => self.cycle_tab(true),
            TabAction::Prev => self.cycle_tab(false),
            TabAction::Close(which) => self.close_tab_via_verb(which),
            TabAction::Move { from, to } => {
                if let Some(front) = self.frontmost_window {
                    self.move_tab(front, from, to);
                }
            }
        }
        // Report the canonical mixed-tab state. The terminal-only `TabIndex` is a
        // compatibility projection and can have a different count/index whenever
        // native tabs are present.
        self.front().map_or((0, 0), |ws| {
            (ws.tab_set.active_index().unwrap_or(0), ws.tab_set.len())
        })
    }

    /// Close the front window's tab `which` (or its ACTIVE tab when `None`) for the
    /// `tab close [N]` verb and the native × button's [`Wake::CloseTab`]. Reuses
    /// [`Self::close_tab_at`] (the SAME whole-tab close the renderer strip's `✕` and
    /// the tab-strip click take); if that was the window's LAST tab it flags
    /// `pending_close` so the `Wake` handler's `escalate_pending_close(el)` tears the
    /// window down (the verb / button paths have no `ActiveEventLoop`), exactly like a
    /// tab-strip close.
    pub(crate) fn close_tab_via_verb(&mut self, which: Option<usize>) {
        let Some(front) = self.frontmost_window else {
            return;
        };
        let i = match which {
            Some(i) => i,
            None => self
                .windows
                .get(&front)
                .and_then(|ws| ws.tab_set.active_index())
                .unwrap_or(0),
        };
        if self.close_tab_at(front, i)
            && let Some(ws) = self.windows.get_mut(&front)
        {
            ws.pending_close = true;
        }
    }

    /// Reorder window `wid`'s canonical tab from index `from` to index `to`, keeping
    /// the same stable tab selected (drag-to-reorder must not silently switch tabs).
    /// A terminal move also reorders its `layouts` projection when its relative
    /// position among terminal tabs changes; a native move never touches that
    /// projection. Out-of-range `from`/`to`, a stale/unknown window, or `from == to`
    /// are no-ops. Re-mirrors the window (the native strip re-tracks via
    /// `sync_window`).
    ///
    /// INVARIANT preserved: `tabs.count == layouts.len()` remains the exact
    /// terminal-only projection, while `tab_set` remains the canonical mixed order.
    pub(crate) fn move_tab(&mut self, wid: WindowId, from: usize, to: usize) {
        let views = &self.view_store;
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let n = ws.tab_set.len();
        if from >= n || to >= n || from == to {
            return;
        }
        let terminal_before = terminal_tab_ids(&ws.tab_set, views);
        let parked = terminal_before.get(ws.tabs.active).copied();
        let stable_id = ws.tab_set.tab_at(from).expect("canonical tab in range").id;
        let reordered = ws.tab_set.reorder(stable_id, to);
        debug_assert!(reordered);
        let terminal_after = terminal_tab_ids(&ws.tab_set, views);
        if let (Some(old), Some(new)) = (
            terminal_before.iter().position(|id| *id == stable_id),
            terminal_after.iter().position(|id| *id == stable_id),
        ) && old != new
        {
            let tree = ws.layouts.remove(old);
            ws.layouts.insert(new, tree);
        }
        if let Some(parked) = parked
            && let Some(index) = terminal_after.iter().position(|id| *id == parked)
        {
            ws.tabs.active = index;
        }
        align_terminal_projection_to_active(ws, views);
        // Mirror the window so the native strip re-tracks the new order/selection.
        if self.frontmost_window == Some(wid) {
            self.sync_active_session();
        } else {
            self.sync_window(wid);
        }
    }

    /// Re-sync window `wid`'s NATIVE toolbar tab strip to the app's current tab
    /// state: rebuild the view-based strip's per-tab views (one per tab, splitting the
    /// band into equal shares with the active one accented — or, at exactly one tab,
    /// the single centred window TITLE band) from [`Self::tab_titles`] + the
    /// window's active index, via [`toolbar::set_window_tabs`]. Called from
    /// [`Self::sync_window`] so the strip tracks EVERY tab mutation (open / close /
    /// switch / detach / migrate / reorder). A no-op off macOS and for a window with no
    /// toolbar handle (headless / a window whose toolbar failed to install).
    ///
    /// Returns the `(titles, metadata)` snapshot it just pushed to the strip, so
    /// a caller that also needs the repaint fingerprint feeds
    /// [`Self::tab_strip_fingerprint_from_parts`] THESE vectors instead of
    /// recomputing them. That is exactly the invariant that function's own doc
    /// asks for ("reads the per-tab titles ONCE and feeds the SAME `Vec` to
    /// both"): `tab_titles` takes each tab's session meta lock and a terminal
    /// `try_lock`, clones its presentation fallback, resolves and allocates the
    /// title rung, and runs the summary composer — none of which should happen
    /// twice per refresh, and doing it once also makes the pushed chrome and the
    /// redraw decision describe ONE snapshot rather than two possibly divergent
    /// reads. Callers that only want the push (`sync_window`, the reorder path,
    /// the chrome-expiry sweep) just drop the tuple.
    ///
    /// NOTE the returned pair deliberately excludes the active index: the one
    /// this function computes is `tab_set.active_index()` (over ALL tabs) while
    /// the strip fingerprint's callers use `ws.tabs.active` (the TERMINAL
    /// projection index). Those diverge the moment a native tab exists.
    pub(crate) fn refresh_window_tabs(
        &mut self,
        wid: WindowId,
    ) -> (Vec<String>, Vec<crate::tab_bar::TabStripMetadata>) {
        // Keep the session identity aligned with `titles` without taking another
        // terminal lock. This is also the bounded key set for the early title-epoch
        // cache below; native tabs carry `None` and never enter either map.
        let label_sessions: Vec<Option<u64>> = self.windows.get(&wid).map_or_else(Vec::new, |ws| {
            ws.tab_set
                .tabs()
                .iter()
                .map(|tab| {
                    self.view_store
                        .get(tab.focus)
                        .copied()
                        .and_then(crate::tab_model::View::terminal_session)
                })
                .collect()
        });
        // The in-grid twin of the native strip's `sync_rename_overlay`, driven from
        // the same place for the same reason: this is the one function every
        // structural tab change funnels through, so it is where an editor notices
        // its tab is gone.
        self.reap_stale_rename_edit(wid);
        let titles = self.tab_titles(wid);
        // Composed per-tab chrome (tooltip + context-menu model) rides the same
        // refresh: epoch-cached, so a steady prompt pays one leaf-lock read per
        // terminal tab (see `composed_session_chrome`), and the strip receives
        // titles/metadata/chrome as ONE consistent snapshot.
        let ext = self.tab_chrome_ext(wid, &titles);
        let metadata = self.tab_strip_metadata(wid);
        let tooltips = self.tab_tooltips(wid);
        let active = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.tab_set.active_index())
            .unwrap_or(0);
        // The canonical STABLE id per tab, paired with `titles` — the strip
        // stamps these on its chips so a right-click captures the clicked tab's
        // identity at menu-pop time (a positional index would re-bind to
        // whatever tab a mid-menu close/reorder moved into the slot).
        let ids: Vec<crate::tab_model::TabId> =
            self.windows.get(&wid).map_or_else(Vec::new, |ws| {
                ws.tab_set.tabs().iter().map(|tab| tab.id).collect()
            });
        // Shadow what the native strip is being told to render BEFORE the push, so a
        // tab mutation that forgets to call this fn leaves the recorded strip state
        // stale — the only way a headless test can witness the strip↔model desync the
        // `tab_strip` machine proves can't happen. (`titles.len()` == tab count.)
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.strip_shadow.set((titles.len(), active));
            // This is the exact title vector passed to native chrome below. Cache it
            // at the existing push point so a later background spinner phase can be
            // rejected before `refresh_window_tabs` takes every tab's terminal lock
            // and allocates another title/tooltip/metadata set. Structural refreshes
            // also land here, ensuring the first post-switch phase is coalescible.
            ws.tab_title_epochs
                .retain(|sid, _| label_sessions.contains(&Some(*sid)));
            ws.tab_chrome_titles
                .retain(|sid, _| label_sessions.contains(&Some(*sid)));
            for (session, title) in label_sessions.iter().copied().zip(&titles) {
                if let Some(session) = session {
                    ws.tab_chrome_titles
                        .entry(session)
                        .or_default()
                        .clone_from(title);
                }
            }
            // The drift cache above is correctly keyed by SESSION, but that key
            // cannot faithfully shadow chrome when two distinct tabs view the
            // same pooled session and resolve different fallbacks under parser
            // contention. Inspection is tab-addressed, so retain and update the
            // exact per-tab title vector independently.
            ws.tab_chrome_titles_by_tab
                .retain(|tab_id, _| ids.contains(tab_id));
            for ((tab_id, session), title) in ids
                .iter()
                .copied()
                .zip(label_sessions.iter().copied())
                .zip(&titles)
            {
                let entry = ws
                    .tab_chrome_titles_by_tab
                    .entry(tab_id)
                    .or_insert_with(|| (session, String::new()));
                entry.0 = session;
                entry.1.clone_from(title);
            }
        }
        if let Some(handle) = self._toolbars.get(&wid) {
            self.apprt.set_toolbar_tabs(
                handle,
                crate::platform::ToolbarTabsModel {
                    titles: &titles,
                    ids: &ids,
                    metadata: &metadata,
                    tooltips: &tooltips,
                    ext: &ext,
                    active,
                },
            );
        }
        // Title drift funnels through here (every structural change + each
        // prompt relabel), so this is where the menu-bar operator icon observes
        // a title gaining/losing `operator`/`⚠`. Fingerprint-gated: a steady
        // prompt costs one glance compare, no AppKit work.
        self.refresh_operator_status_item();
        (titles, metadata)
    }

    /// Repaint everything a session's USER metadata feeds, after a change has
    /// ALREADY been stored and recorded. Shared by the control socket's
    /// `Wake::MetaChanged` arm and the in-process GUI rename commit, so the two
    /// can never drift on what a pin change invalidates.
    ///
    /// The user title is the TOP tab-label rung, so every window labeling a tab
    /// with this session re-reads its strip: the native (macOS) toolbar push,
    /// plus a redraw when either the in-grid strip changed OR this is the
    /// focused session (its metadata also feeds the native OS title even when
    /// `tab_strip_rows = 0`, and `apply_title` runs on the redraw path including
    /// its nothing-changed early-out). This is the exact refresh pair the
    /// epoch-gated title/cwd drift path runs, minus the epoch gate (a pin change
    /// is rare and every caller already gates on an ACTUAL change).
    ///
    /// ORDERING: the `meta-change` timeline record must already have landed —
    /// [`crate::session_timeline::apply_meta_value`] takes it under the meta
    /// guard, before this runs. Refreshing first would let `composed_session_chrome`
    /// re-cache stale tooltip/context-menu text under an unmoved `high_id` key,
    /// which for a quiet session with a pinned label never expires.
    pub(crate) fn refresh_meta_dependent_chrome(&mut self, session: u64) {
        let label_windows: Vec<WindowId> = self
            .windows
            .iter()
            .filter(|(_, ws)| {
                ws.tab_set.tabs().iter().any(|tab| {
                    self.view_store
                        .get(tab.focus)
                        .copied()
                        .and_then(crate::tab_model::View::terminal_session)
                        == Some(session)
                })
            })
            .map(|(wid, _)| *wid)
            .collect();
        for wid in label_windows {
            let focused = self.focused_session_id(wid) == Some(session);
            // Same snapshot the strip was just pushed — see
            // `refresh_window_tabs`; recomputing it here doubled the
            // per-tab meta-lock + term-try-lock + compose round trips.
            let (titles, metadata) = self.refresh_window_tabs(wid);
            let active = self.windows.get(&wid).map_or(0, |ws| ws.tabs.active);
            let fp = self.tab_strip_fingerprint_from_parts(wid, &titles, &metadata, active);
            let strip_changed = self
                .windows
                .get(&wid)
                .is_some_and(|ws| ws.last_present.as_ref().is_none_or(|k| k.tab_strip != fp));
            let needs_redraw = crate::metadata_change_needs_redraw(focused, strip_changed);
            if needs_redraw
                && let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref())
            {
                w.request_redraw();
            }
        }
    }

    /// RETIRED AFFORDANCE (update-flow UX rework): the titlebar "Update" capsule is gone
    /// — the update affordance moved to the VERSION menu (one-click apply, see
    /// [`crate::menu::update_version_menu`] / `App::refresh_version_menu`) — so nothing
    /// calls this any more and the macOS `toolbar::set_update_available` it fans out to
    /// is itself a documented no-op. Kept (allow(dead_code) makes it a live root, so the
    /// `Apprt::set_toolbar_update_available` seam it exercises stays warning-free)
    /// because `platform.rs` is outside this change's surface; delete both together.
    #[allow(
        dead_code,
        reason = "keeps the Apprt::set_toolbar_update_available seam compiling while the titlebar capsule is retired; platform.rs is owned by concurrent work"
    )]
    pub(crate) fn set_toolbar_update_available(&self, available: bool) {
        for handle in self._toolbars.values() {
            self.apprt.set_toolbar_update_available(handle, available);
        }
    }

    /// Cmd-W: close the FOCUSED pane of the FRONTMOST window's active tab. Returns
    /// `Some(window)` — the window whose last tab just closed — iff that was the LAST
    /// pane of the LAST tab, so the caller escalates to closing THAT window (the
    /// frontmost), not whichever window an input event was stamped for. Returns
    /// `None` otherwise. Closing a pane in a SPLIT tab collapses the split onto its
    /// sibling (the sibling — and its reader thread — survive); closing the only pane
    /// of a non-last tab closes the tab. Honors `--hold` ONLY for the implicit close
    /// on a session's own EOF (see `close_session`); an explicit Cmd-W always closes.
    pub(crate) fn close_active_tab(&mut self) -> Option<WindowId> {
        let window = self.frontmost_window?;
        let (stable_tab, stable_view) = self
            .windows
            .get(&window)
            .and_then(|state| state.tab_set.active())
            .map(|tab| (tab.id, tab.focus))?;
        if self.defer_pending_update_handoff_teardown(crate::DeferredHandoffTeardown::mutation(
            crate::DeferredHandoffMutation::CloseView {
                window,
                tab: stable_tab,
                view: stable_view,
            },
        )) {
            return None;
        }
        let mixed_split = self
            .windows
            .get(&window)
            .and_then(|state| state.tab_set.active())
            .is_some_and(|tab| tab.root.len() > 1 && !is_terminal_tab(tab, &self.view_store));
        if mixed_split {
            let _ = self.close_focused_mixed_leaf(window);
            return None;
        }
        if self.active_native_view(window).is_some() {
            let was_last = self
                .windows
                .get(&window)
                .is_some_and(|ws| ws.tab_set.len() == 1);
            return self
                .close_active_native_tab(window)
                .is_ok()
                .then_some(window)
                .filter(|_| was_last);
        }
        let tab = self.front().map_or(0, |ws| ws.tabs.active);
        // M2 quit-safety: this closes the focused PANE, which exits the window only
        // when it is the last pane of the last tab. Refuse a stray such close while a
        // job runs (arming the confirm), exactly like the red ✕ / Cmd-Q — so Cmd-W
        // can't silently SIGHUP an in-flight build/AI run. A split-pane collapse or a
        // non-last tab is never blocked.
        let exits_window = self.windows.get(&window).is_some_and(|ws| {
            ws.tab_set.len() <= 1
                && ws
                    .layouts
                    .get(ws.tabs.active)
                    .is_some_and(|tree| tree.len() <= 1)
        });
        if !self.window_exit_close_allowed(window, exits_window) {
            return None;
        }
        let recovery = self.closed_view_record_for_active(window);
        let outcome = self.active_tree_mut(window).map(|t| t.close_focused())?;
        let collapsed = matches!(outcome, pane::CloseOutcome::Collapsed { .. });
        // `true` = the frontmost window's last tab closed → tell the caller WHICH
        // window to escalate-close (always the frontmost we operated on).
        let closes_window = self.apply_close_outcome(window, tab, outcome);
        if collapsed && let Some(recovery) = recovery {
            self.retain_closed_view(recovery);
        }
        closes_window.then_some(window)
    }

    /// Replay a pre-rollback focused-view close against stable identities.  The
    /// user may have changed focus while the worker was aborting, so temporarily
    /// target the originally requested leaf, run the ordinary close transaction,
    /// then restore any still-live newer selection.
    pub(crate) fn replay_deferred_handoff_view_close(
        &mut self,
        window: WindowId,
        tab: crate::tab_model::TabId,
        view: crate::tab_model::ViewId,
    ) -> Option<WindowId> {
        let (previous_front, previous_active, previous_focus) = {
            let state = self.windows.get(&window)?;
            let target = state.tab_set.get(tab)?;
            if !target.root.contains(view) {
                return None;
            }
            (
                self.frontmost_window,
                state.tab_set.active_id(),
                target.focus,
            )
        };

        self.frontmost_window = Some(window);
        if let Some(state) = self.windows.get_mut(&window) {
            state.tab_set.switch_to(tab);
            let target_index = state
                .tab_set
                .tabs()
                .iter()
                .position(|candidate| candidate.id == tab);
            if let Some(target_index) = target_index
                && let Some(target) = state.tab_set.tab_at_mut(target_index)
            {
                let _ = target.set_focus(view);
            }
        }

        let closes_window = self.close_active_tab();
        let blocked_recovery_owns_focus = self.windows.get(&window).is_some_and(|state| {
            state
                .tab_set
                .get(tab)
                .is_some_and(|target| target.root.contains(view))
                && state
                    .palette()
                    .is_some_and(|palette| palette.is_native_close_recovery_for(window, view))
        });
        if closes_window.is_none() && !blocked_recovery_owns_focus {
            if let Some(state) = self.windows.get_mut(&window) {
                let target_index = state
                    .tab_set
                    .tabs()
                    .iter()
                    .position(|candidate| candidate.id == tab);
                if let Some(target_index) = target_index
                    && let Some(target) = state.tab_set.tab_at_mut(target_index)
                {
                    let _ = target.set_focus(previous_focus);
                }
                if let Some(previous_active) = previous_active {
                    state.tab_set.switch_to(previous_active);
                }
            }
            self.frontmost_window = previous_front
                .filter(|previous| self.windows.contains_key(previous))
                .or_else(|| self.windows.keys().next().copied());
            self.resync_active_or_window(window);
        }
        closes_window
    }

    /// Resolve one deferred whole-tab close by stable identity so intervening tab
    /// selection/reordering cannot redirect it to a different tab.
    pub(crate) fn replay_deferred_handoff_tab_close(
        &mut self,
        window: WindowId,
        tab: crate::tab_model::TabId,
    ) -> bool {
        let Some(index) = self.windows.get(&window).and_then(|state| {
            state
                .tab_set
                .tabs()
                .iter()
                .position(|candidate| candidate.id == tab)
        }) else {
            return false;
        };
        self.close_tab_at(window, index)
    }

    /// Focus the exact leaf that refused a close and expose only the typed
    /// recovery commands returned by its reducer. Keeping this in the tab host
    /// makes every close entry point (including background/control targets)
    /// converge on one visible, generation-bound recovery surface.
    fn surface_native_close_recovery(
        &mut self,
        wid: WindowId,
        tab: crate::tab_model::TabId,
        view: crate::tab_model::ViewId,
        native: crate::tab_model::NativeViewRef,
        recovery: Vec<crate::native_app::Command>,
    ) -> Result<(), String> {
        let window = self
            .windows
            .get_mut(&wid)
            .ok_or_else(|| "blocked native close window disappeared".to_string())?;
        let target = window
            .tab_set
            .get(tab)
            .ok_or_else(|| "blocked native close tab disappeared".to_string())?;
        if !target.root.contains(view) {
            return Err("blocked native close view left its tab".to_string());
        }
        window.tab_set.switch_to(tab);
        let focused = window
            .tab_set
            .active_mut()
            .is_some_and(|target| target.set_focus(view));
        if !focused {
            return Err("blocked native close view could not be focused".to_string());
        }
        self.frontmost_window = Some(wid);
        self.resync_active_or_window(wid);
        self.palette_enter_native_close_recovery(wid, native.instance, view, recovery)
    }

    /// Run one native reducer's close transaction without mutating topology.
    /// `Ok(false)` means the app deliberately retained its view (Blocked or
    /// Pending); a Blocked verdict under
    /// [`ClosePreflightVisibility::Interactive`] has also made its recovery
    /// capabilities visible through [`Self::surface_native_close_recovery`].
    fn prepare_native_leaf_close(
        &mut self,
        wid: WindowId,
        tab: crate::tab_model::TabId,
        view: crate::tab_model::ViewId,
        native: crate::tab_model::NativeViewRef,
        scope: crate::native_app::CloseScope,
        visibility: ClosePreflightVisibility,
    ) -> Result<bool, String> {
        let (readiness, effects) = self
            .native_runtime
            .prepare_close(
                native.instance,
                view,
                crate::native_app::CloseRequest { scope },
            )
            .map_err(|error| format!("native close failed: {error:?}"))?;
        if !effects.is_empty() {
            return Err("native close preparation emitted unhandled effects".to_string());
        }
        match readiness {
            crate::native_app::CloseReadiness::Ready => Ok(true),
            crate::native_app::CloseReadiness::Pending { .. } => Ok(false),
            crate::native_app::CloseReadiness::Blocked { recovery } => {
                // THE VERDICT IS THE SAME EITHER WAY; ONLY THE SCREEN DIFFERS.
                // A Quiet probe still runs the reducer and still reports
                // `Ok(false)`, so no caller gets a weaker safety answer — it
                // simply declines to answer a question nobody asked by taking
                // the user's tab, focus, window and overlay. See
                // [`ClosePreflightVisibility`] for why that distinction is
                // load-bearing rather than cosmetic.
                if visibility == ClosePreflightVisibility::Interactive {
                    self.surface_native_close_recovery(wid, tab, view, native, recovery)?;
                }
                Ok(false)
            }
        }
    }

    /// Preflight every native leaf in one window before an irreversible window
    /// teardown. No document or topology edge changes until all reducers agree.
    pub(crate) fn prepare_window_native_shutdown(
        &mut self,
        wid: WindowId,
        scope: crate::native_app::CloseScope,
    ) -> Result<bool, String> {
        // Every caller of THIS entry point is a person closing a window (Cmd-W,
        // the close button, a control-socket close on the user's behalf), so the
        // recovery surface is the answer to their request. Only the process-wide
        // barrier below is reachable from background policy, so only it takes a
        // visibility argument.
        self.prepare_window_native_shutdown_with(wid, scope, ClosePreflightVisibility::Interactive)
    }

    /// [`Self::prepare_window_native_shutdown`] with an explicit disruption
    /// policy, so the process-wide barrier can forward a background caller's.
    fn prepare_window_native_shutdown_with(
        &mut self,
        wid: WindowId,
        scope: crate::native_app::CloseScope,
        visibility: ClosePreflightVisibility,
    ) -> Result<bool, String> {
        let leaves = self
            .windows
            .get(&wid)
            .ok_or_else(|| "unknown native-close window".to_string())?
            .tab_set
            .tabs()
            .iter()
            .flat_map(|tab| {
                tab.root.leaves().into_iter().filter_map(|view| {
                    let crate::tab_model::View::Native(native) =
                        self.view_store.get(view).copied()?
                    else {
                        return None;
                    };
                    Some((tab.id, view, native))
                })
            })
            .collect::<Vec<_>>();
        for (tab, view, native) in leaves {
            if !self.prepare_native_leaf_close(wid, tab, view, native, scope, visibility)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Process-wide counterpart of [`Self::prepare_window_native_shutdown`].
    /// Every window/view remains live; under
    /// [`ClosePreflightVisibility::Interactive`] the first blocker is also focused
    /// and surfaced. Quit and update-relaunch feed their distinct semantic scope
    /// into this same reducer-owned barrier.
    ///
    /// `visibility` is explicit at this seam and nowhere else because this is the
    /// ONLY close barrier background policy can reach: the automatic update lane
    /// re-probes it on a cooldown and must stay invisible while doing so.
    pub(crate) fn prepare_all_native_shutdown(
        &mut self,
        scope: crate::native_app::CloseScope,
        visibility: ClosePreflightVisibility,
    ) -> Result<bool, String> {
        let windows = self.windows.keys().copied().collect::<Vec<_>>();
        for wid in windows {
            if !self.prepare_window_native_shutdown_with(wid, scope, visibility)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub(crate) fn prepare_quit_native_shutdown(&mut self) -> Result<bool, String> {
        // Quit is always a person's request — including the deferred-update quit
        // path, which only runs once the user has chosen to quit.
        self.prepare_all_native_shutdown(
            crate::native_app::CloseScope::AppQuit,
            ClosePreflightVisibility::Interactive,
        )
    }

    /// Close one focused leaf of a heterogeneous tab as a host transaction.
    /// Native readiness/document durability is proven before topology changes;
    /// terminal ownership is detached only after the canonical tree has a live
    /// repaired focus. The tab necessarily survives because this is called only
    /// for a multi-leaf tree.
    pub(crate) fn close_focused_mixed_leaf(&mut self, wid: WindowId) -> Result<(), String> {
        let recovery = self
            .closed_view_record_for_active(wid)
            .ok_or_else(|| "focused view has no recoverable placement".to_string())?;
        let (tab_id, view, linked) = self
            .windows
            .get(&wid)
            .and_then(|window| {
                let tab = window.tab_set.active()?;
                self.view_store
                    .get(tab.focus)
                    .copied()
                    .map(|linked| (tab.id, tab.focus, linked))
            })
            .ok_or_else(|| "window has no focused view".to_string())?;
        if self.defer_pending_update_handoff_teardown(crate::DeferredHandoffTeardown::mutation(
            crate::DeferredHandoffMutation::CloseView {
                window: wid,
                tab: tab_id,
                view,
            },
        )) {
            return Err("view close deferred until update child rollback".to_string());
        }
        if let crate::tab_model::View::Native(native) = linked {
            if !self.prepare_native_leaf_close(
                wid,
                tab_id,
                view,
                native,
                crate::native_app::CloseScope::View,
                ClosePreflightVisibility::Interactive,
            )? {
                return Err("native view close is not ready".to_string());
            }
            if let Some(document) = self.native_runtime.document_id(native.instance)
                && !self.prepare_document_view_close(wid, tab_id, document, view)?
            {
                return Err("native document close is waiting for durability".to_string());
            }
        }
        let removed = self
            .windows
            .get_mut(&wid)
            .and_then(|window| window.tab_set.active_mut())
            .map(|tab| tab.remove_view(view));
        if removed != Some(crate::tab_model::RemoveLeaf::Removed) {
            return Err("focused view topology changed during close".to_string());
        }
        let removed_link = self
            .remove_view_link(view)
            .ok_or_else(|| "focused view ownership could not detach".to_string())?;
        if let crate::tab_model::View::Terminal(terminal) = removed_link
            && self.detach_session_view(terminal.session)
            && let Some(stable) = self
                .store
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .deregister_local(terminal.session)
        {
            crate::proxy::unpublish_session(&stable);
        }
        self.retain_closed_view(recovery);
        self.refresh_aggregate_tab_presentation(wid, tab_id);
        self.promote_terminal_projection_if_needed(wid, tab_id);
        self.resize_panes(wid);
        if self.frontmost_window == Some(wid) {
            self.sync_active_session();
        } else {
            self.sync_window(wid);
        }
        Ok(())
    }

    /// Close one whole native or heterogeneous tab. Every leaf's app readiness and the
    /// all-document durability batch are proven before any topology, runtime, pool, or
    /// document ownership edge changes.
    pub(crate) fn close_active_native_tab(&mut self, wid: WindowId) -> Result<(), String> {
        let (tab_id, leaves, exits_window) = self
            .windows
            .get(&wid)
            .and_then(|window| {
                let tab = window.tab_set.active()?;
                Some((tab.id, tab.root.leaves(), window.tab_set.len() == 1))
            })
            .ok_or_else(|| "window has no active tab".to_string())?;
        if self.defer_pending_update_handoff_teardown(crate::DeferredHandoffTeardown::mutation(
            crate::DeferredHandoffMutation::CloseTab {
                window: wid,
                tab: tab_id,
            },
        )) {
            return Err("tab close deferred until update child rollback".to_string());
        }
        let has_terminal = leaves.iter().any(|view| {
            matches!(
                self.view_store.get(*view),
                Some(crate::tab_model::View::Terminal(_))
            )
        });
        if has_terminal && !self.window_exit_close_allowed(wid, exits_window) {
            return Err("terminal close requires confirmation".to_string());
        }
        for view in &leaves {
            let Some(crate::tab_model::View::Native(native)) = self.view_store.get(*view).copied()
            else {
                continue;
            };
            if !self.prepare_native_leaf_close(
                wid,
                tab_id,
                *view,
                native,
                crate::native_app::CloseScope::Tab,
                ClosePreflightVisibility::Interactive,
            )? {
                return Err("one or more native leaves are not ready to close".to_string());
            }
        }
        if !self.prepare_document_tab_close_batch(wid, tab_id)? {
            return Err("native document close is waiting for a durable checkpoint".to_string());
        }
        // Capture the complete recursive tab only after every app/document close
        // obligation is ready, but before the live links retire. No process-local view,
        // instance, or tab identity and no draft bytes enter this bounded ledger.
        let reopen = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.tab_set.get(tab_id))
            .and_then(|tab| self.tab_restore_descriptor(tab));
        let original_index = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.tab_set.active_index())
            .unwrap_or(0);
        let terminal_sessions = leaves
            .iter()
            .filter_map(|view| {
                self.view_store
                    .get(*view)
                    .copied()
                    .and_then(crate::tab_model::View::terminal_session)
            })
            .collect::<Vec<_>>();
        let (tab, was_last) = {
            let ws = self
                .windows
                .get_mut(&wid)
                .ok_or_else(|| "unknown window".to_string())?;
            let was_last = ws.tab_set.len() == 1;
            let tab = ws
                .tab_set
                .remove(tab_id)
                .ok_or_else(|| "active tab disappeared".to_string())?;
            if was_last {
                ws.pending_close = true;
            }
            align_terminal_projection_to_active(ws, &self.view_store);
            (tab, was_last)
        };
        self.remove_tab_views(&tab);
        for session in terminal_sessions {
            self.teardown_session(session);
        }
        if let Some(tab) = reopen {
            self.retain_closed_tab(wid, original_index, tab);
        }
        if self.frontmost_window == Some(wid) {
            self.sync_active_session();
        } else {
            self.sync_window(wid);
        }
        debug_assert!(
            !was_last
                || self
                    .windows
                    .get(&wid)
                    .is_some_and(|ws| ws.pending_close && ws.tab_set.is_empty())
        );
        Ok(())
    }

    /// Reopen the most recently closed whole tab from its recursive identity-free
    /// descriptor. The original live window wins when it still exists; otherwise the
    /// current front window receives the tab. Reconstruction may freely fail—the ledger
    /// entry is consumed only after the complete topology and every leaf are committed.
    pub(crate) fn reopen_last_closed_tab(&mut self) -> Result<(), String> {
        let now_ms = self.lat_epoch.elapsed().as_millis() as u64;
        let candidate = self
            .closed_recovery
            .tabs
            .candidate(now_ms)
            .ok_or_else(|| "no recently closed tab".to_string())?;
        let target = self
            .windows
            .contains_key(&candidate.value.original_window)
            .then_some(candidate.value.original_window)
            .or(self.frontmost_window)
            .ok_or_else(|| "no window for reopened tab".to_string())?;
        self.restore_closed_tab_into_window(
            target,
            &candidate.value.tab,
            candidate.value.original_index,
        )?;
        self.closed_recovery
            .tabs
            .commit(candidate.token, now_ms)
            .map_err(|_| "closed-tab recovery candidate changed".to_string())?;
        Ok(())
    }

    pub(crate) fn can_reopen_closed_tab(&self) -> bool {
        let now_ms = self.lat_epoch.elapsed().as_millis() as u64;
        self.closed_recovery
            .tabs
            .candidate_snapshot(now_ms)
            .is_some()
            && !self.windows.is_empty()
    }

    pub(crate) fn reopen_last_closed_view(&mut self) -> Result<(), String> {
        let now_ms = self.lat_epoch.elapsed().as_millis() as u64;
        let candidate = self
            .closed_recovery
            .views
            .candidate(now_ms)
            .ok_or_else(|| "no recently closed view".to_string())?;
        let live_host = self
            .windows
            .get(&candidate.value.original_window)
            .and_then(|window| {
                window
                    .tab_set
                    .get(candidate.value.original_tab)
                    .map(|_| candidate.value.original_window)
            });
        let mut inserted = false;
        if let Some(wid) = live_host {
            let prior_projection = self.windows.get(&wid).and_then(|window| {
                terminal_projection_index(
                    &window.tab_set,
                    &self.view_store,
                    candidate.value.original_tab,
                )
            });
            let (view, _) = self.restore_closed_view_leaf(wid, &candidate.value.view)?;
            let path = crate::tab_model::SplitPath::from_branches(
                candidate
                    .value
                    .placement
                    .parent_path
                    .iter()
                    .map(|branch| match branch {
                        crate::restore::RestoreBranch::First => {
                            crate::tab_model::SplitBranch::First
                        }
                        crate::restore::RestoreBranch::Second => {
                            crate::tab_model::SplitBranch::Second
                        }
                    })
                    .collect(),
            );
            let branch = match candidate.value.placement.removed_branch {
                crate::restore::RestoreBranch::First => crate::tab_model::SplitBranch::First,
                crate::restore::RestoreBranch::Second => crate::tab_model::SplitBranch::Second,
            };
            let axis = match candidate.value.placement.axis {
                crate::restore::SplitKind::Horizontal => crate::tab_model::SplitAxis::Horizontal,
                crate::restore::SplitKind::Vertical => crate::tab_model::SplitAxis::Vertical,
            };
            inserted = self
                .windows
                .get_mut(&wid)
                .and_then(|window| {
                    let index = window
                        .tab_set
                        .tabs()
                        .iter()
                        .position(|tab| tab.id == candidate.value.original_tab)?;
                    let tab = window.tab_set.tab_at_mut(index)?;
                    tab.root
                        .restore_collapsed_leaf(
                            &path,
                            branch,
                            axis,
                            candidate.value.placement.ratio,
                            view,
                        )
                        .then(|| {
                            tab.focus = view;
                            tab.zoomed = false;
                        })
                })
                .is_some();
            if inserted {
                self.reconcile_reopened_view_projection(
                    wid,
                    candidate.value.original_tab,
                    prior_projection,
                );
                self.refresh_aggregate_tab_presentation(wid, candidate.value.original_tab);
                self.resize_panes(wid);
                self.resync_active_or_window(wid);
            } else {
                self.discard_reconstructed_view(view);
            }
        }
        if !inserted {
            let target = live_host
                .or(self.frontmost_window)
                .ok_or_else(|| "no window for reopened view".to_string())?;
            let index = self
                .windows
                .get(&target)
                .map_or(0, |window| window.tab_set.len());
            self.restore_closed_tab_into_window(
                target,
                &crate::restore::RestoredTab {
                    root: crate::restore::RestoredSplitTree::leaf(candidate.value.view.clone()),
                    focused_path: Vec::new(),
                    zoomed: false,
                },
                index,
            )?;
        }
        self.closed_recovery
            .views
            .commit(candidate.token, now_ms)
            .map_err(|_| "closed-view recovery candidate changed".to_string())?;
        Ok(())
    }

    fn discard_reconstructed_view(&mut self, view: crate::tab_model::ViewId) {
        let terminal = self
            .view_store
            .get(view)
            .copied()
            .and_then(crate::tab_model::View::terminal_session);
        self.remove_view_link(view);
        if let Some(session) = terminal {
            self.teardown_session(session);
        }
    }

    fn reconcile_reopened_view_projection(
        &mut self,
        wid: WindowId,
        tab_id: crate::tab_model::TabId,
        prior_projection: Option<usize>,
    ) {
        let Some(projection) = prior_projection else {
            return;
        };
        let Some((root, focus, zoomed, terminal)) = self.windows.get(&wid).and_then(|window| {
            let tab = window.tab_set.get(tab_id)?;
            Some((
                tab.root.clone(),
                tab.focus,
                tab.zoomed,
                tab.root.leaves().into_iter().all(|view| {
                    matches!(
                        self.view_store.get(view),
                        Some(crate::tab_model::View::Terminal(_))
                    )
                }),
            ))
        }) else {
            return;
        };
        if !terminal {
            if let Some(window) = self.windows.get_mut(&wid)
                && projection < window.layouts.len()
            {
                window.layouts.remove(projection);
                remove_terminal_projection(window, projection);
            }
            return;
        }
        let Some(layout) = self.restored_terminal_pane_layout(&root, focus) else {
            return;
        };
        let sessions = root
            .leaves()
            .into_iter()
            .filter_map(|view| {
                self.view_store
                    .get(view)
                    .copied()
                    .and_then(crate::tab_model::View::terminal_session)
            })
            .collect::<Vec<_>>();
        let Some(mut tree) = pane::PaneTree::rebuild(&layout, &sessions) else {
            return;
        };
        if zoomed {
            tree.toggle_zoom();
        }
        if let Some(slot) = self
            .windows
            .get_mut(&wid)
            .and_then(|window| window.layouts.get_mut(projection))
        {
            *slot = tree;
        }
    }

    pub(crate) fn can_reopen_closed_view(&self) -> bool {
        let now_ms = self.lat_epoch.elapsed().as_millis() as u64;
        self.closed_recovery
            .views
            .candidate_snapshot(now_ms)
            .is_some()
            && !self.windows.is_empty()
    }

    /// Close the PANE holding session `id` in window `window` (its reader hit EOF).
    /// With `--hold`, the pane is KEPT so the final output stays visible (the user
    /// closes it with Cmd-W). Returns `true` iff the app should now exit (the last
    /// pane of the last tab of the last window closed and `--hold` is off). A
    /// `Wake::Exit` for an already-closed/unknown session is a no-op.
    pub(crate) fn close_session(&mut self, window: WindowId, id: u64) -> bool {
        if self.hold {
            return false; // keep the window/pane open after the command exits
        }
        if self.defer_pending_update_handoff_teardown(crate::DeferredHandoffTeardown::mutation(
            crate::DeferredHandoffMutation::ExitSession(id),
        )) {
            return false;
        }
        // Resolve canonical stable identity first. A heterogeneous terminal leaf
        // intentionally has no `layouts` entry; treating that projection as truth
        // leaves an exited PTY pooled forever.
        let Some(location) = self.terminal_view_location(window, id) else {
            return false;
        };
        if !location.terminal_only {
            let _ = self.close_heterogeneous_terminal_view(window, location);
            return false;
        }
        let Some(tab) = self.windows.get(&window).and_then(|state| {
            terminal_projection_index(&state.tab_set, &self.view_store, location.tab_id)
        }) else {
            return false;
        };
        let outcome = self
            .windows
            .get_mut(&window)
            .map(|ws| ws.layouts[tab].close_pane(id));
        match outcome {
            Some(o) => self.apply_close_outcome(window, tab, o),
            None => false,
        }
    }

    /// LOGICAL core of the `Wake::Exit` handler (no winit/`el`): mark `session`
    /// `Exited`, then close it in EVERY window that views it. A CO-VIEWED
    /// (Cmd-Shift-O) session is displayed in more than one window but has a SINGLE
    /// reader thread, so its shell exit emits exactly ONE `Wake::Exit`; closing only
    /// the first owner would leave every OTHER viewer pinned to a dead, still-pooled
    /// pane. OCCURRENCES are collected FIRST (closing mutates `self.windows`) and a
    /// window id deliberately appears once PER stable view: sharing and then moving
    /// the shared tab can put multiple views of one session in the same window. Each
    /// in-window close detaches exactly one pool view. If the final occurrence is the
    /// window's last tab, it stays intact for whole-window teardown and that window is
    /// returned exactly once; the caller escalates it to a window close (the last
    /// window closing exits the app, the `ExitIffEmpty` invariant). This is the
    /// el-free twin the multi-window tests drive; `Wake::Exit` wraps it with
    /// `close_window`/`el.exit()`. An already-closed/unknown session finds no owner.
    pub(crate) fn exit_session_logical(&mut self, session: u64) -> Vec<WindowId> {
        self.store
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .set_state(session, session_store::SessionState::Exited);
        let mut owners = Vec::new();
        for (&window, state) in &self.windows {
            for tab in state.tab_set.tabs() {
                for view in tab.root.leaves() {
                    if self
                        .view_store
                        .get(view)
                        .copied()
                        .and_then(crate::tab_model::View::terminal_session)
                        == Some(session)
                    {
                        owners.push(window);
                    }
                }
            }
        }
        let mut to_close = Vec::new();
        for o in owners {
            // `close_session` intentionally leaves a last-tab occurrence intact for
            // `close_window_logical`; do not report or attempt that window twice if a
            // malformed/restored tree ever supplied another snapshot occurrence.
            if to_close.contains(&o) {
                continue;
            }
            if self.close_session(o, session) {
                to_close.push(o);
            }
        }
        to_close
    }

    /// A click in window `wid`'s tab strip at column `col`: resolve it against that
    /// window's cached segments ([`WindowState::tab_segments`]) and SWITCH / CLOSE /
    /// open a tab. A click on bare strip background is ignored. The CLOSE of the last
    /// tab signals the window to close via `ws.pending_close` (the mouse handler has
    /// no `ActiveEventLoop`), mirroring Cmd-W. Repaints after any state change.
    ///
    /// A SECOND press on the same chip within [`crate::MULTI_CLICK_MS`] opens the
    /// inline rename editor — the in-grid twin of the macOS strip's
    /// `event.clickCount() == 2`, which is a service AppKit provides and winit does
    /// not. The streak lives on `ws.strip_press`, never on the grid's
    /// `last_press`/`click_count`: sharing those would both take a blocking
    /// terminal lock on a chrome path and corrupt the grid's word/line selection.
    pub(crate) fn handle_tab_strip_click(&mut self, wid: WindowId, col: u16) {
        let Some(segs) = self.windows.get(&wid).map(|ws| ws.tab_segments.clone()) else {
            return;
        };
        let Some(hit) = tab_bar::hit_test(&segs, col) else {
            // Bare strip background is not a chip, so it breaks the chip streak —
            // and it is still "away" from the field, so it commits an open rename.
            // Both must happen whatever else the press becomes below.
            let now = std::time::Instant::now();
            let band_repeat = self.windows.get(&wid).is_some_and(|ws| {
                ws.strip_band_press
                    .is_some_and(|at| now.duration_since(at).as_millis() <= crate::MULTI_CLICK_MS)
            });
            // `clear_strip_press` also clears the BAND streak (any other press
            // breaks a band double-click), so the repeat test above runs first
            // and the first-press re-arm below runs after.
            self.clear_strip_press(wid);
            self.settle_rename_edit(wid);
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.strip_band_press = (!band_repeat).then_some(now);
            }
            // W2: the strip's background IS caption space, so give it the two
            // caption reflexes — press-drag moves the window, double-click
            // toggles maximize — the Windows Terminal feel, with ZERO non-client
            // surgery. The window stays plainly decorated (`WM_NCCALCSIZE` at
            // DefWindowProc), which is what keeps Snap Layouts / Alt+Space /
            // Win+arrow correct today; `drag_window` just posts
            // `WM_NCLBUTTONDOWN(HTCAPTION)`, entering the SAME modal move loop a
            // real caption press would, so snap-while-dragging works too. The
            // double-click is our own tiny FSM (`strip_band_press`) rather than
            // the OS's because a client-area press never becomes
            // `WM_NCLBUTTONDBLCLK`. Gated to Windows: the macOS strip rows are 0
            // by default (its strip lives in the real titlebar, which already
            // drags/zooms natively), and the Linux daily-driver lane has not
            // ratified winit's per-WM drag behaviour — the band press there
            // stays the pre-W2 no-op. Ordering trap the FSM avoids: the FIRST
            // press starts a drag; a click-release-click lands here as a repeat
            // and must NOT also start a drag (the maximize toggle would fight
            // the modal loop), which is why the repeat arm never calls
            // `drag_window`. Headless / pre-attach (`os_window: None`) is a
            // natural no-op, so the strip unit tests exercise the streak
            // bookkeeping without an OS window.
            #[cfg(windows)]
            if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
                if band_repeat {
                    w.set_maximized(!w.is_maximized());
                } else {
                    let _ = w.drag_window();
                }
            }
            return;
        };
        match hit {
            // Target the CLICKED window, not the frontmost — Close already does, so
            // Select/NewTab must too (a click on a non-front window's strip must act
            // on THAT window even if focus hasn't transferred yet).
            tab_bar::TabHit::Select(i) => {
                let tab = self
                    .windows
                    .get(&wid)
                    .and_then(|ws| ws.tab_set.tabs().get(i))
                    .map(|tab| tab.id);
                let now = std::time::Instant::now();
                let repeat = tab.is_some_and(|tab| {
                    self.windows.get(&wid).is_some_and(|ws| {
                        ws.strip_press.is_some_and(|(at, was)| {
                            was == tab
                                && now.duration_since(at).as_millis() <= crate::MULTI_CLICK_MS
                        })
                    })
                });
                if let Some(ws) = self.windows.get_mut(&wid) {
                    ws.strip_press = tab.map(|tab| (now, tab));
                    // A chip press is not part of a BAND double-click either.
                    ws.strip_band_press = None;
                }
                // A click on a DIFFERENT chip is a click away and COMMITS, matching
                // the macOS `Wake::SelectTab` arm and every other exit. A click on
                // the chip already being edited is not leaving the field.
                let editing_this = self
                    .inline_rename_edit(wid)
                    .map(|edit| edit.tab)
                    .is_some_and(|edited| Some(edited) == tab);
                if !editing_this {
                    self.settle_rename_edit(wid);
                }
                match (repeat, tab) {
                    (true, Some(tab)) => {
                        self.begin_session_rename(wid, tab);
                    }
                    _ => self.switch_tab_in(wid, i),
                }
            }
            tab_bar::TabHit::NewTab => {
                self.clear_strip_press(wid);
                self.settle_rename_edit(wid);
                self.open_tab_in(wid);
            }
            // The leading `↻` update alert (off-macOS chrome — menu-adjacent there):
            // ONE CLICK applies the staged build (the owner's "click-upgrade" ask; the
            // old click-to-details flow was the "too many clicking" complaint). With
            // nothing actually staged it falls back to the details overlay — never a
            // blind restart, never a dead click. Details stay reachable via the App
            // menu's "Check for Updates…".
            tab_bar::TabHit::Update => {
                self.clear_strip_press(wid);
                self.settle_rename_edit(wid);
                self.apply_update_or_details();
            }
            tab_bar::TabHit::Close(i) => {
                self.clear_strip_press(wid);
                // The tab is about to stop existing. Settling FIRST means a name
                // typed into the chip you then close is kept — the same order the
                // menu-bar and context-menu Close Tab now follow.
                self.settle_rename_edit(wid);
                if self.close_tab_at(wid, i)
                    && let Some(ws) = self.windows.get_mut(&wid)
                {
                    ws.pending_close = true;
                }
            }
        }
        if let Some(ws) = self.windows.get(&wid)
            && let Some(w) = &ws.os_window
        {
            w.request_redraw();
        }
    }

    /// Break the strip's double-click streaks — the chip one AND the bare-band
    /// one. Any hit that is not "the same chip again" ends the chip streak, so a
    /// Close/`+`/`↻`/background click between two chip presses cannot be counted
    /// as a double-click on the chip; symmetrically, any press that is not bare
    /// band ends the band streak (W2's double-click-maximize), so
    /// chip-then-band cannot read as a band double-click. The band arm in
    /// `handle_tab_strip_click` samples its streak BEFORE calling this and
    /// re-arms after.
    pub(crate) fn clear_strip_press(&mut self, wid: WindowId) {
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.strip_press = None;
            ws.strip_band_press = None;
        }
    }

    /// C5 — the composed CONTEXT-MENU model for tab `index` of `wid`, or an
    /// empty vec when that tab has no menu (a native app tab; an out-of-range
    /// index). One call into the SAME epoch-gated
    /// [`Self::composed_session_chrome`] the strip's tooltips and the `chrome`
    /// verb's `tab-menu` lines already read, so the popup can never show an item
    /// the introspection mirror does not — the whole point of a single composer.
    ///
    /// It resolves the label through [`Self::tab_titles`] rather than reading
    /// the presentation directly because the composer's first identity line IS
    /// the chip label, and `tab_titles` is the one place that chain (user title
    /// ▸ OSC title ▸ `~`-relative cwd) is resolved.
    pub(crate) fn tab_context_menu_model(
        &mut self,
        wid: WindowId,
        index: usize,
    ) -> Vec<crate::session_chrome::TabMenuEntry> {
        let Some(session) = self.windows.get(&wid).and_then(|ws| {
            let tab = ws.tab_set.tabs().get(index)?;
            self.view_store
                .get(tab.focus)
                .copied()
                .and_then(crate::tab_model::View::terminal_session)
        }) else {
            return Vec::new();
        };
        let titles = self.tab_titles(wid);
        let label = titles.get(index).map_or("", String::as_str);
        self.composed_session_chrome(wid, session, label).menu
    }

    /// C5 — pop the tab context menu over tab `index` of `wid`, anchored at
    /// strip column `anchor_col`.
    ///
    /// Windows' first reflex on a tab is a right-click, and its first rule is
    /// that right-clicking a background tab SELECTS it: the menu's subject must
    /// be the thing the user is looking at, and every one of its actions
    /// (rename, copy, close) reads as being about the front tab. So the switch
    /// happens first, and only then the card pops. (macOS's `NSMenu` on a
    /// background chip does NOT select — AppKit's convention — which is exactly
    /// why this behaviour lives here and not in the shared composer.)
    ///
    /// Returns whether a menu actually opened: a native app tab composes an
    /// EMPTY model (native surfaces own richer affordances of their own), and a
    /// caller that gets `false` must let the gesture fall through to whatever it
    /// would have done before.
    ///
    /// Both real callers (the strip's right press, Shift+F10 / the Menu key)
    /// are `#[cfg(windows)]` — macOS pops a real `NSMenu` off the native strip
    /// instead, and the card is not offered on Linux (see the `Chrome` arm in
    /// `app_mouse`) — so off Windows this is reachable only from tests.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn open_tab_context_menu(
        &mut self,
        wid: WindowId,
        index: usize,
        anchor_col: u16,
        keyboard: bool,
    ) -> bool {
        // No strip, no menu. The card hangs BELOW the band, so with
        // `tab_strip_rows = 0` it would float at the top of the terminal grid
        // attached to nothing — and there is no chip to have aimed at either.
        // (The mouse path cannot reach here with the strip off, since
        // `strip_col_at` already declines; the keyboard path can.)
        if !self.tab_strip_enabled() {
            return false;
        }
        let Some(tab) = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.tab_set.tabs().get(index))
            .map(|tab| tab.id)
        else {
            return false;
        };
        let entries = self.tab_context_menu_model(wid, index);
        if entries.is_empty() {
            return false;
        }
        // A chip press is a press AWAY from an open rename field, and it breaks
        // both strip double-click streaks — the same bookkeeping the LEFT chip
        // path does, for the same reasons (a right press between two left
        // presses must not complete a double-click, and a name typed into the
        // field you then right-click is kept, not dropped).
        self.clear_strip_press(wid);
        self.settle_rename_edit(wid);
        if self.windows.get(&wid).and_then(|ws| ws.tab_set.active_index()) != Some(index) {
            self.switch_tab_in(wid, index);
        }
        // Seeded to the first enabled action only for a KEYBOARD open: a card
        // that pops with a row already lit under a pointer that has not moved is
        // an invitation to activate the wrong thing on the next click.
        let highlight = keyboard
            .then(|| crate::tab_menu::first_selectable(&entries, entries.len()))
            .flatten();
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.tab_menu = Some(crate::tab_menu::TabMenu {
                tab,
                index,
                entries,
                anchor_col,
                highlight,
                keyboard,
            });
            // Stale from a previous pop; the next splice records the real one.
            ws.tab_menu_rect = None;
            if let Some(w) = &ws.os_window {
                w.request_redraw();
            }
        }
        true
    }

    /// C5 — the KEYBOARD entry point (Shift+F10 / the Menu key): pop the
    /// context menu for the window's ACTIVE tab, anchored at that chip's own
    /// left edge rather than at a pointer that was never involved.
    ///
    /// This is what makes the popup usable without a mouse, and on Windows it
    /// is not a nicety: Shift+F10 is the OS-wide "show the context menu for the
    /// focused thing" chord, and a surface that has a right-click menu but no
    /// keyboard route to it is unreachable for anyone driving by keyboard.
    /// Returns whether a menu opened, so the caller can let the chord fall
    /// through when it did not (a native app tab, no strip, no tabs).
    ///
    /// Its real callers are the two `#[cfg(windows)]` chord arms — `on_key`'s
    /// and the convergence seam's `tab_menu_input_event` — which is what makes
    /// `aterm ctl key menu` exercise the same path the physical key does. ⇧F10
    /// is not a menu chord on macOS, and Linux does not offer the card.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn open_active_tab_context_menu(&mut self, wid: WindowId) -> bool {
        let Some(index) = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.tab_set.active_index())
        else {
            return false;
        };
        // The chip's own start column, read off the SAME laid-out segments the
        // strip painted, so the card lines up with the tab it belongs to. A
        // window whose segments have not been laid out yet (no frame drawn)
        // falls back to column 0 — flush left, still under the band.
        let anchor = self
            .windows
            .get(&wid)
            .and_then(|ws| {
                ws.tab_segments
                    .iter()
                    .find(|seg| seg.kind == tab_bar::TabHit::Select(index))
                    .map(|seg| seg.start_col)
            })
            .unwrap_or(0);
        self.open_tab_context_menu(wid, index, anchor, true)
    }

    /// C5 — dismiss any open tab context menu on `wid`. Returns whether one was
    /// actually open (callers use it to decide whether the dismissing gesture
    /// was consumed). Requests the repaint that ERASES the card; the
    /// `RepaintKey`'s `tab_menu_fp` term is what keeps that frame from being
    /// swallowed by the settled-screen early-out.
    pub(crate) fn close_tab_menu(&mut self, wid: WindowId) -> bool {
        let Some(ws) = self.windows.get_mut(&wid) else {
            return false;
        };
        if ws.tab_menu.take().is_none() {
            ws.tab_menu_rect = None;
            return false;
        }
        ws.tab_menu_rect = None;
        if let Some(w) = &ws.os_window {
            w.request_redraw();
        }
        true
    }

    /// C5 — activate entry `i` of the open menu on `wid` and dismiss the card.
    ///
    /// The chosen action is POSTED as [`crate::Wake::TabMenuAction`] rather than
    /// dispatched inline, for two reasons that both matter:
    ///   * `App::dispatch_tab_menu_action` needs an `&ActiveEventLoop` (the
    ///     `Close Tab` arm escalates a last-tab close into a window teardown),
    ///     and neither the mouse nor the key handler has one;
    ///   * it puts this popup on the byte-same path the macOS `NSMenu` relay
    ///     already uses, carrying the STABLE `TabId` captured at pop time — so
    ///     the two platforms share one dispatcher, one targeting rule, and one
    ///     "the tab went away, log it and drop the action" fallback.
    ///
    /// A no-op (beyond the dismiss) when `i` is not a live action row; the
    /// caller has already hit-tested, this is the belt.
    pub(crate) fn activate_tab_menu_entry(&mut self, wid: WindowId, i: usize) {
        let chosen = self.windows.get(&wid).and_then(|ws| {
            let menu = ws.tab_menu.as_ref()?;
            match menu.entries.get(i)? {
                crate::session_chrome::TabMenuEntry::Action {
                    action,
                    enabled: true,
                    ..
                } => Some((menu.tab, *action)),
                _ => None,
            }
        });
        self.close_tab_menu(wid);
        if let Some((tab, action)) = chosen
            && let Some(proxy) = self.proxy.clone()
        {
            let _ = proxy.send_event(crate::Wake::TabMenuAction {
                window: wid,
                tab,
                action,
            });
        }
    }

    /// Cancel a live edit whose TAB has gone away. The in-grid twin of the macOS
    /// strip's `sync_rename_overlay`, and never a commit: the user was naming
    /// something that no longer exists. Without it a background close or a
    /// `tab close` verb would leave an invisible field owning the keyboard.
    pub(crate) fn reap_stale_rename_edit(&mut self, wid: WindowId) {
        let Some(tab) = self.inline_rename_edit(wid).map(|edit| edit.tab) else {
            return;
        };
        if self.tab_index_for_id(wid, tab).is_none() {
            self.end_session_rename_editor(wid);
        }
    }

    /// Close the ENTIRE tab at index `i` of window `wid` (every pane in it), as a
    /// unit — the tab strip's close `x` closes a whole tab, unlike Cmd-W which closes
    /// one pane. DRAINS each of the tab's panes' sessions and `pool.detach`es each
    /// (the last view closes that PTY master), drops its pane tree, and keeps
    /// the terminal projection aligned when applicable. Returns `true` iff that was
    /// the LAST canonical tab (the caller signals the window to close). Out-of-range
    /// `i` is a no-op (returns `false`).
    ///
    /// TRUST anchor: the `Close` action of the ty-proven `tab_strip` machine
    /// (`tab_strip_model()`) — shrinks the tab set and MUST re-sync the clicked
    /// window's native strip (the non-front-window re-sync this fn now performs).
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "tab_strip",
            action = "Close",
            project = "aterm_gui::tab_strip_conformance::project"
        )
    )]
    pub(crate) fn close_tab_at(&mut self, wid: WindowId, i: usize) -> bool {
        let Some((tab_id, is_native, terminal_index, canonical_count)) =
            self.windows.get(&wid).and_then(|ws| {
                let tab = ws.tab_set.tab_at(i)?;
                Some((
                    tab.id,
                    !is_terminal_tab(tab, &self.view_store),
                    terminal_projection_index(&ws.tab_set, &self.view_store, tab.id),
                    ws.tab_set.len(),
                ))
            })
        else {
            return false;
        };
        if self.defer_pending_update_handoff_teardown(crate::DeferredHandoffTeardown::mutation(
            crate::DeferredHandoffMutation::CloseTab {
                window: wid,
                tab: tab_id,
            },
        )) {
            return false;
        }
        if is_native {
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.tab_set.switch_to(tab_id);
            }
            return match self.close_active_native_tab(wid) {
                Ok(()) => canonical_count == 1,
                Err(_) => {
                    // Keep the refused tab selected: its app-owned banner and
                    // the exact recovery palette explain why topology stayed
                    // intact. Restoring the prior tab made close look inert.
                    self.resync_active_or_window(wid);
                    false
                }
            };
        }
        let Some(projection_index) = terminal_index else {
            return false;
        };
        // M2 quit-safety: closing the window's LAST tab (whole tab, all its panes)
        // exits the window — refuse a stray such close while a job runs (arming the
        // confirm), mirroring the red ✕ / Cmd-Q. `false` here means "did not close"
        // (the warning is armed); the caller leaves the window open. Closing one tab
        // among several never escalates, so it is never blocked.
        let exits_window = canonical_count == 1;
        if !self.window_exit_close_allowed(wid, exits_window) {
            return false;
        }
        let recovery = self
            .windows
            .get(&wid)
            .and_then(|window| window.tab_set.get(tab_id))
            .and_then(|tab| self.tab_restore_descriptor(tab));
        // Preserve the historical last-tab handoff: the caller closes the whole
        // window, whose teardown drains this terminal and its views exactly once.
        if exits_window {
            if let Some(tab) = recovery {
                self.retain_closed_tab(wid, i, tab);
            }
            return true;
        }
        let Some(ws) = self.windows.get_mut(&wid) else {
            return false;
        };
        // Drain EVERY pane's session of the removed tab and detach each (NOT a Vec
        // remove): DETACH the pool view FIRST (the last view drops the Session,
        // closing its PTY master), and deregister from the process-wide registry
        // ONLY when that detach actually dropped the session. A shared (Cmd-Shift-O)
        // session still viewed in another window keeps its single store entry while a
        // view remains; a genuinely-closed id then fail-closes a later @<selector>.
        let closing = ws.layouts[projection_index].sessions();
        ws.layouts.remove(projection_index);
        remove_terminal_projection(ws, projection_index);
        let stable_tab = ws.tab_set.remove(tab_id).expect("stable tab mirror exists");
        align_terminal_projection_to_active(ws, &self.view_store);
        self.remove_tab_views(&stable_tab);
        for sid in closing {
            if self.detach_session_view(sid)
                && let Some(stable) = self
                    .store
                    .write()
                    .unwrap_or_else(|p| p.into_inner())
                    .deregister_local(sid)
            {
                crate::proxy::unpublish_session(&stable);
            }
        }
        if let Some(tab) = recovery {
            self.retain_closed_tab(wid, i, tab);
        }
        // Re-sync the CLICKED window — its active index shifted when the tab was
        // removed. Mirror `open_tab_in`'s owner-sync: the global handles follow the
        // FRONT window, but a NON-front window must still re-sync its OWN mirror +
        // native tab strip (`sync_window` → `refresh_window_tabs`), or it keeps a
        // PHANTOM segment past the closed tab. (Proven by `tab_strip` + its Tier-1
        // conformance: closing a tab in a non-front window must not desync its strip.)
        if self.frontmost_window == Some(wid) {
            self.sync_active_session();
        } else {
            self.sync_window(wid);
        }
        false
    }

    /// Live structural oracle for the window/session model (debug builds only;
    /// `debug_assert`-ed after each tab mutation, mirroring how the engine fuzz
    /// harness wires grid invariants as an always-on oracle). It must hold at
    /// every STABLE point:
    ///   - there is always ≥1 logical window and `frontmost_window` names one;
    ///   - every non-closing window has ≥1 canonical tab;
    ///   - `tabs`/`layouts` are an exact terminal-only projection and may both be
    ///     empty for a native-only window;
    ///   - when that projection is non-empty, the window's parked compatibility
    ///     mirror id equals `layouts[tabs.active].focus()`; and
    ///   - every pane's session is owned by the pool (resolvable).
    ///
    /// This is the CODE-LEVEL shadow of the ty-proven `window_routing_model`'s
    /// `ExitIffEmpty`/`FrontmostLive`/`FrontmostAllocated` (crates/aterm-spec).
    //
    // NOT `#[cfg(debug_assertions)]`: the `debug_assert!` call sites type-check
    // their condition in release too (the macro only gates EXECUTION, not
    // compilation), so a debug-only definition fails the release build with
    // E0599. Define it unconditionally; `allow(dead_code)` silences the
    // release-only "never called" warning (debug builds do call it).
    #[cfg_attr(not(debug_assertions), allow(dead_code))]
    pub(crate) fn structural_invariants_ok(&self) -> bool {
        let Some(fid) = self.frontmost_window else {
            return false;
        };
        if !self.windows.contains_key(&fid) {
            return false;
        }
        self.windows.values().all(|ws| {
            let terminal_tabs: Vec<&crate::tab_model::Tab> = ws
                .tab_set
                .tabs()
                .iter()
                .filter(|tab| {
                    tab.root.leaves().into_iter().all(|view| {
                        matches!(
                            self.view_store.get(view),
                            Some(crate::tab_model::View::Terminal(_))
                        )
                    })
                })
                .collect();
            let active_tab = ws.tab_set.active();
            let front_truth = match active_tab.and_then(|tab| {
                self.view_store
                    .get(tab.focus)
                    .copied()
                    .map(|view| (tab.focus, view))
            }) {
                Some((view, crate::tab_model::View::Terminal(terminal))) => {
                    ws.front_content
                        == Some(crate::front_content::FrontContent::Terminal {
                            view,
                            session: terminal.session,
                        })
                        && ws.front_terminal().is_some_and(|mirror| {
                            self.pool.get(terminal.session).is_some_and(|session| {
                                mirror.session == terminal.session
                                    && mirror.master == session.master
                                    && std::sync::Arc::ptr_eq(&mirror.term, &session.term)
                                    && std::sync::Arc::ptr_eq(&mirror.sink, &session.ctx.sink)
                            })
                        })
                }
                Some((view, crate::tab_model::View::Native(native))) => {
                    ws.front_content
                        == Some(crate::front_content::FrontContent::Native {
                            instance: native.instance,
                            view,
                        })
                        && ws.active_terminal.is_none()
                }
                None => {
                    ws.pending_close && ws.front_content.is_none() && ws.active_terminal.is_none()
                }
            };
            let active_terminal = active_tab.and_then(|active| {
                terminal_tabs
                    .iter()
                    .position(|candidate| candidate.id == active.id)
            });
            let focus_truth = match ws.window_focus {
                crate::front_content::WindowFocus::Content(view) => {
                    ws.front_content
                        .map(crate::front_content::FrontContent::view)
                        == Some(view)
                }
                crate::front_content::WindowFocus::Host => ws.front_content.is_none(),
                crate::front_content::WindowFocus::Overlay => true,
            };
            let projection_shape = ws.tabs.count == ws.layouts.len()
                && if ws.layouts.is_empty() {
                    ws.tabs.active == 0
                } else {
                    ws.tabs.active < ws.layouts.len()
                };

            (!ws.tab_set.is_empty() || ws.pending_close)
                && projection_shape
                && terminal_tabs.len() == ws.layouts.len()
                && ws.tab_set.invariant_holds(&self.view_store)
                && front_truth
                && focus_truth
                // An all-terminal active tab has a matching compatibility entry
                // and the optional front capability names its focused session.
                && active_terminal.is_none_or(|index| {
                    index == ws.tabs.active
                        && ws
                            .front_terminal()
                            .is_some_and(|mirror| mirror.session == ws.layouts[index].focus())
                })
                && ws.layouts.iter().zip(&terminal_tabs).all(|(layout, tab)| {
                    let projected = tab.root.map(&mut |view| {
                        self.view_store
                            .get(*view)
                            .copied()
                            .and_then(crate::tab_model::View::terminal_session)
                    });
                    projected == layout.map_sessions(Some)
                        && self
                            .view_store
                            .get(tab.focus)
                            .copied()
                            .and_then(crate::tab_model::View::terminal_session)
                            == Some(layout.focus())
                        && tab.zoomed == layout.is_zoomed()
                })
                && ws.tab_set.tabs().iter().all(|tab| {
                    tab.root.leaves().into_iter().all(|view| {
                        match self.view_store.get(view).copied() {
                            Some(crate::tab_model::View::Terminal(terminal)) => {
                                self.pool.get(terminal.session).is_some()
                            }
                            Some(crate::tab_model::View::Native(native)) => {
                                self.native_runtime.app(native.instance).is_some()
                                    && self.native_runtime.view_state(view).is_some()
                            }
                            None => false,
                        }
                    })
                })
        })
    }

    /// Apply a [`pane::CloseOutcome`] from tab `tab` of window `wid`, keeping the
    /// pool, `layouts`, and `tabs` consistent, and re-mirror the focused pane.
    /// Returns `true` iff that was the last pane of the last tab of the last window
    /// (caller signals the window to close). Detaching the removed view drops the
    /// `Session` (closing its PTY master) iff it was the last view; every OTHER pane
    /// is untouched.
    pub(crate) fn apply_close_outcome(
        &mut self,
        wid: WindowId,
        tab: usize,
        outcome: pane::CloseOutcome,
    ) -> bool {
        match outcome {
            pane::CloseOutcome::Collapsed { .. } => {
                let closed_session = outcome.closed();
                let closed_view = self.windows.get(&wid).and_then(|ws| {
                    let tab_id = terminal_tab_id_at(&ws.tab_set, &self.view_store, tab)?;
                    ws.tab_set
                        .get(tab_id)?
                        .root
                        .leaves()
                        .into_iter()
                        .find(|view| {
                            self.view_store
                                .get(*view)
                                .copied()
                                .and_then(crate::tab_model::View::terminal_session)
                                == Some(closed_session)
                        })
                });
                // Re-project while the retiring focused view still resolves. The
                // canonical terminal-tab classifier intentionally consults `tab.focus`;
                // deleting that identity first made the surviving split tab disappear
                // from the terminal projection exactly during collapse.
                let synced = self.sync_tab_model_from_layout(wid, tab);
                debug_assert!(synced);
                if let Some(view) = closed_view {
                    self.view_store.remove(view);
                }
                // The tab survives (a sibling remained). Detach just the closed
                // pane's view; the sibling's reader thread stays alive.
                self.teardown_session(outcome.closed());
                // The active tab's geometry changed (a sibling grew); the closed
                // tab may not be the active one (background EOF), but re-laying the
                // active tab is cheap and correct. Resize panes to the new layout.
                self.resize_panes(wid);
                // The active pane MOVED (the focused pane collapsed onto its sibling);
                // re-point the global handle, not just the per-window mirror, so a
                // control verb can't keep driving the just-closed pane's session.
                self.resync_active_or_window(wid);
                // A pane CLOSED: retire its effect state (episodes, done marks,
                // parked grid) so a departed pane leaves nothing resident. The
                // surviving panes keep theirs — the collapse only resizes them.
                self.reset_pane_space_decorations(wid);
                false
            }
            pane::CloseOutcome::LastPane { .. } => {
                // That pane was the tab's only one → the tab closes. `tabs.close`
                // returns true iff it was the LAST tab (caller signals the window to
                // close; the last window closing exits the app).
                let Some((stable_id, canonical_last)) = self.windows.get(&wid).and_then(|ws| {
                    Some((
                        terminal_tab_id_at(&ws.tab_set, &self.view_store, tab)?,
                        ws.tab_set.len() == 1,
                    ))
                }) else {
                    return false;
                };
                if canonical_last {
                    return true;
                }
                // Detach EVERY pane's view of the removed tab (a LastPane close has
                // exactly one, but draining `sessions()` is robust and explicit),
                // then drop the tab's tree.
                let drained: Vec<u64> = self
                    .windows
                    .get(&wid)
                    .map(|ws| ws.layouts[tab].sessions())
                    .unwrap_or_default();
                let stable_tab = self.windows.get_mut(&wid).and_then(|ws| {
                    ws.layouts.remove(tab);
                    remove_terminal_projection(ws, tab);
                    let removed = ws.tab_set.remove(stable_id);
                    align_terminal_projection_to_active(ws, &self.view_store);
                    removed
                });
                if let Some(stable_tab) = stable_tab {
                    self.remove_tab_views(&stable_tab);
                }
                for sid in drained {
                    self.teardown_session(sid);
                }
                // The active TAB changed (the closed tab's neighbor became active);
                // re-point the global handle so verbs follow the close-induced switch.
                self.resync_active_or_window(wid);
                false
            }
        }
    }

    /// Tear down exactly the session `id`: DETACH its pool view (which drops its
    /// `Session` — closing its PTY master, ending its reader thread — iff it was the
    /// LAST view) FIRST, then deregister from the process-wide registry (P1.1) ONLY
    /// when that detach actually dropped the session. A REFCOUNTED (Cmd-Shift-O
    /// shared) session still live in another window must NOT be deregistered while a
    /// view remains: `pool.detach` returns `true` iff the view count hit 0. A later
    /// `@<selector>` to a genuinely-closed id fail-closes (unknown -> Deny).
    pub(crate) fn teardown_session(&mut self, id: u64) {
        let dropped = self.detach_session_view(id);
        if dropped
            && let Some(stable) = self
                .store
                .write()
                .unwrap_or_else(|p| p.into_inner())
                .deregister_local(id)
        {
            crate::proxy::unpublish_session(&stable);
        }
    }
}

#[cfg(test)]
mod mixed_tab_tests {
    use super::*;

    fn assert_terminal_fully_retired(app: &App, session: u64, view: crate::tab_model::ViewId) {
        assert!(app.pool.get(session).is_none(), "pool owner must be gone");
        assert_eq!(app.pool.views(session), None, "pool refcount must be gone");
        assert!(
            app.view_store.get(view).is_none(),
            "stable terminal view link must be gone"
        );
        assert!(
            app.store
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .by_local(session)
                .is_none(),
            "process session registry entry must be gone"
        );
        assert!(app.windows.values().all(|window| {
            window
                .tab_set
                .tabs()
                .iter()
                .all(|tab| !tab.root.contains(view))
                && window
                    .layouts
                    .iter()
                    .all(|layout| !layout.contains(session))
                && !window.leaf_render_cache.contains_key(&view)
        }));
    }

    fn assert_native_sibling_survives(
        app: &App,
        instance: crate::tab_model::AppInstanceId,
        view: crate::tab_model::ViewId,
    ) {
        assert!(matches!(
            app.view_store.get(view),
            Some(crate::tab_model::View::Native(native)) if native.instance == instance
        ));
        assert!(app.native_runtime.app(instance).is_some());
        assert!(app.native_runtime.view_state(view).is_some());
    }

    fn enter_settings_draft(app: &mut App, wid: WindowId, view: crate::tab_model::ViewId) {
        app.dispatch_native_view_event(
            wid,
            view,
            crate::native_app::AppEvent::FocusChanged(Some(crate::native_ui::UiKey::new(format!(
                "settings/control/{}",
                crate::prefs::EDIT_FONT_FAMILY
            )))),
        )
        .unwrap();
        app.dispatch_native_view_event(
            wid,
            view,
            crate::native_app::AppEvent::TextInput(crate::native_app::TextInputEvent::SelectAll),
        )
        .unwrap();
        app.dispatch_native_view_event(
            wid,
            view,
            crate::native_app::AppEvent::TextInput(crate::native_app::TextInputEvent::Commit(
                "Topology Draft Mono".to_string(),
            )),
        )
        .unwrap();
    }

    fn assert_exact_settings_close_recovery(app: &App, wid: WindowId) {
        let lines = app.windows[&wid]
            .palette()
            .expect("blocked close opens recovery palette")
            .controls_lines();
        assert!(
            lines
                .first()
                .is_some_and(|line| line.contains("rows=2 shown=2")),
            "only app-supplied recovery is surfaced: {lines:?}"
        );
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.contains("target=native"))
                .count(),
            2,
            "exactly two native recovery rows: {lines:?}"
        );
        for action in ["settings/drafts/review", "settings/drafts/discard-all"] {
            assert!(
                lines.iter().any(|line| {
                    line.contains("target=native")
                        && line.contains(&format!("action={action}"))
                        && line.contains("enabled=true")
                }),
                "recovery exposes {action}: {lines:?}"
            );
        }
    }

    #[test]
    fn cmd_w_mixed_leaf_preserves_settings_draft_and_surfaces_exact_recovery() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (instance, settings) = app.active_native_view(wid).unwrap();
        let (_, terminal) =
            app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
        let tab = app.windows[&wid].tab_set.active_id().unwrap();
        app.windows
            .get_mut(&wid)
            .unwrap()
            .tab_set
            .active_mut()
            .unwrap()
            .set_focus(settings);
        app.sync_window(wid);
        enter_settings_draft(&mut app, wid, settings);
        let before = app.windows[&wid].tab_set.get(tab).unwrap().root.leaves();

        assert_eq!(app.close_active_tab(), None);
        assert_eq!(
            app.windows[&wid].tab_set.get(tab).unwrap().root.leaves(),
            before,
            "blocked Cmd-W cannot detach either mixed leaf"
        );
        assert!(before.contains(&terminal));
        assert_eq!(app.active_native_view(wid), Some((instance, settings)));
        assert_exact_settings_close_recovery(&app, wid);
        assert!(
            !app.native_runtime
                .presentation(instance, settings)
                .unwrap()
                .closable
        );
        assert!(app.structural_invariants_ok());
    }

    #[test]
    fn deferred_mixed_leaf_replay_keeps_the_exact_close_blocker_visible() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (instance, settings) = app.active_native_view(wid).unwrap();
        let (_, terminal) =
            app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
        let tab = app.windows[&wid].tab_set.active_id().unwrap();
        app.windows
            .get_mut(&wid)
            .unwrap()
            .tab_set
            .active_mut()
            .unwrap()
            .set_focus(settings);
        app.sync_window(wid);
        enter_settings_draft(&mut app, wid, settings);
        let before = app.windows[&wid].tab_set.get(tab).unwrap().root.leaves();

        // Simulate a close replay after an update handoff rollback while the user
        // has since selected another tab. A refused replay must not restore that
        // newer selection over the reducer-owned recovery surface.
        app.switch_tab_in(wid, 0);
        assert!(app.active_native_view(wid).is_none());
        assert_eq!(
            app.replay_deferred_handoff_view_close(wid, tab, settings),
            None
        );
        assert_eq!(
            app.windows[&wid].tab_set.get(tab).unwrap().root.leaves(),
            before
        );
        assert!(before.contains(&terminal));
        assert_eq!(app.active_native_view(wid), Some((instance, settings)));
        assert_exact_settings_close_recovery(&app, wid);
        assert!(app.structural_invariants_ok());
    }

    #[test]
    fn background_whole_tab_and_settings_control_close_focus_blocker_and_recovery() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (instance, settings) = app.active_native_view(wid).unwrap();
        enter_settings_draft(&mut app, wid, settings);
        app.switch_tab_in(wid, 0);
        assert!(app.active_native_view(wid).is_none());

        assert_eq!(app.apply_tab_cmd(TabAction::Close(Some(1))), (1, 2));
        assert_eq!(app.active_native_view(wid), Some((instance, settings)));
        assert_exact_settings_close_recovery(&app, wid);
        assert_eq!(app.windows[&wid].tab_set.len(), 2);
        assert!(app.native_runtime.view_state(settings).is_some());
        app.palette_activate();
        assert!(app.windows[&wid].palette().is_none());
        assert!(matches!(
            app.native_runtime.view_state(settings),
            Some(crate::native_app::AppViewState::Settings(state))
                if state.route == crate::native_settings::SettingsRoute::Modified
        ));

        app.switch_tab_in(wid, 0);
        assert!(!app.close_settings_tabs());
        assert_eq!(app.active_native_view(wid), Some((instance, settings)));
        assert_exact_settings_close_recovery(&app, wid);
        assert_eq!(app.windows[&wid].tab_set.len(), 2);
        assert!(app.structural_invariants_ok());
    }

    /// The barrier answers the SAME question two ways: a person gets the recovery
    /// surface, background policy gets only the verdict.
    ///
    /// This is the seam the automatic update lane rides (`app_native.rs`
    /// re-probes it on a `PREFLIGHT_BLOCK_COOLDOWN` schedule), so the Quiet arm
    /// must be provably identical in verdict and provably invisible on screen.
    /// Asserting only "Quiet opens no palette" would pass for a barrier that had
    /// stopped checking anything at all, so the `Ok(false)` and the reducer's
    /// retained state are asserted alongside it.
    #[test]
    fn a_quiet_shutdown_probe_reports_the_same_block_without_taking_the_screen() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));
        let (instance, settings) = app.active_native_view(wid).unwrap();
        enter_settings_draft(&mut app, wid, settings);
        // The user walks away to the terminal tab. Everything below asserts they
        // are still there afterwards.
        app.switch_tab_in(wid, 0);
        let working_tab = app.windows[&wid].tab_set.active_id().unwrap();
        assert!(app.active_native_view(wid).is_none());

        for round in 0..3 {
            assert!(
                !app.prepare_all_native_shutdown(
                    crate::native_app::CloseScope::Relaunch,
                    ClosePreflightVisibility::Quiet
                )
                .unwrap(),
                "round {round}: a quiet probe still refuses the shutdown"
            );
            assert!(
                app.windows[&wid].overlay.is_none(),
                "round {round}: a quiet probe opened an overlay over the user's work"
            );
            assert_eq!(
                app.windows[&wid].tab_set.active_id(),
                Some(working_tab),
                "round {round}: a quiet probe switched the active tab"
            );
            assert!(
                app.active_native_view(wid).is_none(),
                "round {round}: a quiet probe moved focus onto the blocking view"
            );
            // The reducer really ran and really still holds the draft — a barrier
            // that had been short-circuited would also be silent.
            assert!(
                !app.native_runtime
                    .presentation(instance, settings)
                    .unwrap()
                    .closable,
                "round {round}: the draft is still what refused"
            );
        }

        // Same state, Interactive: now the screen moves.
        assert!(
            !app.prepare_all_native_shutdown(
                crate::native_app::CloseScope::Relaunch,
                ClosePreflightVisibility::Interactive
            )
            .unwrap()
        );
        assert_eq!(app.active_native_view(wid), Some((instance, settings)));
        assert_exact_settings_close_recovery(&app, wid);
        assert!(app.structural_invariants_ok());
    }

    #[test]
    fn window_native_shutdown_barrier_retains_all_topology_until_explicit_discard() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));
        let (instance, settings) = app.active_native_view(wid).unwrap();
        enter_settings_draft(&mut app, wid, settings);
        let tabs = app.windows[&wid].tab_set.len();

        assert!(
            !app.prepare_window_native_shutdown(wid, crate::native_app::CloseScope::Window)
                .unwrap()
        );
        assert_eq!(app.windows[&wid].tab_set.len(), tabs);
        assert!(app.native_runtime.view_state(settings).is_some());
        assert_exact_settings_close_recovery(&app, wid);
        assert!(!app.prepare_quit_native_shutdown().unwrap());
        assert_eq!(app.windows[&wid].tab_set.len(), tabs);
        assert_exact_settings_close_recovery(&app, wid);
        assert!(
            !app.prepare_all_native_shutdown(
                crate::native_app::CloseScope::Relaunch,
                ClosePreflightVisibility::Interactive
            )
            .unwrap()
        );
        assert_eq!(app.windows[&wid].tab_set.len(), tabs);
        assert_exact_settings_close_recovery(&app, wid);

        for _ in 0..2 {
            app.dispatch_native_view_event(
                wid,
                settings,
                crate::native_app::AppEvent::Action(crate::native_app::ActionInvocation {
                    id: crate::native_ui::ActionId::new("settings/drafts/discard-all"),
                    value: None,
                }),
            )
            .unwrap();
        }
        assert!(
            app.prepare_window_native_shutdown(wid, crate::native_app::CloseScope::Window)
                .unwrap()
        );
        assert!(
            app.native_runtime
                .presentation(instance, settings)
                .unwrap()
                .closable
        );
        assert_eq!(app.windows[&wid].tab_set.len(), tabs);
        assert!(app.structural_invariants_ok());
    }

    #[test]
    fn focused_terminal_eof_collapses_mixed_tree_without_orphans() {
        use aterm_spec::derive::pane_tree_model;
        use aterm_spec::interp::{State, admits};

        fn project(tab: &crate::tab_model::Tab) -> State {
            let leaves = tab.root.leaves();
            let focused = leaves
                .iter()
                .position(|view| *view == tab.focus)
                .expect("focused identity is a live leaf");
            State::from([
                ("leaf_count", leaves.len() as i64),
                ("focused", focused as i64),
            ])
        }

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (instance, native_view) = app.active_native_view(wid).unwrap();
        let (session, terminal_view) =
            app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
        let tab_id = app.windows[&wid].tab_set.active_id().unwrap();
        let before = project(app.windows[&wid].tab_set.get(tab_id).unwrap());
        assert_eq!(app.focused_session_id(wid), Some(session));
        assert!(app.prepare_heterogeneous_input_scratch(wid).is_some());
        assert!(
            app.windows[&wid]
                .leaf_render_cache
                .contains_key(&terminal_view)
        );

        assert!(
            app.exit_session_logical(session).is_empty(),
            "the native sibling keeps the window alive"
        );

        assert_terminal_fully_retired(&app, session, terminal_view);
        assert_native_sibling_survives(&app, instance, native_view);
        let survivor = app.windows[&wid].tab_set.get(tab_id).unwrap();
        assert_eq!(survivor.root.leaves(), vec![native_view]);
        assert_eq!(survivor.focus, native_view);
        assert_eq!(app.active_native_view(wid), Some((instance, native_view)));

        // Tier-1 bind: the shipping EOF path is an admitted Close transition of
        // the existing derived pane-tree machine.  The dangling-focus negative
        // control proves this is not a vacuous leaf-count assertion.
        let model = pane_tree_model();
        let after = project(survivor);
        assert!(model.successors("Close", &before).contains(&after));
        let mut dangling = after.clone();
        dangling.insert("focused", dangling["leaf_count"]);
        assert_eq!(admits(&model, &before, &dangling), None);
        assert!(!model.check_invariant("FocusInRange", &dangling));
        assert!(app.structural_invariants_ok());
    }

    #[test]
    fn background_mixed_terminal_eof_repairs_exact_tab_and_keeps_front_tab() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (instance, native_view) = app.active_native_view(wid).unwrap();
        let (session, terminal_view) =
            app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Vertical);
        let background_tab = app.windows[&wid].tab_set.active_id().unwrap();

        // Stage a genuinely separate foreground tab. Reopening Settings now
        // correctly finds and focuses the nonfocused Settings leaf above, so
        // it can no longer be abused as a way to manufacture this tab.
        let foreground_session = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(foreground_session));
        let foreground_tab = app.windows[&wid].tab_set.active_id().unwrap();
        assert_ne!(foreground_tab, background_tab);
        assert!(app.exit_session_logical(session).is_empty());

        assert_eq!(app.windows[&wid].tab_set.active_id(), Some(foreground_tab));
        let repaired = app.windows[&wid].tab_set.get(background_tab).unwrap();
        assert_eq!(repaired.root.leaves(), vec![native_view]);
        assert_eq!(repaired.focus, native_view);
        assert_terminal_fully_retired(&app, session, terminal_view);
        assert_native_sibling_survives(&app, instance, native_view);
        assert!(app.structural_invariants_ok());
    }

    #[test]
    fn addressed_close_removes_only_terminal_leaf_from_mixed_tab() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));
        let (instance, native_view) = app.active_native_view(wid).unwrap();
        let (session, terminal_view) =
            app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
        let tab_id = app.windows[&wid].tab_set.active_id().unwrap();

        app.close_session_by_id(session).unwrap();

        let survivor = app.windows[&wid].tab_set.get(tab_id).unwrap();
        assert_eq!(survivor.root.leaves(), vec![native_view]);
        assert_eq!(survivor.focus, native_view);
        assert!(!app.windows[&wid].pending_close);
        assert_terminal_fully_retired(&app, session, terminal_view);
        assert_native_sibling_survives(&app, instance, native_view);
        assert!(app.close_session_by_id(session).is_err());
        assert!(app.structural_invariants_ok());
    }

    #[test]
    fn shared_session_migrated_beside_source_exit_retires_every_occurrence() {
        let mut app = App::headless_for_test();
        let source = WindowId(0);
        let original_view = app.windows[&source].tab_set.active().unwrap().focus;

        // Keep a native sibling in the source, share T0 into B (Cmd-Shift-O), then
        // move B's shared tab back into A (Cmd-Shift-M).  A now owns two distinct
        // stable terminal views of the same pooled session with Settings between
        // them; the old WindowId-deduped exit path retired only the first one.
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (settings_instance, settings_view) = app.active_native_view(source).unwrap();
        app.switch_tab_in(source, 0);
        let shared_window = app
            .open_active_session_in_new_window_logical()
            .expect("share the active session into a second window");
        let shared_view = app.windows[&shared_window].tab_set.active().unwrap().focus;
        assert_ne!(shared_view, original_view);
        assert_eq!(app.pool.views(0), Some(2));

        app.migrate_active_tab_to_next_window();
        assert_eq!(app.windows.len(), 1, "the emptied share window closes");
        assert!(app.windows.contains_key(&source));
        assert_eq!(app.frontmost_window, Some(source));
        assert_eq!(
            app.windows[&source]
                .tab_set
                .tabs()
                .iter()
                .flat_map(|tab| tab.root.leaves())
                .filter(|view| {
                    app.view_store
                        .get(*view)
                        .copied()
                        .and_then(crate::tab_model::View::terminal_session)
                        == Some(0)
                })
                .count(),
            2,
            "both shared-session occurrences now live in one window"
        );

        // Materialize retained-cache edges for both terminal views and the native
        // sibling. Exact retirement must evict only the two dead terminal keys.
        {
            let cache = &mut app.windows.get_mut(&source).unwrap().leaf_render_cache;
            cache.insert(original_view, crate::LeafRenderCache::default());
            cache.insert(shared_view, crate::LeafRenderCache::default());
            cache.insert(settings_view, crate::LeafRenderCache::default());
        }

        assert!(
            app.exit_session_logical(0).is_empty(),
            "Settings keeps the sole window alive"
        );

        assert_terminal_fully_retired(&app, 0, original_view);
        assert_terminal_fully_retired(&app, 0, shared_view);
        assert_native_sibling_survives(&app, settings_instance, settings_view);
        assert_eq!(app.windows[&source].tab_set.len(), 1);
        assert_eq!(
            app.active_native_view(source),
            Some((settings_instance, settings_view)),
            "focus repairs onto the surviving native tab"
        );
        assert!(
            app.windows[&source]
                .leaf_render_cache
                .contains_key(&settings_view),
            "retirement does not evict a live sibling's retained raster"
        );
        assert_eq!(app.windows_displaying(0).count(), 0);
        assert!(app.structural_invariants_ok());
    }

    #[test]
    fn heterogeneous_split_runtime_paints_sizes_routes_drags_and_closes_every_leaf() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        assert!(app.front_terminal(wid).is_none());
        assert_eq!(app.focused_session_id(wid), None);
        let (instance, first_native) = app.active_native_view(wid).expect("Settings focused");
        let second_state = crate::native_app::AppViewState::Settings(Box::new(
            crate::native_settings::SettingsViewState::new(&app.config),
        ));
        let second_native = app
            .split_active_with_native(
                wid,
                crate::tab_model::SplitAxis::Horizontal,
                instance,
                second_state,
            )
            .expect("second native view");
        let (terminal_session, terminal_view) =
            app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Vertical);
        assert_eq!(app.focused_session_id(wid), Some(terminal_session));
        assert_eq!(
            app.front_terminal(wid).map(|front| front.session),
            Some(terminal_session)
        );

        let plan = app.active_visible_leaf_plan(wid).expect("visible plan");
        assert_eq!(
            plan.leaves
                .iter()
                .map(|leaf| leaf.view)
                .collect::<std::collections::BTreeSet<_>>(),
            [first_native, second_native, terminal_view]
                .into_iter()
                .collect()
        );
        let terminal_leaf = plan.leaf(terminal_view).expect("terminal placement");
        let terminal = app.pool.get(terminal_session).expect("terminal pool owner");
        let terminal = crate::term_lock(&terminal.term);
        assert_eq!(
            (terminal.rows(), terminal.cols()),
            (
                (terminal_leaf.rect.size.height.round() as u16).max(1),
                (terminal_leaf.rect.size.width.round() as u16).max(1),
            ),
            "terminal SIGWINCH geometry comes from the shared visible plan"
        );
        drop(terminal);

        assert!(app.prepare_heterogeneous_input_scratch(wid).is_some());
        let window = &app.windows[&wid];
        assert!(
            window.settings_card.is_some(),
            "native siblings share a tray"
        );
        assert_eq!(window.leaf_render_cache.len(), 3);
        assert!(
            window.leaf_render_cache[&first_native].native.is_some()
                && window.leaf_render_cache[&second_native].native.is_some()
        );
        assert_eq!(window.input_scratch.rows, usize::from(window.rows));
        assert_eq!(window.input_scratch.cols, usize::from(window.cols));

        let first_raster = window.leaf_render_cache[&first_native]
            .native
            .as_ref()
            .expect("first retained raster");
        let first_pixels = first_raster.rgba.as_ptr() as usize;
        let first_before = first_raster.rgba.clone();
        let first_full_rasters = first_raster.full_rasters;
        let first_regional_rasters = first_raster.regional_rasters;
        let second_raster = window.leaf_render_cache[&second_native]
            .native
            .as_ref()
            .expect("second retained raster");
        let second_stamp = second_raster.stamp;
        let second_pixels = second_raster.rgba.as_ptr() as usize;
        app.invalidate_native_view_cache(
            wid,
            first_native,
            crate::native_app::DamageRegion::Rect {
                x: 7,
                y: 11,
                width: 19,
                height: 23,
            },
        );
        assert_eq!(
            app.windows[&wid].leaf_render_cache[&first_native].native_damage,
            Some(crate::native_app::DamageRegion::Rect {
                x: 7,
                y: 11,
                width: 19,
                height: 23,
            })
        );
        assert_eq!(
            app.windows[&wid].leaf_render_cache[&second_native].native_damage, None,
            "view-local damage never dirties a native sibling"
        );
        assert!(app.prepare_heterogeneous_input_scratch(wid).is_some());
        let second_raster = app.windows[&wid].leaf_render_cache[&second_native]
            .native
            .as_ref()
            .expect("second raster remains retained");
        assert_eq!(second_raster.stamp, second_stamp);
        assert_eq!(
            second_raster.rgba.as_ptr() as usize,
            second_pixels,
            "undamaged native sibling keeps its retained pixel allocation"
        );
        assert_eq!(
            app.windows[&wid].leaf_render_cache[&first_native].native_damage, None,
            "render consumes the exact leaf's pending damage"
        );
        let first_raster = app.windows[&wid].leaf_render_cache[&first_native]
            .native
            .as_ref()
            .expect("first raster remains retained");
        assert_eq!(
            first_raster.rgba.as_ptr() as usize,
            first_pixels,
            "regional repaint patches the existing leaf allocation in place"
        );
        let crate::NativeRasterWork::Region { rect, pixels } = first_raster.last_work else {
            panic!("regional invalidation performed a full leaf raster")
        };
        assert_eq!(pixels, rect.pixels());
        assert!(pixels < u64::from(first_raster.width) * u64::from(first_raster.height));
        assert_eq!(first_raster.full_rasters, first_full_rasters);
        assert_eq!(first_raster.regional_rasters, first_regional_rasters + 1);
        for y in 0..first_raster.height {
            for x in 0..first_raster.width {
                if x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height
                {
                    continue;
                }
                let index = ((y * first_raster.width + x) * 4) as usize;
                assert_eq!(
                    &first_raster.rgba[index..index + 4],
                    &first_before[index..index + 4],
                    "regional repaint changed a byte outside its device tile"
                );
            }
        }
        let full_rasters_before_paint_change = first_raster.full_rasters;
        let regional_rasters_before_paint_change = first_raster.regional_rasters;
        app.theme.fg ^= 0x0001_0101;
        app.invalidate_native_view_cache(
            wid,
            first_native,
            crate::native_app::DamageRegion::Rect {
                x: 7,
                y: 11,
                width: 19,
                height: 23,
            },
        );
        assert!(app.prepare_heterogeneous_input_scratch(wid).is_some());
        let first_raster = app.windows[&wid].leaf_render_cache[&first_native]
            .native
            .as_ref()
            .expect("paint-changed raster");
        let crate::NativeRasterWork::Full { pixels } = first_raster.last_work else {
            panic!("paint identity change did not widen to a full raster")
        };
        assert_eq!(
            pixels,
            u64::from(first_raster.width) * u64::from(first_raster.height)
        );
        assert_eq!(
            first_raster.full_rasters,
            full_rasters_before_paint_change + 1,
            "theme/font paint identity widens a local request to a full raster"
        );
        assert_eq!(
            first_raster.regional_rasters,
            regional_rasters_before_paint_change
        );

        assert_eq!(
            app.native_runtime.view_lifecycle(first_native),
            Some(crate::front_content::ViewLifecycle::Mounted)
        );
        app.windows
            .get_mut(&wid)
            .unwrap()
            .tab_set
            .active_mut()
            .unwrap()
            .set_focus(first_native);
        app.sync_window(wid);
        assert!(app.front_terminal(wid).is_none());
        assert_eq!(app.focused_session_id(wid), None);
        app.toggle_pane_zoom();
        assert_eq!(
            app.native_runtime.view_lifecycle(first_native),
            Some(crate::front_content::ViewLifecycle::Mounted)
        );
        assert_eq!(
            app.native_runtime.view_lifecycle(second_native),
            Some(crate::front_content::ViewLifecycle::Suspended),
            "zoom suspends hidden native siblings without destroying view state"
        );
        app.toggle_pane_zoom();
        assert_eq!(
            app.native_runtime.view_lifecycle(second_native),
            Some(crate::front_content::ViewLifecycle::Mounted)
        );
        app.windows
            .get_mut(&wid)
            .unwrap()
            .tab_set
            .active_mut()
            .unwrap()
            .set_focus(terminal_view);
        app.sync_window(wid);

        let first = plan.leaf(first_native).expect("first native");
        {
            let window = app.windows.get_mut(&wid).unwrap();
            window.last_mouse_window_cell = (
                (first.rect.origin.y + first.rect.size.height * 0.5) as u16,
                (first.rect.origin.x + first.rect.size.width * 0.5) as u16,
            );
        }
        assert!(app.focus_pane_under_pointer(wid));
        assert_eq!(app.active_native_view(wid), Some((instance, first_native)));
        assert!(app.front_terminal(wid).is_none());

        let before = app.active_visible_leaf_plan(wid).unwrap();
        let divider = before.dividers.first().expect("root divider").clone();
        {
            let window = app.windows.get_mut(&wid).unwrap();
            window.last_mouse_window_cell = (
                (divider.rect.origin.y + divider.rect.size.height * 0.5) as u16,
                (divider.rect.origin.x + divider.rect.size.width * 0.5) as u16,
            );
        }
        assert!(app.begin_divider_drag(wid));
        {
            let window = app.windows.get_mut(&wid).unwrap();
            window.last_mouse_window_cell.1 = window.cols / 3;
        }
        app.drag_divider(wid);
        assert!(app.finish_divider_drag(wid));
        let after = app.active_visible_leaf_plan(wid).unwrap();
        assert_ne!(
            before.leaf(first_native).unwrap().rect,
            after.leaf(first_native).unwrap().rect,
            "canonical divider drag relayouts native and terminal siblings together"
        );

        app.windows
            .get_mut(&wid)
            .unwrap()
            .tab_set
            .active_mut()
            .unwrap()
            .set_focus(terminal_view);
        app.sync_window(wid);
        assert_eq!(app.focused_session_id(wid), Some(terminal_session));
        assert!(app.active_native_view(wid).is_none());
        app.close_active_tab();
        assert!(app.pool.get(terminal_session).is_none());
        let survivors = app
            .windows
            .get(&wid)
            .unwrap()
            .tab_set
            .active()
            .unwrap()
            .root
            .leaves();
        assert_eq!(survivors.len(), 2);
        assert!(survivors.contains(&first_native) && survivors.contains(&second_native));
        assert!(app.can_reopen_closed_view());
        app.reopen_last_closed_view().unwrap();
        let reopened = app.windows[&wid].tab_set.active().unwrap();
        assert_eq!(reopened.root.len(), 3);
        assert!(matches!(
            app.view_store.get(reopened.focus),
            Some(crate::tab_model::View::Terminal(_))
        ));
        assert!(!app.can_reopen_closed_view());
        assert!(app.structural_invariants_ok());
    }

    #[test]
    fn closing_native_from_mixed_promotes_terminal_projection_and_reopen_demotes_it() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (_, settings) = app.active_native_view(wid).unwrap();
        let (_, terminal) =
            app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
        app.windows
            .get_mut(&wid)
            .unwrap()
            .tab_set
            .active_mut()
            .unwrap()
            .set_focus(settings);
        app.sync_window(wid);
        assert_eq!(
            app.windows[&wid].layouts.len(),
            1,
            "only bootstrap projected"
        );

        app.close_active_tab();
        assert_eq!(app.windows[&wid].layouts.len(), 2);
        assert!(matches!(
            app.view_store
                .get(app.windows[&wid].tab_set.active().unwrap().focus),
            Some(crate::tab_model::View::Terminal(_))
        ));
        assert!(
            app.windows[&wid]
                .tab_set
                .active()
                .unwrap()
                .root
                .contains(terminal)
        );
        assert!(app.structural_invariants_ok());

        app.reopen_last_closed_view().unwrap();
        assert_eq!(app.windows[&wid].layouts.len(), 1);
        let tab = app.windows[&wid].tab_set.active().unwrap();
        assert_eq!(tab.root.len(), 2);
        assert!(matches!(
            app.view_store.get(tab.focus),
            Some(crate::tab_model::View::Native(_))
        ));
        assert!(app.structural_invariants_ok());
    }

    #[test]
    fn whole_mixed_tab_close_and_reopen_preserves_one_tab_record_and_topology() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));
        let (terminal, _) =
            app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Vertical);
        let before = app
            .tab_restore_descriptor(app.windows[&wid].tab_set.active().unwrap())
            .unwrap();
        assert!(!app.close_tab_at(wid, 1));
        assert_eq!(app.closed_recovery.tabs.len(), 1);
        assert!(app.closed_recovery.views.is_empty());
        assert!(app.pool.get(terminal).is_none());
        assert_eq!(app.windows[&wid].tab_set.len(), 1);

        app.reopen_last_closed_tab().unwrap();
        let restored = app
            .tab_restore_descriptor(app.windows[&wid].tab_set.active().unwrap())
            .unwrap();
        assert_eq!(restored.focused_path, before.focused_path);
        assert_eq!(restored.zoomed, before.zoomed);
        assert!(matches!(
            restored.root,
            crate::restore::RestoredSplitTree::Split {
                axis: crate::restore::SplitKind::Vertical,
                ..
            }
        ));
        assert_eq!(app.windows[&wid].tab_set.active_index(), Some(1));
        assert!(app.closed_recovery.tabs.is_empty());
        assert!(app.structural_invariants_ok());
    }

    #[test]
    fn canonical_mixed_switch_cycle_close_and_status_ignore_terminal_projection_indices() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));

        // Canonical truth is [terminal, Settings], active Settings. The legacy
        // projection deliberately still contains only terminal index zero.
        assert_eq!(
            app.apply_tab_cmd(TabAction::Select(1)),
            (1, 2),
            "control status reports canonical mixed-tab active/count"
        );
        assert_eq!(app.windows[&wid].tabs, TabIndex::new(0, 1));
        assert!(app.active_handle.lock().unwrap().is_none());
        assert!(!app.is_visible_session(0));
        assert_eq!(app.windows_displaying(0).count(), 0);
        assert!(
            !app.notify_suppress.lock().unwrap().contains(&0),
            "the parked terminal behind Settings is not notification-visible"
        );

        app.switch_tab_in(wid, 0);
        assert_eq!(app.windows[&wid].tab_set.active_index(), Some(0));
        assert!(
            app.active_handle.lock().unwrap().is_some(),
            "selecting the canonical terminal republishes its PTY target"
        );
        assert!(app.is_visible_session(0));
        assert_eq!(app.windows_displaying(0).count(), 1);

        app.cycle_tab(true);
        assert_eq!(app.windows[&wid].tab_set.active_index(), Some(1));
        assert!(app.active_handle.lock().unwrap().is_none());
        app.cycle_tab(false);
        assert_eq!(app.windows[&wid].tab_set.active_index(), Some(0));

        // Closing canonical index zero removes the terminal but not its native
        // sibling. The compatibility projection becomes genuinely empty.
        assert_eq!(
            app.apply_tab_cmd(TabAction::Close(Some(0))),
            (0, 1),
            "Settings survives as the sole canonical tab"
        );
        let ws = &app.windows[&wid];
        assert_eq!(ws.tab_set.len(), 1);
        assert!(matches!(
            app.view_store.get(ws.tab_set.active().unwrap().focus),
            Some(crate::tab_model::View::Native(_))
        ));
        assert!(ws.layouts.is_empty());
        assert_eq!(ws.tabs, TabIndex::new(0, 0));
        assert!(!ws.pending_close);
        assert!(
            app.pool.get(0).is_none(),
            "the closed terminal was torn down"
        );
        assert!(app.active_handle.lock().unwrap().is_none());
        assert!(app.structural_invariants_ok());
    }

    #[test]
    fn native_reorder_detach_and_migrate_move_no_terminal_projection_or_pool_view() {
        let mut app = App::headless_for_test();
        let source = WindowId(0);
        let sid1 = app.next_session_id;
        app.push_stub_tab(source, crate::stub_session(sid1));
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));

        let (terminal_ids, native_id) = {
            let ws = &app.windows[&source];
            (
                terminal_tab_ids(&ws.tab_set, &app.view_store),
                ws.tab_set.active_id().unwrap(),
            )
        };
        let terminal_layouts = vec![0, sid1];
        assert_eq!(app.pool.views(0), Some(1));
        assert_eq!(app.pool.views(sid1), Some(1));

        // Move Settings across both terminals in canonical chrome. Neither
        // terminal-only vector is permuted because terminal relative order did
        // not change.
        app.move_tab(source, 2, 0);
        {
            let ws = &app.windows[&source];
            assert_eq!(ws.tab_set.tabs()[0].id, native_id);
            assert_eq!(terminal_tab_ids(&ws.tab_set, &app.view_store), terminal_ids);
            assert_eq!(
                ws.layouts
                    .iter()
                    .map(pane::PaneTree::focus)
                    .collect::<Vec<_>>(),
                terminal_layouts
            );
            assert_eq!(ws.tabs.active, 1, "parked terminal identity is unchanged");
        }

        // Detach the canonical active native tab into a real native-only logical
        // window. Source terminal projections and pool counts stay byte-identical.
        let native_window = app
            .detach_active_tab_logical()
            .expect("mixed source can detach its active native tab");
        {
            let ws = &app.windows[&source];
            assert_eq!(terminal_tab_ids(&ws.tab_set, &app.view_store), terminal_ids);
            assert_eq!(
                ws.layouts
                    .iter()
                    .map(pane::PaneTree::focus)
                    .collect::<Vec<_>>(),
                terminal_layouts
            );
        }
        {
            let ws = &app.windows[&native_window];
            assert_eq!(ws.tab_set.len(), 1);
            assert_eq!(ws.tab_set.active_id(), Some(native_id));
            assert!(ws.layouts.is_empty());
            assert_eq!(ws.tabs, TabIndex::new(0, 0));
            assert!(ws.active_terminal.is_none());
            assert!(
                ws.front_terminal().is_none(),
                "no terminal capability is fabricated"
            );
        }
        assert_eq!(app.pool.views(0), Some(1));
        assert_eq!(app.pool.views(sid1), Some(1));
        assert!(app.structural_invariants_ok());

        // The same stable native tab can migrate into an existing terminal
        // window; closing its now-empty source still moves no terminal session.
        app.migrate_active_tab_to_next_window();
        assert_eq!(app.windows.len(), 1);
        let ws = &app.windows[&source];
        assert_eq!(ws.tab_set.active_id(), Some(native_id));
        assert_eq!(terminal_tab_ids(&ws.tab_set, &app.view_store), terminal_ids);
        assert_eq!(
            ws.layouts
                .iter()
                .map(pane::PaneTree::focus)
                .collect::<Vec<_>>(),
            terminal_layouts
        );
        assert_eq!(app.pool.views(0), Some(1));
        assert_eq!(app.pool.views(sid1), Some(1));
        assert!(app.structural_invariants_ok());
    }

    #[test]
    fn terminal_reorder_across_native_tab_reorders_only_its_projection_entry() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let sid1 = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(sid1));
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));

        // [T0, T1, N] -> [N, T0, T1] leaves terminal order alone, then moving
        // canonical T0 past T1 must reorder exactly its matching PaneTree.
        app.move_tab(wid, 2, 0);
        let native = app.windows[&wid].tab_set.active_id().unwrap();
        app.move_tab(wid, 1, 2);
        let ws = &app.windows[&wid];
        assert_eq!(ws.tab_set.active_id(), Some(native));
        assert!(matches!(
            app.view_store.get(ws.tab_set.tabs()[0].focus),
            Some(crate::tab_model::View::Native(_))
        ));
        assert_eq!(
            ws.layouts
                .iter()
                .map(pane::PaneTree::focus)
                .collect::<Vec<_>>(),
            vec![sid1, 0]
        );
        assert_eq!(ws.tabs.active, 0, "parked T1 follows its stable identity");
        assert!(app.structural_invariants_ok());

        app.switch_tab_in(wid, 2);
        assert_eq!(
            app.front_terminal(wid)
                .expect("terminal restored to front")
                .session,
            0
        );
        assert_eq!(app.windows[&wid].tabs.active, 1);
        assert_eq!(app.apply_tab_cmd(TabAction::Select(0)), (0, 3));
        assert!(app.active_handle.lock().unwrap().is_none());
    }

    #[test]
    fn central_view_removal_detaches_an_attached_nonfinal_document_view() {
        let dir =
            std::env::temp_dir().join(format!("aterm-remove-view-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("document.md");
        std::fs::write(&path, "shared\n").unwrap();
        let uri = format!("file://{}", path.display());

        let mut app = App::headless_for_test();
        app.open_document_tab(crate::native_app::AppKind::Markdown, &uri)
            .unwrap();
        app.open_document_tab(crate::native_app::AppKind::Editor, &uri)
            .unwrap();
        let wid = WindowId(0);
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        assert_eq!(app.document_store.view_count(document), Some(2));

        let tab_id = app.windows[&wid].tab_set.active_id().unwrap();
        let removed = app.windows.get_mut(&wid).unwrap().tab_set.remove(tab_id);
        assert!(removed.is_some());
        assert!(app.remove_view_link(view).is_some());
        assert_eq!(app.document_store.view_count(document), Some(1));
        assert!(app.view_store.get(view).is_none());
        assert!(app.native_runtime.view_state(view).is_none());

        app.resync_active_or_window(wid);
        assert!(app.structural_invariants_ok());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn reopen_settings_mints_fresh_tab_view_and_instance_with_route() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));
        let old_tab = app.windows[&wid].tab_set.active_id().unwrap();
        let (old_instance, old_view) = app.active_native_view(wid).unwrap();
        assert_eq!(app.pool.views(0), Some(1));

        app.close_active_native_tab(wid).unwrap();
        assert!(app.view_store.get(old_view).is_none());
        assert!(app.native_runtime.app(old_instance).is_none());
        assert_eq!(app.closed_recovery.tabs.len(), 1);
        app.reopen_last_closed_tab().unwrap();

        let new_tab = app.windows[&wid].tab_set.active_id().unwrap();
        let (new_instance, new_view) = app.active_native_view(wid).unwrap();
        assert_ne!(new_tab, old_tab, "closed TabId is never reused");
        assert_ne!(new_view, old_view, "closed ViewId is never reused");
        assert_ne!(
            new_instance, old_instance,
            "final-view close retires and remints its app instance"
        );
        assert!(matches!(
            app.native_runtime.view_state(new_view),
            Some(crate::native_app::AppViewState::Settings(state))
                if state.route == crate::native_settings::SettingsRoute::About
        ));
        assert!(
            app.dispatch_native_view_event(
                wid,
                old_view,
                crate::native_app::AppEvent::FocusChanged(None),
            )
            .is_err(),
            "a delayed event for the retired view may not target its replacement"
        );
        assert_eq!(
            app.pool.views(0),
            Some(1),
            "native reopen has zero PTY churn"
        );
        assert!(app.closed_recovery.tabs.is_empty());
        assert!(app.structural_invariants_ok());
    }

    #[test]
    fn reopened_editor_shares_canonical_document_but_gets_fresh_runtime_ids() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-reopen-shared-document-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("shared.md");
        std::fs::write(&path, "shared\n").unwrap();
        let uri = format!("file://{}", path.to_string_lossy().replace(' ', "%20"));

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.open_document_tab(crate::native_app::AppKind::Markdown, &uri)
            .unwrap();
        app.open_document_tab(crate::native_app::AppKind::Editor, &uri)
            .unwrap();
        let (old_instance, old_view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(old_instance).unwrap();
        let canonical_uri = app
            .document_store
            .canonical_uri(document)
            .expect("canonical shared URI")
            .to_string();
        let old_tab = app.windows[&wid].tab_set.active_id().unwrap();
        assert_eq!(app.document_store.view_count(document), Some(2));

        app.close_active_native_tab(wid).unwrap();
        assert_eq!(app.document_store.view_count(document), Some(1));
        app.reopen_last_closed_tab().unwrap();
        let (new_instance, new_view) = app.active_native_view(wid).unwrap();
        let new_tab = app.windows[&wid].tab_set.active_id().unwrap();
        assert_ne!(new_instance, old_instance);
        assert_ne!(new_view, old_view);
        assert_ne!(new_tab, old_tab);
        assert_eq!(app.native_runtime.document_id(new_instance), Some(document));
        assert_eq!(
            app.document_store.id_for_uri(&canonical_uri),
            Some(document)
        );
        assert_eq!(app.document_store.view_count(document), Some(2));
        assert!(app.structural_invariants_ok());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn reopen_missing_document_installs_recovery_view_instead_of_losing_the_record() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-reopen-missing-document-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("gone.md");
        std::fs::write(&path, "gone soon\n").unwrap();
        let uri = format!("file://{}", path.to_string_lossy().replace(' ', "%20"));

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.open_document_tab(crate::native_app::AppKind::Markdown, &uri)
            .unwrap();
        let (_, old_view) = app.active_native_view(wid).unwrap();
        app.close_active_native_tab(wid).unwrap();
        std::fs::remove_file(&path).unwrap();
        let before_tabs = app.windows[&wid].tab_set.len();
        let before_views = app.view_store.len();

        app.reopen_last_closed_tab().unwrap();
        assert_eq!(app.windows[&wid].tab_set.len(), before_tabs + 1);
        assert_eq!(app.view_store.len(), before_views + 1);
        assert!(app.view_store.get(old_view).is_none());
        assert!(app.closed_recovery.tabs.is_empty());
        let (instance, _) = app.active_native_view(wid).expect("Recovery active");
        assert_eq!(
            app.native_runtime.app(instance).map(|app| app.kind()),
            Some(crate::native_app::AppKind::Recovery)
        );
        assert!(app.structural_invariants_ok());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn closed_terminal_tab_reattaches_a_still_live_shared_session() {
        let mut app = App::headless_for_test();
        let original = WindowId(0);
        let shared = app
            .open_active_session_in_new_window_logical()
            .expect("second terminal viewer");
        assert_eq!(app.pool.views(0), Some(2));
        assert!(app.close_tab_at(shared, 0), "only tab closes its window");
        assert_eq!(app.closed_recovery.tabs.len(), 1);
        assert_eq!(app.close_window_logical(shared), crate::CloseOutcome::Stay);
        assert_eq!(app.pool.views(0), Some(1));
        app.frontmost_window = Some(original);

        app.reopen_last_closed_tab().unwrap();
        assert_eq!(app.pool.views(0), Some(2));
        assert_eq!(app.windows[&original].tab_set.len(), 2);
        assert!(app.closed_recovery.tabs.is_empty());
        assert!(app.structural_invariants_ok());
    }

    #[test]
    fn reopen_without_any_target_window_retains_the_closed_tab_record() {
        let mut app = App::headless_for_test();
        let tab = crate::restore::RestoredTab {
            root: crate::restore::RestoredSplitTree::leaf(crate::restore::RestoredView::Terminal(
                crate::restore::TerminalLeafRestore {
                    cwd: Some("/tmp".to_string()),
                    title: "expired shell".to_string(),
                    profile: None,
                    local_id: None,
                    user_title: None,
                    description: None,
                    icon: None,
                    role: None,
                    attention: None,
                },
            )),
            focused_path: Vec::new(),
            zoomed: false,
        };
        app.closed_recovery.tabs.push(
            crate::closed_recovery::ClosedTab {
                original_window: WindowId(0),
                original_index: 0,
                tab,
            },
            0,
        );
        app.windows.clear();
        app.frontmost_window = None;
        assert!(app.reopen_last_closed_tab().is_err());
        assert_eq!(app.closed_recovery.tabs.len(), 1);
    }

    #[test]
    fn recently_closed_native_ledger_is_bounded_and_evicts_the_oldest_descriptor() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        for index in 0..=crate::closed_recovery::CLOSED_TAB_LIMIT {
            let route = if index == 0 {
                crate::native_settings::SettingsRoute::Home
            } else {
                crate::native_settings::SettingsRoute::About
            };
            assert!(app.open_settings_tab(route));
            app.close_active_native_tab(wid).unwrap();
        }
        assert_eq!(
            app.closed_recovery.tabs.len(),
            crate::closed_recovery::CLOSED_TAB_LIMIT
        );
        assert!(matches!(
            app.closed_recovery.tabs.oldest(0).map(|closed| &closed.tab.root),
            Some(crate::restore::RestoredSplitTree::Leaf {
                view: crate::restore::RestoredView::Native(crate::restore::NativeLeafRestore {
                    restore_tag,
                    route: Some(route),
                    ..
                })
            }) if restore_tag == "settings" && route == "/about"
        ));
        assert!(app.structural_invariants_ok());
    }

    #[test]
    fn reopen_same_window_settings_mints_an_independent_view_instead_of_aliasing() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let existing = app.active_native_view(wid).expect("Settings active");
        let tab_count = app.windows[&wid].tab_set.len();
        let descriptor = crate::restore::NativeTabRestore::Settings {
            route: "/about".to_string(),
        };
        app.closed_recovery.tabs.push(
            crate::closed_recovery::ClosedTab {
                original_window: wid,
                original_index: 0,
                tab: crate::restore::RestoredTab::from_legacy_native(&descriptor).unwrap(),
            },
            0,
        );

        app.reopen_last_closed_tab().unwrap();
        let reopened = app.active_native_view(wid).expect("reopened Settings view");
        assert_eq!(reopened.0, existing.0, "singleton controller is shared");
        assert_ne!(reopened.1, existing.1, "presentation identity is fresh");
        assert_eq!(app.windows[&wid].tab_set.len(), tab_count + 1);
        assert!(app.closed_recovery.tabs.is_empty());
        assert!(!app.can_reopen_closed_tab());
        assert!(app.structural_invariants_ok());
    }

    #[test]
    fn native_reopen_ledger_tier1_conforms_and_negative_controls_are_rejected() {
        use aterm_spec::derive::native_reopen_ledger_model;
        use aterm_spec::interp::{State, admits};

        fn assert_step(
            model: &aterm_spec::derive::Model,
            before: &State,
            after: &State,
            action: &'static str,
        ) {
            assert_eq!(
                model.successors(action, before).as_slice(),
                std::slice::from_ref(after),
                "shipping reopen transition must conform specifically to {action}"
            );
            assert_eq!(admits(model, before, after), Some(action));
        }

        let model = native_reopen_ledger_model();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));
        assert_eq!(app.windows[&wid].tab_set.active_id().unwrap().get(), 2);
        let before_close = model.init_state();
        app.close_active_native_tab(wid).unwrap();
        let mut after_close = before_close.clone();
        after_close.insert("ledger", app.closed_recovery.tabs.len() as i64);
        after_close.insert("native_live", 0);
        after_close.insert("retired_id", 2);
        assert_step(&model, &before_close, &after_close, "Close");

        app.reopen_last_closed_tab().unwrap();
        let reopened = app.windows[&wid].tab_set.active_id().unwrap().get();
        let mut after_reopen = after_close.clone();
        after_reopen.insert("ledger", app.closed_recovery.tabs.len() as i64);
        after_reopen.insert("native_live", 1);
        after_reopen.insert("opened_id", reopened as i64);
        after_reopen.insert("next_id", reopened.saturating_add(1) as i64);
        assert_step(&model, &after_close, &after_reopen, "Reopen");

        let mut reused = after_reopen.clone();
        reused.insert("opened_id", after_close["retired_id"]);
        reused.insert("next_id", after_close["next_id"]);
        reused.insert("reused_retired", 1);
        assert_eq!(admits(&model, &after_close, &reused), None);
        assert!(!model.check_invariant("FreshReopenIdentity", &reused));

        let mut failed = App::headless_for_test();
        assert!(failed.open_settings_tab(crate::native_settings::SettingsRoute::About));
        failed.close_active_native_tab(wid).unwrap();
        failed.windows.clear();
        failed.frontmost_window = None;
        assert!(failed.reopen_last_closed_tab().is_err());
        let mut after_failure = after_close.clone();
        after_failure.insert("ledger", failed.closed_recovery.tabs.len() as i64);
        after_failure.insert("failures", 1);
        assert_step(&model, &after_close, &after_failure, "FailReopen");

        let mut lossy = after_failure.clone();
        lossy.insert("ledger", 0);
        lossy.insert("lost_on_failure", 1);
        assert_eq!(admits(&model, &after_close, &lossy), None);
        assert!(!model.check_invariant("FailedReopenRetainsDescriptor", &lossy));
    }
}

/// Boundary proofs for the `~` cwd abbreviation (the tab-label form of a
/// session cwd — see [`home_relative_suffix`] / [`App::tab_titles`]). Pure
/// (explicit `home`), so every component-boundary case is provable without
/// touching the process environment; the App-level fallback-order tests in
/// `app_restore.rs` cover the same helper wired through `tab_titles` against
/// the REAL `$HOME`.
#[cfg(test)]
mod home_abbreviation_tests {
    use super::home_relative_suffix;

    #[test]
    fn home_itself_and_children_abbreviate() {
        assert_eq!(
            home_relative_suffix("/Users//foo", "/Users//foo"),
            Some(""),
            "home itself reads as the bare ~ (empty suffix)"
        );
        assert_eq!(
            home_relative_suffix("/Users//foo/src/aterm", "/Users//foo"),
            Some("/src/aterm"),
            "a child keeps its /-prefixed remainder (label = ~/src/aterm)"
        );
    }

    #[test]
    fn sibling_prefix_never_abbreviates() {
        // The zsh integration matches $HOME with a trailing slash for exactly
        // this reason: /Users//foobar must never read as ~bar.
        assert_eq!(home_relative_suffix("/Users//foobar", "/Users//foo"), None);
        assert_eq!(home_relative_suffix("/elsewhere/x", "/Users//foo"), None);
    }

    #[test]
    fn degenerate_homes_never_match() {
        // An empty $HOME (set-but-blank env) must not turn EVERY path into
        // `~<path>`; a root home ("/") strips to a suffix with no leading
        // slash, which the component-boundary rule rejects.
        assert_eq!(home_relative_suffix("/anything", ""), None);
        assert_eq!(home_relative_suffix("/anything", "/"), None);
    }
}

/// App-level proofs for the composed per-tab session chrome (tooltip +
/// context-menu model, session-metadata stage 2): the live wiring around the
/// pure `session_chrome` composer — label/meta flow into the composed output,
/// the tooltip feeds through `TabPresentation.tooltip`, the epoch cache serves
/// stale-until-event and recomposes on a timeline advance, and the per-tab
/// action payload resolvers (`Copy Session ID` / `Copy CWD`) read the registry
/// and engine truthfully. All headless — no AppKit; the strip-side `setToolTip`
/// / `NSMenu` pop are the documented manual-verify steps.
#[cfg(test)]
mod session_chrome_app_tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::menu::MenuAction;
    use crate::session_chrome::TabMenuEntry;

    /// The composed chrome reflects live session facts (registry state +
    /// `spawned` timeline event exist for every registered session), the menu
    /// carries the pinned actions with honest enabled bits (a stub has no cwd
    /// ⇒ `Copy CWD` greys), and the composed tooltip is FED THROUGH the tab's
    /// presentation — terminal tabs stop being the tooltip-less kind.
    #[test]
    fn composed_chrome_reaches_presentation_and_menu_actions_are_honest() {
        let mut app = App::headless_for_test();
        let titles = app.tab_titles(WindowId(0));
        let ext = app.tab_chrome_ext(WindowId(0), &titles);
        assert_eq!(ext.len(), 1);
        let tooltip = ext[0]
            .tooltip
            .as_deref()
            .expect("a registered session always has state + a spawned event, so chrome composes");
        assert!(
            tooltip.contains("state: ") && tooltip.contains("spawned · "),
            "tooltip carries registry state + the spawn event: {tooltip:?}"
        );
        assert!(
            tooltip.starts_with(&titles[0]),
            "tooltip opens with the exact chip label"
        );
        // Fed through the presentation (the cross-platform tooltip slot).
        assert_eq!(
            app.windows[&WindowId(0)].tab_set.tabs()[0]
                .presentation
                .tooltip,
            ext[0].tooltip
        );
        // The action rows: session id copy live, cwd copy greyed (stub: none).
        let action = |a: MenuAction| {
            ext[0].menu.iter().find_map(|e| match e {
                TabMenuEntry::Action {
                    action, enabled, ..
                } if *action == a => Some(*enabled),
                _ => None,
            })
        };
        assert_eq!(action(MenuAction::CopySessionId), Some(true));
        assert_eq!(action(MenuAction::CopyCwd), Some(false), "stub has no cwd");
        assert_eq!(action(MenuAction::CloseTab), Some(true));
    }

    /// The epoch cache honours its contract: an input mutated WITHOUT a
    /// timeline record (impossible through the real `meta set` path, which
    /// always records) serves the cached composition, and the next timeline
    /// advance recomposes with the fresh facts. Also proves `meta` identity
    /// flows: user title tops the label chain and icon prefixes the tooltip.
    #[test]
    fn chrome_cache_is_epoch_gated_and_meta_identity_flows() {
        let mut app = App::headless_for_test();
        {
            let ctx = &app.pool.get(0).expect("session 0").ctx;
            let mut meta = ctx.meta.lock().unwrap();
            assert_eq!(meta.set("title", Some("build agent".into())), Some(true));
            assert_eq!(meta.set("icon", Some("🤖".into())), Some(true));
        }
        // No timeline record yet — but the LABEL moved (user title tops the
        // chain), which is itself a cache key, so the first compose sees it.
        let titles = app.tab_titles(WindowId(0));
        assert_eq!(titles[0], "build agent");
        let ext = app.tab_chrome_ext(WindowId(0), &titles);
        let tip = ext[0].tooltip.as_deref().unwrap();
        assert!(tip.starts_with("🤖 build agent"), "{tip:?}");
        assert!(!tip.contains("purpose"), "description not set yet");
        // Mutate the description WITHOUT a timeline record: same high id, same
        // label ⇒ the cache serves the (now stale) composition — the epoch
        // gate, observed.
        {
            let ctx = &app.pool.get(0).expect("session 0").ctx;
            ctx.meta
                .lock()
                .unwrap()
                .set("description", Some("purpose text".into()));
        }
        let stale = app.tab_chrome_ext(WindowId(0), &titles);
        assert_eq!(stale[0], ext[0], "no epoch movement ⇒ cached chrome");
        // Record the meta-change (what the real `meta set` verb does) — the
        // high id advances and the next refresh recomposes.
        {
            let ctx = &app.pool.get(0).expect("session 0").ctx;
            ctx.timeline.lock().unwrap().record(
                "meta-change",
                "field=description value=purpose%20text".into(),
            );
        }
        let fresh = app.tab_chrome_ext(WindowId(0), &titles);
        let tip = fresh[0].tooltip.as_deref().unwrap();
        assert!(
            tip.contains("purpose text") && tip.contains("meta-change · just now"),
            "recomposed with the new description + event: {tip:?}"
        );
    }

    #[test]
    fn chrome_refresh_never_waits_for_terminal_parser_lock() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.config.tab_title_format = Some(crate::app_config::TitleFormat::Title);
        app.note_title_activity(0);
        app.refresh_window_tabs(wid);
        let cached = app
            .session_chrome
            .get(&0)
            .expect("initial chrome cached")
            .ext
            .clone();
        let (term, live_epoch) = {
            let session = app.pool.get(0).expect("session 0");
            let term = session.term.clone();
            let live_epoch = term.lock().unwrap().title_epoch();
            (term, live_epoch)
        };
        app.windows
            .get_mut(&wid)
            .unwrap()
            .tab_title_epochs
            .insert(0, live_epoch);
        app.title_summaries
            .set_test_activity(0, "Reviewing proxy boundary");
        let parser_guard = term.lock().unwrap();

        let started = Instant::now();
        app.refresh_window_tabs(wid);
        // A real regression takes the parser mutex and blocks FOREVER behind the guard
        // this test holds, so ANY finite bound catches it and tightness buys nothing.
        // 100ms measured the scheduler, not the lock discipline: the sibling 1s bound
        // on this same property in app_documents.rs already crossed under full-suite
        // load (cb8c0cff), and this was ten times tighter.
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "chrome refresh blocked behind the terminal parser mutex"
        );
        assert_eq!(
            app.session_chrome.get(&0).unwrap().ext,
            cached,
            "contention serves the last coherent chrome snapshot"
        );
        assert!(
            app.title_drift.pending.contains(&0),
            "contention must own a bounded event-loop retry"
        );
        assert!(app.session_chrome_retry.contains(&0));
        assert!(
            !app.windows[&wid].tab_title_epochs.contains_key(&0),
            "activity-only contention must force the retry past the title-epoch gate"
        );

        drop(parser_guard);
        assert!(app.flush_title_drift(0, Instant::now()));
        assert!(!app.title_drift.pending.contains(&0));
        assert!(!app.session_chrome_retry.contains(&0));
        assert!(
            app.session_chrome[&0]
                .ext
                .tooltip
                .as_deref()
                .is_some_and(|tooltip| tooltip.contains("Reviewing proxy boundary")),
            "retry recomposes activity-only chrome after parser contention"
        );
    }

    /// The context-menu action PAYLOAD resolvers read ground truth per clicked
    /// tab: `Copy Session ID` yields the registry sid for THAT tab's session
    /// (not the active one), `Copy CWD` yields `None` when the engine reports
    /// no cwd, and a native/out-of-range tab resolves to no session at all.
    #[test]
    fn tab_action_payloads_resolve_the_clicked_tab() {
        let mut app = App::headless_for_test();
        app.push_stub_tab(WindowId(0), crate::stub_session(app.next_session_id));
        // Two tabs: index 0 = session 0, index 1 = session 1 (now active).
        assert_eq!(app.tab_terminal_session(WindowId(0), 0), Some(0));
        assert_eq!(app.tab_terminal_session(WindowId(0), 1), Some(1));
        let sid0 = app
            .store
            .read()
            .unwrap()
            .by_local(0)
            .map(|h| h.sid.as_str().to_string())
            .expect("session 0 registered");
        assert_eq!(
            app.tab_session_id_text(WindowId(0), 0).as_deref(),
            Some(sid0.as_str()),
            "index 0 copies session 0's sid even while tab 1 is active"
        );
        assert_ne!(
            app.tab_session_id_text(WindowId(0), 1),
            Some(sid0),
            "each tab resolves its OWN registry identity"
        );
        assert_eq!(app.tab_session_cwd(WindowId(0), 0), None, "stub: no cwd");
        assert_eq!(app.tab_terminal_session(WindowId(0), 9), None, "OOB tab");
    }

    /// REGRESSION (stale-index context menu, reorder half): the menu captures
    /// the clicked tab's STABLE id at pop time, and dispatch re-resolves that
    /// id — so a `tab move` landing while the menu is open re-targets the SAME
    /// session at its new position, while the frozen positional index would
    /// have handed the action to whatever tab drifted into the clicked slot.
    #[test]
    fn tab_menu_target_follows_its_stable_id_across_a_mid_menu_reorder() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
        app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
        // Three tabs: positions 0,1,2 = sessions 0,1,2. "Right-click" tab 1:
        // pop time captures its stable id + the sid its menu displays.
        let popped = app.windows[&wid].tab_set.tabs()[1].id;
        let popped_sid = app
            .tab_session_id_text(wid, 1)
            .expect("clicked tab has a registered session");
        // While the menu tracks: a control-socket `tab move` relocates tab 0
        // to the end. Order is now sessions 1,2,0 — position 1 changed hands.
        app.move_tab(wid, 0, 2);
        // Dispatch re-resolves the POP-TIME id: same session, new position.
        let index = app
            .tab_index_for_id(wid, popped)
            .expect("the clicked tab still exists");
        assert_eq!(index, 0, "the clicked tab moved from position 1 to 0");
        assert_eq!(
            app.tab_session_id_text(wid, index),
            Some(popped_sid.clone()),
            "the action lands on the SAME session the menu was popped on"
        );
        // The old frozen index now names a DIFFERENT live session — the exact
        // wrong-tab close / wrong-sid copy the stable id exists to prevent.
        assert_ne!(
            app.tab_session_id_text(wid, 1),
            Some(popped_sid),
            "the clicked position was inherited by another session"
        );
    }

    /// REGRESSION (stale-index context menu, close half): when the CLICKED tab
    /// closes while its menu is open, the pop-time id resolves to `None` and
    /// the dispatcher drops the action — it must never fall back to the frozen
    /// index, which is still in range and now names the innocent right
    /// neighbor. When a DIFFERENT (lower) tab closes instead, the id keeps
    /// following its session through the index shift.
    #[test]
    fn tab_menu_target_of_a_closed_tab_is_dropped_not_redirected() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
        app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
        // Pop on tab 1 (session 1); its right neighbor is session 2.
        let popped = app.windows[&wid].tab_set.tabs()[1].id;
        let neighbor_sid = app.tab_session_id_text(wid, 2);
        // Mid-menu, the clicked tab's shell exits (`Wake::Exit` → whole-tab
        // close). The old index 1 is STILL in range — it now names session 2.
        // (`close_tab_at` returns `true` only for a LAST-tab window close.)
        assert!(!app.close_tab_at(wid, 1), "two tabs survive the close");
        assert_eq!(app.windows[&wid].tab_set.len(), 2, "clicked tab is gone");
        assert_eq!(
            app.tab_session_id_text(wid, 1),
            neighbor_sid,
            "the frozen index re-binds to the neighbor — the trap"
        );
        assert_eq!(
            app.tab_index_for_id(wid, popped),
            None,
            "the pop-time id refuses the re-bound slot: dispatch no-ops"
        );

        // Contrast: a LOWER tab closing must not orphan the clicked tab — the
        // stable id tracks it through the shift (close tab 0; clicked tab is
        // now at position 0).
        let mut app = App::headless_for_test();
        app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
        app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
        let popped = app.windows[&wid].tab_set.tabs()[1].id;
        let popped_sid = app.tab_session_id_text(wid, 1);
        assert!(!app.close_tab_at(wid, 0), "a lower tab exits mid-menu");
        assert_eq!(app.windows[&wid].tab_set.len(), 2, "lower tab is gone");
        let index = app
            .tab_index_for_id(wid, popped)
            .expect("the clicked tab survived");
        assert_eq!(index, 0, "shifted left by the close");
        assert_eq!(
            app.tab_session_id_text(wid, index),
            popped_sid,
            "still the session the menu was popped on"
        );
    }

    /// REGRESSION (orphan controller release): a `mouse press` arriving off the
    /// card DISMISSES it and is consumed — and because the dismiss CLOSES the
    /// card, the matching RELEASE finds no menu open and would fall through to
    /// a tracking TUI as half a press pair, the same defect the consumed-press
    /// key sets exist to prevent. The dismissing button is latched so its
    /// release is swallowed with it; a button that was never eaten is not.
    #[cfg(windows)]
    #[test]
    fn a_controller_press_that_dismisses_the_card_swallows_its_own_release() {
        use crate::input::InputEvent;
        use aterm_types::mouse::MouseButton;

        let ev = |button, pressed| InputEvent::MouseButton {
            button,
            pressed,
            row: 8,
            col: 8,
            mods: 0,
            click_count: 1,
            side: Default::default(),
            block: false,
            suppress_copy_on_select: false,
            px_off: Default::default(),
        };

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.tab_strip_rows = 1;
        app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
        assert!(app.open_active_tab_context_menu(wid), "the card is up");
        assert!(
            app.tab_menu_input_event(wid, &ev(MouseButton::Left, true)),
            "a press off the card dismisses it AND is consumed"
        );
        assert!(app.windows[&wid].tab_menu.is_none(), "the card is down");
        assert!(
            app.tab_menu_input_event(wid, &ev(MouseButton::Left, false)),
            "and its RELEASE is swallowed too — never handed on alone"
        );
        assert!(
            !app.tab_menu_input_event(wid, &ev(MouseButton::Left, false)),
            "the latch is spent: a later release is ordinary input"
        );

        // Only the latched button is eaten. A different button's release was
        // never half of a pair the modal ate, so it stays ordinary input.
        assert!(app.open_active_tab_context_menu(wid));
        assert!(app.tab_menu_input_event(wid, &ev(MouseButton::Left, true)));
        assert!(
            !app.tab_menu_input_event(wid, &ev(MouseButton::Right, false)),
            "a button the card never consumed passes through"
        );
    }
}

/// THE IN-GRID STRIP'S CLICK STREAK — the service AppKit provides on macOS
/// (`event.clickCount() == 2`) and winit does not, so off macOS the strip has to
/// keep its own. These drive `handle_tab_strip_click` against real laid-out
/// segments, so they pin the mapping from a column to a gesture, not just the FSM.
#[cfg(test)]
mod rename_strip_click_tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// A two-tab window with the in-grid strip painted, so `ws.tab_segments` is
    /// the real laid-out geometry a click hit-tests against.
    fn app_with_two_tabs() -> (App, WindowId) {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.tab_strip_rows = 1;
        app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
        app.splice_tab_strip(wid);
        (app, wid)
    }

    /// A column inside tab `index`'s chip that is not its close `✕`.
    fn chip_col(app: &App, wid: WindowId, index: usize) -> u16 {
        let seg = app.windows[&wid]
            .tab_segments
            .iter()
            .find(|seg| seg.kind == tab_bar::TabHit::Select(index))
            .expect("the tab has a segment");
        seg.start_col + 1
    }

    fn pin(app: &App, session: u64) -> Option<String> {
        app.pool
            .get(session)?
            .ctx
            .meta
            .lock()
            .unwrap()
            .user_title
            .clone()
    }

    /// DOUBLE-CLICK OPENS THE EDITOR. One click only switches; the second within
    /// the multi-click interval opens the inline rename over that chip.
    #[test]
    fn a_second_click_on_the_same_chip_opens_the_inline_editor() {
        let (mut app, wid) = app_with_two_tabs();
        let col = chip_col(&app, wid, 0);

        app.handle_tab_strip_click(wid, col);
        assert_eq!(app.windows[&wid].tabs.active, 0, "the first click switches");
        assert_eq!(app.rename_edit_session(wid), None, "and only switches");

        app.handle_tab_strip_click(wid, col);
        assert_eq!(
            app.rename_edit_session(wid),
            Some(0),
            "the second click on the SAME chip edits its focused session"
        );
    }

    /// The streak is keyed on the STABLE tab, not the column: clicking two
    /// different chips in quick succession is two single clicks, however close
    /// together their columns end up after a relayout.
    #[test]
    fn two_different_chips_are_never_a_double_click() {
        let (mut app, wid) = app_with_two_tabs();
        app.handle_tab_strip_click(wid, chip_col(&app, wid, 0));
        app.handle_tab_strip_click(wid, chip_col(&app, wid, 1));
        assert_eq!(app.rename_edit_session(wid), None);
        assert_eq!(app.windows[&wid].tabs.active, 1);
    }

    /// A slow second click is a new gesture, not a double-click.
    #[test]
    fn a_stale_press_does_not_complete_a_double_click() {
        let (mut app, wid) = app_with_two_tabs();
        let col = chip_col(&app, wid, 0);
        app.handle_tab_strip_click(wid, col);
        let stale = Instant::now() - Duration::from_millis(crate::MULTI_CLICK_MS as u64 + 50);
        let tab = app.windows[&wid].tab_set.tabs()[0].id;
        app.windows.get_mut(&wid).unwrap().strip_press = Some((stale, tab));
        app.handle_tab_strip_click(wid, col);
        assert_eq!(app.rename_edit_session(wid), None);
    }

    /// CLICKING ANOTHER CHIP IS A CLICK AWAY, and clicking away keeps what you
    /// typed — the in-grid twin of the macOS `Wake::SelectTab` fix. Clicking the
    /// chip you are ALREADY editing is not leaving the field, so it keeps editing.
    #[test]
    fn selecting_another_chip_commits_while_the_edited_chip_keeps_editing() {
        let (mut app, wid) = app_with_two_tabs();
        let tab0 = app.windows[&wid].tab_set.tabs()[0].id;
        assert!(app.begin_session_rename(wid, tab0));
        app.rename_field_edit(wid, crate::app_search::SearchEdit::Insert("kept".into()));

        // The chip under the editor: still editing, nothing written.
        app.handle_tab_strip_click(wid, chip_col(&app, wid, 0));
        assert_eq!(app.rename_edit_session(wid), Some(0), "still editing");
        assert_eq!(pin(&app, 0), None, "nothing committed yet");

        // A different chip: commit, then switch.
        app.handle_tab_strip_click(wid, chip_col(&app, wid, 1));
        assert_eq!(app.rename_edit_session(wid), None, "the editor closed");
        assert_eq!(pin(&app, 0).as_deref(), Some("kept"));
        assert_eq!(app.windows[&wid].tabs.active, 1, "and the tab switched");
    }

    /// The `+` is not a chip, so it both breaks the streak and settles the edit.
    #[test]
    fn the_new_tab_affordance_settles_the_edit_and_breaks_the_streak() {
        let (mut app, wid) = app_with_two_tabs();
        let tab0 = app.windows[&wid].tab_set.tabs()[0].id;
        let plus = app.windows[&wid]
            .tab_segments
            .iter()
            .find(|seg| seg.kind == tab_bar::TabHit::NewTab)
            .map(|seg| seg.start_col + 1)
            .expect("the strip has a + affordance");
        assert!(app.begin_session_rename(wid, tab0));
        app.rename_field_edit(wid, crate::app_search::SearchEdit::Insert("kept".into()));

        app.handle_tab_strip_click(wid, plus);
        assert_eq!(app.rename_edit_session(wid), None);
        assert_eq!(pin(&app, 0).as_deref(), Some("kept"));
        assert!(app.windows[&wid].strip_press.is_none(), "streak broken");
    }

    /// W2: the BARE band keeps its own double-click streak — the FSM behind the
    /// Windows caption reflexes (press-drag, double-click-maximize) on the
    /// strip's background. Headless has no OS window, so the drag/maximize
    /// calls themselves are natural no-ops here; the FSM is the half with rules
    /// worth pinning: a first band press ARMS, a repeat within the interval
    /// CONSUMES (so a triple-click starts over instead of re-toggling), a chip
    /// press in between DISARMS, and a stale press re-arms as a fresh first.
    #[test]
    fn bare_band_presses_keep_their_own_double_click_streak() {
        let (mut app, wid) = app_with_two_tabs();
        // Far beyond every laid-out segment: genuinely bare band.
        let band = 999u16;
        assert!(tab_bar::hit_test(&app.windows[&wid].tab_segments, band).is_none());

        app.handle_tab_strip_click(wid, band);
        assert!(
            app.windows[&wid].strip_band_press.is_some(),
            "the first band press arms"
        );
        app.handle_tab_strip_click(wid, band);
        assert!(
            app.windows[&wid].strip_band_press.is_none(),
            "the repeat is consumed — a third press is a fresh gesture"
        );

        // A chip press between two band presses breaks the pair (and the
        // symmetric case — band between chips — is what keeps the rename
        // double-click honest; `clear_strip_press` covers both).
        app.handle_tab_strip_click(wid, band);
        assert!(app.windows[&wid].strip_band_press.is_some());
        app.handle_tab_strip_click(wid, chip_col(&app, wid, 0));
        assert!(
            app.windows[&wid].strip_band_press.is_none(),
            "a chip press disarms the band streak"
        );

        // A slow second press is a new first press, not a double-click.
        app.windows.get_mut(&wid).unwrap().strip_band_press =
            Some(Instant::now() - Duration::from_millis(crate::MULTI_CLICK_MS as u64 + 50));
        app.handle_tab_strip_click(wid, band);
        assert!(
            app.windows[&wid].strip_band_press.is_some(),
            "a stale press re-arms instead of consuming"
        );
    }

    /// A TAB THAT VANISHES mid-edit cancels — never a commit, because the user was
    /// naming something that no longer exists. The in-grid twin of the macOS
    /// strip's `sync_rename_overlay`, driven from the same structural refresh.
    #[test]
    fn a_vanished_tab_reaps_the_editor_instead_of_leaving_a_modal() {
        let (mut app, wid) = app_with_two_tabs();
        let tab1 = app.windows[&wid].tab_set.tabs()[1].id;
        assert!(app.begin_session_rename(wid, tab1));
        app.rename_field_edit(wid, crate::app_search::SearchEdit::Insert("ghost".into()));

        app.close_tab_at(wid, 1);
        let _ = app.refresh_window_tabs(wid);
        assert_eq!(
            app.rename_edit_session(wid),
            None,
            "no invisible field is left owning the keyboard"
        );
    }
}

/// The operator glance end-to-end over the REAL registry + windows map —
/// headless, no AppKit: typed meta drives classification, window rows ride
/// the glance, and the structural seams (create/close/focus) move the
/// fingerprint the status item gates its rebuilds on.
#[cfg(test)]
mod operator_glance_tests {
    use super::*;

    fn set_meta(app: &App, session: u64, field: &str, value: &str) {
        let store = app.store.read().unwrap_or_else(|p| p.into_inner());
        let h = store.by_local(session).expect("registered session");
        h.ctx
            .meta
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .set(field, Some(value.to_string()));
    }

    /// Typed `role`/`attention` meta on a REGISTERED session drives the glance
    /// exactly like the pure classifier promises: role names the operator,
    /// attention escalates with the message, and the single headless window
    /// appears as a frontmost row.
    #[test]
    fn typed_meta_drives_the_headless_glance() {
        let app = App::headless_for_test();
        let g = app.operator_fleet_glance();
        assert_eq!(g.operator_session, None);
        assert_eq!(g.sessions, 1);
        assert_eq!(g.windows.len(), 1);
        assert!(g.windows[0].frontmost);

        set_meta(&app, 0, "role", "operator");
        let g = app.operator_fleet_glance();
        assert_eq!(g.operator_session, Some(0), "typed role names the operator");

        set_meta(&app, 0, "attention", "wedged on CI");
        let g = app.operator_fleet_glance();
        assert_eq!(g.warnings, vec![(0, "⚠ wedged on CI".to_string())]);
        assert_eq!(g.button_title(), "❯⚠");
    }

    /// The structural seams the status item now hooks (window create, focus
    /// re-point, window close) each move the rendered fingerprint, and a
    /// create→close round trip restores it — the change gate can never wedge
    /// showing a stale window list.
    #[test]
    fn structural_seams_move_the_fingerprint() {
        let mut app = App::headless_for_test();
        let fp_base = app.operator_fleet_glance().fingerprint();

        let wid = app.insert_logical_window(crate::stub_session(1), 24, 80);
        let fp_two = app.operator_fleet_glance().fingerprint();
        assert_ne!(fp_base, fp_two, "a new window is a rendered fact");

        app.frontmost_window = Some(WindowId(0));
        let fp_refocus = app.operator_fleet_glance().fingerprint();
        assert_ne!(fp_two, fp_refocus, "the frontmost mark is a rendered fact");

        app.frontmost_window = Some(wid);
        assert_eq!(app.close_window_logical(wid), crate::CloseOutcome::Stay);
        let fp_closed = app.operator_fleet_glance().fingerprint();
        assert_eq!(
            fp_base, fp_closed,
            "create→close round-trips the glance to its baseline"
        );
    }

    /// A busy-spinner frame on the window title (Codex dots / Claude moons)
    /// never reaches the glance: the row text and therefore the fingerprint
    /// stay stable across animation frames, so the native menu is not rebuilt
    /// at spinner rate.
    #[test]
    fn spinner_frames_do_not_churn_the_window_row_or_fingerprint() {
        let mut app = App::headless_for_test();
        app.windows.get_mut(&WindowId(0)).unwrap().current_title = "\u{280b} cargo build".into();
        let a = app.operator_fleet_glance();
        assert_eq!(a.windows[0].title, "cargo build");
        app.windows.get_mut(&WindowId(0)).unwrap().current_title = "\u{2819} cargo build".into();
        let b = app.operator_fleet_glance();
        assert_eq!(a.fingerprint(), b.fingerprint(), "phase-only change is not a rendered fact");
        app.windows.get_mut(&WindowId(0)).unwrap().current_title = "cargo build done".into();
        assert_ne!(a.fingerprint(), app.operator_fleet_glance().fingerprint());
    }

    /// The shared focus helper behind Show Operator and every clickable row:
    /// a live session hits (tab switched; headless has no OS window to raise),
    /// an unknown id misses without side effects — stale menu rows can only
    /// miss, never alias.
    #[test]
    fn focus_session_window_hits_and_misses_honestly() {
        let mut app = App::headless_for_test();
        assert!(app.focus_session_window(0));
        assert!(!app.focus_session_window(777));
    }
}

/// C5 — the in-grid tab CONTEXT MENU's App-level wiring: the popup carries the
/// SAME model `compose_tab_menu` composes (so the `chrome` verb's `tab-menu`
/// mirror and the glass can never disagree), right-clicking a background tab
/// SELECTS it first, and the entries a user could not otherwise reach — `Copy
/// Session ID` and `Copy CWD` — are present and live.
///
/// Headless: no OS window, so `request_redraw` is a no-op and no `Wake` proxy
/// is installed. The dispatch itself (`dispatch_tab_menu_action`) is proved by
/// `session_chrome_app_tests`; what these pin is that the popup resolves a
/// pointer to exactly one of the model's own [`MenuAction`]s.
#[cfg(test)]
mod tab_context_menu_tests {
    use super::*;
    use crate::menu::MenuAction;
    use crate::session_chrome::TabMenuEntry;

    fn actions(entries: &[TabMenuEntry]) -> Vec<(MenuAction, bool)> {
        entries
            .iter()
            .filter_map(|e| match e {
                TabMenuEntry::Action {
                    action, enabled, ..
                } => Some((*action, *enabled)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn opening_pops_the_composed_model_with_the_unreachable_copies_live() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.tab_strip_rows = 1;
        assert!(
            app.open_tab_context_menu(wid, 0, 5, false),
            "a terminal tab always has a menu"
        );
        let menu = app.windows[&wid].tab_menu.as_ref().expect("menu is open");
        assert_eq!(menu.anchor_col, 5, "anchored where the press landed");
        assert_eq!(menu.highlight, None, "a mouse open lights no row");
        // The popup IS the composed model — same items, same order, same
        // enabled bits as the `chrome` verb's mirror reads.
        let titles = app.tab_titles(wid);
        let composed = app.tab_chrome_ext(wid, &titles);
        assert_eq!(
            app.windows[&wid].tab_menu.as_ref().unwrap().entries,
            composed[0].menu,
            "the card renders the one composed model, never a parallel list"
        );
        let acts = actions(&composed[0].menu);
        assert!(
            acts.contains(&(MenuAction::CopySessionId, true)),
            "the GUI-unreachable session-id copy is live: {acts:?}"
        );
        assert!(
            acts.iter().any(|(a, _)| *a == MenuAction::CopyCwd),
            "…and Copy CWD is present (greyed on a stub with no cwd): {acts:?}"
        );
        assert!(acts.contains(&(MenuAction::CloseTab, true)));
        assert!(app.close_tab_menu(wid), "close reports it dismissed one");
        assert!(!app.close_tab_menu(wid), "…and is idempotent");
        assert!(app.windows[&wid].tab_menu_rect.is_none());
    }

    #[test]
    fn a_keyboard_open_lights_the_first_enabled_action() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.tab_strip_rows = 1;
        assert!(app.open_active_tab_context_menu(wid));
        let menu = app.windows[&wid].tab_menu.as_ref().expect("open");
        let lit = menu.highlight.expect("⇧F10 enters with a row selected");
        assert!(
            crate::tab_menu::is_selectable(&menu.entries[lit]),
            "and it is an ENABLED action, never a header or a greyed row"
        );
        assert!(menu.keyboard);
    }

    /// With `tab_strip_rows = 0` there is no band to hang a card under and no
    /// chip to have aimed at — the keyboard chord must fall through untouched
    /// rather than float a menu over the terminal's first row.
    #[test]
    fn no_strip_means_no_menu() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.tab_strip_rows = 0;
        assert!(!app.open_active_tab_context_menu(wid));
        assert!(!app.open_tab_context_menu(wid, 0, 0, false));
        assert!(app.windows[&wid].tab_menu.is_none());
    }

    /// Windows' rule: right-clicking a BACKGROUND tab selects it, because every
    /// action on the card reads as being about the tab you are looking at.
    #[test]
    fn right_clicking_a_background_tab_selects_it_first() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.tab_strip_rows = 1;
        app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
        assert!(
            app.windows[&wid].tab_set.len() >= 2,
            "two tabs to switch between"
        );
        let active = app.windows[&wid].tab_set.active_index().unwrap();
        let other = usize::from(active == 0);
        assert!(app.open_tab_context_menu(wid, other, 0, false));
        assert_eq!(
            app.windows[&wid].tab_set.active_index(),
            Some(other),
            "the menu's subject became the front tab"
        );
        let menu = app.windows[&wid].tab_menu.as_ref().expect("open");
        assert_eq!(
            menu.tab,
            app.windows[&wid].tab_set.tabs()[other].id,
            "…and the card captured that tab's STABLE id, not its slot"
        );
    }
}

/// Whether the operator agent CLI (`claude`) is launchable from a fresh shell:
/// a file by that name in some `PATH` entry. A handful of stat calls — cheap
/// enough to run per glance refresh, and deliberately uncached so an install
/// or uninstall mid-run is noticed on the next refresh.
fn operator_cli_on_path() -> bool {
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir| !dir.as_os_str().is_empty() && dir.join("claude").is_file())
    })
}
