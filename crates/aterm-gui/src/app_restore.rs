// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Session layout capture, sliced cold-launch restoration, and seamless-update handoff adoption.

use crate::app_tabs::{restored_terminal_index, terminal_projection_index, terminal_tab_id_at};
use crate::spawn::spawn_session;
use crate::{App, Session, TabIndex, WindowId, WindowState, pane, restore};
use aterm_window::event_loop::ActiveEventLoop;

type BuiltRestoreTab = (crate::tab_model::Tab, Option<pane::PaneTree>);

#[path = "app_closed_reopen.rs"]
pub(crate) mod closed;
#[path = "app_restore_build.rs"]
mod cold;
#[path = "app_restore_regrid.rs"]
mod regrid;
type RestoredTake = (
    String,
    aterm_agent::harness::relaunch::Snapshot,
    Option<aterm_agent::harness::restart_stamp::Cause>,
    Option<String>,
);
type BuiltRestoreLeaf = (
    crate::tab_model::ViewId,
    crate::tab_model::TabPresentation,
    Option<crate::tab_model::TabId>,
);

/// A historical held-tab notice. Codex preparation reads account and executable files, so it
/// runs off the main thread. Its result is only logged; it never starts or changes a session.
struct HeldNotice {
    sid: String,
    place: String,
    agent: restore::AgentRestore,
    stopped_by_hold: bool,
    fleet: bool,
    lifted: bool,
}

impl HeldNotice {
    fn message(&self, resume: Option<&str>) -> Option<aterm_messages::Message> {
        crate::message_reporters::restored_agent_held(
            &self.sid,
            &self.place,
            &crate::message_reporters::HeldAgent {
                name: self.agent.name().unwrap_or_default(),
                resume,
                stopped_by_hold: self.stopped_by_hold,
                fleet: self.fleet,
                lifted: self.lifted,
            },
        )
    }
}

/// A window's tabs before a native restore stages its leaf, and which one was active.
struct RestoreStagingMark {
    active: Option<crate::tab_model::TabId>,
    tabs: Vec<crate::tab_model::TabId>,
}

/// One unpublished native leaf. Its resources exist without a canonical window or tab.
pub(crate) struct RestoreLeafStaging {
    pub(crate) wid: WindowId,
    pub(crate) tab: Option<crate::tab_model::Tab>,
}

pub(crate) const RESTORE_SLICE_BUDGET: std::time::Duration = std::time::Duration::from_millis(4);

/// State machine that rebuilds a cold launch's restored window/tab layout in bounded slices.
pub(crate) struct RestoreDrain {
    windows: std::collections::VecDeque<restore::WindowLayout>,
    first: bool,
    filling: Option<FillingWindow>,
    placed: RestoredWindows,
    fold: restore::LayoutFold,
    /// What the layout names in the tasks directory, swept once the drain has placed every tab
    /// (`App::close_restore_pass`), or at a quit before that ([`Self::tasks`]).
    tasks: crate::task_launch::RestoredTasks,
    began: std::time::Instant,
    slices: u32,
    longest: std::time::Duration,
    waiting_document: bool,
    not_before: Option<std::time::Instant>,
    finalizing: Option<regrid::Finalizing>,
}

impl RestoreDrain {
    pub(crate) fn waiting_document(&self) -> bool {
        self.waiting_document
    }

    pub(crate) fn runnable(&self, now: std::time::Instant) -> bool {
        !self.waiting_document() && self.not_before.is_none_or(|at| now >= at)
    }

    pub(crate) fn deadline(&self) -> Option<std::time::Instant> {
        self.not_before
    }

    pub(crate) fn document_ready(&mut self) {
        self.waiting_document = false;
    }
    /// Initializes a [`RestoreDrain`] from an optional restore manifest.
    pub(crate) fn new(manifest: Option<restore::RestoreManifest>) -> Self {
        let tasks = manifest
            .as_ref()
            .map(crate::task_launch::RestoredTasks::of)
            .unwrap_or_default();
        let (windows, fold) = manifest.map_or_else(Default::default, |manifest| {
            (manifest.windows.into(), manifest.fold)
        });
        Self {
            windows,
            first: true,
            filling: None,
            placed: RestoredWindows::default(),
            fold,
            tasks,
            began: std::time::Instant::now(),
            slices: 0,
            longest: std::time::Duration::ZERO,
            waiting_document: false,
            not_before: None,
            finalizing: None,
        }
    }

    /// Appends unplaced windows and tabs still owed by this drain into a quit manifest's `windows`.
    pub(crate) fn carry_owed(
        &self,
        app: &App,
        windows: &mut Vec<restore::WindowLayout>,
        filling_at: Option<usize>,
        live_tabs: &[crate::tab_model::TabId],
    ) {
        if let Some(filling) = &self.filling
            && let Some(window) = filling_at.and_then(|at| windows.get_mut(at))
        {
            let mut owed: Vec<_> = filling.queue.iter().cloned().collect();
            if let Some(building) = &filling.building
                && (building.anchor.is_none() || building.preserves_bootstrap(app, filling.wid))
            {
                owed.push((building.index, building.snapshot(app)));
            }
            owed.sort_by_key(|(index, _)| *index);
            let mut present: Vec<_> = filling
                .placed
                .iter()
                .filter_map(|(index, id)| {
                    live_tabs
                        .iter()
                        .position(|live| live == id)
                        .map(|at| (*index, at))
                })
                .collect();
            for (index, mut tab) in owed {
                let target = restored_tab_position(
                    present.iter().copied(),
                    index,
                    window.restored_tabs.len(),
                );
                tab.root.forget_pool_ids();
                window.restored_tabs.insert(target, tab);
                if let Some(active) = window.active_item.as_mut()
                    && *active >= target
                {
                    *active += 1;
                }
                for (_, at) in &mut present {
                    if *at >= target {
                        *at += 1;
                    }
                }
                present.push((index, target));
            }
            window.forget_legacy_mirror();
        }
        if let Some(filling) = &self.filling
            && filling.pending_host.is_some()
        {
            let mut window = filling.source.clone();
            let mut tabs: Vec<_> = filling.queue.iter().cloned().collect();
            if let Some(building) = &filling.building {
                tabs.push((building.index, building.snapshot(app)));
            }
            tabs.sort_by_key(|(index, _)| *index);
            window.restored_tabs = tabs.into_iter().map(|(_, tab)| tab).collect();
            window.forget_pool_ids();
            window.forget_legacy_mirror();
            windows.push(window);
        }
        windows.extend(self.windows.iter().cloned().map(|mut window| {
            window.forget_pool_ids();
            window
        }));
    }

    /// Returns the live [`WindowId`] whose tabs are currently being populated.
    pub(crate) fn filling_window(&self) -> Option<WindowId> {
        self.filling.as_ref().map(|filling| filling.wid)
    }

    /// What the layout this drain is placing names in the tasks directory: swept when the drain
    /// ends, or at a quit that comes first (`App::settle_quit_desk_and_tasks`).
    pub(crate) const fn tasks(&self) -> &crate::task_launch::RestoredTasks {
        &self.tasks
    }
}

/// Window currently receiving restored tabs one slice at a time.
pub(crate) struct FillingWindow {
    wid: WindowId,
    queue: std::collections::VecDeque<(usize, restore::RestoredTab)>,
    placed: Vec<(usize, crate::tab_model::TabId)>,
    front: usize,
    shown: Option<crate::tab_model::TabId>,
    /// The window showed its Workers list when captured ([`restore::WindowLayout::workers_in_front`]).
    workers_in_front: bool,
    building: Option<cold::BuildingTab>,
    bootstrap: Option<cold::Anchor>,
    /// A native-only host has no canonical window until its first tab is complete.
    pending_host: Option<Box<WindowState>>,
    source: restore::WindowLayout,
    attach_owed: bool,
}

/// Summary of windows placed and any layout folds applied during [`App::restore_window`].
#[derive(Default)]
pub(crate) struct RestoredWindows {
    shown: Vec<(WindowId, restore::WindowShow)>,
    bootstrap: Option<WindowId>,
}

fn restored_tab_position(
    present: impl Iterator<Item = (usize, usize)> + Clone,
    index: usize,
    fallback: usize,
) -> usize {
    present
        .clone()
        .filter(|(placed, _)| *placed < index)
        .max_by_key(|(placed, _)| *placed)
        .map(|(_, before)| before + 1)
        .or_else(|| {
            present
                .filter(|(placed, _)| *placed > index)
                .min_by_key(|(placed, _)| *placed)
                .map(|(_, after)| after)
        })
        .unwrap_or(fallback)
}

/// Ledger tracking unsaved Settings drafts carried across a seamless update.
#[derive(Debug, Default)]
pub(crate) struct CarriedDraftsLedger {
    landed: Vec<(crate::tab_model::ViewId, restore::SettingsDraftRestore)>,
    lost: Vec<restore::SettingsDraftRestore>,
    unreadable: usize,
    owed: OwedDraftsLoss,
}

#[derive(Debug, Default)]
struct OwedDraftsLoss {
    lost: Vec<restore::SettingsDraftRestore>,
    unreadable: usize,
}

impl CarriedDraftsLedger {
    /// Initializes a ledger seeded with drafts from a layout the successor could not place.
    pub(crate) fn unplaced(
        (lost, unreadable): (Vec<restore::SettingsDraftRestore>, usize),
    ) -> Self {
        Self {
            landed: Vec::new(),
            lost,
            unreadable,
            owed: OwedDraftsLoss::default(),
        }
    }
}

pub(crate) const TRACKS_NORMAL_FRAME: bool = !cfg!(target_os = "macos");

/// Returns whether `window` is maximized, tiled, or fullscreen rather than in its normal frame.
pub(crate) fn frame_held(window: &aterm_window::window::Window) -> bool {
    window.is_maximized() || window.is_tiled() || (!cfg!(windows) && window.is_shown_fullscreen())
}

/// Last normal (non-maximized, non-tiled, non-fullscreen) origin and grid size of a window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NormalFrame {
    pub(crate) origin: Option<(i32, i32)>,
    pub(crate) rows: u16,
    pub(crate) cols: u16,
}

impl NormalFrame {
    /// Records `origin` when the window is in its normal frame (`!held`).
    pub(crate) const fn note_origin(&mut self, held: bool, origin: (i32, i32)) {
        if !held {
            self.origin = Some(origin);
        }
    }

    /// Records `(rows, cols)` when the window is in its normal frame (`!held`).
    pub(crate) const fn note_grid(&mut self, held: bool, (rows, cols): (u16, u16)) {
        if !held {
            self.rows = rows;
            self.cols = cols;
        }
    }
}

/// Returns whether restore capture should persist the window's saved normal frame instead of its live frame.
#[cfg(any(not(windows), test))]
#[must_use]
pub(crate) fn captured_hold(
    handoff: bool,
    maximized: Option<bool>,
    held: impl FnOnce() -> bool,
) -> Option<bool> {
    match maximized {
        Some(maximized) if !handoff => Some(maximized || held()),
        maximized => maximized,
    }
}

/// Computes the `(origin, (rows, cols))` frame persisted for a window during restore capture.
#[must_use]
pub(crate) fn captured_frame(
    live_origin: Option<(i32, i32)>,
    live_grid: (u16, u16),
    held: Option<bool>,
    normal: NormalFrame,
) -> (Option<(i32, i32)>, (u16, u16)) {
    if held == Some(true) {
        (normal.origin, (normal.rows, normal.cols))
    } else {
        (live_origin, live_grid)
    }
}

/// Whether an OS window's size has still to reach its grid: its inner size gives another grid
/// than the one it holds, or it does not show a maximize the restore asked for.
const fn size_unlanded(
    inner_grid: (u16, u16),
    grid: (u16, u16),
    maximize_asked: bool,
    maximized: bool,
) -> bool {
    inner_grid.0 != grid.0 || inner_grid.1 != grid.1 || (maximize_asked && !maximized)
}

/// How long a window owing its restore re-grid waits for its OS size once nothing more is to be
/// asked of the OS ([`App::pay_restore_regrid_if_due`]).
pub(crate) const RESTORE_SHOW_SETTLE: std::time::Duration = std::time::Duration::from_secs(2);

/// The longest a window owes its restore re-grid, whatever is still to come.
pub(crate) const RESTORE_SHOW_CAP: std::time::Duration = std::time::Duration::from_secs(10);

/// The re-grid a restore's close left a window owing, for the size its OS window had still to bring
/// ([`App::regrid_restored_windows`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct RestoreRegridOwed {
    /// When the restore closed.
    pub(crate) closed: std::time::Instant,
    /// When the wait for the size began: the close, the last frame a carried full screen was
    /// still to enter, or its entry (`App::enter_carried_fullscreen_if_due`).
    pub(crate) since: std::time::Instant,
    /// Whether that size was a carried full screen's.
    pub(crate) fullscreen: bool,
}

/// Provenance of `local_id`s in a restore descriptor (`HandedOff` from a predecessor or `SameProcess`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum LeafIds {
    Live,
    Retired,
}

#[derive(Clone, Copy)]
enum FillingShell<'a> {
    Registered(u64),
    Unregistered(&'a Session),
}

#[cfg(test)]
fn stub_adopted_session(id: u64, master: crate::spawn::HandedMaster) -> crate::Session {
    if master.raw() < 3 {
        return crate::stub_session(id);
    }
    crate::stub_session_with_sink(id, master.into_sink())
}

/// Removes and returns the unplaced handed-off shell matching `local_id`, if present.
pub(crate) fn take_handed_off_shell(
    handed_off: &mut Vec<crate::spawn::Adopted>,
    local_id: Option<u64>,
) -> Option<crate::spawn::Adopted> {
    let local_id = local_id?;
    let index = handed_off
        .iter()
        .position(|shell| shell.local_id == local_id)?;
    Some(handed_off.remove(index))
}

/// Removes and returns the handed-off shell adopted as bootstrap session 0. With no layout that
/// is the first shell no quit kept: one the last window's quit kept past it (the Mac Setup tab's
/// installer, `keeper_link::Claim::kept`) never becomes the window's first tab, so a launch with
/// nothing to restore opens its own shell in front and that install lands behind it
/// ([`App::land_orphan_tab`]).
pub(crate) fn take_session0_shell(
    handed_off: &mut Vec<crate::spawn::Adopted>,
    layout: Option<&restore::RestoreManifest>,
) -> Option<crate::spawn::Adopted> {
    match layout.and_then(|layout| layout.windows.first()) {
        Some(window0) => take_handed_off_shell(handed_off, window0.bootstrap_local_id()),
        None => {
            let index = handed_off.iter().position(|shell| {
                !shell
                    .keeper
                    .as_ref()
                    .is_some_and(crate::keeper_link::Claim::kept)
            })?;
            Some(handed_off.remove(index))
        }
    }
}

/// Returns whether the current layout has settled and may be written to the crash journal.
pub(crate) const fn crash_journal_writable(
    first_present_done: bool,
    restore_pending: bool,
    handoff_pending: bool,
) -> bool {
    first_present_done && !restore_pending && !handoff_pending
}

/// Result of querying the process table for a session's root foreground process.
#[derive(Debug)]
pub(crate) enum RootAnswer {
    #[cfg_attr(unix, allow(dead_code))]
    Unasked,
    #[cfg_attr(not(unix), allow(dead_code))]
    Read(Option<crate::quit_safety::SessionRoot>),
}

/// Determines whether a terminal leaf is running a non-shell foreground program for the crash journal.
pub(crate) fn leaf_program(
    group: i32,
    root_pid: i32,
    root: &RootAnswer,
    named: Option<(i32, &str)>,
) -> Option<String> {
    if crate::quit_safety::foreground_is_job(group, root_pid) {
        return Some(
            named
                .filter(|(at, _)| group > 0 && *at == group)
                .map_or_else(String::new, |(_, name)| name.to_string()),
        );
    }
    match root {
        RootAnswer::Unasked => None,
        RootAnswer::Read(None) => Some(String::new()),
        RootAnswer::Read(Some(root)) if !crate::quit_safety::is_shell_name(&root.comm) => Some(
            named
                .filter(|(at, name)| {
                    root_pid > 0 && *at == root_pid && !crate::quit_safety::is_shell_name(name)
                })
                .map_or_else(String::new, |(_, name)| name.to_string()),
        ),
        RootAnswer::Read(Some(root)) => (!root.children.is_empty()).then(String::new),
    }
}

fn recovery_capability(
    native: &restore::NativeLeafRestore,
) -> Option<crate::native_app::RecoveryCapability> {
    match native.restore_tag.as_str() {
        "about" if native.route.is_none() && native.uri.is_none() => {
            Some(crate::native_app::RecoveryCapability::About)
        }
        "system-state" if native.route.is_none() && native.uri.is_none() => {
            Some(crate::native_app::RecoveryCapability::SystemState)
        }
        "settings" => native
            .route
            .as_deref()
            .and_then(crate::native_settings::SettingsRoute::from_path)
            .map(|route| crate::native_app::RecoveryCapability::Settings {
                route: route.path().to_string(),
            }),
        "markdown" | "editor" => {
            let uri = native.uri.as_ref()?;
            (uri.len() <= 4_096
                && uri.starts_with("file:///")
                && !uri
                    .chars()
                    .any(|character| matches!(character, '\0' | '\r' | '\n')))
            .then(|| crate::native_app::RecoveryCapability::Document {
                kind: if native.restore_tag == "markdown" {
                    crate::native_app::AppKind::Markdown
                } else {
                    crate::native_app::AppKind::Editor
                },
                uri: uri.clone(),
                config_editor: native.config_editor,
            })
        }
        _ => None,
    }
}

impl App {
    pub(crate) fn cancel_restore_anchor(&mut self, view: crate::tab_model::ViewId) {
        let building = self.restore_drain.as_mut().and_then(|drain| {
            let filling = drain.filling.as_mut()?;
            if !filling
                .building
                .as_ref()?
                .anchor
                .as_ref()
                .is_some_and(|anchor| anchor.view == view)
            {
                return None;
            }
            drain.waiting_document = false;
            filling.building.take()
        });
        if let Some(building) = building {
            building.cancel(self);
        }
    }

    /// Cancel owned unfinished resources after the close captured its non-destructive snapshot.
    pub(crate) fn cancel_restore_window(&mut self, wid: WindowId, all: bool) {
        let building = self.restore_drain.as_mut().and_then(|drain| {
            drain.not_before = None;
            if let Some(finalizing) = drain.finalizing.as_mut() {
                finalizing.cancel(wid, all);
            }
            if all {
                drain.windows.clear();
            }
            let filling = drain.filling.as_mut()?;
            if !all && filling.wid != wid {
                return None;
            }
            filling.queue.clear();
            filling.bootstrap = None;
            filling.pending_host = None;
            drain.waiting_document = false;
            filling.building.take()
        });
        if let Some(building) = building {
            building.cancel(self);
        }
    }
    /// Captures the full [`restore::RestoreManifest`] across all open windows.
    pub(crate) fn capture_restore_manifest(&self) -> restore::RestoreManifest {
        self.capture_manifest(false, false)
    }

    /// The handoff layout with no lone Workers list: what a target without `desk1` is handed, so
    /// its emptiness says the desk holds no tab.
    #[cfg(unix)]
    pub(crate) fn capture_handoff_layout(&self) -> restore::RestoreManifest {
        self.capture_handoff_layout_for(false)
    }

    #[cfg(unix)]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeUpdateSettingsDraftCarry",
            action = "Park",
            project = "aterm_gui::settings_draft_carry_conformance::Rig::project"
        )
    )]
    /// Captures the [`restore::RestoreManifest`] for a seamless-update handoff, with a window that
    /// holds only an empty Workers list where `lone_lists` says the target rebuilds one.
    pub(crate) fn capture_handoff_layout_for(&self, lone_lists: bool) -> restore::RestoreManifest {
        self.capture_manifest(true, lone_lists)
    }

    fn capture_manifest(&self, handoff: bool, lone_lists: bool) -> restore::RestoreManifest {
        let filling_window = self
            .restore_drain
            .as_deref()
            .and_then(RestoreDrain::filling_window);
        let mut filling_tabs = Vec::new();
        #[cfg(target_os = "macos")]
        let stack_ranks = {
            let live: Vec<WindowId> = self.windows.keys().copied().collect();
            crate::window_show::stacking_ranks(&live, &self.focus_order)
        };
        let mut windows: Vec<restore::WindowLayout> = self
            .windows
            .iter()
            .enumerate()
            .map(|(index, (&wid, ws))| {
                let os_window = ws.os_window.as_ref().filter(|_| !self.event_loop_failed);
                #[cfg(windows)]
                let (pos, maximized) = {
                    let placement = os_window.and_then(|w| crate::app_window::placement::read(w));
                    let pos = os_window.and_then(|w| w.outer_position().ok());
                    match placement {
                        Some((true, (x, y))) => (
                            Some(aterm_window::dpi::PhysicalPosition::new(x, y)),
                            Some(true),
                        ),
                        Some((false, _)) => (pos, Some(false)),
                        None => (pos, None),
                    }
                };
                #[cfg(not(windows))]
                let (pos, maximized, held) = {
                    let live = os_window
                        .and_then(|w| w.outer_position().ok())
                        .map(|p| (p.x, p.y));
                    let maximized = if TRACKS_NORMAL_FRAME {
                        os_window.map(|w| w.is_maximized())
                    } else {
                        None
                    };
                    let held = captured_hold(handoff, maximized, || {
                        os_window.is_some_and(|w| frame_held(w))
                    });
                    let (origin, _) =
                        captured_frame(live, (ws.rows, ws.cols), held, ws.normal_frame);
                    (
                        origin.map(|(x, y)| aterm_window::dpi::PhysicalPosition::new(x, y)),
                        maximized,
                        held,
                    )
                };
                #[cfg(windows)]
                let held = maximized;
                let (rows, cols) =
                    captured_frame(None, (ws.rows, ws.cols), held, ws.normal_frame).1;
                #[cfg(target_os = "macos")]
                let show = crate::window_show::captured_show(
                    os_window.map(|w| crate::window_show::ShowProbe {
                        fullscreen: w.fullscreen().is_some(),
                        minimized: w.is_minimized(),
                    }),
                    stack_ranks.get(index).copied().unwrap_or(u32::MAX),
                    self.frontmost_window == Some(wid),
                    self.carried_fullscreen_pending(wid),
                );
                #[cfg(not(target_os = "macos"))]
                let show = {
                    let _ = (index, wid);
                    restore::WindowShow::UNKNOWN
                };
                // A tab of Mac Setup terminals alone is described only to a handoff, as each such
                // leaf is ([`App::is_developer_tools_session`]); its terminal index goes with it.
                // A preview is temporary. Bring its selected strip tab back to the
                // front after a restart, and keep the hidden session's own record.
                let saved_active = ws
                    .active_session_preview()
                    .and_then(|p| p.anchor)
                    .or_else(|| ws.tab_set.active_id());
                let saved_terminal = saved_active
                    .and_then(|tab| {
                        crate::app_tabs::terminal_projection_index(
                            &ws.tab_set,
                            &self.view_store,
                            tab,
                        )
                    })
                    .unwrap_or(ws.tabs.active);
                let borrowed = ws
                    .dock
                    .preview
                    .filter(|p| p.borrowed && self.pool.views(p.session).is_some_and(|v| v > 1))
                    .map(|p| p.tab);
                let borrowed_index = borrowed.and_then(|tab| {
                    crate::app_tabs::terminal_projection_index(&ws.tab_set, &self.view_store, tab)
                });
                let terminal_tabs = ws
                    .layouts
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| Some(*index) != borrowed_index)
                    .map(|(_, tree)| tree)
                    .filter(|tree| {
                        let mut plain = false;
                        tree.visit_sessions(&mut |session| {
                            plain |= !self.is_developer_tools_session(session);
                        });
                        handoff || plain
                    })
                    .map(|tree| tree.to_layout(&|id| self.restore_session_meta(id)))
                    .collect::<Vec<_>>();
                let mut native_tabs = Vec::new();
                let mut tab_order = Vec::with_capacity(ws.tab_set.len());
                let mut restored_tabs = Vec::with_capacity(ws.tab_set.len());
                let mut active_item = None;
                let mut terminal_index = 0usize;
                for tab in ws.tab_set.tabs() {
                    if Some(tab.id) == borrowed {
                        continue;
                    }
                    if let Some(restored) = self.tab_restore_descriptor_carrying(tab, handoff) {
                        if saved_active == Some(tab.id) {
                            active_item = Some(restored_tabs.len());
                        }
                        if filling_window == Some(wid) {
                            filling_tabs.push(tab.id);
                        }
                        restored_tabs.push(restored);
                    }
                    let entry = if !handoff
                        && !tab
                            .root
                            .any_leaf(&mut |view| !self.view_is_developer_tools_session(*view))
                    {
                        None
                    } else if !tab.root.any_leaf(&mut |view| {
                        !matches!(
                            self.view_store.get(*view),
                            Some(crate::tab_model::View::Terminal(_))
                        )
                    }) {
                        let entry = restore::TabOrderEntry::Terminal {
                            index: terminal_index,
                        };
                        terminal_index = terminal_index.saturating_add(1);
                        Some(entry)
                    } else {
                        self.native_restore_descriptor(tab)
                            .filter(|descriptor| descriptor.joins_mirror(&native_tabs))
                            .map(|descriptor| {
                                let index = native_tabs.len();
                                native_tabs.push(descriptor);
                                restore::TabOrderEntry::Native { index }
                            })
                    };
                    if let Some(entry) = entry {
                        tab_order.push(entry);
                    }
                }
                let fullscreen = show.fullscreen == Some(true)
                    || os_window.is_some_and(|w| w.fullscreen().is_some());
                let inner_px = ws
                    .win_px
                    .filter(|size| {
                        held != Some(true)
                            && (handoff || !fullscreen)
                            && size.width > 0
                            && size.height > 0
                    })
                    .map(|size| [size.width, size.height]);
                restore::WindowLayout {
                    rows,
                    cols,
                    active_tab: (saved_terminal
                        - usize::from(borrowed_index.is_some_and(|i| i < saved_terminal)))
                    .min(terminal_tabs.len().saturating_sub(1)),
                    outer_x: pos.map(|p| p.x),
                    outer_y: pos.map(|p| p.y),
                    maximized,
                    inner_px,
                    presence_row: false,
                    show,
                    tabs: terminal_tabs,
                    native_tabs,
                    tab_order,
                    active_item,
                    workers_in_front: ws.tab_set.home().is_some()
                        && ws.tab_set.home() == saved_active,
                    restored_tabs,
                    // The record that agrees with the grid and frame written beside it; a
                    // headless app seats no dock and keeps its rows' records as they came.
                    dock: if self.headless {
                        ws.dock.pref
                    } else {
                        ws.dock.written()
                    },
                    dock_by_default: ws.dock.written_by_default(self.headless),
                    // The scale the origin and frame above were read at.
                    scale: os_window
                        .filter(|_| pos.is_some() || inner_px.is_some())
                        .map(|w| w.scale_factor()),
                }
            })
            .collect();
        if !handoff && let Some(drain) = self.restore_drain.as_deref() {
            let filling_at = drain
                .filling_window()
                .and_then(|wid| self.windows.keys().position(|live| *live == wid));
            drain.carry_owed(self, &mut windows, filling_at, &filling_tabs);
        }
        // A desk with only private documents has nothing a handoff can describe, and a kept image
        // would hold its update until they closed, though a handoff from any other desk drops the
        // same tabs. A Recovery tab says where they went, and every sessionless target restores it.
        // A desk with only that tab, after an earlier handoff, goes the same way.
        if handoff
            && !windows
                .iter()
                .any(|window| window.kept_in_layout(lone_lists))
        {
            for (window, &wid) in windows.iter_mut().zip(self.windows.keys()) {
                if self.window_holds_no_capture(wid) {
                    window.stand_in_for_private_documents();
                }
            }
        }
        // After `carry_owed`, which fills by position.
        windows.retain(|window| window.kept_in_layout(lone_lists));
        let mut manifest = restore::RestoreManifest::new(windows);
        if handoff {
            manifest.fold_into_caps();
        }
        manifest
    }

    /// Projects a single-view native tab into its legacy [`restore::NativeTabRestore`] descriptor.
    pub(crate) fn native_restore_descriptor(
        &self,
        tab: &crate::tab_model::Tab,
    ) -> Option<restore::NativeTabRestore> {
        let crate::tab_model::SplitTree::Leaf(view) = tab.root else {
            return None;
        };
        if tab.focus != view {
            return None;
        }
        if self.view_is_private(view) {
            return None;
        }
        let crate::tab_model::View::Native(native) = self.view_store.get(view).copied()? else {
            return None;
        };
        let app = self.native_runtime.app(native.instance)?;
        match app.kind() {
            crate::native_app::AppKind::Mail => Some(restore::NativeTabRestore::Mail),
            crate::native_app::AppKind::About => Some(restore::NativeTabRestore::About),
            crate::native_app::AppKind::Allocation => Some(restore::NativeTabRestore::Allocation),
            crate::native_app::AppKind::SystemState => Some(restore::NativeTabRestore::SystemState),
            crate::native_app::AppKind::Settings => {
                let crate::native_app::AppViewState::Settings(state) =
                    self.native_runtime.view_state(view)?
                else {
                    return None;
                };
                Some(restore::NativeTabRestore::Settings {
                    route: state.route.path().to_string(),
                })
            }
            crate::native_app::AppKind::Editor => {
                let document = app.document_id()?;
                let uri = self.document_store.canonical_uri(document)?.to_string();
                Some(restore::NativeTabRestore::Editor { uri })
            }
            // Tables is restored from its leaf alone ([`App::view_restore_descriptor`]): a
            // build from before the page reads a leaf it does not know as a Recovery tab, and
            // a kind it does not know here as a layout it cannot read at all.
            crate::native_app::AppKind::Markdown
            | crate::native_app::AppKind::Recovery
            | crate::native_app::AppKind::Workers
            | crate::native_app::AppKind::Registry
            | crate::native_app::AppKind::Tables
            | crate::native_app::AppKind::Navigator
            | crate::native_app::AppKind::SystemResources => None,
        }
    }

    /// Projects a terminal or native split tab into a recursive v2 [`restore::RestoredTab`] descriptor.
    pub(crate) fn tab_restore_descriptor(
        &self,
        tab: &crate::tab_model::Tab,
    ) -> Option<restore::RestoredTab> {
        self.tab_restore_descriptor_carrying(tab, false)
    }

    /// A private document's view ([`App::view_is_private`]) is never described: its values live
    /// only while its reader holds the tab, so a split drops it and its sibling takes its place,
    /// and a tab that holds nothing else is not restored or reopened. A Mac Setup tab's terminal
    /// ([`App::is_developer_tools_session`]) is left out the same way unless `carry_drafts` (an
    /// update's handoff, whose successor fills its leaf with the same PTY): any other restore
    /// would bring its one install attempt back as a plain shell. So is a failed open's Recovery
    /// tab that no plain open can try again ([`failed_open_leaf`]).
    fn tab_restore_descriptor_carrying(
        &self,
        tab: &crate::tab_model::Tab,
        carry_drafts: bool,
    ) -> Option<restore::RestoredTab> {
        /// One captured subtree: described, or wholly left out ([`App::view_is_left_out`]).
        enum Captured {
            Kept(Box<restore::RestoredSplitTree>),
            Private,
        }

        struct CaptureContext<'a> {
            app: &'a App,
            focus: crate::tab_model::ViewId,
            carry_drafts: bool,
            worker: bool,
            pending: Option<&'a (crate::tab_model::ViewId, restore::RestoredTab)>,
        }

        fn capture(
            context: &CaptureContext<'_>,
            node: &crate::tab_model::SplitTree<crate::tab_model::ViewId>,
            path: &mut Vec<restore::RestoreBranch>,
            focused_path: &mut Option<Vec<restore::RestoreBranch>>,
        ) -> Option<Captured> {
            match node {
                crate::tab_model::SplitTree::Leaf(view) => {
                    if let Some((anchor, record)) =
                        context.pending.filter(|(anchor, _)| anchor == view)
                    {
                        if *anchor == context.focus {
                            let mut focused = path.clone();
                            focused.extend(record.focused_path.iter().copied());
                            *focused_path = Some(focused);
                        }
                        return Some(Captured::Kept(Box::new(record.root.clone())));
                    }
                    if context.app.view_is_left_out(*view, context.carry_drafts) {
                        return Some(Captured::Private);
                    }
                    if *view == context.focus {
                        *focused_path = Some(path.clone());
                    }
                    let view = context.app.view_restore_descriptor_carrying(
                        *view,
                        context.carry_drafts,
                        context.worker,
                    )?;
                    Some(Captured::Kept(Box::new(restore::RestoredSplitTree::leaf(
                        view,
                    ))))
                }
                crate::tab_model::SplitTree::Split {
                    axis,
                    ratio,
                    first,
                    second,
                } => {
                    path.push(restore::RestoreBranch::First);
                    let first = capture(context, first, path, focused_path)?;
                    path.pop();
                    path.push(restore::RestoreBranch::Second);
                    let second = capture(context, second, path, focused_path)?;
                    path.pop();
                    Some(match (first, second) {
                        (Captured::Kept(first), Captured::Kept(second)) => {
                            Captured::Kept(Box::new(restore::RestoredSplitTree::Split {
                                axis: match axis {
                                    crate::tab_model::SplitAxis::Horizontal => {
                                        restore::SplitKind::Horizontal
                                    }
                                    crate::tab_model::SplitAxis::Vertical => {
                                        restore::SplitKind::Vertical
                                    }
                                },
                                ratio: *ratio,
                                first,
                                second,
                            }))
                        }
                        (Captured::Kept(kept), Captured::Private)
                        | (Captured::Private, Captured::Kept(kept)) => {
                            // The kept side takes this split's place: a focus inside it loses
                            // this level's branch.
                            if let Some(focused) = focused_path.as_mut()
                                && focused.len() > path.len()
                                && focused.starts_with(path)
                            {
                                focused.remove(path.len());
                            }
                            Captured::Kept(kept)
                        }
                        (Captured::Private, Captured::Private) => Captured::Private,
                    })
                }
            }
        }

        let pending_builder = (!carry_drafts)
            .then(|| {
                let filling = self.restore_drain.as_ref()?.filling.as_ref()?;
                Some((filling.wid, filling.building.as_ref()?))
            })
            .flatten()
            .filter(|(wid, building)| {
                !building.preserves_bootstrap(self, *wid)
                    && building
                        .anchor
                        .as_ref()
                        .is_some_and(|anchor| anchor.tab == tab.id)
            })
            .map(|(_, building)| building);
        let pending = pending_builder.map(|building| {
            let mut record = building.snapshot(self);
            record.root.forget_pool_ids();
            (
                building.anchor.as_ref().expect("filtered anchor").view,
                record,
            )
        });
        let mut focused_path = None;
        let context = CaptureContext {
            app: self,
            focus: tab.focus,
            carry_drafts,
            worker: tab.ephemeral,
            pending: pending.as_ref(),
        };
        let Captured::Kept(root) =
            capture(&context, &tab.root, &mut Vec::new(), &mut focused_path)?
        else {
            return None;
        };
        // A focus on a dropped leaf moves to the kept tree's first leaf.
        let focused_path = match focused_path {
            Some(path) => path,
            None if self.view_is_left_out(tab.focus, carry_drafts) => {
                let mut path = Vec::new();
                let mut node = &*root;
                while let restore::RestoredSplitTree::Split { first, .. } = node {
                    path.push(restore::RestoreBranch::First);
                    node = &**first;
                }
                path
            }
            None => return None,
        };
        let carried = |current, original, restored| {
            if current == original {
                restored
            } else {
                current
            }
        };
        let (zoomed, parked, ephemeral, fleet) = pending_builder.map_or(
            (tab.zoomed, tab.parked, tab.ephemeral, tab.fleet),
            |building| {
                let original = &building.anchor.as_ref().expect("filtered anchor").original;
                (
                    carried(tab.zoomed, original.zoomed, building.record.zoomed),
                    carried(tab.parked, original.parked, building.record.parked),
                    carried(tab.ephemeral, original.ephemeral, building.record.ephemeral),
                    carried(tab.fleet, original.fleet, building.record.fleet),
                )
            },
        );
        Some(restore::RestoredTab {
            root: *root,
            focused_path,
            zoomed,
            parked,
            ephemeral,
            fleet: fleet && (parked || ephemeral),
        })
    }

    /// Whether a tab's capture leaves `view` out, its sibling taking its place: a view no capture
    /// describes ([`App::view_has_no_capture`]), a page that holds nothing of its own to bring
    /// back ([`App::view_is_unrestored_page`]: the navigator, System Resources), a Mac Setup
    /// tab's terminal unless `carry_drafts`, and a failed open's Recovery tab no plain open can
    /// try again ([`failed_open_leaf`]).
    fn view_is_left_out(&self, view: crate::tab_model::ViewId, carry_drafts: bool) -> bool {
        self.view_has_no_capture(view)
            || self.view_is_unrestored_page(view)
            || (!carry_drafts && self.view_is_developer_tools_session(view))
            || self.view_is_spent_failed_open(view)
    }

    /// Whether `view` is a failed open's Recovery tab ([`crate::native_app::RecoveryOrigin::Open`])
    /// a relaunch cannot try again ([`failed_open_leaf`]).
    fn view_is_spent_failed_open(&self, view: crate::tab_model::ViewId) -> bool {
        let Some(crate::tab_model::View::Native(native)) = self.view_store.get(view).copied()
        else {
            return false;
        };
        matches!(
            self.native_runtime.app(native.instance),
            Some(crate::native_app::NativeApp::Recovery(recovery))
                if matches!(recovery.origin, crate::native_app::RecoveryOrigin::Open { .. })
                    && failed_open_leaf(recovery).is_none()
        )
    }

    /// Projects a single native view leaf into its [`restore::RestoredView`] descriptor.
    pub(crate) fn view_restore_descriptor(
        &self,
        view: crate::tab_model::ViewId,
    ) -> Option<restore::RestoredView> {
        self.view_restore_descriptor_carrying(view, false, false)
    }

    fn view_restore_descriptor_carrying(
        &self,
        view: crate::tab_model::ViewId,
        carry_drafts: bool,
        worker: bool,
    ) -> Option<restore::RestoredView> {
        if self.view_has_no_capture(view) {
            return None;
        }
        match self.view_store.get(view).copied()? {
            crate::tab_model::View::Terminal(terminal) => {
                let (
                    (cwd, title),
                    user_meta,
                    (sid, lineage, opened_ms, log_ep),
                    claim,
                    task,
                    session_held,
                ) = self
                    .pool
                    .get(terminal.session)
                    .map(|s| {
                        let cwd_and_title = Self::session_cwd_and_title(s);
                        let user_meta = s
                            .ctx
                            .meta
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .sanitized();
                        let (lineage, opened_ms, log_ep) = {
                            let tl = s.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
                            (
                                tl.slog().lineage(),
                                tl.slog().opened_ms(),
                                tl.slog().log().ep(),
                            )
                        };
                        let sid = s.ctx.self_id.as_str();
                        let ids = (
                            restore::carried_sid(sid).then(|| sid.to_string()),
                            lineage
                                .filter(|l| l.as_str() != sid && restore::carried_sid(l.as_str()))
                                .map(|l| l.as_str().to_string()),
                            opened_ms,
                            Some(log_ep),
                        );
                        let claim = crate::harness_host::credit_claim::claim_worker(&s.ctx);
                        let task = worker
                            .then(|| crate::task_record::restore_row(&s.ctx))
                            .flatten();
                        let session_held = cfg!(unix) && !carry_drafts && Self::session_held(s);
                        (cwd_and_title, user_meta, ids, claim, task, session_held)
                    })
                    .unwrap_or_default();
                let store_sid = (carry_drafts || session_held)
                    .then(|| self.session_sid(terminal.session))
                    .flatten();
                let (owed, supervised, lost, held) = if carry_drafts {
                    let owed = store_sid
                        .as_deref()
                        .and_then(|sid| self.pending_restored_agent_by_sid(sid, true));
                    let supervised = if owed.is_none() {
                        store_sid
                            .as_deref()
                            .and_then(crate::harness_host::followed_snapshot)
                            .map(|snap| Box::new(restore::AgentRestore::of(snap)))
                    } else {
                        None
                    };
                    let lost = store_sid
                        .as_deref()
                        .and_then(|sid| self.harness.as_ref()?.lost_word(sid))
                        .map(Box::new);
                    (owed, supervised, lost, false)
                } else {
                    let held = session_held
                        && !store_sid.as_deref().is_some_and(|sid| {
                            crate::harness_host::published_snapshot(sid).is_some()
                        });
                    (None, None, None, held)
                };
                // Marked from the agent the leaf carries here (a handoff's owed one), and again
                // once a desk's agents are filled in (`fill_desk_agents`).
                let mut leaf = restore::TerminalLeafRestore {
                    cwd,
                    title,
                    profile: None,
                    local_id: Some(terminal.session),
                    user_title: user_meta.user_title,
                    description: user_meta.description,
                    icon: user_meta.icon,
                    role: user_meta.role,
                    attention: user_meta.attention,
                    questions: user_meta.questions,
                    autopilot: user_meta.autopilot,
                    land_at: user_meta.land_at,
                    autopilot_by: user_meta.autopilot_set_by.map(|set_by| set_by.carried()),
                    pinned: user_meta.pinned,
                    theme: self
                        .kept_session_theme(terminal.session)
                        .map(|theme| theme.selector.clone()),
                    agent: owed,
                    supervised,
                    lost,
                    held,
                    sid,
                    lineage,
                    opened_ms,
                    log_ep,
                    holds: Vec::new(),
                    claim_ended: claim.as_ref().is_some_and(|&(_, ended)| ended),
                    not_claim_worker: false,
                    claim_worker: claim.map(|(folder, _)| folder),
                    task,
                };
                leaf.mark_claim_bystander();
                Some(restore::RestoredView::Terminal(leaf))
            }
            crate::tab_model::View::Native(native) => {
                let app = self.native_runtime.app(native.instance)?;
                let state = self.native_runtime.view_state(view)?;
                if let (
                    crate::native_app::NativeApp::Recovery(recovery),
                    crate::native_app::AppViewState::Recovery(_),
                ) = (app, state)
                {
                    // A document the person asked to open that did not open is captured as that
                    // document's leaf, so a relaunch tries the file again (and restore's own
                    // Recovery takes its place if it still fails); one a plain open cannot try
                    // again is left out (`failed_open_leaf`). A restored leaf's keeps its
                    // Placeholder.
                    if let crate::native_app::RecoveryOrigin::Open { .. } = recovery.origin {
                        return failed_open_leaf(recovery).map(restore::RestoredView::Native);
                    }
                    return Some(restore::RestoredView::Placeholder(
                        restore::PlaceholderLeafRestore {
                            restore_tag: recovery.restore_tag.clone(),
                            reason: recovery.reason.clone(),
                            metadata: recovery.metadata.clone(),
                            unhanded: None,
                        },
                    ));
                }
                let mut descriptor = match (app.kind(), state) {
                    (
                        crate::native_app::AppKind::Allocation,
                        crate::native_app::AppViewState::Allocation(_),
                    ) => restore::NativeLeafRestore::allocation(),
                    (
                        crate::native_app::AppKind::Mail,
                        crate::native_app::AppViewState::Mail(_),
                    ) => restore::NativeLeafRestore::mail(),
                    (
                        crate::native_app::AppKind::About,
                        crate::native_app::AppViewState::About(_),
                    ) => restore::NativeLeafRestore::about(),
                    (
                        crate::native_app::AppKind::SystemState,
                        crate::native_app::AppViewState::SystemState(_),
                    ) => restore::NativeLeafRestore::system_state(),
                    (
                        crate::native_app::AppKind::Tables,
                        crate::native_app::AppViewState::Tables(tables),
                    ) => restore::NativeLeafRestore::tables(tables.row_route()),
                    (
                        crate::native_app::AppKind::Registry,
                        crate::native_app::AppViewState::Registry(registry),
                    ) => restore::NativeLeafRestore::registry(registry.selected()),
                    (
                        crate::native_app::AppKind::Settings,
                        crate::native_app::AppViewState::Settings(settings),
                    ) => restore::NativeLeafRestore::settings(settings.route.path().to_string()),
                    (
                        crate::native_app::AppKind::Editor,
                        crate::native_app::AppViewState::Editor(_),
                    ) => restore::NativeLeafRestore::document(
                        "editor",
                        self.document_store
                            .canonical_uri(app.document_id()?)?
                            .to_string(),
                    ),
                    _ => return None,
                };
                match state {
                    crate::native_app::AppViewState::About(_)
                    | crate::native_app::AppViewState::Mail(_)
                    | crate::native_app::AppViewState::Allocation(_)
                    | crate::native_app::AppViewState::Workers(_)
                    | crate::native_app::AppViewState::Registry(_)
                    | crate::native_app::AppViewState::Tables(_)
                    | crate::native_app::AppViewState::Navigator(_)
                    | crate::native_app::AppViewState::SystemResources(_)
                    | crate::native_app::AppViewState::SystemState(_) => {}
                    crate::native_app::AppViewState::Settings(settings) => {
                        if carry_drafts {
                            descriptor.settings_drafts = settings.carried_field_drafts().carried;
                        }
                    }
                    crate::native_app::AppViewState::Editor(editor) => {
                        descriptor.markdown = editor.markdown.as_ref().map(|workspace| {
                            use crate::native_app::{MarkdownPane, MarkdownWorkspaceMode};
                            restore::MarkdownWorkspaceRestore {
                                mode: match workspace.mode {
                                    MarkdownWorkspaceMode::Read => "read",
                                    MarkdownWorkspaceMode::Edit => "edit",
                                    MarkdownWorkspaceMode::Split => "split",
                                }
                                .into(),
                                focus: match workspace.focus {
                                    MarkdownPane::Editor => "editor",
                                    MarkdownPane::Reader => "reader",
                                }
                                .into(),
                                source_anchor: workspace.reader.source_anchor,
                                visual_row: workspace.reader.visual_row,
                                reader_entry_anchor: workspace.reader_entry_anchor,
                                selection: workspace.reader.selection.as_ref().map(|range| {
                                    restore::RestoreSelection {
                                        anchor: range.start,
                                        head: range.end,
                                    }
                                }),
                                split_per_mille: workspace.split_per_mille,
                            }
                        });
                        descriptor.config_editor =
                            self.native_runtime.config_editor_enabled(native.instance);
                        if let Some(buffer) = editor.buffer.as_ref() {
                            descriptor.editor_selections = buffer
                                .selections
                                .iter()
                                .map(|selection| restore::RestoreSelection {
                                    anchor: selection.anchor,
                                    head: selection.head,
                                })
                                .collect();
                            descriptor.primary_selection = buffer.primary;
                            descriptor.viewport_anchor = buffer.viewport_anchor;
                        }
                        descriptor.durable_seq = app
                            .document_id()
                            .and_then(|document| self.document_store.checkpoint_seq(document))
                            .map_or(0, |sequence| sequence.0);
                    }
                    crate::native_app::AppViewState::Recovery(_) => return None,
                }
                Some(restore::RestoredView::Native(descriptor))
            }
        }
    }

    /// Reapplies bounded scroll, selection, and draft state onto a restored native view.
    pub(crate) fn apply_native_view_restore(
        &mut self,
        view: crate::tab_model::ViewId,
        restore: &restore::NativeLeafRestore,
    ) -> bool {
        fn clamp_boundary(text: &str, position: usize) -> usize {
            let mut position = position.min(text.len());
            while position > 0 && !text.is_char_boundary(position) {
                position -= 1;
            }
            position
        }

        let Some(crate::tab_model::View::Native(native)) = self.view_store.get(view).copied()
        else {
            return false;
        };
        let Some(app) = self.native_runtime.app(native.instance) else {
            return false;
        };
        let legacy_markdown =
            restore.restore_tag == "markdown" && app.kind() == crate::native_app::AppKind::Editor;
        if app.vtable().restore_tag != restore.restore_tag && !legacy_markdown {
            return false;
        }
        let document_text = app
            .document_id()
            .and_then(|document| self.document_store.snapshot(document))
            .map(|snapshot| snapshot.text);
        self.native_runtime
            .change_view_state(view, |state| match state {
                crate::native_app::AppViewState::About(_)
                | crate::native_app::AppViewState::Mail(_)
                | crate::native_app::AppViewState::Settings(_)
                | crate::native_app::AppViewState::Allocation(_)
                | crate::native_app::AppViewState::Workers(_)
                | crate::native_app::AppViewState::Registry(_)
                | crate::native_app::AppViewState::Tables(_)
                | crate::native_app::AppViewState::Navigator(_)
                | crate::native_app::AppViewState::SystemResources(_)
                | crate::native_app::AppViewState::SystemState(_) => true,
                crate::native_app::AppViewState::Editor(editor) => {
                    let (Some(text), Some(buffer)) =
                        (document_text.as_deref(), editor.buffer.as_mut())
                    else {
                        return false;
                    };
                    if !restore.editor_selections.is_empty() {
                        buffer.selections = restore
                            .editor_selections
                            .iter()
                            .map(|selection| crate::native_editor::Selection {
                                anchor: clamp_boundary(text, selection.anchor),
                                head: clamp_boundary(text, selection.head),
                            })
                            .collect();
                        buffer.primary = restore
                            .primary_selection
                            .min(buffer.selections.len().saturating_sub(1));
                    }
                    if !legacy_markdown {
                        buffer.viewport_anchor = clamp_boundary(text, restore.viewport_anchor);
                    }
                    if let Some(workspace) = editor.markdown.as_mut() {
                        use crate::native_app::{MarkdownPane, MarkdownWorkspaceMode};
                        let restored = restore.markdown.as_ref();
                        if let Some(restored) = restored {
                            workspace.mode = match restored.mode.as_str() {
                                "read" => MarkdownWorkspaceMode::Read,
                                "split" => MarkdownWorkspaceMode::Split,
                                _ => MarkdownWorkspaceMode::Edit,
                            };
                            workspace.focus = match workspace.mode {
                                MarkdownWorkspaceMode::Read => MarkdownPane::Reader,
                                MarkdownWorkspaceMode::Edit => MarkdownPane::Editor,
                                MarkdownWorkspaceMode::Split if restored.focus == "reader" => {
                                    MarkdownPane::Reader
                                }
                                MarkdownWorkspaceMode::Split => MarkdownPane::Editor,
                            };
                            workspace.split_per_mille = restored.split_per_mille.clamp(200, 800);
                            workspace.reader_entry_anchor = restored
                                .reader_entry_anchor
                                .map(|anchor| clamp_boundary(text, anchor));
                        } else if legacy_markdown {
                            workspace.mode = MarkdownWorkspaceMode::Read;
                            workspace.focus = MarkdownPane::Reader;
                        }
                        if restored.is_some() || legacy_markdown {
                            let source_anchor =
                                restored.map_or(restore.source_anchor, |r| r.source_anchor);
                            let visual_row =
                                restored.map_or(restore.viewport_anchor, |r| r.visual_row);
                            let selection = restored.map_or(restore.selection, |r| r.selection);
                            workspace.reader.source_anchor = clamp_boundary(text, source_anchor);
                            workspace.reader.visual_row = visual_row;
                            workspace.reader.selection = selection.map(|selection| {
                                let anchor = clamp_boundary(text, selection.anchor);
                                let head = clamp_boundary(text, selection.head);
                                anchor.min(head)..anchor.max(head)
                            });
                            workspace.reader.history.visit(
                                crate::native_markdown::MarkdownLocation::new(
                                    workspace.reader.source_anchor,
                                    workspace.reader.visual_row,
                                ),
                            );
                        }
                        editor.common.last_focus = workspace
                            .editor_focused()
                            .then(|| crate::native_ui::UiKey::new("editor/buffer"));
                        editor.common.retire_input();
                    }
                    true
                }
                crate::native_app::AppViewState::Recovery(_) => false,
            })
    }

    fn pending_restored_agent_by_sid(
        &self,
        sid: &str,
        handoff: bool,
    ) -> Option<Box<restore::AgentRestore>> {
        let host = self.harness.as_ref()?;
        let owed = host.pending_restored_for(sid)?;
        let asked = owed.asked.filter(|_| handoff);
        Some(Box::new(restore::AgentRestore::owed(owed.snap, asked)))
    }

    fn unhanded_restored_agent(&self, id: u64) -> Option<restore::AgentRestore> {
        self.restored_agents
            .iter()
            .map(|(session, agent)| (session, agent))
            .chain(
                self.restored_held
                    .iter()
                    .filter(|(session, ..)| *session != id || !self.desk_holds(id).is_empty())
                    .map(|(session, agent, ..)| (session, agent)),
            )
            .find(|(session, _)| **session == id)
            .map(|(_, agent)| restore::AgentRestore {
                cause: None,
                ..agent.clone()
            })
    }

    fn unhanded_restored_holds(&self, id: u64) -> &[String] {
        self.restored_held
            .iter()
            .find(|(session, ..)| *session == id)
            .map_or(&[][..], |(.., rows)| rows.as_slice())
    }

    /// Populates running agent metadata onto terminal leaves of `manifest` for an unexpected exit.
    pub(crate) fn fill_desk_agents(&mut self, manifest: &mut restore::RestoreManifest) {
        manifest.fill_agents(&|id| {
            if let Some(sid) = self.session_sid(id)
                && let Some(agent) = crate::harness_host::followed_snapshot(&sid)
                    .map(restore::AgentRestore::of)
                    .or_else(|| {
                        self.pending_restored_agent_by_sid(&sid, false)
                            .map(|agent| *agent)
                    })
            {
                return Some(agent);
            }
            self.unhanded_restored_agent(id)
        });
        manifest.fill_holds(&|id| self.desk_holds(id));
        let pool = &self.pool;
        self.crash_journal
            .carry_forward(manifest, &|id| pool.get(id).and_then(Self::handed_session));
        // Whichever agent each leaf now carries, a relaunch still owed or one carried forward from
        // the predecessor's image included.
        manifest.mark_claim_bystanders();
    }

    fn desk_holds(&self, id: u64) -> Vec<String> {
        let live = self
            .pool
            .get(id)
            .map(|live| crate::fabric::render_holds(&live.ctx.fabric.holds()))
            .unwrap_or_default();
        let carried = self.unhanded_restored_holds(id);
        if carried.is_empty() {
            return live;
        }
        crate::fabric::render_holds(&crate::fabric::parse_holds(
            live.iter().chain(carried).map(String::as_str),
        ))
    }

    /// Marks the crash journal dirty when an operator Autopilot or hold setting changes on `local_id`.
    pub(crate) fn journal_owner_words(&mut self, local_id: u64) {
        if !self.crash_journal.is_active() && !self.end_session.taking() {
            return;
        }
        let Some((autopilot, land_at)) = self.pool.get(local_id).map(|s| {
            s.ctx
                .meta
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .carried_autopilot()
        }) else {
            return;
        };
        let words = crate::crash_journal::OwnerWords {
            autopilot,
            land_at,
            holds: self.desk_holds(local_id),
        };
        self.end_session.note_owner_words(local_id, &words);
        self.crash_journal.note_owner_words(local_id, &words);
    }

    #[cfg_attr(
        not(any(windows, test)),
        allow(
            dead_code,
            reason = "only the Windows lane publishes the end-session snapshot; every host tests \
                      when it is taken"
        )
    )]
    /// Records a closing session's final snapshot for agent restore.
    pub(crate) fn end_session_snapshot(
        &mut self,
        now: std::time::Instant,
    ) -> Option<Option<restore::RestoreManifest>> {
        if !self.end_session.due(now) {
            return None;
        }
        let snapshot = self.config.restore_session_or_default().then(|| {
            let mut manifest = self.capture_restore_manifest();
            self.fill_desk_agents(&mut manifest);
            manifest
        });
        self.end_session.took(snapshot.as_ref(), now);
        Some(snapshot)
    }

    /// Registers `wake` on the session store so operator word changes trigger a durability flush.
    pub(crate) fn arm_word_durability(&self, wake: impl Fn() + Send + Sync + 'static) {
        let durable = self.crash_journal.durable();
        durable.set_wake(wake);
        self.store
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .set_journal_durable(durable);
    }

    /// Populates agent restore records and operator holds onto `manifest` during a clean quit.
    pub(crate) fn quit_agents(&mut self, manifest: &mut restore::RestoreManifest, keeps: bool) {
        if keeps {
            self.fill_desk_agents(manifest);
        } else {
            manifest.forget_agents_and_holds();
        }
    }

    fn handed_session(session: &Session) -> Option<crate::crash_journal::HandedSession> {
        use crate::crash_journal::{HandedSession, Runs};
        let was = session.handoff_local_id?;
        let timeline = session
            .ctx
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let publication = timeline.agent();
        let runs = match crate::harness_host::agent_of(publication) {
            Some(agent) => Runs::Agent(agent),
            None if publication.program.is_none() => Runs::Unread,
            None => Runs::Other,
        };
        Some(HandedSession {
            was,
            runs,
            keyed: session.journal_key,
        })
    }

    fn session_cwd_and_title(s: &Session) -> (Option<String>, String) {
        use crate::cwd_native::ReportedCwd as _;
        let t = match s.term.try_lock() {
            Ok(guard) => guard,
            Err(std::sync::TryLockError::Poisoned(poison)) => poison.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {
                return (None, String::new());
            }
        };
        (
            t.native_working_directory().map(|cwd| cwd.into_owned()),
            t.title().to_string(),
        )
    }

    fn restore_session_meta(&self, id: u64) -> (Option<String>, String) {
        self.pool
            .get(id)
            .map_or((None, String::new()), Self::session_cwd_and_title)
    }

    /// Flushes a due crash-journal snapshot before the event loop parks.
    pub(crate) fn tick_crash_journal(&mut self, now: std::time::Instant) {
        if !self.crash_journal.is_active() {
            return;
        }
        if !self.config.restore_session_or_default() {
            self.crash_journal.withdraw();
            crate::restore::settle_take();
            return;
        }
        self.crash_journal.set_live(true);
        self.crash_journal.poll_writer();
        if !crash_journal_writable(
            self.desk.on_glass(self.first_present_done),
            self.restore_pass_queued(),
            self.incoming_handoff_pending,
        ) || !self.crash_journal.capture_due(now)
        {
            return;
        }
        self.crash_journal.begin_capture();
        let mut manifest = self.capture_restore_manifest();
        self.fill_desk_agents(&mut manifest);
        let programs = self.crash_journal_programs(&manifest);
        let written = self.crash_journal.offer(manifest, programs, now);
        aterm_log::debug!(
            "crash journal: capture {} {} (images {})",
            self.crash_journal.captures,
            if written { "handed over" } else { "unchanged" },
            self.crash_journal.writes
        );
    }

    /// Returns the next `ControlFlow` deadline when a pending crash-journal write is waiting on its debounce window.
    pub(crate) fn crash_journal_wake(
        &mut self,
        now: std::time::Instant,
    ) -> Option<std::time::Instant> {
        self.tick_crash_journal(now);
        self.crash_journal.next_wake(now)
    }

    fn crash_journal_programs(
        &self,
        manifest: &restore::RestoreManifest,
    ) -> Vec<crate::crash_journal::LeafProgram> {
        let mut programs = Vec::new();
        let mut captured: Vec<u64> = Vec::new();
        for (window, layout) in manifest.windows.iter().enumerate() {
            for (tab, restored) in layout.restored_tabs.iter().enumerate() {
                restored.root.visit_terminals(&mut |leaf| {
                    let Some(id) = leaf.local_id else {
                        return;
                    };
                    let agent = leaf.agent.is_some();
                    let Some(session) = self.pool.get(id) else {
                        return;
                    };
                    if session.pid <= 0
                        || session.master < 0
                        || session
                            .child_reaped
                            .load(std::sync::atomic::Ordering::Relaxed)
                    {
                        return;
                    }
                    let group = crate::quit_safety::foreground_pgrp(session.master);
                    #[cfg(unix)]
                    let root = RootAnswer::Read(crate::quit_safety::session_root(session.pid));
                    #[cfg(not(unix))]
                    let root = RootAnswer::Unasked;
                    let Some(program) = ({
                        let timeline = session
                            .ctx
                            .timeline
                            .lock()
                            .unwrap_or_else(|p| p.into_inner());
                        let agent = timeline.agent();
                        leaf_program(
                            group,
                            session.pid,
                            &root,
                            agent
                                .program
                                .as_deref()
                                .map(|name| (agent.program_pgid, name)),
                        )
                    }) else {
                        return;
                    };
                    programs.push(crate::crash_journal::LeafProgram {
                        window: u32::try_from(window).unwrap_or(u32::MAX),
                        tab: u32::try_from(tab).unwrap_or(u32::MAX),
                        program,
                        agent,
                        again: captured.contains(&id),
                    });
                    captured.push(id);
                });
            }
        }
        programs
    }

    /// Starts rebuilding the previous session's layout from `pending_restore` on first resume.
    pub(crate) fn apply_pending_restore(&mut self, el: &ActiveEventLoop) {
        let mut handed_off_leaves = Vec::new();
        let mut tasks = crate::task_launch::RestoredTasks::default();
        if let Some(manifest) = self.pending_restore.take() {
            // A cold restore hands nothing off and keeps none.
            if !self.seamless_adopt.is_empty() {
                handed_off_leaves = manifest.handed_off_leaves();
            }
            tasks = crate::task_launch::RestoredTasks::of(&manifest);
            let fold = manifest.fold.clone();
            self.apply_restore_manifest(el, manifest);
            self.say_layout_folded(&fold);
        }
        self.close_restore_pass(&handed_off_leaves, &tasks);
    }

    fn close_restore_pass(
        &mut self,
        handed_off_leaves: &[restore::TerminalLeafRestore],
        tasks: &crate::task_launch::RestoredTasks,
    ) {
        self.finish_restore_placement(handed_off_leaves, tasks);
        self.regrid_restored_windows();
    }

    fn finish_restore_placement(
        &mut self,
        handed_off_leaves: &[restore::TerminalLeafRestore],
        tasks: &crate::task_launch::RestoredTasks,
    ) {
        self.adopt_orphan_shells_as_tabs(handed_off_leaves);
        // Every record the layout brings back is seeded and every shell handed back is a tab: what
        // the layout's session IDs left in the tasks directory, and no record here keeps, goes. A
        // seamless update's successor sweeps nothing: its sessions keep their IDs, so the files
        // under the IDs its layout names are its own live sessions', which their eventual rows
        // or a saved layout keep. The predecessor ends at the Commit and removes none.
        // The sweep is one-shot for those IDs, so it waits for the session table's load: before
        // it every result is held, and would be kept for good.
        let table = self.store.read().unwrap_or_else(|p| p.into_inner()).table();
        if !self.handoff_successor {
            self.collect_restored_session_tabs();
            let sweep = crate::task_launch::PendingRestoredSweep::new(tasks.clone(), &self.store);
            self.restored_tasks_sweep = Some(sweep.clone());
            table.when_loaded(move || sweep.run());
        }
        self.seamless_held.clear();
        self.seamless_held_shown.clear();
        self.hand_restored_agents();
        self.settle_carried_settings_drafts();
        // Every session the launch brings back is registered: the table's budget may judge which
        // rows live sessions protect (tracker ST-15), and the harness host's accounts' session
        // reconcile, which waits for it, which sessions live.
        if table.sessions_settled() {
            crate::harness_host::ring();
        }
    }

    /// Older layouts could hold several session tabs in every window. Each auxiliary window
    /// keeps the tab it displayed and the main strip collects the others in saved order.
    /// A hot update runs this only after replay, while its saved topology is no longer owed.
    pub(crate) fn collect_restored_session_tabs(&mut self) {
        let Some(home) = self.tab_home_window() else {
            return;
        };
        let mut moves = Vec::new();
        for (&wid, ws) in &self.windows {
            if wid == home {
                continue;
            }
            let keep = ws.tab_set.active_id();
            for (_, tab) in ws.strip_tabs() {
                if Some(tab.id) != keep && ws.tab_set.home() != Some(tab.id) {
                    moves.push((wid, tab.id));
                }
            }
        }
        for (wid, tab) in moves {
            if let Err(why) = self.transfer_tab_between(wid, tab, home, false) {
                aterm_log::warn!(
                    "restore: tab {} remains in window {}: {why}",
                    tab.get(),
                    wid.0
                );
            }
        }
    }

    /// Re-grid every window's panes to its layout once the restore has placed all of its tabs. A
    /// shell a seamless update handed over keeps the grid its engine was parked at, which is not
    /// its window's where this build sized the window otherwise before the tabs landed (the Deck
    /// took the other form under a kept frame, [`Self::regrid_to_attach_frame`]). Switching to such
    /// a tab or drawing it does not re-grid it, nor does a resize that leaves the window's grid as it
    /// was, so it would keep that grid until something laid out the whole window. Booked
    /// `site=restore`; a pane already at its size books nothing.
    ///
    /// A window whose OS window has a size still to bring is not re-gridded here: a resize still
    /// queued, a maximize the restore asked for that it does not show yet, or a carried full screen
    /// still to enter ([`Self::restore_size_unlanded`], [`Self::carried_fullscreen_owed`]). Its tabs
    /// would be resized to a grid the window is leaving and then again. It owes the re-grid
    /// instead ([`RestoreRegridOwed`]): the grid that size commits lays out every tab
    /// ([`Self::apply_term_resize`]), and a frame pays it once nothing is to land or the wait runs
    /// out ([`Self::pay_restore_regrid_if_due`]).
    fn regrid_restored_windows(&mut self) {
        let _site = crate::resize_ledger::SiteScope::enter(crate::resize_ledger::Cause::Restore);
        let now = std::time::Instant::now();
        let windows: Vec<WindowId> = self.windows.keys().copied().collect();
        for wid in windows {
            let fullscreen = self.carried_fullscreen_owed(wid);
            if fullscreen || self.restore_size_unlanded(wid) {
                if let Some(mut route_guard_ws) = self.windows.get_mut(&wid) {
                    let ws = &mut **route_guard_ws;
                    ws.restore_regrid_owed = Some(RestoreRegridOwed {
                        closed: now,
                        since: now,
                        fullscreen,
                    });
                }
            } else {
                self.settle_restore_regrid(wid);
            }
        }
    }

    /// Whether window `wid`'s OS window has a size its grid has not taken yet: a resize still to
    /// reach the grid, or a maximize the restore asked for that it does not show yet. A window
    /// with no OS window (a headless app) has none.
    pub(crate) fn restore_size_unlanded(&self, wid: WindowId) -> bool {
        let Some(ws) = self.windows.get(&wid) else {
            return false;
        };
        let Some(w) = ws.os_window.as_ref() else {
            return false;
        };
        size_unlanded(
            self.grid_dims_for(wid, w.inner_size()),
            (ws.rows, ws.cols),
            ws.restore_maximize_asked,
            ws.restore_maximize_asked && w.is_maximized(),
        )
    }

    /// Whether the carried show state still has window `wid` to re-enter full screen (only macOS
    /// carries one).
    #[cfg(target_os = "macos")]
    pub(crate) fn carried_fullscreen_owed(&self, wid: WindowId) -> bool {
        self.carried_fullscreen_pending(wid)
    }

    /// Whether the carried show state still has window `wid` to re-enter full screen (only macOS
    /// carries one).
    #[cfg(not(target_os = "macos"))]
    #[allow(
        clippy::unused_self,
        reason = "the macOS arm reads the carried show state"
    )]
    pub(crate) const fn carried_fullscreen_owed(&self, _wid: WindowId) -> bool {
        false
    }

    /// At a frame of window `wid`, pay the re-grid the restore's close left it
    /// ([`Self::regrid_restored_windows`]) once nothing is to land, or once it has waited
    /// [`RESTORE_SHOW_SETTLE`] (counted from when a carried full screen was entered) or
    /// [`RESTORE_SHOW_CAP`] in all. Booked `site=restore`.
    pub(crate) fn pay_restore_regrid_if_due(&mut self, wid: WindowId, now: std::time::Instant) {
        let Some(owed) = self.windows.get(&wid).and_then(|ws| ws.restore_regrid_owed) else {
            return;
        };
        if self.resume_drain_regrid() {
            return;
        }
        let capped = now.saturating_duration_since(owed.closed) >= RESTORE_SHOW_CAP;
        if !capped && owed.fullscreen && self.carried_fullscreen_owed(wid) {
            if let Some(mut route_guard_ws) = self.windows.get_mut(&wid) {
                let ws = &mut **route_guard_ws;
                ws.restore_regrid_owed = Some(RestoreRegridOwed { since: now, ..owed });
            }
            return;
        }
        let waited = now.saturating_duration_since(owed.since) >= RESTORE_SHOW_SETTLE;
        if capped || waited || (!owed.fullscreen && !self.restore_size_unlanded(wid)) {
            let _site =
                crate::resize_ledger::SiteScope::enter(crate::resize_ledger::Cause::Restore);
            self.settle_restore_regrid(wid);
        }
    }

    /// Lay out every tab of window `wid` once and drop what the restore noted of its show.
    pub(crate) fn settle_restore_regrid(&mut self, wid: WindowId) {
        if let Some(mut route_guard_ws) = self.windows.get_mut(&wid) {
            let ws = &mut **route_guard_ws;
            ws.restore_regrid_owed = None;
            ws.restore_maximize_asked = false;
            // Marked only: the park pays it, outside this scope (`App::settle_dock_fall`).
            ws.dock.restore_settled();
        }
        self.resize_panes_scoped(wid, false);
    }

    pub(crate) fn resume_drain_regrid(&mut self) -> bool {
        if let Some(drain) = self.restore_drain.as_mut() {
            drain.not_before = None;
            return true;
        }
        false
    }

    /// Executes one bounded callback, including publication and the final re-grid work.
    pub(crate) fn run_restore_slice(&mut self, el: &ActiveEventLoop) -> bool {
        self.drain_restore_slice(
            RESTORE_SLICE_BUDGET,
            |app, first, wl, placed, filling| {
                app.restore_window(el, first, wl, placed, Some(filling))
            },
            |app, filling| app.attach_drained_window(el, filling),
        )
    }

    fn drain_restore_slice(
        &mut self,
        budget: std::time::Duration,
        mut begin: impl FnMut(
            &mut Self,
            bool,
            restore::WindowLayout,
            &mut RestoredWindows,
            &mut Option<FillingWindow>,
        ) -> std::ops::ControlFlow<()>,
        mut attach: impl FnMut(&mut Self, &FillingWindow) -> bool,
    ) -> bool {
        let slice_began = std::time::Instant::now();
        let mut drain = self
            .restore_drain
            .take()
            .unwrap_or_else(|| Box::new(RestoreDrain::new(self.pending_restore.take())));
        if !drain.runnable(slice_began) {
            self.restore_drain = Some(drain);
            return false;
        }
        drain.not_before = None;
        loop {
            if let Some(filling) = drain.filling.as_mut() {
                let more = self.land_next_drained_tab(filling);
                if filling.attach_owed {
                    filling.attach_owed = false;
                    let attached = attach(self, filling);
                    let published = if attached {
                        match self.publish_attached_restore_window(filling.wid) {
                            Ok(()) => true,
                            Err(error) => {
                                crate::logging::stderr_line!(
                                    "aterm-gui: session restore: could not publish attached window: {error}"
                                );
                                false
                            }
                        }
                    } else {
                        false
                    };
                    if published {
                        drain.placed.shown.push((filling.wid, filling.source.show));
                    } else {
                        self.rollback_unattached_restore_window(filling.wid);
                        if let Some(building) = filling.building.take() {
                            building.cancel(self);
                        }
                        filling.queue.clear();
                    }
                }
                drain.waiting_document = filling
                    .building
                    .as_ref()
                    .is_some_and(cold::BuildingTab::waiting);
                if !more && let Some(done) = drain.filling.take() {
                    self.finish_sliced_fill(&done);
                }
            } else if let Some(wl) = drain.windows.pop_front() {
                let first = std::mem::replace(&mut drain.first, false);
                let RestoreDrain {
                    placed, filling, ..
                } = &mut *drain;
                if begin(self, first, wl, placed, filling).is_break() {
                    drain.windows.clear();
                }
                drain.waiting_document = drain
                    .filling
                    .as_ref()
                    .and_then(|filling| filling.building.as_ref())
                    .is_some_and(cold::BuildingTab::waiting);
            } else if let Some(finalizing) = drain.finalizing.as_mut() {
                match finalizing.step(self) {
                    regrid::Step::Advanced => {}
                    regrid::Step::Wait(deadline) => drain.not_before = Some(deadline),
                    regrid::Step::Complete => break,
                }
            } else {
                self.record_carried_window_show(&drain.placed.shown, drain.placed.bootstrap);
                self.say_layout_folded(&drain.fold);
                self.finish_restore_placement(&[], &drain.tasks);
                self.refresh_disambiguated_document_titles();
                drain.finalizing = Some(regrid::Finalizing::new(self.windows.keys().copied()));
            }
            if drain.waiting_document
                || drain.not_before.is_some()
                || slice_began.elapsed() >= budget
            {
                break;
            }
        }
        if let Some(window) = drain
            .filling_window()
            .and_then(|wid| self.windows.get(&wid))
            .and_then(|ws| ws.os_window.as_ref())
        {
            window.request_redraw();
        }
        drain.slices = drain.slices.saturating_add(1);
        drain.longest = drain.longest.max(slice_began.elapsed());
        let finished = drain.filling.is_none()
            && drain.windows.is_empty()
            && drain
                .finalizing
                .as_ref()
                .is_some_and(regrid::Finalizing::finished);
        if !finished {
            self.restore_drain = Some(drain);
            return false;
        }
        aterm_log::info!(
            "session restore: drained {} window(s) in {} slice(s) over {:.1} ms, longest callback {:.1} ms",
            drain.placed.shown.len(),
            drain.slices,
            drain.began.elapsed().as_secs_f64() * 1000.0,
            drain.longest.as_secs_f64() * 1000.0
        );
        true
    }

    fn attach_drained_window(&mut self, el: &ActiveEventLoop, filling: &FillingWindow) -> bool {
        let wid = filling.wid;
        #[cfg(any(windows, target_os = "linux"))]
        self.stage_restore_placement(
            wid,
            (filling.source.outer_x, filling.source.outer_y),
            filling.source.maximized,
        );
        if !self.headless && !self.attach_restored_os_window(el, wid) {
            return false;
        }
        let wl = &filling.source;
        if !cfg!(windows)
            && let (Some(x), Some(y)) = (wl.outer_x, wl.outer_y)
            && let Some(window) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref())
            && crate::app_window::restored_position_on_screen(window, x, y)
        {
            window.set_outer_position(self.restored_origin(wid, window, x, y));
        }
        if !cfg!(windows)
            && wl.maximized == Some(true)
            && let Some(window) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref())
        {
            window.set_maximized(true);
        }
        true
    }

    fn publish_attached_restore_window(&mut self, wid: WindowId) -> Result<(), String> {
        let tabs = self
            .windows
            .get(&wid)
            .ok_or_else(|| "restored window disappeared before publication".to_string())?
            .tab_set
            .tabs()
            .to_vec();
        for tab in &tabs {
            self.publish_restore_document_views(tab)?;
        }
        for tab in tabs {
            self.publish_finished_restore_tab(wid, Some(tab.id));
        }
        Ok(())
    }

    fn rollback_unattached_restore_window(&mut self, wid: WindowId) {
        let Some(window) = self.windows.remove(&wid) else {
            return;
        };
        if self.frontmost_window == Some(wid) {
            self.frontmost_window = self.windows.keys().next().copied();
        }
        if let Some(error) = window
            .tab_set
            .tabs()
            .iter()
            .find_map(|tab| self.validate_restore_document_rollback(tab).err())
        {
            self.windows.insert(wid, window);
            crate::logging::stderr_line!(
                "aterm-gui: session restore: could not validate unattached rollback: {error}"
            );
            return;
        }
        let mut views = Vec::new();
        for tab in window.tab_set.tabs() {
            tab.root.visit(&mut |view| views.push(*view));
        }
        for view in views {
            self.rollback_restore_document_view(view)
                .expect("exclusive borrow preserves preflighted rollback custody");
        }
        self.windows.insert(wid, window);
        self.retire_unattached_restored_window(wid);
    }

    /// [`Self::restore_window`]'s native window for row `wl`, before its tabs are rebuilt and it
    /// attaches: made with no OS window, the front left as it was, holding the row's carried
    /// frame, dock record ([`crate::app_window::DockRow::of`]) and maximize ask. It needs no
    /// event loop, so a fixture drives the restore's own write of the record.
    fn begin_native_restore_window(&mut self, wl: &restore::WindowLayout) -> WindowId {
        let previous_front = self.frontmost_window;
        let wid = self.create_native_restore_window(wl.rows, wl.cols);
        self.frontmost_window = previous_front;
        if let Some(mut route_guard_ws) = self.windows.get_mut(&wid) {
            let ws = &mut **route_guard_ws;
            let frame = crate::app_window::CarriedFrame::of(wl);
            ws.take_carried_frame(frame.map(|frame| frame.inner_px));
            ws.dock.take_row(crate::app_window::DockRow::of(wl));
            ws.restore_maximize_asked = wl.maximized == Some(true);
        }
        wid
    }

    fn begin_native_sliced_fill(&mut self, mut wl: restore::WindowLayout) -> Option<FillingWindow> {
        if wl.migrate_legacy_tabs().is_none() || wl.restored_tabs.is_empty() {
            return None;
        }
        let front = wl.active_item.unwrap_or(0).min(wl.restored_tabs.len() - 1);
        let queue = std::mem::take(&mut wl.restored_tabs)
            .into_iter()
            .enumerate()
            .collect();
        wl.forget_legacy_mirror();
        let wid = WindowId(self.next_window_id);
        self.next_window_id = self.next_window_id.saturating_add(1);
        let mut host = WindowState::new_native(
            None,
            wl.rows,
            wl.cols,
            self.unattached_window_metrics(),
            crate::tab_model::TabSet::default(),
        );
        host.scale = self.unattached_window_scale();
        let frame = crate::app_window::CarriedFrame::of(&wl);
        host.take_carried_frame(frame.map(|frame| frame.inner_px));
        host.dock.take_row(crate::app_window::DockRow::of(&wl));
        host.restore_maximize_asked = wl.maximized == Some(true);
        Some(FillingWindow {
            wid,
            queue,
            placed: Vec::new(),
            front,
            shown: None,
            workers_in_front: wl.workers_in_front,
            building: None,
            bootstrap: None,
            pending_host: Some(Box::new(host)),
            source: wl,
            attach_owed: false,
        })
    }

    fn begin_sliced_fill(
        &mut self,
        wid: WindowId,
        mut wl: restore::WindowLayout,
    ) -> Option<FillingWindow> {
        if wl.is_lone_workers_list() {
            self.restore_lone_workers_list(wid);
            return None;
        }
        if wl.migrate_legacy_tabs().is_none() || wl.restored_tabs.is_empty() {
            return None;
        }
        let front = wl.active_item.unwrap_or(0).min(wl.restored_tabs.len() - 1);
        let mut queue: std::collections::VecDeque<_> = std::mem::take(&mut wl.restored_tabs)
            .into_iter()
            .enumerate()
            .collect();
        // The live bootstrap started for this window's first terminal-bearing tab,
        // which may follow native-only tabs. Build that tab first so its shell stays
        // the owner of input received while those earlier documents are pending.
        // Keep the saved indices: publication still restores their original order.
        if let Some(at) = queue
            .iter()
            .position(|(_, tab)| tab.root.first_terminal_leaf().is_some())
        {
            let bootstrap = queue.remove(at).expect("located bootstrap tab");
            queue.push_front(bootstrap);
        }
        wl.forget_legacy_mirror();
        let bootstrap = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.tab_set.tabs().first())
            .map(|tab| cold::Anchor {
                tab: tab.id,
                view: tab.focus,
                original: tab.clone(),
            });
        let mut filling = FillingWindow {
            wid,
            queue,
            placed: Vec::new(),
            front,
            shown: bootstrap.as_ref().map(|anchor| anchor.tab),
            workers_in_front: wl.workers_in_front,
            building: None,
            bootstrap,
            pending_host: None,
            source: wl,
            attach_owed: false,
        };
        self.land_next_drained_tab(&mut filling);
        if filling.building.is_none() && filling.queue.is_empty() {
            self.finish_sliced_fill(&filling);
            return None;
        }
        Some(filling)
    }

    fn land_next_drained_tab(&mut self, filling: &mut FillingWindow) -> bool {
        if !self.windows.contains_key(&filling.wid) && filling.pending_host.is_none() {
            if let Some(building) = filling.building.take() {
                building.cancel(self);
            }
            filling.queue.clear();
            return false;
        }
        if filling.building.is_none() {
            let Some((index, record)) = filling.queue.pop_front() else {
                return false;
            };
            let anchor = filling.bootstrap.take();
            let reusable = anchor.as_ref().and_then(|anchor| {
                self.view_store
                    .get(anchor.view)
                    .copied()
                    .and_then(crate::tab_model::View::terminal_session)
                    .map(|session| (anchor.view, session))
            });
            filling.building = Some(cold::BuildingTab::new(index, record, anchor, reusable));
        }
        let mut building = filling.building.take().expect("one pending tab");
        if let Some(anchor) = building.anchor.as_ref()
            && !self
                .windows
                .get(&filling.wid)
                .and_then(|ws| ws.tab_set.get(anchor.tab))
                .is_some_and(|tab| tab.root.any_leaf(&mut |view| *view == anchor.view))
        {
            building.cancel(self);
            return !filling.queue.is_empty();
        }
        let step = building.step(self, filling.wid);
        let missing_window =
            !self.windows.contains_key(&filling.wid) && filling.pending_host.is_none();
        let missing_anchor = building.anchor.as_ref().is_some_and(|anchor| {
            !self
                .windows
                .get(&filling.wid)
                .and_then(|window| window.tab_set.get(anchor.tab))
                .is_some_and(|tab| tab.root.contains(anchor.view))
        });
        if missing_window || missing_anchor {
            building.cancel(self);
            if missing_window {
                filling.queue.clear();
            }
            return !filling.queue.is_empty();
        }
        match step {
            cold::Step::Waiting | cold::Step::Advanced => {
                filling.building = Some(building);
            }
            cold::Step::Complete(built) => {
                let attaching = filling.pending_host.is_some();
                if filling.pending_host.is_none()
                    && let Err(error) = self.publish_restore_document_views(&built.0)
                {
                    building.cancel(self);
                    crate::logging::stderr_line!(
                        "aterm-gui: session restore: could not publish document views: {error}"
                    );
                    return !filling.queue.is_empty();
                }
                if let Some(host) = filling.pending_host.take() {
                    self.windows.insert(filling.wid, host);
                    filling.attach_owed = true;
                }
                if building.preserves_bootstrap(self, filling.wid) {
                    // The live shell became the person's tab. Keep its identity and selection;
                    // the saved native tabs still land in their original relative order.
                    filling.shown = None;
                    self.land_built_drained_tab(filling, building.index, built);
                } else if let Some(anchor) = building.anchor.as_ref() {
                    self.graft_drained_tab(filling, building.index, anchor, built);
                } else {
                    self.land_built_drained_tab(filling, building.index, built);
                }
                if !attaching {
                    self.publish_finished_restore_tab(
                        filling.wid,
                        filling.placed.last().map(|(_, id)| *id),
                    );
                }
                if let Some(at) = filling
                    .queue
                    .iter()
                    .position(|(index, _)| *index == filling.front)
                    && let Some(entry) = filling.queue.remove(at)
                {
                    filling.queue.push_front(entry);
                }
            }
            cold::Step::Failed(error) => {
                building.cancel(self);
                crate::logging::stderr_line!(
                    "aterm-gui: session restore: could not allocate a recovered tab: {error}"
                );
            }
        }
        filling.building.is_some() || !filling.queue.is_empty()
    }

    fn land_built_drained_tab(
        &mut self,
        filling: &mut FillingWindow,
        index: usize,
        (tab, layout): BuiltRestoreTab,
    ) {
        let wid = filling.wid;
        let id = tab.id;
        let Some(mut route_guard_window) = self.windows.get_mut(&wid) else {
            return;
        };
        let window = &mut **route_guard_window;
        let tabs = window.tab_set.tabs();
        let present = filling
            .placed
            .iter()
            .filter_map(|(placed, id)| Some((*placed, tabs.iter().position(|tab| tab.id == *id)?)));
        let target = restored_tab_position(present, index, tabs.len());
        let front_before = window.tab_set.active_id();
        let previous = terminal_tab_id_at(&window.tab_set, &self.view_store, window.tabs.active);
        window
            .tab_set
            .push(tab)
            .expect("fresh restored tab identity");
        window.tab_set.place(id, target);
        let bring_front =
            front_before.is_none() || (index == filling.front && front_before == filling.shown);
        let front = if bring_front { Some(id) } else { front_before };
        if let Some(front) = front {
            let switched = window.tab_set.switch_to(front);
            debug_assert!(switched);
        }
        if bring_front {
            filling.shown = Some(id);
        }
        if let Some(layout) = layout {
            let layout_at = terminal_projection_index(&window.tab_set, &self.view_store, id)
                .expect("terminal restore projection");
            window.layouts.insert(layout_at, layout);
        }
        let active = restored_terminal_index(&window.tab_set, &self.view_store, previous);
        window.tabs = TabIndex::new(active, window.layouts.len());
        filling.placed.push((index, id));
        drop(route_guard_window);
        if front == front_before {
            self.refresh_tab_chrome_windows(vec![wid]);
        } else {
            self.resync_active_or_window(wid);
        }
        debug_assert!(
            self.structural_invariants_ok(),
            "{}",
            self.structural_invariant_violation().unwrap_or_default(),
        );
    }

    fn publish_finished_restore_tab(
        &mut self,
        wid: WindowId,
        tab: Option<crate::tab_model::TabId>,
    ) {
        let mut views = Vec::new();
        if let Some(tab) = tab.and_then(|id| self.windows.get(&wid)?.tab_set.get(id)) {
            tab.root.visit(&mut |view| views.push(*view));
        }
        for view in views {
            let Some(crate::tab_model::View::Native(native)) = self.view_store.get(view).copied()
            else {
                continue;
            };
            if self.native_runtime.config_editor_enabled(native.instance)
                && let Some(document) = self.native_runtime.document_id(native.instance)
            {
                self.request_config_host_diagnostics(document);
            }
            self.refresh_native_presentation(wid, native.instance, view);
        }
    }

    fn graft_drained_tab(
        &mut self,
        filling: &mut FillingWindow,
        index: usize,
        anchor: &cold::Anchor,
        (built, _): BuiltRestoreTab,
    ) {
        let wid = filling.wid;
        let Some(window) = self.windows.get(&wid) else {
            return;
        };
        let Some(current) = window.tab_set.get(anchor.tab) else {
            return;
        };
        let previous = terminal_tab_id_at(&window.tab_set, &self.view_store, window.tabs.active);
        let old_projection =
            terminal_projection_index(&window.tab_set, &self.view_store, anchor.tab);
        if current.root != anchor.original.root || current.focus != anchor.original.focus {
            filling.shown = None;
        }
        let mut tab = current.clone();
        let restore_focus = tab.focus == anchor.view;
        let built_focus = built.focus;
        if !cold::graft(&mut tab.root, anchor.view, &mut Some(built.root)) {
            return;
        }
        if restore_focus {
            tab.focus = built_focus;
        }
        if tab.zoomed == anchor.original.zoomed {
            tab.zoomed = built.zoomed;
        }
        if tab.parked == anchor.original.parked {
            tab.parked = built.parked;
        }
        if tab.ephemeral == anchor.original.ephemeral {
            tab.ephemeral = built.ephemeral;
        }
        if tab.fleet == anchor.original.fleet {
            tab.fleet = built.fleet;
        }
        let mut presentations = Vec::new();
        tab.root.visit(&mut |view| {
            if let Some(presentation) = self.live_view_presentation(*view) {
                presentations.push((*view, presentation));
            }
        });
        if let Some(presentation) =
            crate::tab_model::aggregate_presentations(tab.focus, presentations)
        {
            tab.presentation = presentation;
        }
        let projection = self
            .restored_terminal_pane_layout(&tab.root, tab.focus)
            .and_then(|layout| {
                let mut sessions = Vec::new();
                tab.root.visit(&mut |view| {
                    if let Some(session) = self
                        .view_store
                        .get(*view)
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
                Some(tree)
            });
        let keeps_bootstrap = tab.root.any_leaf(&mut |view| *view == anchor.view);
        let mut route_guard_window = self.windows.get_mut(&wid).expect("graft window");
        let window = &mut **route_guard_window;
        let at = window
            .tab_set
            .tabs()
            .iter()
            .position(|tab| tab.id == anchor.tab)
            .expect("graft tab");
        let front = window.tab_set.active_id();
        window.tab_set.remove(anchor.tab).expect("graft tab");
        window.tab_set.push(tab).expect("preserved graft identity");
        window.tab_set.place(anchor.tab, at);
        if let Some(front) = front {
            window.tab_set.switch_to(front);
        }
        if let Some(at) = old_projection {
            window.layouts.remove(at);
        }
        if let Some(projection) = projection {
            let at = terminal_projection_index(&window.tab_set, &self.view_store, anchor.tab)
                .expect("terminal graft projection");
            window.layouts.insert(at, projection);
        }
        let active = restored_terminal_index(&window.tab_set, &self.view_store, previous);
        window.tabs = TabIndex::new(active, window.layouts.len());
        filling.placed.push((index, anchor.tab));
        drop(route_guard_window);
        if !keeps_bootstrap {
            let session = self
                .view_store
                .get(anchor.view)
                .copied()
                .and_then(crate::tab_model::View::terminal_session);
            self.remove_view_link(anchor.view);
            if let Some(session) = session {
                self.teardown_session(session);
            }
        }
        self.resync_active_or_window(wid);
        debug_assert!(
            self.structural_invariants_ok(),
            "{}",
            self.structural_invariant_violation().unwrap_or_default()
        );
    }

    fn finish_sliced_fill(&mut self, filling: &FillingWindow) {
        let wid = filling.wid;
        let Some(window) = self.windows.get(&wid) else {
            return;
        };
        if !filling
            .placed
            .iter()
            .any(|(index, _)| *index == filling.front)
            && window.tab_set.active_id() == filling.shown
            && let Some(nearest) = filling
                .placed
                .iter()
                .filter(|(_, id)| window.tab_set.get(*id).is_some())
                .min_by_key(|(index, _)| index.abs_diff(filling.front))
                .map(|(_, id)| *id)
            && let Some(at) = window
                .tab_set
                .tabs()
                .iter()
                .position(|tab| tab.id == nearest)
        {
            self.switch_tab_in(wid, at);
        }
        // Parked tabs landed behind the front, each with no sync of its own: the list that stands
        // for them opens now, so its chip is there as soon as the fill ends.
        self.settle_workers_tab(wid);
        if filling.workers_in_front {
            self.bring_workers_list_front(wid);
        }
        self.refresh_tab_chrome_windows(vec![wid]);
        debug_assert!(
            self.restored_window_sessions_are_distinct(wid),
            "session restore aliased two leaves of window {wid:?} onto one shell",
        );
    }

    /// Posts a log record describing layout folds applied by the outgoing build.
    pub(crate) fn say_layout_folded(&mut self, fold: &restore::LayoutFold) {
        let Some(record) = crate::update_words::layout_folded(fold) else {
            return;
        };
        aterm_log::info!(
            "update: the outgoing build folded its desk to fit the handoff layout: {} window(s) \
             merged into tabs, {} tab(s) moved to another window",
            fold.windows,
            fold.tabs
        );
        self.post_message(record);
    }

    /// Posts or updates the window message summarizing restored agent sessions.
    pub(crate) fn post_restored_row(
        &mut self,
        row: crate::harness_host::RestoredRow,
        stamp: aterm_messages::WallStamp,
    ) {
        if row.seq <= self.restored_shown.seq {
            return;
        }
        self.restored_shown.seq = row.seq;
        if self.restored_shown.id.is_some_and(|id| {
            self.messages.live(id).is_none()
                && self
                    .messages
                    .log()
                    .get(id)
                    .and_then(aterm_messages::LogRecord::retired)
                    .is_some_and(restored_row_seen_through)
        }) {
            let named = std::mem::take(&mut self.restored_shown.named);
            self.restored_shown.seen.extend(named);
            self.restored_shown.id = None;
        }
        let shown = self.shown_session_sids();
        let mut named = Vec::new();
        let mut lines = Vec::new();
        for line in &row.lines {
            let said = (line.sid.clone(), line.said);
            if self.restored_shown.seen.contains(&said) {
                continue;
            }
            let place = if shown.contains(&line.sid) {
                self.upgrade_place(&line.sid)
            } else if self.restored_shown.named.contains(&said) {
                continue;
            } else {
                line.place.clone()
            };
            named.push(said);
            lines.push((place, line.agent, line.outcome.clone()));
        }
        if lines.is_empty() {
            return;
        }
        let mut msg =
            crate::message_reporters::restored_agents_not_resumed(&lines, row.log.as_deref());
        msg.key = Some(crate::message_reporters::restored_agents_key());
        let id = self.post_unsynced(msg, stamp);
        self.restored_shown.id = Some(id);
        self.restored_shown.named = named;
    }

    fn hand_restored_agents(&mut self) {
        self.say_restored_held();
        let restored = self.take_restored_agents();
        if restored.is_empty() {
            return;
        }
        if self.harness.is_none() {
            let sessions: Vec<u64> = {
                let g = self.store.read().unwrap_or_else(|p| p.into_inner());
                restored
                    .iter()
                    .filter_map(|(sid, ..)| {
                        g.by_sid(&aterm_session::SessionId::parse(sid)?)
                            .map(|h| h.local_id)
                    })
                    .collect()
            };
            for id in sessions {
                self.restored_agent_over(id);
            }
            return;
        }
        if let Some(host) = &self.harness {
            let restored = restored
                .into_iter()
                .map(
                    |(sid, snap, asked, was)| crate::harness_host::RestoredAgent {
                        place: self.upgrade_place(&sid),
                        cold: true,
                        sid,
                        snap,
                        asked,
                        was,
                    },
                )
                .collect();
            host.relaunch_restored(restored);
        }
    }

    fn say_restored_held(&mut self) {
        let lifted: Vec<bool> = self
            .restored_held
            .iter()
            .map(|(id, ..)| self.desk_holds(*id).is_empty())
            .collect();
        let held = std::mem::take(&mut self.restored_held);
        self.say_held_agents(held.into_iter().zip(lifted));
    }

    /// Releases a held tab inside the cold-restore drain when the operator lifts its hold.
    pub(crate) fn let_go_lifted_held(&mut self, session: u64) {
        if !self.restored_held.iter().any(|(id, ..)| *id == session)
            || !self.desk_holds(session).is_empty()
        {
            return;
        }
        let (lifted, held): (Vec<_>, Vec<_>) = std::mem::take(&mut self.restored_held)
            .into_iter()
            .partition(|(id, ..)| *id == session);
        self.restored_held = held;
        self.say_held_agents(lifted.into_iter().map(|entry| (entry, true)));
    }

    /// Releases all tabs in the cold-restore drain whose operator hold has been lifted.
    pub(crate) fn let_go_every_lifted_held(&mut self) {
        let mut sessions: Vec<u64> = self.restored_held.iter().map(|(id, ..)| *id).collect();
        sessions.dedup();
        for session in sessions {
            self.let_go_lifted_held(session);
        }
    }

    fn say_held_agents(
        &mut self,
        held: impl IntoIterator<Item = ((u64, restore::AgentRestore, bool, Vec<String>), bool)>,
    ) {
        use aterm_agent::harness::upgrade::Agent;

        let relaunching = self
            .harness
            .as_ref()
            .is_some_and(crate::harness_host::HostHandle::relaunches_restored);
        let mut notices = Vec::new();
        let mut said: Vec<(u32, String)> = Vec::new();
        for ((id, agent, fleet, _), lifted) in held {
            // A held tab's agent is its holder's: the restore relaunches none.
            self.restored_agent_over(id);
            let (pid, start) = agent.ran_as();
            if said.iter().any(|(p, s)| (*p, s.as_str()) == (pid, start)) {
                continue;
            }
            let sid = {
                let g = self.store.read().unwrap_or_else(|p| p.into_inner());
                g.by_local(id).map(|h| h.sid.as_str().to_string())
            };
            let Some(sid) = sid else {
                continue;
            };
            said.push((pid, start.to_string()));
            let place = self.upgrade_place(&sid);
            aterm_log::info!(
                "restore @{sid}: the agent that ran {place} (conversation {}) is not relaunched: \
                 the tab was held when aterm ended",
                agent.session.as_deref().unwrap_or("-")
            );
            aterm_log::info!(
                "harness @{sid}: {}",
                crate::harness_host::restored_held_line(
                    agent.pid,
                    &sid,
                    agent.session.as_deref(),
                    agent.version.as_deref(),
                )
            );
            if let (Some(host), Some(was)) = (&self.harness, agent.ran_in_another(&sid)) {
                host.note_earlier_tab(&sid, &was);
            }
            let name = agent.name();
            let notice = HeldNotice {
                sid,
                place,
                stopped_by_hold: relaunching && agent.resumes() && name.is_some(),
                agent,
                fleet,
                lifted,
            };
            let reads_accounts = match notice.agent.agent() {
                Some(Agent::Codex) => !matches!(
                    notice.agent.run.as_ref(),
                    Some(restore::RunRestore::Codex(restore::CodexRestore {
                        alab: Some(restore::AlabRestore {
                            codex_home: Some(home),
                            codex_auth_path: None,
                            ..
                        }),
                        ..
                    })) if !home.is_empty()
                ),
                Some(Agent::Claude | Agent::Agy | Agent::Muse) | None => false,
            };
            if reads_accounts {
                notices.push(notice);
            } else {
                // An explicit Codex home with no named override cannot use the pool. Like
                // other agents' recorded commands, it is a pure rendering and stays immediate.
                let resume = notice.agent.resume_line_at(None);
                if let Some(message) = notice.message(resume.as_deref()) {
                    self.record_message(message);
                }
            }
        }
        if notices.is_empty() {
            return;
        }
        // If a worker cannot start, preserve the historical notice without offering an
        // unchecked command. A late result is still a historical record, not a live action.
        let fallback: Vec<_> = notices
            .iter()
            .filter_map(|notice| notice.message(None))
            .collect();
        let spawned = self.proxy.clone().ok_or(()).and_then(|proxy| {
            std::thread::Builder::new()
                .name("aterm-held-resume".into())
                .spawn(move || {
                    crate::qos::set_self(crate::qos::Role::Background);
                    let pool_seat = crate::task_launch::PoolSeat::here();
                    let seat = pool_seat.as_ref().map(crate::task_launch::PoolSeat::seat);
                    let messages = notices
                        .iter()
                        .filter_map(|notice| {
                            let resume = seat
                                .as_ref()
                                .and_then(|seat| notice.agent.resume_line_at(Some(seat)));
                            notice.message(resume.as_deref())
                        })
                        .collect();
                    let _ = proxy.send_event(crate::Wake::HeldAgentMessages(messages));
                })
                .map(|_| ())
                .map_err(|_| ())
        });
        if spawned.is_err() {
            for message in fallback {
                self.record_message(message);
            }
        }
    }

    /// Transfers unplaced restored-agent records from a predecessor handoff at Commit.
    #[cfg(unix)]
    pub(crate) fn hand_carried_restored_agents(&mut self) {
        self.adopt_carried_lost_words();
        self.queue_carried_restored_agents();
        self.hand_restored_agents();
        self.watch_handed_agents();
    }

    #[cfg(unix)]
    fn watch_handed_agents(&mut self) {
        let handed = std::mem::take(&mut self.handoff_supervised_agents);
        let Some(host) = self.harness.as_ref() else {
            return;
        };
        let agents: Vec<(String, aterm_agent::harness::relaunch::Snapshot)> = {
            let g = self.store.read().unwrap_or_else(|p| p.into_inner());
            handed
                .into_iter()
                .filter_map(|(named, agent)| {
                    let session = self
                        .pool
                        .iter()
                        .find(|session| session.handoff_local_id == Some(named))?;
                    let sid = g.by_local(session.id)?.sid.as_str().to_string();
                    let adopted = u32::try_from(session.pid).ok()?;
                    if agent.retired() {
                        aterm_log::info!(
                            "handoff @{sid}: the agent that ran here (pid {}) is not watched: {}",
                            agent.pid,
                            aterm_phase::RETIRED_AGENT
                        );
                        return None;
                    }
                    Some((sid.clone(), agent.handed_snapshot(sid, adopted)?))
                })
                .collect()
        };
        host.watch_handed(agents);
    }

    /// Adopts lost-agent diagnostic records carried across a seamless update.
    #[cfg(unix)]
    pub(crate) fn adopt_carried_lost_words(&mut self) {
        let carried = std::mem::take(&mut self.handoff_lost_words);
        let Some(host) = self.harness.as_ref() else {
            return;
        };
        if carried.is_empty() {
            return;
        }
        let words: Vec<(String, restore::LostRestore)> = {
            let g = self.store.read().unwrap_or_else(|p| p.into_inner());
            carried
                .into_iter()
                .filter_map(|(named, lost)| {
                    let session = self
                        .pool
                        .iter()
                        .find(|session| session.handoff_local_id == Some(named))?;
                    Some((g.by_local(session.id)?.sid.as_str().to_string(), lost))
                })
                .collect()
        };
        host.adopt_lost_words(words);
    }

    /// Activates harness supervision on handed-off sessions once the successor commits.
    #[cfg(unix)]
    pub(crate) fn take_over_supervision_at_commit(&mut self) {
        self.take_carried_person_stamps();
        crate::control::sweep_after_handoff(self.sock_plan.as_ref());
        self.adopt_carried_lost_words();
        #[cfg(test)]
        if let Some(probe) = self.handoff_before_host_resume.take() {
            probe(self);
        }
        if let Some(host) = self.harness.as_ref() {
            host.resume_after_handoff(self.handoff_carry_on_places());
        }
        self.hand_carried_restored_agents();
    }

    #[cfg(any(unix, test))]
    fn queue_carried_restored_agents(&mut self) {
        let carried = std::mem::take(&mut self.handoff_restored_agents);
        for (named, agent) in carried {
            match self
                .pool
                .iter()
                .find(|session| session.handoff_local_id == Some(named))
                .map(|session| session.id)
            {
                Some(session) => self.restored_agents.push((session, agent)),
                None => aterm_log::warn!(
                    "session restore: the agent the previous aterm was still relaunching in \
                     its session {named} was not carried on: that session was not adopted"
                ),
            }
        }
    }

    /// Returns `(sid, place)` pairs for all live sessions held by this process.
    #[cfg(unix)]
    pub(crate) fn handoff_carry_on_places(&self) -> Vec<(String, String)> {
        let sids: Vec<String> = {
            let g = self.store.read().unwrap_or_else(|p| p.into_inner());
            g.snapshot()
                .iter()
                .filter(|h| h.state != crate::session_store::SessionState::Exited)
                .map(|h| h.sid.as_str().to_string())
                .collect()
        };
        sids.into_iter()
            .map(|sid| {
                let place = self.upgrade_place(&sid);
                (sid, place)
            })
            .collect()
    }

    /// The restored agents to hand the harness. A tab whose agent this drops is not relaunched, so
    /// its worker's record says so ([`crate::task_record::mark_restored_over`]).
    fn take_restored_agents(&mut self) -> Vec<RestoredTake> {
        let taken = std::mem::take(&mut self.restored_agents);
        if taken.is_empty() {
            return Vec::new();
        }
        let mut handed: Vec<(u32, String)> = Vec::new();
        let mut dropped: Vec<u64> = Vec::new();
        let g = self.store.read().unwrap_or_else(|p| p.into_inner());
        let pool = &self.pool;
        let mut take_one = |id: u64, agent: restore::AgentRestore| {
            let sid = g.by_local(id)?.sid.as_str().to_string();
            let shell = u32::try_from(pool.get(id)?.pid).ok()?;
            let (pid, start) = agent.ran_as();
            if handed.iter().any(|(p, s)| (*p, s.as_str()) == (pid, start)) {
                return None;
            }
            handed.push((pid, start.to_string()));
            if agent.retired() {
                aterm_log::info!(
                    "restore @{sid}: the agent that ran here (pid {pid}) is not relaunched: {}",
                    aterm_phase::RETIRED_AGENT
                );
                return None;
            }
            if !agent.relaunched() {
                aterm_log::info!(
                    "restore @{sid}: the codex that ran here (pid {pid}) is not relaunched: its \
                     leaf carries no line aterm's copy of codex-alab's own record validated"
                );
                return None;
            }
            let asked = agent.asked();
            let was = agent.ran_in_another(&sid);
            let snap = agent.snapshot(sid.clone(), shell)?;
            Some((sid, snap, asked, was))
        };
        // Every agent not handed on is over here: its worker's record says so.
        let mut take = Vec::new();
        for (id, agent) in taken {
            match take_one(id, agent) {
                Some(one) => take.push(one),
                None => dropped.push(id),
            }
        }
        drop(g);
        for id in dropped {
            self.restored_agent_over(id);
        }
        take
    }

    /// The restored agent of session `id` is over without coming back: its worker's record, if it
    /// still reads restored, says so ([`crate::task_record::mark_restored_over`]).
    pub(crate) fn restored_agent_over(&mut self, id: u64) {
        if let Some(ctx) = self.pool.get(id).map(|live| live.ctx.clone()) {
            crate::task_record::mark_restored_over(&ctx, id);
        }
    }

    /// Re-mints carried fabric connections once all handed-off sessions have been registered.
    pub(crate) fn remint_carried_connections(&mut self) {
        use aterm_session::SessionId;
        let carried = std::mem::take(&mut self.pending_conn_carry);
        if carried.is_empty() {
            return;
        }
        let mut pairs: Vec<((String, String), Vec<String>)> = Vec::new();
        for row in carried {
            let key = (row.src, row.dst);
            match pairs.iter_mut().find(|(k, _)| *k == key) {
                Some((_, ops)) => ops.push(row.op),
                None => pairs.push((key, vec![row.op])),
            }
        }
        let mut minted_any = false;
        for ((src, dst), ops) in pairs {
            // A connection joins two sessions: a row whose ends do not read as sessions' IDs
            // names no edge to mint or to audit.
            let (Some(src), Some(dst)) = (SessionId::parse(&src), SessionId::parse(&dst)) else {
                continue;
            };
            let dst_ctx = {
                let store = self.store.read().unwrap_or_else(|p| p.into_inner());
                let src_live = store.by_sid(&src).is_some();
                let dst_ctx = store.by_sid(&dst).map(|h| h.ctx.clone());
                (src_live && src != dst).then_some(dst_ctx).flatten()
            };
            let (kind, dropped) = match &dst_ctx {
                Some(_) => crate::connections::carried_kind(&ops),
                None => (None, ops),
            };
            for op in &dropped {
                crate::session_edge_audit::emit(
                    crate::session_edge_audit::EdgeAction::Drop,
                    "handoff",
                    &src,
                    &dst,
                    op,
                );
            }
            if let (Some(kind), Some(ctx)) = (kind, dst_ctx) {
                minted_any |= crate::connections::connect_in(
                    &self.connections,
                    &src,
                    &dst,
                    &ctx.edges,
                    &ctx.nonce,
                    kind,
                    "handoff",
                );
            }
        }
        if minted_any {
            self.refresh_connection_surfaces();
        }
    }

    fn apply_restore_manifest(&mut self, el: &ActiveEventLoop, manifest: restore::RestoreManifest) {
        let mut placed = RestoredWindows::default();
        for (index, wl) in manifest.windows.into_iter().enumerate() {
            if self
                .restore_window(el, index == 0, wl, &mut placed, None)
                .is_break()
            {
                break;
            }
        }
        self.record_carried_window_show(&placed.shown, placed.bootstrap);
    }

    fn restore_window(
        &mut self,
        el: &ActiveEventLoop,
        first: bool,
        wl: restore::WindowLayout,
        placed: &mut RestoredWindows,
        sliced: Option<&mut Option<FillingWindow>>,
    ) -> std::ops::ControlFlow<()> {
        // The first persisted window maps onto the bootstrap window: session 0 already runs in
        // the persisted first-leaf cwd (seeded in `main`), so its layout is rebuilt in place
        // around it. Its geometry is not applied here: the grid and the carried frame seeded the
        // window's creation (`run`, `CarriedFrame::cold_first_seed`) and `resumed` applied its
        // position/maximize (post-attach, or staged for `attach_os_window` where a window is
        // placed before it is shown), both before this deferred pass, so the bootstrap frame
        // never visibly hops.
        if first {
            self.restore_bootstrap_window(wl, placed, sliced);
            return std::ops::ControlFlow::Continue(());
        }
        if self.restores_without_bootstrap(&wl) {
            if !wl.is_lone_workers_list()
                && let Some(filling) = sliced
            {
                *filling = self.begin_native_sliced_fill(wl);
                return std::ops::ControlFlow::Continue(());
            }
            let previous_front = self.frontmost_window;
            let outer = (wl.outer_x, wl.outer_y);
            let maximized = wl.maximized;
            let show = wl.show;
            let wid = self.begin_native_restore_window(&wl);
            #[cfg(any(windows, target_os = "linux"))]
            self.stage_restore_placement(wid, outer, maximized);
            self.restore_into_window(wid, wl);
            let restored = self
                .windows
                .get(&wid)
                .is_some_and(|ws| !ws.tab_set.is_empty());
            if !restored {
                self.windows.remove(&wid);
                self.frontmost_window = previous_front;
                return std::ops::ControlFlow::Continue(());
            }
            if !self.headless && !self.attach_restored_os_window(el, wid) {
                self.retire_unattached_restored_window(wid);
                return std::ops::ControlFlow::Continue(());
            }
            placed.shown.push((wid, show));
            if !cfg!(windows)
                && let (Some(x), Some(y)) = outer
                && let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref())
                && crate::app_window::restored_position_on_screen(w, x, y)
            {
                w.set_outer_position(self.restored_origin(wid, w, x, y));
            }
            if !cfg!(windows)
                && maximized == Some(true)
                && let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref())
            {
                w.set_maximized(true);
            }
            self.sync_active_session();
            return std::ops::ControlFlow::Continue(());
        }
        let cwd0: Option<String> = wl.bootstrap_cwd();
        let adopt = take_handed_off_shell(&mut self.seamless_adopt, wl.bootstrap_local_id());
        // A cold restore spawns the window's bootstrap shell under its leaf's tab ID.
        let kept = adopt
            .is_none()
            .then(|| wl.bootstrap_sid())
            .flatten()
            .and_then(crate::tab_ids::keep);
        let outer = (wl.outer_x, wl.outer_y);
        let maximized = wl.maximized;
        let show = wl.show;
        // The window's bootstrap shell is the restore's, as each new shell of its leaves is.
        let restoring =
            crate::tab_marks::ActScope::enter(crate::session_store::ExitActor::Harness, "restored");
        let created = self.create_window_internal_connected(
            el,
            cwd0.as_deref(),
            kept,
            adopt,
            None,
            None,
            crate::app_window::CarriedFrame::of(&wl),
            (outer, maximized),
            crate::app_window::DockRow::of(&wl),
        );
        drop(restoring);
        let Some(wid) = created else {
            crate::logging::stderr_line!(
                "aterm-gui: session restore: could not create a window; stopping here"
            );
            self.post_message(crate::message_reporters::restore_stopped_early());
            if self.handoff_ready.is_some() {
                self.handoff_degraded = true;
            }
            return std::ops::ControlFlow::Break(());
        };
        if let Some(mut route_guard_ws) = self.windows.get_mut(&wid) {
            let ws = &mut **route_guard_ws;
            ws.restore_maximize_asked = maximized == Some(true);
        }
        if !cfg!(windows)
            && let (Some(x), Some(y)) = outer
            && let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref())
            && crate::app_window::restored_position_on_screen(w, x, y)
        {
            w.set_outer_position(self.restored_origin(wid, w, x, y));
        }
        if !cfg!(windows)
            && maximized == Some(true)
            && let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref())
        {
            w.set_maximized(true);
        }
        placed.shown.push((wid, show));
        self.fill_restored_window(wid, wl, sliced);
        if self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.pending_reveal.is_some())
        {
            self.redraw_window(wid);
        }
        std::ops::ControlFlow::Continue(())
    }

    fn restore_bootstrap_window(
        &mut self,
        wl: restore::WindowLayout,
        placed: &mut RestoredWindows,
        sliced: Option<&mut Option<FillingWindow>>,
    ) {
        if let Some(front) = self.frontmost_window {
            placed.shown.push((front, wl.show));
            placed.bootstrap = Some(front);
            self.fill_restored_window(front, wl, sliced);
        }
    }

    fn fill_restored_window(
        &mut self,
        wid: WindowId,
        wl: restore::WindowLayout,
        sliced: Option<&mut Option<FillingWindow>>,
    ) {
        match sliced {
            Some(slot) => *slot = self.begin_sliced_fill(wid, wl),
            None => self.restore_into_window(wid, wl),
        }
    }

    pub(crate) fn retire_unattached_restored_window(&mut self, wid: WindowId) {
        // A native-only window adds no PTY to the adoption set; losing it must still
        // prevent a proof of the complete restored layout.
        if self.handoff_ready.is_some() {
            self.handoff_degraded = true;
        }
        self.close_window_logical(wid);
    }

    fn create_native_restore_window(&mut self, rows: u16, cols: u16) -> WindowId {
        let wid = WindowId(self.next_window_id);
        self.next_window_id = self.next_window_id.saturating_add(1);
        let metrics = self.unattached_window_metrics();
        let ws = WindowState::new_native(
            None,
            rows,
            cols,
            metrics,
            crate::tab_model::TabSet::default(),
        );
        self.insert_unattached_window(wid, ws);
        self.frontmost_window = Some(wid);
        wid
    }

    fn adopt_orphan_shells_as_tabs(&mut self, handed_off_leaves: &[restore::TerminalLeafRestore]) {
        if self.seamless_adopt.is_empty() {
            return;
        }
        let orphans = std::mem::take(&mut self.seamless_adopt);
        let can_spawn = self.proxy.is_some() || (cfg!(test) && self.headless);
        let Some(wid) = self.frontmost_window.filter(|_| can_spawn) else {
            crate::logging::stderr_line!(
                "aterm-gui: seamless: no front window to adopt {} orphan shell(s) into",
                orphans.len()
            );
            return;
        };
        let Some((rows, cols)) = self.windows.get(&wid).map(|ws| (ws.rows, ws.cols)) else {
            return;
        };
        for adopted in orphans {
            let id = self.next_session_id;
            let local_id = adopted.local_id;
            match self.spawn_orphan_shell(id, wid, rows, cols, adopted) {
                Ok(s) => {
                    if let Some(leaf) = handed_off_leaves
                        .iter()
                        .find(|leaf| leaf.local_id == Some(local_id))
                    {
                        self.carry_restored_identity(
                            leaf,
                            FillingShell::Unregistered(&s),
                            LeafIds::Retired,
                        );
                    }
                    self.next_session_id += 1;
                    self.land_orphan_tab(wid, s);
                }
                Err(e) => {
                    crate::logging::stderr_line!(
                        "aterm-gui: seamless: could not adopt an orphan shell: {e}"
                    );
                    self.post_message(crate::message_reporters::shell_lost_in_update(
                        &e.to_string(),
                    ));
                }
            }
        }
        if self.frontmost_window == Some(wid) {
            self.sync_active_session();
        } else {
            self.sync_window(wid);
        }
    }

    /// Put `session`, adopted from the keeper with no leaf to fill, into window `wid` as a tab
    /// of its own, in front; but a Mac Setup tab whose install an earlier aterm's quit or crash
    /// left running ([`App::recover_developer_tools_install`] takes it as it joins the pool)
    /// lands behind the tab the window shows, as its tries do, so the restored desk keeps its
    /// front tab. The caller syncs the window.
    pub(crate) fn land_orphan_tab(&mut self, wid: WindowId, session: Session) {
        let id = session.id;
        Self::register_session(&self.store, &session, None);
        let tree = pane::PaneTree::new(id);
        let tab = crate::register_terminal_tab(&mut self.tab_ids, &mut self.view_store, &tree)
            .expect("restored orphan tab identity space");
        self.pool_insert(session);
        let behind = self.is_developer_tools_session(id);
        if let Some(mut route_guard_ws) = self.windows.get_mut(&wid) {
            let ws = &mut **route_guard_ws;
            let front = ws.tab_set.active_id().filter(|_| behind);
            crate::app_tabs::land_terminal_tab(ws, &self.view_store, tree, tab)
                .expect("fresh restored tab id");
            if let Some(front) = front
                && ws.tab_set.switch_to(front)
            {
                crate::app_tabs::align_terminal_projection_to_active(ws, &self.view_store);
            }
        }
    }

    fn spawn_orphan_shell(
        &self,
        id: u64,
        wid: WindowId,
        rows: u16,
        cols: u16,
        adopted: crate::spawn::Adopted,
    ) -> std::io::Result<Session> {
        #[cfg(test)]
        if self.proxy.is_none() && self.headless {
            let mut session = stub_adopted_session(id, adopted.master);
            session.handoff_local_id = Some(adopted.local_id);
            session.journal_key = adopted.journal_key;
            session.frozen_path = adopted.frozen_path;
            crate::keeper_link::register(&session, adopted.keeper);
            return Ok(session);
        }
        let proxy = self
            .proxy
            .clone()
            .ok_or_else(|| std::io::Error::other("terminal spawning is unavailable"))?;
        spawn_session(
            id,
            wid,
            rows,
            cols,
            self.spawn_cell_px(wid),
            &self.session_factory,
            &proxy,
            None,
            None,
            None,
            None,
            Some(adopted),
        )
    }

    /// Synchronous window reconstruction for handoff.
    /// Legacy records are converted once; every reconstruction uses the recursive tree builder.
    pub(crate) fn restore_into_window(&mut self, wid: WindowId, mut wl: restore::WindowLayout) {
        if wl.is_lone_workers_list() {
            self.restore_lone_workers_list(wid);
            return;
        }
        if wl.migrate_legacy_tabs().is_none() {
            crate::logging::stderr_line!(
                "aterm-gui: session restore: invalid tab ordering; keeping bootstrap"
            );
            return;
        }
        if !wl.restored_tabs.is_empty() {
            self.restore_recursive_into_window(wid, wl);
        }
    }

    fn restore_recursive_into_window(&mut self, wid: WindowId, wl: restore::WindowLayout) {
        let restore::WindowLayout {
            active_item,
            workers_in_front,
            restored_tabs,
            ..
        } = wl;
        let has_terminal = restored_tabs
            .iter()
            .any(|tab| tab.root.first_terminal_leaf().is_some());
        let preserve_adopted_bootstrap = !has_terminal && self.window_holds_handed_off_shell(wid);
        let mut reusable_terminal = if preserve_adopted_bootstrap {
            None
        } else {
            self.clear_restore_target(wid, has_terminal)
        };

        self.frontmost_window = Some(wid);
        let mut built = Vec::with_capacity(restored_tabs.len());
        for record in &restored_tabs {
            match self.build_recursive_restore_tab(
                wid,
                record,
                &mut reusable_terminal,
                LeafIds::Retired,
            ) {
                Ok(tab) => built.push(tab),
                Err(error) => crate::logging::stderr_line!(
                    "aterm-gui: session restore: could not allocate a recovered tab: {error}"
                ),
            }
        }

        if let Some((view, session)) = reusable_terminal.take() {
            self.view_store.remove(view);
            self.teardown_session(session);
        }

        let restored_ids = built.iter().map(|(tab, _)| tab.id).collect::<Vec<_>>();
        // In the order `push` seats them, so `layouts` stays in canonical order even where a
        // folded manifest put a shown tab behind a parked one.
        built.sort_by_key(|(tab, _): &(crate::tab_model::Tab, _)| tab.parked);
        if let Some(mut route_guard_window) = self.windows.get_mut(&wid) {
            let window = &mut **route_guard_window;
            for (tab, layout) in built {
                if let Some(layout) = layout {
                    window.layouts.push(layout);
                }
                window
                    .tab_set
                    .push(tab)
                    .expect("fresh recursive restore tab identity");
            }
            if preserve_adopted_bootstrap {
                for (index, id) in restored_ids.iter().copied().enumerate() {
                    let moved = window.tab_set.reorder(id, index);
                    debug_assert!(moved);
                }
            }
            window.tabs = TabIndex::new(0, window.layouts.len());
            let selected = active_item
                .unwrap_or(0)
                .min(restored_ids.len().saturating_sub(1));
            if let Some(id) = restored_ids.get(selected).copied() {
                let switched = window.tab_set.switch_to(id);
                debug_assert!(switched);
            }
            if let Some(active) = window.tab_set.active_id()
                && let Some(projection) = window
                    .tab_set
                    .tabs()
                    .iter()
                    .filter(|tab| {
                        !tab.root.any_leaf(&mut |view| {
                            !matches!(
                                self.view_store.get(*view),
                                Some(crate::tab_model::View::Terminal(_))
                            )
                        })
                    })
                    .position(|tab| tab.id == active)
            {
                window.tabs.active = projection;
            }
        }

        self.resync_active_or_window(wid);
        if workers_in_front {
            self.bring_workers_list_front(wid);
        }
        debug_assert!(
            self.restored_window_sessions_are_distinct(wid),
            "session restore aliased two leaves of window {wid:?} onto one shell",
        );
        if self
            .windows
            .get(&wid)
            .is_some_and(|window| !window.tab_set.is_empty())
        {
            debug_assert!(
                self.structural_invariants_ok(),
                "{}",
                self.structural_invariant_violation().unwrap_or_default(),
            );
        }
    }

    fn restored_window_sessions_are_distinct(&self, wid: WindowId) -> bool {
        self.windows.get(&wid).is_none_or(|window| {
            let mut seen = Vec::new();
            window.tab_set.tabs().iter().all(|tab| {
                !tab.root.any_leaf(&mut |view| {
                    self.view_store
                        .get(*view)
                        .copied()
                        .and_then(crate::tab_model::View::terminal_session)
                        .is_some_and(|session| {
                            let duplicate = seen.contains(&session)
                                && !self
                                    .pool
                                    .get(session)
                                    .is_some_and(|live| live.handoff_local_id.is_some());
                            seen.push(session);
                            duplicate
                        })
                })
            })
        })
    }

    /// Rebuilds a window that held only an empty Workers list: the list is installed first and the
    /// bootstrap retired after, so the tab set never stands empty, and the person is never left a
    /// shell they did not have. A handed-off shell stays, as any restore keeps one.
    fn restore_lone_workers_list(&mut self, wid: WindowId) {
        self.frontmost_window = Some(wid);
        if self.window_holds_handed_off_shell(wid) {
            return;
        }
        if !self.install_workers_tab_keeping(wid, None) {
            crate::logging::stderr_line!(
                "aterm-gui: session restore: could not reopen a Workers list; keeping bootstrap"
            );
            return;
        }
        self.remove_restore_bootstrap_terminals(wid);
        self.resync_active_or_window(wid);
        debug_assert!(
            self.structural_invariants_ok(),
            "{}",
            self.structural_invariant_violation().unwrap_or_default()
        );
    }

    fn window_holds_handed_off_shell(&self, wid: WindowId) -> bool {
        self.windows.get(&wid).is_some_and(|window| {
            window.tab_set.tabs().iter().any(|tab| {
                tab.root.any_leaf(&mut |view| {
                    self.view_store
                        .get(*view)
                        .copied()
                        .and_then(crate::tab_model::View::terminal_session)
                        .and_then(|session| self.pool.get(session))
                        .is_some_and(|session| session.handoff_local_id.is_some())
                })
            })
        })
    }

    fn clear_restore_target(
        &mut self,
        wid: WindowId,
        retain_terminal: bool,
    ) -> Option<(crate::tab_model::ViewId, u64)> {
        let old_tabs = self.windows.get_mut(&wid).map(|mut window| {
            let window = &mut **window;
            let old = std::mem::take(&mut window.tab_set);
            window.layouts.clear();
            window.tabs = TabIndex::new(0, 0);
            window.routing.set_front(None);
            window.bind_trail_admission_source();
            window.active_terminal = None;
            window.window_focus = crate::front_content::WindowFocus::Host;
            old.tabs().to_vec()
        })?;
        let reusable = if retain_terminal {
            old_tabs.iter().find_map(|tab| {
                let mut found = None;
                tab.root.any_leaf(&mut |&view| {
                    let Some(crate::tab_model::View::Terminal(terminal)) =
                        self.view_store.get(view).copied()
                    else {
                        return false;
                    };
                    if self.pool.get(terminal.session).is_some() {
                        found = Some((view, terminal.session));
                        true
                    } else {
                        false
                    }
                });
                found
            })
        } else {
            None
        };
        for tab in old_tabs {
            tab.root.visit(&mut |&view| {
                if reusable.is_some_and(|(kept, _)| kept == view) {
                    return;
                }
                let terminal = self
                    .view_store
                    .get(view)
                    .copied()
                    .and_then(crate::tab_model::View::terminal_session);
                self.remove_view_link(view);
                if let Some(session) = terminal {
                    self.teardown_session(session);
                }
            });
        }
        reusable
    }

    fn build_recursive_restore_tab(
        &mut self,
        wid: WindowId,
        record: &restore::RestoredTab,
        reusable_terminal: &mut Option<(crate::tab_model::ViewId, u64)>,
        ids: LeafIds,
    ) -> Result<BuiltRestoreTab, String> {
        let reusable_before = *reusable_terminal;
        let mut leaves = Vec::with_capacity(record.root.leaf_count());
        let root = match self.build_recursive_restore_tree(
            wid,
            &record.root,
            reusable_terminal,
            ids,
            &mut leaves,
        ) {
            Ok(root) => root,
            Err(error) => {
                self.rollback_recursive_restore_leaves(&leaves, reusable_before);
                if reusable_before
                    .is_some_and(|(view, _)| leaves.iter().any(|(created, _, _)| *created == view))
                {
                    *reusable_terminal = reusable_before;
                }
                return Err(error);
            }
        };
        let result = self.finish_recursive_restore_tab(wid, record, root, &leaves);
        if let Ok((tab, _)) = &result {
            self.note_restored_opens(wid, tab);
        }
        if result.is_err() {
            self.rollback_recursive_restore_leaves(&leaves, reusable_before);
            if reusable_before
                .is_some_and(|(view, _)| leaves.iter().any(|(created, _, _)| *created == view))
            {
                *reusable_terminal = reusable_before;
            }
        }
        result
    }

    fn finish_recursive_restore_tab(
        &mut self,
        _wid: WindowId,
        record: &restore::RestoredTab,
        root: crate::tab_model::SplitTree<crate::tab_model::ViewId>,
        leaves: &[BuiltRestoreLeaf],
    ) -> Result<BuiltRestoreTab, String> {
        let focus =
            Self::restored_focus(&root, &record.focused_path).unwrap_or_else(|| root.first_leaf());
        let presentation = crate::tab_model::aggregate_presentations(
            focus,
            leaves
                .iter()
                .map(|(view, presentation, _)| (*view, presentation.clone())),
        )
        .ok_or_else(|| "restored tab has no presentation".to_string())?;
        let pane_layout = self.restored_terminal_pane_layout(&root, focus);
        let pane_tree = pane_layout.and_then(|layout| {
            let mut sessions = Vec::with_capacity(root.len());
            root.visit(&mut |&view| {
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
            if record.zoomed {
                tree.toggle_zoom();
            }
            Some(tree)
        });
        let tab_id = match leaves
            .iter()
            .find_map(|(_, _, staging_tab)| *staging_tab)
            .map_or_else(|| self.tab_ids.allocate(), Ok)
        {
            Ok(tab_id) => tab_id,
            Err(_) => {
                return Err("tab identity space exhausted".to_string());
            }
        };
        let mut tab =
            crate::tab_model::Tab::from_root(tab_id, root, focus, record.zoomed, presentation);
        // The first parked workers predate the ephemeral flag, but carried a task
        // record. A hidden person's tab carries no worker record even when its live
        // session has a task. A lost worker is recovered among the visible tabs.
        let legacy_worker = record.parked
            && !record.ephemeral
            && !record.root.all_terminals(&mut |leaf| leaf.task.is_none());
        let worker = pane_tree.is_some() && (record.fleet || self.tab_has_task(&tab));
        tab.ephemeral =
            worker && (record.ephemeral || legacy_worker || (record.parked && record.fleet));
        tab.parked = record.parked && pane_tree.is_some() && (!record.ephemeral || worker);
        tab.fleet = record.fleet && tab.ephemeral;
        Ok((tab, pane_tree))
    }

    fn build_recursive_restore_tree(
        &mut self,
        wid: WindowId,
        tree: &restore::RestoredSplitTree,
        reusable_terminal: &mut Option<(crate::tab_model::ViewId, u64)>,
        ids: LeafIds,
        leaves: &mut Vec<BuiltRestoreLeaf>,
    ) -> Result<crate::tab_model::SplitTree<crate::tab_model::ViewId>, String> {
        match tree {
            restore::RestoredSplitTree::Leaf { view } => {
                let (view, presentation, staging_tab) = self.build_recursive_restore_leaf(
                    wid,
                    view,
                    reusable_terminal,
                    ids,
                    wid.into(),
                )?;
                leaves.push((view, presentation, staging_tab));
                Ok(crate::tab_model::SplitTree::leaf(view))
            }
            restore::RestoredSplitTree::Split {
                axis,
                ratio,
                first,
                second,
            } => Ok(crate::tab_model::SplitTree::Split {
                axis: match axis {
                    restore::SplitKind::Horizontal => crate::tab_model::SplitAxis::Horizontal,
                    restore::SplitKind::Vertical => crate::tab_model::SplitAxis::Vertical,
                },
                ratio: ratio.clamp(0.05, 0.95),
                first: Box::new(self.build_recursive_restore_tree(
                    wid,
                    first,
                    reusable_terminal,
                    ids,
                    leaves,
                )?),
                second: Box::new(self.build_recursive_restore_tree(
                    wid,
                    second,
                    reusable_terminal,
                    ids,
                    leaves,
                )?),
            }),
        }
    }

    fn build_recursive_restore_leaf(
        &mut self,
        wid: WindowId,
        descriptor: &restore::RestoredView,
        reusable_terminal: &mut Option<(crate::tab_model::ViewId, u64)>,
        ids: LeafIds,
        target: crate::app_tabs::NativeInstallTarget,
    ) -> Result<BuiltRestoreLeaf, String> {
        self.build_recursive_restore_leaf_policy(
            wid,
            descriptor,
            reusable_terminal,
            ids,
            target,
            false,
        )
    }

    fn build_recursive_restore_leaf_policy(
        &mut self,
        wid: WindowId,
        descriptor: &restore::RestoredView,
        reusable_terminal: &mut Option<(crate::tab_model::ViewId, u64)>,
        ids: LeafIds,
        target: crate::app_tabs::NativeInstallTarget,
        strict: bool,
    ) -> Result<BuiltRestoreLeaf, String> {
        match descriptor {
            restore::RestoredView::Terminal(terminal) => {
                match self.restore_terminal_leaf(wid, terminal, reusable_terminal, ids) {
                    Ok(view) => Ok((
                        view,
                        crate::tab_model::TabPresentation {
                            title: if terminal.title.is_empty() {
                                "Terminal".to_string()
                            } else {
                                terminal.title.clone()
                            },
                            icon: None,
                            indicators: crate::tab_model::TabIndicators::default(),
                            conn: None,
                            closable: true,
                            tooltip: terminal.cwd.as_ref().map(|cwd| format!("Terminal · {cwd}")),
                        },
                        None,
                    )),
                    Err(error) if strict => Err(error),
                    Err(error) => self.restore_recovery_leaf(
                        target,
                        &restore::PlaceholderLeafRestore {
                            restore_tag: "terminal".to_string(),
                            reason: Self::bounded_recovery_text(&format!(
                                "Terminal could not be restarted: {error}"
                            )),
                            metadata: Self::bounded_recovery_text(&format!(
                                "folder: {}\ntitle: {}",
                                terminal.cwd.as_deref().unwrap_or("none"),
                                terminal.title
                            )),
                            unhanded: None,
                        },
                    ),
                }
            }
            restore::RestoredView::Native(native) => match self.restore_native_leaf(target, native)
            {
                Ok(restored) => Ok(restored),
                Err(error) if strict => Err(error),
                Err(error) => self.restore_failed_native_leaf(target, native, &error),
            },
            restore::RestoredView::Placeholder(placeholder) => {
                // A pane the outgoing process kept open after its command exited, whose final
                // screen crossed: shown read-only where its placeholder stands. A held pane open
                // in two windows is named at both leaves: the first shows its screen, each later
                // one that same session again, as a co-viewed live shell's leaves do.
                if let Some((leaf, local_id)) = placeholder
                    .unhanded
                    .as_deref()
                    .and_then(|leaf| Some((leaf, leaf.local_id?)))
                {
                    let shown = match self.held_shown_in_this_restore(local_id) {
                        Some(session) => Some(self.show_held_pane_again(session)),
                        None => self
                            .take_held_pane(local_id)
                            .map(|held| self.restore_held_leaf(held, leaf)),
                    };
                    match shown {
                        Some(Ok(built)) => return Ok(built),
                        Some(Err(error)) => aterm_log::warn!(
                            "restore: a held pane's screen could not be shown ({error}); \
                             keeping its placeholder"
                        ),
                        None => {}
                    }
                }
                self.restore_recovery_leaf(target, placeholder)
            }
        }
    }

    fn with_detached_restore_leaf(
        &mut self,
        wid: WindowId,
        build: impl FnOnce(&mut Self) -> Result<BuiltRestoreLeaf, String>,
    ) -> Result<BuiltRestoreLeaf, String> {
        assert!(
            self.restore_leaf_staging.is_none(),
            "nested restore leaf staging"
        );
        self.restore_leaf_staging = Some(RestoreLeafStaging { wid, tab: None });
        let result = build(self);
        if let Some(tab) = self
            .restore_leaf_staging
            .take()
            .and_then(|staging| staging.tab)
        {
            tab.root.visit(&mut |view| {
                if let Err(error) = self.rollback_restore_document_view(*view) {
                    crate::logging::stderr_line!(
                        "aterm-gui: session restore: could not roll back staged view: {error}"
                    );
                }
            });
            self.remove_tab_views(&tab);
        }
        result
    }

    fn restore_failed_native_leaf(
        &mut self,
        target: crate::app_tabs::NativeInstallTarget,
        native: &restore::NativeLeafRestore,
        error: &str,
    ) -> Result<BuiltRestoreLeaf, String> {
        self.carried_settings_drafts
            .lost
            .extend(native.settings_drafts.iter().cloned());
        self.carried_settings_drafts.unreadable = self
            .carried_settings_drafts
            .unreadable
            .saturating_add(native.settings_drafts_unreadable);
        self.restore_recovery_leaf_with_capability(
            target,
            &restore::PlaceholderLeafRestore {
                restore_tag: native.restore_tag.clone(),
                reason: Self::bounded_recovery_text(error),
                metadata: Self::bounded_recovery_text(&format!(
                    "route: {}\nfile: {}",
                    native.route.as_deref().unwrap_or("none"),
                    native.uri.as_deref().unwrap_or("none")
                )),
                unhanded: None,
            },
            recovery_capability(native),
        )
    }

    fn build_admitted_restore_leaf(
        &mut self,
        wid: WindowId,
        descriptor: &restore::NativeLeafRestore,
        result: Result<crate::app_documents::RestoreAdmitted, String>,
    ) -> Result<BuiltRestoreLeaf, String> {
        self.build_admitted_restore_leaf_policy(wid, descriptor, result, false)
    }

    fn build_admitted_restore_leaf_policy(
        &mut self,
        wid: WindowId,
        descriptor: &restore::NativeLeafRestore,
        result: Result<crate::app_documents::RestoreAdmitted, String>,
        strict: bool,
    ) -> Result<BuiltRestoreLeaf, String> {
        let mark = self.restore_staging_mark(wid);
        let restored = result
            .and_then(|admitted| self.install_restore_admitted_document(wid, admitted))
            .and_then(|()| self.finish_restore_native_leaf(wid, descriptor, &mark, None));
        match restored {
            Ok(leaf) => Ok(leaf),
            Err(error) if strict => Err(error),
            Err(error) => self.restore_failed_native_leaf(
                crate::app_tabs::NativeInstallTarget::DetachedRestore(wid),
                descriptor,
                &error,
            ),
        }
    }

    fn take_held_pane(&mut self, local_id: u64) -> Option<crate::seamless::HeldPane> {
        let index = self
            .seamless_held
            .iter()
            .position(|held| held.local_id == local_id)?;
        Some(self.seamless_held.remove(index))
    }

    fn held_shown_in_this_restore(&self, local_id: u64) -> Option<u64> {
        self.seamless_held_shown
            .iter()
            .find(|(held, _)| *held == local_id)
            .map(|(_, session)| *session)
            .filter(|session| self.pool.get(*session).is_some())
    }

    fn show_held_pane_again(&mut self, session: u64) -> Result<BuiltRestoreLeaf, String> {
        self.pool.attach(session);
        let Ok(view) = self.view_store.insert_terminal(session) else {
            self.detach_session_view(session);
            return Err("terminal view identity space exhausted".to_string());
        };
        Ok((view, self.held_pane_presentation(session), None))
    }

    fn held_pane_presentation(&self, session: u64) -> crate::tab_model::TabPresentation {
        let title = self
            .pool
            .get(session)
            .map(|session| crate::term_lock(&session.term).title().to_string())
            .unwrap_or_default();
        crate::tab_model::TabPresentation {
            title: if title.is_empty() {
                "Terminal".to_string()
            } else {
                title
            },
            icon: None,
            indicators: crate::tab_model::TabIndicators::default(),
            conn: None,
            closable: true,
            tooltip: None,
        }
    }

    fn restore_held_leaf(
        &mut self,
        held: crate::seamless::HeldPane,
        leaf: &restore::TerminalLeafRestore,
    ) -> Result<BuiltRestoreLeaf, String> {
        let id = self.next_session_id;
        let sid = held
            .sid
            .filter(|sid| {
                self.store
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .by_sid(sid)
                    .is_none()
            })
            .inspect(crate::tab_ids::note_here)
            .unwrap_or_else(crate::tab_ids::mint);
        let mut session = crate::held_pane_session(
            id,
            sid,
            &held.checkpoint,
            self.session_factory.terminal_config.as_ref(),
        );
        if let Some(exit) = held.exit {
            let _ = session.child_exit.set(exit);
        }
        session.handoff_history = crate::held_pane_history(&session.term, held.history);
        let carried_block =
            crate::session_status::completed_block(&crate::term_lock(&session.term));
        let mut control = held.control;
        *session
            .ctx
            .turns
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            crate::handoff_carry::adopted_ledger(control.as_mut());
        if let Some(control) = control {
            crate::spawn::install_adopted_archive(
                &mut crate::term_lock(&session.term),
                control,
                id,
            );
        }
        Self::seed_restored_user_meta(&session, leaf);
        // Before registration, so the log's open keeps the leaf's first open: under its own sid
        // the pane is the same session, and under a minted one it stands in for the leaf's.
        self.carry_restored_lineage(leaf, FillingShell::Unregistered(&session));
        let view = self
            .view_store
            .insert_terminal(id)
            .map_err(|_| "terminal view identity space exhausted".to_string())?;
        self.next_session_id = self.next_session_id.saturating_add(1);
        Self::register_session(&self.store, &session, None);
        self.record_session_exited(id);
        self.pool_insert(session);
        self.carry_restored_theme(id, leaf);
        self.index_autopilot_timer(id, Some(crate::autopilot_timers::ClosedOver::Aterm));
        self.seamless_held_shown.push((held.local_id, id));
        self.note_carried_exit(id, held.failure_seen, carried_block);
        Ok((view, self.held_pane_presentation(id), None))
    }

    fn restore_terminal_leaf(
        &mut self,
        wid: WindowId,
        terminal: &restore::TerminalLeafRestore,
        reusable_terminal: &mut Option<(crate::tab_model::ViewId, u64)>,
        ids: LeafIds,
    ) -> Result<crate::tab_model::ViewId, String> {
        // THE GRAFT CARRIES THE LEAF'S USER IDENTITY, not just its position. The
        // reusable terminal is the window's already-running BOOTSTRAP session —
        // on a seamless update, the adopted shell itself (session 0 is adopted
        // in `main_entry`, every further window's in `create_window_internal_connected`, each
        // picked by `WindowLayout::bootstrap_local_id`, i.e. by this leaf); on a
        // cold restore, the shell those same two spawned for the window. Either
        // way it started with a fresh ctx, so whatever the operator stamped with
        // `meta set` in the previous process exists only in this leaf. With the
        // view returned without it, a handoff of two windows, one terminal each,
        // comes back with `user_title=- description=- icon=- role=- attention=-` on
        // BOTH of its sessions: every session there is a graft, and only a leaf that
        // spawns or re-adopts a session below is re-seeded. Which shell the leaf's
        // identity lands on is `carry_restored_identity`'s business, and what it
        // may overwrite there is `graft_restored_user_meta`'s.
        if let Some((view, session)) = reusable_terminal.take() {
            self.carry_restored_identity(terminal, FillingShell::Registered(session), ids);
            self.carry_restored_theme(session, terminal);
            return Ok(view);
        }
        if let Some(session) = self.adopted_in_this_restore(terminal, ids) {
            self.carry_restored_identity(terminal, FillingShell::Registered(session), ids);
            self.refresh_meta_dependent_chrome(session);
            self.mark_status_dirty();
            self.pool.attach(session);
            return match self.view_store.insert_terminal(session) {
                Ok(view) => Ok(view),
                Err(_) => {
                    self.detach_session_view(session);
                    Err("terminal view identity space exhausted".to_string())
                }
            };
        }
        if ids == LeafIds::Live
            && let Some(session) = terminal
                .local_id
                .filter(|session| self.pool.get(*session).is_some())
        {
            self.pool.attach(session);
            return match self.view_store.insert_terminal(session) {
                Ok(view) => Ok(view),
                Err(_) => {
                    self.detach_session_view(session);
                    Err("terminal view identity space exhausted".to_string())
                }
            };
        }
        let id = self.next_session_id;
        #[cfg(test)]
        if self.proxy.is_none() && self.headless {
            let shell = take_handed_off_shell(&mut self.seamless_adopt, terminal.local_id);
            let kept = shell
                .is_none()
                .then(|| self.kept_leaf_identity(terminal, ids))
                .flatten();
            let mut session = match kept {
                Some(kept) => crate::stub_session_as(id, kept),
                None => crate::stub_session(id),
            };
            if let Some(shell) = shell {
                session.handoff_local_id = Some(shell.local_id);
                session.journal_key = shell.journal_key;
                session.frozen_path = shell.frozen_path;
                crate::spawn::seed_adopted_fabric(
                    &session.ctx.fabric,
                    &session.ctx.timeline,
                    &shell.topics,
                    &shell.holds,
                    &shell.fabric,
                );
                crate::keeper_link::register(&session, shell.keeper);
            }
            self.carry_restored_identity(terminal, FillingShell::Unregistered(&session), ids);
            let view = self
                .view_store
                .insert_terminal(id)
                .map_err(|_| "terminal view identity space exhausted".to_string())?;
            self.next_session_id = self.next_session_id.saturating_add(1);
            Self::register_session(&self.store, &session, None);
            self.pool_insert(session);
            self.carry_restored_theme(id, terminal);
            return Ok(view);
        }
        let proxy = self
            .proxy
            .clone()
            .ok_or_else(|| "terminal spawning is unavailable".to_string())?;
        let (rows, cols) = self
            .windows
            .get(&wid)
            .map(|window| (window.rows, window.cols))
            .ok_or_else(|| "restore window disappeared".to_string())?;
        let adopt = take_handed_off_shell(&mut self.seamless_adopt, terminal.local_id);
        let kept = adopt
            .is_none()
            .then(|| self.kept_leaf_identity(terminal, ids))
            .flatten();
        #[cfg(windows)]
        let shell_factory = adopt
            .is_none()
            .then(|| self.shell_factory(terminal.theme.as_deref()));
        #[cfg(windows)]
        let factory = shell_factory.as_ref().unwrap_or(&self.session_factory);
        #[cfg(not(windows))]
        let factory = &self.session_factory;
        let session = spawn_session(
            id,
            wid,
            rows,
            cols,
            self.spawn_cell_px(wid),
            factory,
            &proxy,
            terminal.start_cwd().as_deref(),
            None,
            None,
            kept,
            adopt,
        )
        .map_err(|error| error.to_string())?;
        self.carry_restored_identity(terminal, FillingShell::Unregistered(&session), ids);
        self.next_session_id = self.next_session_id.saturating_add(1);
        let view = self
            .view_store
            .insert_terminal(id)
            .map_err(|_| "terminal view identity space exhausted".to_string())?;
        Self::register_session(&self.store, &session, None);
        self.pool_insert(session);
        self.carry_restored_theme(id, terminal);
        Ok(view)
    }

    /// The tab ID a cold restore keeps for `terminal`'s new shell (LT-01: a tab keeps its ID until
    /// it closes, across a quit, reboot or crash with no keeper): the leaf's own, unless a tab of
    /// this process or a live instance on this Mac holds it. A reopened closed tab
    /// ([`LeafIds::Live`]) is a new tab, with a new ID.
    pub(crate) fn kept_leaf_identity(
        &self,
        terminal: &restore::TerminalLeafRestore,
        ids: LeafIds,
    ) -> Option<aterm_session::SessionId> {
        if ids != LeafIds::Retired {
            return None;
        }
        let sid = aterm_session::SessionId::parse(terminal.sid.as_deref()?)?;
        let held = self
            .store
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .by_sid(&sid)
            .is_some();
        if held {
            return None;
        }
        crate::tab_ids::keep(sid.as_str())
    }

    fn carry_restored_theme(&mut self, id: u64, leaf: &restore::TerminalLeafRestore) {
        if self.pool.theme_decided(id) {
            return;
        }
        if let Some(selector) = leaf.theme.clone() {
            let theme = Some(crate::session_theme::SessionTheme {
                selector,
                origin: crate::session_theme::SessionThemeOrigin::Restored,
            });
            if let Some(trial) = self
                .palette_tab_theme
                .as_mut()
                .filter(|trial| trial.session == id)
            {
                trial.prior = theme;
            } else {
                self.set_session_theme(id, theme);
            }
        }
    }

    fn adopted_in_this_restore(
        &self,
        leaf: &restore::TerminalLeafRestore,
        ids: LeafIds,
    ) -> Option<u64> {
        let local_id = leaf.local_id.filter(|_| ids == LeafIds::Retired)?;
        self.adopted_handoff_session(local_id)
    }

    fn adopted_handoff_session(&self, local_id: u64) -> Option<u64> {
        if !self.handoff_successor {
            return None;
        }
        self.pool
            .iter()
            .find(|session| session.handoff_local_id == Some(local_id))
            .map(|session| session.id)
    }

    /// Returns whether `wl` should be rebuilt into an empty window host without a bootstrap shell.
    pub(crate) fn restores_without_bootstrap(&self, wl: &restore::WindowLayout) -> bool {
        if wl.is_lone_workers_list() {
            return true;
        }
        let native_only =
            wl.holds_no_terminal() && !(wl.restored_tabs.is_empty() && wl.native_tabs.is_empty());
        native_only
            || (!wl.restored_tabs.is_empty()
                && wl
                    .bootstrap_local_id()
                    .and_then(|local_id| self.adopted_handoff_session(local_id))
                    .is_some())
    }

    fn seed_restored_user_meta(session: &crate::Session, leaf: &restore::TerminalLeafRestore) {
        if leaf.user_title.is_none()
            && leaf.description.is_none()
            && leaf.icon.is_none()
            && leaf.role.is_none()
            && leaf.attention.is_none()
            && leaf.questions.is_none()
            && leaf.autopilot.is_none()
            && !leaf.pinned
        {
            return;
        }
        let mut meta = session.ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
        let _ = meta.set("title", leaf.user_title.clone());
        let _ = meta.set("description", leaf.description.clone());
        let _ = meta.set("icon", leaf.icon.clone());
        let _ = meta.set("role", leaf.role.clone());
        let _ = meta.set("attention", leaf.attention.clone());
        let _ = meta.set("questions", leaf.questions.clone());
        // An Autopilot word aterm wrote on this session as its record was seeded stands, as a
        // driver's write stands against a graft (`session_timeline::restore_carried_meta`): the
        // Off of a disk worker an earlier build left (`task_record::retire_session`).
        if !meta.driver_wrote(crate::session_timeline::MetaField::Autopilot) {
            let _ = meta.set_autopilot(leaf.autopilot.clone(), leaf.carried_land_at());
            meta.autopilot_set_by = leaf.carried_set_by().filter(|_| meta.autopilot.is_some());
        }
        meta.pinned = leaf.pinned;
        drop(meta);
        crate::session_timeline::keep_worker_autopilot(&session.ctx);
    }

    fn carry_restored_identity(
        &mut self,
        leaf: &restore::TerminalLeafRestore,
        filling: FillingShell<'_>,
        ids: LeafIds,
    ) {
        self.carry_restored_lineage(leaf, filling);
        let mut held_at = None;
        let filled_by_a_live_shell = match &filling {
            FillingShell::Registered(session) => self
                .pool
                .get(*session)
                .is_some_and(|live| live.handoff_local_id.is_some()),
            FillingShell::Unregistered(session) => session.handoff_local_id.is_some(),
        } || leaf.local_id.is_some_and(|named| {
            self.pool
                .iter()
                .any(|live| live.handoff_local_id == Some(named))
        });
        // A worker's record (its tab parked or ephemeral), which a seamless update carries on its
        // own road.
        let mut not_resumed = false;
        if ids == LeafIds::Retired
            && !self.handoff_successor
            && let Some(row) = &leaf.task
        {
            let (session, ctx) = match &filling {
                FillingShell::Registered(session) => (
                    *session,
                    self.pool.get(*session).map(|live| live.ctx.clone()),
                ),
                FillingShell::Unregistered(session) => (session.id, Some(session.ctx.clone())),
            };
            if let Some(ctx) = ctx {
                not_resumed = crate::task_record::seed_restored(
                    &ctx,
                    row,
                    leaf.agent.is_some(),
                    filled_by_a_live_shell,
                );
                // An end the person could read before the quit is no news after it: it comes back
                // seen, so no chip, row or `•` lights for it again. An end the restore itself made
                // (an agent that is not coming back) is news.
                let carried_end = ctx
                    .timeline
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .task()
                    .filter(|record| {
                        record.state.has_an_end_to_read()
                            && record.carried_entered == Some(record.entered)
                    })
                    .map(|record| record.entered);
                if let Some(entered) = carried_end {
                    self.done_seen.insert(session, entered);
                }
            }
        }
        // A disk worker an earlier build left, kept as a tab, carries no record, and is as
        // disposable as a parked one: its agent's folder says what it is.
        if leaf.task.is_none() && !filled_by_a_live_shell {
            not_resumed |= leaf.agent.as_ref().is_some_and(|agent| {
                crate::task_record::disposable_worker(Some(agent.cwd.as_str()))
            });
        }
        if ids == LeafIds::Retired && !self.handoff_successor {
            let ctx = match &filling {
                FillingShell::Registered(session) => {
                    self.pool.get(*session).map(|live| live.ctx.clone())
                }
                FillingShell::Unregistered(session) => Some(session.ctx.clone()),
            };
            if let Some(ctx) = ctx {
                self.carry_restored_managers(leaf.sid.as_deref(), &ctx);
            }
        }
        // A claim worker's tab: the worker brought back, or still running in a shell the PTY
        // keeper hands back, or a tab whose worker is not coming back, its leaf with no agent to
        // relaunch (its shell is all an ended worker leaves, and a quit forgets a running one's)
        // or its worker ended. A layout before `claim_worker` names only an agent, one a later
        // writer says is someone's own (`not_claim_worker`) none.
        let worker = leaf
            .claim_worker
            .as_deref()
            .or_else(|| {
                leaf.agent
                    .as_ref()
                    .filter(|_| !leaf.not_claim_worker)
                    .map(|agent| agent.cwd.as_str())
            })
            .map(|folder| {
                let gone = leaf.agent.is_none() && !filled_by_a_live_shell;
                (folder, leaf.claim_ended || gone)
            });
        if ids == LeafIds::Retired
            && !self.handoff_successor
            && let Some((folder, ended)) = worker
        {
            let (session, ctx) = match &filling {
                FillingShell::Registered(session) => (
                    *session,
                    self.pool.get(*session).map(|live| live.ctx.clone()),
                ),
                FillingShell::Unregistered(session) => (session.id, Some(session.ctx.clone())),
            };
            if let Some(ctx) = ctx {
                crate::harness_host::credit_claim::note_restored_worker(
                    &ctx, session, folder, ended,
                );
            }
        }
        if ids == LeafIds::Retired
            && let Some(agent) = &leaf.agent
        {
            if !self.handoff_successor && filled_by_a_live_shell {
                let (session, shell) = match &filling {
                    FillingShell::Registered(id) => (*id, self.pool.get(*id)),
                    FillingShell::Unregistered(session) => (session.id, Some(*session)),
                };
                if let Some(ctx) = shell.map(|shell| shell.ctx.clone()) {
                    self.check_kept_worker(session, &ctx, agent);
                }
                aterm_log::info!("restore: the reattached shell keeps its agent; not relaunched");
            } else if !self.handoff_successor && not_resumed {
                aterm_log::info!(
                    "restore: a disk worker an earlier build left is not resumed after aterm ended"
                );
            } else if !self.handoff_successor {
                let session = match &filling {
                    FillingShell::Registered(session) => *session,
                    FillingShell::Unregistered(session) => session.id,
                };
                let holds = leaf.carried_holds();
                if holds.is_held() {
                    aterm_log::info!(
                        "restore: the tab was held when aterm ended ({} hold); its agent is the \
                         holder's, not relaunched",
                        holds.origins()
                    );
                    held_at = Some(self.restored_held.len());
                    self.restored_held.push((
                        session,
                        (**agent).clone(),
                        !holds.has("local"),
                        Vec::new(),
                    ));
                } else {
                    self.restored_agents.push((session, (**agent).clone()));
                }
            } else if let Some(named) = leaf.local_id
                && !self
                    .handoff_restored_agents
                    .iter()
                    .any(|(owed, _)| *owed == named)
            {
                self.handoff_restored_agents
                    .push((named, (**agent).clone()));
            }
        }
        if ids == LeafIds::Retired
            && self.handoff_successor
            && leaf.agent.is_none()
            && let (Some(named), Some(supervised)) = (leaf.local_id, &leaf.supervised)
            && !self
                .handoff_supervised_agents
                .iter()
                .any(|(watched, _)| *watched == named)
        {
            self.handoff_supervised_agents
                .push((named, (**supervised).clone()));
        }
        if ids == LeafIds::Retired
            && self.handoff_successor
            && let (Some(named), Some(lost)) = (leaf.local_id, &leaf.lost)
            && !self.handoff_lost_words.iter().any(|(tab, _)| *tab == named)
        {
            self.handoff_lost_words.push((named, (**lost).clone()));
        }
        let filling_adopted_as = match filling {
            FillingShell::Registered(session) => self
                .pool
                .get(session)
                .and_then(|live| live.handoff_local_id),
            FillingShell::Unregistered(session) => session.handoff_local_id,
        };
        let from_handoff = ids == LeafIds::Retired && self.handoff_successor;
        let the_tabs_session = filling_adopted_as.is_none() || filling_adopted_as == leaf.local_id;
        let owners = leaf.carried_holds().of_origin("local");
        if !from_handoff && the_tabs_session && owners.is_held() {
            match &filling {
                FillingShell::Registered(session) => {
                    if let Some(live) = self.pool.get(*session)
                        && !live.ctx.fabric.holds().has("local")
                    {
                        for hold in owners.standing() {
                            let wake = || live.ctx.watchers.ring_watchers();
                            if !crate::fabric::carry_hold_waking(&live.ctx, hold, &wake) {
                                aterm_log::warn!(
                                    "session restore: the owner's hold on a reopened tab was not \
                                     put back: an update is committing the session; its agent is \
                                     not relaunched"
                                );
                            }
                        }
                    }
                }
                FillingShell::Unregistered(session) => {
                    session.ctx.fabric.seed_holds(owners, &session.ctx.timeline);
                }
            }
        }
        if let Some(at) = held_at {
            let standing = match &filling {
                FillingShell::Registered(session) => self
                    .pool
                    .get(*session)
                    .map(|live| live.ctx.fabric.holds())
                    .unwrap_or_default(),
                FillingShell::Unregistered(session) => session.ctx.fabric.holds(),
            };
            let unput: Vec<String> = leaf
                .holds
                .iter()
                .filter(|row| !standing.has(&crate::fabric::parse_hold(row).origin))
                .cloned()
                .collect();
            if let Some(held) = self.restored_held.get_mut(at) {
                held.3 = unput;
            }
        }
        if !from_handoff
            || leaf
                .local_id
                .is_some_and(|named| filling_adopted_as == Some(named))
        {
            match filling {
                FillingShell::Registered(session) => {
                    self.graft_restored_user_meta(session, leaf);
                }
                FillingShell::Unregistered(session) => {
                    Self::seed_restored_user_meta(session, leaf);
                    let closed = match ids {
                        LeafIds::Live => crate::autopilot_timers::ClosedOver::Tab,
                        LeafIds::Retired => crate::autopilot_timers::ClosedOver::Aterm,
                    };
                    self.index_autopilot_timer_of(session.id, &session.ctx, Some(closed));
                }
            }
            return;
        }
        let identity = match leaf.local_id.and_then(|named| self.adopted_session(named)) {
            Some(named) => {
                if self.graft_restored_user_meta(named, leaf) {
                    self.refresh_meta_dependent_chrome(named);
                    self.mark_status_dirty();
                    "; that pane's identity went to the shell it names, which runs in another pane"
                } else {
                    ""
                }
            }
            None if leaf.carried_user_meta() != crate::session_timeline::SessionMeta::default() => {
                "; the shell it names is not running here yet, and the orphan net puts that \
                 pane's identity on it if it places it"
            }
            None => "",
        };
        crate::logging::stderr_line!(
            "aterm-gui: session restore: a pane was filled by a shell its layout did not \
             name{identity}"
        );
    }

    /// A WORKER'S MANAGER STAYS ITS MANAGER ACROSS A COLD RESTART, which keeps a leaf's tab ID
    /// ([`Self::kept_leaf_identity`]) and gives its session a new sid only where it cannot (a sid
    /// that is no tab ID, an ID a tab of this process holds or a live instance on this Mac serves,
    /// a reopened closed tab): the leaf that held sid `was` came back as `ctx`, so where the two
    /// differ every record whose `from=` names `was` names `ctx`'s sid now, and `ctx`'s own record
    /// names its manager's new sid where that manager came back before it under a new one. A
    /// manager that did not come back keeps the sid it had, which no session here holds.
    fn carry_restored_managers(&mut self, was: Option<&str>, ctx: &crate::SessionCtx) {
        let now = ctx.self_id.as_str();
        if let Some(was) = was.filter(|was| *was != now) {
            self.restored_sids.insert(was.to_owned(), now.to_owned());
            for live in self.pool.iter() {
                restored_manager_named(&live.ctx, &self.restored_sids);
            }
        }
        restored_manager_named(ctx, &self.restored_sids);
    }

    fn carry_restored_lineage(
        &self,
        leaf: &restore::TerminalLeafRestore,
        filling: FillingShell<'_>,
    ) {
        let shell = match filling {
            FillingShell::Registered(session) => self.pool.get(session),
            FillingShell::Unregistered(session) => Some(session),
        };
        let Some(shell) = shell else {
            return;
        };
        let named = |s: &crate::Session| {
            leaf.sid.as_deref() == Some(s.ctx.self_id.as_str())
                || (leaf.local_id.is_some() && s.handoff_local_id == leaf.local_id)
        };
        // The same session (a seamless update's handed-over shell, or a cold restore's shell
        // under the leaf's own tab ID) takes the leaf's lineage and first open with no record; a
        // new shell standing in for the leaf's session records that it does.
        let seed = |s: &crate::Session| {
            let mut tl = s.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            tl.slog_mut().seed_lineage(leaf.lineage.as_deref());
            tl.slog_mut().seed_opened(leaf.opened_ms);
        };
        if named(shell) {
            seed(shell);
        } else if shell.handoff_local_id.is_none() {
            let _ = note_restored(
                shell,
                leaf.sid.as_deref(),
                leaf.lineage.as_deref(),
                leaf.opened_ms,
            );
        } else if let Some(other) = leaf
            .local_id
            .and_then(|l| self.adopted_session(l))
            .and_then(|s| self.pool.get(s))
        {
            seed(other);
        }
    }

    fn adopted_session(&self, local_id: u64) -> Option<u64> {
        self.pool.sessions.iter().find_map(|(&session, pooled)| {
            (pooled.session.handoff_local_id == Some(local_id)).then_some(session)
        })
    }

    /// Applies carried user metadata from `leaf` onto live `session`.
    pub(crate) fn graft_restored_user_meta(
        &mut self,
        session: u64,
        leaf: &restore::TerminalLeafRestore,
    ) -> bool {
        let Some(live) = self.pool.get(session) else {
            return false;
        };
        let ctx = live.ctx.clone();
        let lands = self.harness_ab_reading(session).1;
        let mut moved =
            crate::session_timeline::restore_carried_meta(&ctx, &leaf.carried_user_meta());
        moved |= crate::session_timeline::keep_worker_autopilot(&ctx);
        self.harness_mode_carried(session, lands);
        self.index_autopilot_timer(session, Some(crate::autopilot_timers::ClosedOver::Aterm));
        moved
    }

    fn restore_native_leaf(
        &mut self,
        target: impl Into<crate::app_tabs::NativeInstallTarget>,
        descriptor: &restore::NativeLeafRestore,
    ) -> Result<BuiltRestoreLeaf, String> {
        let target = target.into();
        let wid = target.window();
        let mark = self.restore_staging_mark(wid);
        let mut settings_route = None;
        match descriptor.restore_tag.as_str() {
            "allocation" => {
                self.stage_allocation_tab(target)
                    .map_err(|why| format!("Allocation could not be reopened: {why:?}"))?;
            }
            "mail" => {
                self.stage_mail_tab(target)?;
            }
            "about" => {
                self.stage_about_tab(target)
                    .map_err(|why| format!("About could not be reopened: {why:?}"))?;
            }
            "system-state" => {
                self.stage_system_state_tab(target)
                    .map_err(|why| format!("System State could not be reopened: {why:?}"))?;
            }
            "tables" => {
                let (_, view) =
                    self.stage_tables_tab(target, None, crate::tables_store::FAMILIES)?;
                if let Some((sheet, key)) = descriptor
                    .route
                    .as_deref()
                    .and_then(restore::table_row_route)
                    && let Some(sheet) = crate::tables_store::routed_collection(sheet)
                {
                    self.reveal_table_record(view, sheet, key)?;
                }
            }
            "registry" => {
                let machine = descriptor
                    .route
                    .as_deref()
                    .and_then(restore::table_row_route)
                    .filter(|(sheet, _)| *sheet == "machine")
                    .map(|(_, key)| key.to_owned());
                self.stage_registry_tab(target, None, machine, crate::qos::Role::Background)?;
            }
            "settings" => {
                let route = descriptor
                    .route
                    .as_deref()
                    .and_then(crate::native_settings::SettingsRoute::from_path)
                    .ok_or_else(|| "Settings route is unavailable".to_string())?;
                self.stage_settings_restore_view(target, route)?;
                settings_route = Some(route);
            }
            "markdown" | "editor" => {
                if target.detached() {
                    return Err(
                        "cold document restore requires completed asynchronous admission"
                            .to_string(),
                    );
                }
                let uri = descriptor
                    .uri
                    .as_deref()
                    .ok_or_else(|| "document URI is missing".to_string())?;
                let kind = if descriptor.restore_tag == "markdown" {
                    crate::native_app::AppKind::Markdown
                } else {
                    crate::native_app::AppKind::Editor
                };
                if descriptor.config_editor {
                    self.stage_config_editor_restore_view(wid)?;
                } else {
                    self.stage_document_restore_view(wid, kind, uri)?;
                }
            }
            _ => return Err("This app is unavailable in this build".to_string()),
        }
        self.finish_restore_native_leaf(wid, descriptor, &mark, settings_route)
    }

    /// Detaches the staged leaf and applies its saved view state; a Settings
    /// leaf (`settings_route`) then starts the reads its page shows, unless an
    /// incoming handoff will publish them.
    fn finish_restore_native_leaf(
        &mut self,
        wid: WindowId,
        descriptor: &restore::NativeLeafRestore,
        mark: &RestoreStagingMark,
        settings_route: Option<crate::native_settings::SettingsRoute>,
    ) -> Result<BuiltRestoreLeaf, String> {
        let (view, mut presentation, staging_tab) = self.detach_restore_staging_tab(wid, mark)?;
        if !self.apply_native_view_restore(view, descriptor) {
            self.rollback_restore_document_view(view)?;
            self.remove_view_link(view);
            return Err("The saved view state did not match the reopened app".to_string());
        }
        if self.reopen_carried_settings_drafts(view, descriptor) {
            // The staging presentation was taken before the drafts landed: the
            // tab must show them unsaved (dirty, not closable) from its first
            // frame, as the outgoing tab did.
            if let Some(live) = self.live_view_presentation(view) {
                presentation = live;
            }
        }
        if !self.incoming_handoff_pending {
            use crate::native_settings::SettingsRoute;
            match settings_route {
                Some(SettingsRoute::Harness) => {
                    self.start_harness_stats_refresh();
                    self.publish_native_harness_state();
                }
                Some(SettingsRoute::Lab) => self.publish_native_harness_state(),
                Some(SettingsRoute::Machine) => {
                    self.execute_machine_page(crate::native_app::MachineRequest::Read);
                }
                _ => {}
            }
        }
        Ok((view, presentation, Some(staging_tab)))
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeUpdateSettingsDraftCarry",
            action = "Restore",
            project = "aterm_gui::settings_draft_carry_conformance::Rig::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeUpdateSettingsDraftCarry",
            action = "RestoreUnreadable",
            project = "aterm_gui::settings_draft_carry_conformance::Rig::project"
        )
    )]
    fn reopen_carried_settings_drafts(
        &mut self,
        view: crate::tab_model::ViewId,
        descriptor: &restore::NativeLeafRestore,
    ) -> bool {
        if descriptor.settings_drafts.is_empty() && descriptor.settings_drafts_unreadable == 0 {
            return false;
        }
        let instance = match self.view_store.get(view).copied() {
            Some(crate::tab_model::View::Native(native))
                if matches!(
                    self.native_runtime.view_state(view),
                    Some(crate::native_app::AppViewState::Settings(_))
                ) =>
            {
                Some(native.instance)
            }
            _ => None,
        };
        if let Some(instance) = instance
            && !descriptor.settings_drafts.is_empty()
            && let Err(error) = self.native_runtime.dispatch(
                instance,
                view,
                crate::native_app::AppEvent::SettingsDraftsCarried(
                    descriptor.settings_drafts.clone(),
                ),
            )
        {
            aterm_log::warn!(
                "update: the reopened Settings view refused its carried drafts: {error:?}"
            );
        }
        let landed =
            |draft: &restore::SettingsDraftRestore| match self.native_runtime.view_state(view) {
                Some(crate::native_app::AppViewState::Settings(state)) => state
                    .field_inputs
                    .get(&draft.key)
                    .is_some_and(|input| input.value() == draft.text),
                _ => false,
            };
        let (took, lost): (Vec<_>, Vec<_>) = descriptor
            .settings_drafts
            .iter()
            .cloned()
            .partition(|draft| landed(draft));
        let reopened = took.len();
        if reopened > 0 {
            aterm_log::info!(
                "update: reopened {reopened} unsaved Settings draft(s) carried through the \
                 update; they are still unsaved"
            );
        }
        let ledger = &mut self.carried_settings_drafts;
        ledger
            .landed
            .extend(took.into_iter().map(|draft| (view, draft)));
        ledger.lost.extend(lost);
        ledger.unreadable = ledger
            .unreadable
            .saturating_add(descriptor.settings_drafts_unreadable);
        reopened > 0
    }

    /// Posts a summary notice for any carried Settings drafts that could not be restored.
    pub(crate) fn settle_carried_settings_drafts(&mut self) {
        let ledger = &mut self.carried_settings_drafts;
        let landed = std::mem::take(&mut ledger.landed);
        let mut lost = std::mem::take(&mut ledger.lost);
        let unreadable = std::mem::take(&mut ledger.unreadable);
        for (view, draft) in landed {
            let kept = matches!(
                self.native_runtime.view_state(view),
                Some(crate::native_app::AppViewState::Settings(state))
                    if state
                        .field_inputs
                        .get(&draft.key)
                        .is_some_and(|input| input.value() == draft.text)
            );
            if !kept {
                aterm_log::warn!(
                    "update: the Settings view that reopened the carried draft of `{}` was \
                     closed before the restore finished",
                    draft.key
                );
                lost.push(draft);
            }
        }
        self.report_unreopened_settings_drafts(&lost, unreadable);
    }

    fn report_unreopened_settings_drafts(
        &mut self,
        lost: &[restore::SettingsDraftRestore],
        unreadable: usize,
    ) {
        if lost.is_empty() && unreadable == 0 {
            return;
        }
        if self.message_persist_frozen() {
            let owed = &mut self.carried_settings_drafts.owed;
            owed.lost.extend_from_slice(lost);
            owed.unreadable = owed.unreadable.saturating_add(unreadable);
            aterm_log::info!(
                "update: {} carried Settings draft(s) could not be reopened ({} unreadable); \
                 said and saved if this handoff commits — until then the outgoing process \
                 still holds them",
                lost.len().saturating_add(unreadable),
                unreadable
            );
            return;
        }
        let saved = self
            .messages_log
            .as_ref()
            .and_then(crate::messages_store::Writer::folder)
            .filter(|_| !lost.is_empty())
            .and_then(|folder| match save_lost_settings_drafts(folder, lost) {
                Ok(path) => Some(path),
                Err(error) => {
                    aterm_log::warn!(
                        "update: could not save the text of the carried Settings drafts in \
                         {}: {error}",
                        folder.display()
                    );
                    None
                }
            });
        aterm_log::warn!(
            "update: {} carried Settings draft(s) could not be reopened ({} unreadable); \
             their fields: {:?}; their text: {}",
            lost.len().saturating_add(unreadable),
            unreadable,
            lost.iter()
                .map(|draft| draft.key.as_str())
                .collect::<Vec<_>>(),
            saved.as_deref().map_or_else(
                || "not saved".to_string(),
                |path| path.display().to_string()
            )
        );
        self.post_message(crate::update_words::settings_drafts_not_reopened(
            lost,
            unreadable,
            saved.as_deref(),
        ));
    }

    /// Returns whether carried Settings draft losses are waiting to be announced at Commit.
    #[cfg(all(test, unix))]
    pub(crate) fn carried_settings_drafts_loss_owed(&self) -> bool {
        let owed = &self.carried_settings_drafts.owed;
        !owed.lost.is_empty() || owed.unreadable > 0
    }

    /// Posts the pending carried Settings draft loss notice at successor Commit.
    #[cfg(unix)]
    pub(crate) fn say_carried_settings_drafts_loss(&mut self) {
        let OwedDraftsLoss { lost, unreadable } =
            std::mem::take(&mut self.carried_settings_drafts.owed);
        self.report_unreopened_settings_drafts(&lost, unreadable);
    }

    fn stage_settings_restore_view(
        &mut self,
        target: impl Into<crate::app_tabs::NativeInstallTarget>,
        route: crate::native_settings::SettingsRoute,
    ) -> Result<(), String> {
        let target = target.into();
        let snapshot = self.native_config_service.snapshot();
        let mut state = crate::native_settings::SettingsViewState::from_snapshot(&snapshot)
            .expect("versioned config service snapshots are valid Settings input");
        self.arm_settings_config_file(&mut state);
        state.navigate(route);
        let view_state = crate::native_app::AppViewState::Settings(Box::new(state));
        let presentation = crate::tab_model::TabPresentation {
            title: "Settings".to_string(),
            icon: Some(crate::tab_model::TabIconKind::Settings),
            indicators: crate::tab_model::TabIndicators::default(),
            conn: None,
            closable: true,
            tooltip: Some(format!("Settings · {}", route.label())),
        };
        let install = if let Some(instance) = self
            .native_runtime
            .instance_by_kind(crate::native_app::AppKind::Settings)
        {
            self.install_native_tab(target, instance, view_state, presentation)
                .map(|_| ())
        } else {
            let checking = matches!(
                self.native_updater_service.snapshot().phase,
                crate::native_updater_service::UpdaterPhase::Checking
                    | crate::native_updater_service::UpdaterPhase::Available
                    | crate::native_updater_service::UpdaterPhase::Downloading
            );
            let app = crate::native_app::NativeApp::Settings(Box::new(
                crate::native_settings::SettingsApp::new_at_config_revision(
                    self.update_snapshot(checking),
                    snapshot.revision,
                ),
            ));
            self.install_new_native_tab(target, app, view_state, presentation)
                .map(|_| ())
        };
        install.map_err(|error| {
            aterm_log::warn!("Settings restore view could not be installed: {error:?}");
            "Settings could not be reopened".to_string()
        })
    }

    fn stage_document_restore_view(
        &mut self,
        wid: WindowId,
        kind: crate::native_app::AppKind,
        uri: &str,
    ) -> Result<(), String> {
        self.stage_document_restore_target(wid.into(), kind, uri)
    }

    fn stage_document_restore_target(
        &mut self,
        target: crate::app_tabs::NativeInstallTarget,
        kind: crate::native_app::AppKind,
        uri: &str,
    ) -> Result<(), String> {
        let wid = target.window();
        let read_intent = kind == crate::native_app::AppKind::Markdown;
        if !matches!(
            kind,
            crate::native_app::AppKind::Markdown | crate::native_app::AppKind::Editor
        ) {
            return Err("restore descriptor is not a document app".to_string());
        }
        let requested_kind = kind;
        let kind = crate::native_app::AppKind::Editor;
        let Some(document) = self.document_store.id_for_uri(uri) else {
            if target.detached() {
                return Err("detached document requires asynchronous admission".into());
            }
            return self
                .open_document_tab_in_window_sync(wid, requested_kind, uri)
                .map(|_| ());
        };
        if !self.document_preparation_allows_attach(document) {
            return Err("canonical document preparation is not ready".into());
        }
        let instance = self.native_runtime.instance_for_document(kind, document);
        let (title, markdown) = if let Some(instance) = instance {
            match self.native_runtime.app(instance) {
                Some(crate::native_app::NativeApp::Editor(app)) => {
                    (app.title.clone(), app.markdown.is_some())
                }
                _ => return Err("document restore instance changed kind".into()),
            }
        } else {
            let path = crate::native_document_host::file_uri_path(uri)
                .map_err(|error| error.to_string())?;
            (
                path.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("Document")
                    .to_string(),
                crate::native_app::is_markdown_uri(uri),
            )
        };
        let mut state = crate::native_app::EditorViewState::default();
        if markdown {
            state.markdown = Some(crate::native_app::MarkdownWorkspaceView::new(
                if read_intent {
                    crate::native_app::MarkdownWorkspaceMode::Read
                } else {
                    crate::native_app::MarkdownWorkspaceMode::Edit
                },
            ));
        }
        let state = crate::native_app::AppViewState::Editor(Box::new(state));
        let presentation = crate::tab_model::TabPresentation {
            title: title.clone(),
            icon: Some(kind.icon()),
            indicators: crate::tab_model::TabIndicators::default(),
            conn: None,
            closable: true,
            tooltip: Some(format!("{} · {uri}", kind.as_str())),
        };
        let installed = if let Some(instance) = instance {
            self.install_native_tab_keeping(target, instance, state, presentation, None)
                .map(|(tab, view)| (instance, tab, view))
        } else {
            let mut editor =
                crate::native_app::EditorApp::new_with_uri(document, title, uri.to_owned());
            editor.concealed = self.document_store.concealed(document).cloned();
            editor.read_only = self.document_store.read_only(document);
            self.install_new_native_tab(
                target,
                crate::native_app::NativeApp::Editor(Box::new(editor)),
                state,
                presentation,
            )
        };
        let (instance, tab, view) = installed
            .map_err(|error| format!("document restore view could not be installed: {error:?}"))?;
        if let Err(error) =
            self.attach_document_view_with_visibility(kind, document, view, target.detached())
        {
            self.rollback_document_tab(target, tab);
            return Err(error);
        }
        if let Some(snapshot) = self.document_store.snapshot(document) {
            self.native_runtime.publish_document(
                document,
                snapshot.text.as_ref(),
                self.document_store.revision(document).unwrap_or(0),
                self.document_store.dirty(document).unwrap_or(false),
            );
        }
        if !target.detached() {
            self.refresh_native_presentation(wid, instance, view);
        }
        Ok(())
    }

    fn build_existing_reopen_document(
        &mut self,
        wid: WindowId,
        descriptor: &restore::NativeLeafRestore,
    ) -> Option<Result<BuiltRestoreLeaf, String>> {
        let uri = descriptor.uri.as_deref()?;
        #[cfg(all(target_os = "macos", any(test, debug_assertions)))]
        crate::restore_measurement::closed_reopen_canonical(
            uri,
            self.document_store.id_for_uri(uri).is_some(),
        );
        if descriptor.config_editor {
            return None;
        }
        if let Some(prepared) = self.ready_restore_document(uri) {
            #[cfg(all(target_os = "macos", any(test, debug_assertions)))]
            crate::restore_measurement::closed_reopen_reused(uri);
            let kind = if descriptor.restore_tag == "markdown" {
                crate::native_app::AppKind::Markdown
            } else {
                crate::native_app::AppKind::Editor
            };
            return Some(self.with_detached_restore_leaf(wid, |app| {
                let mark = app.restore_staging_mark(wid);
                app.install_prepared_document(
                    crate::app_tabs::NativeInstallTarget::DetachedRestore(wid),
                    kind,
                    uri,
                    prepared,
                    crate::app_edit::Placement::NewTab,
                )?;
                app.finish_restore_native_leaf(wid, descriptor, &mark, None)
            }));
        }
        if self.document_preparation_pending(uri) {
            return None;
        }
        let document = self.document_store.id_for_uri(uri)?;
        if !self.document_preparation_allows_attach(document) {
            return None;
        }
        let kind = if descriptor.restore_tag == "markdown" {
            crate::native_app::AppKind::Markdown
        } else {
            crate::native_app::AppKind::Editor
        };
        #[cfg(all(target_os = "macos", any(test, debug_assertions)))]
        crate::restore_measurement::closed_reopen_reused(uri);
        Some(self.with_detached_restore_leaf(wid, |app| {
            let mark = app.restore_staging_mark(wid);
            app.stage_document_restore_target(
                crate::app_tabs::NativeInstallTarget::DetachedRestore(wid),
                kind,
                uri,
            )?;
            app.finish_restore_native_leaf(wid, descriptor, &mark, None)
        }))
    }

    fn restore_recovery_leaf(
        &mut self,
        target: impl Into<crate::app_tabs::NativeInstallTarget>,
        placeholder: &restore::PlaceholderLeafRestore,
    ) -> Result<BuiltRestoreLeaf, String> {
        self.restore_recovery_leaf_with_capability(target, placeholder, None)
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeUpdateEditorCarry",
            action = "RestoreFails",
            project = "aterm_gui::editor_carry_conformance::Rig::project"
        )
    )]
    fn restore_recovery_leaf_with_capability(
        &mut self,
        target: impl Into<crate::app_tabs::NativeInstallTarget>,
        placeholder: &restore::PlaceholderLeafRestore,
        capability: Option<crate::native_app::RecoveryCapability>,
    ) -> Result<BuiltRestoreLeaf, String> {
        let target = target.into();
        let wid = target.window();
        let mark = self.restore_staging_mark(wid);
        let presentation = crate::tab_model::TabPresentation {
            title: "Recovery".to_string(),
            icon: Some(crate::tab_model::TabIconKind::Recovery),
            indicators: crate::tab_model::TabIndicators {
                attention: true,
                ..crate::tab_model::TabIndicators::default()
            },
            conn: None,
            closable: true,
            tooltip: Some(format!(
                "{} · {}",
                placeholder.restore_tag, placeholder.reason
            )),
        };
        self.install_new_native_tab(
            target,
            crate::native_app::NativeApp::Recovery(crate::native_app::RecoveryApp {
                restore_tag: placeholder.restore_tag.clone(),
                reason: placeholder.reason.clone(),
                metadata: placeholder.metadata.clone(),
                capability,
                origin: crate::native_app::RecoveryOrigin::Restore,
            }),
            crate::native_app::AppViewState::Recovery(
                crate::native_app::RecoveryViewState::default(),
            ),
            presentation,
        )
        .map_err(|error| format!("Recovery view could not be installed: {error:?}"))?;
        self.detach_restore_staging_tab(wid, &mark)
            .map(|(view, presentation, tab)| (view, presentation, Some(tab)))
    }

    /// What `wid` held before a native restore staged its leaf.
    fn restore_staging_mark(&self, wid: WindowId) -> RestoreStagingMark {
        let window = self.windows.get(&wid);
        RestoreStagingMark {
            active: window.and_then(|window| window.tab_set.active_id()),
            tabs: window
                .map(|window| window.tab_set.tabs().iter().map(|tab| tab.id).collect())
                .unwrap_or_default(),
        }
    }

    /// Lift the single-leaf staging tab a native restore built out of `wid` and make the tab
    /// active before staging (`mark`) active again, so removing it fronts no other tab
    /// (`TabSet::remove` would front the tab before it). A native staging tab never had a
    /// terminal projection slot, so the projection is as it was before staging. An active
    /// tab `wid` already held before staging is the person's, never a staging tab: it is
    /// left in place, the front goes back, and the restore fails.
    fn detach_restore_staging_tab(
        &mut self,
        wid: WindowId,
        mark: &RestoreStagingMark,
    ) -> Result<
        (
            crate::tab_model::ViewId,
            crate::tab_model::TabPresentation,
            crate::tab_model::TabId,
        ),
        String,
    > {
        if let Some(staging) = self
            .restore_leaf_staging
            .as_mut()
            .filter(|staging| staging.wid == wid)
        {
            let tab = staging
                .tab
                .take()
                .ok_or_else(|| "native restore did not prepare a leaf".to_string())?;
            let crate::tab_model::SplitTree::Leaf(view) = tab.root else {
                self.remove_tab_views(&tab);
                return Err("native restore prepared a split instead of a leaf".to_string());
            };
            return Ok((view, tab.presentation, tab.id));
        }
        let tab_id = self
            .windows
            .get(&wid)
            .and_then(|window| window.tab_set.active_id())
            .ok_or_else(|| "native staging tab did not become active".to_string())?;
        let restore_front = |window: &mut WindowState| {
            if let Some(prior) = mark.active {
                window.tab_set.switch_to(prior);
            }
        };
        if mark.tabs.contains(&tab_id) {
            if let Some(mut route_guard_window) = self.windows.get_mut(&wid) {
                let window = &mut **route_guard_window;
                restore_front(window);
            }
            self.resync_after_restore_candidate(wid);
            return Err("the restore fronted an open tab instead of staging one".to_string());
        }
        let tab = self
            .windows
            .get_mut(&wid)
            .as_deref_mut()
            .and_then(|window| {
                let tab = window.tab_set.remove(tab_id)?;
                restore_front(window);
                Some(tab)
            })
            .ok_or_else(|| "native staging tab disappeared".to_string())?;
        let crate::tab_model::SplitTree::Leaf(view) = tab.root else {
            self.remove_tab_views(&tab);
            self.resync_after_restore_candidate(wid);
            return Err("native staging tab was not a single leaf".to_string());
        };
        let staged = (view, tab.presentation, tab.id);
        self.resync_after_restore_candidate(wid);
        Ok(staged)
    }

    fn resync_after_restore_candidate(&mut self, wid: WindowId) {
        let empty = self
            .windows
            .get(&wid)
            .is_none_or(|window| window.tab_set.is_empty());
        if !empty {
            self.resync_active_or_window(wid);
            return;
        }
        self.sync_window(wid);
        if self.frontmost_window == Some(wid) {
            crate::menu::set_active_tab_is_terminal(false);
            let mut active = self
                .active_handle
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *active = None;
        }
    }

    fn restored_focus(
        root: &crate::tab_model::SplitTree<crate::tab_model::ViewId>,
        path: &[restore::RestoreBranch],
    ) -> Option<crate::tab_model::ViewId> {
        let mut node = root;
        for branch in path {
            let crate::tab_model::SplitTree::Split { first, second, .. } = node else {
                return None;
            };
            node = match branch {
                restore::RestoreBranch::First => first,
                restore::RestoreBranch::Second => second,
            };
        }
        match node {
            crate::tab_model::SplitTree::Leaf(view) => Some(*view),
            crate::tab_model::SplitTree::Split { .. } => None,
        }
    }

    /// Rebuilds a split-pane layout tree of terminal leaves inside a window.
    pub(crate) fn restored_terminal_pane_layout(
        &self,
        root: &crate::tab_model::SplitTree<crate::tab_model::ViewId>,
        focus: crate::tab_model::ViewId,
    ) -> Option<restore::PaneLayout> {
        match root {
            crate::tab_model::SplitTree::Leaf(view) => {
                let crate::tab_model::View::Terminal(terminal) =
                    self.view_store.get(*view).copied()?
                else {
                    return None;
                };
                let (cwd, title) = self.restore_session_meta(terminal.session);
                Some(restore::PaneLayout::Leaf {
                    cwd,
                    title,
                    focused: *view == focus,
                    local_id: Some(terminal.session),
                })
            }
            crate::tab_model::SplitTree::Split {
                axis,
                ratio,
                first,
                second,
            } => Some(restore::PaneLayout::Split {
                dir: match axis {
                    crate::tab_model::SplitAxis::Horizontal => restore::SplitKind::Vertical,
                    crate::tab_model::SplitAxis::Vertical => restore::SplitKind::Horizontal,
                },
                ratio: *ratio,
                first: Box::new(self.restored_terminal_pane_layout(first, focus)?),
                second: Box::new(self.restored_terminal_pane_layout(second, focus)?),
            }),
        }
    }

    fn rollback_recursive_restore_leaves(
        &mut self,
        leaves: &[BuiltRestoreLeaf],
        reusable: Option<(crate::tab_model::ViewId, u64)>,
    ) {
        for (view, _, _) in leaves.iter().rev() {
            if reusable.is_some_and(|(kept, _)| kept == *view) {
                continue;
            }
            if let Err(error) = self.rollback_restore_document_view(*view) {
                crate::logging::stderr_line!(
                    "aterm-gui: session restore: could not roll back unpublished view: {error}"
                );
                continue;
            }
            let terminal = self
                .view_store
                .get(*view)
                .copied()
                .and_then(crate::tab_model::View::terminal_session);
            self.remove_view_link(*view);
            if let Some(session) = terminal {
                if self.pool.views(session).is_some_and(|views| views > 1) {
                    // A live session lent this owner one edge. Its other panes and holds stay.
                    self.detach_session_view(session);
                } else {
                    self.teardown_session(session);
                }
            }
        }
    }

    fn bounded_recovery_text(value: &str) -> String {
        value.chars().take(120).collect()
    }

    /// Reconstructs a closed recursive tab inside `wid` without consuming its closed-tab ledger entry.
    #[cfg(test)]
    pub(crate) fn restore_closed_tab_into_window(
        &mut self,
        wid: WindowId,
        record: &restore::RestoredTab,
        canonical_index: usize,
    ) -> Result<crate::tab_model::TabId, String> {
        if !self.windows.contains_key(&wid) {
            return Err("closed-tab target window disappeared".to_string());
        }
        let mut reusable = None;
        let (tab, layout) =
            self.build_recursive_restore_tab(wid, record, &mut reusable, LeafIds::Live)?;
        debug_assert!(reusable.is_none());
        let id = tab.id;
        let terminal = layout.is_some();
        let mut window = self
            .windows
            .get_mut(&wid)
            .ok_or_else(|| "closed-tab target window disappeared".to_string())?;
        if let Some(layout) = layout {
            window.layouts.push(layout);
        }
        window
            .tab_set
            .push(tab)
            .map_err(|_| "closed-tab identity collided".to_string())?;
        let target = window
            .tab_set
            .place(id, canonical_index)
            .unwrap_or(canonical_index);
        if terminal {
            let projection = window
                .tab_set
                .tabs()
                .iter()
                .take(target)
                .filter(|tab| {
                    !tab.root.any_leaf(&mut |view| {
                        !matches!(
                            self.view_store.get(*view),
                            Some(crate::tab_model::View::Terminal(_))
                        )
                    })
                })
                .count();
            let layout = window.layouts.pop().expect("new terminal layout exists");
            window.layouts.insert(projection, layout);
            window.tabs = TabIndex::new(projection, window.layouts.len());
        } else {
            window.tabs.count = window.layouts.len();
        }
        let switched = window.tab_set.switch_to(id);
        debug_assert!(switched);
        // A split's leaves were made at the whole window's grid.
        drop(window);
        self.resize_panes(wid);
        self.resync_active_or_window(wid);
        debug_assert!(
            self.structural_invariants_ok(),
            "{}",
            self.structural_invariant_violation().unwrap_or_default(),
        );
        Ok(id)
    }

    fn remove_restore_bootstrap_terminals(&mut self, wid: WindowId) {
        let mut removed_tabs = Vec::new();
        let mut sessions = Vec::new();
        if let Some(mut route_guard_ws) = self.windows.get_mut(&wid) {
            let ws = &mut **route_guard_ws;
            let tab_ids: Vec<_> = ws
                .tab_set
                .tabs()
                .iter()
                .filter_map(|tab| {
                    matches!(
                        self.view_store.get(tab.focus),
                        Some(crate::tab_model::View::Terminal(_))
                    )
                    .then_some(tab.id)
                })
                .collect();
            for layout in ws.layouts.drain(..) {
                layout.visit_sessions(&mut |session| sessions.push(session));
            }
            ws.tabs = TabIndex::new(0, 0);
            removed_tabs.reserve(tab_ids.len());
            for tab_id in tab_ids {
                if let Some(tab) = ws.tab_set.remove(tab_id) {
                    removed_tabs.push(tab);
                }
            }
        }
        for tab in &removed_tabs {
            self.remove_tab_views(tab);
        }
        for session in sessions {
            self.teardown_session(session);
        }
    }
}

/// `session`, a restore's new shell, stands in for `prev`, whose chain of restores began at
/// `lineage` and first opened at `opened_ms` (`SessionLog::seed_opened`).
fn note_restored(
    session: &crate::Session,
    prev: Option<&str>,
    lineage: Option<&str>,
    opened_ms: Option<u64>,
) -> bool {
    let mut tl = session
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let at = tl.slog().now();
    tl.slog_mut().note_restored(prev, lineage, at);
    tl.slog_mut().seed_opened(opened_ms);
    tl.slog_take_wake()
}

const LOST_SETTINGS_DRAFTS_PREFIX: &str = "settings-drafts-";

const LOST_SETTINGS_DRAFTS_KEPT: usize = 8;

fn save_lost_settings_drafts(
    folder: &std::path::Path,
    lost: &[restore::SettingsDraftRestore],
) -> std::io::Result<std::path::PathBuf> {
    use std::io::Write as _;
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let path = folder.join(format!(
        "{LOST_SETTINGS_DRAFTS_PREFIX}{secs}-{}.txt",
        std::process::id()
    ));
    let mut text = String::from(
        "Unsaved Settings text an aterm update carried across and could not put back.\n\
         Type it again in Settings to keep it.\n",
    );
    for draft in lost {
        text.push_str(&format!("\n[{}]\n{}\n", draft.key, draft.text));
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    options.open(&path)?.write_all(text.as_bytes())?;
    prune_lost_settings_drafts(folder);
    Ok(path)
}

fn prune_lost_settings_drafts(folder: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    let mut saved: Vec<(std::time::SystemTime, std::path::PathBuf)> = entries
        .flatten()
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with(LOST_SETTINGS_DRAFTS_PREFIX) && name.ends_with(".txt")
        })
        .filter_map(|entry| {
            let metadata = entry.metadata().ok().filter(std::fs::Metadata::is_file)?;
            Some((metadata.modified().ok()?, entry.path()))
        })
        .collect();
    saved.sort_unstable_by(|a, b| b.cmp(a));
    for (_, path) in saved.into_iter().skip(LOST_SETTINGS_DRAFTS_KEPT) {
        let _ = std::fs::remove_file(path);
    }
}

const fn restored_row_seen_through(how: &aterm_messages::Retired) -> bool {
    matches!(
        how,
        aterm_messages::Retired::Dismissed
            | aterm_messages::Retired::Answered { .. }
            | aterm_messages::Retired::Resolved(_)
            | aterm_messages::Retired::Folded
    )
}

/// Snapshot of the restored-agents notice last posted by this process.
#[derive(Debug, Default)]
pub(crate) struct RestoredShown {
    seq: u64,
    id: Option<aterm_messages::MessageId>,
    named: Vec<(String, u64)>,
    seen: std::collections::BTreeSet<(String, u64)>,
}

/// `ctx`'s task record names its manager by the sid that manager came back as, where `sids` (old to
/// new, [`App::carry_restored_managers`]) knows it.
fn restored_manager_named(
    ctx: &crate::SessionCtx,
    sids: &std::collections::HashMap<String, String>,
) {
    let mut timeline = ctx
        .timeline
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(mut record) = timeline.task().cloned() else {
        return;
    };
    let Some(now) = sids.get(&record.from) else {
        return;
    };
    record.from.clone_from(now);
    timeline.seed_task(record);
}

/// The document leaf a failed open's Recovery tab stands for ([`crate::native_app::RecoveryOrigin::Open`]),
/// when a relaunch can try the file again through a plain open: a Markdown or Editor file URI,
/// neither a private edit's file (no capture describes one) nor a directory entry its open never
/// resolved (a plain open refuses the entry's link). `None` for any other: such a page says only
/// why one open failed, so a capture leaves it out rather than bring it back as a restore's
/// recovery request.
fn failed_open_leaf(
    recovery: &crate::native_app::RecoveryApp,
) -> Option<restore::NativeLeafRestore> {
    let crate::native_app::RecoveryOrigin::Open { entry: false, .. } = recovery.origin else {
        return None;
    };
    match &recovery.capability {
        Some(crate::native_app::RecoveryCapability::Document {
            kind: kind @ (crate::native_app::AppKind::Markdown | crate::native_app::AppKind::Editor),
            uri,
            config_editor: false,
        }) if !crate::native_document_journal::private_uri(uri) => Some(
            restore::NativeLeafRestore::document(kind.as_str(), uri.clone()),
        ),
        _ => None,
    }
}

#[cfg(test)]
#[path = "app_restore_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "app_restore_drain_tests.rs"]
mod drain_tests;

#[cfg(test)]
#[path = "app_restore_dock_tests.rs"]
mod dock_tests;

#[cfg(test)]
#[path = "app_restore_cursor_tests.rs"]
mod cursor_tests;

#[cfg(test)]
#[path = "app_restore_carried_draft_settle_tests.rs"]
mod carried_draft_settle_tests;

#[cfg(all(test, unix))]
#[path = "app_restore_crash_journal_program_tests.rs"]
pub(crate) mod crash_journal_program_tests;
