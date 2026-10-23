// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tab and session-view lifecycle, tab-strip chrome, and close preflight.

use aterm_window::event_loop::ActiveEventLoop;

use crate::platform::AppRt;
use crate::{
    App, TabAction, TabIndex, WindowId, WindowState, pane, session_store, tab_bar, term_lock,
};

/// How soon after its spawn a failed interactive shell can count as one that never started.
pub(crate) const SHELL_START_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);

/// Off unix, the most the exit path's first look waits for the status of a shell that may have failed at start; on unix the reader waits for its retirement before it posts the exit (`spawn::SHELL_RETIRE_HANG`), so every look is non-blocking.
#[cfg(not(unix))]
const SHELL_START_STATUS_WAIT: std::time::Duration = std::time::Duration::from_millis(20);

/// Which look at an exited child's status [`App::exit_status`] is making.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExitLook {
    /// The first on the exit path.
    First,
    /// The ledger's second chance after the classifier found nothing.
    Retry,
}

/// Whether a native close preflight may take over the screen when a reducer answers [`crate::native_app::CloseReadiness::Blocked`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClosePreflightVisibility {
    /// A person asked for this close: surface the reducer's recovery commands on the blocking leaf.
    Interactive,
    /// Background policy asked: report the verdict, disturb nothing.
    Quiet,
}

/// Type `line` and Enter into a fresh session's shell, through its canonical input receipt, as ONE frame (`line + CR`).
pub(crate) fn inject_launch_line(
    term: &std::sync::Arc<std::sync::Mutex<aterm_core::terminal::Terminal>>,
    ctx: &crate::SessionCtx,
    line: &str,
) {
    let mut bytes = line.as_bytes().to_vec();
    bytes.push(0x0d);
    let event = crate::input::InputEvent::KeySequence(bytes);
    crate::app_input::tracked_egress(
        term,
        &ctx.modes,
        &ctx.sink,
        &ctx.output_echo,
        &event,
        crate::input::EgressMode::Backpressured,
    );
}

/// Decide whether a background tab's new title epoch changes visible tab chrome.
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
#[must_use]
pub(crate) fn resolved_terminal_title_rung(
    user_title: Option<&str>,
    live_title: &str,
    live_cwd: Option<&str>,
) -> Option<String> {
    let live_title = crate::tab_label::informative(live_title);
    user_title
        .filter(|title| !title.is_empty())
        .map(str::to_owned)
        .or_else(|| (!live_title.is_empty()).then(|| live_title.to_string()))
        .or_else(|| live_cwd.filter(|cwd| !cwd.is_empty()).map(home_abbreviated))
}

/// Make the keep-stale tab-title cache agree with the exact snapshot that caused an effective chrome refresh.
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
        cache.remove(&session);
    }
}

#[cfg(test)]
thread_local! {
    /// The herald's notifications on this thread (test-only).
    pub(crate) static HERALD_POSTS: std::cell::RefCell<Vec<crate::status_item::HeraldNotice>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// Why the notifier's queue refuses this thread's next herald post (test-only).
    pub(crate) static HERALD_REFUSAL: std::cell::Cell<Option<&'static str>> =
        const { std::cell::Cell::new(None) };
    static RAISED_WINDOWS: std::cell::RefCell<Vec<WindowId>> =
        const { std::cell::RefCell::new(Vec::new()) };
    #[cfg(target_os = "linux")]
    static RAISED_ACTIVATIONS: std::cell::RefCell<Vec<Option<String>>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// The windows this thread's `App::raise_window` calls named, drained (test-only).
#[cfg(test)]
pub(crate) fn take_raised_windows() -> Vec<WindowId> {
    RAISED_WINDOWS.with(|raised| std::mem::take(&mut *raised.borrow_mut()))
}

/// The activation token each of this thread's `App::raise_window` calls presented, `None` where it had none, drained (test-only, Linux).
#[cfg(all(test, target_os = "linux"))]
pub(crate) fn take_raised_activations() -> Vec<Option<String>> {
    RAISED_ACTIVATIONS.with(|raised| std::mem::take(&mut *raised.borrow_mut()))
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
    fn a_console_program_path_falls_through_to_the_cwd() {
        let pwsh = "C:\\Program Files\\WindowsApps\\Microsoft.PowerShell_7.6.6.0_x64__8wekyb3d8bbwe\\pwsh.exe";
        assert_eq!(
            resolved_terminal_title_rung(None, pwsh, Some("/srv/demo-cwd")).as_deref(),
            Some("/srv/demo-cwd")
        );
        assert_eq!(resolved_terminal_title_rung(None, pwsh, None), None);
        assert_eq!(
            resolved_terminal_title_rung(
                None,
                "C:\\Windows\\system32\\cmd.exe - ping build-01",
                Some("/srv/demo-cwd")
            )
            .as_deref(),
            Some("ping build-01")
        );
        assert_eq!(
            resolved_terminal_title_rung(None, "~\\aterm", Some("/srv/demo-cwd")).as_deref(),
            Some("~\\aterm")
        );
    }

    #[test]
    fn captured_drift_snapshot_wins_a_contended_refresh_read() {
        let mut cache = std::collections::HashMap::from([(7, "stale".to_owned())]);

        synchronize_captured_title_cache(&mut cache, 7, Some("fresh"), true);
        assert_eq!(cache.get(&7).map(String::as_str), Some("fresh"));

        synchronize_captured_title_cache(&mut cache, 7, Some("⠙ fresh"), false);
        assert_eq!(cache.get(&7).map(String::as_str), Some("fresh"));

        synchronize_captured_title_cache(&mut cache, 7, None, true);
        assert!(!cache.contains_key(&7));
    }
}

fn is_terminal_tab(tab: &crate::tab_model::Tab, views: &crate::tab_model::ViewStore) -> bool {
    tab.root
        .leaves()
        .into_iter()
        .all(|view| matches!(views.get(view), Some(crate::tab_model::View::Terminal(_))))
}

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

/// If canonical focus names a terminal, point the compatibility mirror at the corresponding terminal projection entry.
pub(crate) fn align_terminal_projection_to_active(
    ws: &mut WindowState,
    views: &crate::tab_model::ViewStore,
) {
    let Some(active) = ws.tab_set.active_id() else {
        return;
    };
    if let Some(index) = terminal_projection_index(&ws.tab_set, views, active) {
        ws.tabs.switch_to(index);
    }
}

fn remove_terminal_projection(ws: &mut WindowState, projection_index: usize) {
    if ws.tabs.count == 1 {
        debug_assert_eq!(projection_index, 0);
        ws.tabs = TabIndex::new(0, 0);
    } else {
        let was_last = ws.tabs.close(projection_index);
        debug_assert!(!was_last);
    }
}

/// Cancel a DEFERRED last-tab close on `ws`.
pub(crate) fn cancel_pending_close(ws: &mut WindowState) {
    if std::mem::take(&mut ws.pending_close) {
        ws.pending_close_attribution = None;
    }
}

/// How far [`App::close_session_by_id`] got, not the wire verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CloseProgress {
    /// The session left the registry inside the close: nothing is pending.
    Retired,
    /// The close dropped window `.0`'s last tab.
    Deferred(WindowId),
}

/// The wire refusal of a busy pane close.
pub(crate) const WIRE_PANE_CLOSE_REFUSED: &str = "close refused (a running job in that pane)";

/// The flagless wire `tab` verb's refusal when no window is front.
pub(crate) const NO_FOCUSED_WINDOW: &str = "no window is focused";

/// Every edge table's rows at one moment ([`crate::connections::all_edges`]), with the connections revision read just before them.
pub(crate) struct EdgeSnapshot {
    revision: u64,
    edges: Vec<aterm_session::Edge>,
}

struct ConnectedPlan {
    origin_local: u64,
    host: Option<WindowId>,
    cwd: Option<String>,
    origin_folder: Option<crate::spawn_folder::PaneFolder>,
}

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

/// THE INSTANT A TASK'S END WAS TOLD AT, unix ms, for the menu bar's Done for you row
/// (`docs/DESIGN-harness-autopilot-2026-10-15.md` §11.4.3, item 1): `told`, the time the timeline
/// holds for the end's face (the face, or the one held across the host's "unknown" between loop
/// runs; `SessionTimeline::task_face_at`), else the end's own minute (`Stamp::unix_ms`); `0` where
/// neither is known.
pub(crate) fn end_wall_ms(end: crate::harness_glass::EndGlass, told: Option<i64>) -> u64 {
    told.or_else(|| end.at.and_then(crate::harness_glass::Stamp::unix_ms))
        .and_then(|ms| u64::try_from(ms).ok())
        .unwrap_or(0)
}

/// The `home`-relative suffix of `path`.
pub(crate) fn home_relative_suffix<'p>(path: &'p str, home: &str) -> Option<&'p str> {
    if home.is_empty() {
        return None;
    }
    let rest = path.strip_prefix(home)?;
    (rest.is_empty() || rest.starts_with('/')).then_some(rest)
}

/// The process `$HOME`, read once and cached.
pub(crate) fn cached_home() -> Option<&'static str> {
    static HOME: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    HOME.get_or_init(|| std::env::var("HOME").ok().filter(|home| !home.is_empty()))
        .as_deref()
}

/// `path` with a leading `$HOME` component abbreviated to `~`.
pub(crate) fn home_abbreviated(path: &str) -> String {
    match cached_home().and_then(|home| home_relative_suffix(path, home)) {
        Some(rest) => format!("~{rest}"),
        None => path.to_string(),
    }
}

/// A close a test runs inside another ([`CLOSE_BETWEEN_WRITES`]).
#[cfg(test)]
pub(crate) type BetweenWrites = Box<dyn FnOnce(&mut App)>;

#[cfg(test)]
thread_local! {
    /// What a test runs between a close's two store writes (its deregister, which links its closed-index slot, and its log's attach).
    pub(crate) static CLOSE_BETWEEN_WRITES: std::cell::RefCell<Option<BetweenWrites>> =
        const { std::cell::RefCell::new(None) };
}

impl App {
    /// Detach one terminal view and retire all per-session auxiliary state only when the pool confirms that this was the final attachment.
    pub(crate) fn detach_session_view(&mut self, session: u64) -> bool {
        let handed = self
            .pool
            .get(session)
            .and_then(|session| session.handoff_local_id);
        let dropped = self.pool.detach(session);
        if dropped {
            if let Some(local_id) = handed {
                self.note_handed_shell_lost(local_id);
            }
            self.chrome_labels.retire(session);
            self.retire_session_status(session);
            self.retire_presence(session);
            crate::harness_host::look_at_upgrades_soon();
            self.session_chrome.remove(&session);
            self.session_chrome_retry.remove(&session);
            self.link_estimates.remove(&session);
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

    fn note_handed_shell_lost(&mut self, local_id: u64) {
        if self.handoff_ready.is_none() || self.handoff_degraded {
            return;
        }
        aterm_log::warn!(
            "overlap handoff: adopted session {local_id} was dropped before the adoption proof \
             (its pane could not be kept); withholding the proof so the parked parent resumes \
             every shell"
        );
        self.handoff_degraded = true;
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
        self.closed_view_record_for(window, tab.id, tab.focus)
    }

    fn closed_view_record_for(
        &self,
        window: WindowId,
        tab_id: crate::tab_model::TabId,
        view: crate::tab_model::ViewId,
    ) -> Option<crate::closed_recovery::ClosedView> {
        let tab = self.windows.get(&window)?.tab_set.get(tab_id)?;
        if crate::closed_recovery::leaf_close_record_kind(tab.root.len())
            != crate::closed_recovery::LeafCloseRecordKind::ClosedView
        {
            return None;
        }
        let (parent, branch, axis, ratio) = tab.root.leaf_placement(view)?;
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
            view: self.view_restore_descriptor(view)?,
            placement,
        })
    }

    fn retain_closed_view(&mut self, record: crate::closed_recovery::ClosedView) {
        let now_ms = self.lat_epoch.elapsed().as_millis() as u64;
        self.closed_recovery.views.push(record, now_ms);
    }

    pub(crate) fn live_view_presentation(
        &self,
        view: crate::tab_model::ViewId,
    ) -> Option<crate::tab_model::TabPresentation> {
        match self.view_store.get(view).copied()? {
            crate::tab_model::View::Terminal(terminal) => {
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
                    icon: Some(presentation.icon),
                    indicators: crate::tab_model::TabIndicators {
                        dirty: presentation.indicators.dirty,
                        busy: presentation.indicators.busy,
                        attention: presentation.indicators.attention
                            || self.edit_view_attention(view),
                        status_attention: false,
                    },
                    conn: None,
                    closable: presentation.closable,
                    tooltip: presentation.tooltip,
                })
            }
        }
    }

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

    /// Every tab that views this session, addressed by leaf rather than by focus.
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

    fn tab_leaf_indicators(
        &self,
        wid: WindowId,
        tab_id: crate::tab_model::TabId,
    ) -> Option<crate::tab_model::TabIndicators> {
        let tab = self.windows.get(&wid)?.tab_set.get(tab_id)?;
        Some(
            tab.root
                .leaves()
                .into_iter()
                .filter_map(|view| match self.view_store.get(view)? {
                    crate::tab_model::View::Terminal(terminal) => {
                        Some(self.session_status_indicators(terminal.session))
                    }
                    crate::tab_model::View::Native(native) => self
                        .native_runtime
                        .presentation(native.instance, view)
                        .ok()
                        .map(|presentation| crate::tab_model::TabIndicators {
                            dirty: presentation.indicators.dirty,
                            busy: presentation.indicators.busy,
                            attention: presentation.indicators.attention
                                || self.edit_view_attention(view),
                            status_attention: false,
                        }),
                })
                .fold(crate::tab_model::TabIndicators::default(), |acc, bits| {
                    crate::tab_model::TabIndicators {
                        dirty: acc.dirty || bits.dirty,
                        busy: acc.busy || bits.busy,
                        attention: acc.attention || bits.attention,
                        status_attention: acc.status_attention || bits.status_attention,
                    }
                }),
        )
    }

    /// Refresh the native owners' indicators and the terminal classifier's contribution together, preserving the composed title and tooltip.
    pub(crate) fn refresh_native_tab_indicators(
        &mut self,
        wid: WindowId,
        tab_id: crate::tab_model::TabId,
    ) -> bool {
        self.refresh_tab_indicators(wid, tab_id, true)
    }

    /// A terminal status update owns busy/status_attention, including native siblings' current busy state.
    pub(crate) fn refresh_tab_status_indicators(
        &mut self,
        wid: WindowId,
        tab_id: crate::tab_model::TabId,
    ) -> bool {
        self.refresh_tab_indicators(wid, tab_id, false)
    }

    fn refresh_tab_indicators(
        &mut self,
        wid: WindowId,
        tab_id: crate::tab_model::TabId,
        native_owner: bool,
    ) -> bool {
        let Some(folded) = self.tab_leaf_indicators(wid, tab_id) else {
            return false;
        };
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
        let indicators = crate::tab_model::TabIndicators {
            dirty: if native_owner {
                folded.dirty
            } else {
                tab.presentation.indicators.dirty
            },
            busy: folded.busy,
            attention: if native_owner {
                folded.attention
            } else {
                tab.presentation.indicators.attention
            },
            status_attention: folded.status_attention,
        };
        let changed = tab.presentation.indicators != indicators;
        tab.presentation.indicators = indicators;
        changed
    }

    pub(crate) fn promote_terminal_projection_if_needed(
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

    /// Open the singleton System State controller in the frontmost window.
    pub(crate) fn open_system_state_tab(&mut self) -> bool {
        let Some(wid) = self.frontmost_window else {
            return false;
        };
        self.open_system_state_tab_in_window(wid)
    }

    /// aterm ▸ System State… and its palette row (`MenuAction::SystemState`).
    pub(crate) fn open_system_state_from_menu(&mut self) {
        if let Err(why) = self.open_app(crate::app_control::OpenRequest::SystemState) {
            self.pending_action_refusal = Some(why);
        }
    }

    pub(crate) fn open_system_state_tab_in_window(&mut self, wid: WindowId) -> bool {
        let existing = self.windows.get(&wid).and_then(|window| {
            window.tab_set.tabs().iter().find_map(|tab| {
                tab.root.leaves().into_iter().find_map(|view| {
                    let crate::tab_model::View::Native(native) = self.view_store.get(view)? else {
                        return None;
                    };
                    (self.native_runtime.app(native.instance)?.kind()
                        == crate::native_app::AppKind::SystemState)
                        .then_some((tab.id, view))
                })
            })
        });
        let (tab, view) = match existing {
            Some(existing) => existing,
            None => match self.stage_system_state_tab(wid) {
                Ok((tab, view)) => (tab, view),
                Err(_) => return false,
            },
        };
        let Some(window) = self.windows.get_mut(&wid) else {
            return false;
        };
        if !window.tab_set.switch_to(tab)
            || !window
                .tab_set
                .active_mut()
                .is_some_and(|tab| tab.set_focus(view))
        {
            return false;
        }
        cancel_pending_close(window);
        window.last_present = None;
        self.resync_active_or_window(wid);
        self.sync_active_session();
        if let Some(window) = self
            .windows
            .get(&wid)
            .and_then(|window| window.os_window.as_ref())
        {
            window.focus_window();
            window.request_redraw();
        }
        true
    }

    /// Always mint a view for restore.
    pub(crate) fn stage_system_state_tab(
        &mut self,
        wid: WindowId,
    ) -> Result<(crate::tab_model::TabId, crate::tab_model::ViewId), NativeTabInstallError> {
        let state = crate::native_app::AppViewState::SystemState(Box::default());
        let presentation = crate::tab_model::TabPresentation {
            title: "System State".into(),
            icon: Some(crate::tab_model::TabIconKind::Settings),
            indicators: crate::tab_model::TabIndicators::default(),
            conn: None,
            closable: true,
            tooltip: Some("System State".into()),
        };
        if let Some(instance) = self
            .native_runtime
            .instance_by_kind(crate::native_app::AppKind::SystemState)
        {
            self.install_native_tab(wid, instance, state, presentation)
        } else {
            self.install_new_native_tab(
                wid,
                crate::native_app::NativeApp::SystemState(Box::new(
                    crate::system_state::SystemStateApp::new(
                        aterm_types::macos_setup::Plan::default(),
                    ),
                )),
                state,
                presentation,
            )
            .map(|(_, tab, view)| (tab, view))
        }
    }

    /// Attach one view of an existing native app instance and append it as the active stable tab in `wid`.
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
        self.resync_active_or_window(wid);
        Ok((tab_id, view))
    }

    /// Create a native app instance and install its first tab view atomically.
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

    /// Attach another view of an existing native app as a sibling of the focused canonical leaf, for the mixed-leaf tests (no shipping command duplicates a native view).
    #[cfg(test)]
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
        debug_assert!(
            self.structural_invariants_ok(),
            "{}",
            self.structural_invariant_violation().unwrap_or_default(),
        );
        Ok(view)
    }

    /// Re-project one terminal compatibility tree into its stable generic tab.
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

    /// Remove one stable view edge and its native reducer state, if any.
    pub(crate) fn remove_view_link(
        &mut self,
        view: crate::tab_model::ViewId,
    ) -> Option<crate::tab_model::View> {
        self.remove_view_link_for_close(view, crate::native_app::CloseScope::Window)
    }

    fn remove_view_link_for_close(
        &mut self,
        view: crate::tab_model::ViewId,
        scope: crate::native_app::CloseScope,
    ) -> Option<crate::tab_model::View> {
        let linked = self.view_store.get(view).copied()?;
        if let crate::tab_model::View::Native(native) = linked
            && let Some(document) = self.native_runtime.document_id(native.instance)
            && self
                .document_store
                .detach_view_if_ready(document, crate::document_store::DocumentViewId(view.get()))
                .is_err()
        {
            return None;
        }
        let removed = self.view_store.remove(view)?;
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
                self.finish_system_state_close(native.instance, scope);
                self.native_runtime.remove_instance(native.instance);
                self.refresh_disambiguated_document_titles();
            }
            self.native_documents.release_due = true;
        }
        self.edit_view_retired(view);
        Some(removed)
    }

    pub(crate) fn remove_tab_views(&mut self, tab: &crate::tab_model::Tab) {
        for view in tab.root.leaves() {
            self.remove_view_link(view);
        }
    }

    /// Whether the visible tab strip is enabled (`tab_strip_rows > 0`), for every window.
    pub(crate) const fn tab_strip_enabled(&self) -> bool {
        self.tab_strip_rows > 0
    }

    /// THE CHROME ROWS above the terminal grid.
    pub(crate) fn chrome_rows(&self, wid: WindowId) -> u16 {
        let _ = wid;
        self.chrome_rows_shared()
    }

    /// The chrome rows every window shares — the strip and the message band.
    pub(crate) const fn chrome_rows_shared(&self) -> u16 {
        self.tab_strip_rows.saturating_add(self.message_band_rows)
    }

    /// One title per top-level tab of window `wid`, for the strip labels.
    pub(crate) fn tab_titles(&mut self, wid: WindowId) -> Vec<String> {
        let Some(ws) = self.windows.get(&wid) else {
            return Vec::new();
        };
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
                    return (fallback, None, None);
                };
                let Some(s) = self.pool.get(view.session) else {
                    return (fallback, None, None);
                };
                let user_title = s
                    .ctx
                    .meta
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .presentation_value("title");
                if user_title.is_some() {
                    return (fallback, Some(view.session), user_title);
                }
                let term_label = |t: &aterm_core::terminal::Terminal| {
                    use crate::cwd_native::ReportedCwd as _;
                    let cwd = t.native_working_directory();
                    resolved_terminal_title_rung(None, t.title(), cwd.as_deref())
                        .unwrap_or_default()
                };
                let fresh = match s.term.try_lock() {
                    Ok(t) => Some(term_label(&t)),
                    Err(std::sync::TryLockError::Poisoned(p)) => Some(term_label(&p.into_inner())),
                    Err(std::sync::TryLockError::WouldBlock) => None,
                };
                (fallback, Some(view.session), fresh)
            })
            .collect();
        let chrome_labels = &self.chrome_labels;
        let Some(ws) = self.windows.get_mut(&wid) else {
            return Vec::new();
        };
        let live: std::collections::HashSet<u64> =
            read.iter().filter_map(|(_, sid, _)| *sid).collect();
        ws.tab_title_cache.retain(|sid, _| live.contains(sid));
        read.into_iter()
            .map(|(fallback, sid, fresh)| {
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
                chrome_labels.present(
                    Some(sid),
                    &raw,
                    crate::chrome_label::ChromeSurface::TabStrip,
                )
            })
            .collect()
    }

    fn stamp_tab_connection_roles(&mut self, wid: WindowId) {
        let roles = if self.config.tab_connection_badge_or_default() {
            crate::connections::roles_by_local(&self.store)
        } else {
            std::collections::HashMap::new()
        };
        let Some(ws) = self.windows.get(&wid) else {
            return;
        };
        let per_tab: Vec<Option<crate::tab_model::TabConnRole>> = ws
            .tab_set
            .tabs()
            .iter()
            .map(|tab| {
                let (mut outbound, mut inbound) = (false, false);
                tab.root.any_leaf(&mut |view| {
                    if let Some(session) = self
                        .view_store
                        .get(*view)
                        .copied()
                        .and_then(crate::tab_model::View::terminal_session)
                        && let Some(role) = roles.get(&session)
                    {
                        outbound |= role.outbound;
                        inbound |= role.inbound;
                    }
                    false
                });
                crate::tab_model::TabConnRole::from_flags(outbound, inbound)
            })
            .collect();
        if let Some(ws) = self.windows.get_mut(&wid) {
            for (index, conn) in per_tab.into_iter().enumerate() {
                if let Some(tab) = ws.tab_set.tab_at_mut(index) {
                    tab.presentation.conn = conn;
                }
            }
        }
    }

    /// Canonical, non-title chrome metadata paired by index with [`Self::tab_titles`].
    pub(crate) fn tab_strip_metadata(
        &self,
        wid: WindowId,
    ) -> Vec<crate::tab_bar::TabStripMetadata> {
        let mut metadata = self.windows.get(&wid).map_or_else(Vec::new, |ws| {
            ws.tab_set
                .tabs()
                .iter()
                .map(|tab| crate::tab_bar::TabStripMetadata::from_presentation(&tab.presentation))
                .collect()
        });
        self.stamp_presence_chips(wid, &mut metadata);
        metadata
    }

    /// The per-tab chrome extensions (hover tooltip and right-click menu model) paired by index with [`Self::tab_titles`].
    pub(crate) fn tab_chrome_ext(
        &mut self,
        wid: WindowId,
        titles: &[String],
    ) -> Vec<crate::session_chrome::TabChromeExt> {
        // A backstop bound on the cache (`detach_session_view` drops a closed
        // session's entry). The live-id set is collected first so the retain
        // closure borrows no field.
        if self.session_chrome.len() > 64 {
            let live: std::collections::HashSet<u64> = self.pool.iter().map(|s| s.id).collect();
            self.session_chrome.retain(|sid, _| live.contains(sid));
        }
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
                        let tooltip = if session.is_none() {
                            tab.presentation.tooltip.clone()
                        } else {
                            None
                        };
                        (session, tooltip)
                    })
                    .collect()
            });
        let mut edges = None;
        let ext: Vec<crate::session_chrome::TabChromeExt> = tabs
            .iter()
            .enumerate()
            .map(|(i, (session, presentation_tooltip))| match session {
                Some(session) => {
                    let label = titles.get(i).map_or("", String::as_str);
                    self.composed_session_chrome(wid, *session, label, &mut edges)
                }
                None => crate::session_chrome::TabChromeExt {
                    tooltip: presentation_tooltip.clone(),
                    menu: Vec::new(),
                },
            })
            .collect();
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

    /// THE SESSION CHROME'S WAKE, for the event loop to fold at `now` (`now_ms` on [`crate::turn_ledger::now_ms`]'s clock): one wake at the earliest instant the clock moves a cached composition ([`crate::session_chrome::clock_expiry_ms`]: an upgrade's words).
    pub(crate) fn session_chrome_wake(
        &mut self,
        now: std::time::Instant,
        now_ms: u64,
    ) -> Option<std::time::Instant> {
        use crate::session_chrome as chrome;
        self.advance_session_chrome_expiry(now_ms);
        if self.session_chrome_expiry.pending() {
            return Some(now + crate::SESSION_CHROME_EXPIRY_DRAIN_INTERVAL);
        }
        let expiry_ms = chrome::next_cache_expiry_ms(&self.session_chrome)?;
        let remaining_ms = expiry_ms
            .saturating_sub(now_ms)
            .min(chrome::CACHE_MAX_AGE_MS);
        Some(now + std::time::Duration::from_millis(remaining_ms))
    }

    /// Advance one expired session's fan-out by at most [`crate::session_chrome::EXPIRY_WINDOW_SCAN_BUDGET`] window scans.
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

    /// THE SESSION LOG'S CLAUSE as `session`'s chrome is composed with it (phase 3B), only with `[session_log] clause` on: a `try_lock` on `timeline`, taken after the blocking `high_id` read as a separate acquisition.
    pub(crate) fn chrome_clause_read(
        &mut self,
        retry_window: WindowId,
        session: u64,
        timeline: &std::sync::Mutex<crate::session_timeline::SessionTimeline>,
        now: u64,
    ) -> Option<(
        crate::session_chrome::ClauseKey,
        crate::session_chrome::ClauseLine,
    )> {
        if !self.config.session_log_clause() {
            return None;
        }
        match timeline.try_lock() {
            Ok(g) => g.slog().chrome_clause(now),
            Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner().slog().chrome_clause(now),
            Err(std::sync::TryLockError::WouldBlock) => {
                if let Some(window) = self.windows.get_mut(&retry_window) {
                    window.tab_title_epochs.remove(&session);
                }
                self.session_chrome_retry.insert(session);
                self.title_drift.pending.insert(session);
                self.session_chrome
                    .get(&session)
                    .and_then(|c| c.clause.clone().zip(c.clause_line.clone()))
            }
        }
    }

    fn composed_session_chrome(
        &mut self,
        retry_window: WindowId,
        session: u64,
        label: &str,
        edges: &mut Option<EdgeSnapshot>,
    ) -> crate::session_chrome::TabChromeExt {
        use crate::session_chrome as chrome;
        let Some((term, ctx)) = self
            .pool
            .get(session)
            .map(|session| (session.term.clone(), session.ctx.clone()))
        else {
            return chrome::TabChromeExt::default();
        };
        let connections_revision = self.connections.revision();
        let upgrade_revision = self.upgrade_view.revision();
        let activity_revision = self.session_status_revision(session);
        let high_id = ctx
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .view_head()
            .unwrap_or(0);
        let now = crate::turn_ledger::now_ms();
        let clause = self.chrome_clause_read(retry_window, session, &ctx.timeline, now);
        if let Some(c) = self.session_chrome.get(&session)
            && c.high_id == high_id
            && c.label == label
            && c.activity_revision == activity_revision
            && c.connections_revision == connections_revision
            && c.upgrade_revision == upgrade_revision
            && c.clause.as_ref() == clause.as_ref().map(|(key, _)| key)
        {
            return c.ext.clone();
        }
        let activity = self.session_status_text(session);
        let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let native_cwd = |t: &aterm_core::terminal::Terminal| {
            use crate::cwd_native::ReportedCwd as _;
            t.native_working_directory()
                .filter(|c| !c.is_empty())
                .map(|c| c.into_owned())
        };
        let cwd = match term.try_lock() {
            Ok(term) => native_cwd(&term),
            Err(std::sync::TryLockError::Poisoned(poisoned)) => native_cwd(&poisoned.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => {
                let stale = self
                    .session_chrome
                    .get(&session)
                    .map(|cached| cached.ext.clone())
                    .unwrap_or_default();
                if let Some(window) = self.windows.get_mut(&retry_window) {
                    window.tab_title_epochs.remove(&session);
                }
                self.session_chrome_retry.insert(session);
                self.title_drift.pending.insert(session);
                return stale;
            }
        };
        let (state, has_session, sid) = {
            let g = self.store.read().unwrap_or_else(|p| p.into_inner());
            match g.by_local(session) {
                Some(h) => (
                    Some(h.state.as_str().to_string()),
                    true,
                    Some(h.sid.clone()),
                ),
                None => (None, false, None),
            }
        };
        let snapshot = edges.get_or_insert_with(|| EdgeSnapshot {
            revision: self.connections.revision(),
            edges: crate::connections::all_edges(&self.store),
        });
        let connections_revision = connections_revision.min(snapshot.revision);
        let connections = sid.as_ref().map_or_else(Vec::new, |sid| {
            self.connection_facts_in(&snapshot.edges, sid)
        });
        let timeline: Vec<chrome::TimelineNote> = {
            let tl = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            chrome::timeline_tail(tl.view_newest_first())
        };
        let can_rename = self.can_rename_session(retry_window);
        let upgrade = sid.as_ref().and_then(|sid| {
            self.upgrade_view
                .menu_for(sid.as_str(), crate::upgrade_host::now_s())
        });
        let agent = self.agent_menu_facts(session);
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
            connections,
            upgrade,
            attention: meta.presentation_value("attention"),
            agent,
            sid,
            clause: clause.as_ref().map(|(_, line)| line.clone()),
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
                connections_revision,
                upgrade_revision,
                expires_ms: chrome::clock_expiry_ms(now, &input),
                clause: clause.as_ref().map(|(key, _)| key.clone()),
                clause_line: input.clause.clone(),
                ext: ext.clone(),
            },
        );
        ext
    }

    fn connection_facts_in(
        &self,
        edges: &[aterm_session::Edge],
        sid: &aterm_session::SessionId,
    ) -> Vec<crate::session_chrome::ConnectionFact> {
        use crate::session_chrome::{ConnDirection, ConnFlow, ConnectionFact};
        #[derive(Default)]
        struct Halves {
            out_any: bool,
            out_write: bool,
            in_any: bool,
            in_write: bool,
        }
        let mut peers: std::collections::BTreeMap<String, (aterm_session::SessionId, Halves)> =
            std::collections::BTreeMap::new();
        for edge in edges {
            // Distinct branch.
            if edge.src == edge.dst {
                continue;
            }
            let (peer, outbound) = if edge.src == *sid {
                (edge.dst.clone(), true)
            } else if edge.dst == *sid {
                (edge.src.clone(), false)
            } else {
                continue;
            };
            let write = matches!(edge.op, aterm_session::Op::WriteInput);
            let entry = &mut peers
                .entry(peer.as_str().to_string())
                .or_insert_with(|| (peer, Halves::default()))
                .1;
            if outbound {
                entry.out_any = true;
                entry.out_write |= write;
            } else {
                entry.in_any = true;
                entry.in_write |= write;
            }
        }
        if peers.is_empty() {
            return Vec::new();
        }
        let g = self.store.read().unwrap_or_else(|p| p.into_inner());
        peers
            .into_values()
            .map(|(peer, halves)| {
                let peer_title = g.by_sid(&peer).map_or_else(
                    || "unknown".to_string(),
                    crate::session_store::SessionHandle::display_title,
                );
                let (direction, push) = match (halves.out_any, halves.in_any) {
                    (true, true) => (ConnDirection::Peer, halves.out_write),
                    (true, false) => (ConnDirection::Outbound, halves.out_write),
                    _ => (ConnDirection::Inbound, halves.in_write),
                };
                ConnectionFact {
                    peer_sid: peer,
                    peer_title,
                    direction,
                    kind: if push { ConnFlow::Push } else { ConnFlow::Pull },
                    live: false,
                }
            })
            .collect()
    }

    /// Full hover/accessibility context paired by index with [`Self::tab_titles`].
    pub(crate) fn tab_tooltips(&self, wid: WindowId) -> Vec<Option<String>> {
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
                    match session {
                        Some(session) => {
                            self.presence_hover(session, tab.presentation.tooltip.clone())
                        }
                        None => tab.presentation.tooltip.clone(),
                    }
                })
                .collect()
        })
    }

    /// A cheap fingerprint of the visible tab strip, folded into the redraw [`crate::RepaintKey`] so a tab change repaints the strip over an unchanged grid.
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
            if let Some(ws) = self.windows.get(&wid)
                && !ws.preedit.is_empty()
            {
                ws.preedit.hash(&mut h);
                ws.preedit_caret.hash(&mut h);
            }
        }
        if let Some(ws) = self.windows.get(&wid)
            && (ws.strip_hover.is_some()
                || ws.strip_hover_new_tab
                || ws.strip_hover_version
                || ws.strip_hover_caption.is_some())
        {
            "strip-hover".hash(&mut h);
            ws.strip_hover.hash(&mut h);
            ws.strip_hover_new_tab.hash(&mut h);
            ws.strip_hover_version.hash(&mut h);
            ws.strip_hover_caption.hash(&mut h);
        }
        if let Some(frame) = self.windows.get(&wid).and_then(|ws| ws.client_frame) {
            "client-frame".hash(&mut h);
            frame.hash(&mut h);
        }
        h.finish() | 1
    }

    /// "Move Tab to New Window" (Cmd-Shift-N).
    pub(crate) fn detach_active_tab_logical(&mut self) -> Option<WindowId> {
        let wid_a = self.frontmost_window?;
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
                        ui_waiting: session.ctx.ui_waiting.clone(),
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
        if let Some(ws) = self.windows.get_mut(&wid_a) {
            if let Some(index) = terminal_index {
                ws.layouts.remove(index);
                remove_terminal_projection(ws, index);
            }
            ws.tab_set.remove(stable_tab.id);
            align_terminal_projection_to_active(ws, &self.view_store);
        }
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
        self.windows.insert(wid_b, Box::new(ws_b));
        self.frontmost_window = Some(wid_b);
        self.sync_window(wid_a);
        self.sync_active_session();
        debug_assert!(
            self.structural_invariants_ok(),
            "window/session structural invariants violated after tearing a tab into its own window: {}",
            self.structural_invariant_violation().unwrap_or_default(),
        );
        Some(wid_b)
    }

    /// Full "Move Tab to New Window".
    pub(crate) fn detach_active_tab(&mut self, el: &ActiveEventLoop) {
        let wid_a = self.frontmost_window;
        let Some(wid_b) = self.detach_active_tab_logical() else {
            return;
        };
        if !self.headless && !self.attach_os_window(el, wid_b) {
            self.detach_rollback_logical(wid_a, wid_b);
        }
    }

    /// Undo a `detach_active_tab_logical` when the new window's OS surface failed.
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
        self._toolbars.remove(&wid_b);
        self.frontmost_window = wid_a;
        self.sync_active_session();
    }

    /// "Move Tab to Next Window" (Cmd-Shift-M).
    pub(crate) fn migrate_active_tab_to_next_window(&mut self) -> Result<(), &'static str> {
        let Some(wid_a) = self.frontmost_window else {
            return Err("no window is focused");
        };
        let dest = self
            .windows
            .range((std::ops::Bound::Excluded(wid_a), std::ops::Bound::Unbounded))
            .next()
            .map(|(k, _)| *k)
            .or_else(|| self.windows.keys().next().copied());
        let Some(wid_b) = dest else {
            return Err("there is no other window to move the tab to");
        };
        if wid_b == wid_a {
            return Err("there is no other window to move the tab to");
        }
        let (stable_tab, terminal_index, tree) = {
            let Some(ws) = self.windows.get(&wid_a) else {
                return Err("the focused window is gone");
            };
            let Some(stable_tab) = ws.tab_set.active().cloned() else {
                return Err("this window has no active tab to move");
            };
            let terminal_index =
                terminal_projection_index(&ws.tab_set, &self.view_store, stable_tab.id);
            let tree = terminal_index.and_then(|index| ws.layouts.get(index).cloned());
            if terminal_index.is_some() && tree.is_none() {
                return Err("this tab's pane layout is missing, so it cannot be moved");
            }
            (stable_tab, terminal_index, tree)
        };
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
        self.frontmost_window = Some(wid_b);
        if tree.is_some() {
            self.resize_panes(wid_b);
        }
        self.sync_window(wid_b);
        if source_now_empty {
            let _ = self.close_window_logical(wid_a);
        } else {
            self.sync_window(wid_a);
        }
        self.sync_active_session();
        if let Some(w) = self.windows.get(&wid_b).and_then(|ws| ws.os_window.clone()) {
            w.focus_window();
        }
        debug_assert!(
            self.structural_invariants_ok(),
            "window/session structural invariants violated after \
             migrate_active_tab_to_next_window: {}",
            self.structural_invariant_violation().unwrap_or_default(),
        );
        Ok(())
    }

    /// "Open Active Session in New Window" (Cmd-Shift-O).
    pub(crate) fn open_active_session_in_new_window_logical(&mut self) -> Option<WindowId> {
        let wid_a = self.frontmost_window?;
        let s = self.front_terminal(wid_a)?.session;
        let (rows, cols) = self.windows.get(&wid_a).map(|ws| (ws.rows, ws.cols))?;
        self.pool.attach(s);
        let Some(sess) = self.pool.get(s) else {
            self.detach_session_view(s);
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
            sess.ctx.ui_waiting.clone(),
            s,
            rows,
            cols,
            metrics,
            TabIndex::new(0, 1),
            vec![layout],
            crate::tab_model::TabSet::new(tab),
        );
        self.windows.insert(wid_b, Box::new(ws_b));
        self.install_window_config_assets(wid_b);
        self.frontmost_window = Some(wid_b);
        self.sync_active_session();
        debug_assert!(
            self.structural_invariants_ok(),
            "window/session structural invariants violated after tearing a tab into its own window: {}",
            self.structural_invariant_violation().unwrap_or_default(),
        );
        Some(wid_b)
    }

    /// Full "Open Active Session in New Window".
    pub(crate) fn open_active_session_in_new_window(&mut self, el: &ActiveEventLoop) {
        let Some(wid) = self.open_active_session_in_new_window_logical() else {
            return;
        };
        if !self.headless && !self.attach_os_window(el, wid) {
            self.close_window_logical(wid);
        }
    }

    /// Test-only: append a stub `session` as a new tab of existing window `wid` and switch to it, through the [`Self::install_tab_session`] body a tab lands through ([`Self::install_new_tab`]).
    #[cfg(test)]
    pub(crate) fn push_stub_tab(&mut self, wid: WindowId, session: crate::Session) {
        self.install_new_tab(wid, session);
    }

    /// Test-only: split window `wid`'s active tab vertically, spawning a stub session for the new focused pane, through the [`Self::install_split_session`] body a split lands through ([`Self::install_split_pane`]).
    #[cfg(test)]
    pub(crate) fn split_active_stub_tab(&mut self, wid: WindowId) -> u64 {
        self.split_active_stub_tab_dir(wid, pane::SplitDir::Vertical)
    }

    /// [`Self::split_active_stub_tab`] in a named direction.
    #[cfg(test)]
    pub(crate) fn split_active_stub_tab_dir(&mut self, wid: WindowId, dir: pane::SplitDir) -> u64 {
        self.split_active_stub_tab_dir_with_sink(
            wid,
            dir,
            std::sync::Arc::new(aterm_session::sink::SinkWriter::new(-1)),
        )
    }

    /// [`Self::split_active_stub_tab_dir`] with a caller-provided sink for the new pane's session, so a test can observe bytes crossing that pane's own egress.
    #[cfg(test)]
    pub(crate) fn split_active_stub_tab_dir_with_sink(
        &mut self,
        wid: WindowId,
        dir: pane::SplitDir,
        sink: std::sync::Arc<aterm_session::sink::SinkWriter>,
    ) -> u64 {
        let sid = self.next_session_id;
        let stub = crate::stub_session_with_sink(sid, sink);
        if let Err(why) = self.install_split_pane(wid, dir, stub) {
            panic!("stub split must succeed: {why}");
        }
        sid
    }

    /// Test-only heterogeneous counterpart of [`Self::split_active_stub_tab`].
    #[cfg(test)]
    pub(crate) fn split_active_with_stub_terminal(
        &mut self,
        wid: WindowId,
        axis: crate::tab_model::SplitAxis,
    ) -> (u64, crate::tab_model::ViewId) {
        self.split_active_with_stub_terminal_with_sink(
            wid,
            axis,
            std::sync::Arc::new(aterm_session::sink::SinkWriter::new(-1)),
        )
    }

    /// [`Self::split_active_with_stub_terminal`] with a caller-provided sink for the new pane's session.
    #[cfg(test)]
    pub(crate) fn split_active_with_stub_terminal_with_sink(
        &mut self,
        wid: WindowId,
        axis: crate::tab_model::SplitAxis,
        sink: std::sync::Arc<aterm_session::sink::SinkWriter>,
    ) -> (u64, crate::tab_model::ViewId) {
        debug_assert!(
            self.active_tree(wid).is_none(),
            "split_active_with_stub_terminal is the heterogeneous arm; an \
             all-terminal active tab must use split_active_stub_tab"
        );
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
        self.pool.insert(crate::stub_session_with_sink(sid, sink));
        self.resize_panes(wid);
        self.resync_active_or_window(wid);
        (sid, view)
    }

    /// Cmd-T: open a new tab (a fresh shell session in the frontmost window) and switch to it.
    pub(crate) fn open_tab(&mut self) {
        if let Some(front) = self.frontmost_window {
            self.open_tab_in(front);
        }
    }

    /// Revalidate the booked authority before attaching a reader or registering the worker's newborn.
    pub(crate) fn validate_spawn_admission(
        &self,
        admission: &crate::tab_spawn::SpawnAdmission,
        host: Option<WindowId>,
    ) -> Result<(), String> {
        match admission {
            crate::tab_spawn::SpawnAdmission::Plain => {}
            crate::tab_spawn::SpawnAdmission::Connected { origin, local } => {
                let store = self.store.read().unwrap_or_else(|p| p.into_inner());
                let Some(handle) = store.by_sid(origin) else {
                    return Err("no such origin session".to_string());
                };
                if handle.local_id != *local {
                    return Err("the origin session changed while spawning".to_string());
                }
                if matches!(handle.state, session_store::SessionState::Exited) {
                    return Err("the origin exited before the session started".to_string());
                }
                if host.is_some_and(|window| self.terminal_view_location(window, *local).is_none())
                {
                    return Err("the booked window no longer hosts the origin session".to_string());
                }
            }
            crate::tab_spawn::SpawnAdmission::Summary => {}
        }
        Ok(())
    }

    /// Resolve and validate a connected spawn's origin and place before anything is created.
    pub(crate) fn connected_spawn_precheck(
        &self,
        place: crate::connections::ConnectedSpawnPlace,
        origin: &aterm_session::SessionId,
    ) -> Result<(u64, Option<WindowId>), String> {
        let origin_local = {
            let g = self.store.read().unwrap_or_else(|p| p.into_inner());
            match g.by_sid(origin) {
                None => return Err("no such session".to_string()),
                Some(h) if matches!(h.state, session_store::SessionState::Exited) => {
                    return Err("exited".to_string());
                }
                Some(h) => h.local_id,
            }
        };
        match place {
            crate::connections::ConnectedSpawnPlace::Window => {
                if self.headless {
                    return Err("headless: use place=tab".to_string());
                }
                Ok((origin_local, None))
            }
            crate::connections::ConnectedSpawnPlace::Tab => {
                let host = self
                    .windows
                    .keys()
                    .find(|wid| self.terminal_view_location(**wid, origin_local).is_some())
                    .copied();
                match host {
                    Some(host) => Ok((origin_local, Some(host))),
                    None => Err("no window hosts the origin session".to_string()),
                }
            }
        }
    }

    fn session_folder_by_local(&self, local: u64) -> Option<crate::spawn_folder::PaneFolder> {
        self.pool.get(local).and_then(Self::session_folder)
    }

    /// Mint a connected spawn's `both` connection through the one mint path ([`crate::connections::connect_in`]).
    pub(crate) fn mint_connected(
        &self,
        kind: crate::connections::ConnectedSpawnKind,
        origin: &aterm_session::SessionId,
        newborn: &aterm_session::SessionId,
        surface: &str,
    ) -> Result<(), String> {
        let (src, dst) = match kind {
            crate::connections::ConnectedSpawnKind::Controlled => (origin, newborn),
            crate::connections::ConnectedSpawnKind::Controller => (newborn, origin),
        };
        let dst_ctx = {
            let g = self.store.read().unwrap_or_else(|p| p.into_inner());
            match g.by_sid(dst) {
                Some(h) => h.ctx.clone(),
                None => return Err("no such session".to_string()),
            }
        };
        if crate::connections::connect_in(
            &self.connections,
            src,
            dst,
            &dst_ctx.edges,
            &dst_ctx.nonce,
            aterm_session::ConnectionKind::Both,
            surface,
        ) {
            Ok(())
        } else {
            Err("self-loop".to_string())
        }
    }

    /// THE CONNECTED SPAWN end-to-end (design §2.3 / §6 `spawn connected=`), the wire handler's (`Wake::SpawnConnectedSession`; no menu row starts one any more, `docs/DESIGN-owner-surfaces-2026-10-16.md` S12).
    #[allow(
        clippy::too_many_arguments,
        reason = "the connected spawn's birth facts — kind, place, origin, cwd and the \
                  theme — are each a per-spawn datum, and `done` \
                  is its answer; a wrapper struct would relocate the list, not simplify it"
    )]
    pub(crate) fn spawn_connected_session(
        &mut self,
        kind: crate::connections::ConnectedSpawnKind,
        place: crate::connections::ConnectedSpawnPlace,
        origin: &aterm_session::SessionId,
        cwd: Option<String>,
        theme: Option<crate::session_theme::SessionTheme>,
        done: crate::tab_spawn::SpawnDone,
    ) {
        use crate::connections::{ConnectedSpawnKind as Kind, ConnectedSpawnPlace as Place};
        let ConnectedPlan {
            origin_local,
            host,
            cwd,
            origin_folder,
        } = match self.connected_spawn_plan(place, origin, cwd) {
            Ok(plan) => plan,
            Err(why) => return done(self, Err(why)),
        };
        let lineage_parent = matches!(kind, Kind::Controlled).then(|| origin.clone());
        let observe = matches!(kind, Kind::Controller).then(|| origin.clone());
        let ask = crate::tab_spawn::SpawnAsk {
            admission: crate::tab_spawn::SpawnAdmission::Connected {
                origin: origin.clone(),
                local: origin_local,
            },
            folder_of: origin_folder,
            observe,
            theme,
            ..crate::tab_spawn::SpawnAsk::requested(cwd)
        };
        let then = Self::connected_landing(kind, origin.clone(), done);
        match place {
            Place::Window => self.open_window_off_loop(ask, lineage_parent, then),
            Place::Tab => {
                let host = host.expect("tab precheck resolves a host window");
                self.open_off_loop(host, crate::tab_spawn::SpawnPlace::Tab, ask, then);
            }
        }
    }

    fn connected_landing(
        kind: crate::connections::ConnectedSpawnKind,
        origin: aterm_session::SessionId,
        done: crate::tab_spawn::SpawnDone,
    ) -> crate::tab_spawn::TabSpawnThen {
        crate::tab_spawn::TabSpawnThen::Run(Box::new(move |app, landed| {
            let connected = landed.and_then(|sid| {
                let newborn = aterm_session::SessionId::new(sid);
                app.connect_newborn(kind, &origin, &newborn)
            });
            done(app, connected);
        }))
    }

    fn connected_spawn_plan(
        &self,
        place: crate::connections::ConnectedSpawnPlace,
        origin: &aterm_session::SessionId,
        cwd: Option<String>,
    ) -> Result<ConnectedPlan, String> {
        let (origin_local, host) = self.connected_spawn_precheck(place, origin)?;
        let origin_folder = if cwd.is_none() {
            self.session_folder_by_local(origin_local)
        } else {
            None
        };
        Ok(ConnectedPlan {
            origin_local,
            host,
            cwd,
            origin_folder,
        })
    }

    fn connect_newborn(
        &mut self,
        kind: crate::connections::ConnectedSpawnKind,
        origin: &aterm_session::SessionId,
        newborn: &aterm_session::SessionId,
    ) -> Result<String, String> {
        self.mint_connected(kind, origin, newborn, "wire")?;
        self.refresh_connection_surfaces();
        Ok(newborn.as_str().to_string())
    }

    /// Re-stamp every window's §4 tab marks through the one strip funnel and repaint.
    pub(crate) fn refresh_connection_surfaces(&mut self) {
        let wids: Vec<WindowId> = self.windows.keys().copied().collect();
        for wid in wids {
            let _ = self.refresh_window_tabs(wid);
        }
        self.overview_refresh_all();
        self.mark_status_dirty();
        self.request_redraw_all_windows();
    }

    /// One registry handle as the status item sees it (`docs/DESIGN-owner-surfaces-2026-10-16.md` §3): the tab's label as every Agents surface names it ([`Self::tab_label_of_session`]: the pill, the Overview, the tab menu and the long-hold entry name it the same), the session's PUBLISHED verdict ([`Self::publish_need`]), its agent word, its task record and what the person's hold refused.
    pub(crate) fn status_session_row(
        &self,
        h: &crate::session_store::SessionHandle,
    ) -> crate::status_item::SessionRow {
        let glass_end = self
            .harness_glass
            .get(&h.local_id)
            .and_then(crate::harness_glass::SessionGlass::end);
        let landed = self.chip_face_landed(h.local_id);
        let hold = self.shown_hold(h.local_id);
        let landing_tip = hold.and_then(|hold| {
            crate::harness_glass::hold_words(&hold, None, crate::harness_glass::HoldForm::Tip, None)
        });
        let (verdict, agent, task, told) = {
            let tl = h.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            (
                tl.need().clone(),
                tl.agent().word,
                tl.task().map(|t| crate::status_item::TaskRow {
                    state: t.state,
                    why: t.why,
                    from: t.from.clone(),
                    entered: t.entered,
                    since_unix_ms: t.since_unix_ms,
                    result_bytes: t.result_bytes,
                    result_path: t.result_path.clone(),
                    seen: false,
                }),
                glass_end.and_then(|end| tl.task_face_at(end.face.task_face())),
            )
        };
        let task = task.map(|mut t| {
            t.seen = self.done_seen.get(&h.local_id) == Some(&t.entered);
            t
        });
        let end = glass_end.map(|end| crate::status_item::EndRow {
            serial: end.serial,
            since_unix_ms: end_wall_ms(end, told),
            seen: !end.unseen(),
        });
        crate::status_item::SessionRow {
            id: h.local_id,
            sid: h.sid.as_str().to_string(),
            title: self.tab_label_of_session(h.local_id),
            need: verdict.need,
            waiting: verdict.waiting,
            unsupervised: verdict.recheck_unix.is_some(),
            held: verdict.held,
            agent,
            task,
            need_since: self.presence.herald.need_since(h.local_id),
            held_since: self.presence.herald.held_since(h.local_id),
            person_refusals: h.ctx.fabric.person_refusals(),
            held_by_host: h.ctx.fabric.holds().remote().map(|r| r.host.clone()),
            end,
            landed,
            landing: hold.is_some(),
            landing_tip,
            place: None,
        }
    }

    /// Whether session `session`'s Autopilot chip wears `✓ landed` ([`crate::claude_lights::ChipFace::of`] over its view and its glass, the fold the frame draws).
    pub(crate) fn chip_face_landed(&self, session: u64) -> bool {
        let glass = self.chip_glass(session);
        glass.task == Some(aterm_agent::harness::acts::TaskFace::Landed)
            && crate::claude_lights::ChipFace::of(self.session_autopilot(session).1, &glass)
                == crate::claude_lights::ChipFace::Landed
    }

    fn place_alike_rows(&self, rows: &mut [crate::status_item::SessionRow]) {
        let shared: Vec<usize> = (0..rows.len())
            .filter(|&i| {
                rows.iter()
                    .enumerate()
                    .any(|(j, other)| j != i && other.title == rows[i].title)
            })
            .collect();
        if shared.is_empty() {
            return;
        }
        for &i in &shared {
            rows[i].place = Some(self.session_place(&rows[i].sid));
        }
        let named: Vec<String> = shared.iter().map(|&i| rows[i].named()).collect();
        for (k, &i) in shared.iter().enumerate() {
            if named
                .iter()
                .enumerate()
                .any(|(l, other)| l != k && *other == named[k])
            {
                rows[i].place = Some(self.pane_place(&rows[i].sid).0);
            }
        }
    }

    /// Fold one session's PUBLISHED need into the herald ([`crate::status_item::Herald`], §9 S4: the herald is fed by the need): remember since when its need and hold stand (the glance's ages, headless too), dirty the menu-bar item where what it shows of the session moved, and post the one native notification a transition earns.
    pub(crate) fn herald_session(&mut self, session: u64) {
        // A fleet need's key (the herald's timer re-heralds an owed one):
        // the fleet's notices are re-read from the last fleet read.
        if crate::fleet_glance::is_herald_key(session) {
            self.herald_fleet();
            return;
        }
        let handle = self
            .store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .by_local(session)
            .filter(|h| !matches!(h.state, crate::session_store::SessionState::Exited))
            .cloned();
        let row = handle.as_ref().map(|h| self.status_session_row(h));
        let now_unix = status_unix_now();
        if let Some(row) = &row {
            self.adopt_status_carry(session, row, now_unix);
            self.adopt_end_carry(session, &row.sid);
        }
        if self
            .presence
            .herald
            .observe(session, row.as_ref(), now_unix)
        {
            self.mark_status_dirty();
        }
        if self.headless {
            return;
        }
        let current = row.as_ref().and_then(crate::status_item::escalation);
        let looking = self
            .notify_suppress
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains(&session);
        self.herald_deliver(session, current.as_ref(), looking);
    }

    fn adopt_status_carry(
        &mut self,
        session: u64,
        row: &crate::status_item::SessionRow,
        now_unix: i64,
    ) {
        if self.status_carry.is_empty() {
            return;
        }
        let Some(carry) = self.status_carry.remove(&row.sid) else {
            return;
        };
        if let (Some(done), Some(task)) = (carry.done, &row.task)
            && task.state == crate::task_record::TaskState::Done
            && task.since_unix_ms == done
        {
            self.mark_done_seen(session, task.entered);
        }
        self.presence
            .herald
            .adopt(session, carry, now_unix, std::time::Instant::now());
    }

    fn adopt_end_carry(&mut self, session: u64, sid: &str) {
        if self.end_carry.is_empty() {
            return;
        }
        let Some(carry) = self.end_carry.remove(sid) else {
            return;
        };
        if carry.worked_ms > 0 {
            self.harness_screen.ab.carry_work(session, carry.worked_ms);
        }
        let Some(end) = carry.end else {
            return;
        };
        let told = self.pool.get(session).and_then(|s| {
            s.ctx
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .task_face()
        });
        if told.is_some_and(|told| told != end.told()) {
            return;
        }
        let at = crate::harness_glass::Stamp::at(end.at_ms.div_euclid(1000));
        if !self
            .harness_glass
            .entry(session)
            .or_default()
            .carry_end(end.face, at, end.seen)
        {
            return;
        }
        let seen = self
            .frontmost_window
            .map(|front| self.see_front(front))
            .unwrap_or_default();
        if seen.task {
            self.mark_status_dirty();
        }
        if self.end_unseen(session) {
            self.restamp_for_end();
        }
    }

    /// Each session's Autopilot end and its task's work, as the rows a seamless update's park stamps on its manifest (`SessionRecord::autopilot_end`, `app_update_handoff::stamp_autopilot_end`; §11.2.3, item 8): the end standing on the glass.
    #[cfg(unix)]
    pub(crate) fn end_carry_rows(&self) -> Vec<(u64, String)> {
        let now = crate::turn_ledger::now_ms();
        self.pool
            .iter()
            .filter_map(|session| {
                let id = session.id;
                let end = self
                    .harness_glass
                    .get(&id)
                    .and_then(crate::harness_glass::SessionGlass::end)
                    .and_then(|end| {
                        let at_ms = session
                            .ctx
                            .timeline
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .task_face_at(end.face.task_face())?;
                        Some(crate::harness_end_carry::CarriedEnd {
                            face: end.face,
                            at_ms,
                            seen: !end.unseen(),
                        })
                    });
                crate::harness_end_carry::EndCarry {
                    end,
                    worked_ms: self.harness_screen.ab.worked(id, now),
                }
                .row()
                .map(|row| (id, row))
            })
            .collect()
    }

    /// What the status item knows of each session, as the rows a seamless update's park stamps on its manifest (`SessionRecord::status_seen`, `app_update_handoff::stamp_status_seen`).
    #[cfg(unix)]
    pub(crate) fn status_carry_rows(&self) -> Vec<(u64, String)> {
        self.pool
            .iter()
            .filter_map(|session| {
                let id = session.id;
                let mut carry = self.presence.herald.carry_of(id);
                carry.done = self.done_seen.get(&id).and_then(|entered| {
                    session
                        .ctx
                        .timeline
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .task()
                        .filter(|t| {
                            t.state == crate::task_record::TaskState::Done && t.entered == *entered
                        })
                        .map(|t| t.since_unix_ms)
                });
                carry.row().map(|row| (id, row))
            })
            .collect()
    }

    /// The herald's decision for `session` (a local id or a fleet need's key) at `current`, and what it posts.
    pub(crate) fn herald_deliver(
        &mut self,
        session: u64,
        current: Option<&crate::status_item::Escalation>,
        looking: bool,
    ) {
        let now = std::time::Instant::now();
        let alerts = self.config.desktop_alerts_or_default();
        let outcome = self
            .presence
            .herald
            .note(session, current, looking, alerts, now);
        let post = match outcome.notice.as_ref() {
            Some(_) if !alerts => crate::status_item::HeraldPost::Suppressed,
            Some(notice) => match self.post_herald_notice(notice) {
                Ok(()) => crate::status_item::HeraldPost::Queued,
                Err(why) => crate::status_item::HeraldPost::Refused(why),
            },
            None => crate::status_item::HeraldPost::Queued,
        };
        if matches!(post, crate::status_item::HeraldPost::Refused(_)) {
            self.presence.herald.refused(session, outcome.owed, now);
        }
        if let Some((level, line)) = crate::status_item::herald_log_line(
            session,
            current,
            &outcome,
            self.presence.herald.last_quiet(),
            post,
        ) {
            match level {
                aterm_log::Level::Warn => aterm_log::warn!("{line}"),
                aterm_log::Level::Info => aterm_log::info!("{line}"),
                _ => aterm_log::debug!("{line}"),
            }
        }
        if outcome.row_moved {
            self.mark_status_dirty();
        }
    }

    fn post_herald_notice(
        &self,
        notice: &crate::status_item::HeraldNotice,
    ) -> Result<(), &'static str> {
        let message = crate::notify::NotifyMsg::herald(notice);
        #[cfg(test)]
        {
            let _ = message;
            if let Some(why) = HERALD_REFUSAL.take() {
                return Err(why);
            }
            HERALD_POSTS.with(|posts| posts.borrow_mut().push(notice.clone()));
            Ok(())
        }
        #[cfg(not(test))]
        {
            self.session_factory
                .notify_tx
                .try_send(message)
                .map_err(|refused| match refused {
                    std::sync::mpsc::TrySendError::Full(_) => "the notifier's queue is full",
                    std::sync::mpsc::TrySendError::Disconnected(_) => "the notifier is gone",
                })
        }
    }

    /// THE STATUS GLANCE (§3): the REGISTRY snapshot → one [`crate::status_item::SessionRow`] per live session ([`Self::status_session_row`], leaf locks only) → the pure [`crate::status_item::Glance::build`], with the last sibling scan, the digest's entries and the alerts switch.
    pub(crate) fn status_glance(&self) -> crate::status_item::Glance {
        let started = std::time::Instant::now();
        let glance = self.status_glance_built();
        crate::metrics::note_status_glance(
            u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
        );
        glance
    }

    fn status_glance_built(&self) -> crate::status_item::Glance {
        let handles = self
            .store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .snapshot();
        let mut rows: Vec<crate::status_item::SessionRow> = handles
            .iter()
            .filter(|h| !matches!(h.state, crate::session_store::SessionState::Exited))
            .map(|h| self.status_session_row(h))
            .collect();
        self.place_alike_rows(&mut rows);
        let api_since = self.api_outage.values().min().copied();
        let fleet = self
            .fleet_glance
            .as_ref()
            .map(crate::fleet_glance::FleetGlance::copy);
        let mut glance = crate::status_item::Glance::build(
            &rows,
            &self.siblings,
            fleet.as_ref(),
            aterm_time::unix_millis().unwrap_or(0),
            api_since,
        );
        glance.exceptions = self
            .messages
            .digest()
            .entries
            .into_iter()
            .map(|entry| (entry.id.raw(), entry.title))
            .collect();
        glance.alerts = self.config.desktop_alerts_or_default();
        glance.voice = crate::notify::own_voice();
        glance
    }

    /// The menu-bar item owes a rebuild.
    pub(crate) const fn mark_status_dirty(&mut self) {
        self.status_dirty = true;
    }

    /// Whether the menu-bar item owes a rebuild ([`Self::mark_status_dirty`]), taking the debt.
    pub(crate) fn take_status_rebuild(&mut self) -> bool {
        std::mem::take(&mut self.status_dirty)
    }

    /// Recompute the glance and hand it to the menu-bar status item.
    pub(crate) fn refresh_status_item(&mut self) {
        self.note_done_seen_looking();
        if self._status_item.is_none() {
            return;
        }
        let glance = self.status_glance();
        let fp = glance.fingerprint();
        if self.status_fingerprint == Some(fp) {
            return;
        }
        self.status_fingerprint = Some(fp);
        let glance = self.status_drawn_next(glance);
        if let Some(handle) = &self._status_item {
            crate::status_item::update(handle, glance);
        }
    }

    fn status_drawn_next(
        &mut self,
        mut glance: crate::status_item::Glance,
    ) -> crate::status_item::Glance {
        self.status_gen = self.status_gen.wrapping_add(1);
        glance.generation = self.status_gen;
        self.status_drawn.push(glance.clone());
        while self.status_drawn.len() > 2 {
            self.status_drawn.remove(0);
        }
        glance
    }

    /// Install the menu-bar status item with the first glance (the first OS window's attach; never headless) and seed its sibling rows with a background scan so the first open has them.
    pub(crate) fn install_status_item(
        &mut self,
        proxy: &aterm_window::event_loop::EventLoopProxy<crate::Wake>,
    ) {
        let glance = self.status_glance();
        let fp = glance.fingerprint();
        let glance = self.status_drawn_next(glance);
        self._status_item = self.apprt.install_status_item(proxy, &glance);
        if self._status_item.is_some() {
            self.status_fingerprint = Some(fp);
            crate::sibling_scan::request_scan(proxy.clone());
            self.start_fleet_glance();
            #[cfg(target_os = "macos")]
            self.learn_notice_voice();
        }
    }

    /// The drawn glance a click's `pick` was painted from — the one the menu's last open was painted from ([`Self::note_status_painted`]), else one of the last two handed over — or `None`.
    #[cfg_attr(
        all(not(target_os = "macos"), not(test)),
        expect(dead_code, reason = "the menu-bar status item is macOS's")
    )]
    pub(crate) fn status_pick(
        &self,
        pick: crate::status_item::Pick,
    ) -> Option<&crate::status_item::Glance> {
        self.status_painted
            .iter()
            .chain(self.status_drawn.iter().rev())
            .find(|g| g.generation == pick.generation)
    }

    /// The status menu opened painted from `painted` (`None`: no paint ran, and the menu still shows the last one): keep it until the next open.
    #[cfg_attr(
        all(not(target_os = "macos"), not(test)),
        expect(dead_code, reason = "the menu-bar status item is macOS's")
    )]
    pub(crate) fn note_status_painted(&mut self, painted: Option<crate::status_item::Glance>) {
        if let Some(glance) = painted {
            self.status_painted = Some(glance);
        }
    }

    #[cfg(target_os = "macos")]
    fn learn_notice_voice(&self) {
        let Some(proxy) = self.proxy.clone() else {
            return;
        };
        crate::notify::learn_voice(self.notice_center, move || {
            let _ = proxy.send_event(crate::Wake::NoticeVoice);
        });
    }

    /// The owner went to session `session` from the menu bar or a notice.
    pub(crate) fn note_session_done_seen(&mut self, session: u64) {
        self.see_session_end(session);
        let done = self
            .pool
            .get(session)
            .and_then(|s| {
                s.ctx
                    .timeline
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .task()
                    .map(|t| (t.state, t.entered))
            })
            .filter(|(state, _)| *state == crate::task_record::TaskState::Done)
            .map(|(_, entered)| entered);
        if let Some(entered) = done {
            self.mark_done_seen(session, entered);
        }
    }

    /// FOCUSING THE TAB ANY OTHER WAY MARKS IT SEEN (§3), by the one rule the tab's `•` reads too ([`Self::note_done_seen`]).
    pub(crate) fn note_done_seen_looking(&mut self) {
        if let Some(front) = self.frontmost_window {
            self.note_done_seen(front);
        }
        let pool = &self.pool;
        self.done_seen
            .retain(|session, _| pool.get(*session).is_some());
    }

    /// The menu-bar item's mark as `aterm ctl chrome`'s `statusitem` line ([`Self::status_item_line_of`]) of a glance built now.
    #[cfg(test)]
    pub(crate) fn status_item_chrome_line(&self) -> String {
        self.status_item_line_of(&self.status_glance())
    }

    /// The menu-bar item as `aterm ctl chrome` prints it (§3).
    pub(crate) fn status_item_chrome_lines(&self) -> Vec<String> {
        self.status_item_chrome_lines_at(
            aterm_time::unix_secs().map_or(0, |s| i64::try_from(s).unwrap_or(0)),
        )
    }

    #[cfg_attr(
        all(not(target_os = "macos"), not(test)),
        expect(unused_variables, reason = "the menu-bar status item is macOS's")
    )]
    fn status_item_chrome_lines_at(&self, now_unix: i64) -> Vec<String> {
        let glance = self.status_glance();
        let mark = self.status_item_line_of(&glance);
        #[cfg(any(target_os = "macos", test))]
        {
            let fleet = self.fleet_glance.as_ref().map_or_else(Vec::new, |fleet| {
                crate::status_item::compose_fleet_menu(&fleet.copy(), now_unix)
            });
            std::iter::once(mark)
                .chain(crate::status_item::menu_lines(
                    &crate::status_item::compose_status_menu(&glance, now_unix),
                    &fleet,
                ))
                .collect()
        }
        #[cfg(not(any(target_os = "macos", test)))]
        {
            vec![mark]
        }
    }

    fn status_item_line_of(&self, glance: &crate::status_item::Glance) -> String {
        let fleet = self
            .fleet_glance
            .as_ref()
            .map_or_else(|| "off".to_string(), |f| f.copy().word());
        format!(
            "statusitem title={} needs={} stuck={} exceptions={} done={} fleet={fleet}",
            glance.button_title(),
            glance.need_count(),
            glance.stuck_count(),
            glance.exceptions.len(),
            glance.done.len(),
        )
    }

    #[cfg(any(target_os = "macos", test))]
    fn hand_back_all(&mut self) {
        let held: Vec<u64> = self.person_holds().iter().map(|h| h.session).collect();
        for session in held {
            if let Err(why) = self.hand_back_session(session) {
                aterm_log::info!("status item: Hand Back All: {session}: {why}");
            }
        }
    }

    #[cfg(target_os = "macos")]
    fn toggle_desktop_alerts(&mut self) {
        let desired = !self.config.desktop_alerts_or_default();
        self.config.desktop_alerts = Some(desired);
        self.apply_desktop_alerts(desired);
        if desired {
            let proxy = self.proxy.clone();
            let center = self.notice_center;
            center.ask(Box::new(move || {
                if let Some(proxy) = proxy {
                    crate::notify::learn_voice(center, move || {
                        let _ = proxy.send_event(crate::Wake::NoticeVoice);
                    });
                }
            }));
        }
        let (reply, answer) = std::sync::mpsc::channel();
        self.queue_control_settings_field(
            crate::prefs::EDIT_DESKTOP_ALERTS.to_string(),
            Some(desired.to_string()),
            reply,
        );
        let _ = std::thread::Builder::new()
            .name("aterm-status-notify-save".to_string())
            .spawn(move || {
                if let Ok(Err(why)) = answer.recv_timeout(std::time::Duration::from_secs(60)) {
                    aterm_log::warn!(
                        "status item: desktop_alerts = {desired} was not saved: {why}"
                    );
                }
            });
        self.mark_status_dirty();
    }

    /// Read Result: a finished worker's result as a Markdown document tab on the front window, raised ([`Self::raise_window`]), which marks its done seen, and the captured supervised agent's task end with it ([`Self::see_session_end`]).
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn read_status_result(&mut self, result: &crate::status_item::ResultRef) {
        let Some(wid) = self
            .frontmost_window
            .or_else(|| self.windows.keys().next().copied())
        else {
            return;
        };
        let opened = crate::native_document_host::path_to_file_uri(&result.path)
            .map_err(|e| e.to_string())
            .and_then(|uri| {
                self.request_document_tab_in_window(wid, crate::native_app::AppKind::Markdown, &uri)
            });
        if let Err(e) = opened {
            aterm_log::info!("status item: {}'s result did not open: {e}", result.label);
            return;
        }
        self.raise_window(wid);
        self.mark_done_seen(result.session, result.entered);
        if result.end_serial.is_some_and(|serial| {
            self.harness_glass
                .get(&result.session)
                .and_then(crate::harness_glass::SessionGlass::end)
                .is_some_and(|end| end.serial == serial)
        }) {
            self.see_session_end(result.session);
        }
    }

    fn see_session_end(&mut self, session: u64) {
        if self
            .harness_glass
            .get_mut(&session)
            .is_some_and(crate::harness_glass::SessionGlass::see_end)
        {
            self.restamp_for_end();
        }
    }

    /// The status menu is opening (`Wake::StatusMenuOpening`) painted from `painted`, which its clicks resolve against until the next open ([`Self::note_status_painted`]).
    #[cfg(target_os = "macos")]
    pub(crate) fn on_status_menu_opening(&mut self, painted: Option<crate::status_item::Glance>) {
        self.note_status_painted(painted);
        self.learn_notice_voice();
        if let Some(proxy) = self.proxy.clone() {
            crate::sibling_scan::request_scan(proxy);
        }
        if let Some(fleet) = &self.fleet_glance {
            fleet.kick(crate::fleet_glance::Kick::MenuOpening);
        }
    }

    /// A DEVELOPMENT BUILD'S STAND-IN FOR THE PERSON CLICKS THE MENU-BAR ROW AT `at` (`aterm ctl person menubar press <at>`, `docs/DESIGN-harness-autopilot-2026-10-15.md` §12.5.2): the menu composed as `chrome` composes it ([`Self::status_item_chrome_lines`]), from a glance stamped with the next generation and kept as the one the menu was painted from ([`Self::note_status_painted`], what a menu's open keeps).
    #[cfg(any(test, debug_assertions))]
    #[cfg_attr(
        not(target_os = "macos"),
        expect(unused_variables, reason = "the menu-bar status item is macOS's")
    )]
    pub(crate) fn person_menubar_press(
        &mut self,
        at: &crate::status_item::MenuAt,
        post: &mut dyn FnMut(crate::Wake) -> bool,
    ) -> Result<String, String> {
        #[cfg(not(target_os = "macos"))]
        {
            Err("no menu-bar item off macOS".to_string())
        }
        #[cfg(target_os = "macos")]
        {
            use crate::status_item::{StatusAction, StatusRow};
            let now_unix = aterm_time::unix_secs().map_or(0, |s| i64::try_from(s).unwrap_or(0));
            let mut glance = self.status_glance();
            self.status_gen = self.status_gen.wrapping_add(1);
            glance.generation = self.status_gen;
            let fleet = self.fleet_glance.as_ref().map_or_else(Vec::new, |fleet| {
                crate::status_item::compose_fleet_menu(&fleet.copy(), now_unix)
            });
            let rows = crate::status_item::compose_status_menu(&glance, now_unix);
            let (label, action) = match crate::status_item::row_at(&rows, &fleet, at) {
                None => return Err(format!("no row at {at}")),
                Some(StatusRow::Action {
                    label,
                    enabled: false,
                    ..
                }) => return Err(format!("disabled {label:?}")),
                Some(StatusRow::Action { label, action, .. }) => (label.clone(), *action),
                Some(
                    StatusRow::Info(_)
                    | StatusRow::Separator
                    | StatusRow::Submenu { .. }
                    | StatusRow::FleetSubmenu { .. },
                ) => return Err("not an action".to_string()),
            };
            let Some(action) = StatusAction::from_tag(action.tag()) else {
                return Err("not an action".to_string());
            };
            self.note_status_painted(Some(glance));
            if !post(crate::Wake::StatusAction { action }) {
                return Err("event loop gone".to_string());
            }
            Ok(format!("pressed {label:?}"))
        }
    }

    /// Dispatch one click from the menu-bar status item (the `dispatch_menu_action` twin for `Wake::StatusAction`).
    #[cfg(target_os = "macos")]
    pub(crate) fn dispatch_status_action(&mut self, action: crate::status_item::StatusAction) {
        use crate::status_item::StatusAction;
        match action {
            StatusAction::Overview => {
                let opened = self
                    .frontmost_window
                    .ok_or_else(|| "no front window to open the overview on".to_string())
                    .and_then(|wid| {
                        self.open_overview_raised(wid, crate::app_overview::OverviewAt::Top)
                    });
                if let Err(e) = opened {
                    aterm_log::info!("overview: {e}");
                }
            }
            StatusAction::ReadThisMac => {
                self.run_status_read(crate::app_take_over::CliPlan::new(
                    crate::app_take_over::CliVerb::ReadThisMac,
                ));
            }
            StatusAction::ToggleNotify => self.toggle_desktop_alerts(),
            StatusAction::HandBackAll => self.hand_back_all(),
            StatusAction::DismissExceptions => {
                let _ = self.dismiss_exceptions();
            }
            StatusAction::ShowException(id) => {
                if !self.show_status_exception(id) {
                    aterm_log::info!("status item: couldn't open message {id}");
                }
            }
            StatusAction::Go(pick) => {
                let session = self
                    .status_pick(pick)
                    .and_then(|g| g.go.get(usize::from(pick.index)).copied());
                if let Some(session) = session {
                    self.go_to_session(session);
                }
            }
            StatusAction::ReadResult(pick) => {
                let result = self
                    .status_pick(pick)
                    .and_then(|g| g.reads.get(usize::from(pick.index)).cloned());
                if let Some(result) = result {
                    self.read_status_result(&result);
                }
            }
            StatusAction::RaiseSibling(pick) => {
                let sibling = self
                    .status_pick(pick)
                    .and_then(|g| g.raises.get(usize::from(pick.index)).cloned());
                if let (Some(sibling), Some(proxy)) = (sibling, self.proxy.clone()) {
                    crate::sibling_scan::raise_sibling(
                        proxy,
                        sibling.pid,
                        sibling.sock,
                        sibling.sid,
                    );
                }
            }
            StatusAction::ReadFleet => {
                self.run_status_read(crate::app_take_over::CliPlan::new(
                    crate::app_take_over::CliVerb::ReadFleet,
                ));
            }
            StatusAction::ReadFleetNow => {
                if let Some(fleet) = &self.fleet_glance {
                    fleet.kick(crate::fleet_glance::Kick::Now);
                }
            }
            StatusAction::OpenLocalNetwork => {
                let _ = self.apprt.open_url(crate::fleet_glance::LOCAL_NETWORK_PANE);
            }
            StatusAction::PeerRead(pick)
            | StatusAction::PeerScreen(pick)
            | StatusAction::PeerSsh(pick)
            | StatusAction::PeerTakeOver(pick)
            | StatusAction::PeerHandBack(pick) => {
                let peer = self
                    .status_pick(pick)
                    .and_then(|g| g.peers.get(usize::from(pick.index)).cloned());
                if let Some(peer) = peer {
                    self.fleet_act(
                        action,
                        &peer.dial,
                        &peer.name,
                        peer.host.as_deref(),
                        Some(&peer.sid),
                    );
                }
            }
            StatusAction::MacRead(pick)
            | StatusAction::MacScreen(pick)
            | StatusAction::MacSsh(pick) => {
                let mac = self.fleet_glance.as_ref().and_then(|f| {
                    f.copy()
                        .snapshot_at(pick.generation)
                        .and_then(|s| s.macs.get(usize::from(pick.index)).cloned())
                });
                if let Some(mac) = mac {
                    self.fleet_act(action, &mac.dial, &mac.name, mac.host.as_deref(), None);
                }
            }
        }
    }

    /// A "To look at" line: Settings ▸ Messages at that entry, in the front window (else the first), and that window raised.
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn show_status_exception(&mut self, id: u64) -> bool {
        let Some(wid) = self.open_messages_entry_anywhere(id) else {
            return false;
        };
        self.raise_window(wid);
        true
    }

    /// TAKE ME THERE: the ❯ menu's Go, and a click on a notice about `session` (`Wake::NoticeClicked`: aterm's own macOS notice, a Windows balloon, a Linux notification through `Self::notice_clicked`).
    #[cfg_attr(
        all(
            not(target_os = "macos"),
            not(windows),
            not(target_os = "linux"),
            not(test)
        ),
        expect(
            dead_code,
            reason = "the menu-bar status item and a notice's click are macOS's, Windows' and \
                      Linux's"
        )
    )]
    pub(crate) fn go_to_session(&mut self, session: u64) -> bool {
        let shown = self.focus_session_window(session);
        self.note_session_done_seen(session);
        shown
    }

    /// Focus the tab displaying `session`, and the pane showing it there, and raise its window.
    pub(crate) fn focus_session_window(&mut self, session: u64) -> bool {
        self.focus_session_preferring(None, session)
    }

    /// [`Self::focus_session_window`], in window `prefer` first where it shows `session`.
    pub(crate) fn focus_session_preferring(
        &mut self,
        prefer: Option<WindowId>,
        session: u64,
    ) -> bool {
        let found = prefer
            .into_iter()
            .chain(self.windows.keys().copied())
            .find_map(|wid| {
                self.terminal_view_location(wid, session)
                    .map(|location| (wid, location.canonical_index, location.view))
            });
        let Some((wid, index, view)) = found else {
            return false;
        };
        self.switch_tab_in(wid, index);
        self.focus_session_pane(wid, session, view);
        self.raise_window(wid);
        true
    }

    fn focus_session_pane(&mut self, wid: WindowId, session: u64, view: crate::tab_model::ViewId) {
        let terminal_projection = self.active_tree(wid).is_some();
        let moved = if terminal_projection {
            self.active_tree_mut(wid)
                .is_some_and(|tree| tree.focus() != session && tree.set_focus(session))
        } else {
            self.windows
                .get_mut(&wid)
                .and_then(|window| window.tab_set.active_mut())
                .is_some_and(|tab| tab.focus != view && tab.set_focus(view))
        };
        if !moved {
            return;
        }
        if terminal_projection {
            let active = self.windows.get(&wid).map_or(0, |ws| ws.tabs.active);
            let synced = self.sync_tab_model_from_layout(wid, active);
            debug_assert!(synced);
        }
        self.resync_active_or_window(wid);
    }

    /// Bring window `wid` to the front of every app and Space.
    pub(crate) fn raise_window(&mut self, wid: WindowId) {
        #[cfg(target_os = "linux")]
        let activation = self.notice_activation.take();
        #[cfg(test)]
        RAISED_WINDOWS.with(|raised| raised.borrow_mut().push(wid));
        #[cfg(all(test, target_os = "linux"))]
        RAISED_ACTIVATIONS.with(|raised| raised.borrow_mut().push(activation.clone()));
        if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.clone()) {
            if w.is_minimized().unwrap_or(false) {
                self.pending_deminiaturize_focus = Some(wid);
            }
            w.set_minimized(false);
            #[cfg(target_os = "linux")]
            if let Some(token) = activation {
                use aterm_window::platform::wayland::WindowExtWayland as _;
                w.focus_window_with_activation_token(&token);
                return;
            }
            w.focus_window();
        }
    }

    /// A CLICK ON A LINUX NOTIFICATION about `session` (`Wake::NoticeClicked`, heard by `notify`'s bus thread).
    #[cfg(target_os = "linux")]
    pub(crate) fn notice_clicked(&mut self, session: u64, activation: Option<String>) {
        self.notice_activation = activation;
        if !self.go_to_session(session)
            && let Some(wid) = self.frontmost_window
        {
            self.raise_window(wid);
        }
        self.notice_activation = None;
    }

    /// `raise <sid>` (the `Wake::RaiseSession` body).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "window_routing",
            action = "RaiseSession",
            project = "aterm_gui::App::project_window_routing"
        )
    )]
    pub(crate) fn raise_session_by_id(&mut self, session: u64) -> Result<(), String> {
        if self.focus_session_window(session) {
            Ok(())
        } else {
            Err("no such session".to_string())
        }
    }

    /// SHOW (Settings ▸ Harness's session row, design §10.3.1).
    pub(crate) fn raise_session_pane(&mut self, session: u64) -> bool {
        self.focus_session_window(session)
    }

    /// Close the pane showing `session` (its tab only when it was the tab's last pane), reporting how far the close got.
    pub(crate) fn close_session_by_id(&mut self, session: u64) -> Result<CloseProgress, String> {
        let found = self.windows.keys().find_map(|wid| {
            self.terminal_view_location(*wid, session)
                .map(|location| (*wid, location))
        });
        let Some((wid, location)) = found else {
            return Err("no such session".to_string());
        };
        let split = location.terminal_only && self.tab_has_sibling_panes(wid, location.tab_id);
        let exits_window = location.terminal_only
            && !split
            && self
                .windows
                .get(&wid)
                .is_some_and(|ws| ws.tab_set.len() <= 1);
        if !exits_window && !self.pane_close_allowed(self.pane_close_hangs_up_a_job(session)) {
            return Err(WIRE_PANE_CLOSE_REFUSED.to_string());
        }
        let mut deferred = None;
        if !location.terminal_only {
            if !self.close_heterogeneous_terminal_view(wid, location) {
                return Err("session view changed during close".to_string());
            }
        } else if split {
            self.close_split_pane_by_id(wid, location, session)?;
        } else if self.close_tab_at(wid, location.canonical_index)
            && let Some(ws) = self.windows.get_mut(&wid)
        {
            ws.pending_close = true;
            deferred = Some(wid);
        }
        if !self.session_registered(session) {
            Ok(CloseProgress::Retired)
        } else if let Some(wid) = deferred {
            Ok(CloseProgress::Deferred(wid))
        } else if self.view_store.get(location.view).is_none() {
            Err(self.surviving_session_reason(wid, session))
        } else {
            Err(crate::app_window::WIRE_CLOSE_REFUSED.to_string())
        }
    }

    fn tab_has_sibling_panes(&self, wid: WindowId, tab_id: crate::tab_model::TabId) -> bool {
        self.windows
            .get(&wid)
            .and_then(|ws| ws.tab_set.get(tab_id))
            .is_some_and(|tab| tab.root.len() > 1)
    }

    fn close_split_pane_by_id(
        &mut self,
        wid: WindowId,
        location: TerminalViewLocation,
        session: u64,
    ) -> Result<(), String> {
        if self.defer_pending_update_handoff_teardown(crate::DeferredHandoffTeardown::mutation(
            crate::DeferredHandoffMutation::CloseView {
                window: wid,
                tab: location.tab_id,
                view: location.view,
            },
        )) {
            return Err(
                "close deferred (a pending update handoff is being cancelled first)".to_string(),
            );
        }
        let Some(tab) = self.windows.get(&wid).and_then(|ws| {
            terminal_projection_index(&ws.tab_set, &self.view_store, location.tab_id)
        }) else {
            return Err("session view changed during close".to_string());
        };
        let recovery = self.closed_view_record_for(wid, location.tab_id, location.view);
        let outcome = self.windows.get_mut(&wid).and_then(|ws| {
            let tree = ws.layouts.get_mut(tab)?;
            tree.contains(session).then(|| tree.close_pane(session))
        });
        let Some(outcome @ pane::CloseOutcome::Collapsed { .. }) = outcome else {
            return Err("session view changed during close".to_string());
        };
        let _ = self.apply_close_outcome(wid, tab, outcome);
        if self.view_store.get(location.view).is_some() {
            return Err("close refused (the pane is still open)".to_string());
        }
        if let Some(recovery) = recovery {
            self.retain_closed_view(recovery);
        }
        Ok(())
    }

    fn pane_close_hangs_up_a_job(&self, session: u64) -> bool {
        self.pool.views(session).is_some_and(|views| views <= 1)
            && self.session_foreground_busy(session)
    }

    fn tab_close_hangs_up_a_job(&self, wid: WindowId, i: usize) -> bool {
        let Some(tab) = self.windows.get(&wid).and_then(|ws| ws.tab_set.tab_at(i)) else {
            return false;
        };
        let sessions: Vec<u64> = tab
            .root
            .leaves()
            .into_iter()
            .filter_map(|view| {
                self.view_store
                    .get(view)
                    .copied()
                    .and_then(crate::tab_model::View::terminal_session)
            })
            .collect();
        sessions.iter().any(|&session| {
            let here = sessions.iter().filter(|&&s| s == session).count() as u32;
            self.pool.views(session).is_some_and(|views| views <= here)
                && self.session_foreground_busy(session)
        })
    }

    fn pane_close_allowed(&self, busy: bool) -> bool {
        if self.headless {
            return true;
        }
        !(busy && self.close_confirm == crate::app_window::CloseConfirm::WireRefuseBusy)
    }

    fn surviving_session_reason(&self, wid: WindowId, session: u64) -> String {
        if self
            .windows
            .keys()
            .any(|other| *other != wid && self.window_contains_session(*other, session))
        {
            "close deferred (another window still displays this session)".to_string()
        } else if self.window_contains_session(wid, session) {
            "close deferred (another tab still displays this session)".to_string()
        } else {
            "close deferred (the session outlived its pane)".to_string()
        }
    }

    /// The wire verdict of a close whose [`CloseProgress`] is known, read after the caller ran the deferred teardown (`escalate_pending_close`).
    pub(crate) fn close_session_verdict(
        &self,
        session: u64,
        progress: CloseProgress,
    ) -> Result<(), String> {
        match progress {
            CloseProgress::Retired => Ok(()),
            CloseProgress::Deferred(_) if !self.session_registered(session) => Ok(()),
            CloseProgress::Deferred(window) => Err(self.deferred_close_reason(window, session)),
        }
    }

    fn deferred_close_reason(&self, window: WindowId, session: u64) -> String {
        if self
            .windows
            .keys()
            .any(|wid| *wid != window && self.window_contains_session(*wid, session))
        {
            "close deferred (another window still displays this session)".to_string()
        } else if self.windows.contains_key(&window) {
            "close deferred (the window teardown has not run)".to_string()
        } else {
            "close deferred (the session outlived its window's teardown)".to_string()
        }
    }

    fn session_registered(&self, session: u64) -> bool {
        self.store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .by_local(session)
            .is_some()
    }

    /// Open a new tab in window `owner` (the tab-strip `+` of a non-frontmost window opens there).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "tab_strip",
            action = "NewTab",
            project = "aterm_gui::tab_strip_conformance::project"
        )
    )]
    pub(crate) fn open_tab_in(&mut self, owner: WindowId) {
        self.open_tab_in_cwd(owner, None);
    }

    /// [`Self::open_tab_in`] with an optional cwd override.
    pub(crate) fn open_tab_in_cwd(&mut self, owner: WindowId, cwd_override: Option<&str>) {
        self.open_tab_off_loop(owner, cwd_override, crate::tab_spawn::TabSpawnThen::Nothing);
    }

    /// Install an already-spawned `session` as a new last tab of window `owner` and switch to it, as a tab spawned off the loop lands ([`Self::install_tab_session`] under the focused pane's parent), for the headless tests that drive that same body with a stub (`push_stub_tab`).
    #[cfg(test)]
    pub(crate) fn install_new_tab(&mut self, owner: WindowId, session: crate::Session) {
        debug_assert_eq!(
            session.id, self.next_session_id,
            "a new tab's session id must be the minted session id",
        );
        self.next_session_id += 1;
        let parent = self.new_tab_parent(owner);
        self.install_tab_session(owner, session, parent);
    }

    /// The family-tree parent of a tab opened in `owner`.
    pub(crate) fn new_tab_parent(&self, owner: WindowId) -> Option<aterm_session::SessionId> {
        self.front_terminal(owner)
            .and_then(|terminal| self.pool.get(terminal.session))
            .map(|s| s.ctx.self_id.clone())
    }

    /// `Self::install_new_tab` for a session whose id is already reserved (a tab spawned off the event loop, [`crate::tab_spawn`]).
    pub(crate) fn install_tab_session(
        &mut self,
        owner: WindowId,
        session: crate::Session,
        parent: Option<aterm_session::SessionId>,
    ) {
        self.install_tab_session_at(owner, session, parent, None);
    }

    /// [`Self::install_tab_session`] for a tab that lands behind the one at index `kept` (a wire `spawn` that does not raise).
    pub(crate) fn install_tab_session_behind(
        &mut self,
        owner: WindowId,
        session: crate::Session,
        parent: Option<aterm_session::SessionId>,
        kept: usize,
    ) {
        self.install_tab_session_at(owner, session, parent, Some(kept));
    }

    fn install_tab_session_at(
        &mut self,
        owner: WindowId,
        session: crate::Session,
        parent: Option<aterm_session::SessionId>,
        behind: Option<usize>,
    ) {
        let id = session.id;
        Self::register_session(&self.store, &session, parent);
        let layout = pane::PaneTree::new(id);
        let tab = crate::register_terminal_tab(&mut self.tab_ids, &mut self.view_store, &layout)
            .expect("tab/view identity space");
        self.pool_insert(session);
        if let Some(ws) = self.windows.get_mut(&owner) {
            ws.layouts.push(layout);
            ws.tabs.add();
            ws.tab_set.push(tab).expect("fresh tab id");
            if let Some(front) = behind
                .and_then(|kept| ws.tab_set.tab_at(kept))
                .map(|t| t.id)
            {
                let switched = ws.tab_set.switch_to(front);
                debug_assert!(switched);
                if let Some(index) = terminal_projection_index(&ws.tab_set, &self.view_store, front)
                {
                    ws.tabs.switch_to(index);
                }
            }
        }
        self.resync_active_or_window(owner);
    }

    /// Cmd-1..Cmd-9: switch to tab index `i` (0-based) if it exists.
    pub(crate) fn switch_tab(&mut self, i: usize) {
        if let Some(front) = self.frontmost_window {
            self.switch_tab_in(front, i);
        }
    }

    /// Switch window `wid` to tab `i` (a tab-strip click targets the clicked window, which may not be the frontmost).
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

    /// Cmd-Shift-] / Cmd-Shift-[: cycle to the next/previous tab, wrapping.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "tab_strip",
            action = "SelectTab",
            project = "aterm_gui::tab_strip_conformance::project"
        )
    )]
    pub(crate) fn cycle_tab(&mut self, forward: bool) {
        let Some(wid) = self.frontmost_window else {
            return;
        };
        self.cycle_tab_in(wid, forward);
    }

    /// [`Self::cycle_tab`] on an explicit window.
    pub(crate) fn cycle_tab_in(&mut self, wid: WindowId, forward: bool) {
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
        if self.frontmost_window == Some(wid) {
            self.sync_active_session();
        } else {
            self.sync_window(wid);
        }
        self.palette_sync_native_scope(wid);
    }

    /// Apply a control-socket `tab` verb ([`TabAction`]) to the front window and return `(active_index, tab_count)` for the reply.
    pub(crate) fn apply_tab_cmd(&mut self, action: TabAction) -> Result<(usize, usize), String> {
        match self.frontmost_window {
            Some(front) => self.apply_tab_cmd_in(front, action),
            None => Err(NO_FOCUSED_WINDOW.to_string()),
        }
    }

    /// [`Self::apply_tab_cmd`] on an explicit window.
    pub(crate) fn apply_tab_cmd_in(
        &mut self,
        wid: WindowId,
        action: TabAction,
    ) -> Result<(usize, usize), String> {
        let Some(count) = self.windows.get(&wid).map(|ws| ws.tab_set.len()) else {
            return Ok((0, 0));
        };
        let no_such = |i: usize| crate::control::no_such_tab(i, count);
        self.pending_action_refusal = None;
        match action {
            TabAction::New => self.open_tab_in(wid),
            TabAction::Select(n) => {
                if n >= count {
                    return Err(no_such(n));
                }
                self.switch_tab_in(wid, n);
            }
            TabAction::Next => self.cycle_tab_in(wid, true),
            TabAction::Prev => self.cycle_tab_in(wid, false),
            TabAction::Close(which) => {
                if let Some(i) = which
                    && i >= count
                {
                    return Err(no_such(i));
                }
                self.close_tab_via_verb(wid, which);
            }
            TabAction::Move { from, to } => {
                if from >= count {
                    return Err(no_such(from));
                }
                if to >= count {
                    return Err(no_such(to));
                }
                self.move_tab(wid, from, to);
            }
        }
        if let Some(why) = self.pending_action_refusal.take() {
            return Err(why);
        }
        Ok(self.windows.get(&wid).map_or((0, 0), |ws| {
            (ws.tab_set.active_index().unwrap_or(0), ws.tab_set.len())
        }))
    }

    /// Close window `wid`'s tab `which` (or its active tab) for the `tab close [N]` verb.
    pub(crate) fn close_tab_via_verb(&mut self, wid: WindowId, which: Option<usize>) {
        let _closing = session_store::CloseAttribution::enter(
            session_store::ExitReason::CtlClose,
            session_store::ExitActor::Unknown,
        );
        let i = match which {
            Some(i) => i,
            None => self
                .windows
                .get(&wid)
                .and_then(|ws| ws.tab_set.active_index())
                .unwrap_or(0),
        };
        let exits_window = self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.tab_set.len() <= 1);
        if !exits_window && !self.pane_close_allowed(self.tab_close_hangs_up_a_job(wid, i)) {
            self.pending_action_refusal =
                Some(crate::app_window::WIRE_TAB_CLOSE_REFUSED.to_string());
            return;
        }
        if self.close_tab_at(wid, i)
            && let Some(ws) = self.windows.get_mut(&wid)
        {
            ws.pending_close = true;
        }
    }

    /// Reorder window `wid`'s canonical tab from index `from` to `to`, keeping the same stable tab selected.
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
        if self.frontmost_window == Some(wid) {
            self.sync_active_session();
        } else {
            self.sync_window(wid);
        }
    }

    /// Windows showing this session as some tab's focus.
    pub(crate) fn windows_with_focused_session(&self, session: u64) -> Vec<WindowId> {
        self.windows
            .iter()
            .filter(|(_, state)| {
                state.tab_set.tabs().iter().any(|tab| {
                    self.view_store
                        .get(tab.focus)
                        .copied()
                        .and_then(crate::tab_model::View::terminal_session)
                        == Some(session)
                })
            })
            .map(|(wid, _)| *wid)
            .collect()
    }

    /// Rebuild and push each window's tab chrome once, then repaint it.
    pub(crate) fn refresh_tab_chrome_windows(&mut self, windows: Vec<WindowId>) {
        for wid in windows {
            self.refresh_window_tabs(wid);
            if let Some(window) = self
                .windows
                .get(&wid)
                .and_then(|state| state.os_window.as_ref())
            {
                window.request_redraw();
            }
        }
    }

    /// Re-sync window `wid`'s native toolbar strip with [`Self::tab_titles`] and the active index (via [`crate::platform::AppRt::set_toolbar_tabs`]).
    pub(crate) fn refresh_window_tabs(
        &mut self,
        wid: WindowId,
    ) -> (Vec<String>, Vec<crate::tab_bar::TabStripMetadata>) {
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
        self.reap_stale_rename_edit(wid);
        let titles = self.tab_titles(wid);
        let mut chrome_titles = crate::chrome_label::capped_labels(&titles);
        let ext = self.tab_chrome_ext(wid, &chrome_titles);
        self.stamp_tab_connection_roles(wid);
        let metadata = self.tab_strip_metadata(wid);
        if let std::borrow::Cow::Owned(marked) =
            crate::tab_bar::team_titles(&chrome_titles, &metadata)
        {
            chrome_titles = std::borrow::Cow::Owned(marked);
        }
        let tooltips = self.tab_tooltips(wid);
        #[cfg(test)]
        crate::work_counts::tooltips_pushed(wid, &tooltips);
        let active = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.tab_set.active_index())
            .unwrap_or(0);
        let ids: Vec<crate::tab_model::TabId> =
            self.windows.get(&wid).map_or_else(Vec::new, |ws| {
                ws.tab_set.tabs().iter().map(|tab| tab.id).collect()
            });
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.strip_shadow.set((titles.len(), active));
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
            ws.tab_chrome_titles_by_tab
                .retain(|tab_id, _| ids.contains(tab_id));
            for ((tab_id, session), title) in ids
                .iter()
                .copied()
                .zip(label_sessions.iter().copied())
                .zip(chrome_titles.iter())
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
            let pill = self.presence_pill(wid);
            self.apprt.set_toolbar_tabs(
                handle,
                crate::platform::ToolbarTabsModel {
                    titles: &chrome_titles,
                    ids: &ids,
                    metadata: &metadata,
                    tooltips: &tooltips,
                    ext: &ext,
                    active,
                    pill: pill.as_ref(),
                },
            );
        }
        #[cfg(any(test, debug_assertions))]
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.tooltips_pushed = tooltips;
        }
        self.mark_status_dirty();
        (titles, metadata)
    }

    /// Repaint everything a session's user metadata feeds, after the change was stored and recorded.
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

    /// Cmd-W: close the focused pane of the frontmost window's active tab.
    pub(crate) fn close_active_tab(&mut self) -> Option<WindowId> {
        let _closing = session_store::CloseAttribution::enter(
            session_store::ExitReason::UiClose,
            session_store::ExitActor::Human,
        );
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
        if !exits_window {
            let busy = self
                .focused_session_id(window)
                .is_some_and(|session| self.pane_close_hangs_up_a_job(session));
            let replay = if self.tab_has_sibling_panes(window, stable_tab) {
                crate::close_confirm::CloseReplay::Pane {
                    wid: window,
                    tab: stable_tab,
                    view: stable_view,
                }
            } else {
                crate::close_confirm::CloseReplay::Tab {
                    wid: window,
                    tab: stable_tab,
                }
            };
            if !self.view_close_allowed(window, busy, replay) {
                return None;
            }
        }
        let recovery = self.closed_view_record_for_active(window);
        let outcome = self.active_tree_mut(window).map(|t| t.close_focused())?;
        let collapsed = matches!(outcome, pane::CloseOutcome::Collapsed { .. });
        let closes_window = self.apply_close_outcome(window, tab, outcome);
        if collapsed && let Some(recovery) = recovery {
            self.retain_closed_view(recovery);
        }
        if closes_window {
            self.stash_deferred_close_attribution(window);
        }
        closes_window.then_some(window)
    }

    /// Replay a pre-rollback focused-view close against stable identities.
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

    /// Resolve one deferred whole-tab close by stable identity so intervening tab selection/reordering cannot redirect it to a different tab.
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
            crate::native_app::CloseReadiness::Blocked { recovery } => {
                if visibility == ClosePreflightVisibility::Interactive {
                    self.surface_native_close_recovery(wid, tab, view, native, recovery)?;
                }
                Ok(false)
            }
        }
    }

    /// Preflight every native leaf in one window before an irreversible window teardown.
    pub(crate) fn prepare_window_native_shutdown(
        &mut self,
        wid: WindowId,
        scope: crate::native_app::CloseScope,
    ) -> Result<bool, String> {
        self.prepare_window_native_shutdown_with(wid, scope, ClosePreflightVisibility::Interactive)
    }

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
    pub(crate) fn prepare_all_native_shutdown(
        &mut self,
        scope: crate::native_app::CloseScope,
        visibility: ClosePreflightVisibility,
    ) -> Result<bool, String> {
        if matches!(
            scope,
            crate::native_app::CloseScope::Relaunch
                | crate::native_app::CloseScope::CarriedRelaunch
        ) && self.system_state_cancel_pending()
        {
            return Ok(false);
        }
        let windows = self.windows.keys().copied().collect::<Vec<_>>();
        for wid in windows {
            if !self.prepare_window_native_shutdown_with(wid, scope, visibility)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub(crate) fn prepare_quit_native_shutdown(&mut self) -> Result<bool, String> {
        self.prepare_all_native_shutdown(
            crate::native_app::CloseScope::AppQuit,
            ClosePreflightVisibility::Interactive,
        )
    }

    /// Close one focused leaf of a heterogeneous tab as a host transaction.
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
            .remove_view_link_for_close(view, crate::native_app::CloseScope::View)
            .ok_or_else(|| "focused view ownership could not detach".to_string())?;
        if let crate::tab_model::View::Terminal(terminal) = removed_link
            && self.detach_session_view(terminal.session)
        {
            self.retire_session_registration(terminal.session);
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

    /// Close one whole native or heterogeneous tab.
    pub(crate) fn close_active_native_tab(&mut self, wid: WindowId) -> Result<(), String> {
        self.close_active_native_tab_keeping(wid, None)
    }

    /// [`Self::close_active_native_tab`], then `keep` in front where it is given and still stands.
    pub(crate) fn close_active_native_tab_keeping(
        &mut self,
        wid: WindowId,
        keep: Option<crate::tab_model::TabId>,
    ) -> Result<(), String> {
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
            if let Some(keep) = keep.filter(|keep| *keep != tab_id) {
                ws.tab_set.switch_to(keep);
            }
            if was_last {
                ws.pending_close = true;
            }
            align_terminal_projection_to_active(ws, &self.view_store);
            (tab, was_last)
        };
        if was_last {
            self.stash_deferred_close_attribution(wid);
        }
        for view in tab.root.leaves() {
            self.remove_view_link_for_close(view, crate::native_app::CloseScope::Tab);
        }
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

    /// Reopen the most recently closed whole tab from its recursive identity-free descriptor.
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
        self.closed_recovery.tabs.has_candidate(now_ms) && !self.windows.is_empty()
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
        self.closed_recovery.views.has_candidate(now_ms) && !self.windows.is_empty()
    }

    /// Close the pane holding session `id` in window `window` (its reader hit EOF).
    pub(crate) fn close_session(&mut self, window: WindowId, id: u64) -> bool {
        // Distinct branch.
        if self.hold {
            return false;
        }
        if self.defer_pending_update_handoff_teardown(crate::DeferredHandoffTeardown::mutation(
            crate::DeferredHandoffMutation::ExitSession(id),
        )) {
            self.log_closes_deferred.insert(id);
            return false;
        }
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

    /// LOGICAL core of the `Wake::Exit` handler (no winit/`el`).
    pub(crate) fn exit_session_logical(&mut self, session: u64) -> Vec<WindowId> {
        if let Some(ended) = self.pool.get(session) {
            ended
                .ctx
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .slog_mut()
                .note_close_due();
        }
        self.store
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .set_state(session, session_store::SessionState::Exited);
        if let Some(ended) = self.pool.get(session) {
            ended.ctx.fabric.close();
        }
        self.note_shell_exit_for_ledger(session);
        if self.keep_shell_failed_at_start(session) {
            self.settle_log_close(session);
            return Vec::new();
        }
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
            // `close_session` leaves a last-tab occurrence intact for
            // `close_window_logical`; never report a window twice.
            if to_close.contains(&o) {
                continue;
            }
            if self.close_session(o, session) {
                to_close.push(o);
            }
        }
        if to_close.is_empty() {
            self.settle_log_close(session);
        }
        to_close
    }

    /// THE EXIT HANDLING OF `session` IS OVER ([`Self::exit_session_logical`] and, after it, the closes of the windows it returned).
    pub(crate) fn settle_log_close(&self, session: u64) {
        if self.log_closes_deferred.contains(&session) {
            return;
        }
        if let Some(ended) = self.pool.get(session) {
            let owed = ended
                .ctx
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .slog_mut()
                .take_close_due();
            if owed {
                ended.ctx.watchers.ring_watchers();
            }
        }
    }

    fn unstarted_shell<'s>(
        &self,
        pooled: &'s crate::Session,
    ) -> Option<&'s crate::spawn::ShellStart> {
        if self.hold {
            return None;
        }
        let start = pooled.shell_start.as_ref()?;
        let closed = pooled
            .pty_closed_at
            .get()
            .copied()
            .unwrap_or_else(std::time::Instant::now);
        if closed.saturating_duration_since(start.at) > SHELL_START_WINDOW
            || pooled.ctx.sink.driven_input() != start.input
            || term_lock(&pooled.term).all_blocks().next().is_some()
        {
            return None;
        }
        Some(start)
    }

    /// How `session`'s child ended, for every reader on the exit path.
    pub(crate) fn exit_status(&self, session: u64, look: ExitLook) -> Option<aterm_pty::ChildExit> {
        let pooled = self.pool.get(session)?;
        if pooled
            .child_reaped
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return pooled.child_exit.get().copied();
        }
        if look == ExitLook::Retry && self.unstarted_shell(pooled).is_some() {
            return None;
        }
        if let Some(exit) = pooled.reap_child() {
            return Some(exit);
        }
        #[cfg(not(unix))]
        if look == ExitLook::First && pooled.pid > 1 && self.unstarted_shell(pooled).is_some() {
            let deadline = std::time::Instant::now() + SHELL_START_STATUS_WAIT;
            while std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(1));
                if let Some(exit) = pooled.reap_child() {
                    return Some(exit);
                }
            }
        }
        None
    }

    /// The line a pane keeps when its interactive shell failed at start ([`Self::unstarted_shell`]) and ended non-zero or by a signal.
    pub(crate) fn shell_failed_at_start(&self, session: u64) -> Option<String> {
        let pooled = self.pool.get(session)?;
        let start = self.unstarted_shell(pooled)?;
        let how = match pooled.child_exit.get().copied()? {
            aterm_pty::ChildExit::Code(0) => return None,
            aterm_pty::ChildExit::Code(code) => format!("exit {code}"),
            aterm_pty::ChildExit::Signal(signal) => format!("signal {signal}"),
        };
        Some(format!("[{} ended at start: {how}]", start.name))
    }

    fn keep_shell_failed_at_start(&mut self, session: u64) -> bool {
        let Some(line) = self.shell_failed_at_start(session) else {
            return false;
        };
        if let Some(pooled) = self.pool.get(session) {
            let mut term = term_lock(&pooled.term);
            let lead = if term.cursor().col == 0 { "" } else { "\r\n" };
            term.process(format!("{lead}{line}\r\n").as_bytes());
        }
        let _ = self.admit_output_redraws(session, std::time::Instant::now());
        true
    }

    /// A click in window `wid`'s tab strip at column `col`.
    pub(crate) fn handle_tab_strip_click(&mut self, wid: WindowId, col: u16) {
        let Some(segs) = self.windows.get(&wid).map(|ws| ws.tab_segments.clone()) else {
            return;
        };
        let Some(hit) = tab_bar::hit_test(&segs, col) else {
            let now = std::time::Instant::now();
            let band_repeat = self.windows.get(&wid).is_some_and(|ws| {
                ws.strip_band_press
                    .is_some_and(|at| now.duration_since(at).as_millis() <= crate::multi_click_ms())
            });
            self.clear_strip_press(wid);
            self.settle_rename_edit(wid);
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.strip_band_press = (!band_repeat).then_some(now);
            }
            #[cfg(windows)]
            if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
                if band_repeat {
                    w.set_maximized(!w.is_maximized());
                } else {
                    let _ = w.drag_window();
                }
            }
            #[cfg(target_os = "linux")]
            if let Some(ws) = self.windows.get_mut(&wid)
                && let Some(frame) = ws.client_frame.filter(|frame| frame.header)
            {
                if band_repeat {
                    ws.caption_move = None;
                    if frame.maximize
                        && let Some(w) = &ws.os_window
                    {
                        w.set_maximized(!frame.maximized);
                    }
                } else if ws.pointer_position_known {
                    ws.caption_move = Some(ws.last_cursor_px);
                }
            }
            return;
        };
        match hit {
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
                                && now.duration_since(at).as_millis() <= crate::multi_click_ms()
                        })
                    })
                });
                let editing_this = self
                    .inline_rename_edit(wid)
                    .map(|edit| edit.tab)
                    .is_some_and(|edited| Some(edited) == tab);
                if let Some(ws) = self.windows.get_mut(&wid) {
                    ws.strip_press = tab.map(|tab| (now, tab));
                    ws.strip_band_press = None;
                    ws.strip_drag = match (repeat || editing_this, tab) {
                        (false, Some(tab)) => Some(crate::app_mouse::StripDrag {
                            tab,
                            origin_col: col,
                        }),
                        _ => None,
                    };
                }
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
            tab_bar::TabHit::Update => {
                self.clear_strip_press(wid);
                self.settle_rename_edit(wid);
                self.apply_update_or_details();
            }
            tab_bar::TabHit::Version { attention, .. } => {
                self.clear_strip_press(wid);
                self.settle_rename_edit(wid);
                self.post_menu_action(crate::menu::version_slot_click(attention));
            }
            tab_bar::TabHit::Pill(hit) => {
                self.clear_strip_press(wid);
                self.settle_rename_edit(wid);
                if let Some(session) = self.windows.get(&wid).and_then(|ws| ws.strip_pill_session) {
                    self.on_pill_act(wid, session, hit);
                }
            }
            tab_bar::TabHit::Close(i) => self.close_tab_from_strip(wid, i),
            tab_bar::TabHit::Caption(button) => {
                self.clear_strip_press(wid);
                self.settle_rename_edit(wid);
                self.press_caption_button(wid, button);
            }
        }
        if let Some(ws) = self.windows.get(&wid)
            && let Some(w) = &ws.os_window
        {
            w.request_redraw();
        }
    }

    fn press_caption_button(&mut self, wid: WindowId, button: tab_bar::CaptionButton) {
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        match button {
            tab_bar::CaptionButton::Close => ws.caption_close = true,
            tab_bar::CaptionButton::Minimize => {
                if let Some(w) = &ws.os_window {
                    w.set_minimized(true);
                }
            }
            tab_bar::CaptionButton::Maximize | tab_bar::CaptionButton::Restore => {
                if let Some(w) = &ws.os_window {
                    w.set_maximized(button == tab_bar::CaptionButton::Maximize);
                }
            }
        }
    }

    /// A MIDDLE press on the in-grid tab strip at `col`.
    pub(crate) fn handle_tab_strip_middle_press(&mut self, wid: WindowId, col: u16) {
        let Some(segs) = self.windows.get(&wid).map(|ws| ws.tab_segments.clone()) else {
            return;
        };
        let Some(tab_bar::TabHit::Select(i) | tab_bar::TabHit::Close(i)) =
            tab_bar::hit_test(&segs, col)
        else {
            return;
        };
        self.close_tab_from_strip(wid, i);
        if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
            w.request_redraw();
        }
    }

    fn close_tab_from_strip(&mut self, wid: WindowId, i: usize) {
        self.clear_strip_press(wid);
        self.settle_rename_edit(wid);
        if self.close_tab_at(wid, i)
            && let Some(ws) = self.windows.get_mut(&wid)
        {
            ws.pending_close = true;
        }
    }

    /// Break the strip's double-click streaks — the chip one AND the bare-band one.
    pub(crate) fn clear_strip_press(&mut self, wid: WindowId) {
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.strip_press = None;
            ws.strip_band_press = None;
            ws.strip_drag = None;
        }
    }

    /// The composed context-menu model for tab `index` of `wid`, or empty when the tab has none (a native app tab, an out-of-range index).
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
        let label = crate::chrome_label::capped_label(titles.get(index).map_or("", String::as_str));
        crate::session_chrome::menu_shown_at(
            &self
                .composed_session_chrome(wid, session, &label, &mut None)
                .menu,
            crate::turn_ledger::now_ms(),
        )
    }

    /// C5 — pop the tab context menu over tab `index` of `wid`, anchored at strip column `anchor_col`.
    pub(crate) fn open_tab_context_menu(
        &mut self,
        wid: WindowId,
        index: usize,
        anchor_col: u16,
        keyboard: bool,
    ) -> bool {
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
        self.clear_strip_press(wid);
        self.settle_rename_edit(wid);
        if self
            .windows
            .get(&wid)
            .and_then(|ws| ws.tab_set.active_index())
            != Some(index)
        {
            self.switch_tab_in(wid, index);
        }
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
            ws.scroll_residual = 0.0;
            ws.scroll_residual_x = 0.0;
            ws.tab_menu_rect = None;
            if let Some(w) = &ws.os_window {
                w.request_redraw();
            }
        }
        true
    }

    /// The keyboard entry point (the Menu key; Shift+F10 too under `tab_menu_chord = "on"`).
    pub(crate) fn open_active_tab_context_menu(&mut self, wid: WindowId) -> bool {
        let Some(index) = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.tab_set.active_index())
        else {
            return false;
        };
        let anchor = self.tab_chip_anchor_col(wid, index);
        self.open_tab_context_menu(wid, index, anchor, true)
    }

    fn tab_chip_anchor_col(&self, wid: WindowId, index: usize) -> u16 {
        self.windows
            .get(&wid)
            .and_then(|ws| {
                ws.tab_segments
                    .iter()
                    .find(|seg| seg.kind == tab_bar::TabHit::Select(index))
                    .map(|seg| seg.start_col)
            })
            .unwrap_or(0)
    }

    /// C5 — open the context menu for chip `index` with no pointer column of its own (the tests' right press).
    #[cfg(test)]
    pub(crate) fn open_tab_context_menu_at_chip(&mut self, wid: WindowId, index: usize) -> bool {
        let anchor = self.tab_chip_anchor_col(wid, index);
        self.open_tab_context_menu(wid, index, anchor, false)
    }

    /// Dismiss any open tab context menu on `wid`.
    pub(crate) fn close_tab_menu(&mut self, wid: WindowId) -> bool {
        let Some(ws) = self.windows.get_mut(&wid) else {
            return false;
        };
        if ws.tab_menu.take().is_none() {
            ws.tab_menu_rect = None;
            return false;
        }
        ws.scroll_residual = 0.0;
        ws.scroll_residual_x = 0.0;
        ws.tab_menu_rect = None;
        if let Some(w) = &ws.os_window {
            w.request_redraw();
        }
        true
    }

    /// Activate entry `i` of the open menu on `wid` and dismiss the card.
    pub(crate) fn activate_tab_menu_entry(&mut self, wid: WindowId, i: usize) {
        let upgrade = self.windows.get(&wid).and_then(|ws| {
            let menu = ws.tab_menu.as_ref()?;
            match menu.entries.get(i)? {
                crate::session_chrome::TabMenuEntry::Upgrade { sid, to, word, .. } => {
                    Some((menu.tab, sid.clone(), to.clone(), *word))
                }
                _ => None,
            }
        });
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
        let session_row = self.windows.get(&wid).and_then(|ws| {
            let menu = ws.tab_menu.as_ref()?;
            match menu.entries.get(i)? {
                crate::session_chrome::TabMenuEntry::ConnectionAction {
                    peer_sid,
                    verb,
                    enabled: true,
                    ..
                } => Some((menu.tab, peer_sid.clone(), *verb)),
                _ => None,
            }
        });
        self.close_tab_menu(wid);
        if let Some((tab, sid, to, word)) = upgrade {
            self.dispatch_tab_menu_upgrade(wid, tab, sid, to, word);
        }
        if let Some((tab, peer_sid, verb)) = session_row {
            self.dispatch_tab_menu_connection(wid, tab, &peer_sid, verb);
            return;
        }
        if let Some((tab, action)) = chosen
            && matches!(
                action,
                crate::menu::MenuAction::TakeOver | crate::menu::MenuAction::HandBack
            )
        {
            let session = self
                .tab_index_for_id(wid, tab)
                .and_then(|index| self.tab_terminal_session(wid, index));
            if let Some(session) = session {
                let done = if action == crate::menu::MenuAction::TakeOver {
                    self.take_over_session(session)
                } else {
                    self.hand_back_session(session)
                };
                if let Err(why) = done {
                    aterm_log::info!("tab menu {action:?}: {why}");
                }
            }
            return;
        }
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

    /// Cancel a live edit whose tab has gone away.
    pub(crate) fn reap_stale_rename_edit(&mut self, wid: WindowId) {
        let Some(tab) = self.inline_rename_edit(wid).map(|edit| edit.tab) else {
            return;
        };
        if self.tab_index_for_id(wid, tab).is_none() {
            self.end_session_rename_editor(wid);
        }
    }

    /// Stash the current close gesture's attribution on window `wid` when a last-tab close is deferred.
    pub(crate) fn stash_deferred_close_attribution(&mut self, wid: WindowId) {
        let attribution = session_store::current_close_attribution();
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.pending_close_attribution = Some(attribution);
        }
    }

    /// Close the entire tab at index `i` of window `wid` (every pane in it), as the strip's ✕ does, unlike Cmd-W.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "tab_strip",
            action = "Close",
            project = "aterm_gui::tab_strip_conformance::project"
        )
    )]
    pub(crate) fn close_tab_at(&mut self, wid: WindowId, i: usize) -> bool {
        let _closing = session_store::CloseAttribution::enter(
            session_store::ExitReason::UiClose,
            session_store::ExitActor::Human,
        );
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
            self.pending_action_refusal = Some(
                "tab close deferred: a pending update handoff is being cancelled first".to_string(),
            );
            return false;
        }
        if is_native {
            let mut front = None;
            if let Some(ws) = self.windows.get_mut(&wid) {
                front = ws.tab_set.active_id().filter(|front| *front != tab_id);
                ws.tab_set.switch_to(tab_id);
            }
            return match self.close_active_native_tab_keeping(wid, front) {
                Ok(()) => canonical_count == 1,
                Err(why) => {
                    self.resync_active_or_window(wid);
                    self.pending_action_refusal = Some(why);
                    false
                }
            };
        }
        let Some(projection_index) = terminal_index else {
            return false;
        };
        let exits_window = canonical_count == 1;
        if !self.window_exit_close_allowed(wid, exits_window) {
            return false;
        }
        if !exits_window {
            let busy = self.tab_close_hangs_up_a_job(wid, i);
            let replay = crate::close_confirm::CloseReplay::Tab { wid, tab: tab_id };
            if !self.view_close_allowed(wid, busy, replay) {
                return false;
            }
        }
        let recovery = self
            .windows
            .get(&wid)
            .and_then(|window| window.tab_set.get(tab_id))
            .and_then(|tab| self.tab_restore_descriptor(tab));
        if exits_window {
            if let Some(tab) = recovery {
                self.retain_closed_tab(wid, i, tab);
            }
            self.stash_deferred_close_attribution(wid);
            return true;
        }
        let Some(ws) = self.windows.get_mut(&wid) else {
            return false;
        };
        let closing = ws.layouts[projection_index].sessions();
        ws.layouts.remove(projection_index);
        remove_terminal_projection(ws, projection_index);
        let stable_tab = ws.tab_set.remove(tab_id).expect("stable tab mirror exists");
        align_terminal_projection_to_active(ws, &self.view_store);
        self.remove_tab_views(&stable_tab);
        for sid in closing {
            if self.detach_session_view(sid) {
                self.retire_session_registration(sid);
            }
        }
        if let Some(tab) = recovery {
            self.retain_closed_tab(wid, i, tab);
        }
        if self.frontmost_window == Some(wid) {
            self.sync_active_session();
        } else {
            self.sync_window(wid);
        }
        false
    }

    /// Live structural oracle for the window/session model, `debug_assert`-ed after each tab mutation.
    pub(crate) fn structural_invariants_ok(&self) -> bool {
        self.structural_invariant_violation().is_none()
    }

    /// The oracle of [`Self::structural_invariants_ok`], naming the first false clause and the values that make it false.
    pub(crate) fn structural_invariant_violation(&self) -> Option<String> {
        let Some(fid) = self.frontmost_window else {
            return Some("frontmost_window is None: no window holds the front".to_string());
        };
        if !self.windows.contains_key(&fid) {
            return Some(format!("frontmost_window {fid:?} names no live window"));
        }
        self.windows.iter().find_map(|(wid, ws)| {
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
            };
            let projection_shape = ws.tabs.count == ws.layouts.len()
                && if ws.layouts.is_empty() {
                    ws.tabs.active == 0
                } else {
                    ws.tabs.active < ws.layouts.len()
                };

            if ws.tab_set.is_empty() && !ws.pending_close {
                return Some(format!("{wid:?}: window has no tabs and is not closing"));
            }
            if !projection_shape {
                return Some(format!(
                    "{wid:?}: tabs mirror {{count: {}, active: {}}} is not a shape for {} layouts",
                    ws.tabs.count,
                    ws.tabs.active,
                    ws.layouts.len(),
                ));
            }
            if terminal_tabs.len() != ws.layouts.len() {
                return Some(format!(
                    "{wid:?}: {} all-terminal tabs project onto {} layouts",
                    terminal_tabs.len(),
                    ws.layouts.len(),
                ));
            }
            if !ws.tab_set.invariant_holds(&self.view_store) {
                return Some(format!(
                    "{wid:?}: tab_set is self-inconsistent — the active id is absent or \
                     duplicated, or some tab's focus is not a leaf it owns, or a leaf names a \
                     view the store no longer holds",
                ));
            }
            if !front_truth {
                return Some(format!(
                    "{wid:?}: front_content {:?} does not mirror the active tab's focused view \
                     {:?} ({:?}); active_terminal names {:?}",
                    ws.front_content,
                    active_tab.map(|tab| tab.focus),
                    active_tab.and_then(|tab| self.view_store.get(tab.focus).copied()),
                    ws.active_terminal.as_ref().map(|mirror| mirror.session),
                ));
            }
            if !focus_truth {
                return Some(format!(
                    "{wid:?}: window_focus {:?} disagrees with front_content {:?}",
                    ws.window_focus, ws.front_content,
                ));
            }
            if let Some(index) = active_terminal {
                if index != ws.tabs.active {
                    return Some(format!(
                        "{wid:?}: the active tab is terminal projection {index}, but the tabs \
                         mirror says active={}",
                        ws.tabs.active,
                    ));
                }
                if !ws
                    .front_terminal()
                    .is_some_and(|mirror| mirror.session == ws.layouts[index].focus())
                {
                    return Some(format!(
                        "{wid:?}: front_terminal {:?} does not name layouts[{index}]'s focused \
                         session {:?}",
                        ws.front_terminal().map(|mirror| mirror.session),
                        ws.layouts[index].focus(),
                    ));
                }
            }
            for (index, (layout, tab)) in ws.layouts.iter().zip(&terminal_tabs).enumerate() {
                let projected = tab.root.map(&mut |view| {
                    self.view_store
                        .get(*view)
                        .copied()
                        .and_then(crate::tab_model::View::terminal_session)
                });
                if projected != layout.map_sessions(Some) {
                    return Some(format!(
                        "{wid:?}: tab {:?}'s pane tree does not project onto layouts[{index}] — \
                         tab sessions {:?} vs layout sessions {:?}",
                        tab.id,
                        projected.leaves(),
                        layout.map_sessions(Some).leaves(),
                    ));
                }
                if self
                    .view_store
                    .get(tab.focus)
                    .copied()
                    .and_then(crate::tab_model::View::terminal_session)
                    != Some(layout.focus())
                {
                    return Some(format!(
                        "{wid:?}: tab {:?}'s focused leaf {:?} names session {:?}, but \
                         layouts[{index}] is focused on session {:?}",
                        tab.id,
                        tab.focus,
                        self.view_store
                            .get(tab.focus)
                            .copied()
                            .and_then(crate::tab_model::View::terminal_session),
                        layout.focus(),
                    ));
                }
                if tab.zoomed != layout.is_zoomed() {
                    return Some(format!(
                        "{wid:?}: tab {:?} is zoomed={} but layouts[{index}] is zoomed={}",
                        tab.id,
                        tab.zoomed,
                        layout.is_zoomed(),
                    ));
                }
            }
            for tab in ws.tab_set.tabs() {
                for view in tab.root.leaves() {
                    let owned = match self.view_store.get(view).copied() {
                        Some(crate::tab_model::View::Terminal(terminal)) => {
                            self.pool.get(terminal.session).is_some()
                        }
                        Some(crate::tab_model::View::Native(native)) => {
                            self.native_runtime.app(native.instance).is_some()
                                && self.native_runtime.view_state(view).is_some()
                        }
                        None => false,
                    };
                    if !owned {
                        return Some(format!(
                            "{wid:?}: tab {:?}'s leaf {view:?} ({:?}) is not owned by the session \
                             pool / native runtime",
                            tab.id,
                            self.view_store.get(view).copied(),
                        ));
                    }
                }
            }
            None
        })
    }

    /// Apply a [`pane::CloseOutcome`] from tab `tab` of window `wid`, keeping the pool, `layouts` and `tabs` consistent, and re-mirror the focused pane.
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
                let synced = self.sync_tab_model_from_layout(wid, tab);
                debug_assert!(synced);
                let orphaned = closed_view.is_some_and(|view| {
                    self.windows
                        .get(&wid)
                        .is_some_and(|ws| ws.tab_set.tabs().iter().any(|t| t.root.contains(view)))
                });
                if orphaned {
                    aterm_log::info!(
                        "pane close refused: session {closed_session}'s leaf survived the \
                         re-projection, so its view stays live — retiring it would leave an \
                         unresolvable leaf and stop the window presenting"
                    );
                    self.resize_panes(wid);
                    self.resync_active_or_window(wid);
                    return false;
                }
                if let Some(view) = closed_view {
                    self.view_store.remove(view);
                }
                self.teardown_session(outcome.closed());
                self.split_regrid_cause = crate::split_regrid_hold::RelayCause::Collapse;
                self.resize_panes(wid);
                self.split_regrid_cause = crate::split_regrid_hold::RelayCause::Other;
                self.resync_active_or_window(wid);
                self.reset_pane_space_decorations(wid);
                false
            }
            pane::CloseOutcome::LastPane { .. } => {
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
                self.resync_active_or_window(wid);
                false
            }
        }
    }

    /// Tear down session `id`: detach its pool view first (the last view drops its `Session`, closing its PTY master and ending its reader), then deregister only when that detach dropped the session.
    pub(crate) fn teardown_session(&mut self, id: u64) {
        self.split_regrid_holds.forget(id);
        if self.detach_session_view(id) {
            self.retire_session_registration(id);
        }
    }

    /// Retire everything keyed by a DROPPED session's stable sid.
    pub(crate) fn retire_session_registration(&mut self, id: u64) {
        self.retire_session_registration_keeping(id, crate::session_log::ClosedKeep::Skeleton);
    }

    /// [`Self::retire_session_registration`], its closed-session index entry as `keep` says.
    pub(crate) fn retire_session_registration_keeping(
        &mut self,
        id: u64,
        keep: crate::session_log::ClosedKeep,
    ) {
        #[cfg(target_os = "macos")]
        self.withdraw_askpass_session(id);
        let (reason, by) = session_store::current_close_attribution();
        let (closing, instance) = {
            let store = self.store.read().unwrap_or_else(|p| p.into_inner());
            let ctx = store.by_local(id).map(|h| std::sync::Arc::clone(&h.ctx));
            (ctx, store.instance_watchers())
        };
        let tasked = closing.as_ref().is_some_and(|ctx| {
            ctx.timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .task()
                .is_some()
        });
        let held = (
            closing.as_ref().map(|ctx| ctx.watchers.hold_rings()),
            instance.hold_rings(),
        );
        let deregistered = self
            .store
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .deregister_closing(id, reason, by);
        let (stable, exit_seq) = match deregistered {
            Some(closed) => {
                drop(closed.freed);
                (Some(closed.sid), closed.exit_seq)
            }
            None => (None, None),
        };
        let reads = closing.as_ref().map(|ctx| {
            let mut edges = ctx.edges.lock().unwrap_or_else(|p| p.into_inner());
            edges.keep_only(aterm_session::Op::ReadScreen);
            (std::sync::Arc::clone(&ctx.edges), edges.len())
        });
        #[cfg(test)]
        if let Some(between) = CLOSE_BETWEEN_WRITES.with(|b| b.borrow_mut().take()) {
            between(self);
        }
        if let (Some(stable), Some(seq), Some(ctx), Some((reads, read_rows))) =
            (stable.as_ref(), exit_seq, closing, reads)
        {
            let log = {
                let tl = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
                crate::session_log::ClosedLog::kept_from(
                    tl.slog().log(),
                    stable.clone(),
                    reads,
                    read_rows,
                    ctx.nonce,
                    keep,
                )
            };
            let freed = self
                .store
                .write()
                .unwrap_or_else(|p| p.into_inner())
                .keep_closed_log(seq, log);
            drop(freed);
        }
        drop(held);
        self.forget_harness_glass(id);
        self.autopilot_timers.forget(id);
        self.claude_facts.remove(&id);
        self.autopilot_recorded.remove(&id);
        self.spare_poke();
        if let Some(stable) = stable {
            self.resume_stories.remove(stable.as_str());
            if tasked {
                crate::task_launch::retire_task_file(&stable);
            }
            crate::proxy::unpublish_session(&stable);
            crate::connections::sweep_session_closed_in(&self.connections, &stable, &self.store);
            self.overview_refresh_all();
        }
    }
}

#[cfg(test)]
mod system_state_runtime_tests {
    use super::*;

    fn edit_review(app: &mut App, wid: WindowId, view: crate::tab_model::ViewId) {
        use crate::native_app::{ActionInvocation, AppEvent, SemanticInput};
        for (key, value) in [
            ("system-state/setting/0", SemanticInput::Bool(false)),
            (
                "system-state/url",
                SemanticInput::Text("http://draft.local/v1/team".into()),
            ),
            (
                "system-state/password",
                SemanticInput::Text("inert-password".into()),
            ),
        ] {
            app.dispatch_native_view_event(
                wid,
                view,
                AppEvent::Action(ActionInvocation {
                    id: crate::native_ui::ActionId::new(key),
                    value: Some(value),
                }),
            )
            .unwrap();
        }
    }

    fn assert_review(app: &mut App, wid: WindowId) {
        use crate::native_ui::{SemanticValue, UiKey};
        assert!(app.open_system_state_tab_in_window(wid));
        let (instance, view) = app.active_native_view(wid).unwrap();
        assert!(app.commit_glassless_native_frame(wid));
        let ui = &app.windows[&wid].leaf_render_cache[&view]
            .native
            .as_ref()
            .unwrap()
            .compiled;
        for (key, value) in [
            ("system-state/setting/0", SemanticValue::Bool(false)),
            (
                "system-state/url",
                SemanticValue::Text("http://draft.local/v1/team".into()),
            ),
            ("system-state/password", SemanticValue::Text("•".repeat(14))),
        ] {
            assert_eq!(ui.semantic(&UiKey::new(key)).unwrap().value, value);
        }
        assert!(
            app.native_runtime
                .presentation(instance, view)
                .unwrap()
                .indicators
                .dirty
        );
    }

    #[test]
    fn system_state_failed_window_close_preflight_keeps_the_shared_review() {
        use crate::native_app::{ActionInvocation, AppEvent, CloseScope, TextInputEvent};
        use crate::workstation_setup::Request;
        use aterm_types::macos_setup::Plan;
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let window = app.windows.get_mut(&wid).unwrap();
        window.rows = 90;
        window.cols = 160;
        assert!(app.open_system_state_tab());
        let (_, review) = app.active_native_view(wid).unwrap();
        let (send, receive) = std::sync::mpsc::channel();
        let request = Request::new(Plan::default(), send);
        assert!(app.native_runtime.offer_system_state(request.clone()));
        edit_review(&mut app, wid, review);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));
        let (_, settings) = app.active_native_view(wid).unwrap();
        for event in [
            AppEvent::Action(ActionInvocation {
                id: crate::native_ui::ActionId::new(format!(
                    "settings/set/{}",
                    crate::prefs::EDIT_FONT_FAMILY
                )),
                value: None,
            }),
            AppEvent::TextInput(TextInputEvent::SelectAll),
            AppEvent::TextInput(TextInputEvent::Commit("Inert Draft Mono".into())),
        ] {
            app.dispatch_native_view_event(wid, settings, event)
                .unwrap();
        }
        assert!(
            !app.prepare_window_native_shutdown(wid, CloseScope::Window)
                .unwrap()
        );
        assert!(!app.prepare_quit_native_shutdown().unwrap());
        assert!(request.pending());
        assert!(receive.try_recv().is_err());
        assert_review(&mut app, wid);
        assert!(app.structural_invariants_ok());
    }

    #[test]
    fn system_state_second_window_close_preserves_review_until_its_final_view_closes() {
        use crate::native_app::CloseScope;
        use crate::workstation_setup::{Request, WorkstationSetupDecision};
        use aterm_types::macos_setup::Plan;
        for window_close in [false, true] {
            let mut app = App::headless_for_test();
            let first_window = WindowId(0);
            let window = app.windows.get_mut(&first_window).unwrap();
            window.rows = 90;
            window.cols = 160;
            assert!(app.open_system_state_tab());
            let (instance, first) = app.active_native_view(first_window).unwrap();
            let (send, receive) = std::sync::mpsc::channel();
            let request = Request::new(Plan::default(), send);
            assert!(app.native_runtime.offer_system_state(request.clone()));
            edit_review(&mut app, first_window, first);
            app.stage_system_state_tab(first_window).unwrap();
            let second_window = app.detach_active_tab_logical().unwrap();
            assert_eq!(app.active_native_view(second_window).unwrap().0, instance);
            assert!(
                app.prepare_window_native_shutdown(second_window, CloseScope::Window)
                    .unwrap()
            );
            if !window_close {
                app.close_active_native_tab(second_window).unwrap();
            }
            app.close_window_logical(second_window);
            assert!(request.pending());
            assert!(receive.try_recv().is_err());
            assert_review(&mut app, first_window);
            assert!(app.structural_invariants_ok());
            let expected = if window_close {
                assert!(
                    app.prepare_window_native_shutdown(first_window, CloseScope::Window)
                        .unwrap()
                );
                app.close_window_logical(first_window);
                WorkstationSetupDecision::Unavailable
            } else {
                app.close_active_native_tab(first_window).unwrap();
                WorkstationSetupDecision::Cancel
            };
            assert_eq!(receive.try_recv().unwrap().decision, expected);
            assert!(!request.pending());
            assert!(app.native_runtime.app(instance).is_none());
        }
    }

    #[test]
    fn system_state_opens_from_control_reuses_singleton_and_reopens_without_pty_churn() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert_eq!(
            app.open_app(crate::app_control::parse_open("app system-state").unwrap())
                .unwrap(),
            "app system-state"
        );
        let original = app.active_native_view(wid).unwrap();
        let tab_count = app.windows[&wid].tab_set.len();
        assert_eq!(
            app.native_runtime
                .app(original.0)
                .unwrap()
                .vtable()
                .restore_tag,
            "system-state"
        );
        assert!(matches!(
            app.native_runtime.view_state(original.1),
            Some(crate::native_app::AppViewState::SystemState(_))
        ));
        assert!(app.open_system_state_tab());
        assert_eq!(app.active_native_view(wid), Some(original));
        assert_eq!(app.windows[&wid].tab_set.len(), tab_count);
        let descriptor = app
            .native_restore_descriptor(app.windows[&wid].tab_set.active().unwrap())
            .unwrap();
        assert_eq!(descriptor, crate::restore::NativeTabRestore::SystemState);
        app.close_active_native_tab(wid).unwrap();
        app.reopen_last_closed_tab().unwrap();
        let reopened = app.active_native_view(wid).unwrap();
        assert_ne!(original, reopened, "closed runtime identities are retired");
        assert_eq!(
            app.pool.views(0),
            Some(1),
            "native opens and restore never create a PTY"
        );
        assert!(app.structural_invariants_ok());
    }

    #[test]
    fn the_system_state_menu_row_opens_the_one_system_state_tab() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let front_kind = |app: &App| {
            let (instance, _) = app.active_native_view(wid)?;
            Some(app.native_runtime.app(instance)?.kind())
        };
        let terminal_only = app.windows[&wid].tab_set.len();
        assert_eq!(front_kind(&app), None, "a terminal tab is in front");
        app.open_system_state_from_menu();
        assert_eq!(app.pending_action_refusal, None);
        assert_eq!(
            front_kind(&app),
            Some(crate::native_app::AppKind::SystemState)
        );
        let opened = app.active_native_view(wid).unwrap();
        assert_eq!(app.windows[&wid].tab_set.len(), terminal_only + 1);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        assert_eq!(front_kind(&app), Some(crate::native_app::AppKind::Settings));
        app.open_system_state_from_menu();
        assert_eq!(app.pending_action_refusal, None);
        assert_eq!(app.active_native_view(wid), Some(opened));
        assert_eq!(app.windows[&wid].tab_set.len(), terminal_only + 2);
        assert!(app.structural_invariants_ok());

        app.frontmost_window = None;
        app.open_system_state_from_menu();
        let refusal = app.pending_action_refusal.take().expect("no front window");
        assert!(refusal.contains("System State"), "{refusal}");
    }

    #[test]
    fn system_state_restore_mints_a_view_and_shares_the_existing_controller() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_system_state_tab());
        let existing = app.active_native_view(wid).unwrap();
        let descriptor = crate::restore::NativeTabRestore::SystemState;
        app.closed_recovery.tabs.push(
            crate::closed_recovery::ClosedTab {
                original_window: wid,
                original_index: 0,
                tab: crate::restore::RestoredTab::from_legacy_native(&descriptor).unwrap(),
            },
            0,
        );
        app.reopen_last_closed_tab().unwrap();
        let reopened = app.active_native_view(wid).unwrap();
        assert_eq!(
            existing.0, reopened.0,
            "one System State controller across restored views"
        );
        assert_ne!(existing.1, reopened.1, "restore must never alias a view");
        assert!(app.view_store.get(existing.1).is_some());
        assert!(app.structural_invariants_ok());
    }

    #[test]
    fn system_state_copies_the_url_selection_and_never_the_password() {
        use crate::native_app::{ActionInvocation, AppEvent, SemanticInput, TextInputEvent};
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_system_state_tab());
        let (_, view) = app.active_native_view(wid).unwrap();
        app.dispatch_native_view_event(
            wid,
            view,
            AppEvent::FocusChanged(Some(crate::native_ui::UiKey::new("system-state/url"))),
        )
        .unwrap();
        app.dispatch_native_view_event(wid, view, AppEvent::TextInput(TextInputEvent::SelectAll))
            .unwrap();
        assert_eq!(
            app.native_selection_text(wid),
            Some(aterm_types::macos_setup::Plan::default().vault_url)
        );
        app.dispatch_native_view_event(
            wid,
            view,
            AppEvent::Action(ActionInvocation {
                id: crate::native_ui::ActionId::new("system-state/password"),
                value: Some(SemanticInput::Text("synthetic_password_only".into())),
            }),
        )
        .unwrap();
        app.dispatch_native_view_event(wid, view, AppEvent::TextInput(TextInputEvent::SelectAll))
            .unwrap();
        assert_eq!(
            app.native_selection_text(wid),
            None,
            "password selection never grants clipboard text"
        );
    }
}

#[cfg(test)]
mod mixed_tab_tests {
    use super::*;

    #[test]
    fn per_agent_off_questions_reach_the_herald_only_for_disabled_agents() {
        use aterm_agent::supervise::SupervisorConfig;
        use aterm_phase::Program;

        let mut app = App::headless_for_test();
        app.push_stub_tab(WindowId(0), crate::stub_session(app.next_session_id));
        for (id, program) in [(0, Program::Agy), (1, Program::Claude)] {
            let ctx = &app.pool.get(id).unwrap().ctx;
            let mut timeline = ctx.timeline.lock().unwrap();
            timeline.set_program(4242 + id as i32, Some(program.name().to_string()));
            timeline.publish_agent(
                "question",
                None,
                None,
                Some(program),
                crate::session_timeline::AgentStamp {
                    generation: crate::control::ScreenGen { epoch: 1, seq: 1 },
                    fp: 1,
                },
            );
        }
        let host = crate::harness_host::HostHandle::for_supervised_test(
            Vec::new(),
            std::sync::Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
        );
        app.harness = Some(host.clone());
        let mut observed = Vec::new();
        for off in [
            Some("agy.enabled"),
            Some("claude.enabled"),
            None,
            Some("enabled"),
        ] {
            let mut cfg = SupervisorConfig::default();
            if let Some(key) = off {
                cfg.set(key, "false").unwrap();
            }
            host.set_config(cfg);
            app.publish_needs_all();
            observed.push({
                let mut sessions = app.status_glance().local_need_sessions();
                sessions.sort_unstable();
                sessions
            });
        }
        let mut cfg = SupervisorConfig::default();
        cfg.set("agy.enabled", "false").unwrap();
        host.set_config(cfg);
        let own = crate::harness_host::holder();
        let mut claimed = Vec::new();
        let mut claims_unchanged = Vec::new();
        for holder in [own.as_str(), "external-supervisor"] {
            for id in 0..2 {
                let ctx = &app.pool.get(id).unwrap().ctx;
                ctx.meta.lock().unwrap().supervisor = None;
                crate::session_timeline::claim_supervisor(
                    ctx,
                    holder,
                    None,
                    None,
                    crate::metrics::now_us(),
                )
                .unwrap();
            }
            app.publish_needs_all();
            claimed.push({
                let mut sessions = app.status_glance().local_need_sessions();
                sessions.sort_unstable();
                sessions
            });
            claims_unchanged.push((0..2).all(|id| {
                app.pool
                    .get(id)
                    .unwrap()
                    .ctx
                    .meta
                    .lock()
                    .unwrap()
                    .live_supervisor(crate::metrics::now_us())
                    == Some(holder)
            }));
        }
        host.shutdown_and_join();
        assert_eq!(
            observed,
            [vec![0], vec![1], vec![], vec![0, 1]],
            "only an admitted agent's question is assigned to the harness"
        );
        assert_eq!(
            claimed,
            [vec![0], vec![]],
            "a disabled own claim is winding down; an external supervisor still answers its questions"
        );
        assert!(
            claims_unchanged.into_iter().all(|unchanged| unchanged),
            "projection never rewrites the actual claims"
        );
    }

    fn file_uri(path: &std::path::Path) -> String {
        crate::native_document_host::path_to_file_uri(path).unwrap()
    }

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
            crate::native_app::AppEvent::Action(crate::native_app::ActionInvocation {
                id: crate::native_ui::ActionId::new(format!(
                    "settings/set/{}",
                    crate::prefs::EDIT_FONT_FAMILY
                )),
                value: None,
            }),
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
        for replay in [false, true] {
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

            if replay {
                app.switch_tab_in(wid, 0);
                assert!(app.active_native_view(wid).is_none());
                assert_eq!(
                    app.replay_deferred_handoff_view_close(wid, tab, settings),
                    None
                );
            } else {
                assert_eq!(app.close_active_tab(), None);
            }
            assert_eq!(
                app.windows[&wid].tab_set.get(tab).unwrap().root.leaves(),
                before,
                "replay={replay}: a blocked close cannot detach either mixed leaf"
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
    }

    #[test]
    fn an_out_of_range_tab_action_refuses_instead_of_reporting_ok() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
        let count = app.windows[&wid].tab_set.len();
        assert!(count >= 2, "two tabs so a real move has somewhere to go");

        app.apply_tab_cmd_in(wid, TabAction::Select(count))
            .expect_err("an index no tab holds must refuse");

        app.apply_tab_cmd_in(wid, TabAction::Close(Some(count + 3)))
            .expect_err("closing an index no tab holds must refuse");

        app.apply_tab_cmd_in(
            wid,
            TabAction::Move {
                from: 0,
                to: count + 1,
            },
        )
        .expect_err("moving to an index no tab holds must refuse");

        assert_eq!(
            app.apply_tab_cmd_in(wid, TabAction::Select(0)),
            Ok((0, count)),
            "an in-range select still reports the state"
        );
        assert_eq!(
            app.apply_tab_cmd_in(wid, TabAction::Move { from: 0, to: 1 }),
            Ok((1, count)),
            "an in-range move still reports the state"
        );
        let after = app
            .apply_tab_cmd_in(wid, TabAction::Close(Some(0)))
            .expect("an in-range close acts");
        assert_eq!(after.1, count - 1, "the tab count really moved");
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

        app.apply_tab_cmd(TabAction::Close(Some(1)))
            .expect_err("a settings draft declines the close");
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

    #[test]
    fn a_quiet_shutdown_probe_reports_the_same_block_without_taking_the_screen() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));
        let (instance, settings) = app.active_native_view(wid).unwrap();
        enter_settings_draft(&mut app, wid, settings);
        app.switch_tab_in(wid, 0);
        let working_tab = app.windows[&wid].tab_set.active_id().unwrap();
        assert!(app.active_native_view(wid).is_none());

        assert!(
            !app.prepare_all_native_shutdown(
                crate::native_app::CloseScope::Relaunch,
                ClosePreflightVisibility::Quiet
            )
            .unwrap(),
            "a quiet probe still refuses the shutdown"
        );
        assert!(
            app.windows[&wid].overlay.is_none(),
            "a quiet probe opened an overlay over the user's work"
        );
        assert_eq!(
            app.windows[&wid].tab_set.active_id(),
            Some(working_tab),
            "a quiet probe switched the active tab"
        );
        assert!(
            app.active_native_view(wid).is_none(),
            "a quiet probe moved focus onto the blocking view"
        );
        assert!(
            !app.native_runtime
                .presentation(instance, settings)
                .unwrap()
                .closable,
            "the draft is still what refused"
        );

        assert!(
            !app.prepare_all_native_shutdown(
                crate::native_app::CloseScope::Relaunch,
                ClosePreflightVisibility::Interactive
            )
            .unwrap()
        );
        assert_eq!(app.active_native_view(wid), Some((instance, settings)));
        assert_exact_settings_close_recovery(&app, wid);

        let tabs = app.windows[&wid].tab_set.len();
        assert!(
            !app.prepare_window_native_shutdown(wid, crate::native_app::CloseScope::Window)
                .unwrap()
        );
        assert!(!app.prepare_quit_native_shutdown().unwrap());
        assert_eq!(app.windows[&wid].tab_set.len(), tabs);
        assert!(app.native_runtime.view_state(settings).is_some());
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
        use aterm_models_gui::pane_tree_model;
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
    fn shared_session_migrated_beside_source_exit_retires_every_occurrence() {
        let mut app = App::headless_for_test();
        let source = WindowId(0);
        let original_view = app.windows[&source].tab_set.active().unwrap().focus;

        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (settings_instance, settings_view) = app.active_native_view(source).unwrap();
        app.switch_tab_in(source, 0);
        let shared_window = app
            .open_active_session_in_new_window_logical()
            .expect("share the active session into a second window");
        let shared_view = app.windows[&shared_window].tab_set.active().unwrap().focus;
        assert_ne!(shared_view, original_view);
        assert_eq!(app.pool.views(0), Some(2));

        let _ = app.migrate_active_tab_to_next_window();
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
    fn heterogeneous_split_runtime_paints_sizes_routes_and_drags_every_leaf() {
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

        let second_raster = window.leaf_render_cache[&second_native]
            .native
            .as_ref()
            .expect("second retained raster");
        let second_stamp = second_raster.stamp;
        let second_pixels = second_raster.rgba.as_ptr() as usize;
        let damage = crate::native_app::DamageRegion::Rect {
            x: 7,
            y: 11,
            width: 19,
            height: 23,
        };
        app.invalidate_native_view_cache(wid, first_native, damage);
        assert_eq!(
            app.windows[&wid].leaf_render_cache[&first_native].native_damage,
            Some(damage)
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
        assert!(matches!(
            app.windows[&wid].leaf_render_cache[&first_native]
                .native
                .as_ref()
                .expect("first raster remains retained")
                .last_work,
            crate::NativeRasterWork::Region { .. }
        ));

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

        assert_eq!(
            app.apply_tab_cmd(TabAction::Select(1)),
            Ok((1, 2)),
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

        assert_eq!(
            app.apply_tab_cmd(TabAction::Close(Some(0))),
            Ok((0, 1)),
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
        let unmoved = |app: &App, what: &str| {
            let ws = &app.windows[&source];
            assert_eq!(
                terminal_tab_ids(&ws.tab_set, &app.view_store),
                terminal_ids,
                "{what}"
            );
            assert_eq!(
                ws.layouts
                    .iter()
                    .map(pane::PaneTree::focus)
                    .collect::<Vec<_>>(),
                vec![0, sid1],
                "{what}"
            );
            assert_eq!(app.pool.views(0), Some(1), "{what}");
            assert_eq!(app.pool.views(sid1), Some(1), "{what}");
            assert!(app.structural_invariants_ok(), "{what}");
        };
        unmoved(&app, "before");

        app.move_tab(source, 2, 0);
        assert_eq!(app.windows[&source].tab_set.tabs()[0].id, native_id);
        assert_eq!(
            app.windows[&source].tabs.active, 1,
            "parked terminal identity is unchanged"
        );
        unmoved(&app, "after the reorder");

        let native_window = app
            .detach_active_tab_logical()
            .expect("mixed source can detach its active native tab");
        unmoved(&app, "after the detach");
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

        let _ = app.migrate_active_tab_to_next_window();
        assert_eq!(app.windows.len(), 1);
        assert_eq!(app.windows[&source].tab_set.active_id(), Some(native_id));
        unmoved(&app, "after the migrate");
    }

    #[test]
    fn terminal_reorder_across_native_tab_reorders_only_its_projection_entry() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let sid1 = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(sid1));
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));

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
        assert_eq!(app.apply_tab_cmd(TabAction::Select(0)), Ok((0, 3)));
        assert!(app.active_handle.lock().unwrap().is_none());
    }

    #[test]
    fn reopen_settings_mints_fresh_tab_view_and_instance_with_route() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));
        let (old_instance, old_view) = app.active_native_view(wid).unwrap();
        assert_eq!(app.pool.views(0), Some(1));

        app.close_active_native_tab(wid).unwrap();
        assert!(app.view_store.get(old_view).is_none());
        assert!(app.native_runtime.app(old_instance).is_none());
        app.reopen_last_closed_tab().unwrap();

        let (new_instance, new_view) = app.active_native_view(wid).unwrap();
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
        let uri = file_uri(&path);

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
        let uri = file_uri(&path);

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
        use aterm_models_native::native_reopen_ledger_model;
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

        let cap = model
            .consts
            .iter()
            .find_map(|&(name, value)| (name == "Cap").then_some(value))
            .expect("NativeReopenLedger declares Cap");
        let mut full = App::headless_for_test();
        full.closed_recovery.tabs = crate::closed_recovery::RecoveryLedger::new(
            usize::try_from(cap).expect("small model capacity"),
            crate::closed_recovery::CLOSED_TAB_MAX_AGE_MS,
        );
        let active_id = |app: &App| app.windows[&wid].tab_set.active_id().unwrap().get() as i64;
        assert!(full.open_settings_tab(crate::native_settings::SettingsRoute::About));
        let mut state = model.init_state();
        let mut before_last_close = state.clone();
        for close in 0..=cap {
            if close > 0 {
                assert!(full.open_settings_tab(crate::native_settings::SettingsRoute::About));
                let opened = active_id(&full);
                let mut after_open = state.clone();
                after_open.insert("native_live", 1);
                after_open.insert("opened_id", opened);
                after_open.insert("next_id", opened + 1);
                assert_step(&model, &state, &after_open, "OpenAnother");
                state = after_open;
            }
            let retired = active_id(&full);
            full.close_active_native_tab(wid).unwrap();
            let mut after_close = state.clone();
            after_close.insert("ledger", full.closed_recovery.tabs.len() as i64);
            after_close.insert("native_live", 0);
            after_close.insert("retired_id", retired);
            assert_step(&model, &state, &after_close, "Close");
            before_last_close = std::mem::replace(&mut state, after_close);
        }
        assert_eq!(state["ledger"], cap, "the full ledger evicted its oldest");

        let mut overfull = state.clone();
        overfull.insert("ledger", cap + 1);
        assert_eq!(admits(&model, &before_last_close, &overfull), None);
        assert!(!model.check_invariant("LedgerBounded", &overfull));
    }
}

#[cfg(test)]
mod home_abbreviation_tests {
    use super::home_relative_suffix;

    #[test]
    fn home_abbreviation_respects_component_boundaries() {
        for (path, home, want) in [
            ("/Users//foo", "/Users//foo", Some("")),
            ("/Users//foo/src/aterm", "/Users//foo", Some("/src/aterm")),
            ("/Users//foobar", "/Users//foo", None),
            ("/elsewhere/x", "/Users//foo", None),
            ("/anything", "", None),
            ("/anything", "/", None),
        ] {
            assert_eq!(
                home_relative_suffix(path, home),
                want,
                "{path} under {home:?}"
            );
        }
    }
}

#[cfg(test)]
mod session_chrome_app_tests {
    use std::time::Instant;

    use super::*;
    use crate::menu::MenuAction;
    use crate::session_chrome::TabMenuEntry;

    #[test]
    fn composed_chrome_reaches_presentation_and_menu_actions_are_honest() {
        let mut app = App::headless_for_test();
        let titles = app.tab_titles(WindowId(0));
        let ext = app.tab_chrome_ext(WindowId(0), &titles);
        assert_eq!(ext.len(), 1);
        assert_eq!(
            ext[0].tooltip, None,
            "a bare live session says nothing its chip does not: `alive` and a spawn \
             event are no tooltip"
        );
        assert_eq!(
            app.windows[&WindowId(0)].tab_set.tabs()[0]
                .presentation
                .tooltip,
            ext[0].tooltip
        );
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
        {
            let ctx = &app.pool.get(0).expect("session 0").ctx;
            ctx.meta
                .lock()
                .unwrap()
                .set("description", Some("purpose text".into()));
            ctx.timeline
                .lock()
                .unwrap()
                .record_raw_meta_change("field=description value=purpose%20text".into());
        }
        app.refresh_window_tabs(WindowId(0));
        let label = app.tab_titles(WindowId(0)).remove(0);
        let tip = app.windows[&WindowId(0)].tab_set.tabs()[0]
            .presentation
            .tooltip
            .clone()
            .expect("a description composes a tooltip");
        assert!(
            tip.starts_with(&label) && tip.contains("purpose text"),
            "{tip:?}"
        );
    }

    #[test]
    fn session_chrome_gather_cuts_agent_moves_before_the_tail_cap() {
        let mut app = App::headless_for_test();
        {
            let ctx = &app.pool.get(0).expect("session 0").ctx;
            let mut tl = ctx.timeline.lock().unwrap();
            tl.record_hand_row("cwd-change", "cwd=/tmp".into());
            for _ in 0..crate::session_chrome::TIMELINE_TAIL + 2 {
                tl.record_hand_row("agent-change", "agent=busy".into());
                tl.record_hand_row("human", "bytes=1".into());
            }
        }
        let titles = app.tab_titles(WindowId(0));
        let ext = app.tab_chrome_ext(WindowId(0), &titles);
        let shown =
            crate::session_chrome::menu_shown_at(&ext[0].menu, crate::turn_ledger::now_ms());
        let rows: Vec<&str> = shown
            .iter()
            .filter_map(|e| match e {
                TabMenuEntry::Header(h) if h.contains(" \u{b7} ") => Some(h.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            rows.iter()
                .any(|r| r.starts_with("changed directory \u{b7} ")),
            "{rows:?}"
        );
        assert!(
            rows.iter()
                .all(|r| !r.starts_with("agent-change") && !r.starts_with("human")),
            "{rows:?}"
        );
    }

    #[test]
    fn chrome_refresh_never_waits_for_terminal_parser_lock() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
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
        {
            let ctx = &app.pool.get(0).expect("session 0").ctx;
            ctx.meta
                .lock()
                .unwrap()
                .set("description", Some("Reviewing proxy boundary".into()));
            ctx.timeline.lock().unwrap().record_raw_meta_change(
                "field=description value=Reviewing%20proxy%20boundary".into(),
            );
        }
        let parser_guard = term.lock().unwrap();

        app.refresh_window_tabs(wid);
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
            "fact-only contention must force the retry past the title-epoch gate"
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
            "retry recomposes the moved chrome after parser contention"
        );
    }

    #[test]
    fn tab_menu_target_follows_its_stable_id_across_a_mid_menu_reorder() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
        app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
        let popped = app.windows[&wid].tab_set.tabs()[1].id;
        let popped_sid = app
            .tab_session_id_text(wid, 1)
            .expect("clicked tab has a registered session");
        app.move_tab(wid, 0, 2);
        let index = app
            .tab_index_for_id(wid, popped)
            .expect("the clicked tab still exists");
        assert_eq!(index, 0, "the clicked tab moved from position 1 to 0");
        assert_eq!(
            app.tab_session_id_text(wid, index),
            Some(popped_sid.clone()),
            "the action lands on the SAME session the menu was popped on"
        );
        assert_ne!(
            app.tab_session_id_text(wid, 1),
            Some(popped_sid),
            "the clicked position was inherited by another session"
        );
    }

    type Endpoint = (aterm_session::SessionId, std::sync::Arc<crate::SessionCtx>);

    fn connected_pair_app() -> (App, Endpoint, Endpoint) {
        let mut app = App::headless_for_test();
        app.push_stub_tab(WindowId(0), crate::stub_session(app.next_session_id));
        let (a, b) = {
            let g = app.store.read().unwrap();
            let a = g.by_local(0).expect("session 0 registered");
            let b = g.by_local(1).expect("session 1 registered");
            (
                (a.sid.clone(), a.ctx.clone()),
                (b.sid.clone(), b.ctx.clone()),
            )
        };
        (app, a, b)
    }

    #[test]
    fn connection_acts_invalidate_the_chrome_cache_without_expiry() {
        let (mut app, (a_sid, _a_ctx), (b_sid, b_ctx)) = connected_pair_app();
        let titles = app.tab_titles(WindowId(0));
        let before = app.tab_chrome_ext(WindowId(0), &titles);
        assert!(
            !before[0]
                .tooltip
                .as_deref()
                .unwrap_or_default()
                .contains("\u{21e5}"),
            "no connection line before any connection exists"
        );
        assert!(crate::connections::connect_in(
            &app.connections,
            &a_sid,
            &b_sid,
            &b_ctx.edges,
            &b_ctx.nonce,
            aterm_session::ConnectionKind::Both,
            "test"
        ));
        let fresh = app.tab_chrome_ext(WindowId(0), &titles);
        let tip_a = fresh[0].tooltip.as_deref().unwrap_or_default();
        assert!(
            tip_a.contains("\u{21e5} pushes into"),
            "A recomposed with its outbound line, no expiry needed: {tip_a:?}"
        );
        let tip_b = fresh[1].tooltip.as_deref().unwrap_or_default();
        assert!(
            tip_b.contains("\u{21e4} pushed by"),
            "B recomposed with the inbound inverse: {tip_b:?}"
        );
        assert!(
            crate::connections::disconnect_kind_in(
                &app.connections,
                &a_sid,
                &b_sid,
                &b_ctx.edges,
                None,
                "test"
            )
            .is_some()
        );
        let cleared = app.tab_chrome_ext(WindowId(0), &titles);
        for (i, ext) in cleared.iter().enumerate() {
            let tip = ext.tooltip.as_deref().unwrap_or_default();
            assert!(
                !tip.contains("\u{21e5}") && !tip.contains("\u{21e4}"),
                "tab {i} still lists a dissolved connection: {tip:?}"
            );
        }
    }

    #[test]
    fn a_pass_whose_edge_snapshot_predates_a_mint_recomposes_on_the_next_pass() {
        let (mut app, (a_sid, _a_ctx), (b_sid, b_ctx)) = connected_pair_app();
        let titles = app.tab_titles(WindowId(0));
        let mut snapshot = Some(EdgeSnapshot {
            revision: app.connections.revision(),
            edges: crate::connections::all_edges(&app.store),
        });
        assert!(crate::connections::connect_in(
            &app.connections,
            &a_sid,
            &b_sid,
            &b_ctx.edges,
            &b_ctx.nonce,
            aterm_session::ConnectionKind::Both,
            "test"
        ));
        let names_the_mint = |ext: &crate::session_chrome::TabChromeExt| {
            ext.tooltip
                .as_deref()
                .unwrap_or_default()
                .contains("\u{21e5} pushes into")
        };
        let mid_pass = app.composed_session_chrome(WindowId(0), 0, &titles[0], &mut snapshot);
        assert!(
            !names_the_mint(&mid_pass),
            "composed from the snapshot taken before the mint"
        );
        let next = app.tab_chrome_ext(WindowId(0), &titles);
        assert!(
            names_the_mint(&next[0]),
            "the next pass recomposes the tab with the mint"
        );
    }

    #[test]
    fn tab_menu_session_rows_show_and_never_stop_unconfirmed() {
        use crate::session_chrome::ConnVerb;
        use crate::task_record::{TaskMark, TaskState, mark_session_task};
        let (mut app, (_a_sid, _a_ctx), (b_sid, b_ctx)) = connected_pair_app();
        let wid = WindowId(0);
        let tab_a = app.windows[&wid].tab_set.tabs()[0].id;
        app.dispatch_tab_menu_connection(wid, tab_a, &b_sid, ConnVerb::Show);
        assert_eq!(
            app.windows[&wid].tab_set.active_index(),
            Some(1),
            "Show raises the named session's tab"
        );
        app.switch_tab_in(wid, 0);
        app.dispatch_tab_menu_connection(
            wid,
            crate::tab_model::TabId::from_stored(u64::MAX),
            &b_sid,
            ConnVerb::Show,
        );
        assert_eq!(app.windows[&wid].tab_set.active_index(), Some(0));
        let b_local = app.store.read().unwrap().by_sid(&b_sid).unwrap().local_id;
        mark_session_task(&b_ctx, b_local, &TaskMark::new(TaskState::Working)).expect("unfenced");
        app.dispatch_tab_menu_connection(wid, tab_a, &b_sid, ConnVerb::StopWorker);
        assert_eq!(app.agents.stop_signals, 0, "nothing was signalled");
        let card = app.windows[&wid].overview().expect("the overview opened");
        assert!(
            card.controls_lines()
                .iter()
                .any(|l| l.contains(&format!("sid={} ", b_sid.as_str()))
                    && l.contains("confirm=true")),
            "armed on B"
        );
        app.overview_exit(wid);
        app.agents.owner_sheet_for_test = Some(Some(false));
        app.dispatch_tab_menu_connection(wid, tab_a, &b_sid, ConnVerb::StopWorker);
        assert_eq!(app.agents.stop_signals, 0, "Cancel stops nothing");
        assert!(app.windows[&wid].overview().is_none());
        app.agents.owner_sheet_for_test = Some(Some(true));
        app.dispatch_tab_menu_connection(wid, tab_a, &b_sid, ConnVerb::StopWorker);
        assert_eq!(app.agents.stop_signals, 1, "Stop signals once");
    }

    #[test]
    fn tab_menu_target_of_a_closed_tab_is_dropped_not_redirected() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
        app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
        let popped = app.windows[&wid].tab_set.tabs()[1].id;
        let neighbor_sid = app.tab_session_id_text(wid, 2);
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
    }

    #[cfg(any(windows, target_os = "linux"))]
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

        assert!(app.open_active_tab_context_menu(wid));
        assert!(app.tab_menu_input_event(wid, &ev(MouseButton::Left, true)));
        assert!(
            !app.tab_menu_input_event(wid, &ev(MouseButton::Right, false)),
            "a button the card never consumed passes through"
        );
    }
}

#[cfg(test)]
mod rename_strip_click_tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn app_with_two_tabs() -> (App, WindowId) {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.tab_strip_rows = 1;
        app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
        app.splice_tab_strip(wid);
        (app, wid)
    }

    fn chip_col(app: &App, wid: WindowId, index: usize) -> u16 {
        let seg = app.windows[&wid]
            .tab_segments
            .iter()
            .find(|seg| seg.kind == tab_bar::TabHit::Select(index))
            .expect("the tab has a segment");
        seg.start_col + 1
    }

    use crate::session_pin_for_test as pin;

    #[test]
    fn a_press_on_a_background_tabs_marks_selects_it() {
        let (mut app, wid) = app_with_two_tabs();
        let front = app.windows[&wid].tab_set.active_index();
        assert_eq!(front, Some(1), "the pushed tab is in front");
        app.windows
            .get_mut(&wid)
            .and_then(|ws| ws.tab_set.tab_at_mut(0))
            .expect("tab 0")
            .presentation
            .indicators
            .busy = true;
        app.splice_tab_strip(wid);
        let seg = app.windows[&wid]
            .tab_segments
            .iter()
            .copied()
            .find(|seg| seg.kind == tab_bar::TabHit::Select(0))
            .expect("tab 0's chip");
        let marks = seg.close_col.map_or(seg.end_col - 2, |close| close - 2);
        assert_eq!(
            tab_bar::hit_test(&app.windows[&wid].tab_segments, marks),
            Some(tab_bar::TabHit::Select(0))
        );

        app.handle_tab_strip_click(wid, marks);
        assert!(app.windows[&wid].tab_menu.is_none(), "no menu");
        assert_eq!(
            app.windows[&wid].tab_set.active_index(),
            Some(0),
            "selected"
        );
        assert_eq!(app.rename_edit_session(wid), None);
    }

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

        let (mut app, wid) = app_with_two_tabs();
        app.handle_tab_strip_click(wid, chip_col(&app, wid, 0));
        app.handle_tab_strip_click(wid, chip_col(&app, wid, 1));
        assert_eq!(app.rename_edit_session(wid), None, "two chips, two clicks");
        assert_eq!(app.windows[&wid].tabs.active, 1);

        let (mut app, wid) = app_with_two_tabs();
        app.handle_tab_strip_click(wid, col);
        let stale = Instant::now() - Duration::from_millis(crate::multi_click_ms() as u64 + 50);
        let tab = app.windows[&wid].tab_set.tabs()[0].id;
        app.windows.get_mut(&wid).unwrap().strip_press = Some((stale, tab));
        app.handle_tab_strip_click(wid, col);
        assert_eq!(app.rename_edit_session(wid), None, "a stale press");
    }

    #[test]
    fn selecting_another_chip_commits_while_the_edited_chip_keeps_editing() {
        let (mut app, wid) = app_with_two_tabs();
        let tab0 = app.windows[&wid].tab_set.tabs()[0].id;
        assert!(app.begin_session_rename(wid, tab0));
        app.rename_field_edit(wid, crate::app_search::SearchEdit::Insert("kept".into()));

        app.handle_tab_strip_click(wid, chip_col(&app, wid, 0));
        assert_eq!(app.rename_edit_session(wid), Some(0), "still editing");
        assert_eq!(pin(&app, 0), None, "nothing committed yet");

        app.handle_tab_strip_click(wid, chip_col(&app, wid, 1));
        assert_eq!(app.rename_edit_session(wid), None, "the editor closed");
        assert_eq!(pin(&app, 0).as_deref(), Some("kept"));
        assert_eq!(app.windows[&wid].tabs.active, 1, "and the tab switched");
    }

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

    #[cfg(windows)]
    #[test]
    fn a_version_press_is_a_button_not_caption() {
        crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
        let (mut app, wid) = app_with_two_tabs();
        let label = app.windows[&wid]
            .tab_segments
            .iter()
            .find(|seg| matches!(seg.kind, tab_bar::TabHit::Version { .. }))
            .map(|seg| seg.start_col + 2)
            .expect("the strip lays the version label out once the UI face is in");
        let tab0 = app.windows[&wid].tab_set.tabs()[0].id;
        assert!(app.begin_session_rename(wid, tab0));
        app.rename_field_edit(wid, crate::app_search::SearchEdit::Insert("kept".into()));
        let (tabs, active) = (
            app.windows[&wid].tab_set.len(),
            app.windows[&wid].tabs.active,
        );

        for press in 0..2 {
            app.handle_tab_strip_click(wid, label);
            let ws = &app.windows[&wid];
            assert!(
                ws.strip_band_press.is_none(),
                "press {press}: not a caption press"
            );
            assert!(ws.strip_press.is_none(), "press {press}: not a chip press");
            assert!(ws.strip_drag.is_none(), "press {press}: grabs no tab");
            assert_eq!(
                ws.tab_set.len(),
                tabs,
                "press {press}: no tab opened or closed"
            );
            assert_eq!(ws.tabs.active, active, "press {press}: no tab switched");
        }
        assert_eq!(
            app.rename_edit_session(wid),
            None,
            "the open rename settled"
        );
        assert_eq!(
            pin(&app, 0).as_deref(),
            Some("kept"),
            "…keeping what was typed"
        );
        crate::tray_raster::clear_ui_fonts_for_test();
    }

    #[test]
    fn bare_band_presses_keep_their_own_double_click_streak() {
        let (mut app, wid) = app_with_two_tabs();
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

        app.handle_tab_strip_click(wid, band);
        assert!(app.windows[&wid].strip_band_press.is_some());
        app.handle_tab_strip_click(wid, chip_col(&app, wid, 0));
        assert!(
            app.windows[&wid].strip_band_press.is_none(),
            "a chip press disarms the band streak"
        );

        app.windows.get_mut(&wid).unwrap().strip_band_press =
            Some(Instant::now() - Duration::from_millis(crate::multi_click_ms() as u64 + 50));
        app.handle_tab_strip_click(wid, band);
        assert!(
            app.windows[&wid].strip_band_press.is_some(),
            "a stale press re-arms instead of consuming"
        );
    }

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

#[cfg(test)]
mod status_glance_tests {
    use super::*;

    fn set_meta(app: &mut App, session: u64, field: &str, value: &str) {
        {
            let store = app.store.read().unwrap_or_else(|p| p.into_inner());
            let h = store.by_local(session).expect("registered session");
            h.ctx
                .meta
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .set(field, Some(value.to_string()));
        }
        app.on_meta_changed(session);
    }

    fn publish_meta(app: &mut App, session: u64, field: &str, value: &str) {
        {
            let store = app.store.read().unwrap_or_else(|p| p.into_inner());
            let h = store.by_local(session).expect("registered session");
            h.ctx
                .meta
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .set(field, Some(value.to_string()));
        }
        app.publish_need(session);
    }

    #[test]
    fn typed_meta_drives_the_headless_glance() {
        let mut app = App::headless_for_test();
        let g = app.status_glance();
        assert!(g.needs.is_empty());
        assert_eq!(g.button_title(), "\u{276f}");
        set_meta(&mut app, 0, "role", "operator");
        assert!(app.status_glance().needs.is_empty(), "a role is a label");
        set_meta(&mut app, 0, "attention", "wedged on CI");
        let g = app.status_glance();
        assert_eq!(g.local_need_sessions(), vec![0]);
        assert_eq!(g.button_title(), "\u{276f}1");
        assert_eq!(g.needs[0].label, "operator", "the role labels the tab");
        let line = app.status_item_chrome_line();
        assert!(line.contains(" needs=1 stuck=0 "), "{line}");
    }

    #[test]
    fn chrome_reads_the_status_menu_as_its_builder_composes_it() {
        let mut app = App::headless_for_test();
        let needs = |app: &App| {
            app.status_item_chrome_lines()
                .iter()
                .any(|line| line.starts_with("statusmenu \"Needs you (1)\""))
        };
        assert!(!needs(&app));
        set_meta(&mut app, 0, "attention", "wedged on CI");
        let drawn = (
            app.status_gen,
            app.status_drawn.len(),
            app.status_painted.is_some(),
        );
        let now = aterm_time::unix_secs().map_or(0, |s| i64::try_from(s).unwrap_or(0));
        let lines = app.status_item_chrome_lines_at(now);
        assert!(
            lines[0].starts_with("statusitem title=\u{276f}1 "),
            "{lines:#?}"
        );
        assert_eq!(
            lines[1..],
            crate::status_item::menu_lines(
                &crate::status_item::compose_status_menu(&app.status_glance(), now),
                &[],
            )[..],
        );
        assert!(needs(&app), "{lines:#?}");
        assert!(
            lines.iter().any(|line| line.starts_with(
                "statusmenu \"Notify Me When I\u{2019}m Needed\" enabled=yes checked=no "
            )),
            "{lines:#?}"
        );
        assert_eq!(
            (
                app.status_gen,
                app.status_drawn.len(),
                app.status_painted.is_some()
            ),
            drawn,
            "a read changes nothing of the menu"
        );
    }

    #[test]
    fn hand_back_all_lifts_every_person_hold_and_nothing_else() {
        use aterm_types::needs::HoldWord;
        let mut app = App::headless_for_test();
        let wid = app.frontmost_window.expect("the test App's window");
        for _ in 0..2 {
            app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
        }
        let ctx = |app: &App, session: u64| app.pool.get(session).expect("pooled").ctx.clone();
        app.take_over_session(0).expect("the person takes tab 1");
        app.take_over_session(1).expect("and tab 2");
        assert!(crate::fabric::apply_hold_for_test(
            &ctx(&app, 2),
            Some(crate::fabric::Hold {
                reason: "-".into(),
                origin: "local".into(),
            })
        ));
        app.refresh_presence_session(2);
        let held = |app: &App| -> Vec<HoldWord> {
            let mut held: Vec<HoldWord> = app.status_glance().held.iter().map(|h| h.hold).collect();
            held.sort();
            held
        };
        assert_eq!(
            held(&app),
            [HoldWord::Person, HoldWord::Person, HoldWord::Local]
        );
        app.hand_back_all();
        assert!(app.person_holds().is_empty(), "every person hold lifted");
        assert!(ctx(&app, 2).fabric.holds().is_held(), "the client's stands");
        assert_eq!(held(&app), [HoldWord::Local]);
    }

    #[test]
    fn a_write_the_persons_hold_refused_reaches_the_you_hold_row() {
        let mut app = App::headless_for_test();
        app.take_over_session(0).expect("the person takes tab 1");
        let ctx = app.pool.get(0).expect("pooled").ctx.clone();
        let _ = app.take_status_rebuild();
        app.note_person_refusals();
        assert!(!app.take_status_rebuild(), "nothing refused yet");
        assert_eq!(app.status_glance().held[0].refused, 0);
        assert_eq!(crate::fabric::halt_refusal(&ctx, "status"), None);
        app.note_person_refusals();
        assert!(!app.take_status_rebuild(), "a read moves nothing");
        assert_eq!(app.status_glance().held[0].refused, 0);
        let refusal = crate::fabric::halt_refusal(&ctx, "send").expect("halted");
        assert!(refusal.contains("origin=person"), "{refusal}");
        app.note_person_refusals();
        assert!(
            app.take_status_rebuild(),
            "the count moved: one rebuild owed"
        );
        assert_eq!(app.status_glance().held[0].refused, 1);
        app.note_person_refusals();
        assert!(!app.take_status_rebuild(), "and only once");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn the_status_items_overview_opens_the_card_raised() {
        let mut app = App::headless_for_test();
        let wid = app.frontmost_window.expect("the test App's window");
        let _ = take_raised_windows();
        app.open_overview().expect("the Agents row opens it");
        assert!(app.windows[&wid].overview().is_some());
        assert_eq!(take_raised_windows(), Vec::new(), "control: no raise");
        app.overview_exit(wid);
        assert!(app.windows[&wid].overview().is_none());
        app.dispatch_status_action(crate::status_item::StatusAction::Overview);
        assert!(app.windows[&wid].overview().is_some(), "the card is open");
        assert_eq!(take_raised_windows(), vec![wid], "and its window raised");
    }

    #[test]
    fn a_stale_pick_resolves_to_nothing() {
        let mut app = App::headless_for_test();
        set_meta(&mut app, 0, "attention", "look");
        let first = app.status_glance();
        let first = app.status_drawn_next(first);
        let second = app.status_glance();
        let second = app.status_drawn_next(second);
        let pick = |generation, index| crate::status_item::Pick { generation, index };
        assert!(app.status_pick(pick(first.generation, 0)).is_some());
        assert!(app.status_pick(pick(second.generation, 0)).is_some());
        let third = app.status_glance();
        let third = app.status_drawn_next(third);
        assert!(
            app.status_pick(pick(first.generation, 0)).is_none(),
            "a generation no longer kept"
        );
        let g = app.status_pick(pick(third.generation, 7)).expect("kept");
        assert!(g.go.get(7).is_none(), "an index past the table");
    }

    #[test]
    fn an_open_menu_s_clicks_survive_the_hand_overs_it_causes() {
        let pick = |generation, index| crate::status_item::Pick { generation, index };
        for keep in [true, false] {
            let mut app = App::headless_for_test();
            set_meta(&mut app, 0, "attention", "look");
            let before = app.status_glance();
            let before = app.status_drawn_next(before);
            let painted = app.status_glance();
            let painted = app.status_drawn_next(painted);
            if keep {
                app.note_status_painted(Some(painted.clone()));
            }
            let began = app.status_glance();
            let _ = app.status_drawn_next(began);
            let ended = app.status_glance();
            let ended = app.status_drawn_next(ended);
            let go = app
                .status_pick(pick(painted.generation, 0))
                .and_then(|g| g.go.first().copied());
            if !keep {
                assert_eq!(go, None, "the control: the painted glance is gone");
                continue;
            }
            assert_eq!(go, Some(0), "the painted glance's Go still lands");
            assert!(app.status_pick(pick(ended.generation, 0)).is_some());
            assert!(
                app.status_pick(pick(before.generation, 0)).is_none(),
                "drawn before that open"
            );
            app.note_status_painted(None);
            assert!(
                app.status_pick(pick(painted.generation, 0)).is_some(),
                "an open with no paint keeps the last"
            );
            app.note_status_painted(Some(ended.clone()));
            assert!(
                app.status_pick(pick(painted.generation, 0)).is_none(),
                "the next open's paint replaces it"
            );
        }
    }

    #[test]
    fn a_done_stands_until_its_tab_is_looked_at() {
        let mut app = App::headless_for_test();
        app.push_stub_tab(WindowId(0), crate::stub_session(1));
        let ctx = app.pool.get(1).expect("pooled").ctx.clone();
        let mark = |app: &mut App, state| {
            let mark = crate::task_record::TaskMark::new(state);
            crate::task_record::mark_session_task(&ctx, 1, &mark).expect("marked");
            app.on_task_changed(1);
        };
        mark(&mut app, crate::task_record::TaskState::Working);
        mark(&mut app, crate::task_record::TaskState::Done);
        app.note_done_seen_looking();
        assert_eq!(app.status_glance().done.len(), 1, "nobody looked");
        assert_eq!(app.status_glance().button_title(), "\u{276f}\u{2022}");
        let ws = app.windows.get_mut(&WindowId(0)).expect("window 0");
        ws.band_on_screen_for_test = true;
        app.note_done_seen_looking();
        assert!(app.status_glance().done.is_empty(), "looked at");
        app.windows
            .get_mut(&WindowId(0))
            .expect("window 0")
            .occluded = true;
        mark(&mut app, crate::task_record::TaskState::Working);
        mark(&mut app, crate::task_record::TaskState::Done);
        app.note_done_seen_looking();
        assert_eq!(app.status_glance().done.len(), 1, "a later done is new");
    }

    #[cfg(unix)]
    #[test]
    fn an_adopted_standing_need_is_told_once() {
        use crate::task_record::TaskState;
        let take_posts = || HERALD_POSTS.with(|p| std::mem::take(&mut *p.borrow_mut()));
        let desk = |app: &mut App, task: Option<&str>| {
            app.push_stub_tab(WindowId(0), crate::stub_session(1));
            let ctx = app.pool.get(1).expect("pooled").ctx.clone();
            match task {
                Some(row) => crate::task_record::seed_carried(&ctx, Some(row)),
                None => {
                    for state in [TaskState::Working, TaskState::Done] {
                        let mark = crate::task_record::TaskMark::new(state);
                        crate::task_record::mark_session_task(&ctx, 1, &mark).expect("marked");
                    }
                }
            }
            app.on_task_changed(1);
            app.headless = false;
            app.config.desktop_alerts = Some(true);
            app.notify_suppress.lock().unwrap().clear();
            HERALD_REFUSAL.set(None);
            publish_meta(app, 1, "attention", "wedged on CI");
            ctx
        };
        let mut parent = App::headless_for_test();
        let ctx = desk(&mut parent, None);
        let on_screen = |app: &mut App, on: bool| {
            app.windows
                .get_mut(&WindowId(0))
                .expect("window 0")
                .band_on_screen_for_test = on;
        };
        on_screen(&mut parent, true);
        parent.note_done_seen_looking();
        on_screen(&mut parent, false);
        let _ = take_posts();
        parent.herald_session(1);
        assert_eq!(take_posts().len(), 1, "PRECONDITION: the parent told it");
        let row = parent
            .status_carry_rows()
            .into_iter()
            .find_map(|(id, row)| (id == 1).then_some(row))
            .expect("session 1's row");
        let task = ctx
            .timeline
            .lock()
            .unwrap()
            .task()
            .expect("the task")
            .carry_row();

        let successor = |carried: bool| {
            let mut app = App::headless_for_test();
            let ctx = desk(&mut app, Some(&task));
            if carried {
                let mut carry = crate::status_item::StatusCarry::parse(&row);
                carry.need = carry.need.map(|(word, _)| (word, 1_000));
                app.status_carry
                    .insert(ctx.self_id.as_str().to_string(), carry);
            }
            let _ = take_posts();
            app.herald_session(1);
            app
        };
        let mut adopted = successor(true);
        assert!(take_posts().is_empty(), "told before the update");
        assert_eq!(adopted.presence.herald.need_since(1), Some(1_000));
        assert!(adopted.status_glance().done.is_empty(), "read before it");
        assert!(adopted.status_carry.is_empty(), "spent");
        publish_meta(&mut adopted, 1, "attention", "a new question");
        adopted.herald_session(1);
        assert_eq!(take_posts().len(), 1, "a new ask after it");

        let fresh = successor(false);
        assert_eq!(take_posts().len(), 1, "no carry: told again");
        assert_ne!(fresh.presence.herald.need_since(1), Some(1_000));
        assert_eq!(fresh.status_glance().done.len(), 1, "no carry: unread");
    }

    #[test]
    fn go_focuses_the_named_pane_of_a_split_tab() {
        let focused = |app: &App| {
            app.active_tree(WindowId(0))
                .map(crate::pane::PaneTree::focus)
        };
        let looked_at = |app: &App, session: u64| {
            app.notify_suppress
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .contains(&session)
        };
        let mut app = App::headless_for_test();
        let right = app.split_active_stub_tab(WindowId(0));
        assert_eq!(
            focused(&app),
            Some(right),
            "fixture: the split pane has the keyboard"
        );
        assert!(
            !looked_at(&app, 0),
            "fixture: the needy pane is not looked at"
        );
        assert!(app.focus_session_window(0));
        assert_eq!(focused(&app), Some(0), "the needy pane has the keyboard");
        assert!(looked_at(&app, 0), "and it is looked at");
        assert!(!looked_at(&app, right));

        let mut single = App::headless_for_test();
        assert_eq!(focused(&single), Some(0));
        assert!(single.focus_session_window(0));
        assert_eq!(focused(&single), Some(0), "one pane: unchanged");
        assert!(looked_at(&single, 0));
    }

    #[test]
    fn a_notice_click_goes_to_its_tab_and_pane() {
        use crate::notice_center::{DEFAULT_ACTION, clicked_session};
        let wid = WindowId(0);
        let state = |app: &App| {
            (
                app.windows[&wid].tabs.active,
                app.active_tree(wid).map(crate::pane::PaneTree::focus),
            )
        };
        let own = std::process::id();
        let pid = own.to_string();
        let incarnation = "captured-notice-process";
        let instance = Some(incarnation);
        let mut app = App::headless_for_test();
        let right = app.split_active_stub_tab(wid);
        assert_eq!(state(&app), (0, Some(right)), "fixture: [0 | right]");
        app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
        let elsewhere = state(&app);
        assert_eq!(elsewhere.0, 1, "fixture: the owner is on another tab");
        let _ = take_raised_windows();

        let dismissed = "com.apple.UNNotificationDismissActionIdentifier";
        assert_eq!(
            clicked_session(dismissed, Some("0"), Some(&pid), instance, own, incarnation),
            None
        );
        assert_eq!(
            clicked_session(
                DEFAULT_ACTION,
                Some("0"),
                Some("1"),
                instance,
                own,
                incarnation
            ),
            None
        );
        let gone = clicked_session(
            DEFAULT_ACTION,
            Some("999"),
            Some(&pid),
            instance,
            own,
            incarnation,
        )
        .expect("a click on this process's notice names its session");
        app.go_to_session(gone);
        assert_eq!(state(&app), elsewhere, "a session gone moves nothing");
        assert_eq!(take_raised_windows(), Vec::new(), "and raises nothing");

        let clicked = clicked_session(
            DEFAULT_ACTION,
            Some("0"),
            Some(&pid),
            instance,
            own,
            incarnation,
        );
        app.go_to_session(clicked.expect("the notice named session 0"));
        assert_eq!(state(&app), (0, Some(0)), "its tab, and its pane");
        assert_eq!(take_raised_windows(), vec![wid], "its window raised");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn only_turning_notify_me_on_asks_macos() {
        use crate::notice_center::take_asks;
        use crate::status_item::StatusAction;
        let mut app = App::headless_for_test();
        assert!(!app.config.desktop_alerts_or_default(), "off by default");
        let _ = take_asks();
        app.dispatch_status_action(StatusAction::ToggleNotify);
        assert!(app.config.desktop_alerts_or_default(), "on");
        assert_eq!(take_asks(), 1, "turning it on asks");
        app.dispatch_status_action(StatusAction::ToggleNotify);
        assert!(!app.config.desktop_alerts_or_default(), "off");
        assert_eq!(take_asks(), 0, "turning it off asks nothing");
    }

    #[test]
    fn a_refused_herald_notice_stays_owed() {
        let take_posts = || HERALD_POSTS.with(|p| std::mem::take(&mut *p.borrow_mut()));
        let herald = |refusal: Option<&'static str>| {
            let mut app = App::headless_for_test();
            app.headless = false;
            app.config.desktop_alerts = Some(true);
            app.notify_suppress
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clear();
            publish_meta(&mut app, 0, "attention", "wedged on CI");
            let _ = take_posts();
            HERALD_REFUSAL.set(refusal);
            app.herald_session(0);
            app
        };

        let app = herald(Some("the notifier's queue is full"));
        assert!(take_posts().is_empty(), "the queue refused it");
        let later = std::time::Instant::now() + crate::status_item::NOTIFY_BURST_WINDOW;
        assert_eq!(app.presence.herald.due(later).0, vec![0], "owed again");

        let app = herald(None);
        assert_eq!(take_posts().len(), 1, "the queue took it");
        let later = std::time::Instant::now() + crate::status_item::NOTIFY_BURST_WINDOW;
        assert_eq!(app.presence.herald.due(later), (vec![], None));
    }

    #[test]
    fn desktop_alerts_off_never_arms_rate_limited_herald_retries() {
        let mut app = App::headless_for_test();
        app.config.desktop_alerts = Some(false);
        app.apply_desktop_alerts(false);
        for _ in 0..crate::status_item::NOTIFY_BURST {
            app.push_stub_tab(WindowId(0), crate::stub_session(app.next_session_id));
        }
        app.headless = false;
        app.notify_suppress.lock().unwrap().clear();
        HERALD_POSTS.with(|p| p.borrow_mut().clear());
        HERALD_REFUSAL.set(None);
        let sessions: Vec<_> = app.pool.iter().map(|s| s.id).collect();
        for &session in &sessions {
            set_meta(&mut app, session, "attention", "first request");
            app.herald_session(session);
            set_meta(&mut app, session, "attention", "second request");
            app.herald_session(session);
        }
        let now = std::time::Instant::now();
        for at in [now, now + crate::status_item::NOTIFY_BURST_WINDOW] {
            assert_eq!(app.presence.herald.due(at), (vec![], None));
        }
        assert!(HERALD_POSTS.with(|p| p.borrow().is_empty()));
        assert_eq!(app.status_glance().need_count(), sessions.len());
    }

    #[test]
    fn disabling_desktop_alerts_cancels_owed_notices_without_replaying_them() {
        for refused in [false, true] {
            let mut app = App::headless_for_test();
            app.push_stub_tab(WindowId(0), crate::stub_session(app.next_session_id));
            app.headless = false;
            app.config.desktop_alerts = Some(true);
            app.apply_desktop_alerts(true);
            app.notify_suppress.lock().unwrap().clear();
            HERALD_POSTS.with(|p| p.borrow_mut().clear());
            HERALD_REFUSAL.set(refused.then_some("the notifier's queue is full"));
            set_meta(&mut app, 0, "attention", "first request");
            app.herald_session(0);
            if !refused {
                set_meta(&mut app, 0, "attention", "second request");
                app.herald_session(0);
            }
            let later = std::time::Instant::now() + crate::status_item::NOTIFY_BURST_WINDOW;
            assert_eq!(app.presence.herald.due(later).0, vec![0], "debt exists");
            let warnings = app.status_glance().needs;

            app.config.desktop_alerts = Some(false);
            app.apply_desktop_alerts(false);
            assert_eq!(app.presence.herald.due(later), (vec![], None));
            assert_eq!(app.status_glance().needs, warnings);
            assert!(
                !app.notify_own_alerts
                    .load(std::sync::atomic::Ordering::Acquire)
            );

            HERALD_POSTS.with(|p| p.borrow_mut().clear());
            app.config.desktop_alerts = Some(true);
            app.apply_desktop_alerts(true);
            app.herald_session(0);
            assert!(HERALD_POSTS.with(|p| p.borrow().is_empty()), "no replay");
            assert_eq!(app.presence.herald.due(later), (vec![], None));
            set_meta(&mut app, 1, "attention", "new request after enabling");
            app.herald_session(1);
            HERALD_POSTS.with(|p| {
                let posts = std::mem::take(&mut *p.borrow_mut());
                assert_eq!(posts.len(), 1, "new transitions still notify");
                assert_eq!(posts[0].session, 1);
            });
        }
    }
}

#[cfg(test)]
mod tab_context_menu_tests {
    use super::*;

    #[test]
    fn opening_pops_the_composed_model() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.tab_strip_rows = 1;
        let opened_from = crate::turn_ledger::now_ms();
        assert!(
            app.open_tab_context_menu(wid, 0, 5, false),
            "a terminal tab always has a menu"
        );
        let opened_by = crate::turn_ledger::now_ms();
        let menu = app.windows[&wid].tab_menu.as_ref().expect("menu is open");
        assert_eq!(menu.anchor_col, 5, "anchored where the press landed");
        assert_eq!(menu.highlight, None, "a mouse open lights no row");
        let titles = app.tab_titles(wid);
        let composed = app.tab_chrome_ext(wid, &titles);
        let card = &app.windows[&wid].tab_menu.as_ref().unwrap().entries;
        assert!(
            [opened_from, opened_by]
                .iter()
                .any(|&at| *card == crate::session_chrome::menu_shown_at(&composed[0].menu, at)),
            "the card renders the one composed model, never a parallel list \
             (its action bits: `composed_chrome_reaches_presentation_and_menu_actions_are_honest`):\
             \n{card:?}\n{:?}",
            composed[0].menu
        );
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

    #[test]
    fn no_strip_means_no_menu() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.tab_strip_rows = 0;
        assert!(!app.open_active_tab_context_menu(wid));
        assert!(!app.open_tab_context_menu(wid, 0, 0, false));
        assert!(app.windows[&wid].tab_menu.is_none());
    }

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

fn status_unix_now() -> i64 {
    aterm_time::unix_secs().map_or(0, |s| i64::try_from(s).unwrap_or(0))
}

#[cfg(test)]
mod connected_spawn_tests {
    use super::*;
    use crate::connections::{ConnectedSpawnKind, ConnectedSpawnPlace};
    use aterm_session::{Op, SessionId};

    fn register_stub(app: &mut App, id: u64) -> SessionId {
        let s = crate::stub_session(id);
        let sid = s.ctx.self_id.clone();
        App::register_session(&app.store, &s, None);
        app.pool.insert(s);
        sid
    }

    fn session0_sid(app: &App) -> SessionId {
        app.store
            .read()
            .unwrap()
            .by_local(0)
            .expect("harness registers session 0")
            .sid
            .clone()
    }

    #[test]
    fn precheck_refuses_headless_window_and_dead_origins_but_allows_tabs() {
        let app = App::headless_for_test();
        let sid0 = session0_sid(&app);
        assert_eq!(
            app.connected_spawn_precheck(ConnectedSpawnPlace::Window, &sid0),
            Err("headless: use place=tab".to_string()),
            "place=window under headless refuses BEFORE any create (design §1.4#7)"
        );
        assert_eq!(
            app.connected_spawn_precheck(ConnectedSpawnPlace::Tab, &sid0),
            Ok((0, Some(WindowId(0)))),
            "place=tab is headless-legal and lands beside the origin"
        );
        assert_eq!(
            app.connected_spawn_precheck(ConnectedSpawnPlace::Tab, &SessionId::new("s-nope")),
            Err("no such session".to_string())
        );
        app.store
            .write()
            .unwrap()
            .set_state(0, session_store::SessionState::Exited);
        assert_eq!(
            app.connected_spawn_precheck(ConnectedSpawnPlace::Tab, &sid0),
            Err("exited".to_string()),
            "a dead origin can neither drive nor be driven"
        );
    }

    #[test]
    fn mint_connected_writes_a_both_connection_in_the_right_direction() {
        let mut app = App::headless_for_test();
        let origin = session0_sid(&app);
        let newborn = register_stub(&mut app, 41);

        assert_eq!(
            app.mint_connected(ConnectedSpawnKind::Controlled, &origin, &newborn, "test"),
            Ok(())
        );
        let rows = {
            let ctx = {
                let g = app.store.read().unwrap();
                g.by_sid(&newborn).unwrap().ctx.clone()
            };
            let edges = ctx.edges.lock().unwrap();
            edges.edges()
        };
        assert_eq!(rows.len(), 3, "both = pull + push = 3 ops: {rows:?}");
        for row in &rows {
            assert_eq!(row.src, origin, "the origin drives the newborn");
            assert_eq!(row.dst, newborn);
        }
        let ops: Vec<Op> = rows.iter().map(|e| e.op).collect();
        for op in [Op::ReadScreen, Op::WriteInput, Op::Signal] {
            assert!(ops.contains(&op), "missing {op:?} in {ops:?}");
        }
        let roles = crate::connections::roles_by_local(&app.store);
        assert!(roles[&0].outbound && !roles[&0].inbound);
        assert!(roles[&41].inbound && !roles[&41].outbound);

        let supervisor = register_stub(&mut app, 42);
        assert_eq!(
            app.mint_connected(ConnectedSpawnKind::Controller, &origin, &supervisor, "test"),
            Ok(())
        );
        let origin_rows = {
            let ctx = {
                let g = app.store.read().unwrap();
                g.by_sid(&origin).unwrap().ctx.clone()
            };
            let edges = ctx.edges.lock().unwrap();
            edges.edges()
        };
        assert_eq!(origin_rows.len(), 3);
        for row in &origin_rows {
            assert_eq!(row.src, supervisor, "the newborn supervises the origin");
            assert_eq!(row.dst, origin);
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_connected_tab_rechecks_its_origin_before_installing() {
        use crate::tab_spawn::{SpawnAdmission, TabSpawned};
        use std::{cell::RefCell, rc::Rc};
        for kind in [
            ConnectedSpawnKind::Controlled,
            ConnectedSpawnKind::Controller,
        ] {
            for change in ["exited", "removed", "unhosted", "unchanged"] {
                let mut app = App::headless_for_test();
                let origin = session0_sid(&app);
                let (local, host) = app
                    .connected_spawn_precheck(ConnectedSpawnPlace::Tab, &origin)
                    .unwrap();
                let host = host.unwrap();
                let original = app
                    .store
                    .read()
                    .unwrap()
                    .by_sid(&origin)
                    .unwrap()
                    .ctx
                    .clone();
                let heard = Rc::new(RefCell::new(None));
                let answer = Rc::clone(&heard);
                let then = App::connected_landing(
                    kind,
                    origin.clone(),
                    Box::new(move |_, result| {
                        *answer.borrow_mut() = Some(result);
                    }),
                );
                let id = crate::tab_spawn::tests::book_admitted(
                    &mut app,
                    host,
                    SpawnAdmission::Connected {
                        origin: origin.clone(),
                        local,
                    },
                    then,
                );
                match change {
                    "exited" => {
                        app.store
                            .write()
                            .unwrap()
                            .set_state(local, session_store::SessionState::Exited);
                    }
                    "removed" => app.retire_session_registration(local),
                    "unhosted" => {
                        app.windows.get_mut(&host).unwrap().tab_set =
                            crate::tab_model::TabSet::default();
                    }
                    _ => {}
                }
                let session = crate::stub_session(id);
                let newborn = session.ctx.self_id.clone();
                let newborn_ctx = session.ctx.clone();
                let landed = app.install_spawned_tab(TabSpawned {
                    id,
                    window: host,
                    session: Ok(session),
                    reader_gate: None,
                });
                if change == "unchanged" {
                    assert!(landed);
                    assert_eq!(*heard.borrow(), Some(Ok(newborn.as_str().to_string())));
                    let edge_ctx = if matches!(kind, ConnectedSpawnKind::Controlled) {
                        &newborn_ctx
                    } else {
                        &original
                    };
                    assert_eq!(edge_ctx.edges.lock().unwrap().edges().len(), 3);
                } else {
                    assert!(
                        !landed,
                        "{kind:?} origin {change}: no newborn may be installed"
                    );
                    assert!(heard.borrow().as_ref().unwrap().is_err());
                    assert!(app.store.read().unwrap().by_sid(&newborn).is_none());
                    assert!(app.pool.get(id).is_none());
                    assert!(original.edges.lock().unwrap().edges().is_empty());
                    assert!(newborn_ctx.edges.lock().unwrap().edges().is_empty());
                }
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_connected_window_lands_connected_only_while_its_origin_lives() {
        use crate::tab_spawn::{SpawnAdmission, TabSpawned};
        use std::{cell::RefCell, rc::Rc};
        for kind in [
            ConnectedSpawnKind::Controlled,
            ConnectedSpawnKind::Controller,
        ] {
            for change in ["exited", "removed", "unchanged"] {
                let mut app = App::headless_for_test();
                let origin = session0_sid(&app);
                let (local, _) = app
                    .connected_spawn_precheck(ConnectedSpawnPlace::Tab, &origin)
                    .unwrap();
                let windows = app.windows.len();
                let heard = Rc::new(RefCell::new(None));
                let answer = Rc::clone(&heard);
                let then = App::connected_landing(
                    kind,
                    origin.clone(),
                    Box::new(move |_, result| {
                        *answer.borrow_mut() = Some(result);
                    }),
                );
                let lineage =
                    matches!(kind, ConnectedSpawnKind::Controlled).then(|| origin.clone());
                let (id, wid) = crate::tab_spawn::test_support::book_window(
                    &mut app,
                    lineage,
                    SpawnAdmission::Connected {
                        origin: origin.clone(),
                        local,
                    },
                    then,
                );
                match change {
                    "exited" => {
                        app.store
                            .write()
                            .unwrap()
                            .set_state(local, session_store::SessionState::Exited);
                    }
                    "removed" => app.retire_session_registration(local),
                    _ => {}
                }
                let (session, _pty) = crate::tab_spawn::tests::registered_piped_session(id);
                let newborn = session.ctx.self_id.clone();
                let landed = app.install_spawned_tab(TabSpawned {
                    id,
                    window: wid,
                    session: Ok(session),
                    reader_gate: None,
                });
                if change == "unchanged" {
                    assert!(landed, "{kind:?}: the window lands");
                    assert_eq!(app.frontmost_window, Some(wid));
                    assert_eq!(app.focused_session_id(wid), Some(id));
                    assert_eq!(*heard.borrow(), Some(Ok(newborn.as_str().to_string())));
                    let store = app.store.read().unwrap();
                    let parent = store.by_sid(&newborn).unwrap().parent.clone();
                    let expected =
                        matches!(kind, ConnectedSpawnKind::Controlled).then(|| origin.clone());
                    assert_eq!(parent, expected, "{kind:?}: the newborn's lineage");
                    let dst = match kind {
                        ConnectedSpawnKind::Controlled => &newborn,
                        ConnectedSpawnKind::Controller => &origin,
                    };
                    let edges = store.by_sid(dst).unwrap().ctx.edges.lock().unwrap().edges();
                    assert_eq!(edges.len(), 3, "{kind:?}: both = pull + push = 3 ops");
                } else {
                    assert!(!landed, "{kind:?} origin {change}: no window may be made");
                    assert!(heard.borrow().as_ref().unwrap().is_err());
                    assert_eq!(app.windows.len(), windows);
                    assert!(!app.windows.contains_key(&wid));
                    assert!(app.store.read().unwrap().by_sid(&newborn).is_none());
                    assert!(app.pool.get(id).is_none());
                }
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_connected_tab_is_connected_as_it_lands() {
        use std::cell::RefCell;
        use std::rc::Rc;
        let mut app = App::headless_for_test();
        let origin = session0_sid(&app);
        let heard: Rc<RefCell<Vec<Result<String, String>>>> = Rc::default();
        let tell =
            |heard: &Rc<RefCell<Vec<Result<String, String>>>>| -> crate::tab_spawn::SpawnDone {
                let heard = Rc::clone(heard);
                Box::new(move |_, spawned| heard.borrow_mut().push(spawned))
            };
        let then =
            App::connected_landing(ConnectedSpawnKind::Controller, origin.clone(), tell(&heard));
        let id = crate::tab_spawn::test_support::book(&mut app, WindowId(0), then);
        let session = crate::stub_session(id);
        let newborn = session.ctx.self_id.clone();
        assert!(app.install_spawned_tab(crate::tab_spawn::TabSpawned {
            id,
            window: WindowId(0),
            session: Ok(session),
            reader_gate: None,
        }));
        let rows = {
            let ctx = {
                let g = app.store.read().unwrap();
                g.by_sid(&origin).unwrap().ctx.clone()
            };
            let edges = ctx.edges.lock().unwrap();
            edges.edges()
        };
        assert_eq!(rows.len(), 3, "both = pull + push = 3 ops: {rows:?}");
        assert!(
            rows.iter()
                .all(|row| row.src == newborn && row.dst == origin),
            "the newborn supervises the origin: {rows:?}"
        );

        let then = App::connected_landing(ConnectedSpawnKind::Controller, origin, tell(&heard));
        let id = crate::tab_spawn::test_support::book(&mut app, WindowId(0), then);
        assert!(!app.install_spawned_tab(crate::tab_spawn::TabSpawned {
            id,
            window: WindowId(0),
            session: Err(std::io::Error::other("exec refused")),
            reader_gate: None,
        }));
        assert_eq!(
            *heard.borrow(),
            [
                Ok(newborn.as_str().to_string()),
                Err("exec refused".to_string())
            ]
        );
    }
}

#[cfg(test)]
mod exit_attribution_tests {
    use super::*;
    use crate::CloseOutcome;
    use crate::app_window::CloseConfirm;
    use crate::session_store::{ExitReason, RosterChange};

    /// The newest close's reason and closer, from the closed index (6G-9).
    fn sole_exit(app: &App) -> (ExitReason, String) {
        let g = app.store.read().unwrap_or_else(|p| p.into_inner());
        let rec = g
            .closed_logs()
            .exits_since(0)
            .next_back()
            .expect("one exit record after the close");
        (rec.reason, rec.actor.as_wire().to_string())
    }

    fn exit_rows(app: &App) -> usize {
        let g = app.store.read().unwrap_or_else(|p| p.into_inner());
        g.roster_since(0)
            .filter(|r| r.change == RosterChange::Exited)
            .count()
    }

    fn ctl_close(app: &mut App, session: u64) -> Result<CloseProgress, String> {
        let _closing = session_store::CloseAttribution::enter(
            ExitReason::CtlClose,
            session_store::ExitActor::Unknown,
        );
        app.close_confirm = CloseConfirm::WireRefuseBusy;
        let progress = app.close_session_by_id(session);
        app.close_confirm = CloseConfirm::Interactive;
        progress
    }

    fn start_in_shell_job(app: &App, session: u64) {
        feed(
            app,
            session,
            b"\x1b]133;A\x07PS> \x1b]133;B\x07Start-Sleep 60\n\x1b]133;C\x07",
        );
    }

    fn finish_job(app: &App, session: u64) {
        feed(app, session, b"\x1b]133;D;0\x07");
    }

    use crate::feed_session_for_test as feed;

    fn grid(app: &App, session: u64) -> (u16, u16) {
        let term = app.pool.get(session).expect("live session").term.clone();
        let t = term.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        (t.rows(), t.cols())
    }

    fn draw_alt_frame(app: &App, session: u64) {
        let (rows, _) = grid(app, session);
        let mut bytes = b"\x1b[?1049h\x1b[2J\x1b[H".to_vec();
        for r in 0..rows {
            if r > 0 {
                bytes.extend_from_slice(b"\r\n");
            }
            bytes.extend_from_slice(format!("ROW{r}").as_bytes());
        }
        feed(app, session, &bytes);
    }

    #[test]
    fn a_split_closed_at_once_holds_the_siblings_take_back_for_the_quiet() {
        use crate::split_regrid_hold::SPLIT_REGRID_QUIET;
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let full = grid(&app, 0);
        draw_alt_frame(&app, 0);

        let new = app.split_active_stub_tab_dir(wid, pane::SplitDir::Horizontal);
        let shrunk = grid(&app, 0);
        assert!(
            shrunk.0 < full.0 && shrunk.1 == full.1,
            "the split shrinks the sibling at once: {full:?} -> {shrunk:?}"
        );
        feed(&app, 0, b"\x1b[1;1Hx");

        let progress = ctl_close(&mut app, new).expect("the close proceeds");
        assert_eq!(progress, CloseProgress::Retired);
        assert_eq!(
            app.windows[&wid].layouts[0].sessions(),
            vec![0],
            "the scratch pane is gone; its sibling fills the tab"
        );
        assert_eq!(
            grid(&app, 0),
            shrunk,
            "the take-back is withheld inside the quiet: no N -> N/2 -> N pair"
        );
        app.resize_panes(wid);
        assert_eq!(grid(&app, 0), shrunk, "a re-lay inside the quiet holds");
        let due = app
            .split_regrid_hold_deadline()
            .expect("a withheld take-back owes one wake");

        app.service_split_regrid_holds(due - std::time::Duration::from_millis(1));
        assert_eq!(grid(&app, 0), shrunk);
        app.service_split_regrid_holds(due);
        assert_eq!(grid(&app, 0), full, "the take-back lands after the quiet");
        assert_eq!(app.split_regrid_hold_deadline(), None);
        assert!(due <= std::time::Instant::now() + SPLIT_REGRID_QUIET);
    }

    #[test]
    fn the_split_hold_withholds_only_the_take_back_inside_the_quiet() {
        use crate::split_regrid_hold::SPLIT_REGRID_QUIET;
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let full = grid(&app, 0);

        let new = app.split_active_stub_tab_dir(wid, pane::SplitDir::Horizontal);
        let _ = ctl_close(&mut app, new).expect("the close proceeds");
        assert_ne!(grid(&app, 0), full, "held");
        let bigger = (full.0 + 4, full.1 + 10);
        assert!(app.apply_term_resize(wid, bigger.0, bigger.1));
        assert_eq!(
            grid(&app, 0),
            bigger,
            "a different target applies at once and ends the hold"
        );
        assert_eq!(app.split_regrid_hold_deadline(), None);

        let new = app.split_active_stub_tab_dir(wid, pane::SplitDir::Vertical);
        assert_ne!(grid(&app, 0), bigger, "the vertical split narrows it");
        app.split_regrid_holds.age(SPLIT_REGRID_QUIET);
        let _ = ctl_close(&mut app, new).expect("the close proceeds");
        assert_eq!(
            grid(&app, 0),
            bigger,
            "past the quiet the take-back is immediate"
        );
        assert_eq!(app.split_regrid_hold_deadline(), None);
    }

    #[test]
    fn the_newborn_pane_of_a_split_is_never_held() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let full = grid(&app, 0);
        let new = app.split_active_stub_tab_dir(wid, pane::SplitDir::Horizontal);
        assert!(grid(&app, new).0 < full.0, "the newborn has half the rect");
        assert!(app.split_regrid_holds.get(new).is_none(), "no hold on it");

        let progress = ctl_close(&mut app, 0).expect("the close proceeds");
        assert_eq!(progress, CloseProgress::Retired);
        assert_eq!(
            grid(&app, new),
            full,
            "the newborn grows to the whole rect at once"
        );
        assert_eq!(app.split_regrid_hold_deadline(), None);
    }

    #[test]
    fn a_zoom_inside_the_quiet_is_not_withheld() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let full = grid(&app, 0);

        let new = app.split_active_stub_tab_dir(wid, pane::SplitDir::Horizontal);
        let shrunk = grid(&app, 0);
        app.toggle_pane_zoom();
        assert_eq!(grid(&app, new), full, "the newborn's zoom applies at once");
        app.toggle_pane_zoom();

        assert_eq!(grid(&app, 0), shrunk);
        assert!(
            app.split_regrid_holds.get(0).is_some(),
            "the split armed it"
        );
        assert!(
            app.active_tree_mut(wid)
                .is_some_and(|tree| tree.set_focus(0))
        );
        app.toggle_pane_zoom();
        assert_eq!(grid(&app, 0), full, "the original's zoom applies at once");
        assert!(app.split_regrid_holds.get(0).is_none(), "and ends the hold");
        assert_eq!(app.split_regrid_hold_deadline(), None);
        app.toggle_pane_zoom();
        assert_eq!(grid(&app, 0), shrunk, "unzoom restores the split");
    }

    #[test]
    fn a_ctl_close_on_one_pane_of_a_split_retires_only_that_pane() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let a = app.split_active_stub_tab(wid);
        let b = app.split_active_stub_tab_dir(wid, pane::SplitDir::Horizontal);
        assert_eq!(app.windows[&wid].layouts[0].sessions(), vec![0, a, b]);
        assert_eq!(
            app.windows[&wid].tab_set.len(),
            1,
            "one tab, split three ways"
        );

        let stable = |app: &App, local| {
            app.store
                .read()
                .unwrap()
                .by_local(local)
                .unwrap()
                .sid
                .clone()
        };
        let closed = stable(&app, a);
        let sibling = stable(&app, b);
        for sid in [&closed, &sibling] {
            app.note_harness_host(crate::harness_host::HostNote::ResumeException {
                sid: sid.as_str().to_string(),
                agent: aterm_phase::Program::Claude,
                standing: Some(
                    aterm_agent::supervise::policy::exception::Exception::of_notice(
                        &aterm_agent::supervise::policy::notice::LOGIN_SIGN_IN,
                        Some("main"),
                        None,
                        "the agent working",
                    ),
                ),
            });
        }
        let records: Vec<_> = [&closed, &sibling]
            .into_iter()
            .map(|sid| {
                let key = crate::message_reporters::notice_ask_key(sid.as_str(), Some("si"));
                let id = app.messages.live_by_key(&key).expect("its notice").id;
                app.messages.log().get(id).unwrap().clone()
            })
            .collect();
        start_in_shell_job(&app, a);
        let headless = std::mem::replace(&mut app.headless, false);
        assert_eq!(
            ctl_close(&mut app, a),
            Err(WIRE_PANE_CLOSE_REFUSED.to_string())
        );
        app.headless = headless;
        finish_job(&app, a);
        app.teardown_session(u64::MAX);
        app.retire_session_registration(u64::MAX);
        assert!(app.resume_stories.contains_key(closed.as_str()));
        assert!(app.resume_stories.contains_key(sibling.as_str()));

        let progress = ctl_close(&mut app, a).expect("the close proceeds");
        assert_eq!(
            progress,
            CloseProgress::Retired,
            "a pane close retires inside the call: the tab stands, nothing defers"
        );
        assert_eq!(app.close_session_verdict(a, progress), Ok(()));

        assert_eq!(app.windows[&wid].tab_set.len(), 1, "the tab is still there");
        assert!(
            !app.windows[&wid].pending_close,
            "…and its window is not closing"
        );
        assert_eq!(
            app.windows[&wid].layouts[0].sessions(),
            vec![0, b],
            "the aimed pane went; both siblings stayed"
        );
        assert!(
            !app.session_registered(a),
            "the aimed session left the registry"
        );
        assert!(
            app.session_registered(0) && app.session_registered(b),
            "the siblings' sessions still answer"
        );
        assert!(
            app.pool.get(0).is_some() && app.pool.get(b).is_some(),
            "…with their PTYs: nothing else was hung up"
        );
        assert_eq!(exit_rows(&app), 1, "one close, ONE exit row — not three");
        let (reason, _by) = sole_exit(&app);
        assert_eq!(reason, ExitReason::CtlClose);
        assert!(!app.resume_stories.contains_key(closed.as_str()));
        assert!(app.resume_stories.contains_key(sibling.as_str()));
        for record in records {
            assert_eq!(
                app.messages.log().get(record.id),
                Some(&record),
                "retirement forgets the process-local story, not its notice or outcome"
            );
        }
        assert!(app.structural_invariants_ok());
    }

    #[test]
    fn a_ctl_close_aims_at_the_pane_not_at_the_focus_or_the_active_tab() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let a = app.split_active_stub_tab(wid);
        assert_eq!(app.windows[&wid].layouts[0].focus(), a);
        let c = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(c));
        assert_eq!(app.windows[&wid].tab_set.active_index(), Some(1));

        let progress = ctl_close(&mut app, 0).expect("the close proceeds");
        assert_eq!(progress, CloseProgress::Retired);

        assert_eq!(
            app.windows[&wid].tab_set.len(),
            2,
            "the background tab survives its collapse"
        );
        assert_eq!(app.windows[&wid].layouts[0].sessions(), vec![a]);
        assert_eq!(
            app.windows[&wid].layouts[0].focus(),
            a,
            "focus stays on the survivor it already had"
        );
        assert_eq!(
            app.windows[&wid].tab_set.active_index(),
            Some(1),
            "the active tab did not move"
        );
        assert!(!app.session_registered(0));
        assert!(app.structural_invariants_ok());
    }

    #[test]
    fn a_ctl_close_of_a_busy_pane_is_refused_and_a_busy_sibling_never_refuses_one() {
        let mut app = App::headless_for_test();
        app.headless = false;
        let wid = WindowId(0);
        let a = app.split_active_stub_tab(wid);
        start_in_shell_job(&app, a);
        assert!(app.pane_close_hangs_up_a_job(a));
        assert!(
            !app.pane_close_hangs_up_a_job(0),
            "the idle sibling is idle"
        );

        assert_eq!(
            ctl_close(&mut app, a),
            Err(WIRE_PANE_CLOSE_REFUSED.to_string()),
            "the busy pane is refused in the pane's words"
        );
        assert_eq!(
            app.windows[&wid].layouts[0].sessions(),
            vec![0, a],
            "nothing collapsed"
        );
        assert!(
            app.session_registered(a) && app.pool.get(a).is_some(),
            "the busy session was not retired"
        );
        assert_eq!(exit_rows(&app), 0);

        assert_eq!(
            ctl_close(&mut app, 0),
            Ok(CloseProgress::Retired),
            "the idle pane closes although its neighbour is busy"
        );
        assert_eq!(app.windows[&wid].layouts[0].sessions(), vec![a]);

        assert_eq!(
            ctl_close(&mut app, a),
            Err(crate::app_window::WIRE_CLOSE_REFUSED.to_string()),
            "alone in the last tab, the refusal is the last-tab confirm's"
        );
        finish_job(&app, a);
        assert!(!app.pane_close_hangs_up_a_job(a));
        assert_eq!(
            ctl_close(&mut app, a),
            Ok(CloseProgress::Deferred(wid)),
            "idle again, the last-tab close defers as it always did"
        );
        assert!(app.windows[&wid].pending_close);
    }

    #[test]
    fn a_ctl_close_of_a_busy_tab_that_is_not_the_last_is_refused() {
        let mut app = App::headless_for_test();
        app.headless = false;
        let wid = WindowId(0);
        let c = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(c));
        start_in_shell_job(&app, c);

        assert_eq!(
            ctl_close(&mut app, c),
            Err(WIRE_PANE_CLOSE_REFUSED.to_string())
        );
        assert_eq!(app.windows[&wid].tab_set.len(), 2, "both tabs stand");
        assert!(app.pool.get(c).is_some(), "the job's PTY was not hung up");
        assert_eq!(exit_rows(&app), 0);

        finish_job(&app, c);
        assert_eq!(ctl_close(&mut app, c), Ok(CloseProgress::Retired));
        assert_eq!(app.windows[&wid].tab_set.len(), 1);
        assert!(!app.windows[&wid].pending_close);
        assert!(app.structural_invariants_ok());
    }

    #[test]
    fn a_ctl_close_of_a_busy_terminal_leaf_in_a_mixed_tab_is_refused() {
        let mut app = App::headless_for_test();
        app.headless = false;
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));
        let (_instance, native_view) = app.active_native_view(wid).unwrap();
        let (session, terminal_view) =
            app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
        let tab_id = app.windows[&wid].tab_set.active_id().unwrap();
        start_in_shell_job(&app, session);

        assert_eq!(
            ctl_close(&mut app, session),
            Err(WIRE_PANE_CLOSE_REFUSED.to_string())
        );
        assert_eq!(
            app.windows[&wid].tab_set.get(tab_id).unwrap().root.leaves(),
            vec![native_view, terminal_view],
            "nothing collapsed"
        );
        assert!(app.pool.get(session).is_some());

        finish_job(&app, session);
        assert_eq!(ctl_close(&mut app, session), Ok(CloseProgress::Retired));
        assert_eq!(
            app.windows[&wid].tab_set.get(tab_id).unwrap().root.leaves(),
            vec![native_view],
            "idle, only the terminal leaf goes; the native sibling keeps the tab"
        );
        assert!(app.pool.get(session).is_none() && !app.session_registered(session));
        assert!(!app.windows[&wid].pending_close);
        assert!(app.structural_invariants_ok());
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn a_busy_tab_that_is_not_the_last_asks_before_it_closes() {
        use crate::close_confirm::{CloseReplay, kind};
        let mut app = App::headless_for_test();
        app.headless = false;
        app.close_confirm = CloseConfirm::Interactive;
        let wid = WindowId(0);
        let first = app.focused_session_id(wid).expect("tab 0");
        app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
        app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
        let busy = app.focused_session_id(wid).expect("tab 2, active");
        start_in_shell_job(&app, busy);
        let tabs = |app: &App| app.windows[&wid].tab_set.len();

        assert!(app.close_active_tab().is_none());
        assert_eq!(tabs(&app), 3, "the busy tab stays while it asks");
        let pending = app.close_banner.as_ref().expect("asked in the window");
        assert!(matches!(pending.replay, CloseReplay::Tab { .. }));
        assert_eq!(
            kind(&pending.prompt),
            "close-tab",
            "its one pane is the tab"
        );
        let replay = app.answer_close_banner(true).expect("Enter replays");
        app.close_confirm = CloseConfirm::Programmatic;
        assert_eq!(app.replay_confirmed_view_close(replay), None);
        app.close_confirm = CloseConfirm::Interactive;
        assert_eq!(tabs(&app), 2, "the answer closed it");
        assert!(!app.session_registered(busy), "and its session");

        start_in_shell_job(&app, first);
        assert!(!app.close_tab_at(wid, 0));
        let pending = app.close_banner.as_ref().expect("asked again");
        assert_eq!(kind(&pending.prompt), "close-tab");
        assert!(matches!(pending.replay, CloseReplay::Tab { wid: w, .. } if w == wid));
        assert_eq!(app.answer_close_banner(false), None);
        assert_eq!(tabs(&app), 2, "Escape kept it");

        let pane = app.split_active_stub_tab(wid);
        start_in_shell_job(&app, pane);
        assert!(app.close_active_tab().is_none());
        let pending = app.close_banner.as_ref().expect("the pane asks");
        assert_eq!(kind(&pending.prompt), "close-pane");
        let replay = app.answer_close_banner(true).expect("Enter replays");
        app.close_confirm = CloseConfirm::Programmatic;
        assert_eq!(app.replay_confirmed_view_close(replay), None);
        app.close_confirm = CloseConfirm::Interactive;
        assert!(!app.session_registered(pane), "the pane closed");
        assert_eq!(tabs(&app), 2, "and its tab stayed");

        app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
        assert!(app.close_active_tab().is_none());
        assert!(app.close_banner.is_none());
        assert_eq!(tabs(&app), 2, "an idle tab closes at once");
    }

    #[cfg(not(target_os = "macos"))]
    fn exit_of(app: &App, session: u64) -> (ExitReason, String) {
        let g = app.store.read().unwrap_or_else(|p| p.into_inner());
        let rec = g
            .closed_logs()
            .exits_since(0)
            .rfind(|e| e.local_id == session)
            .expect("an exit record for the session");
        (rec.reason, rec.actor.as_wire().to_string())
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn a_confirmed_busy_tab_close_leaves_no_attribution_behind() {
        let mut app = App::headless_for_test();
        app.headless = false;
        app.close_confirm = CloseConfirm::Interactive;
        let wid = WindowId(0);
        let first = app.focused_session_id(wid).expect("tab 0");
        app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
        let busy = app.focused_session_id(wid).expect("tab 1, active");
        start_in_shell_job(&app, busy);

        assert!(app.close_active_tab().is_none(), "it asks");
        let replay = app.answer_close_banner(true).expect("Enter replays");
        app.close_confirm = CloseConfirm::Programmatic;
        assert_eq!(app.replay_confirmed_view_close(replay), None);
        app.close_confirm = CloseConfirm::Interactive;
        assert!(!app.session_registered(busy), "the answer closed it");
        assert_eq!(exit_of(&app, busy), (ExitReason::UiClose, "human".into()));
        assert!(
            app.windows[&wid].pending_close_attribution.is_none(),
            "the replay consumed the question's attribution"
        );

        app.store
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .set_state(first, crate::session_store::SessionState::Exited);
        assert_eq!(app.close_window_logical(wid), CloseOutcome::Exit);
        assert_eq!(exit_of(&app, first), (ExitReason::ShellExit, "-".into()));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn a_wire_confirmed_busy_tab_close_journals_ctl_close() {
        let mut app = App::headless_for_test();
        app.headless = false;
        app.close_confirm = CloseConfirm::Interactive;
        let wid = WindowId(0);
        app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
        let busy = app.focused_session_id(wid).expect("tab 1, active");
        start_in_shell_job(&app, busy);

        let raised = app.invoke_attributed(crate::session_store::ExitActor::Ctl, |app| {
            Ok(format!("{:?}", app.close_active_tab()))
        });
        assert_eq!(raised.as_deref(), Ok("None"), "the close waits");
        let replay = app
            .close_banner
            .as_ref()
            .expect("asked in the window")
            .replay;
        assert_eq!(
            app.answer_close_confirm_from_wire(true),
            Ok("answered=yes kind=close-tab".to_string())
        );
        app.close_confirm = CloseConfirm::Programmatic;
        assert_eq!(app.replay_confirmed_view_close(replay), None);
        app.close_confirm = CloseConfirm::Interactive;
        assert!(!app.session_registered(busy), "the answer closed it");
        assert_eq!(exit_of(&app, busy), (ExitReason::CtlClose, "ctl".into()));
        assert!(app.windows[&wid].pending_close_attribution.is_none());
    }

    #[test]
    fn a_ctl_close_of_a_co_viewed_pane_collapses_it_and_names_the_other_viewer() {
        let mut app = App::headless_for_test();
        app.headless = false;
        let wid = WindowId(0);
        let a = app.split_active_stub_tab(wid);
        let second = app
            .open_active_session_in_new_window_logical()
            .expect("Cmd-Shift-O opens a second viewer");
        assert!(app.window_contains_session(second, a));
        start_in_shell_job(&app, a);
        assert!(
            !app.pane_close_hangs_up_a_job(a),
            "a job with another viewer is not this pane's to hang up"
        );

        assert_eq!(
            ctl_close(&mut app, a),
            Err("close deferred (another window still displays this session)".to_string())
        );
        assert_eq!(
            app.windows[&wid].layouts[0].sessions(),
            vec![0],
            "the pane collapsed here"
        );
        assert!(
            app.session_registered(a),
            "…and the co-viewer keeps the session"
        );
        assert!(app.window_contains_session(second, a));
        assert_eq!(
            exit_rows(&app),
            0,
            "nothing exited: a view was detached, not a shell"
        );
    }

    #[test]
    fn a_ctl_close_of_a_co_viewed_tab_names_the_other_viewer_not_a_refusal() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let c = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(c));
        let second = app
            .open_active_session_in_new_window_logical()
            .expect("Cmd-Shift-O opens a second viewer");
        assert!(app.window_contains_session(second, c));

        let result = ctl_close(&mut app, c);
        assert_eq!(
            result,
            Err("close deferred (another window still displays this session)".to_string())
        );
        assert_eq!(app.windows[&wid].tab_set.len(), 1, "the tab went here");
        assert!(app.window_contains_session(second, c));
        assert!(app.session_registered(c));
    }

    #[test]
    fn a_ctl_close_of_a_pane_twinned_in_the_same_window_names_the_other_tab() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let a = app.split_active_stub_tab(wid);
        let shared = app
            .open_active_session_in_new_window_logical()
            .expect("Cmd-Shift-O shares a");
        assert!(app.window_contains_session(shared, a));
        let _ = app.migrate_active_tab_to_next_window();
        assert_eq!(app.windows.len(), 1, "the share window emptied and closed");
        assert_eq!(app.windows[&wid].tab_set.len(), 2, "tab 1 is a's twin");

        assert_eq!(
            ctl_close(&mut app, a),
            Err("close deferred (another tab still displays this session)".to_string())
        );
        assert_eq!(
            app.windows[&wid].layouts[0].sessions(),
            vec![0],
            "the split pane collapsed"
        );
        assert!(
            app.window_contains_session(wid, a),
            "its twin still shows it"
        );
        assert!(app.session_registered(a));
        assert!(app.structural_invariants_ok());
    }

    #[test]
    fn a_ctl_closed_pane_leaves_the_reopen_record_cmd_w_leaves() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let a = app.split_active_stub_tab(wid);
        app.focus_pane(pane::FocusDir::Left);
        assert_eq!(
            app.windows[&wid].layouts[0].focus(),
            0,
            "the aim is unfocused"
        );
        assert!(!app.can_reopen_closed_view(), "nothing closed yet");

        assert_eq!(ctl_close(&mut app, a), Ok(CloseProgress::Retired));

        assert!(
            app.can_reopen_closed_view(),
            "the unfocused pane's record was kept"
        );
    }

    #[test]
    fn a_last_tab_ctl_close_journals_ctl_close() {
        let mut app = App::headless_for_test();
        app.close_tab_via_verb(WindowId(0), None);
        assert!(
            app.windows[&WindowId(0)].pending_close,
            "closing the only tab defers the window close"
        );
        assert_eq!(app.close_window_logical(WindowId(0)), CloseOutcome::Exit);
        let (reason, _by) = sole_exit(&app);
        assert_eq!(reason, ExitReason::CtlClose);
    }

    #[test]
    fn a_last_tab_ctl_close_is_deferred_not_refused_and_answers_after_the_teardown() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let session = 0;
        let progress = {
            let _closing = session_store::CloseAttribution::enter(
                session_store::ExitReason::CtlClose,
                session_store::ExitActor::Unknown,
            );
            app.close_session_by_id(session)
                .expect("the close proceeds")
        };
        assert_eq!(
            progress,
            CloseProgress::Deferred(wid),
            "a last-tab close defers; it is not a refusal"
        );
        assert!(app.windows[&wid].pending_close, "the window is flagged");
        assert_eq!(
            app.close_session_verdict(session, progress),
            Err("close deferred (the window teardown has not run)".to_string()),
            "before the escalation the honest answer names the deferral"
        );

        assert_eq!(app.close_window_logical(wid), CloseOutcome::Exit);
        assert_eq!(
            app.close_session_verdict(session, progress),
            Ok(()),
            "the session is gone: the verb answers OK closed"
        );
        let (reason, _by) = sole_exit(&app);
        assert_eq!(reason, ExitReason::CtlClose);
    }

    #[test]
    fn a_wire_close_never_reaches_the_dialog_and_refuses_only_a_busy_job() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        for (policy, headless, busy, proceeds) in [
            (CloseConfirm::WireRefuseBusy, false, false, true),
            (CloseConfirm::WireRefuseBusy, false, true, false),
            (CloseConfirm::Programmatic, false, true, true),
            (CloseConfirm::WireRefuseBusy, true, true, true),
        ] {
            app.close_confirm = policy;
            app.headless = headless;
            let case = format!("{policy:?} headless={headless} busy={busy}");
            assert_eq!(
                app.confirm_destructive_close(wid, true, busy),
                proceeds,
                "{case}: the last-tab confirm"
            );
            assert_eq!(
                app.pane_close_allowed(busy),
                proceeds,
                "{case}: the pane guard"
            );
        }
        app.headless = false;
        app.close_confirm = CloseConfirm::Interactive;
        assert!(
            app.pane_close_allowed(true),
            "a person's close is not the wire guard's"
        );
    }

    #[test]
    fn a_deferred_close_of_a_co_viewed_session_names_the_other_window() {
        let mut app = App::headless_for_test();
        let session = 0;
        let second = app
            .open_active_session_in_new_window_logical()
            .expect("Cmd-Shift-O opens a second viewer");
        assert!(app.window_contains_session(second, session));

        let progress = {
            let _closing = session_store::CloseAttribution::enter(
                session_store::ExitReason::CtlClose,
                session_store::ExitActor::Unknown,
            );
            app.close_session_by_id(session)
                .expect("the close proceeds")
        };
        assert_eq!(
            progress,
            CloseProgress::Deferred(WindowId(0)),
            "the close aims at the LOWEST window id holding the session"
        );
        assert_eq!(
            app.close_session_verdict(session, progress),
            Err("close deferred (another window still displays this session)".to_string()),
            "the co-viewer is the reason the close cannot take, from the start"
        );

        assert_eq!(app.close_window_logical(WindowId(0)), CloseOutcome::Stay);
        assert!(!app.windows.contains_key(&WindowId(0)), "window 0 is gone");
        assert!(
            app.session_registered(session),
            "the co-viewer's pool view keeps the session registered"
        );
        assert_eq!(
            app.close_session_verdict(session, progress),
            Err("close deferred (another window still displays this session)".to_string()),
            "the teardown DID run: the co-viewer is why the session is still here"
        );
    }

    #[test]
    fn a_wire_close_question_keeps_its_name_through_a_settings_open() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let prompt =
            crate::quit_safety::confirm_prompt(true, true, true).expect("a busy quit prompts");
        let raised = app.invoke_attributed(crate::session_store::ExitActor::Ctl, |app| {
            Ok(app
                .present_close_banner(wid, prompt, crate::close_confirm::CloseReplay::Window(wid))
                .to_string())
        });
        assert_eq!(
            raised.as_deref(),
            Ok("false"),
            "the gesture waits for its answer"
        );
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        assert!(!app.windows[&wid].pending_close, "nothing was parked");
        assert_eq!(
            app.windows[&wid].pending_close_attribution,
            Some((ExitReason::CtlClose, crate::session_store::ExitActor::Ctl)),
            "the standing question's attribution survives the Settings open"
        );
        assert_eq!(
            app.answer_close_confirm_from_wire(true),
            Ok("answered=yes kind=quit".to_string())
        );
        assert_eq!(app.close_window_logical(wid), CloseOutcome::Exit);
        let (reason, by) = sole_exit(&app);
        assert_eq!(reason, ExitReason::CtlClose);
        assert_eq!(by, "ctl");
    }

    #[test]
    fn a_last_tab_ui_close_journals_ui_close_by_human() {
        let mut app = App::headless_for_test();
        let closed = app
            .close_active_tab()
            .expect("the frontmost's last tab closed");
        assert_eq!(closed, WindowId(0));
        assert_eq!(app.close_window_logical(WindowId(0)), CloseOutcome::Exit);
        let (reason, by) = sole_exit(&app);
        assert_eq!(reason, ExitReason::UiClose);
        assert_eq!(by, "human");
    }
}

#[cfg(test)]
mod pane_close_render_seam_tests {
    use super::*;

    fn assert_window_keeps_rendering(app: &mut App, wid: WindowId, what: &str) {
        let unresolved: Vec<_> = app.windows[&wid]
            .tab_set
            .tabs()
            .iter()
            .flat_map(|tab| tab.root.leaves())
            .filter(|view| app.view_store.get(*view).is_none())
            .collect();
        assert!(
            unresolved.is_empty(),
            "{what}: canonical tree still names dead view(s) {unresolved:?}"
        );
        let plan = app
            .active_visible_leaf_plan(wid)
            .unwrap_or_else(|| panic!("{what}: the active tab has no layout plan to paint"));
        assert!(!plan.leaves.is_empty(), "{what}: nothing to paint");
        assert!(
            app.active_visible_content_route(wid).is_some(),
            "{what}: no visible content route — redraw_window returns before any present"
        );
        assert!(
            app.structural_invariants_ok(),
            "{what}: structural invariants"
        );
    }

    fn pane_sessions(app: &App, wid: WindowId) -> Vec<u64> {
        app.windows[&wid].layouts[app.windows[&wid].tabs.active].sessions()
    }

    fn canonical_leaves(app: &App, wid: WindowId) -> Vec<crate::tab_model::ViewId> {
        app.windows[&wid]
            .tab_set
            .active()
            .expect("active tab")
            .root
            .leaves()
    }

    #[test]
    fn closing_a_pane_keeps_the_window_rendering() {
        type Setup = fn(&mut App, WindowId) -> Vec<u64>;
        let cases: [(&str, Setup); 4] = [
            ("the focused split pane", |app, wid| {
                let closed = app.split_active_stub_tab(wid);
                assert_eq!(pane_sessions(app, wid), vec![0, closed]);
                vec![0]
            }),
            ("the unfocused original pane", |app, wid| {
                let survivor = app.split_active_stub_tab(wid);
                app.focus_pane(pane::FocusDir::Left);
                assert_eq!(app.windows[&wid].layouts[0].focus(), 0);
                vec![survivor]
            }),
            ("a pane whose sibling is a split", |app, wid| {
                let a = app.split_active_stub_tab(wid);
                let b = app.split_active_stub_tab_dir(wid, pane::SplitDir::Horizontal);
                app.focus_pane(pane::FocusDir::Left);
                assert_eq!(app.windows[&wid].layouts[0].focus(), 0);
                assert_eq!(pane_sessions(app, wid), vec![0, a, b]);
                vec![a, b]
            }),
            ("a tab's last pane beside another tab", |app, wid| {
                let second = app.next_session_id;
                app.push_stub_tab(wid, crate::stub_session(second));
                vec![0]
            }),
        ];
        for (what, setup) in cases {
            let mut app = App::headless_for_test();
            let wid = WindowId(0);
            let survivors = setup(&mut app, wid);
            assert_eq!(app.close_active_tab(), None, "{what}: the window stays");
            assert_eq!(pane_sessions(&app, wid), survivors, "{what}");
            assert_eq!(
                canonical_leaves(&app, wid).len(),
                survivors.len(),
                "{what}: the canonical tree shrank with the pane tree"
            );
            assert_window_keeps_rendering(&mut app, wid, what);
        }
    }

    #[test]
    fn pane_focus_reports_only_whether_focus_actually_moved() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);

        for dir in [
            pane::FocusDir::Left,
            pane::FocusDir::Right,
            pane::FocusDir::Up,
            pane::FocusDir::Down,
        ] {
            assert!(
                !app.focus_pane_in(wid, dir),
                "a single-pane tab reports no move"
            );
        }

        let split = app.split_active_stub_tab(wid);
        assert_eq!(app.windows[&wid].layouts[0].focus(), split);
        assert!(
            app.focus_pane_in(wid, pane::FocusDir::Left),
            "there is a pane to the left of the new one"
        );
        assert_eq!(
            app.windows[&wid].layouts[0].focus(),
            0,
            "moved=1 means the focused pane really changed"
        );
        assert!(!app.focus_pane_in(wid, pane::FocusDir::Left));
        assert_eq!(
            app.windows[&wid].layouts[0].focus(),
            0,
            "moved=0 means the focused pane really did not change"
        );
        assert!(app.focus_pane_in(wid, pane::FocusDir::Right));
        assert_eq!(app.windows[&wid].layouts[0].focus(), split);
    }

    #[test]
    fn a_close_that_did_not_shrink_the_tree_refuses_to_retire_the_view() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let closed = app.split_active_stub_tab(wid);
        let before = canonical_leaves(&app, wid);
        assert_eq!(before.len(), 2);

        let refused = app.apply_close_outcome(wid, 0, pane::CloseOutcome::Collapsed { closed });

        assert!(!refused, "a refused close never closes the window");
        assert_eq!(
            canonical_leaves(&app, wid),
            before,
            "topology is untouched by a close that did not happen"
        );
        assert!(
            app.pool.get(closed).is_some(),
            "the session keeps its PTY: nothing was torn down"
        );
        assert_window_keeps_rendering(&mut app, wid, "after a refused pane close");
    }
}

#[cfg(all(test, unix))]
mod shell_start_exit_tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn run_to_zombie(script: &str) -> i32 {
        let child = aterm_uds::spawnfd::command("/bin/sh")
            .args(["-c", script])
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .stderr(aterm_uds::spawnfd::Stdio::null())
            .spawn()
            .expect("spawn the stand-in shell");
        let pid = i32::try_from(child.id()).expect("pid fits");
        std::mem::forget(child);
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            // SAFETY: a zeroed `siginfo_t` out-param for a non-reaping peek.
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            // SAFETY: `WNOWAIT` leaves the child waitable; `WNOHANG` never blocks.
            let rc = unsafe {
                libc::waitid(
                    libc::P_PID,
                    pid as libc::id_t,
                    &mut info,
                    libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
                )
            };
            assert_eq!(rc, 0, "waitid on our own child");
            // SAFETY: `waitid` either filled a `CLD_*` siginfo or, under `WNOHANG` with nothing waitable, left it zeroed; `si_pid` is defined in both.
            let exited = unsafe { info.si_pid() };
            if exited == pid {
                return pid;
            }
            assert!(Instant::now() < deadline, "the stand-in shell did not exit");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    type Start = Option<(&'static str, Instant)>;

    fn started(name: &'static str, at: Instant) -> Start {
        Some((name, at))
    }

    fn eof_at_start(at: Instant) -> Option<Instant> {
        Some(at + Duration::from_millis(100))
    }

    fn app_with(pid: i32, start: Start, closed: Option<Instant>) -> (App, WindowId, u64) {
        let sink = std::sync::Arc::new(aterm_session::sink::SinkWriter::new(-1));
        app_with_sink(pid, start, closed, sink)
    }

    fn app_with_sink(
        pid: i32,
        start: Start,
        closed: Option<Instant>,
        sink: std::sync::Arc<aterm_session::sink::SinkWriter>,
    ) -> (App, WindowId, u64) {
        let mut app = App::headless_for_test();
        let id = app.next_session_id;
        let mut session = crate::stub_session_with_sink(id, sink);
        session.pid = pid;
        session.shell_identity = aterm_pty::record_spawned_shell(pid);
        session.shell_start = start.map(|(name, at)| crate::spawn::ShellStart {
            at,
            name: name.to_string(),
            input: session.ctx.sink.driven_input(),
        });
        if let Some(at) = closed {
            let _ = session.pty_closed_at.set(at);
        }
        App::register_session(&app.store, &session, None);
        let wid = app.insert_logical_window(session, 24, 80);
        (app, wid, id)
    }

    fn ledger_code(app: &App, session: u64) -> Option<i32> {
        app.store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .noted_exit_code(session)
    }

    fn reader_eof(app: &mut App, session: u64) -> Vec<WindowId> {
        app.note_session_exit(session);
        app.retire_input_watch(session);
        app.exit_session_logical(session)
    }

    fn pane_text(app: &App, session: u64) -> String {
        term_lock(&app.pool.get(session).expect("the pane's session").term).visible_content()
    }

    fn log_close_due(app: &App, session: u64) -> bool {
        app.pool.get(session).is_some_and(|s| {
            s.ctx
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .slog()
                .close_due()
        })
    }

    fn assert_kept(app: &App, wid: WindowId, session: u64, to_close: &[WindowId], line: &str) {
        assert!(to_close.is_empty(), "nothing closes: {to_close:?}");
        assert!(
            app.window_contains_session(wid, session),
            "the pane stays in its window"
        );
        assert!(
            pane_text(app, session).contains(line),
            "the pane says why: {:?}",
            pane_text(app, session)
        );
        let reaped = app
            .pool
            .get(session)
            .expect("still pooled")
            .child_reaped
            .load(std::sync::atomic::Ordering::Acquire);
        assert!(reaped, "the exit path reaped the child");
        assert_eq!(
            app.store
                .read()
                .unwrap_or_else(|p| p.into_inner())
                .by_local(session)
                .map(|h| h.state),
            Some(session_store::SessionState::Exited),
            "the registry still says the session exited"
        );
        assert!(
            !log_close_due(app, session),
            "the log knows no close is coming"
        );
    }

    fn parked_close_wait(app: &App, session: u64) -> std::sync::mpsc::Receiver<String> {
        let pooled = app.pool.get(session).expect("the pane's session");
        let (term, ctx) = (pooled.term.clone(), std::sync::Arc::clone(&pooled.ctx));
        let (ep, head) = {
            let tl = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            (tl.slog().log().ep(), tl.slog().log().last_seq())
        };
        let store = app.store.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(crate::control::cmd_await(
                &term,
                &store,
                session,
                &ctx,
                &format!("log close=session since={head} ep={ep} timeout=600000"),
            ));
        });
        rx
    }

    #[test]
    fn an_escalated_exit_keeps_the_close_due_until_its_window_closes() {
        let pid = run_to_zombie("exit 0");
        let (mut app, wid, id) = app_with(pid, None, Some(Instant::now()));
        let waiter = parked_close_wait(&app, id);
        assert_eq!(reader_eof(&mut app, id), vec![wid], "the window closes");
        assert!(log_close_due(&app, id), "its close comes with its window's");
        let _ = app.close_window_logical(wid);
        app.settle_log_close(id);
        let reply = waiter
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("answered by the window's close");
        assert!(
            reply.contains(" close ") && reply.contains(" out=shell-exit"),
            "{reply}"
        );

        let pid = run_to_zombie("exit 0");
        let (mut app, wid, id) = app_with(pid, None, Some(Instant::now()));
        let waiter = parked_close_wait(&app, id);
        assert_eq!(reader_eof(&mut app, id), vec![wid], "the window closes");
        app.settle_log_close(id);
        assert!(!log_close_due(&app, id), "no close is coming");
        assert_eq!(
            waiter
                .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                .expect("answered by the settle"),
            "ERR exited\n"
        );
    }

    fn close_probe(app: &App, session: u64) -> String {
        let pooled = app.pool.get(session).expect("the pane's session");
        crate::control::cmd_await(
            &pooled.term,
            &app.store,
            session,
            &pooled.ctx,
            "log close=session timeout=0",
        )
    }

    #[test]
    fn an_exit_a_handoff_deferred_keeps_its_close_due_until_the_replay() {
        let pid = run_to_zombie("exit 0");
        let (mut app, wid, id) = app_with(pid, None, Some(Instant::now()));
        let waiter = parked_close_wait(&app, id);
        app.pending_update_handoff = Some(crate::messages_host::tests::parked());
        assert!(
            reader_eof(&mut app, id).is_empty(),
            "the handoff holds the close"
        );
        app.settle_log_close(id);
        assert!(
            log_close_due(&app, id),
            "the replay's retire writes the close"
        );
        assert_eq!(
            close_probe(&app, id),
            "OK timeout\n",
            "the close is still to come"
        );
        let teardown = app
            .pending_update_handoff
            .take()
            .expect("the attempt's record")
            .teardown;
        let deferred = std::mem::take(&mut app.log_closes_deferred);
        assert_eq!(deferred, std::collections::BTreeSet::from([id]));
        assert_eq!(
            teardown,
            crate::DeferredHandoffTeardown::mutation(crate::DeferredHandoffMutation::ExitSession(
                id
            )),
            "the exit is what the handoff deferred"
        );
        assert_eq!(
            app.exit_session_logical(id),
            vec![wid],
            "the replay closes the window"
        );
        let _ = app.close_window_logical(wid);
        for session in deferred {
            app.settle_log_close(session);
        }
        let reply = waiter
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("answered by the replay's close");
        assert!(
            reply.contains(" close ") && reply.contains(" out=shell-exit"),
            "{reply}"
        );

        let pid = run_to_zombie("exit 0");
        let (mut app, _wid, id) = app_with(pid, None, Some(Instant::now()));
        let waiter = parked_close_wait(&app, id);
        app.pending_update_handoff = Some(crate::messages_host::tests::parked());
        assert!(reader_eof(&mut app, id).is_empty());
        assert!(
            app.defer_pending_update_handoff_teardown(
                crate::DeferredHandoffTeardown::QuitRequested
            ),
            "a quit dominates the deferred exit"
        );
        assert!(log_close_due(&app, id), "still the attempt's to settle");
        let teardown = app
            .pending_update_handoff
            .take()
            .expect("the attempt's record")
            .teardown;
        let deferred = std::mem::take(&mut app.log_closes_deferred);
        assert_eq!(teardown, crate::DeferredHandoffTeardown::QuitRequested);
        for session in deferred {
            app.settle_log_close(session);
        }
        assert!(!log_close_due(&app, id), "no close is coming");
        assert_eq!(
            waiter
                .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                .expect("answered by the completion's settle"),
            "ERR exited\n"
        );
    }

    #[test]
    fn a_shell_that_exits_3_at_start_keeps_its_pane_with_the_line() {
        let at = Instant::now();
        let pid = run_to_zombie("exit 3");
        let (mut app, wid, id) = app_with(pid, started("zsh", at), eof_at_start(at));

        let to_close = reader_eof(&mut app, id);

        assert_kept(&app, wid, id, &to_close, "[zsh ended at start: exit 3]");
        assert_eq!(
            app.session_status.status(id).map(|s| s.last_outcome),
            Some(crate::session_status::Outcome::Failure { exit_code: 3 }),
            "the classification is unchanged"
        );
        assert_eq!(ledger_code(&app, id), Some(3), "the ledger has the code");
        for look in [ExitLook::First, ExitLook::Retry] {
            assert_eq!(
                app.exit_status(id, look),
                Some(aterm_pty::ChildExit::Code(3)),
                "{look:?} reads the status the first look kept"
            );
        }
        assert_eq!(
            app.close_session_by_id(id),
            Ok(CloseProgress::Deferred(wid)),
            "an explicit close still takes it"
        );
        let _ = app.close_window_logical(wid);
        assert!(app.pool.get(id).is_none(), "and the session is retired");
    }

    #[test]
    fn a_shell_that_ends_at_start_keeps_its_pane_whoever_reads_the_status() {
        for (script, shell, tab_status, slow, line, code) in [
            (
                "kill -KILL $$",
                "fish",
                None,
                false,
                "[fish ended at start: signal 9]",
                None,
            ),
            (
                "kill -KILL $$",
                "fish",
                Some(false),
                false,
                "[fish ended at start: signal 9]",
                None,
            ),
            (
                "exit 4",
                "zsh",
                Some(false),
                false,
                "[zsh ended at start: exit 4]",
                Some(4),
            ),
            (
                "exit 3",
                "zsh",
                None,
                true,
                "[zsh ended at start: exit 3]",
                Some(3),
            ),
        ] {
            let pid = run_to_zombie(script);
            let (at, closed) = if slow {
                let at = Instant::now()
                    .checked_sub(Duration::from_secs(3))
                    .expect("the clock reaches back 3 s");
                (at, Some(at + Duration::from_secs(1)))
            } else {
                let at = Instant::now();
                (at, eof_at_start(at))
            };
            let (mut app, wid, id) = app_with(pid, started(shell, at), closed);
            app.config.tab_status = tab_status;

            let to_close = reader_eof(&mut app, id);

            assert_kept(&app, wid, id, &to_close, line);
            assert_eq!(
                ledger_code(&app, id),
                code,
                "{script} tab_status={tab_status:?} slow={slow}"
            );
        }
    }

    #[test]
    fn a_shell_that_got_input_closes_as_before_even_on_exit_1() {
        let at = Instant::now();
        let pid = run_to_zombie("exit 1");
        let mut fds = [0; 2];
        // SAFETY: a fresh two-slot out-param for `pipe(2)`.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe");
        let sink = std::sync::Arc::new(aterm_session::sink::SinkWriter::new(fds[1]));
        let (mut app, wid, id) =
            app_with_sink(pid, started("zsh", at), eof_at_start(at), sink.clone());
        sink.write_frame(b"\x04")
            .expect("the Ctrl-D reaches the PTY");

        assert_eq!(reader_eof(&mut app, id), vec![wid], "the window closes");
        assert!(!pane_text(&app, id).contains("ended at start"));
        assert_eq!(ledger_code(&app, id), Some(1), "the ledger is unchanged");

        drop(app);
        drop(sink);
        // SAFETY: both ends of the pipe opened above; the borrowed sink that used the write end is gone.
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
    }

    #[test]
    fn a_shell_that_was_only_answered_keeps_its_pane() {
        let at = Instant::now();
        let pid = run_to_zombie("exit 1");
        let mut fds = [0; 2];
        // SAFETY: a fresh two-slot out-param for `pipe(2)`.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe");
        let sink = std::sync::Arc::new(aterm_session::sink::SinkWriter::new(fds[1]));
        let (mut app, wid, id) =
            app_with_sink(pid, started("fish", at), eof_at_start(at), sink.clone());
        let (lane, _writer) = crate::spawn::spawn_reply_writer_for_test(sink.clone());
        assert!(
            lane.submit(std::sync::Arc::from(&b"\x1b[?62;22c"[..])),
            "queued"
        );
        let mut ready = libc::pollfd {
            fd: fds[0],
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one live pollfd; a minute is a hang detector, not a budget.
        assert_eq!(
            unsafe { libc::poll(&mut ready, 1, 60_000) },
            1,
            "the reply arrives"
        );

        let to_close = reader_eof(&mut app, id);

        assert_kept(&app, wid, id, &to_close, "[fish ended at start: exit 1]");
        drop((app, lane, sink));
        // SAFETY: both ends of the pipe opened above; the borrowed sink that used the write end is gone.
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
    }

    #[test]
    fn an_exit_that_is_no_failure_at_start_closes_as_today() {
        let now = Instant::now();
        let before_window = now
            .checked_sub(SHELL_START_WINDOW + Duration::from_millis(500))
            .expect("the clock reaches back past the start window");
        for (case, script, start, closed, prompt) in [
            (
                "clean exit",
                "exit 0",
                started("zsh", now),
                eof_at_start(now),
                false,
            ),
            ("no recorded start", "exit 1", None, Some(now), false),
            (
                "after the window",
                "exit 3",
                started("zsh", before_window),
                Some(now),
                false,
            ),
            (
                "drew its prompt",
                "kill -KILL $$",
                started("zsh", now),
                eof_at_start(now),
                true,
            ),
        ] {
            let pid = run_to_zombie(script);
            let (mut app, wid, id) = app_with(pid, start, closed);
            if prompt {
                term_lock(&app.pool.get(id).expect("the pane's session").term)
                    .process(b"\x1b]133;A\x07% \x1b]133;B\x07");
            }

            assert_eq!(
                reader_eof(&mut app, id),
                vec![wid],
                "{case}: the window closes"
            );
            assert!(!pane_text(&app, id).contains("ended at start"), "{case}");
            assert!(
                log_close_due(&app, id),
                "{case}: its close is coming with its window's"
            );
        }
    }

    #[test]
    fn hold_keeps_its_own_behaviour() {
        let at = Instant::now();
        let pid = run_to_zombie("exit 3");
        let (mut app, wid, id) = app_with(pid, started("zsh", at), eof_at_start(at));
        app.hold = true;

        assert!(reader_eof(&mut app, id).is_empty());
        assert!(app.window_contains_session(wid, id), "held, as always");
        assert!(!pane_text(&app, id).contains("ended at start"));
        assert!(!log_close_due(&app, id), "the log knows no close is coming");
    }

    #[test]
    fn a_shared_shell_that_fails_at_start_stays_in_every_viewer() {
        let at = Instant::now();
        let pid = run_to_zombie("exit 1");
        let (mut app, wid, id) = app_with(pid, started("zsh", at), eof_at_start(at));
        app.frontmost_window = Some(wid);
        let second = app
            .open_active_session_in_new_window_logical()
            .expect("share the pane into a second window");

        let to_close = reader_eof(&mut app, id);

        assert_kept(&app, wid, id, &to_close, "[zsh ended at start: exit 1]");
        assert!(!log_close_due(&app, id), "the log knows no close is coming");
        assert!(app.window_contains_session(second, id));
        assert_eq!(
            pane_text(&app, id).matches("ended at start").count(),
            1,
            "one line, not one per viewer"
        );
    }

    #[test]
    fn an_adopted_session_never_reaps_the_child_its_freed_pid_now_names() {
        let pid = run_to_zombie("exit 3");
        let (mut app, _wid, id) = app_with(pid, None, None);
        app.pool
            .sessions
            .get_mut(&id)
            .expect("pooled")
            .session
            .shell_identity = aterm_pty::ShellIdentity::from_parts(pid, None, false);

        let _ = reader_eof(&mut app, id);

        for look in [ExitLook::First, ExitLook::Retry] {
            assert_eq!(app.exit_status(id, look), None, "{look:?}: unknown");
        }
        assert_eq!(
            ledger_code(&app, id),
            None,
            "exit_code=-, never the stranger's 3"
        );
        let mut status: libc::c_int = 0;
        // SAFETY: a non-blocking wait on this test's own zombie child.
        let reaped = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        assert_eq!(reaped, pid, "the zombie was left for its owner");
        assert_eq!(
            libc::WEXITSTATUS(status),
            3,
            "whose wait reads its own status"
        );
    }

    #[cfg(target_vendor = "apple")]
    #[test]
    fn an_adopted_shell_s_exit_status_comes_from_its_exit_watch() {
        use std::io::Write as _;
        let mut shell = aterm_uds::spawnfd::command("/bin/sh")
            .args(["-c", "read _; exit 5"])
            .stdin(aterm_uds::spawnfd::Stdio::piped())
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .stderr(aterm_uds::spawnfd::Stdio::null())
            .spawn()
            .expect("spawn the shell");
        let pid = i32::try_from(shell.id()).expect("pid fits");
        let (mut app, _wid, id) = app_with(pid, None, None);
        {
            let session = &mut app.pool.sessions.get_mut(&id).expect("pooled").session;
            let birth = aterm_pty::record_spawned_shell(pid).birth;
            assert!(
                birth.is_some(),
                "PRECONDITION: the live shell's birth reads"
            );
            session.shell_identity = aterm_pty::ShellIdentity::from_parts(pid, birth, false);
            session.exit_watch = crate::spawn::adopted_exit_watch(&session.shell_identity);
            assert!(
                session.exit_watch.is_some(),
                "the live adopted shell is watched"
            );
        }
        shell
            .stdin
            .take()
            .expect("stdin")
            .write_all(b"\n")
            .expect("release the shell");
        assert_eq!(shell.wait().expect("its parent reaps it").code(), Some(5));

        let _ = reader_eof(&mut app, id);

        assert_eq!(ledger_code(&app, id), Some(5), "the ledger has the code");
        assert_eq!(
            app.session_status.status(id).map(|s| s.last_outcome),
            Some(crate::session_status::Outcome::Failure { exit_code: 5 }),
            "the classifier has the failure"
        );
        for look in [ExitLook::First, ExitLook::Retry] {
            assert_eq!(
                app.exit_status(id, look),
                Some(aterm_pty::ChildExit::Code(5)),
                "{look:?} reads the witnessed status"
            );
        }
    }
}

#[cfg(test)]
#[path = "status_end_tests.rs"]
mod status_end_tests;
