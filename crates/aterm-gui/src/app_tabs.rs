// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tab and session-view lifecycle, tab-strip chrome, and close preflight.

use aterm_window::event_loop::ActiveEventLoop;

use crate::platform::AppRt;
use crate::{
    App, TabAction, TabIndex, WindowId, WindowState, pane, session_store, tab_bar, term_lock,
};

/// Identities for one complete strip refresh, including parked tabs and shared sessions.
/// This snapshot borrows no topology and is discarded when the refresh finishes.
struct LabelIdentities {
    ids: Vec<crate::tab_model::TabId>,
    sessions: Vec<Option<u64>>,
    live_tabs: std::collections::HashSet<crate::tab_model::TabId>,
    live_sessions: std::collections::HashSet<u64>,
}

impl LabelIdentities {
    fn capture(app: &App, wid: WindowId) -> Self {
        let tabs = app
            .windows
            .get(&wid)
            .map_or(&[][..], |ws| ws.tab_set.tabs());
        let mut snapshot = Self {
            ids: Vec::with_capacity(tabs.len()),
            sessions: Vec::with_capacity(tabs.len()),
            live_tabs: std::collections::HashSet::with_capacity(tabs.len()),
            live_sessions: std::collections::HashSet::with_capacity(tabs.len()),
        };
        for tab in tabs {
            #[cfg(test)]
            crate::work_counts::title_identity_visited();
            let session = app
                .view_store
                .get(tab.focus)
                .copied()
                .and_then(crate::tab_model::View::terminal_session);
            snapshot.ids.push(tab.id);
            snapshot.sessions.push(session);
            snapshot.live_tabs.insert(tab.id);
            snapshot.live_sessions.extend(session);
        }
        snapshot
    }
}

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

/// The active tab and focused content that a window shortcut was issued from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TabWindowOrigin {
    window: WindowId,
    tab: crate::tab_model::TabId,
    content: crate::front_content::FrontContent,
}

/// A window-creating shortcut retains its target while waiting on the event loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TabWindowCommand {
    Detach(TabWindowOrigin),
    ViewSession(TabWindowOrigin),
}

impl TabWindowCommand {
    const fn origin(self) -> TabWindowOrigin {
        match self {
            Self::Detach(origin) | Self::ViewSession(origin) => origin,
        }
    }
}

/// An operation issued by a native tab chip retains its canonical identity.
#[cfg(any(target_os = "macos", all(test, unix)))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NativeTabCommand {
    Select(crate::tab_model::TabId),
    Close(crate::tab_model::TabId),
    Move {
        tab: crate::tab_model::TabId,
        neighbor: crate::tab_model::TabId,
        forward: bool,
    },
}

/// The tab and adjacent identities represented by one native chip.
#[cfg(any(target_os = "macos", all(test, unix)))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TabStripOrigin {
    pub(crate) tab: crate::tab_model::TabId,
    pub(crate) previous: Option<crate::tab_model::TabId>,
    pub(crate) next: Option<crate::tab_model::TabId>,
}

#[cfg(any(target_os = "macos", all(test, unix)))]
impl TabStripOrigin {
    /// A drag swaps the pressed tab with its captured adjacent tab.
    pub(crate) fn drag(self, forward: bool) -> Option<NativeTabCommand> {
        Some(NativeTabCommand::Move {
            tab: self.tab,
            neighbor: if forward { self.next } else { self.previous }?,
            forward,
        })
    }
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

/// A tab's title as the strip last read it, kept for a reader that finds its terminal busy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CapturedTitle {
    /// The terminal's rungs of its label ([`resolved_terminal_title_rung`]).
    pub(crate) title: String,
    /// Nothing the tab runs gave the title ([`crate::app_take_over::untitled_by_its_program`]),
    /// read in the same snapshot.
    pub(crate) untitled: bool,
}

/// Make the keep-stale tab-title cache agree with the exact snapshot that caused an effective chrome refresh.
pub(crate) fn synchronize_captured_title_cache(
    cache: &mut std::collections::HashMap<u64, CapturedTitle>,
    session: u64,
    resolved_rung: Option<&str>,
    untitled: bool,
    needs_refresh: bool,
) {
    if !needs_refresh {
        return;
    }
    if let Some(title) = resolved_rung {
        cache.insert(
            session,
            CapturedTitle {
                title: title.to_owned(),
                untitled,
            },
        );
    } else {
        cache.remove(&session);
    }
}

#[cfg(test)]
thread_local! {
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
#[path = "app_tabs_background_title_tests.rs"]
mod background_title_tests;

fn is_terminal_tab(tab: &crate::tab_model::Tab, views: &crate::tab_model::ViewStore) -> bool {
    !tab.root.any_leaf(&mut |view| {
        !matches!(views.get(*view), Some(crate::tab_model::View::Terminal(_)))
    })
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

#[cfg(test)]
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

/// Put terminal tab `tab` and its pane tree into `ws` as the front tab, where [`crate::tab_model::TabSet::push`] seats it: the projection follows the suffix of parked tabs, so `layouts` stays in canonical order.
pub(crate) fn land_terminal_tab(
    ws: &mut WindowState,
    views: &crate::tab_model::ViewStore,
    tree: pane::PaneTree,
    tab: crate::tab_model::Tab,
) -> Result<(), crate::tab_model::DuplicateTabId> {
    let id = tab.id;
    ws.tab_set.push(tab)?;
    let at = terminal_projection_index(&ws.tab_set, views, id).unwrap_or(ws.layouts.len());
    ws.layouts.insert(at, tree);
    ws.tabs = TabIndex::new(at, ws.layouts.len());
    Ok(())
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

/// Apply `change`, which moves tab `moved` within `ws.tab_set`, and carry its pane tree and the mirror's active entry along with it. Whether `change` moved anything.
pub(crate) fn reorder_with_projection(
    ws: &mut WindowState,
    views: &crate::tab_model::ViewStore,
    moved: crate::tab_model::TabId,
    change: impl FnOnce(&mut crate::tab_model::TabSet) -> bool,
) -> bool {
    let held = terminal_tab_id_at(&ws.tab_set, views, ws.tabs.active);
    let old = terminal_projection_index(&ws.tab_set, views, moved);
    if !change(&mut ws.tab_set) {
        return false;
    }
    if let (Some(old), Some(new)) = (old, terminal_projection_index(&ws.tab_set, views, moved))
        && old != new
    {
        let tree = ws.layouts.remove(old);
        ws.layouts.insert(new, tree);
    }
    if let Some(held) = held
        && let Some(index) = terminal_projection_index(&ws.tab_set, views, held)
    {
        ws.tabs.active = index;
    }
    align_terminal_projection_to_active(ws, views);
    true
}

/// The tab a chip's menu and marks speak for: `tab`, except that the Workers chip speaks for the worker it shows in place of the list.
fn chip_subject<'a>(
    ws: &'a WindowState,
    tab: &'a crate::tab_model::Tab,
) -> &'a crate::tab_model::Tab {
    if ws.tab_set.home() == Some(tab.id)
        && let Some(shown) = ws.tab_set.active().filter(|active| active.parked)
    {
        return shown;
    }
    tab
}

/// Whether `ws`'s active tab cannot leave for another window: a parked worker, which moves to a tab first, or the Workers tab, with workers or without. A list is its window's own: only the window's [`crate::tab_model::TabSet::home`] is published to and retired, and the other window, with no home of its own or one already, would hold a list nothing reads or two.
fn workers_stay_in_window(ws: &WindowState) -> bool {
    ws.tab_set.active_is_parked()
        || (ws.tab_set.home().is_some() && ws.tab_set.home() == ws.tab_set.active_id())
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
        let sid = self
            .pool
            .get(session)
            .map(|session| session.ctx.self_id.as_str().to_string());
        if self.pool.views(session) == Some(1) {
            self.rewind_end(session, "the pane Rewind showed closed");
            // Its terminal closes with this view: its processes answer for its tab's totals now.
            self.note_tab_spent(session);
        }
        let dropped = self.pool.detach(session);
        if dropped {
            self.retire_developer_tools_install(session);
            if let Some(local_id) = handed {
                self.note_handed_shell_lost(local_id);
            }
            self.chrome_labels.retire(session);
            self.retire_session_status(session);
            self.retire_presence(session);
            if let Some(sid) = sid {
                self.note_manager_gone(&sid);
            }
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
                let mut found = None;
                tab.root.any_leaf(&mut |&view| {
                    if self
                        .view_store
                        .get(view)
                        .copied()
                        .and_then(crate::tab_model::View::terminal_session)
                        == Some(session)
                    {
                        found = Some(view);
                        true
                    } else {
                        false
                    }
                });
                let view = found?;
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
        self.resize_panes_after_collapse(window);
        self.resync_active_or_window(window);
        true
    }

    /// Keeps a closed tab on the person's reopen stack, never a worker's: a parked tab is not one
    /// the person had, whoever closed it, and a manager closing its workers would bury theirs.
    fn retain_closed_tab(
        &mut self,
        window: WindowId,
        original_index: usize,
        tab: crate::restore::RestoredTab,
    ) {
        if tab.parked {
            return;
        }
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

    pub(crate) fn closed_view_record_for(
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

    pub(crate) fn retain_closed_view(&mut self, record: crate::closed_recovery::ClosedView) {
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
        let tab = self.windows.get(&wid)?.tab_set.get(tab_id)?;
        let mut presentations = Vec::with_capacity(tab.root.len());
        tab.root.visit(&mut |&view| {
            if let Some(value) = self.live_view_presentation(view) {
                presentations.push((view, value));
            }
        });
        crate::tab_model::aggregate_presentations(tab.focus, presentations)
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
        let mut acc = crate::tab_model::TabIndicators::default();
        tab.root.visit(&mut |&view| {
            let Some(bits) = (match self.view_store.get(view) {
                Some(crate::tab_model::View::Terminal(terminal)) => {
                    Some(self.session_status_indicators(terminal.session))
                }
                Some(crate::tab_model::View::Native(native)) => self
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
                None => None,
            }) else {
                return;
            };
            acc.dirty |= bits.dirty;
            acc.busy |= bits.busy;
            acc.attention |= bits.attention;
            acc.status_attention |= bits.status_attention;
        });
        Some(acc)
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
        let Some((tree, canonical_index)) = self.windows.get(&wid).and_then(|window| {
            let canonical_index = window
                .tab_set
                .tabs()
                .iter()
                .position(|tab| tab.id == tab_id)?;
            let tab = window.tab_set.get(tab_id)?;
            if !is_terminal_tab(tab, &self.view_store) {
                return None;
            }
            let layout = self.restored_terminal_pane_layout(&tab.root, tab.focus)?;
            let mut sessions = Vec::with_capacity(tab.root.len());
            tab.root.visit(&mut |&view| {
                if let Some(session) = self
                    .view_store
                    .get(view)
                    .copied()
                    .and_then(crate::tab_model::View::terminal_session)
                {
                    sessions.push(session);
                }
            });
            let mut tree = pane::PaneTree::rebuild(&layout, &sessions)?;
            if tab.zoomed {
                tree.toggle_zoom();
            }
            Some((tree, canonical_index))
        }) else {
            return;
        };
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
        let had = window.layouts.len();
        window.layouts.insert(projection, tree);
        // The active terminal tab, or the one parked while a native or mixed tab is
        // active, moves one slot on when the promoted tab lands at or before it.
        let active = if had > 0 && projection <= window.tabs.active {
            window.tabs.active + 1
        } else {
            window.tabs.active
        };
        window.tabs = TabIndex::new(
            active.min(window.layouts.len().saturating_sub(1)),
            window.layouts.len(),
        );
        align_terminal_projection_to_active(window, &self.view_store);
    }

    /// Open the singleton System State controller in the frontmost window.
    pub(crate) fn open_system_state_tab(&mut self) -> bool {
        let Some(wid) = self.frontmost_window else {
            return false;
        };
        self.open_system_state_tab_in_window(wid)
    }

    /// The System State tab for an offer that must not take the person's tab (design §6): one
    /// that is open stays where it is, and one opened now goes behind the tab the frontmost
    /// window shows, or the first window's when none is in front. Whether a System State tab is
    /// open.
    pub(crate) fn open_system_state_tab_quietly(&mut self) -> bool {
        if self
            .native_runtime
            .instance_by_kind(crate::native_app::AppKind::SystemState)
            .is_some_and(|instance| {
                self.view_store.iter().any(|(_, view)| {
                    matches!(view, crate::tab_model::View::Native(native) if native.instance == instance)
                })
            })
        {
            return true;
        }
        let Some(wid) = self
            .frontmost_window
            .filter(|wid| self.windows.contains_key(wid))
            .or_else(|| self.windows.keys().next().copied())
        else {
            return false;
        };
        let previous = self
            .windows
            .get(&wid)
            .and_then(|window| window.tab_set.active_id());
        if self.stage_system_state_tab(wid).is_err() {
            return false;
        }
        let Some(window) = self.windows.get_mut(&wid) else {
            return false;
        };
        if let Some(previous) = previous {
            window.tab_set.switch_to(previous);
        }
        window.last_present = None;
        self.resync_active_or_window(wid);
        self.sync_active_session();
        if let Some(window) = self
            .windows
            .get(&wid)
            .and_then(|window| window.os_window.as_ref())
        {
            window.request_redraw();
        }
        true
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
                let mut found = None;
                tab.root.any_leaf(&mut |&view| {
                    let Some(crate::tab_model::View::Native(native)) = self.view_store.get(view)
                    else {
                        return false;
                    };
                    if self
                        .native_runtime
                        .app(native.instance)
                        .is_some_and(|app| app.kind() == crate::native_app::AppKind::SystemState)
                    {
                        found = Some((tab.id, view));
                        true
                    } else {
                        false
                    }
                });
                found
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
        let open = self.system_state_host.fixture_line();
        let state = crate::native_app::AppViewState::SystemState(Box::new(
            crate::system_state::SystemStateViewState::with_line_open(open),
        ));
        let presentation = crate::tab_model::TabPresentation {
            title: "System State".into(),
            icon: Some(crate::tab_model::TabIconKind::SystemState),
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
            let (status, _) =
                self.project_system_state_status(self.system_state_host.latest_status().clone());
            let mut app = crate::system_state::SystemStateApp::opened();
            app.publish(status);
            self.install_new_native_tab(
                wid,
                crate::native_app::NativeApp::SystemState(Box::new(app)),
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
        self.install_native_tab_keeping(wid, instance, state, presentation, None)
    }

    /// [`Self::install_native_tab`], except that where `keep` is given and stands in `wid`
    /// the new tab is appended behind it: `keep` stays the active tab and nothing syncs, so
    /// the caller owes the window its one sync.
    pub(crate) fn install_native_tab_keeping(
        &mut self,
        wid: WindowId,
        instance: crate::tab_model::AppInstanceId,
        state: crate::native_app::AppViewState,
        presentation: crate::tab_model::TabPresentation,
        keep: Option<crate::tab_model::TabId>,
    ) -> Result<(crate::tab_model::TabId, crate::tab_model::ViewId), NativeTabInstallError> {
        if !self.windows.contains_key(&wid) {
            return Err(NativeTabInstallError::UnknownWindow);
        }
        // The view lays out in the faces its frame paints with, from its first layout.
        self.land_native_layout_faces();

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
        let keep = keep.filter(|keep| ws.tab_set.get(*keep).is_some());
        if ws.tab_set.push(tab).is_err() {
            self.native_runtime.remove_view(view);
            self.view_store.remove(view);
            return Err(NativeTabInstallError::DuplicateTab);
        }
        // A push makes the new tab active: the kept tab goes back before anything syncs.
        if let Some(keep) = keep {
            let kept = ws.tab_set.switch_to(keep);
            debug_assert!(kept);
            align_terminal_projection_to_active(ws, &self.view_store);
            debug_assert!(ws.tab_set.invariant_holds(&self.view_store));
            return Ok((tab_id, view));
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
        self.install_new_native_tab_keeping(wid, app, state, presentation, None)
    }

    /// [`Self::install_new_native_tab`] with [`Self::install_native_tab_keeping`]'s `keep`.
    pub(crate) fn install_new_native_tab_keeping(
        &mut self,
        wid: WindowId,
        app: crate::native_app::NativeApp,
        state: crate::native_app::AppViewState,
        presentation: crate::tab_model::TabPresentation,
        keep: Option<crate::tab_model::TabId>,
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
        match self.install_native_tab_keeping(wid, instance, state, presentation, keep) {
            Ok((tab, view)) => Ok((instance, tab, view)),
            Err(error) => {
                self.native_runtime.remove_instance(instance);
                Err(error)
            }
        }
    }

    /// Attach a view of an existing native app beside the focused canonical leaf.
    pub(crate) fn split_active_with_native(
        &mut self,
        wid: WindowId,
        axis: crate::tab_model::SplitAxis,
        instance: crate::tab_model::AppInstanceId,
        state: crate::native_app::AppViewState,
    ) -> Result<crate::tab_model::ViewId, NativeTabInstallError> {
        let (tab_id, prior_projection) = self
            .windows
            .get(&wid)
            .and_then(|window| {
                let tab = window.tab_set.active()?;
                Some((
                    tab.id,
                    terminal_projection_index(&window.tab_set, &self.view_store, tab.id),
                ))
            })
            .ok_or(NativeTabInstallError::UnknownWindow)?;
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
        self.reconcile_view_topology_projection(wid, tab_id, prior_projection);
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
        let mut by_session = std::collections::HashMap::new();
        let mut used = std::collections::HashSet::new();
        let Some((layout, tab_id)) = self.windows.get(&wid).and_then(|ws| {
            let tab_id = terminal_tab_id_at(&ws.tab_set, &self.view_store, index)?;
            let layout = ws.layouts.get(index)?.clone();
            ws.tab_set.get(tab_id)?.root.visit(&mut |&view| {
                if let Some(session) = self
                    .view_store
                    .get(view)
                    .copied()
                    .and_then(crate::tab_model::View::terminal_session)
                {
                    by_session.insert(session, view);
                    used.insert(view);
                }
            });
            Some((layout, tab_id))
        }) else {
            return false;
        };

        let mut missing = false;
        layout.visit_sessions(&mut |session| {
            if missing || by_session.contains_key(&session) {
                return;
            }
            let Some(view) = self.view_store.iter().find_map(|(id, view)| {
                (!used.contains(&id) && view.terminal_session() == Some(session)).then_some(id)
            }) else {
                missing = true;
                return;
            };
            by_session.insert(session, view);
            used.insert(view);
        });
        if missing {
            return false;
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
            self.retire_native_ime_view(view);
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
        self.publish_system_state_front();
        Some(removed)
    }

    pub(crate) fn remove_tab_views(&mut self, tab: &crate::tab_model::Tab) {
        tab.root.visit(&mut |&view| {
            self.remove_view_link(view);
        });
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

    /// The chrome rows every window shares: the tab strip's.
    pub(crate) const fn chrome_rows_shared(&self) -> u16 {
        self.tab_strip_rows
    }

    /// One title per top-level tab of window `wid`, for the strip labels.
    pub(crate) fn tab_titles(&mut self, wid: WindowId) -> Vec<String> {
        let identities = LabelIdentities::capture(self, wid);
        self.tab_titles_from_identities(wid, &identities)
    }

    fn tab_titles_from_identities(
        &mut self,
        wid: WindowId,
        identities: &LabelIdentities,
    ) -> Vec<String> {
        let Some(ws) = self.windows.get(&wid) else {
            return Vec::new();
        };
        let read: Vec<_> = ws
            .tab_set
            .tabs()
            .iter()
            .zip(&identities.sessions)
            .map(|(tab, session)| {
                #[cfg(test)]
                crate::work_counts::title_read_visited();
                if let Some(title) = self.workers_chip_title(ws, tab) {
                    return (title, None, None);
                }
                let fallback = if !tab.presentation.title.is_empty() {
                    tab.presentation.title.clone()
                } else {
                    "aterm".to_string()
                };
                let Some(session) = *session else {
                    return (fallback, None, None);
                };
                let Some(s) = self.pool.get(session) else {
                    return (fallback, None, None);
                };
                let user_title = s
                    .ctx
                    .meta
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .presentation_value("title");
                if user_title.is_some() {
                    let pinned = user_title.map(|title| CapturedTitle {
                        title,
                        untitled: false,
                    });
                    return (fallback, Some(session), pinned);
                }
                let term_label = |t: &aterm_core::terminal::Terminal| {
                    use crate::cwd_native::ReportedCwd as _;
                    let cwd = t.native_working_directory();
                    CapturedTitle {
                        title: resolved_terminal_title_rung(None, t.title(), cwd.as_deref())
                            .unwrap_or_default(),
                        untitled: crate::app_take_over::untitled_by_its_program(t),
                    }
                };
                let fresh = match s.term.try_lock() {
                    Ok(t) => Some(term_label(&t)),
                    Err(std::sync::TryLockError::Poisoned(p)) => Some(term_label(&p.into_inner())),
                    Err(std::sync::TryLockError::WouldBlock) => None,
                };
                (fallback, Some(session), fresh)
            })
            .collect();
        // A worker chip, native leaf or missing pool session must not retain a captured
        // terminal title. A busy terminal still must: its last capture is the fallback.
        let captured_sessions: std::collections::HashSet<_> = read
            .iter()
            .filter_map(|(_, session, _)| {
                #[cfg(test)]
                crate::work_counts::title_eligibility_read();
                *session
            })
            .collect();
        let chrome_labels = &self.chrome_labels;
        let Some(ws) = self.windows.get_mut(&wid) else {
            return Vec::new();
        };
        ws.tab_title_cache.retain(|sid, _| {
            #[cfg(test)]
            crate::work_counts::title_cache_probed();
            captured_sessions.contains(sid)
        });
        read.into_iter()
            .map(|(fallback, sid, fresh)| {
                let Some(sid) = sid else {
                    return fallback;
                };
                let raw = match fresh {
                    Some(label) => {
                        if label.title.is_empty() {
                            ws.tab_title_cache.remove(&sid);
                            fallback
                        } else {
                            let title = label.title.clone();
                            ws.tab_title_cache.insert(sid, label);
                            title
                        }
                    }
                    None => ws
                        .tab_title_cache
                        .get(&sid)
                        .map(|cached| cached.title.clone())
                        .filter(|cached| !cached.is_empty())
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
        let roles = self
            .config
            .tab_connection_badge_or_default()
            .then(|| crate::connections::roles_by_local(&self.store));
        let Self {
            windows,
            view_store,
            ..
        } = self;
        let Some(ws) = windows.get_mut(&wid) else {
            return;
        };
        for index in 0..ws.tab_set.len() {
            let Some(tab) = ws.tab_set.tab_at_mut(index) else {
                continue;
            };
            let (mut outbound, mut inbound) = (false, false);
            if let Some(roles) = roles.as_ref() {
                tab.root.visit(&mut |view| {
                    if let Some(session) = view_store
                        .get(*view)
                        .copied()
                        .and_then(crate::tab_model::View::terminal_session)
                        && let Some(role) = roles.get(&session)
                    {
                        outbound |= role.outbound;
                        inbound |= role.inbound;
                    }
                });
            }
            tab.presentation.conn = crate::tab_model::TabConnRole::from_flags(outbound, inbound);
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
                .map(|tab| {
                    let mut metadata =
                        crate::tab_bar::TabStripMetadata::from_presentation(&tab.presentation);
                    if metadata.icon.is_none()
                        && self
                            .view_store
                            .get(tab.focus)
                            .copied()
                            .and_then(crate::tab_model::View::terminal_session)
                            .is_some_and(|session| {
                                self.pool.get(session).is_some_and(|slot| {
                                    use aterm_phase::Program;
                                    match slot
                                        .ctx
                                        .timeline
                                        .lock()
                                        .unwrap_or_else(|p| p.into_inner())
                                        .agent()
                                        .reader
                                    {
                                        Some(Program::Codex) => true,
                                        Some(
                                            Program::Claude
                                            | Program::Agy
                                            | Program::Muse
                                            | Program::Grok
                                            | Program::Kimi
                                            | Program::Generic,
                                        )
                                        | None => false,
                                    }
                                })
                            })
                    {
                        metadata.icon = Some(crate::tab_model::TabIconKind::Codex);
                    }
                    metadata
                })
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
        // session's entry): `pool` is borrowed separately so the retain
        // closure borrows no other field of `self`.
        if self.session_chrome.len() > 64 {
            let pool = &self.pool;
            self.session_chrome
                .retain(|sid, _| pool.get(*sid).is_some());
        }
        let mut tabs: Vec<(
            Option<crate::session_chrome::TerminalMenuOrigin>,
            Option<String>,
            Option<crate::tab_model::TabId>,
        )> = self.windows.get(&wid).map_or_else(Vec::new, |ws| {
            ws.tab_set
                .tabs()
                .iter()
                .map(|tab| {
                    // The Workers chip's menu is the shown worker's, as its title names it, so
                    // each of its rows acts on the worker's tab.
                    let subject = chip_subject(ws, tab);
                    let menu_tab = (subject.id != tab.id).then_some(subject.id);
                    let focus = subject.focus;
                    let origin = self
                        .view_store
                        .get(focus)
                        .copied()
                        .and_then(crate::tab_model::View::terminal_session)
                        .map(|session| crate::session_chrome::TerminalMenuOrigin {
                            view: focus,
                            session,
                        });
                    let tooltip = if origin.is_none() {
                        tab.presentation.tooltip.clone()
                    } else {
                        None
                    };
                    (origin, tooltip, menu_tab)
                })
                .collect()
        });
        let mut edges = None;
        let ext: Vec<crate::session_chrome::TabChromeExt> = tabs
            .iter_mut()
            .enumerate()
            .map(
                |(i, (origin, presentation_tooltip, menu_tab))| match origin {
                    Some(origin) => {
                        let label = titles.get(i).map_or("", String::as_str);
                        let mut ext =
                            self.composed_session_chrome(wid, origin.session, label, &mut edges);
                        ext.bind_terminal_origin(*origin);
                        ext.menu_tab = *menu_tab;
                        ext
                    }
                    None => crate::session_chrome::TabChromeExt {
                        tooltip: presentation_tooltip.take(),
                        menu: Vec::new(),
                        menu_tab: None,
                    },
                },
            )
            .collect();
        if let Some(ws) = self.windows.get_mut(&wid) {
            for (i, ((origin, ..), ext)) in tabs.iter().zip(&ext).enumerate() {
                if origin.is_some()
                    && ws
                        .tab_set
                        .home()
                        .is_none_or(|home| ws.tab_set.tab_at(i).is_none_or(|tab| tab.id != home))
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
        let (high_id, goal_term) = {
            let timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            (
                timeline.view_head().unwrap_or(0),
                crate::claude_footer::goal_term(timeline.goal()),
            )
        };
        let now = crate::turn_ledger::now_ms();
        let clause = self.chrome_clause_read(retry_window, session, &ctx.timeline, now);
        let can_rename = self.can_rename_session(retry_window);
        if let Some(c) = self.session_chrome.get(&session)
            && c.high_id == high_id
            && c.label == label
            && c.can_rename == can_rename
            && c.activity_revision == activity_revision
            && c.connections_revision == connections_revision
            && c.upgrade_revision == upgrade_revision
            && c.goal == goal_term
            && c.clause.as_ref() == clause.as_ref().map(|(key, _)| key)
        {
            return c.ext.clone();
        }
        let activity = self.session_status_text(session);
        let (icon, description, attention) = {
            let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
            (
                meta.presentation_value("icon"),
                meta.presentation_value("description"),
                meta.presentation_value("attention"),
            )
        };
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
        let (timeline, goal): (Vec<chrome::TimelineNote>, Option<String>) = {
            let tl = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            (
                chrome::timeline_tail(tl.view_newest_first()),
                tl.goal().map(str::to_owned),
            )
        };
        let goal_term = crate::claude_footer::goal_term(goal.as_deref());
        let upgrade = sid.as_ref().and_then(|sid| {
            self.upgrade_view
                .menu_for(sid.as_str(), crate::upgrade_host::now_s())
        });
        let agent = self.agent_menu_facts(session);
        let (clause_key, clause) = match clause {
            Some((key, line)) => (Some(key), Some(line)),
            None => (None, None),
        };
        let input = chrome::SessionChromeInput {
            can_rename,
            label: label.to_string(),
            icon,
            description,
            activity,
            goal,
            cwd,
            home: cached_home().map(str::to_string),
            state,
            has_session,
            timeline,
            connections,
            upgrade,
            attention,
            agent,
            sid,
            clause,
            rewind: self.session_records(session),
        };
        let ext = chrome::TabChromeExt {
            tooltip: chrome::compose_tooltip(&input),
            menu: chrome::compose_tab_menu(&input),
            menu_tab: None,
        };
        let expires_ms = chrome::clock_expiry_ms(now, &input);
        self.session_chrome.insert(
            session,
            chrome::CachedChrome {
                high_id,
                label: input.label,
                can_rename,
                activity_revision,
                connections_revision,
                upgrade_revision,
                goal: goal_term,
                expires_ms,
                clause: clause_key,
                clause_line: input.clause,
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
        let mut peers: std::collections::BTreeMap<&str, (&aterm_session::SessionId, Halves)> =
            std::collections::BTreeMap::new();
        for edge in edges {
            // Distinct branch.
            if edge.src == edge.dst {
                continue;
            }
            let (peer, outbound) = if edge.src == *sid {
                (&edge.dst, true)
            } else if edge.dst == *sid {
                (&edge.src, false)
            } else {
                continue;
            };
            let write = matches!(edge.op, aterm_session::Op::WriteInput);
            let entry = &mut peers
                .entry(peer.as_str())
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
                let peer_title = g.by_sid(peer).map_or_else(
                    || "unknown".to_string(),
                    crate::session_store::SessionHandle::display_title,
                );
                let (direction, push) = match (halves.out_any, halves.in_any) {
                    (true, true) => (ConnDirection::Peer, halves.out_write),
                    (true, false) => (ConnDirection::Outbound, halves.out_write),
                    _ => (ConnDirection::Inbound, halves.in_write),
                };
                ConnectionFact {
                    peer_sid: peer.clone(),
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
    ///
    /// The active tab is read here, by its canonical index as the strip paints it: a caller's
    /// `ws.tabs.active` counts only all-terminal tabs, so it would never match a frame's
    /// fingerprint once a native or mixed tab is active or sits ahead of the active tab.
    pub(crate) fn tab_strip_fingerprint_from_parts(
        &self,
        wid: WindowId,
        titles: &[String],
        metadata: &[crate::tab_bar::TabStripMetadata],
    ) -> u64 {
        if !self.tab_strip_enabled() {
            return 0;
        }
        let active = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.tab_set.strip_active())
            .unwrap_or(0);
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

    fn tab_window_origin(&self, window: WindowId) -> Option<TabWindowOrigin> {
        let tab = self.windows.get(&window)?.tab_set.active()?;
        let content = match *self.view_store.get(tab.focus)? {
            crate::tab_model::View::Terminal(terminal) => {
                crate::front_content::FrontContent::Terminal {
                    view: tab.focus,
                    session: terminal.session,
                }
            }
            crate::tab_model::View::Native(native) => crate::front_content::FrontContent::Native {
                instance: native.instance,
                view: tab.focus,
            },
        };
        Some(TabWindowOrigin {
            window,
            tab: tab.id,
            content,
        })
    }

    /// Capture the keyboard shortcut's source before posting its wake.
    pub(crate) fn tab_window_shortcut(
        &self,
        window: WindowId,
        key: &str,
    ) -> Option<TabWindowCommand> {
        let origin = self.tab_window_origin(window)?;
        match key {
            "n" | "N" => Some(TabWindowCommand::Detach(origin)),
            "o" | "O" => Some(TabWindowCommand::ViewSession(origin)),
            _ => None,
        }
    }

    /// Act on the captured content, allowing another window to have become frontmost.
    pub(crate) fn apply_tab_window_command_logical(
        &mut self,
        command: TabWindowCommand,
    ) -> Option<WindowId> {
        let origin = command.origin();
        if self.tab_window_origin(origin.window) != Some(origin) {
            return None;
        }
        match command {
            TabWindowCommand::Detach(_) => self.detach_active_tab_in_window_logical(origin.window),
            TabWindowCommand::ViewSession(_) => {
                self.open_session_view_from_window_logical(origin.window)
            }
        }
    }

    /// Deliver a queued shortcut and attach its new OS window.
    pub(crate) fn apply_tab_window_command(
        &mut self,
        el: &ActiveEventLoop,
        command: TabWindowCommand,
    ) {
        let previous_front = self.frontmost_window;
        let Some(created) = self.apply_tab_window_command_logical(command) else {
            return;
        };
        if !self.headless && !self.attach_os_window(el, created) {
            self.rollback_tab_window_command(command, created, previous_front);
        }
    }

    /// An attachment failure restores the source tab and the previously front window.
    pub(crate) fn rollback_tab_window_command(
        &mut self,
        command: TabWindowCommand,
        created: WindowId,
        previous_front: Option<WindowId>,
    ) {
        match command {
            TabWindowCommand::Detach(origin) => {
                self.detach_rollback_with_front(Some(origin.window), created, previous_front);
            }
            TabWindowCommand::ViewSession(_) => {
                self.close_window_logical(created);
                self.frontmost_window = previous_front.filter(|wid| self.windows.contains_key(wid));
                self.sync_active_session();
            }
        }
    }

    /// "Move Tab to New Window" from the front window.
    pub(crate) fn detach_active_tab_logical(&mut self) -> Option<WindowId> {
        let wid_a = self.frontmost_window?;
        self.detach_active_tab_in_window_logical(wid_a)
    }

    fn detach_active_tab_in_window_logical(&mut self, wid_a: WindowId) -> Option<WindowId> {
        let (stable_tab, terminal_index, tree, rows, cols) = {
            let ws = self.windows.get(&wid_a)?;
            if ws.tab_set.len() <= 1 || workers_stay_in_window(ws) {
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
        self.insert_unattached_window(wid_b, ws_b);
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
        self.detach_rollback_with_front(wid_a, wid_b, wid_a);
    }

    fn detach_rollback_with_front(
        &mut self,
        wid_a: Option<WindowId>,
        wid_b: WindowId,
        previous_front: Option<WindowId>,
    ) {
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
                land_terminal_tab(ws_a, &self.view_store, tree, tab)
                    .expect("returned tab id remains unique");
            } else {
                debug_assert!(tree.is_none());
                ws_a.tab_set
                    .push(tab)
                    .expect("returned tab id remains unique");
            }
            align_terminal_projection_to_active(ws_a, &self.view_store);
        }
        self.winit_to_window.retain(|_, &mut v| v != wid_b);
        self.focus_order.retain(|w| *w != wid_b);
        self._toolbars.remove(&wid_b);
        self.frontmost_window = previous_front.filter(|wid| self.windows.contains_key(wid));
        if let Some(source) = wid_a
            && self.frontmost_window != Some(source)
        {
            self.sync_window(source);
        }
        self.sync_active_session();
    }

    /// "Move Tab to Next Window" (Cmd-Shift-M).
    pub(crate) fn migrate_active_tab_to_next_window(&mut self) -> Result<(), &'static str> {
        let Some(wid_a) = self.frontmost_window else {
            return Err("no window is focused");
        };
        self.migrate_active_tab_from_window(wid_a)
    }

    /// Move the active tab in the named window to the next window.
    pub(crate) fn migrate_active_tab_from_window(
        &mut self,
        wid_a: WindowId,
    ) -> Result<(), &'static str> {
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
            if workers_stay_in_window(ws) {
                return Err(if ws.tab_set.active_is_parked() {
                    "a worker moves to a tab before it moves to another window"
                } else {
                    "the Workers tab stays in its window: it lists only that window's workers"
                });
            }
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
            match tree.as_ref() {
                Some(tree) => land_terminal_tab(ws, &self.view_store, tree.clone(), stable_tab),
                None => ws.tab_set.push(stable_tab).map(|_| ()),
            }
            .expect("moved tab id is process-unique");
            align_terminal_projection_to_active(ws, &self.view_store);
        }
        // `frontmost_window` is left to the OS: B's `Focused(true)` moves it
        // (`window_focus_event`). A destination that never takes focus (a minimized
        // window on macOS, any window on Wayland) leaves the front on the window that
        // still holds the keyboard, so a menu key equivalent (⌘V, ⌘W, ⌘C) and a bare
        // ctl verb never act on a window the person cannot see.
        // A mixed tab has no `layouts` projection, but its terminal leaves still
        // carry the source window's grid.
        self.resize_panes(wid_b);
        self.sync_window(wid_b);
        if source_now_empty {
            let _ = self.close_window_logical(wid_a);
        } else {
            self.sync_window(wid_a);
        }
        self.sync_active_session();
        // `raise_window`, not a bare `focus_window`: macOS ignores focus for a
        // minimized window, so the raise un-minimizes it first.
        self.raise_window(wid_b);
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
        self.open_session_view_from_window_logical(wid_a)
    }

    fn open_session_view_from_window_logical(&mut self, wid_a: WindowId) -> Option<WindowId> {
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
        self.insert_unattached_window(wid_b, ws_b);
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

    /// Test-only: close the tab holding `session` as its program's end does.
    #[cfg(test)]
    pub(crate) fn close_session_tab_for_test(&mut self, session: u64) -> bool {
        let found = self.windows.iter().find_map(|(wid, ws)| {
            ws.tab_set
                .tabs()
                .iter()
                .position(|tab| {
                    self.view_store
                        .get(tab.focus)
                        .copied()
                        .and_then(crate::tab_model::View::terminal_session)
                        == Some(session)
                })
                .map(|at| (*wid, at))
        });
        let Some((wid, at)) = found else {
            return false;
        };
        self.close_tab_via_verb(wid, Some(at));
        true
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

    /// One registry handle as the status item sees it (`docs/DESIGN-owner-surfaces-2026-10-16.md` §3): the tab's label as every Agents surface names it ([`Self::tab_label_of_session`]: the Overview, the tab menu and the long-hold entry name it the same), the session's PUBLISHED verdict ([`Self::publish_need`]), its agent word, its task record and what the person's hold refused.
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
        let stop = self.shown_stop(h.local_id);
        let held_tip = hold
            .as_ref()
            .and_then(|hold| hold.words(None, crate::harness_glass::HoldForm::Tip, None, stop));
        let stop_tip = stop.map(|stop| stop.words(crate::harness_glass::HoldForm::Tip));
        let landing_tip = match (stop_tip, held_tip) {
            (Some(stop), Some(held)) => {
                Some(format!("{stop}{}{held}", crate::native_ui::FACT_SEPARATOR))
            }
            (stop, held) => stop.or(held),
        };
        let need_alone = hold.as_ref().is_some_and(|hold| hold.hold.need_alone());
        let (verdict, agent, program, task, told, receipt) = {
            let tl = h.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            // What aterm checked at the landing (§14.9, slice 9c), beside a landed face only.
            let receipt = tl
                .task_face()
                .filter(|(face, _)| *face == aterm_agent::harness::acts::TaskFace::Landed)
                .and_then(|_| tl.land_receipt().cloned());
            (
                tl.need().clone(),
                tl.agent().word,
                // The survey's render-time rule: a reader that names no state (codex's) leaves
                // the word to what the program says itself.
                tl.program_shown()
                    .filter(|_| tl.agent().word == "unknown")
                    .map(|shown| crate::program_state::survey_word(shown.state)),
                tl.task().map(|t| crate::status_item::TaskRow {
                    state: t.state,
                    why: t.why,
                    finished: t.finished,
                    from: t.from.clone(),
                    entered: t.entered,
                    since_unix_ms: t.since_unix_ms,
                    result_bytes: t.result_bytes,
                    result_path: t.result_path.clone(),
                    seen: false,
                    own_report: crate::task_record::reports_its_own_end(t.trusted_cwd.as_deref()),
                }),
                glass_end.and_then(|end| tl.task_face_at(end.face.task_face())),
                receipt,
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
            program,
            task,
            need_since: self.presence.need_ages.need_since(h.local_id),
            held_since: self.presence.need_ages.held_since(h.local_id),
            person_refusals: h.ctx.fabric.person_refusals(),
            held_by_host: h.ctx.fabric.holds().remote().map(|r| r.host.clone()),
            end,
            landed,
            landing: hold.is_some(),
            landing_tip,
            receipt,
            need_alone,
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

    /// Fold one session's PUBLISHED need into the need ages ([`crate::status_item::NeedAges`], §9 S4: the ages are fed by the need): remember since when its need and hold stand (the glance's ages), and dirty the menu-bar item where what it shows of the session moved.
    pub(crate) fn age_session_need(&mut self, session: u64) {
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
            .need_ages
            .observe(session, row.as_ref(), now_unix)
        {
            self.mark_status_dirty();
            crate::control::needs_moved();
        }
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
            && task.state.has_an_end_to_read()
            && task.since_unix_ms == done
        {
            self.mark_done_seen(session, task.entered);
        }
        self.presence.need_ages.adopt(session, carry, now_unix);
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

    /// Each session's Autopilot end and its task's work, as the rows a seamless update's park stamps on its manifest (`SessionRecord::autopilot_end`, `app_update_handoff::stamp_autopilot_end`; §11.2.3, item 8): the end standing on the glass, or, with none there, the face of a proven self-claim, which no loop restates at the successor ([`Self::self_claim_face`]).
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
                let face = end.is_none().then(|| self.self_claim_face(id)).flatten();
                crate::harness_end_carry::EndCarry {
                    end,
                    face,
                    worked_ms: self.harness_screen.ab.worked(id, now),
                }
                .row()
                .map(|row| (id, row))
            })
            .collect()
    }

    /// THE FACE A SEAMLESS UPDATE CARRIES FOR A PROVEN SELF-CLAIM on session `session`, which no loop restates at the successor ([`Self::end_carry_rows`]; the Commit stands down where it moved since the park drew it, `App::carried_claims_held`): beside the claim, or in the gap of its own re-exec, the face shown (Task facts are dropped there, so it is the claim's); and the face an earlier update carried here while nothing has told one since, so a second update before the claim is proven again carries it on. `None` for every other session.
    #[cfg(unix)]
    pub(crate) fn self_claim_face(
        &self,
        session: u64,
    ) -> Option<(aterm_agent::harness::acts::TaskFace, i64)> {
        let self_claimed =
            self.beside_proven_self_claim(session) || self.in_self_claim_reexec(session);
        let timeline = self
            .pool
            .get(session)?
            .ctx
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let carried = timeline.carried_task_face();
        if self_claimed {
            timeline.task_face().or(carried)
        } else {
            carried
        }
    }

    /// What the status item knows of each session, as the rows a seamless update's park stamps on its manifest (`SessionRecord::status_seen`, `app_update_handoff::stamp_status_seen`).
    #[cfg(all(unix, test))]
    pub(crate) fn status_carry_rows(&self) -> Vec<(u64, String)> {
        self.status_carry_snapshots()
            .into_iter()
            .filter_map(|(id, carry)| carry.row().map(|row| (id, row)))
            .collect()
    }

    /// What the status item knows of each session ([`crate::status_item::StatusCarry`]), which a seamless update's park stamps on its manifest as rows (`app_update_handoff::stamp_status_seen`).
    #[cfg(unix)]
    pub(crate) fn status_carry_snapshots(&self) -> Vec<(u64, crate::status_item::StatusCarry)> {
        self.pool
            .iter()
            .map(|session| {
                let id = session.id;
                let mut carry = self.presence.need_ages.carry_of(id);
                carry.done = self.done_seen.get(&id).and_then(|entered| {
                    session
                        .ctx
                        .timeline
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .task()
                        .filter(|t| t.state.has_an_end_to_read() && t.entered == *entered)
                        .map(|t| t.since_unix_ms)
                });
                (id, carry)
            })
            .collect()
    }

    /// THE STATUS GLANCE (§3): the REGISTRY snapshot → one [`crate::status_item::SessionRow`] per live session ([`Self::status_session_row`], leaf locks only) → the pure [`crate::status_item::Glance::build`], with the last sibling scan, the fleet read, the API outage's start and the digest's entries.
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
        glance.system_state = self.system_state_menu_line();
        glance.network = crate::net_place::menu_line();
        glance.access = self.lock_face.on_disk();
        glance.access_shown = self.lock_face.shown();
        glance.access_flash = self.lock_face.flash(std::time::Instant::now());
        #[cfg(target_os = "macos")]
        {
            glance.machine = self
                .machine_view
                .as_ref()
                .map(crate::machine_title::machine_menu_bar_title);
        }
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
        self.reread_fleet_access();
        let glance = self.status_glance();
        let fp = glance.fingerprint();
        let glance = self.status_drawn_next(glance);
        self._status_item = self.apprt.install_status_item(proxy, &glance);
        if self._status_item.is_some() {
            self.status_fingerprint = Some(fp);
            crate::sibling_scan::request_scan(proxy.clone());
            self.start_fleet_glance();
        }
    }

    /// Read this Mac's stored fleet access again (`crate::control::fleet_access_stated`): at
    /// the item's install, on the change notification `aterm fleet access`, a leave, a join and
    /// another window's lock post (`Wake::FleetAccessChanged`), as the menu opens, before a
    /// click rotates it, before a pending raise is written and as `aterm ctl chrome` prints it.
    /// A level this App did not write replaces the one in force, and a pending raise is dropped
    /// where the state it was asked over moved ([`crate::status_item::LockFace::seen`]); the
    /// change is journalled, and a drop below Read and Write that has rested lifts the remote
    /// holds ([`Self::observe_fleet_access`]).
    pub(crate) fn reread_fleet_access(&mut self) {
        let (level, age) = crate::control::fleet_access_stated();
        if self
            .lock_face
            .seen(level, crate::control::fleet_access_basis())
        {
            self.mark_status_dirty();
        }
        self.observe_fleet_access(level, age, None);
    }

    /// Tell this process's fleet listener the level this Mac is at, stated `age` ago, and
    /// whether the lock holds a raise its person clicked for
    /// (`crate::fleet_listen::observe_access`), and remember when it asks to be told again: a
    /// drop below Read and Write lifts the remote holds once it has rested. A test's level is
    /// its thread's and is not this process's to observe.
    #[cfg_attr(
        any(not(unix), test),
        expect(
            clippy::unused_self,
            clippy::needless_pass_by_ref_mut,
            reason = "the fleet listener is unix's, and a test observes no level"
        )
    )]
    fn observe_fleet_access(
        &mut self,
        level: aterm_types::fleet_access::FleetAccess,
        age: Option<std::time::Duration>,
        by: Option<&str>,
    ) {
        #[cfg(all(unix, not(test)))]
        {
            let again =
                crate::fleet_listen::observe_access(level, age, by, Some(self.lock_face.raising()));
            #[cfg(target_os = "macos")]
            {
                self.access_look = again;
            }
            #[cfg(not(target_os = "macos"))]
            let _ = again;
        }
        #[cfg(any(not(unix), test))]
        let _ = (level, age, by);
    }

    /// Store `level` as this Mac's fleet access, the menu-bar lock's change
    /// (`crate::control::store_fleet_access`), and show it. A raise is stored only over the
    /// state this face last read: one that moved since (the level stated again elsewhere, the
    /// fleet left or changed) leaves the raise unwritten, and the state is read again. A write
    /// that failed is logged and the face stays at the level in force: the lock never shows a
    /// level the listeners do not enforce for longer than a pending raise's settle.
    #[cfg(any(target_os = "macos", test))]
    fn write_fleet_access(&mut self, level: aterm_types::fleet_access::FleetAccess) {
        let over = (level > self.lock_face.on_disk()).then(|| self.lock_face.basis());
        match crate::control::store_fleet_access(level, over) {
            Ok(true) => {
                self.lock_face
                    .wrote(level, crate::control::fleet_access_basis());
                self.observe_fleet_access(
                    level,
                    Some(std::time::Duration::ZERO),
                    Some(aterm_agent::fleet_access::By::Menu.said()),
                );
            }
            Ok(false) => {
                aterm_log::info!(
                    "status item: fleet access `{}` was not stored: the stored level or this \
                     Mac's fleet changed since the lock was clicked",
                    level.word()
                );
                self.reread_fleet_access();
            }
            Err(why) => {
                aterm_log::info!(
                    "status item: fleet access `{}` was not stored: {why}",
                    level.word()
                );
            }
        }
        self.mark_status_dirty();
    }

    /// The lock's face changed with nothing written (a raise began to wait, or was dropped):
    /// the menu-bar item repaints, and the fleet listener is told whether a raise waits.
    #[cfg(any(target_os = "macos", test))]
    fn lock_face_moved(&mut self) {
        self.mark_status_dirty();
        let (level, age) = crate::control::fleet_access_stated();
        self.observe_fleet_access(level, age, None);
    }

    /// A left click on the lock at `now` (`StatusAction::RotateFleetAccess`): one step on from
    /// the level it shows. A lower level is written at once and a raise waits its settle; a
    /// `repeated` click, a multi-click's second or later, lowers or cancels and never raises
    /// ([`crate::status_item::LockFace::click`], [`Self::tick_lock_face`]).
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn rotate_fleet_access(&mut self, now: std::time::Instant, repeated: bool) {
        self.reread_fleet_access();
        if let Some(level) = self.lock_face.click(now, repeated) {
            self.write_fleet_access(level);
        }
        self.lock_face_moved();
    }

    /// A level picked in the Fleet Access submenu (`StatusAction::SetFleetAccess`): written at
    /// once, and no raise is pending any more.
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn pick_fleet_access(&mut self, level: aterm_types::fleet_access::FleetAccess) {
        self.reread_fleet_access();
        if let Some(level) = self.lock_face.pick(level) {
            self.write_fleet_access(level);
        }
        self.lock_face_moved();
    }

    /// The lock's own clock at `now`: a pending raise whose settle has passed is written, where
    /// the state it was asked over still stands (it is read again first), and a click's level
    /// leaves the title once it is no longer fresh. A drop the fleet listener asked to be told
    /// of again is told ([`Self::observe_fleet_access`]). The event loop wakes for both
    /// ([`Self::lock_deadline`]).
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn tick_lock_face(&mut self, now: std::time::Instant) {
        let before = self.lock_face;
        if self.lock_face.raise_due(now) {
            self.reread_fleet_access();
        }
        if let Some(level) = self.lock_face.due(now) {
            self.write_fleet_access(level);
        }
        if self.lock_face != before {
            self.lock_face_moved();
        }
        #[cfg(target_os = "macos")]
        if self.access_look.is_some_and(|at| at <= now) {
            self.reread_fleet_access();
        }
    }

    /// When the lock next needs its clock ([`Self::tick_lock_face`]): a pending raise falls
    /// due, a click's level leaves the title, or a drop has rested.
    #[cfg(target_os = "macos")]
    pub(crate) fn lock_deadline(&self) -> Option<std::time::Instant> {
        [self.lock_face.next_deadline(), self.access_look]
            .into_iter()
            .flatten()
            .min()
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
            .filter(|(state, _)| state.has_an_end_to_read())
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
            "statusitem title={} needs={} stuck={} exceptions={} done={} fleet={fleet} access={} \
             shown={}",
            glance.button_title(),
            glance.need_count(),
            glance.stuck_count(),
            glance.exceptions.len(),
            glance.done.len(),
            glance.access.word(),
            glance.access_shown.word(),
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
            // The row's label can be the program's own title (DESIGN §3.6): the log names the
            // session.
            aterm_log::info!(
                "status item: session {}'s result did not open: {e}",
                result.session
            );
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
        // Opening the menu cancels a raise that has not taken effect, and reads the level
        // again, so the next open's checkmark is the file's.
        let before = self.lock_face;
        self.lock_face.menu_opened();
        self.reread_fleet_access();
        if self.lock_face != before {
            self.lock_face_moved();
        }
        self.notice_center.adopt_clicks();
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

    /// A DEVELOPMENT BUILD'S STAND-IN FOR THE PERSON LEFT-CLICKS THE MENU-BAR LOCK (`aterm ctl
    /// person menubar click`, beside [`Self::person_menubar_press`]): the click's action, posted
    /// as the button's handler posts it. It rotates whether or not this instance is in a fleet:
    /// a headless lab instance has no fleet glance, and its level is its scratch state's.
    #[cfg(any(test, debug_assertions))]
    pub(crate) fn person_menubar_click(
        &mut self,
        post: &mut dyn FnMut(crate::Wake) -> bool,
    ) -> Result<String, String> {
        #[cfg(not(target_os = "macos"))]
        {
            let _ = post;
            Err("no menu-bar item off macOS".to_string())
        }
        #[cfg(target_os = "macos")]
        {
            self.reread_fleet_access();
            let from = self.lock_face.shown();
            let action = crate::status_item::StatusAction::RotateFleetAccess { repeated: false };
            if !post(crate::Wake::StatusAction { action }) {
                return Err("event loop gone".to_string());
            }
            Ok(format!(
                "clicked the lock at {}: {} next",
                from.word(),
                from.rotated().word()
            ))
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
            StatusAction::HandBackAll => self.hand_back_all(),
            StatusAction::RotateFleetAccess { repeated } => {
                self.rotate_fleet_access(std::time::Instant::now(), repeated);
            }
            StatusAction::SetFleetAccess(level) => self.pick_fleet_access(level),
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
            StatusAction::OpenSystemState => {
                let _ = self.open_system_state_from_status_item();
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

    /// [`Self::focus_session_window`], in window `prefer` first where it shows `session`,
    /// keeping its active tab when that tab already shows the session.
    pub(crate) fn focus_session_preferring(
        &mut self,
        prefer: Option<WindowId>,
        session: u64,
    ) -> bool {
        let found = prefer
            .into_iter()
            .chain(self.windows.keys().copied())
            .find_map(|wid| {
                self.windows
                    .get(&wid)
                    .and_then(|window| {
                        let index = window.tab_set.active_index()?;
                        let tab = window.tab_set.active()?;
                        let mut found = None;
                        tab.root.any_leaf(&mut |&view| {
                            if self
                                .view_store
                                .get(view)
                                .copied()
                                .and_then(crate::tab_model::View::terminal_session)
                                == Some(session)
                            {
                                found = Some(view);
                                true
                            } else {
                                false
                            }
                        });
                        Some((wid, index, found?))
                    })
                    .or_else(|| {
                        self.terminal_view_location(wid, session)
                            .map(|location| (wid, location.canonical_index, location.view))
                    })
            });
        let Some((wid, index, view)) = found else {
            return false;
        };
        self.switch_tab_in(wid, index);
        self.focus_session_pane(wid, session, view);
        self.raise_window(wid);
        true
    }

    pub(crate) fn focus_session_pane(
        &mut self,
        wid: WindowId,
        session: u64,
        view: crate::tab_model::ViewId,
    ) {
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
        let exits_window = self.close_exits_window(wid, location);
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

    /// Whether closing the pane showing `session` would end its window: the only pane of that
    /// window's only tab, which asks before it goes. A session no window shows ends none.
    pub(crate) fn close_ends_window(&self, session: u64) -> bool {
        self.windows.keys().any(|wid| {
            self.terminal_view_location(*wid, session)
                .is_some_and(|location| self.close_exits_window(*wid, location))
        })
    }

    fn close_exits_window(&self, wid: WindowId, location: TerminalViewLocation) -> bool {
        location.terminal_only
            && !self.tab_has_sibling_panes(wid, location.tab_id)
            && self
                .windows
                .get(&wid)
                .is_some_and(|ws| ws.tab_set.len() <= 1)
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
        tab.root.any_leaf(&mut |&view| {
            let Some(session) = self
                .view_store
                .get(view)
                .copied()
                .and_then(crate::tab_model::View::terminal_session)
            else {
                return false;
            };
            let mut here = 0u32;
            tab.root.visit(&mut |&other| {
                if self
                    .view_store
                    .get(other)
                    .copied()
                    .and_then(crate::tab_model::View::terminal_session)
                    == Some(session)
                {
                    here += 1;
                }
            });
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
        self.install_tab_session_at(owner, session, parent, None, false, false);
    }

    /// [`Self::install_tab_session`] for a tab that lands behind the one at index `kept` (a wire `spawn` that does not raise).
    pub(crate) fn install_tab_session_behind(
        &mut self,
        owner: WindowId,
        session: crate::Session,
        parent: Option<aterm_session::SessionId>,
        kept: usize,
    ) {
        self.install_tab_session_at(owner, session, parent, Some(kept), false, false);
    }

    /// [`Self::install_tab_session`] for a worker: its tab lands parked and task-owned, behind what the window shows, and the sync that follows opens the window's Workers tab when this is its first. The pane remains after its task concludes. `fleet`: another Mac's drive asked for it ([`crate::tab_model::Tab::fleet`]).
    pub(crate) fn install_tab_session_parked(
        &mut self,
        owner: WindowId,
        session: crate::Session,
        parent: Option<aterm_session::SessionId>,
        fleet: bool,
    ) {
        let kept = self
            .windows
            .get(&owner)
            .and_then(|ws| ws.tab_set.active_index());
        self.install_tab_session_at(owner, session, parent, kept, true, fleet);
    }

    fn install_tab_session_at(
        &mut self,
        owner: WindowId,
        session: crate::Session,
        parent: Option<aterm_session::SessionId>,
        behind: Option<usize>,
        parked: bool,
        fleet: bool,
    ) {
        let id = session.id;
        Self::register_session(&self.store, &session, parent.clone());
        let layout = pane::PaneTree::new(id);
        let mut tab =
            crate::register_terminal_tab(&mut self.tab_ids, &mut self.view_store, &layout)
                .expect("tab/view identity space");
        tab.parked = parked;
        tab.ephemeral = parked;
        tab.fleet = parked && fleet;
        self.pool_insert(session);
        self.note_tab_open(id, owner, parked, parent.as_ref(), "new-tab");
        if let Some(ws) = self.windows.get_mut(&owner) {
            // By identity: a shown tab seated at the boundary moves a parked front along.
            let front = behind
                .and_then(|kept| ws.tab_set.tab_at(kept))
                .map(|t| t.id);
            land_terminal_tab(ws, &self.view_store, layout, tab).expect("fresh tab id");
            if let Some(front) = front {
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

    /// Cmd-1..9: the `i`th tab the strip draws; past them, nothing (a parked worker has no chip).
    pub(crate) fn switch_shown_tab_in(&mut self, wid: WindowId, i: usize) {
        if self
            .windows
            .get(&wid)
            .is_some_and(|ws| i < ws.tab_set.shown_len())
        {
            self.switch_tab_in(wid, i);
        }
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
        if ws.tab_set.shown_len() <= 1 && !ws.tab_set.active_is_parked() {
            return;
        }
        let Some(next) = ws.tab_set.cycle_shown(forward) else {
            return;
        };
        if let Some(index) = terminal_projection_index(&ws.tab_set, &self.view_store, next) {
            ws.tabs.switch_to(index);
        }
        if self.frontmost_window == Some(wid) {
            self.sync_active_session();
        } else {
            self.sync_window(wid);
        }
        self.palette_sync_native_scope(wid);
    }

    /// Deliver a native chip command, refusing stale identities before input side effects.
    #[cfg(any(target_os = "macos", all(test, unix)))]
    pub(crate) fn apply_native_tab_command(
        &mut self,
        window: WindowId,
        command: NativeTabCommand,
    ) -> bool {
        let tab = match command {
            NativeTabCommand::Select(tab)
            | NativeTabCommand::Close(tab)
            | NativeTabCommand::Move { tab, .. } => tab,
        };
        let Some(index) = self.tab_index_for_id(window, tab) else {
            return false;
        };
        let destination = if let NativeTabCommand::Move {
            neighbor, forward, ..
        } = command
        {
            let expected = if forward {
                index.checked_add(1)
            } else {
                index.checked_sub(1)
            };
            let Some(destination) = self.tab_index_for_id(window, neighbor) else {
                return false;
            };
            if expected != Some(destination) {
                return false;
            }
            Some(destination)
        } else {
            None
        };
        // A click on a chip types nothing; a change of front resets the glow on its own.
        self.fence_pointer_license(window);
        self.settle_rename_edit(window);
        match command {
            NativeTabCommand::Select(_) => self.switch_tab_in(window, index),
            NativeTabCommand::Close(_) => {
                if self.close_tab_at(window, index)
                    && let Some(ws) = self.windows.get_mut(&window)
                {
                    ws.pending_close = true;
                }
            }
            NativeTabCommand::Move { .. } => {
                if let Some(destination) = destination {
                    self.move_tab(window, index, destination);
                }
            }
        }
        true
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
                let shown = self
                    .windows
                    .get(&wid)
                    .map_or(count, |ws| ws.tab_set.shown_len());
                if (from < shown) != (to < shown) {
                    return Err(format!(
                        "tab move: {from} and {to} sit either side of the parked workers \
                         (from {shown}); a worker leaves them by Keep as Tab"
                    ));
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
        let stable_id = ws.tab_set.tab_at(from).expect("canonical tab in range").id;
        if !reorder_with_projection(ws, views, stable_id, |tabs| tabs.reorder(stable_id, to)) {
            return;
        }
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
        self.reap_stale_rename_edit(wid);
        let identities = LabelIdentities::capture(self, wid);
        let titles = self.tab_titles_from_identities(wid, &identities);
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
            .and_then(|ws| ws.tab_set.strip_active())
            .unwrap_or(0);
        let shown = self
            .windows
            .get(&wid)
            .map_or(titles.len(), |ws| ws.tab_set.shown_len());
        let ids = &identities.ids;
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.strip_shadow.set((shown, active));
            ws.tab_title_epochs.retain(|sid, _| {
                #[cfg(test)]
                crate::work_counts::title_cache_probed();
                identities.live_sessions.contains(sid)
            });
            ws.tab_chrome_titles.retain(|sid, _| {
                #[cfg(test)]
                crate::work_counts::title_cache_probed();
                identities.live_sessions.contains(sid)
            });
            for (session, title) in identities.sessions.iter().zip(&titles) {
                if let Some(session) = session {
                    #[cfg(test)]
                    crate::work_counts::title_cache_copy(title.len());
                    ws.tab_chrome_titles
                        .entry(*session)
                        .or_default()
                        .clone_from(title);
                }
            }
            ws.tab_chrome_titles_by_tab.retain(|tab_id, _| {
                #[cfg(test)]
                crate::work_counts::title_cache_probed();
                identities.live_tabs.contains(tab_id)
            });
            for ((id, session), title) in ids
                .iter()
                .zip(&identities.sessions)
                .zip(chrome_titles.iter())
            {
                #[cfg(test)]
                crate::work_counts::title_cache_copy(title.len());
                let entry = ws
                    .tab_chrome_titles_by_tab
                    .entry(*id)
                    .or_insert_with(|| (*session, String::new()));
                entry.0 = *session;
                entry.1.clone_from(title);
            }
        }
        if let Some(handle) = self._toolbars.get(&wid) {
            self.apprt.set_toolbar_tabs(
                handle,
                crate::platform::ToolbarTabsModel {
                    titles: &chrome_titles[..shown.min(chrome_titles.len())],
                    ids: &ids[..shown.min(ids.len())],
                    metadata: &metadata[..shown.min(metadata.len())],
                    tooltips: &tooltips[..shown.min(tooltips.len())],
                    ext: &ext[..shown.min(ext.len())],
                    active,
                },
            );
        }
        // What the strip was given: one tip per shown tab, never a parked worker's.
        #[cfg(any(test, debug_assertions))]
        if let Some(ws) = self.windows.get_mut(&wid) {
            let mut pushed = tooltips;
            pushed.truncate(shown);
            ws.tooltips_pushed = pushed;
        }
        self.mark_status_dirty();
        (titles, metadata)
    }

    /// Repaint everything a session's user metadata feeds, after the change was stored and recorded.
    pub(crate) fn refresh_meta_dependent_chrome(&mut self, session: u64) {
        for wid in self.windows_with_focused_session(session) {
            let focused = self.focused_session_id(wid) == Some(session);
            let (titles, metadata) = self.refresh_window_tabs(wid);
            let fp = self.tab_strip_fingerprint_from_parts(wid, &titles, &metadata);
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

    /// Close the focused pane of the frontmost window's active tab.
    pub(crate) fn close_active_tab(&mut self) -> Option<WindowId> {
        self.close_active_tab_in(self.frontmost_window?)
    }

    /// Close the focused pane of the originating window's active tab.
    pub(crate) fn close_active_tab_in(&mut self, window: WindowId) -> Option<WindowId> {
        let _closing = session_store::CloseAttribution::enter(
            session_store::ExitReason::UiClose,
            session_store::ExitActor::Human,
        );
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
            // A mixed tab's terminal leaf hangs up its job like any pane's, so it asks first.
            let busy = self
                .view_store
                .get(stable_view)
                .copied()
                .and_then(crate::tab_model::View::terminal_session)
                .is_some_and(|session| self.pane_close_hangs_up_a_job(session));
            let replay = crate::close_confirm::CloseReplay::Pane {
                wid: window,
                tab: stable_tab,
                view: stable_view,
            };
            if !self.view_close_allowed(window, busy, replay) {
                return None;
            }
            let _ = self.close_focused_mixed_leaf(window);
            return None;
        }
        if self.active_native_view(window).is_some() {
            if self.workers_tab_holds(window, stable_tab) {
                self.workers_tab_close_refused();
                return None;
            }
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
        let tab = self.windows.get(&window).map_or(0, |ws| ws.tabs.active);
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

    /// Replay a pre-rollback focused-view close (or a confirmed close question's pane close)
    /// against stable identities, from behind: the window's front tab stays in front and
    /// every tab keeps its focused leaf (a closed leaf's heir takes its place), with no sync
    /// of another tab, except that a native leaf whose close asks first (its recovery
    /// palette) brings its tab forward, focused on that leaf.
    pub(crate) fn replay_deferred_handoff_view_close(
        &mut self,
        window: WindowId,
        tab: crate::tab_model::TabId,
        view: crate::tab_model::ViewId,
    ) -> Option<WindowId> {
        let mixed = {
            let target = self.windows.get(&window)?.tab_set.get(tab)?;
            if !target.root.contains(view) {
                return None;
            }
            !is_terminal_tab(target, &self.view_store)
        };
        // A native leaf has no job to ask about: it closes as it would have, from behind.
        let session = match self.view_store.get(view).copied() {
            Some(crate::tab_model::View::Native(_)) => {
                let last = self.windows.get(&window).is_some_and(|state| {
                    state.tab_set.len() == 1
                        && state
                            .tab_set
                            .get(tab)
                            .is_some_and(|target| target.root.len() == 1)
                });
                return self
                    .close_view_keeping_front(window, tab, view)
                    .is_ok()
                    .then_some(window)
                    .filter(|_| last);
            }
            Some(crate::tab_model::View::Terminal(terminal)) => terminal.session,
            None => return None,
        };
        // A terminal leaf closes from behind too, asking first where ⌘W on it would ask (a
        // confirmed question replays under `CloseConfirm::Programmatic`, which asks nothing).
        let _closing = session_store::CloseAttribution::enter(
            session_store::ExitReason::UiClose,
            session_store::ExitActor::Human,
        );
        if self.defer_pending_update_handoff_teardown(crate::DeferredHandoffTeardown::mutation(
            crate::DeferredHandoffMutation::CloseView { window, tab, view },
        )) {
            return None;
        }
        if !mixed {
            return self.close_terminal_pane_from_behind(window, tab, view, session);
        }
        let busy = self.pane_close_hangs_up_a_job(session);
        let replay = crate::close_confirm::CloseReplay::Pane {
            wid: window,
            tab,
            view,
        };
        if self.view_close_allowed(window, busy, replay) {
            let _ = self.close_view_keeping_front(window, tab, view);
        }
        None
    }

    /// Close `view`, the pane showing `session` in all-terminal tab `tab` of `window`, as ⌘W
    /// closes it (the whole tab when it is the tab's one pane), by its pane rather than by
    /// focus. Neither the window's front tab nor any tab's focus moves for it, so the close's
    /// one sync finds the tab and leaf that were in front still there. Returns `window` when
    /// the pane was the window's last.
    fn close_terminal_pane_from_behind(
        &mut self,
        window: WindowId,
        tab: crate::tab_model::TabId,
        view: crate::tab_model::ViewId,
        session: u64,
    ) -> Option<WindowId> {
        let (index, exits_window, siblings) = self.windows.get(&window).and_then(|state| {
            let index = terminal_projection_index(&state.tab_set, &self.view_store, tab)?;
            let tree = state
                .layouts
                .get(index)
                .filter(|tree| tree.contains(session))?;
            Some((
                index,
                state.tab_set.len() <= 1 && tree.len() <= 1,
                tree.len() > 1,
            ))
        })?;
        if !self.window_exit_close_allowed(window, exits_window) {
            return None;
        }
        if !exits_window {
            let busy = self.pane_close_hangs_up_a_job(session);
            let replay = if siblings {
                crate::close_confirm::CloseReplay::Pane {
                    wid: window,
                    tab,
                    view,
                }
            } else {
                crate::close_confirm::CloseReplay::Tab { wid: window, tab }
            };
            if !self.view_close_allowed(window, busy, replay) {
                return None;
            }
        }
        let recovery = self.closed_view_record_for(window, tab, view);
        let outcome = self
            .windows
            .get_mut(&window)
            .and_then(|state| state.layouts.get_mut(index))
            .map(|tree| tree.close_pane(session))?;
        let collapsed = matches!(outcome, pane::CloseOutcome::Collapsed { .. });
        let closes_window = self.apply_close_outcome(window, index, outcome);
        if collapsed
            && self.view_store.get(view).is_none()
            && let Some(recovery) = recovery
        {
            self.retain_closed_view(recovery);
        }
        if closes_window {
            self.stash_deferred_close_attribution(window);
        }
        closes_window.then_some(window)
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
        // An open grid cell commits first; a refused commit keeps the view open.
        if !self.commit_grid_draft_before_close(view) {
            return Ok(false);
        }
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

    /// Every native leaf of window `wid`, tab by tab.
    fn native_leaves_of(
        &self,
        wid: WindowId,
    ) -> Vec<(
        crate::tab_model::TabId,
        crate::tab_model::ViewId,
        crate::tab_model::NativeViewRef,
    )> {
        let Some(window) = self.windows.get(&wid) else {
            return Vec::new();
        };
        let mut views = Vec::new();
        for tab in window.tab_set.tabs() {
            tab.root.visit(&mut |&view| {
                if let Some(crate::tab_model::View::Native(native)) =
                    self.view_store.get(view).copied()
                {
                    views.push((tab.id, view, native));
                }
            });
        }
        views
    }

    fn prepare_window_native_shutdown_with(
        &mut self,
        wid: WindowId,
        scope: crate::native_app::CloseScope,
        visibility: ClosePreflightVisibility,
    ) -> Result<bool, String> {
        if !self.windows.contains_key(&wid) {
            return Err("unknown native-close window".to_string());
        }
        let leaves = self.native_leaves_of(wid);
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
        let windows = self.windows.keys().copied().collect::<Vec<_>>();
        for wid in windows {
            if !self.prepare_window_native_shutdown_with(wid, scope, visibility)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub(crate) fn prepare_quit_native_shutdown(&mut self) -> Result<bool, String> {
        let Some(cause) = self.quit_unattended else {
            return self.prepare_all_native_shutdown(
                crate::native_app::CloseScope::AppQuit,
                ClosePreflightVisibility::Interactive,
            );
        };
        // Nobody can press a tab's Review or Discard, and the process ends at its backstop
        // whatever is pressed: each tab that would have held the quit is named, and its unsaved
        // work ends with the process, now rather than at the backstop.
        let windows = self.windows.keys().copied().collect::<Vec<_>>();
        for wid in windows {
            let leaves = self.native_leaves_of(wid);
            for (tab, view, native) in leaves {
                if !self.prepare_native_leaf_close(
                    wid,
                    tab,
                    view,
                    native,
                    crate::native_app::CloseScope::AppQuit,
                    ClosePreflightVisibility::Quiet,
                )? {
                    // The leaf's title names the document it holds: the log names the tab by its
                    // place and its app's kind.
                    let place = self
                        .windows
                        .get(&wid)
                        .and_then(|window| window.tab_set.tabs().iter().position(|t| t.id == tab));
                    let kind = self
                        .native_runtime
                        .app(native.instance)
                        .map_or("unknown", |app| app.kind().as_str());
                    aterm_log::warn!(
                        "quit: tab {} ({kind}) held unsaved work nobody could save or discard \
                         ({cause}); it ends with this aterm",
                        place.map_or_else(|| "?".to_string(), |at| (at + 1).to_string())
                    );
                }
            }
        }
        Ok(true)
    }

    /// Close one focused leaf of a heterogeneous tab as a host transaction.
    pub(crate) fn close_focused_mixed_leaf(&mut self, wid: WindowId) -> Result<(), String> {
        self.close_focused_mixed_leaf_keeping(wid, None, None)
    }

    /// [`Self::close_focused_mixed_leaf`], then `keep` in front where it is given and still
    /// stands. `heir`, where it is given and still a leaf, takes the tab's focus in place of
    /// the closed leaf's neighbour.
    pub(crate) fn close_focused_mixed_leaf_keeping(
        &mut self,
        wid: WindowId,
        keep: Option<crate::tab_model::TabId>,
        heir: Option<crate::tab_model::ViewId>,
    ) -> Result<(), String> {
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
            .map(|tab| {
                let removed = tab.remove_view(view);
                if removed == crate::tab_model::RemoveLeaf::Removed
                    && let Some(heir) = heir
                {
                    let _ = tab.set_focus(heir);
                }
                removed
            });
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
        self.resize_panes_after_collapse(wid);
        // The tab was selected for its close only: the tab in front goes back before the sync.
        if let Some(keep) = keep.filter(|keep| *keep != tab_id)
            && let Some(state) = self.windows.get_mut(&wid)
            && state.tab_set.switch_to(keep)
        {
            align_terminal_projection_to_active(state, &self.view_store);
        }
        if self.frontmost_window == Some(wid) {
            self.sync_active_session();
        } else {
            self.sync_window(wid);
        }
        Ok(())
    }

    /// Close `view`, a leaf of `tab` in `wid`, as ⌘W closes it: the whole tab when it is the
    /// tab's one leaf, else that leaf. The tab is selected for the close's preflight only,
    /// with no sync: the tab in front before is put back in the same topology edit, and after
    /// a refused close by [`Self::restore_front_after_close`].
    pub(crate) fn close_view_keeping_front(
        &mut self,
        wid: WindowId,
        tab: crate::tab_model::TabId,
        view: crate::tab_model::ViewId,
    ) -> Result<(), String> {
        let (front, split, focus) = self
            .windows
            .get(&wid)
            .and_then(|state| {
                let target = state.tab_set.get(tab).filter(|t| t.root.contains(view))?;
                let front = state.tab_set.active_id().filter(|front| *front != tab);
                Some((front, target.root.len() > 1, target.focus))
            })
            .ok_or_else(|| "the view could not take focus to close".to_string())?;
        // The leaf the person had focused keeps the focus the close borrowed.
        let focus = Some(focus).filter(|focus| *focus != view);
        if let Some(state) = self.windows.get_mut(&wid)
            && state.tab_set.switch_to(tab)
            && let Some(target) = state.tab_set.active_mut()
        {
            let _ = target.set_focus(view);
        }
        let closed = if split {
            self.close_focused_mixed_leaf_keeping(wid, front, focus)
        } else {
            self.close_active_native_tab_keeping(wid, front)
        };
        if closed.is_err() {
            self.restore_front_after_close(wid, tab, front, focus);
        }
        closed
    }

    /// Put `front` back in front of `wid` after a close that switched to `tab` from behind
    /// it was refused and left `tab` standing, and `focus` back as `tab`'s focus where the
    /// close moved it, then resync. The tab and the leaf in question keep both only while
    /// the window's palette asks about closing one of its leaves (a blocked native close),
    /// which needs them.
    pub(crate) fn restore_front_after_close(
        &mut self,
        wid: WindowId,
        tab: crate::tab_model::TabId,
        front: Option<crate::tab_model::TabId>,
        focus: Option<crate::tab_model::ViewId>,
    ) {
        let asks = self.windows.get(&wid).is_some_and(|state| {
            state.tab_set.get(tab).is_some_and(|target| {
                target.root.any_leaf(&mut |view| {
                    state
                        .palette()
                        .is_some_and(|palette| palette.is_native_close_recovery_for(wid, *view))
                })
            })
        });
        if !asks
            && let Some(focus) = focus
            && let Some(state) = self.windows.get_mut(&wid)
            && let Some(index) = state.tab_set.tabs().iter().position(|t| t.id == tab)
            && let Some(target) = state.tab_set.tab_at_mut(index)
        {
            let _ = target.set_focus(focus);
        }
        if let Some(front) = front.filter(|front| *front != tab && !asks)
            && let Some(state) = self.windows.get_mut(&wid)
            && state.tab_set.switch_to(front)
        {
            align_terminal_projection_to_active(state, &self.view_store);
        }
        self.resync_active_or_window(wid);
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
        if self.workers_tab_holds(wid, tab_id) {
            return Err(crate::app_workers::WORKERS_TAB_STAYS.to_string());
        }
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
        let (tab, was_last, was_workers) = {
            let ws = self
                .windows
                .get_mut(&wid)
                .ok_or_else(|| "unknown window".to_string())?;
            let was_last = ws.tab_set.len() == 1;
            let was_workers = ws.tab_set.home() == Some(tab_id);
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
            (tab, was_last, was_workers)
        };
        if was_last {
            self.stash_deferred_close_attribution(wid);
        }
        // The Workers tab closes only with no worker left for it: its Finished rows leave the
        // window's page with it, or the sync would open it again for them; what they left stays
        // for their managers to read, as Clear Finished keeps it.
        if was_workers {
            self.store
                .write()
                .unwrap_or_else(|p| p.into_inner())
                .retired_workers_mut()
                .unlist_window(wid);
        }
        self.retiring_edit_leaves_in(wid, |app| {
            tab.root.visit(&mut |&view| {
                app.remove_view_link_for_close(view, crate::native_app::CloseScope::Tab);
            });
        });
        for session in terminal_sessions {
            self.teardown_session(session);
        }
        if let Some(tab) = reopen {
            self.retain_closed_tab(wid, original_index, tab);
        }
        // A retiring leaf can hand the window to another tab (an `edit` view gives it to
        // its caller's tab): the kept tab goes back in front before the sync all the same.
        if let Some(keep) = keep.filter(|keep| *keep != tab_id)
            && let Some(ws) = self.windows.get_mut(&wid)
            && ws.tab_set.active_id() != Some(keep)
            && ws.tab_set.switch_to(keep)
        {
            align_terminal_projection_to_active(ws, &self.view_store);
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
                self.reconcile_view_topology_projection(
                    wid,
                    candidate.value.original_tab,
                    prior_projection,
                );
                if let Some(window) = self.windows.get_mut(&wid) {
                    window.tab_set.switch_to(candidate.value.original_tab);
                    align_terminal_projection_to_active(window, &self.view_store);
                }
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
                    parked: false,
                    ephemeral: false,
                    fleet: false,
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

    /// Reconcile a preexisting terminal projection after a tab's leaf topology changes.
    fn reconcile_view_topology_projection(
        &mut self,
        wid: WindowId,
        tab_id: crate::tab_model::TabId,
        prior_projection: Option<usize>,
    ) {
        let Some(projection) = prior_projection else {
            return;
        };
        let Some(rebuilt) = self.windows.get(&wid).and_then(|window| {
            let tab = window.tab_set.get(tab_id)?;
            if !is_terminal_tab(tab, &self.view_store) {
                return Some(None);
            }
            let layout = self.restored_terminal_pane_layout(&tab.root, tab.focus)?;
            let mut sessions = Vec::with_capacity(tab.root.len());
            tab.root.visit(&mut |&view| {
                if let Some(session) = self
                    .view_store
                    .get(view)
                    .copied()
                    .and_then(crate::tab_model::View::terminal_session)
                {
                    sessions.push(session);
                }
            });
            let mut tree = pane::PaneTree::rebuild(&layout, &sessions)?;
            if tab.zoomed {
                tree.toggle_zoom();
            }
            Some(Some(tree))
        }) else {
            return;
        };
        let Some(tree) = rebuilt else {
            if let Some(window) = self.windows.get_mut(&wid)
                && projection < window.layouts.len()
            {
                window.layouts.remove(projection);
                remove_terminal_projection(window, projection);
            }
            return;
        };
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
        if self.finish_developer_tools_install(session) || self.keep_shell_failed_at_start(session)
        {
            self.settle_log_close(session);
            return Vec::new();
        }
        let mut owners = Vec::new();
        for (&window, state) in &self.windows {
            for tab in state.tab_set.tabs() {
                tab.root.visit(&mut |&view| {
                    if self
                        .view_store
                        .get(view)
                        .copied()
                        .and_then(crate::tab_model::View::terminal_session)
                        == Some(session)
                    {
                        owners.push(window);
                    }
                });
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

    /// A click in window `wid`'s tab strip at column `col`, pressed at `at`.
    pub(crate) fn handle_tab_strip_click(
        &mut self,
        wid: WindowId,
        col: u16,
        at: std::time::Instant,
    ) {
        let Some(segs) = self.windows.get(&wid).map(|ws| ws.tab_segments.clone()) else {
            return;
        };
        let Some(hit) = tab_bar::hit_test(&segs, col) else {
            let band_repeat = self.windows.get(&wid).is_some_and(|ws| {
                ws.strip_band_press.is_some_and(|last| {
                    at.saturating_duration_since(last).as_millis() <= crate::multi_click_ms()
                })
            });
            self.clear_strip_press(wid);
            self.settle_rename_edit(wid);
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.strip_band_press = (!band_repeat).then_some(at);
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
                let repeat = tab.is_some_and(|tab| {
                    self.windows.get(&wid).is_some_and(|ws| {
                        ws.strip_press.is_some_and(|(last, was)| {
                            was == tab
                                && at.saturating_duration_since(last).as_millis()
                                    <= crate::multi_click_ms()
                        })
                    })
                });
                let editing_this = self
                    .inline_rename_edit(wid)
                    .map(|edit| edit.tab)
                    .is_some_and(|edited| Some(edited) == tab);
                if let Some(ws) = self.windows.get_mut(&wid) {
                    ws.strip_press = tab.map(|tab| (at, tab));
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
        if self.inline_rename_edit(wid).is_some() {
            self.clear_strip_press(wid);
            self.settle_rename_edit(wid);
        }
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
        let Some(origin) = self.windows.get(&wid).and_then(|ws| {
            let tab = ws.tab_set.tabs().get(index)?;
            self.view_store
                .get(tab.focus)
                .copied()
                .and_then(crate::tab_model::View::terminal_session)
                .map(|session| crate::session_chrome::TerminalMenuOrigin {
                    view: tab.focus,
                    session,
                })
        }) else {
            return Vec::new();
        };
        let titles = self.tab_titles(wid);
        let label = crate::chrome_label::capped_label(titles.get(index).map_or("", String::as_str));
        let mut ext = self.composed_session_chrome(wid, origin.session, &label, &mut None);
        ext.bind_terminal_origin(origin);
        crate::session_chrome::menu_shown_at(&ext.menu, crate::turn_ledger::now_ms())
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
        // The Workers chip's press means the worker it shows, without going back to the list.
        let index = self
            .windows
            .get(&wid)
            .and_then(|ws| {
                let tab = ws.tab_set.tabs().get(index)?;
                let subject = chip_subject(ws, tab).id;
                ws.tab_set.tabs().iter().position(|tab| tab.id == subject)
            })
            .unwrap_or(index);
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
        let Some((index, chip)) = self
            .windows
            .get(&wid)
            .and_then(|ws| Some((ws.tab_set.active_index()?, ws.tab_set.strip_active()?)))
        else {
            return false;
        };
        let anchor = self.tab_chip_anchor_col(wid, chip);
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

    /// Window `wid`'s open tab menu for its accessibility tree, where the last frame laid it out.
    #[cfg(a11y_tree)]
    pub(crate) fn tab_grid_menu(&self, wid: WindowId) -> Option<crate::a11y_tree::GridMenu> {
        use std::hash::{Hash, Hasher};
        let ws = self.windows.get(&wid)?;
        let menu = ws.tab_menu.as_ref()?;
        let rect = ws.tab_menu_rect?;
        let top = rect.row.checked_sub(usize::from(self.chrome_rows(wid)))?;
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        menu.tab.hash(&mut hash);
        let mut items = Vec::new();
        let mut selected = None;
        for (i, entry) in crate::tab_menu::reader_rows(&menu.entries, rect) {
            let label = crate::tab_menu::entry_text(entry).to_owned();
            label.hash(&mut hash);
            if menu.highlight == Some(i) {
                selected = Some(items.len());
            }
            items.push(crate::a11y_tree::GridMenuItem {
                label,
                checked: None,
                disabled: !crate::tab_menu::is_selectable(entry),
                row: top + 1 + i,
                slider: None,
            });
        }
        Some(crate::a11y_tree::GridMenu {
            label: "Tab menu".to_owned(),
            epoch: crate::a11y_tree::menu_epoch_from(hash.finish()),
            items,
            selected,
            row: top,
            col: rect.col,
            width: rect.w,
            height: rect.h,
        })
    }

    /// Activate `item` from a screen reader if `epoch` matches the painted tab menu.
    #[cfg(a11y_tree)]
    pub(crate) fn choose_tab_menu_item_for_reader(
        &mut self,
        wid: WindowId,
        epoch: u32,
        item: usize,
    ) {
        if self
            .tab_grid_menu(wid)
            .is_none_or(|menu| menu.epoch != epoch)
        {
            return;
        }
        let entry = self.windows.get(&wid).and_then(|ws| {
            let (menu, rect) = (ws.tab_menu.as_ref()?, ws.tab_menu_rect?);
            crate::tab_menu::reader_rows(&menu.entries, rect)
                .nth(item)
                .filter(|(_, entry)| crate::tab_menu::is_selectable(entry))
                .map(|(i, _)| i)
        });
        if let Some(i) = entry {
            self.activate_tab_menu_entry(wid, i);
        }
    }

    /// Activate entry `i` of the open menu on `wid` and dismiss the card.
    pub(crate) fn activate_tab_menu_entry(&mut self, wid: WindowId, i: usize) {
        let session_action = self.windows.get(&wid).and_then(|ws| {
            let menu = ws.tab_menu.as_ref()?;
            match menu.entries.get(i)? {
                crate::session_chrome::TabMenuEntry::SessionAction {
                    action,
                    enabled: true,
                    origin: Some(origin),
                    ..
                } => Some((menu.tab, *origin, *action)),
                _ => None,
            }
        });
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
        let rewind_row = self.windows.get(&wid).and_then(|ws| {
            let menu = ws.tab_menu.as_ref()?;
            match menu.entries.get(i)? {
                crate::session_chrome::TabMenuEntry::RewindTo {
                    at_ms,
                    origin: Some(origin),
                    ..
                } => Some((menu.tab, *origin, *at_ms)),
                _ => None,
            }
        });
        self.close_tab_menu(wid);
        if let Some((tab, origin, action)) = session_action {
            self.dispatch_tab_menu_session_action(wid, tab, origin, action);
            return;
        }
        if let Some((tab, origin, at_ms)) = rewind_row {
            self.dispatch_tab_menu_rewind(wid, tab, origin, at_ms);
            return;
        }
        if let Some((tab, sid, to, word)) = upgrade {
            self.dispatch_tab_menu_upgrade(wid, tab, sid, to, word);
        }
        if let Some((tab, peer_sid, verb)) = session_row {
            self.dispatch_tab_menu_connection(wid, tab, &peer_sid, verb);
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
        let Some((tab_id, is_native, has_terminal, terminal_index, canonical_count)) =
            self.windows.get(&wid).and_then(|ws| {
                let tab = ws.tab_set.tab_at(i)?;
                Some((
                    tab.id,
                    !is_terminal_tab(tab, &self.view_store),
                    tab.root.any_leaf(&mut |view| {
                        matches!(
                            self.view_store.get(*view),
                            Some(crate::tab_model::View::Terminal(_))
                        )
                    }),
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
            // A mixed tab's terminal leaves hang up their jobs as a terminal tab's do, so it
            // asks as one does.
            if has_terminal {
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
            }
            let mut front = None;
            if let Some(ws) = self.windows.get_mut(&wid) {
                front = ws.tab_set.active_id().filter(|front| *front != tab_id);
                ws.tab_set.switch_to(tab_id);
            }
            return match self.close_active_native_tab_keeping(wid, front) {
                Ok(()) => canonical_count == 1,
                Err(why) => {
                    self.restore_front_after_close(wid, tab_id, front, None);
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
        let closing = ws.layouts.remove(projection_index);
        remove_terminal_projection(ws, projection_index);
        let stable_tab = ws.tab_set.remove(tab_id).expect("stable tab mirror exists");
        align_terminal_projection_to_active(ws, &self.view_store);
        self.remove_tab_views(&stable_tab);
        closing.visit_sessions(&mut |sid| {
            if self.detach_session_view(sid) {
                self.retire_session_registration(sid);
            }
        });
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
            let terminal_tabs = || {
                ws.tab_set
                    .tabs()
                    .iter()
                    .filter(|tab| is_terminal_tab(tab, &self.view_store))
            };
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
            let active_terminal = active_tab
                .and_then(|active| terminal_tabs().position(|candidate| candidate.id == active.id));
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
            let terminal_tab_count = terminal_tabs().count();
            if terminal_tab_count != ws.layouts.len() {
                return Some(format!(
                    "{wid:?}: {terminal_tab_count} all-terminal tabs project onto {} layouts",
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
            for (index, (layout, tab)) in ws.layouts.iter().zip(terminal_tabs()).enumerate() {
                if !layout.matches_projected(&tab.root, &|view| {
                    self.view_store
                        .get(view)
                        .copied()
                        .and_then(crate::tab_model::View::terminal_session)
                }) {
                    let projected = tab.root.map(&mut |view| {
                        self.view_store
                            .get(*view)
                            .copied()
                            .and_then(crate::tab_model::View::terminal_session)
                    });
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
                let mut invalid = None;
                tab.root.any_leaf(&mut |&view| {
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
                        invalid = Some(format!(
                            "{wid:?}: tab {:?}'s leaf {view:?} ({:?}) is not owned by the session \
                             pool / native runtime",
                            tab.id,
                            self.view_store.get(view).copied(),
                        ));
                        true
                    } else {
                        false
                    }
                });
                if let Some(err) = invalid {
                    return Some(err);
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
                    let mut found = None;
                    ws.tab_set.get(tab_id)?.root.any_leaf(&mut |&view| {
                        if self
                            .view_store
                            .get(view)
                            .copied()
                            .and_then(crate::tab_model::View::terminal_session)
                            == Some(closed_session)
                        {
                            found = Some(view);
                            true
                        } else {
                            false
                        }
                    });
                    found
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
                self.resize_panes_after_collapse(wid);
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
                let Some((closing, stable_tab)) = self.windows.get_mut(&wid).map(|ws| {
                    let closing = ws.layouts.remove(tab);
                    remove_terminal_projection(ws, tab);
                    let removed = ws.tab_set.remove(stable_id);
                    align_terminal_projection_to_active(ws, &self.view_store);
                    (closing, removed)
                }) else {
                    return false;
                };
                if let Some(stable_tab) = stable_tab {
                    self.remove_tab_views(&stable_tab);
                }
                closing.visit_sessions(&mut |sid| {
                    self.teardown_session(sid);
                });
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
        self.restored_workers.remove(&id);
        #[cfg(target_os = "macos")]
        self.withdraw_askpass_session(id);
        let (reason, by) = session_store::current_close_attribution();
        let (closing, instance) = {
            let store = self.store.read().unwrap_or_else(|p| p.into_inner());
            let held = store.by_local(id).map(|h| {
                (
                    std::sync::Arc::clone(&h.term),
                    std::sync::Arc::clone(&h.ctx),
                )
            });
            (held, store.instance_watchers())
        };
        // The close's last visit, before the log closes: a command the engine finished since the
        // sweep's last visit is logged, not lost uncounted (unit J1-1, SMOKE-3c).
        let closing = closing.map(|(term, ctx)| {
            crate::session_status::last_visit(&term, &ctx);
            ctx
        });
        // Where its task stood, for the request that may own the tab (ATERM-KI-1109-3).
        let task_end = closing.as_ref().and_then(|ctx| {
            crate::requests_host::task_end(
                ctx.timeline
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .task(),
            )
        });
        // What its process tree spent, as the last read before the close found it: the close
        // takes the log's spend, and the request keeps its own copy (ATERM-KI-1109-3, step 4).
        let tree_spent = closing.as_ref().and_then(|ctx| {
            ctx.timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .slog()
                .tree_spent()
        });
        let tasked = task_end.is_some();
        // A worker whose tab closes tells its manager while it is still on this Mac's roster.
        if tasked && let Some(ctx) = closing.as_ref() {
            let _ = self.mail_worker_close(ctx);
        }
        let held = (
            closing.as_ref().map(|ctx| ctx.watchers.hold_rings()),
            instance.hold_rings(),
        );
        let deregistered = self
            .store
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .deregister_closing(id, reason, by.clone());
        let (stable, exit_seq) = match deregistered {
            Some(closed) => {
                drop(closed.freed);
                if let Some(reason) = closed.reason {
                    crate::requests_host::left(crate::requests_host::TabLeft {
                        sid: closed.sid.clone(),
                        reason,
                        code: closed.exit_code,
                        task: task_end,
                        by,
                        spent: tree_spent.map(crate::session_log::TreeSpent::as_spent),
                    });
                }
                (Some(closed.sid), closed.exit_seq)
            }
            None => (None, None),
        };
        // Out of the store now, so a needs pusher's next row no longer lists it.
        if stable.is_some() {
            crate::control::needs_moved();
        }
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
        // The exit edge pulls one reading, so the leftover pass keeps the closed tab's horizon
        // from the reading at its close (leftover §5.3).
        self.vitals.note_tab_closed();
        self.forget_harness_glass(id);
        self.autopilot_timers.forget(id);
        self.claude_facts.remove(&id);
        self.autopilot_recorded.remove(&id);
        self.spare_poke();
        if let Some(stable) = stable {
            self.permission_clicks.clicks.close_tab(stable.as_str());
            self.resume_stories.remove(stable.as_str());
            crate::occupancy_wakes::forget(stable.as_str());
            crate::self_claim::forget_read(stable.as_str());
            self.end_occupancy_lines_of(stable.as_str());
            if tasked {
                // A worker aterm closed keeps its results while its retired row stands.
                let kept = self
                    .store
                    .read()
                    .unwrap_or_else(|p| p.into_inner())
                    .retired_workers()
                    .by_sid(&stable)
                    .is_some();
                crate::task_launch::retire_task_file(&stable, kept);
            }
            crate::proxy::unpublish_session(&stable);
            crate::connections::sweep_session_closed_in(&self.connections, &stable, &self.store);
            self.overview_refresh_all();
        }
    }
}

#[cfg(test)]
#[path = "app_tabs_system_state_runtime_tests.rs"]
mod system_state_runtime_tests;

#[cfg(test)]
#[path = "app_tabs_mixed_tab_tests.rs"]
mod mixed_tab_tests;

#[cfg(test)]
#[path = "app_tabs_home_abbreviation_tests.rs"]
mod home_abbreviation_tests;

#[cfg(test)]
#[path = "app_tabs_session_chrome_app_tests.rs"]
mod session_chrome_app_tests;

#[cfg(test)]
#[path = "app_tabs_rename_strip_click_tests.rs"]
mod rename_strip_click_tests;

#[cfg(test)]
#[path = "app_tabs_status_glance_tests.rs"]
mod status_glance_tests;

#[cfg(test)]
#[path = "app_tabs_tab_context_menu_tests.rs"]
mod tab_context_menu_tests;

fn status_unix_now() -> i64 {
    aterm_time::unix_secs().map_or(0, |s| i64::try_from(s).unwrap_or(0))
}

#[cfg(test)]
#[path = "app_tabs_connected_spawn_tests.rs"]
mod connected_spawn_tests;

#[cfg(test)]
#[path = "app_tabs_exit_attribution_tests.rs"]
mod exit_attribution_tests;

#[cfg(test)]
#[path = "app_tabs_pane_close_render_seam_tests.rs"]
mod pane_close_render_seam_tests;

#[cfg(all(test, unix))]
#[path = "app_tabs_shell_start_exit_tests.rs"]
mod shell_start_exit_tests;

#[cfg(test)]
#[path = "status_end_tests.rs"]
mod status_end_tests;
