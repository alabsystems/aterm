// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! RESTORE-2 wiring: capture the live recursive terminal/native tab tree at quit and
//! rebuild it at the next launch. The persisted format + disk I/O live in
//! [`crate::restore`]; this module is the transaction boundary that mints fresh runtime
//! identities, reconstructs the exact split topology, and substitutes an in-place
//! Recovery view for one unavailable leaf without deleting its healthy siblings.
//!
//! CAPTURE runs at two seams (both feed the one post-loop writer in `main`):
//! a last-window close stashes the manifest into `App::quit_capture` at close-decision
//! time (`close_window_logical` tears the windows down before `el.exit()`), while Cmd-Q
//! exits with `windows` intact and the post-loop writer captures live state directly.
//!
//! APPLY runs once from `resumed`, after the first OS window is up. Window 0's first
//! terminal leaf reuses the already-spawned bootstrap session; later leaves spawn or
//! adopt independently. Native-only windows never fabricate a PTY. The manifest is
//! single-use (deleted on read), so a crash mid-restore cannot loop.

use crate::spawn::spawn_session;
use crate::{App, Session, TabIndex, WindowId, WindowState, pane, restore};
use aterm_window::event_loop::ActiveEventLoop;

type BuiltRestoreTab = (crate::tab_model::Tab, Option<pane::PaneTree>);
/// One agent a restore pass hands the host ([`App::take_restored_agents`]):
/// its tab's sid, its snapshot there, the explicit restart's cause its
/// leaf carries, and the sid of the tab it ran in where that was another
/// (`restore::AgentRestore::tab`). A held tab's agent is never one: it is
/// the holder's ([`App::say_restored_held`]).
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

/// How much of a cold drain one park runs ([`App::run_restore_slice`]) before
/// the loop takes input and presents again: no further window or tab starts
/// once the slice's time plus the last one's would reach it. At least one
/// always lands, so a tab of four panes (four forks) still comes back whole.
pub(crate) const RESTORE_SLICE_BUDGET: std::time::Duration = std::time::Duration::from_millis(4);

/// A COLD LAUNCH'S LAYOUT REBUILD, A SLICE PER PARK (stall audit, 2026-10-15).
/// The whole pass forks every restored tab's shell in one turn right after
/// window 0's first present — about 3 ms a fork, 50-60 ms for a 20-tab desk —
/// while that window shows its shell and cannot take a key. The drain runs
/// the same rebuild in turns of at most [`RESTORE_SLICE_BUDGET`]: window 0 is
/// begun at once (its bootstrap tab, which forks nothing, then its front tab),
/// then a tab or a window per slice ([`App::restore_window`]).
///
/// It owns what the rebuild still owes, and nothing else: every tab it has
/// built is a live tab of its window, so a tab a person closes stays closed
/// and one they open is kept beside the restored ones. While it is under way
/// the rebuild counts as queued (`App::restore_pass_queued`): the crash journal
/// captures nothing, an update waits, and a quit's layout carries the owed
/// windows and tabs ([`Self::carry_owed`]). Its last slice does what follows
/// a whole pass, once.
pub(crate) struct RestoreDrain {
    /// The manifest windows not begun yet, in manifest order.
    windows: std::collections::VecDeque<restore::WindowLayout>,
    /// The first manifest window is still to come: it maps onto the bootstrap
    /// window.
    first: bool,
    /// The window whose tabs are landing, with the ones still owed.
    filling: Option<FillingWindow>,
    placed: RestoredWindows,
    fold: restore::LayoutFold,
    began: std::time::Instant,
    slices: u32,
    longest: std::time::Duration,
}

impl RestoreDrain {
    pub(crate) fn new(manifest: Option<restore::RestoreManifest>) -> Self {
        let (windows, fold) = manifest.map_or_else(Default::default, |manifest| {
            (manifest.windows.into(), manifest.fold)
        });
        Self {
            windows,
            first: true,
            filling: None,
            placed: RestoredWindows::default(),
            fold,
            began: std::time::Instant::now(),
            slices: 0,
            longest: std::time::Duration::ZERO,
        }
    }

    /// Put what the drain still owes into a quit's layout `windows` (whose
    /// position `filling_at` the filling window has): that window's owed tabs
    /// between their nearest restored neighbors, and every window not begun
    /// after the rest. `live_tabs` names each captured tab in that window, so
    /// tabs opened, closed or moved meanwhile and its selected tab stay put. The
    /// legacy mirror of a window that gains tabs goes, as the fold's does
    /// (`restore::WindowLayout::forget_legacy_mirror`).
    ///
    /// WHAT IT CARRIES NAMES NO POOL ID (RV-SURV-1): an owed leaf's `local_id`
    /// is the earlier launch's, which this one mints again from 0
    /// (`restore::WindowLayout::forget_pool_ids`), and the fills of a
    /// restart's quit read a leaf's session by it — so such a tab lost the
    /// agent and the owner's hold it carries, or took a live tab's. Never a
    /// handoff's layout, the one that adopts by it (`App::capture_manifest`).
    pub(crate) fn carry_owed(
        &self,
        windows: &mut Vec<restore::WindowLayout>,
        filling_at: Option<usize>,
        live_tabs: &[crate::tab_model::TabId],
    ) {
        if let Some(filling) = &self.filling
            && !filling.queue.is_empty()
            && let Some(window) = filling_at.and_then(|at| windows.get_mut(at))
        {
            let mut owed: Vec<_> = filling.queue.iter().collect();
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
            for (index, tab) in owed {
                let target = restored_tab_position(
                    present.iter().copied(),
                    *index,
                    window.restored_tabs.len(),
                );
                let mut tab = tab.clone();
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
                present.push((*index, target));
            }
            window.forget_legacy_mirror();
        }
        windows.extend(self.windows.iter().cloned().map(|mut window| {
            window.forget_pool_ids();
            window
        }));
    }

    /// The live window whose tabs are still landing.
    pub(crate) fn filling_window(&self) -> Option<WindowId> {
        self.filling.as_ref().map(|filling| filling.wid)
    }
}

/// One window of a [`RestoreDrain`] whose tabs land a slice at a time.
pub(crate) struct FillingWindow {
    wid: WindowId,
    /// The tabs still owed, in build order, each with its manifest index.
    queue: std::collections::VecDeque<(usize, restore::RestoredTab)>,
    /// Each restored tab built so far: its manifest index and live id.
    placed: Vec<(usize, crate::tab_model::TabId)>,
    /// The manifest index of the tab the layout had in front.
    front: usize,
    /// The tab the restore last brought to the front: a person who chose
    /// another since keeps theirs.
    shown: Option<crate::tab_model::TabId>,
}

/// What [`App::restore_window`] records of the windows a rebuild placed.
#[derive(Default)]
pub(crate) struct RestoredWindows {
    /// The show state each manifest window carried, keyed by the live window
    /// it became ([`App::record_carried_window_show`]).
    shown: Vec<(WindowId, restore::WindowShow)>,
    /// The bootstrap window the first manifest window filled.
    bootstrap: Option<WindowId>,
}

/// Where manifest tab `index` belongs among already restored tabs: just
/// after its nearest earlier neighbor, else before its nearest later one.
/// Both the live drain and its quit capture insert by this rule, leaving
/// the existing live tabs in their current relative order.
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

/// Whether every leaf of `tree` is a terminal: a tab the drain can build on
/// its own, since no native view in it can alias one already on glass
/// ([`App::restore_recursive_into_window`] builds those detached).
fn terminal_leaves_only(tree: &restore::RestoredSplitTree) -> bool {
    match tree {
        restore::RestoredSplitTree::Leaf { view } => {
            matches!(view, restore::RestoredView::Terminal(_))
        }
        restore::RestoredSplitTree::Split { first, second, .. } => {
            terminal_leaves_only(first) && terminal_leaves_only(second)
        }
    }
}

/// The unsaved Settings drafts ONE seamless restore carried, from the leaf
/// that reopened (or could not reopen) them to the end of the restore (plan
/// P2-2; round three of the 2026-09 update robustness work).
///
/// WHY NOT SAY EACH LEAF'S LOSS WHERE IT HAPPENS, as the first cut did. Two
/// things were wrong with it. The loss row is keyed one per update
/// ([`crate::update_words::KEY_SETTINGS_DRAFTS`]), so a second leaf's row
/// REPLACED the first on glass, and the first view's lost text survived only
/// in the history. And a draft that LANDED was counted safe at once, though
/// the restore could still close its view afterwards — a native-only window
/// the OS would not attach (`apply_restore_manifest` closes it logically), a
/// tab whose other leaf failed to build (`build_recursive_restore_tab` rolls it
/// back) — and then the text was gone with nothing said. So the restore
/// records here, and [`App::settle_carried_settings_drafts`] reads every landed
/// draft back from its view once the whole restore is done and says what did
/// not survive, once, with the text.
#[derive(Debug, Default)]
pub(crate) struct CarriedDraftsLedger {
    /// Drafts that landed, with the view that took them: lost if that view is
    /// gone, or no longer holds the text, when the restore settles.
    landed: Vec<(crate::tab_model::ViewId, restore::SettingsDraftRestore)>,
    /// Drafts no view took: a row the new build does not edit as text, a
    /// Settings leaf that did not reopen, a layout that could not be placed.
    lost: Vec<restore::SettingsDraftRestore>,
    /// Drafts the parse had to drop: no text to show, but counted.
    unreadable: usize,
    /// THE LOSS A SEAMLESS SUCCESSOR SETTLED BEFORE COMMIT, said and saved AT
    /// Commit ([`App::say_carried_settings_drafts_loss`]; the review of round
    /// eight's lead U61). Its restore pass runs before the proof and the Commit,
    /// while the parent still runs and still holds every one of these drafts in
    /// its own Settings views: a successor stood down then must have written
    /// nothing — no file with the person's text, no row or log line saying it
    /// could not be put back — and each retried attempt would have left one more
    /// copy. Only at Commit is the text this process's to lose.
    owed: OwedDraftsLoss,
}

/// What [`CarriedDraftsLedger::owed`] holds until Commit: the settled loss,
/// as [`App::report_unreopened_settings_drafts`] takes it.
#[derive(Debug, Default)]
struct OwedDraftsLoss {
    lost: Vec<restore::SettingsDraftRestore>,
    unreadable: usize,
}

impl CarriedDraftsLedger {
    /// A ledger that starts with the drafts of a layout the successor could
    /// not place (`IncomingHandoff::unplaced_settings_drafts`): none of them
    /// has a view to land in.
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

/// Whether this platform tracks a window's NORMAL (un-maximized) frame, so a
/// maximized window's capture can persist the frame it restores DOWN to. On
/// Windows and Linux (X11 / Wayland) maximize is a window-manager show state that
/// winit reports and re-applies; the apply side places the normal frame and
/// maximizes LAST, and the window manager then remembers that frame for
/// restore-down. macOS is NOT wired: its "maximize" is NSWindow zoom, which
/// animates. Reading the vendored winit (2026-09-27): `Resized` comes from the
/// content view's frame-change notification and `Moved` from
/// `windowDidResize:`/`windowDidMove:`, so both fire per animation step, and
/// `is_maximized` is `-isZoomed`, which compares the CURRENT frame with the
/// zoomed standard frame — so an intermediate frame of a zoom-in most likely
/// reads not-maximized, and the tracker would record it as the normal frame.
/// What settles it is one windowed run on a Mac (zoom and un-zoom with a trace
/// of `is_maximized` on every `Moved`/`Resized`); if intermediate frames do
/// read false, the tracker needs a settle rule (commit a frame as normal only
/// once no maximize follows it within the animation) before this flips. Until
/// then a macOS window reopens at the frame it was showing, zoomed or not. (The Linux wiring — `note_normal_origin`
/// on `Moved`, `note_normal_grid` at the grid commit — is likewise unrun on a
/// Linux host; the tracker's rule is [`NormalFrame::note_origin`] /
/// [`NormalFrame::note_grid`], tested here.)
pub(crate) const TRACKS_NORMAL_FRAME: bool = !cfg!(target_os = "macos");

/// A window's last NOT-maximized frame: its origin (when the platform reports
/// one — Wayland does not) and its grid. Updated on every `Moved` and every
/// committed grid while the window is not maximized.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NormalFrame {
    pub(crate) origin: Option<(i32, i32)>,
    pub(crate) rows: u16,
    pub(crate) cols: u16,
}

impl NormalFrame {
    /// The window moved to `origin` while showing `maximized`: only a move of
    /// the NOT-maximized window is its normal origin (a maximize moves it to the
    /// monitor corner, which restore-down must not return to).
    pub(crate) fn note_origin(&mut self, maximized: bool, origin: (i32, i32)) {
        if !maximized {
            self.origin = Some(origin);
        }
    }

    /// A grid of `(rows, cols)` was committed while showing `maximized`: only
    /// the NOT-maximized window's grid is its normal one.
    pub(crate) fn note_grid(&mut self, maximized: bool, (rows, cols): (u16, u16)) {
        if !maximized {
            self.rows = rows;
            self.cols = cols;
        }
    }
}

/// What a restore capture persists for one window's frame: `(origin, (rows,
/// cols))`. A window that is showing MAXIMIZED persists its normal frame (the
/// apply side re-maximizes it after placing that frame, so restore-down lands
/// where the user had it); every other window persists what it is showing. A
/// maximized window with no known normal origin persists none, letting the
/// window manager place it, rather than the maximized frame's corner.
#[must_use]
pub(crate) fn captured_frame(
    live_origin: Option<(i32, i32)>,
    live_grid: (u16, u16),
    maximized: Option<bool>,
    normal: NormalFrame,
) -> (Option<(i32, i32)>, (u16, u16)) {
    if maximized == Some(true) {
        (normal.origin, (normal.rows, normal.cols))
    } else {
        (live_origin, live_grid)
    }
}

/// WHICH PROCESS minted the `local_id`s of the descriptor being rebuilt — and so
/// whether one of them may name a live handle. The two sources look identical on
/// the wire (`TerminalLeafRestore` is one type) and only the caller knows which
/// it holds, so the leaf builder is told rather than left to guess.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum LeafIds {
    /// THIS process's closed-tab ledger. Session ids are never reused within a
    /// run, so an id still in the pool IS the session that tab was showing —
    /// a tab closed off a session another view still holds must re-attach to it
    /// (`closed_terminal_tab_reattaches_a_still_live_shared_session`), not fork
    /// a stranger that merely wears the same title.
    Live,
    /// A manifest written by a process that has since EXITED: session restore,
    /// and the layout a seamless update carries across the swap. Those ids
    /// belong to a namespace this launch re-mints from 0, so a pool hit is never
    /// recognition — only a collision with a shell this same pass just forked.
    /// The one live thing such a manifest can name is a handed-off fd, matched
    /// through `seamless_adopt`.
    Retired,
}

/// The session filling a restored terminal leaf's pane, as
/// [`App::carry_restored_identity`] is handed it — which decides how an
/// identity can reach it.
#[derive(Clone, Copy)]
enum FillingShell<'a> {
    /// The window's bootstrap, grafted onto the leaf: already registered and
    /// served under its sid, so an identity arrives through the live door
    /// ([`App::graft_restored_user_meta`]).
    Registered(u64),
    /// A session the leaf just spawned or re-adopted, which nobody can address
    /// until it is registered: seeded directly
    /// ([`App::seed_restored_user_meta`]).
    Unregistered(&'a Session),
}

/// The headless test harness's stand-in for adopting a handed-off shell: a stub
/// session whose sink takes the handed master over through the SAME
/// [`crate::spawn::HandedMaster::into_sink`] a real adoption (`spawn_session`)
/// uses, so the session owns it and closes it when it drops. The `-1` sentinel
/// the fixtures carry is no descriptor, so that stub owns none.
#[cfg(test)]
fn stub_adopted_session(id: u64, master: crate::spawn::HandedMaster) -> crate::Session {
    if master.raw() < 3 {
        return crate::stub_session(id);
    }
    crate::stub_session_with_sink(id, master.into_sink())
}

/// Remove and return the handed-off shell the outgoing process knew as
/// `local_id`, when it is still waiting to be placed. Every adopt site matches
/// through this — `main_entry` for session 0, `apply_restore_manifest` for each
/// further window's bootstrap, the leaf builders for every other pane — and
/// each match removes the shell, so none is adopted twice. A window or pane
/// whose shell is already taken gets `None` here and spawns a fresh shell in
/// its place.
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

/// The handed-off shell `main_entry` adopts as SESSION 0, removed from
/// `handed_off`, or `None` for a fresh session 0. Session 0 is window 0's
/// bootstrap, and the deferred restore grafts it onto the leaf
/// [`restore::WindowLayout::bootstrap_local_id`] names, so it adopts the shell
/// that leaf names.
///
/// A window 0 with NO terminal leaf (every tab a native view) names no shell,
/// and none belongs in it: the handoff layout names every handed-off shell
/// exactly once (`covers_exact_seamless_ids`), so each one's pane lies in a
/// later window, which adopts it there. Session 0 is then a FRESH bootstrap,
/// which window 0's native-only rebuild retires exactly as a cold restore
/// retires it. Adopting a shell for it instead — the first one listed, as this
/// pick once did — kept that shell in window 0 as an extra tab and put a fresh
/// stand-in in its own pane.
///
/// With no layout to place by (a headless boot keeps none), the first shell;
/// the orphan net places the rest.
pub(crate) fn take_session0_shell(
    handed_off: &mut Vec<crate::spawn::Adopted>,
    layout: Option<&restore::RestoreManifest>,
) -> Option<crate::spawn::Adopted> {
    match layout.and_then(|layout| layout.windows.first()) {
        Some(window0) => take_handed_off_shell(handed_off, window0.bootstrap_local_id()),
        None => (!handed_off.is_empty()).then(|| handed_off.remove(0)),
    }
}

/// Whether this process's layout is its own to journal: the first frame is on
/// glass, no previous layout is still waiting to be rebuilt, and no update
/// handoff is waiting for its Commit.
pub(crate) fn crash_journal_writable(
    first_present_done: bool,
    restore_pending: bool,
    handoff_pending: bool,
) -> bool {
    first_present_done && !restore_pending && !handoff_pending
}

/// What the journal asked the process table about a session's root.
#[derive(Debug)]
pub(crate) enum RootAnswer {
    /// Not asked: a platform whose job test already covers the root's jobs.
    #[cfg_attr(unix, allow(dead_code))]
    Unasked,
    /// Asked: the answer, or `None` when the table could not give one.
    #[cfg_attr(not(unix), allow(dead_code))]
    Read(Option<crate::quit_safety::SessionRoot>),
}

/// THE JOURNAL'S TEST FOR ONE LEAF (D16, rulings 282 and 292): whether its
/// session was running a program that aterm's death would take down, and its
/// name (`Some("")` when unnamed). `group` is the PTY's foreground group
/// (`tcgetpgrp`), `root_pid` the session's root, `named` the status sweep's
/// resolved program and the group it resolved it for. A program ran when:
///
/// * the foreground is a JOB (the quit confirm's test) — named when the sweep
///   resolved that same group;
/// * the root is not a shell (`aterm -e claude`, `exec vim`, a `shell=` that
///   is not one) — named when the sweep resolved the root's group to a
///   non-shell (a name resolved before an `exec` still says the shell);
/// * the root is a shell with a child: a background or suspended job, which
///   the shell hangs up when the PTY closes — unnamed;
/// * the table could not answer — unnamed, because a warning that was not
///   needed costs less than a loss that was never mentioned.
///
/// A shell with no child, holding its own foreground, ran nothing.
pub(crate) fn leaf_program(
    group: i32,
    root_pid: i32,
    root: &RootAnswer,
    named: Option<(i32, &str)>,
) -> Option<String> {
    let name_for = |pgid: i32| {
        named
            .filter(|(at, _)| pgid > 0 && *at == pgid)
            .map(|(_, name)| name.to_string())
    };
    if crate::quit_safety::foreground_is_job(group, root_pid) {
        return Some(name_for(group).unwrap_or_default());
    }
    match root {
        RootAnswer::Unasked => None,
        RootAnswer::Read(None) => Some(String::new()),
        RootAnswer::Read(Some(root)) if !crate::quit_safety::is_shell_name(&root.comm) => Some(
            name_for(root_pid)
                .filter(|name| !crate::quit_safety::is_shell_name(name))
                .unwrap_or_default(),
        ),
        RootAnswer::Read(Some(root)) => (!root.children.is_empty()).then(String::new),
    }
}

/// Mint recovery authority only from the already-validated typed descriptor.
/// Its copyable metadata is deliberately never parsed to recover a path/route.
fn recovery_capability(
    native: &restore::NativeLeafRestore,
) -> Option<crate::native_app::RecoveryCapability> {
    match native.restore_tag.as_str() {
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
    /// Snapshot every window's terminal layouts plus stable native descriptors. Native
    /// titles remain derived data, and process-local tab/view/instance identities,
    /// document bytes, search text, and async generations never enter the persisted
    /// projection. Bounded source-addressed selections and viewport anchors do: they
    /// are view state, validated and clamped against the reopened document on apply.
    /// Windows serialize in `WindowId` order (BTreeMap iteration): stable and
    /// deterministic. Which window was in front and key is carried separately,
    /// as each window's macOS show state (`restore::WindowShow`, gap #29), and
    /// restored as a stack after the windows exist (`crate::window_show`) —
    /// never by reordering the windows, which the layout digest and the Commit
    /// comparison are taken over.
    ///
    /// This is the DURABLE capture (quit, crash journal): it carries no unsaved
    /// Settings text. It does carry each window's raw inner size
    /// ([`restore::WindowLayout::inner_px`]) — not a maximized or full-screen
    /// window's — so a cold restore brings every further window back at its
    /// own grid, and at that size when it still holds the grid. The seamless
    /// update's layout is [`Self::capture_handoff_layout`].
    pub(crate) fn capture_restore_manifest(&self) -> restore::RestoreManifest {
        self.capture_manifest(false)
    }

    /// The seamless update's attempt-bound layout: [`Self::capture_restore_manifest`]
    /// plus every Settings view's carriable unsaved field drafts
    /// ([`restore::NativeLeafRestore::settings_drafts`]), so the successor reopens
    /// each view holding exactly what the person typed, still unsaved (plan P2-2),
    /// and a full-screen window's raw inner size too ([`restore::WindowLayout::inner_px`]),
    /// so it recreates every window at the grid its adopted shells have and the
    /// size it had, not only its grid's frame.
    ///
    /// ONE capture for the park and the Commit. The park's layout is what the
    /// successor restores; the Commit re-captures with this same function and
    /// compares Settings leaves in full (`commit_layout_topology`), so a key
    /// typed into a Settings field while the successor booted refuses the
    /// Commit and the outgoing process keeps the newer draft — never a successor
    /// that reopens the older one. Carried here and nowhere durable: the layout
    /// is a one-shot, owner-only sidecar the successor consumes, while a quit
    /// manifest or crash journal outlives the process that promised nothing.
    #[cfg(unix)]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeUpdateSettingsDraftCarry",
            action = "Park",
            project = "aterm_gui::settings_draft_carry_conformance::Rig::project"
        )
    )]
    pub(crate) fn capture_handoff_layout(&self) -> restore::RestoreManifest {
        self.capture_manifest(true)
    }

    /// `handoff` is the seamless update's capture ([`Self::capture_handoff_layout`]):
    /// it alone carries the Settings drafts, each window's presence row and a
    /// full-screen window's raw inner size ([`restore::WindowLayout::inner_px`]).
    fn capture_manifest(&self, handoff: bool) -> restore::RestoreManifest {
        let filling_window = self
            .restore_drain
            .as_deref()
            .and_then(RestoreDrain::filling_window);
        let mut filling_tabs = Vec::new();
        // The stack, from the focus MRU the App already keeps — no AppKit query
        // (`window_show::stacking_ranks`). Used by the macOS capture only.
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
                // A window system that went away answers nothing (an X11
                // window's position is a round trip on the dead connection,
                // which fails since X11-GONE), nor is one asked after any
                // other failure of the event loop: its windows are captured
                // from this App's own state alone, with no origin, show
                // state, maximized bit or full-screen state.
                let os_window = ws.os_window.as_ref().filter(|_| !self.event_loop_failed);
                // W3 (Windows): show state + frame origin, read TOGETHER from
                // `GetWindowPlacement` when the window is maximized. winit's
                // `outer_position()` reports the MAXIMIZED frame's origin (the
                // monitor corner minus the invisible resize border) — persisting
                // that and re-maximizing on restore would plant the eventual
                // restore-down frame at the monitor corner instead of where the
                // user had it. `rcNormalPosition` is the rect the OS itself would
                // restore down to, so that is what a maximized capture persists.
                // An UN-maximized window keeps the winit read verbatim (it is
                // exact there, and byte-identical to the pre-W3 capture).
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
                        // Placement unreadable (no HWND yet, or the call failed):
                        // degrade to exactly the old capture — position only.
                        None => (pos, None),
                    }
                };
                // Off Windows: the show state is winit's, and a maximized
                // window's origin is its tracked NORMAL origin
                // (`captured_frame`). macOS captures no maximized bit by design
                // (`TRACKS_NORMAL_FRAME`); its full-screen, minimized and stack
                // state is the `show` capture below. The Unix seamless commit's topology
                // equality normalizes the bit (`commit_layout_topology`), so a
                // zoom during a handoff never reads as a structural change.
                #[cfg(not(windows))]
                let (pos, maximized) = {
                    let live = os_window
                        .and_then(|w| w.outer_position().ok())
                        .map(|p| (p.x, p.y));
                    let maximized = if TRACKS_NORMAL_FRAME {
                        os_window.map(|w| w.is_maximized())
                    } else {
                        None
                    };
                    let (origin, _) =
                        captured_frame(live, (ws.rows, ws.cols), maximized, ws.normal_frame);
                    (
                        origin.map(|(x, y)| aterm_window::dpi::PhysicalPosition::new(x, y)),
                        maximized,
                    )
                };
                // The GRID half, on every platform: a maximized window persists
                // its normal grid, so restore-down is the size the user had.
                let (rows, cols) =
                    captured_frame(None, (ws.rows, ws.cols), maximized, ws.normal_frame).1;
                // THE macOS SHOW STATE (gap #29): full screen and minimized off
                // the window itself — winit's full-screen state, and
                // `isMiniaturized` through this window's own handle, reached
                // only when an OS window exists (never headless) — and its place
                // in the stack and whether it is the key window off the App's
                // own focus bookkeeping. A window whose carried full-screen
                // re-entry is still waiting (aterm has not been in front since
                // the last update) is recorded as full screen, so a second
                // update carries it on. Normalized out of the Commit
                // comparison like the position (`commit_layout_topology`).
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
                // Everywhere else no show state is captured — Windows and Linux
                // carry only the maximized bit above — so a restore changes
                // nothing here.
                #[cfg(not(target_os = "macos"))]
                let show = {
                    let _ = (index, wid);
                    restore::WindowShow::UNKNOWN
                };
                let terminal_tabs = ws
                    .layouts
                    .iter()
                    .map(|tree| tree.to_layout(&|id| self.restore_session_meta(id)))
                    .collect::<Vec<_>>();
                let mut native_tabs = Vec::new();
                let mut tab_order = Vec::with_capacity(ws.tab_set.len());
                // `restored_tabs` is the authoritative recursive projection. Track
                // its active position while appending descriptors: the legacy
                // `tab_order` mirror below intentionally cannot represent a
                // heterogeneous split tab, so deriving `active_item` from that
                // compressed list silently selected the wrong tab on restore.
                let mut restored_tabs = Vec::with_capacity(ws.tab_set.len());
                let mut active_item = None;
                for tab in ws.tab_set.tabs() {
                    let Some(restored) = self.tab_restore_descriptor_carrying(tab, handoff) else {
                        continue;
                    };
                    if ws.tab_set.active_id() == Some(tab.id) {
                        active_item = Some(restored_tabs.len());
                    }
                    if filling_window == Some(wid) {
                        filling_tabs.push(tab.id);
                    }
                    restored_tabs.push(restored);
                }
                let mut terminal_index = 0usize;
                for tab in ws.tab_set.tabs() {
                    let entry = if tab.root.leaves().into_iter().all(|view| {
                        matches!(
                            self.view_store.get(view),
                            Some(crate::tab_model::View::Terminal(_))
                        )
                    }) {
                        let entry = restore::TabOrderEntry::Terminal {
                            index: terminal_index,
                        };
                        terminal_index = terminal_index.saturating_add(1);
                        Some(entry)
                    } else {
                        // A second Settings tab, or a second copy of an equal
                        // descriptor (the same document), stays out of the
                        // mirror as a mixed split tab does: every build's parse
                        // refuses a window whose mirror holds one, and such a
                        // window exists once an update folded a window past the
                        // cap into another (`NativeTabRestore::joins_mirror`).
                        // `restored_tabs` above keeps the tab whole.
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
                // THE RAW INNER SIZE, for the next process to recreate this
                // window at: an update's successor (P6 finding 4) and a cold
                // restore alike. Never a maximized window's: its row persists
                // the NORMAL grid, which that size does not hold. Nor, in the
                // durable capture, a full-screen window's: that size is the
                // screen's, and a cold restore brings the window back as it
                // always has (the macOS show state re-enters full screen). An
                // update carries it, since its successor must recreate the
                // window at the grid its adopted shells have.
                let fullscreen = show.fullscreen == Some(true)
                    || os_window.is_some_and(|w| w.fullscreen().is_some());
                let inner_px = ws
                    .win_px
                    .filter(|size| {
                        maximized != Some(true)
                            && (handoff || !fullscreen)
                            && size.width > 0
                            && size.height > 0
                    })
                    .map(|size| [size.width, size.height]);
                restore::WindowLayout {
                    rows,
                    cols,
                    active_tab: ws.tabs.active,
                    outer_x: pos.map(|p| p.x),
                    outer_y: pos.map(|p| p.y),
                    maximized,
                    inner_px,
                    // Presence owns no row: nothing for a successor to
                    // reserve (the field stays on the wire for an older
                    // reader, always unset).
                    presence_row: false,
                    show,
                    tabs: terminal_tabs,
                    native_tabs,
                    tab_order,
                    active_item,
                    restored_tabs,
                }
            })
            .collect();
        // A COLD DRAIN UNDER WAY still owes part of the previous layout
        // ([`RestoreDrain`]): a quit or crash capture keeps it, so a quit in the
        // first instants loses no tab. Never an update's layout, whose leaves
        // name handed shells — and none is taken while a drain runs.
        if !handoff && let Some(drain) = self.restore_drain.as_deref() {
            let filling_at = drain
                .filling_window()
                .and_then(|wid| self.windows.keys().position(|live| *live == wid));
            drain.carry_owed(&mut windows, filling_at, &filling_tabs);
        }
        // THE MANIFEST-WIDE SANITIZE, before anyone hashes this capture:
        // `RestoreManifest::new` runs `WindowLayout::sanitize`, the same pass
        // `from_toml` runs, so the capture is a fixed point of the parse. The
        // update worker checks exactly that (`from_toml(to_toml(layout)) ==
        // layout`), and `layout_digest` is taken over this value — a NUL
        // directory a program reported (OSC 7 `%00`) used to break the round
        // trip and file a Structural failure (the 2026-09-22/23 update audit,
        // plan P0-1f). Now the leaf just loses the directory.
        let mut manifest = restore::RestoreManifest::new(windows);
        // THE CAPS, for the update's layout (round eight of the update audit,
        // D5): a desk past 64 windows or a window past 256 tabs is folded into
        // what one layout holds, so the round trip above holds and every
        // handed shell is named. A desk within the caps is left byte-identical.
        // The durable capture is not folded: a quit layout or crash journal
        // past the caps reopens the windows that fit (`RestoreManifest::from_toml`).
        if handoff {
            manifest.fold_into_caps();
        }
        manifest
    }

    /// Legacy stable descriptor for one currently-live, single-view native tab. The
    /// recursive v2 projection is [`Self::tab_restore_descriptor`]; this compatibility
    /// mirror intentionally refuses heterogeneous trees instead of flattening them.
    pub(crate) fn native_restore_descriptor(
        &self,
        tab: &crate::tab_model::Tab,
    ) -> Option<restore::NativeTabRestore> {
        let leaves = tab.root.leaves();
        if leaves.len() != 1 || tab.focus != leaves[0] {
            return None;
        }
        let view = leaves[0];
        let crate::tab_model::View::Native(native) = self.view_store.get(view).copied()? else {
            return None;
        };
        let app = self.native_runtime.app(native.instance)?;
        match app.kind() {
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
            crate::native_app::AppKind::Markdown | crate::native_app::AppKind::Editor => {
                let document = app.document_id()?;
                let uri = self.document_store.canonical_uri(document)?.to_string();
                Some(match app.kind() {
                    crate::native_app::AppKind::Markdown => {
                        restore::NativeTabRestore::Markdown { uri }
                    }
                    crate::native_app::AppKind::Editor => restore::NativeTabRestore::Editor { uri },
                    crate::native_app::AppKind::Settings
                    | crate::native_app::AppKind::SystemState => unreachable!(),
                    crate::native_app::AppKind::Recovery => unreachable!(),
                })
            }
            crate::native_app::AppKind::Recovery => None,
        }
    }

    /// Recursive RESTORE-2 projection for any terminal/native split tab. Every leaf is
    /// captured independently, so one later-unavailable app can become a placeholder
    /// without deleting a healthy terminal or document sibling.
    pub(crate) fn tab_restore_descriptor(
        &self,
        tab: &crate::tab_model::Tab,
    ) -> Option<restore::RestoredTab> {
        self.tab_restore_descriptor_carrying(tab, false)
    }

    /// [`Self::tab_restore_descriptor`], carrying each Settings leaf's unsaved
    /// drafts when `carry_drafts` (the handoff layout only).
    fn tab_restore_descriptor_carrying(
        &self,
        tab: &crate::tab_model::Tab,
        carry_drafts: bool,
    ) -> Option<restore::RestoredTab> {
        fn capture(
            app: &App,
            node: &crate::tab_model::SplitTree<crate::tab_model::ViewId>,
            focus: crate::tab_model::ViewId,
            carry_drafts: bool,
            path: &mut Vec<restore::RestoreBranch>,
            focused_path: &mut Option<Vec<restore::RestoreBranch>>,
        ) -> Option<restore::RestoredSplitTree> {
            match node {
                crate::tab_model::SplitTree::Leaf(view) => {
                    if *view == focus {
                        *focused_path = Some(path.clone());
                    }
                    Some(restore::RestoredSplitTree::leaf(
                        app.view_restore_descriptor_carrying(*view, carry_drafts)?,
                    ))
                }
                crate::tab_model::SplitTree::Split {
                    axis,
                    ratio,
                    first,
                    second,
                } => {
                    path.push(restore::RestoreBranch::First);
                    let first = capture(app, first, focus, carry_drafts, path, focused_path)?;
                    path.pop();
                    path.push(restore::RestoreBranch::Second);
                    let second = capture(app, second, focus, carry_drafts, path, focused_path)?;
                    path.pop();
                    Some(restore::RestoredSplitTree::Split {
                        axis: match axis {
                            crate::tab_model::SplitAxis::Horizontal => {
                                restore::SplitKind::Horizontal
                            }
                            crate::tab_model::SplitAxis::Vertical => restore::SplitKind::Vertical,
                        },
                        ratio: *ratio,
                        first: Box::new(first),
                        second: Box::new(second),
                    })
                }
            }
        }

        let mut focused_path = None;
        let root = capture(
            self,
            &tab.root,
            tab.focus,
            carry_drafts,
            &mut Vec::new(),
            &mut focused_path,
        )?;
        Some(restore::RestoredTab {
            root,
            focused_path: focused_path?,
            zoomed: tab.zoomed,
        })
    }

    pub(crate) fn view_restore_descriptor(
        &self,
        view: crate::tab_model::ViewId,
    ) -> Option<restore::RestoredView> {
        self.view_restore_descriptor_carrying(view, false)
    }

    /// [`Self::view_restore_descriptor`], carrying a Settings view's unsaved
    /// drafts when `carry_drafts` (the handoff layout only).
    fn view_restore_descriptor_carrying(
        &self,
        view: crate::tab_model::ViewId,
        carry_drafts: bool,
    ) -> Option<restore::RestoredView> {
        match self.view_store.get(view).copied()? {
            crate::tab_model::View::Terminal(terminal) => {
                let (cwd, title) = self.restore_session_meta(terminal.session);
                // USER metadata (session-metadata stage 1): capture the
                // operator's `meta set` fields off the session ctx (a leaf
                // lock; the quit-time capture path holds no other lock here)
                // so the manifest re-seeds them on the respawned session.
                let user_meta = self
                    .pool
                    .get(terminal.session)
                    .map(|s| {
                        s.ctx
                            .meta
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .sanitized()
                    })
                    .unwrap_or_default();
                // THE AGENT A COLD RESTORE STILL OWES THIS TAB (round four,
                // plan item 7), on the handoff layout only: the successor
                // relaunches it at its Commit. A handoff leaf never carried
                // an agent before, so the successor reads `Some` as exactly
                // that (`carry_restored_identity`).
                let owed = if carry_drafts {
                    self.pending_restored_agent(terminal.session, true)
                } else {
                    None
                };
                let (sid, lineage, log_ep) = self
                    .pool
                    .get(terminal.session)
                    .map(|s| {
                        let (lineage, log_ep) = {
                            let tl = s.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
                            (tl.slog().lineage(), tl.slog().log().ep())
                        };
                        let sid = s.ctx.self_id.as_str();
                        (
                            Some(sid.to_string()).filter(|sid| restore::carried_sid(sid)),
                            lineage
                                .map(|l| l.as_str().to_string())
                                .filter(|l| l != sid && restore::carried_sid(l)),
                            Some(log_ep),
                        )
                    })
                    .unwrap_or_default();
                Some(restore::RestoredView::Terminal(
                    restore::TerminalLeafRestore {
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
                        // Its timer, beside the word it goes with (`sanitized`
                        // keeps it under `cruise` only).
                        land_at: user_meta.land_at,
                        // Who set the word, beside it (§11.7.3, item 5).
                        autopilot_by: user_meta.autopilot_set_by.map(|set_by| set_by.carried()),
                        // Its own theme, excluding a Choose Tab Theme… highlight
                        // not yet kept, so it comes back.
                        theme: self
                            .kept_session_theme(terminal.session)
                            .map(|theme| theme.selector.clone()),
                        agent: owed.clone(),
                        // THE AGENT ITS HOST SUPERVISES HERE (round
                        // eighteen), on the handoff layout only and only
                        // where nothing is owed: the successor watches it from
                        // its Commit (`hand_carried_restored_agents`), so an
                        // exit this process reads after the layout is
                        // captured — and will never act on — is the
                        // successor's.
                        supervised: if carry_drafts && owed.is_none() {
                            self.hosted_agent(terminal.session).map(Box::new)
                        } else {
                            None
                        },
                        // THE HOST'S WORD THAT THE TAB'S AGENT CANNOT BE
                        // BROUGHT BACK (LC-4 of ATERM-KI-1016-1), on the
                        // handoff layout only: the successor's host stands
                        // it again at its Commit (`adopt_carried_lost_words`).
                        lost: if carry_drafts {
                            self.hosted_lost_word(terminal.session).map(Box::new)
                        } else {
                            None
                        },
                        // Durable captures only (see the field): the
                        // handoff's Commit compares layouts, and a program
                        // may start or end in between.
                        held: !carry_drafts && self.restore_session_held(terminal.session),
                        // THE SESSION IT WAS AND ITS LINEAGE (session log
                        // phase 2A), and the epoch its log is at (phase 6,
                        // 6A-2), constant for a session, so the Commit's
                        // comparison is unmoved by them.
                        sid,
                        lineage,
                        log_ep,
                        // The tab's holds ride the layouts an end nobody
                        // chose leaves, with its agent
                        // ([`Self::fill_desk_agents`]); a handoff's session
                        // record carries them.
                        holds: Vec::new(),
                    },
                ))
            }
            crate::tab_model::View::Native(native) => {
                let app = self.native_runtime.app(native.instance)?;
                let state = self.native_runtime.view_state(view)?;
                if let (
                    crate::native_app::NativeApp::Recovery(recovery),
                    crate::native_app::AppViewState::Recovery(_),
                ) = (app, state)
                {
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
                        crate::native_app::AppKind::SystemState,
                        crate::native_app::AppViewState::SystemState(_),
                    ) => restore::NativeLeafRestore::system_state(),
                    (
                        crate::native_app::AppKind::Settings,
                        crate::native_app::AppViewState::Settings(settings),
                    ) => restore::NativeLeafRestore::settings(settings.route.path().to_string()),
                    (
                        crate::native_app::AppKind::Markdown,
                        crate::native_app::AppViewState::Markdown(_),
                    ) => restore::NativeLeafRestore::document(
                        "markdown",
                        self.document_store
                            .canonical_uri(app.document_id()?)?
                            .to_string(),
                    ),
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
                    crate::native_app::AppViewState::SystemState(_) => {}
                    crate::native_app::AppViewState::Settings(settings) => {
                        if carry_drafts {
                            descriptor.settings_drafts = settings.carried_field_drafts().carried;
                        }
                    }
                    crate::native_app::AppViewState::Markdown(markdown) => {
                        descriptor.source_anchor = markdown.source_anchor;
                        descriptor.viewport_anchor = markdown.visual_row;
                        descriptor.selection = markdown.selection.as_ref().map(|selection| {
                            restore::RestoreSelection {
                                anchor: selection.start,
                                head: selection.end,
                            }
                        });
                    }
                    crate::native_app::AppViewState::Editor(editor) => {
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

    /// Reapply bounded view-local state after a native descriptor has minted fresh live
    /// identities. Every byte offset is clamped to the current canonical UTF-8 snapshot;
    /// stale restore coordinates cannot become unchecked slice indices.
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
        if app.vtable().restore_tag != restore.restore_tag {
            return false;
        }
        let document_text = app
            .document_id()
            .and_then(|document| self.document_store.snapshot(document))
            .map(|snapshot| snapshot.text);
        let Some(state) = self.native_runtime.view_state_mut(view) else {
            return false;
        };
        match state {
            crate::native_app::AppViewState::Settings(_)
            | crate::native_app::AppViewState::SystemState(_) => true,
            crate::native_app::AppViewState::Markdown(markdown) => {
                let Some(text) = document_text.as_deref() else {
                    return false;
                };
                markdown.source_anchor = clamp_boundary(text, restore.source_anchor);
                markdown.visual_row = restore.viewport_anchor;
                markdown.selection = restore.selection.map(|selection| {
                    let anchor = clamp_boundary(text, selection.anchor);
                    let head = clamp_boundary(text, selection.head);
                    anchor.min(head)..anchor.max(head)
                });
                markdown
                    .history
                    .visit(crate::native_markdown::MarkdownLocation::new(
                        markdown.source_anchor,
                        markdown.visual_row,
                    ));
                true
            }
            crate::native_app::AppViewState::Editor(editor) => {
                let (Some(text), Some(buffer)) = (document_text.as_deref(), editor.buffer.as_mut())
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
                buffer.viewport_anchor = clamp_boundary(text, restore.viewport_anchor);
                true
            }
            crate::native_app::AppViewState::Recovery(_) => false,
        }
    }

    /// The restored agent the supervisor host's cold-restore queue still owes
    /// pool session `id` ([`crate::harness_host::HostHandle::pending_restored_for`]:
    /// read live, and none where an agent runs again), in the form a handoff
    /// leaf — and the layouts that keep the desk's agents
    /// ([`Self::fill_desk_agents`]) — carries it: `None` with no host, or when
    /// the tab owes nothing.
    ///
    /// THE EXPLICIT RESTART'S CAUSE RIDES THE `handoff` LAYOUT ALONE (round
    /// eighteen, the review of its first cut; `restore::AgentRestore::cause`):
    /// its successor adopts the very tab, its sid kept, and relaunches the
    /// agent as the act's. A durable layout's agent is relaunched by a cold
    /// restore in a REOPENED tab, a new sid, as aterm's own end
    /// (`relaunch::after_host_ended`, which closes a relaunch the act left in
    /// flight for the old tab): carried there, the cause made it the act's
    /// (`relaunch::after_exit`), which reads that record as the
    /// conversation's in another tab — a counted wait, eight of them, and the
    /// tab said not to have come back.
    fn pending_restored_agent(&self, id: u64, handoff: bool) -> Option<Box<restore::AgentRestore>> {
        let host = self.harness.as_ref()?;
        let sid = {
            let g = self.store.read().unwrap_or_else(|p| p.into_inner());
            g.by_local(id)?.sid.as_str().to_string()
        };
        let owed = host.pending_restored_for(&sid)?;
        // The one conversion a restore leaf takes (a Codex's run rides it
        // too), the explicit restart's cause with it on a handoff alone.
        let asked = owed.asked.filter(|_| handoff);
        Some(Box::new(restore::AgentRestore::owed(owed.snap, asked)))
    }

    /// The agent pool session `id` hosts, as the supervisor host last read it
    /// and its conversation, build and directory as its own record names
    /// them now ([`crate::harness_host::followed_snapshot`]: an in-app
    /// `/clear` or `/resume` the host has not followed yet is read here, LC-1),
    /// in the form a restore leaf carries ([`Self::fill_desk_agents`]).
    fn hosted_agent(&self, id: u64) -> Option<restore::AgentRestore> {
        crate::harness_host::followed_snapshot(&self.session_sid(id)?)
            .map(restore::AgentRestore::of)
    }

    /// The word standing that pool session `id`'s agent cannot be brought
    /// back, as its supervisor host keeps it
    /// ([`crate::harness_host::HostHandle::lost_word`]), in the form a
    /// handoff leaf carries ([`restore::TerminalLeafRestore::lost`]).
    fn hosted_lost_word(&self, id: u64) -> Option<restore::LostRestore> {
        self.harness.as_ref()?.lost_word(&self.session_sid(id)?)
    }

    /// The agent a restore that is still under way owes pool session `id`'s
    /// tab and has not handed the host yet (`restored_agents`, handed at the
    /// pass's end: [`Self::close_restore_pass`]) — a cold drain's landed tab
    /// until its last slice — in the form a durable leaf carries it, with no
    /// explicit restart's cause ([`Self::pending_restored_agent`]). Neither
    /// the host's queue nor an agent it publishes names it yet, so a restart
    /// inside the drain wrote no agent on any tab it had landed (F1). So too
    /// the agent of a landed tab that was HELD (`restored_held`, said at the
    /// pass's end: [`Self::say_restored_held`]): the holder's still, its leaf
    /// names it beside the hold — the session's, and the rows the restore
    /// could not put back on it ([`Self::unhanded_restored_holds`]) — and
    /// the next launch says it again. With held agents in a list of their
    /// own (LC-8), a restart inside the drain lost it.
    ///
    /// ONLY WHILE A HOLD STANDS ON IT (ruling 23 (ii): a held tab's agent is
    /// not relaunched when the hold lifts; ATERM-KI-1016-1 LC-8). A held
    /// tab's agent the owner's lift has left with no hold on the leaf
    /// ([`Self::desk_holds`] empty) is named nowhere: the leaf of an end
    /// inside the drain named it with no hold, and the next launch
    /// relaunched it into the tab and typed its carry-on there — where the
    /// same lift after the drain's last slice, which says the held agent and
    /// lets it go ([`Self::say_restored_held`]), relaunched nothing. The
    /// lift's own wake says it and lets it go ([`Self::let_go_lifted_held`]);
    /// this keeps a layout taken before that wake is read from naming it.
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

    /// THE HOLD ROWS OF A LANDED HELD TAB THAT ITS SESSION DOES NOT CARRY
    /// (the merge review's GUI-M7-1): the rows of pool session `id`'s leaf
    /// the restore could not put back on it — a fleet's, the owner's under a
    /// committing update's fence ([`Self::carry_restored_identity`]) — while
    /// the pass still holds its agent ([`Self::unhanded_restored_agent`]).
    /// Empty for any other session.
    fn unhanded_restored_holds(&self, id: u64) -> &[String] {
        self.restored_held
            .iter()
            .find(|(session, ..)| *session == id)
            .map_or(&[][..], |(.., rows)| rows.as_slice())
    }

    /// Whether the supervisor host published an agent for pool session `id`
    /// ([`Self::hosted_agent`] without reading its record).
    fn hosts_agent(&self, id: u64) -> bool {
        self.session_sid(id)
            .is_some_and(|sid| crate::harness_host::published_snapshot(&sid).is_some())
    }

    /// THE AGENTS THIS DESK HOLDS, put on `manifest`'s terminal leaves: ONE
    /// question for every layout an end nobody chose leaves the next launch
    /// to relaunch — the crash journal's image ([`Self::tick_crash_journal`])
    /// and the quit layout of a restart, a logout or a shutdown
    /// (`main_entry`, [`crate::system_quit`]). On each leaf, the agent its
    /// session hosts ([`Self::hosted_agent`]), else — none published yet,
    /// as until a relaunched agent runs — the one still OWED there (a cold
    /// restore's queue, a relaunch on exit a worker's back-off holds:
    /// [`Self::pending_restored_agent`]), else — a cold drain's landed tab
    /// before its last slice hands the host its agent — the one the restore
    /// still holds for it ([`Self::unhanded_restored_agent`]), else — an
    /// update's successor, on a shell its parent handed it — what the
    /// parent's image said runs there, until this run knows that tab itself
    /// ([`crate::crash_journal::Lane::carry_forward`], round eight of the
    /// update audit, findings 46 and 76; that carry also keeps the parent's
    /// folder on a leaf whose engine answered none, and names the pairs the
    /// journal's header keys each handed shell by). A person's own quit
    /// keeps none.
    ///
    /// The quit asks it after the host's shutdown, which stops the queue's
    /// steps and keeps what it owes (`harness_host::RestoredQueue::closed`),
    /// and then retires the journal. It used to ask for the hosted agent
    /// alone, after a shutdown that emptied the queue, so a restart inside
    /// the relaunch's window lost the agent the journal had named, and the
    /// next launch reopened its tab bare (restore audit round two,
    /// 2026-10-02) — and so did a restart of an update's successor before
    /// its host published the agent its parent handed it running (the
    /// review of that round, 2026-10-03).
    ///
    /// AND THE HOLDS ON ITS TABS (round twenty, LC-8 of ATERM-KI-1015-1;
    /// [`restore::TerminalLeafRestore::holds`]), on each tab's own leaf,
    /// whatever runs there: a tab its owner or the fleet held when aterm
    /// ended keeps its agent the holder's — the cold restore relaunches
    /// nothing there and an owner's hold comes back on it
    /// ([`Self::carry_restored_identity`], ruling 23) — for as long as the
    /// hold stands: on the agent alone, it was lost at the next end after the
    /// held agent's attempt was over (the review of LC-8). They were carried
    /// nowhere, and the next launch reopened the tab unheld, relaunched its
    /// agent and typed the continuation into it. A landed held tab whose
    /// agent the drain still holds keeps, beside the session's, the rows the
    /// restore could not put back on it ([`Self::unhanded_restored_holds`]:
    /// a fleet's, an owner's a committing update's fence refused), so the
    /// leaf that names the agent names what keeps it down (GUI-M7-1).
    pub(crate) fn fill_desk_agents(&mut self, manifest: &mut restore::RestoreManifest) {
        manifest.fill_agents(&|id| {
            self.hosted_agent(id)
                .or_else(|| self.pending_restored_agent(id, false).map(|agent| *agent))
                .or_else(|| self.unhanded_restored_agent(id))
        });
        manifest.fill_holds(&|id| self.desk_holds(id));
        let pool = &self.pool;
        self.crash_journal
            .carry_forward(manifest, &|id| pool.get(id).and_then(Self::handed_session));
    }

    /// The hold rows session `id`'s leaf carries ([`Self::fill_desk_agents`]):
    /// the session's own, and beside them the rows a landed held tab's
    /// restore could not put back on it ([`Self::unhanded_restored_holds`]).
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
        // The session's own rows first: a slot it holds stands.
        crate::fabric::render_holds(&crate::fabric::parse_holds(
            live.iter().chain(carried).map(String::as_str),
        ))
    }

    /// A PERSON'S STANDING WORD ABOUT TYPING MAY HAVE MOVED ON SESSION
    /// `local_id` — its Autopilot word or timer (any writer: `meta set|unset
    /// autopilot`, the chip, the menu, Settings, a timed Cruise landing), or
    /// a hold (`hold <sid> on|off`, the menu, the fleet's) — so the crash
    /// journal is told what the session's leaf carries now, read as the
    /// capture reads it ([`SessionMeta::carried_autopilot`],
    /// [`Self::desk_holds`]). Where its last image carries other words the
    /// capture is owed now ([`crate::crash_journal::Lane::note_owner_words`]):
    /// the loop turn's own capture takes it whatever the write interval says
    /// — the park's ([`Self::tick_crash_journal`]; `about_to_wait` runs after
    /// every batch of events) or, for a word moved later on the turn than
    /// the park (a timed Cruise's landing), the last look before the loop
    /// sleeps ([`Self::crash_journal_wake`]). Nothing is captured here, so a
    /// batch of words (a scripted `meta set` loop, a fleet's holds) costs one
    /// capture, not one per word (review OW-P1). Every other change keeps the
    /// write interval. Windows' end-session snapshot is told too
    /// ([`Self::end_session_snapshot`]).
    /// Cheap where nothing moved: one meta guard, one fabric guard, one walk
    /// of the last image — so every wake that may move a word asks.
    ///
    /// [`SessionMeta::carried_autopilot`]: crate::session_timeline::SessionMeta::carried_autopilot
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

    /// WINDOWS' END-SESSION SNAPSHOT, WHEN DUE at `now`
    /// ([`crate::crash_journal::EndSessionCadence`]: every couple of seconds,
    /// and at once where a word about typing moved from what the last one
    /// carries, [`Self::journal_owner_words`]): the layout for the
    /// end-session handler to write, `None` inside it where `restore_session`
    /// is off (retracted); `None` when not due. A logoff or a shutdown is an
    /// end nobody chose, so the layout keeps the desk's agents and the
    /// owner's holds on their leaves ([`Self::fill_desk_agents`]), as the
    /// crash journal's image and a restart's quit do: taken from the layout
    /// alone, it carried no hold at all, and the reopened tab was unheld. The Windows lane publishes it
    /// from `about_to_wait` (`platform_win::set_end_session_snapshot`).
    #[cfg_attr(
        not(any(windows, test)),
        allow(
            dead_code,
            reason = "only the Windows lane publishes the end-session snapshot; every host tests \
                      when it is taken"
        )
    )]
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

    /// ARM THE WORDS' DURABILITY ACK (ATERM-KI-1016-1): the session store
    /// hands the control verbs that move a word about typing this lane's
    /// [`crate::crash_journal::Durable`], and a ticket (or a failed write)
    /// wakes the loop with `wake`. Once at launch, and again by a test that
    /// swaps the lane in.
    pub(crate) fn arm_word_durability(&self, wake: impl Fn() + Send + Sync + 'static) {
        let durable = self.crash_journal.durable();
        durable.set_wake(wake);
        self.store
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .set_journal_durable(durable);
    }

    /// WHAT A QUIT'S LAYOUT KEEPS OF THE DESK'S AGENTS AND THE OWNER'S HOLDS
    /// (`main_entry`'s quit): an end nobody chose — `keeps`, a restart, a
    /// logout or a shutdown (`desk::Desk::quit_keeps_agents`) — keeps them
    /// ([`Self::fill_desk_agents`]); a person's own quit keeps none
    /// ([`restore::RestoreManifest::forget_agents_and_holds`]), not even those
    /// a tab a cold drain still owed carries from the layout it rebuilds
    /// ([`RestoreDrain::carry_owed`]), which a crash or a restart wrote: the
    /// next launch relaunched an agent the person's own quit had ended, its
    /// tab held (RV-SURV-1).
    pub(crate) fn quit_agents(&mut self, manifest: &mut restore::RestoreManifest, keeps: bool) {
        if keeps {
            self.fill_desk_agents(manifest);
        } else {
            manifest.forget_agents_and_holds();
        }
    }

    /// `session` as a seamless update's parent handed it to this process —
    /// the parent's pool id for it, and what this process's own status read
    /// says runs there — for the crash journal's carry
    /// ([`crate::crash_journal::Lane::carry_forward`]); `None` for a session
    /// this process started.
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

    /// A pane session's persisted `(cwd, title)`: the engine's OSC-7 cwd and OSC-0/2
    /// title, read under the session lock. An unknown id (can't happen while the pane
    /// tree and pool are in sync) degrades to empty metadata, never a panic at quit.
    fn restore_session_meta(&self, id: u64) -> (Option<String>, String) {
        use crate::cwd_native::ReportedCwd as _;
        self.pool.get(id).map_or((None, String::new()), |s| {
            let t = match s.term.try_lock() {
                Ok(guard) => guard,
                Err(std::sync::TryLockError::Poisoned(poison)) => poison.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => {
                    // Capture is presentation metadata, never an authority. A
                    // reflow/compression lock must not freeze quit or update;
                    // omit cwd/title and preserve the exact layout/session id.
                    return (None, String::new());
                }
            };
            (
                // Persist the NATIVE path. The manifest's cwd is replayed into
                // the next launch's `spawn_session`, so a Windows entry holding
                // the engine's `/C:/Users//x` URI path would resurrect every pane
                // in a directory that does not exist. Written native, it also
                // survives a round trip unchanged (the conversion is idempotent).
                t.native_working_directory().map(|cwd| cwd.into_owned()),
                t.title().to_string(),
            )
        })
    }

    /// Whether a program other than session `id`'s shell holds its PTY, and
    /// it is not the agent aterm hosts there ([`Self::hosted_agent`]), which
    /// runs on this machine — for its leaf's
    /// [`restore::TerminalLeafRestore::held`]: one `tcgetpgrp`
    /// (`App::session_held`), then, only for a held pane, the host's
    /// published snapshot. Off unix it is not asked: no folder is judged
    /// there ([`crate::spawn_folder::folder_fault`]), and the job test walks
    /// the process table.
    fn restore_session_held(&self, id: u64) -> bool {
        cfg!(unix) && self.pool.get(id).is_some_and(Self::session_held) && !self.hosts_agent(id)
    }

    /// THE CRASH JOURNAL'S CAPTURE (PTY keeper P1, [`crate::crash_journal`]), on
    /// the way to every wait. It follows `restore_session` exactly as the quit's
    /// layout write does — off, and the journal is withdrawn — and writes nothing
    /// until this process's layout is its own: after the first frame, once the
    /// deferred restore has rebuilt the previous layout, and — a self-update's
    /// successor — only after its Commit (before it, the outgoing process owns
    /// the sessions, and a refused candidate is reaped with SIGKILL). Then a
    /// capture runs only when the lane says one is due (something other than a
    /// timer woke the loop, and the write interval has passed), and hands the
    /// writer an image only when it differs from the last.
    pub(crate) fn tick_crash_journal(&mut self, now: std::time::Instant) {
        if !self.crash_journal.is_active() {
            return;
        }
        if !self.config.restore_session_or_default() {
            self.crash_journal.withdraw();
            // …and the layout this launch took goes too: nothing is restored after a
            // crash now (`restore::settle_take`).
            crate::restore::settle_take();
            return;
        }
        // The verbs waiting on a word's image are answered by this lane
        // (`crash_journal::Durable`), and a write that failed is owed again.
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
        // The layout, and on each leaf the agent the desk holds there: what
        // an end nobody chose leaves the next launch to relaunch (P6a, owner
        // direction 2026-09-27) — the agent its session hosts, or, where none
        // is published, the one still OWED there, as the handoff layout
        // carries it (restore audit, 2026-10-02), or, on an update's
        // successor, what its parent's image said of a shell it handed
        // (round eight of the update audit, findings 46 and 76):
        // [`Self::fill_desk_agents`]. Nothing is published for a tab until
        // its relaunched agent runs, so a launch that reopened a crashed desk
        // wrote images naming no agent there, and stopped before the
        // relaunch ran it came back bare. Not closed: from a relaunch's
        // landing to its agent's first publication neither names it.
        // The tickets of the words stored so far are read out first: the
        // capture below reads every one of those words.
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

    /// THE CRASH JOURNAL'S LAST LOOK BEFORE THE LOOP SLEEPS, at `now`
    /// (`about_to_wait`'s deadline fold): a capture due since the park's own
    /// ([`Self::tick_crash_journal`]) is taken here — above all one a word
    /// about typing moved on this turn after the park owes now (a timed
    /// Cruise's landing fires after it, `App::expire_autopilot_timers`;
    /// [`Self::journal_owner_words`]), which must not wait for a wake that
    /// may never come — and then the instant the lane wants the loop back
    /// (its trailing edge, a probation's end). The park's capture pushed the
    /// write interval out, so this takes a second capture on one turn only
    /// for a word that moved after it. One due test when nothing is owed.
    pub(crate) fn crash_journal_wake(
        &mut self,
        now: std::time::Instant,
    ) -> Option<std::time::Instant> {
        self.tick_crash_journal(now);
        self.crash_journal.next_wake(now)
    }

    /// THE JOURNAL'S PROGRAMS (D16, rulings 282 and 292): every terminal leaf
    /// of `manifest` whose session ran a program when aterm died would take it
    /// down ([`leaf_program`]): a foreground JOB (the quit confirm's own test,
    /// [`crate::quit_safety`]), a session root that is not a shell (`aterm -e`,
    /// `exec vim`), or a shell with a child (a background or suspended job) —
    /// named when the status sweep has resolved that group's program. One
    /// `tcgetpgrp` and one [`crate::quit_safety::session_root`] read per leaf,
    /// at most once per journal capture (every 2 s at most and only after
    /// activity, save the one capture of a loop turn that moved a tab's word
    /// about typing, [`crate::crash_journal::Lane::note_owner_words`]). A
    /// session with no live child (a stub, one whose shell exited) is skipped.
    /// The layout itself stays exactly the quit's capture, so the update
    /// worker's layout digest never sees a program come and go.
    fn crash_journal_programs(
        &self,
        manifest: &restore::RestoreManifest,
    ) -> Vec<crate::crash_journal::LeafProgram> {
        // Each leaf's session, and whether its restore carries the agent
        // that session hosts (P6a): that program is the agent (ruling 293).
        fn leaves(node: &restore::RestoredSplitTree, out: &mut Vec<(u64, bool)>) {
            match node {
                restore::RestoredSplitTree::Leaf {
                    view: restore::RestoredView::Terminal(leaf),
                } => out.extend(leaf.local_id.map(|id| (id, leaf.agent.is_some()))),
                restore::RestoredSplitTree::Leaf { .. } => {}
                restore::RestoredSplitTree::Split { first, second, .. } => {
                    leaves(first, out);
                    leaves(second, out);
                }
            }
        }
        let mut programs = Vec::new();
        // The sessions an entry already names: a co-viewed session's later
        // leaves are marked, so its program counts once (`LeafProgram::again`).
        let mut captured: Vec<u64> = Vec::new();
        for (window, layout) in manifest.windows.iter().enumerate() {
            for (tab, restored) in layout.restored_tabs.iter().enumerate() {
                let mut ids = Vec::new();
                leaves(&restored.root, &mut ids);
                for (id, agent) in ids {
                    let Some(session) = self.pool.get(id) else {
                        continue;
                    };
                    if session.pid <= 0
                        || session.master < 0
                        || session
                            .child_reaped
                            .load(std::sync::atomic::Ordering::Relaxed)
                    {
                        continue;
                    }
                    let group = crate::quit_safety::foreground_pgrp(session.master);
                    #[cfg(unix)]
                    let root = RootAnswer::Read(crate::quit_safety::session_root(session.pid));
                    // windows: `foreground_is_job`'s child walk already counts a
                    // shell's jobs; a program that is the ConPTY root is not seen.
                    #[cfg(not(unix))]
                    let root = RootAnswer::Unasked;
                    let named = {
                        let timeline = session
                            .ctx
                            .timeline
                            .lock()
                            .unwrap_or_else(|p| p.into_inner());
                        let agent = timeline.agent();
                        agent.program.clone().map(|name| (agent.program_pgid, name))
                    };
                    let Some(program) = leaf_program(
                        group,
                        session.pid,
                        &root,
                        named.as_ref().map(|(pgid, name)| (*pgid, name.as_str())),
                    ) else {
                        continue;
                    };
                    programs.push(crate::crash_journal::LeafProgram {
                        window: u32::try_from(window).unwrap_or(u32::MAX),
                        tab: u32::try_from(tab).unwrap_or(u32::MAX),
                        program,
                        agent,
                        again: captured.contains(&id),
                    });
                    captured.push(id);
                }
            }
        }
        programs
    }

    /// Rebuild the previous quit's layout (one-shot: drains `pending_restore`; a second
    /// `resumed` no-ops). Called after the first OS window is attached, so extra
    /// windows can create their surfaces and every spawned pane has a live event loop.
    pub(crate) fn apply_pending_restore(&mut self, el: &ActiveEventLoop) {
        // Rebuild the persisted layout when there is one. A SEAMLESS adopt with NO
        // manifest (its layout write failed) still falls through to the orphan net below,
        // so its handed-off shells are placed as tabs rather than stranded — the layout is
        // recoverable, a lost shell is not.
        let mut handed_off_leaves = Vec::new();
        if let Some(manifest) = self.pending_restore.take() {
            // The leaves naming handed-off shells outlive the manifest the rebuild
            // consumes, so a shell the rebuild leaves over still gets its identity.
            // A cold restore hands nothing off and keeps none.
            if !self.seamless_adopt.is_empty() {
                handed_off_leaves = manifest.handed_off_leaves();
            }
            let fold = manifest.fold.clone();
            self.apply_restore_manifest(el, manifest);
            // Once its windows are up: what the outgoing build folded to fit.
            self.say_layout_folded(&fold);
        }
        self.close_restore_pass(&handed_off_leaves);
    }

    /// What follows the last window of a rebuild, whole or drained
    /// ([`Self::run_restore_slice`]).
    fn close_restore_pass(&mut self, handed_off_leaves: &[restore::TerminalLeafRestore]) {
        // SEAMLESS orphan safety net: any handed-off live shell the layout did NOT place
        // (a stale/inconsistent/absent manifest, or a leaf that failed to rebuild) is
        // adopted anyway as a fresh tab in the front window. A live shell is NEVER dropped
        // just because its exact pane could not be reconstructed. A no-op on a cold
        // restore (`seamless_adopt` is empty).
        self.adopt_orphan_shells_as_tabs(handed_off_leaves);
        // A held pane's screen no placeholder named (no layout was placed, or
        // its leaf did not rebuild) has no pane to show in: it goes, as the
        // pane itself would have before round five. So does the record of the
        // ones shown, which only this restore's later leaves read.
        self.seamless_held.clear();
        self.seamless_held_shown.clear();
        self.hand_restored_agents();
        // After EVERY window, tab and leaf the restore built or took down:
        // only now is a draft that landed known to have stayed.
        self.settle_carried_settings_drafts();
    }

    /// One slice of a cold launch's rebuild ([`RestoreDrain`]): the first takes
    /// the manifest, each lands windows and tabs until [`RESTORE_SLICE_BUDGET`]
    /// is spent (at least one), and the last closes the pass as a whole one
    /// does. `true` once nothing is owed.
    pub(crate) fn run_restore_slice(&mut self, el: &ActiveEventLoop) -> bool {
        self.drain_restore_slice(RESTORE_SLICE_BUDGET, |app, first, wl, placed, filling| {
            app.restore_window(el, first, wl, placed, Some(filling))
        })
    }

    /// [`Self::run_restore_slice`] with its `budget` and its window step
    /// (`begin`: [`Self::restore_window`], which creates every window after
    /// the first) handed in.
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
    ) -> bool {
        let slice_began = std::time::Instant::now();
        let mut drain = self
            .restore_drain
            .take()
            .unwrap_or_else(|| Box::new(RestoreDrain::new(self.pending_restore.take())));
        loop {
            let unit_began = std::time::Instant::now();
            if let Some(filling) = drain.filling.as_mut() {
                if !self.land_next_drained_tab(filling)
                    && let Some(done) = drain.filling.take()
                {
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
            } else {
                break;
            }
            // Stop before a next unit as long as this one would overrun the
            // budget: a fork costs milliseconds, so most slices land one tab.
            if slice_began.elapsed() + unit_began.elapsed() >= budget {
                break;
            }
        }
        drain.slices = drain.slices.saturating_add(1);
        drain.longest = drain.longest.max(slice_began.elapsed());
        // The tabs this slice put on the strip show before the next one.
        if let Some(window) = drain
            .filling_window()
            .and_then(|wid| self.windows.get(&wid))
            .and_then(|ws| ws.os_window.as_ref())
        {
            window.request_redraw();
        }
        if drain.filling.is_some() || !drain.windows.is_empty() {
            self.restore_drain = Some(drain);
            return false;
        }
        self.record_carried_window_show(&drain.placed.shown, drain.placed.bootstrap);
        self.say_layout_folded(&drain.fold);
        self.close_restore_pass(&[]);
        aterm_log::info!(
            "session restore: drained {} window(s) in {} slice(s) over {:.1} ms, longest slice \
             {:.1} ms",
            drain.placed.shown.len(),
            drain.slices,
            drain.began.elapsed().as_secs_f64() * 1000.0,
            drain.longest.as_secs_f64() * 1000.0
        );
        true
    }

    /// Begin filling window `wid` from `wl` for a cold drain: a window of more
    /// than one tab, every leaf a terminal, lands its bootstrap tab now (the
    /// graft forks nothing) and owes the rest, front tab first. Any other —
    /// the legacy mirror, a native or held leaf, a single tab — is filled whole
    /// ([`Self::restore_into_window`]). `None` when nothing is owed.
    fn begin_sliced_fill(
        &mut self,
        wid: WindowId,
        wl: restore::WindowLayout,
    ) -> Option<FillingWindow> {
        if wl.restored_tabs.len() < 2
            || !wl
                .restored_tabs
                .iter()
                .all(|tab| terminal_leaves_only(&tab.root))
        {
            self.restore_into_window(wid, wl);
            return None;
        }
        let restore::WindowLayout {
            active_item,
            restored_tabs,
            ..
        } = wl;
        let front = active_item
            .unwrap_or(0)
            .min(restored_tabs.len().saturating_sub(1));
        // As `restore_recursive_into_window` does: the bootstrap projection
        // goes and its shell waits, detached, for the first terminal leaf.
        // Nothing detached crosses a turn: tabs land in
        // manifest order until one has and the bootstrap is placed.
        let mut reusable = self.clear_restore_target(wid, true);
        self.frontmost_window = Some(wid);
        let mut filling = FillingWindow {
            wid,
            queue: restored_tabs.into_iter().enumerate().collect(),
            placed: Vec::new(),
            front,
            shown: None,
        };
        while let Some((index, record)) = filling.queue.pop_front() {
            self.land_drained_tab(&mut filling, index, &record, &mut reusable);
            if !filling.placed.is_empty() && reusable.is_none() {
                break;
            }
        }
        // A bootstrap no terminal leaf took is not user content on a cold restore.
        if let Some((view, session)) = reusable.take() {
            self.view_store.remove(view);
            self.teardown_session(session);
        }
        // The tab the layout had in front lands next: it is the one on glass.
        if let Some(at) = filling
            .queue
            .iter()
            .position(|(index, _)| *index == filling.front)
            && let Some(entry) = filling.queue.remove(at)
        {
            filling.queue.push_front(entry);
        }
        if filling.queue.is_empty() {
            self.finish_sliced_fill(&filling);
            return None;
        }
        Some(filling)
    }

    /// Land `filling`'s next owed tab. `false` once it owes none — or its
    /// window is gone, closed meanwhile, which owes nothing more.
    fn land_next_drained_tab(&mut self, filling: &mut FillingWindow) -> bool {
        if !self.windows.contains_key(&filling.wid) {
            filling.queue.clear();
            return false;
        }
        let Some((index, record)) = filling.queue.pop_front() else {
            return false;
        };
        let mut reusable = None;
        self.land_drained_tab(filling, index, &record, &mut reusable);
        !filling.queue.is_empty()
    }

    /// Build manifest tab `index` of `filling`'s window and put it on the tab
    /// strip at once: after the nearest restored tab before it in the layout
    /// (else before the nearest after it), whatever tabs a person opened
    /// meanwhile. The front stays where it was, except that the window's first
    /// tab, and then the layout's front tab, come to the front while the
    /// person has chosen no other.
    fn land_drained_tab(
        &mut self,
        filling: &mut FillingWindow,
        index: usize,
        record: &restore::RestoredTab,
        reusable: &mut Option<(crate::tab_model::ViewId, u64)>,
    ) {
        let wid = filling.wid;
        let (tab, layout) =
            match self.build_recursive_restore_tab(wid, record, reusable, LeafIds::Retired) {
                Ok(built) => built,
                Err(error) => {
                    crate::logging::stderr_line!(
                        "aterm-gui: session restore: could not allocate a recovered tab: {error}"
                    );
                    return;
                }
            };
        let id = tab.id;
        let terminal = |view_store: &crate::tab_model::ViewStore, tab: &crate::tab_model::Tab| {
            tab.root.leaves().into_iter().all(|view| {
                matches!(
                    view_store.get(view),
                    Some(crate::tab_model::View::Terminal(_))
                )
            })
        };
        let Some(window) = self.windows.get_mut(&wid) else {
            return;
        };
        let tabs = window.tab_set.tabs();
        // The restored tabs still on the strip (a person may have closed
        // some), each with its manifest index and where it stands.
        let present = filling
            .placed
            .iter()
            .filter_map(|(placed, id)| Some((*placed, tabs.iter().position(|tab| tab.id == *id)?)));
        let target = restored_tab_position(present, index, tabs.len());
        let front_before = window.tab_set.active_id();
        window
            .tab_set
            .push(tab)
            .expect("fresh restored tab identity");
        let moved = window.tab_set.reorder(id, target);
        debug_assert!(moved);
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
        let projection = |upto: usize| {
            window.tab_set.tabs()[..upto]
                .iter()
                .filter(|tab| terminal(&self.view_store, tab))
                .count()
        };
        let layout_at = projection(target);
        let front_at = window
            .tab_set
            .active()
            .filter(|tab| terminal(&self.view_store, tab))
            .and_then(|_| window.tab_set.active_index())
            .map(projection);
        if let Some(layout) = layout {
            window.layouts.insert(layout_at, layout);
        }
        window.tabs = TabIndex::new(front_at.unwrap_or(0), window.layouts.len());
        filling.placed.push((index, id));
        // A tab that lands behind the front one changes the strip and nothing
        // else, so only the strip is redrawn. A full sync (`App::sync_window`)
        // would close the find bar, end a selection drag and reset the input
        // predictor of the tab the person is using, once per slice.
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

    /// A drained window's tabs are all in: the layout's front tab, if it never
    /// landed, gives way to the nearest restored tab that did (while the person
    /// has chosen no other), as the whole pass picks among the tabs it built.
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
        debug_assert!(
            self.restored_window_sessions_are_distinct(wid),
            "session restore aliased two leaves of window {wid:?} onto one shell",
        );
    }

    /// Say ONCE, as a calm record on glass, what the outgoing build folded to
    /// bring this update's desk within the layout's caps (round eight of the
    /// update audit, D5: `restore::RestoreManifest::fold_into_caps`) — the
    /// windows merged into tabs, the tabs moved to another window — so a
    /// person who finds a window's tabs elsewhere is told why. Only a
    /// seamless update's layout carries a fold; silent for every other.
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

    /// THE RESTORED AGENTS' ROW, WORDED BY THE WINDOW (ATERM-KI-1016-1 host-7):
    /// the row the host said (`harness_host::say_restored`, every miss it
    /// said this process), as the owner should see it now. The host knows
    /// which agents did not come back; only the window knows where their tabs
    /// are and what became of the row it showed last ([`RestoredShown`]).
    ///
    /// - A ROW SAID BEFORE A LATER ONE IS PASSED OVER ([`RestoredRow::seq`]):
    ///   the later one names it all. Two sayers raced after a Commit, and the
    ///   row posted last could be the older one, taking the newer one's tabs off.
    /// - WHAT THE OWNER SAW THROUGH STAYS DOWN: once the row this process
    ///   showed was seen through ([`restored_row_seen_through`]: dismissed,
    ///   answered, resolved by its reporter, or on the glass for its whole
    ///   span), the misses it named are not named again, so the next row
    ///   starts a fresh list, as the folder rows do. Until then a row the owner
    ///   dismissed came back inside the next one. A row that left the band
    ///   otherwise — it never reached the glass (unseen), the band was full
    ///   (evicted), it went stale or was withdrawn — was seen through by
    ///   nobody: its misses are named again in the next row, as a row still up
    ///   is (the review of host-7's first cut, which counted any row no longer
    ///   up as seen and dropped a miss said while the band was full or before
    ///   the window showed it). A miss said again is said anew.
    /// - EACH TAB WHERE IT IS NOW: a line is worded with its tab's place now
    ///   ([`Self::upgrade_place`]). A miss said before whose tab no tab shows
    ///   any more leaves the row. A miss said in this row whose tab is not shown
    ///   keeps the place it was said with. Until then a tab renumbered since
    ///   its miss was named by its old place, which could be another tab's
    ///   now.
    /// - POSTED UNDER THIS PROCESS'S KEY
    ///   ([`crate::message_reporters::restored_agents_key`]): a seamless update's
    ///   successor never supersedes the row carried from the aterm before it,
    ///   which names the tabs said at the park.
    ///
    /// Nothing is posted when no line is left: the row up, if any, stands.
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

    /// Hand the agents the restore pass found ([`Self::carry_restored_identity`])
    /// to the supervisor host, which relaunches each in the tab that now
    /// stands for it ([`crate::harness_host::HostHandle::relaunch_restored`]):
    /// the tab's sid and its new shell, and what the live layout carried of
    /// the agent. A session no longer here (its leaf failed to rebuild) is
    /// dropped; with no host (the harness off) nothing is relaunched.
    fn hand_restored_agents(&mut self) {
        self.say_restored_held();
        let restored = self.take_restored_agents();
        if restored.is_empty() {
            return;
        }
        if let Some(host) = &self.harness {
            // Each with where its tab is, for the row that says it did not
            // come back (ruling 293).
            let restored = restored
                .into_iter()
                .map(
                    |(sid, snap, asked, was)| crate::harness_host::RestoredAgent {
                        place: self.upgrade_place(&sid),
                        // The layout's snapshot: no process was read for it.
                        // The home and the login its leaf names ride it, and
                        // only one naming neither takes the roster's account
                        // (`harness_host::RestoredAgent::unread`).
                        cold: true,
                        sid,
                        snap,
                        // The explicit restart's cause its leaf carries (round
                        // eighteen): relaunched as the act's.
                        asked,
                        // The tab it ran in, a cold restore's old sid.
                        was,
                    },
                )
                .collect();
            host.relaunch_restored(restored);
        }
    }

    /// THE AGENTS OF HELD TABS, SAID (LC-8, ruling 23): each tab that was held
    /// when aterm ended reopened at its shell, its agent not relaunched
    /// ([`Self::carry_restored_identity`]) — a record per tab
    /// ([`crate::message_reporters::restored_agent_held`]) and a log line.
    /// The record gives the hold as the reason only where the cold restore
    /// would otherwise have relaunched the agent (a host that relaunches
    /// restored agents, [`crate::harness_host::HostHandle::relaunches_restored`],
    /// and a conversation to resume), and names the agent's own resume line
    /// ([`restore::AgentRestore::resume_line`]: its program's, its home on
    /// it). A session no longer here (its leaf failed to rebuild) is passed
    /// over, and an agent two leaves named (a session shown in two windows)
    /// is said once, at the first ([`restore::AgentRestore::ran_as`], as its
    /// relaunch would be made). One-shot. A tab whose hold the owner has
    /// lifted since is said lifted ([`Self::let_go_lifted_held`] says it as
    /// the lift's wake is read).
    fn say_restored_held(&mut self) {
        let lifted: Vec<bool> = self
            .restored_held
            .iter()
            .map(|(id, ..)| self.desk_holds(*id).is_empty())
            .collect();
        let held = std::mem::take(&mut self.restored_held);
        self.say_held_agents(held.into_iter().zip(lifted));
    }

    /// A LANDED HELD TAB WHOSE HOLD THE OWNER LIFTED INSIDE THE COLD DRAIN IS
    /// LET GO AS THE LIFT'S WAKE IS READ, ITS RECORD SAID (the review's
    /// KR-J-5 of ATERM-KI-1016-1; ruling 23 (ii)): pool session `session`'s
    /// held entry (`restored_held`), once no hold stands on its leaf
    /// ([`Self::desk_holds`]: the session's rows, and the rows the restore
    /// could not put back). Its leaf names its agent only while a hold
    /// stands ([`Self::unhanded_restored_agent`]), so a restart or crash
    /// inside the drain after the lift relaunches nothing — and the record
    /// that names the agent and the line that resumes it, said only at the
    /// pass's end ([`Self::say_restored_held`]), was said nowhere: the next
    /// launch has no held tab to say. Said here, on the lift's own loop
    /// turn (`Wake::FabricChanged`, `App::on_fabric_changed`), it is in the
    /// message log before an end the turn's park did not reach; as the same
    /// lift after the pass's end, it lets the agent go.
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

    /// Every held tab the drain still holds whose hold no longer stands, let
    /// go and said ([`Self::let_go_lifted_held`]): at the exit, before the
    /// message log closes (`App::close_messages_at_exit`), for a lift whose
    /// wake the loop never read — the quit's layout, written after the log
    /// closed, names no agent there, and the record would be said nowhere.
    pub(crate) fn let_go_every_lifted_held(&mut self) {
        let mut sessions: Vec<u64> = self.restored_held.iter().map(|(id, ..)| *id).collect();
        sessions.dedup();
        for session in sessions {
            self.let_go_lifted_held(session);
        }
    }

    /// The records and log lines of held tabs' agents
    /// ([`Self::say_restored_held`]), each with whether its hold was lifted
    /// since it came back.
    fn say_held_agents(
        &mut self,
        held: impl IntoIterator<Item = ((u64, restore::AgentRestore, bool, Vec<String>), bool)>,
    ) {
        let relaunching = self
            .harness
            .as_ref()
            .is_some_and(crate::harness_host::HostHandle::relaunches_restored);
        let mut said: Vec<(u32, String)> = Vec::new();
        for ((id, agent, fleet, _), lifted) in held {
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
            // Journaled where every other end of the restored queue is (the
            // review of LC-8: `relaunch-after-host-ended … step=ended:held`),
            // and the tab it ran in noted beside the reopened tab's loop
            // ledger, as a queued agent's is: no relaunch records it (the
            // decision review's RR-LC-3).
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
            let resume = agent.resume_line();
            // An agent this build has no lane for would not have been
            // relaunched held or not (`AgentRestore::snapshot`).
            let name = agent.name();
            if let Some(record) = crate::message_reporters::restored_agent_held(
                &sid,
                &place,
                &crate::message_reporters::HeldAgent {
                    name: name.unwrap_or_default(),
                    resume: resume.as_deref(),
                    stopped_by_hold: relaunching && agent.resumes() && name.is_some(),
                    fleet,
                    lifted,
                },
            ) {
                self.record_message(record);
            }
        }
    }

    /// THE SUCCESSOR'S COMMIT (round four, plan item 7): the restored agents
    /// the outgoing instance's cold-restore queue still owed at the park, each
    /// on the adopted shell its handoff leaf names
    /// ([`Self::carry_restored_identity`]), handed to this process's host to
    /// relaunch ([`Self::hand_restored_agents`]). Each relaunch is the same
    /// step a cold restore takes (`relaunch::after_host_ended`), so a relaunch
    /// the outgoing instance already typed is carried on from its record,
    /// never typed twice, and one whose agent is back is left to it. A named
    /// shell this process did not adopt is dropped, said in one log line.
    /// One-shot.
    #[cfg(unix)]
    pub(crate) fn hand_carried_restored_agents(&mut self) {
        self.adopt_carried_lost_words();
        self.queue_carried_restored_agents();
        self.hand_restored_agents();
        self.watch_handed_agents();
    }

    /// THE AGENTS THE OUTGOING INSTANCE'S HOST SUPERVISED (round eighteen;
    /// `restore::TerminalLeafRestore::supervised`), each in the shell this
    /// process adopted under the id its leaf names, handed to this process's
    /// host to watch from its Commit
    /// ([`crate::harness_host::HostHandle::watch_handed`]): an exit of one
    /// that came during the outgoing instance's Commit — read there, in a
    /// process about to exit, and acted on by nobody — or before this
    /// host's first pass names it is this host's to read and relaunch. One
    /// whose shell this process did not adopt is dropped; with no host
    /// nothing is watched. One-shot.
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
                    Some((sid.clone(), agent.handed_snapshot(sid, adopted)?))
                })
                .collect()
        };
        host.watch_handed(agents);
    }

    /// THE WORDS THE OUTGOING INSTANCE'S HOST KEPT THAT A TAB'S AGENT CANNOT
    /// BE BROUGHT BACK (LC-4 of ATERM-KI-1016-1;
    /// `restore::TerminalLeafRestore::lost`), each stood again by this
    /// process's host on the shell adopted under the id its leaf names
    /// ([`crate::harness_host::HostHandle::adopt_lost_words`]). Called at the
    /// Commit before the host resumes, so no pass of it starts a worker in
    /// such a tab — and lowers the carried mark as an earlier aterm's —
    /// before the word that keeps it stands. One whose shell this process did
    /// not adopt is dropped; with no host nothing is stood. One-shot: a later
    /// call finds nothing.
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

    /// THE SUPERVISION TAKEN OVER AT A SUCCESSOR'S COMMIT (the Commit wake,
    /// `Wake::ActivateCommittedHandoff`): the predecessor's supervisor host
    /// stopped at its Commit, and this one takes the sessions over (one
    /// supervisor per session), carries on every restart it left in flight,
    /// and relaunches the restored agents its cold-restore queue still owed
    /// (round four, plan item 7). Before the host resumes, what the
    /// predecessor carried for it stands here (LC-4 of ATERM-KI-1016-1): the
    /// person's stamps it wrote right before its Commit, taken before its
    /// files are swept ([`Self::take_carried_person_stamps`]) — a relaunch
    /// the host carries through a hand asks whether nobody keyed since — and
    /// the words it kept that a tab's agent cannot be brought back
    /// ([`Self::adopt_carried_lost_words`]), so no pass of this host lowers a
    /// carried mark one of them keeps. One-shot.
    #[cfg(unix)]
    pub(crate) fn take_over_supervision_at_commit(&mut self) {
        self.take_carried_person_stamps();
        // The predecessor leaves at Commit without its graceful cleanup; its
        // per-process files go once it has.
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

    /// [`Self::hand_carried_restored_agents`]' queue: each carried agent
    /// onto the pool session adopted under the id its leaf names, beside the
    /// cold restore's ([`Self::take_restored_agents`] reads both alike).
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
                // A live session's hold rides the handoff record, and the
                // relaunch line waits on it.
                Some(session) => self.restored_agents.push((session, agent)),
                None => aterm_log::warn!(
                    "session restore: the agent the previous aterm was still relaunching in \
                     its session {named} was not carried on: that session was not adopted"
                ),
            }
        }
    }

    /// Every live session this process holds, `(sid, place)`: where the
    /// successor's host looks, at its Commit, for a restart the outgoing
    /// instance left in flight
    /// ([`crate::harness_host::HostHandle::resume_after_handoff`]), and how a
    /// row names each tab.
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

    /// The restore pass's agents ([`Self::carry_restored_identity`]) as the
    /// host relaunches them: each tab's sid, and a snapshot naming that tab,
    /// its shell, and what the live layout carried of the agent. Taken: a
    /// second call finds none.
    ///
    /// ONE AGENT, ONE RELAUNCH (restore audit, 2026-10-02): a session shown
    /// in two windows (`Open Session in New Window`) names its agent on both
    /// leaves, and a cold restore gives each leaf a shell of its own, so the
    /// agent was relaunched once per leaf — one conversation resumed in two
    /// tabs. It goes to the first tab still here
    /// ([`restore::AgentRestore::ran_as`]), the place the reopened row names
    /// (`crash_journal::Reopened::resumed_agents`).
    ///
    /// Each with the explicit restart's cause its leaf carries (round
    /// eighteen, `restore::AgentRestore::cause`), and the tab it ran in where
    /// a cold restore minted another sid (`restore::AgentRestore::tab`). A
    /// held tab's agent is none of them: it is the holder's
    /// ([`Self::say_restored_held`]).
    fn take_restored_agents(&mut self) -> Vec<RestoredTake> {
        let taken = std::mem::take(&mut self.restored_agents);
        if taken.is_empty() {
            return Vec::new();
        }
        let mut handed: Vec<(u32, String)> = Vec::new();
        let g = self.store.read().unwrap_or_else(|p| p.into_inner());
        taken
            .into_iter()
            .filter_map(|(id, agent)| {
                let sid = g.by_local(id)?.sid.as_str().to_string();
                let shell = u32::try_from(self.pool.get(id)?.pid).ok()?;
                let (pid, start) = agent.ran_as();
                if handed.iter().any(|(p, s)| (*p, s.as_str()) == (pid, start)) {
                    return None;
                }
                handed.push((pid, start.to_string()));
                // A Codex no copy validated a line for comes back at its
                // shell (the Codex screen lane's retirement, 2026-10-16),
                // said once, here.
                if !agent.relaunched() {
                    aterm_log::info!(
                        "restore @{sid}: the codex that ran here (pid {pid}) is not relaunched: \
                         its leaf carries no line aterm's copy of codex-alab's own record \
                         validated"
                    );
                    return None;
                }
                let asked = agent.asked();
                let was = agent.ran_in_another(&sid);
                Some((sid.clone(), agent.snapshot(sid, shell)?, asked, was))
            })
            .collect()
    }

    /// SEAMLESS CONNECTION RE-MINT (design §1.4#6): re-establish the manifest's
    /// tokenless `(src, dst, op)` triples through the ONE kind-bounded mint
    /// helper, now that every handed-off session is registered under its fresh
    /// launch nonce. One-shot: the carry is drained on entry. Grouped per
    /// directed pair into the largest [`ConnectionKind`] the carried op set
    /// proves ([`crate::connections::carried_kind`], never-widen); each mint
    /// rebuilds the pair's `ConnectionRecord` and emits the standard
    /// `session_edge` grant events under origin `handoff`. FAIL-SOFT: a triple
    /// whose endpoint did not survive the swap — or whose op the connection
    /// vocabulary cannot spell — is dropped silently-but-audited
    /// (`action=drop origin=handoff`, hex-free); it can only LOSE authority,
    /// never widen it, and never fails the adoption.
    pub(crate) fn remint_carried_connections(&mut self) {
        use aterm_session::SessionId;
        let carried = std::mem::take(&mut self.pending_conn_carry);
        if carried.is_empty() {
            return;
        }
        // Group the flat rows per directed pair, preserving the manifest's
        // `(src, dst, op)` sort so mint/audit order is deterministic.
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
            let src = SessionId::new(src);
            let dst = SessionId::new(dst);
            // Both endpoints must have survived the swap: dst owns the table
            // the rows go into; a dead src must stay swept (§1.4#4 — re-minting
            // its rows would resurrect authority its close dissolved). A
            // carried self-loop (nothing mints one, but the manifest is data)
            // takes the same drop path. Clone-then-release: the store guard
            // drops before any table lock below.
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
            // The tables changed ON the main thread — run the
            // `Wake::ConnectionsChanged` body directly (marks, map, ❯ count).
            self.refresh_connection_surfaces();
        }
    }

    /// Rebuild one consumed [`restore::RestoreManifest`] into live windows/tabs/panes
    /// (adopting handed-off shells where a leaf's `local_id` matches, forking fresh
    /// otherwise), every window in this one call. Split out from
    /// [`Self::apply_pending_restore`] so the orphan net there runs on every path,
    /// including a seamless adopt whose manifest was absent. A cold drain takes the
    /// same windows a slice at a time ([`Self::run_restore_slice`]).
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

    /// Rebuild manifest window `wl` — the first (`first`) onto the bootstrap
    /// window, every further one into a window created for it — and record its
    /// show state in `placed`. `sliced` is a cold drain's slot: a window whose
    /// tabs can land one at a time leaves the rest there
    /// ([`Self::begin_sliced_fill`]); `None` fills it whole. `Break` when a
    /// window could not be created: the rebuild stops there.
    fn restore_window(
        &mut self,
        el: &ActiveEventLoop,
        first: bool,
        wl: restore::WindowLayout,
        placed: &mut RestoredWindows,
        sliced: Option<&mut Option<FillingWindow>>,
    ) -> std::ops::ControlFlow<()> {
        // THE SHOW STATE each manifest window carried, keyed by the live window
        // it became — applied as a stack once every one of them is on glass
        // (`crate::window_show`, gap #29). A window that is skipped or fails to
        // create is simply absent.
        //
        // The first persisted window maps onto the bootstrap window: session 0 already
        // runs in the persisted first-leaf cwd (seeded in `main`), so its layout is
        // rebuilt in place around that live session. Its GEOMETRY is deliberately not
        // applied here: the grid seeded window 1's creation (`run`), and the
        // position/maximized state applied post-attach in `resumed` — both strictly
        // earlier than this deferred (post-first-present) pass, so the bootstrap frame
        // never visibly hops the way a here-applied move would.
        if first {
            self.restore_bootstrap_window(wl, placed, sliced);
            return std::ops::ControlFlow::Continue(());
        }
        // Every further persisted window: a full window create — its first session
        // spawns straight into the persisted first-leaf cwd — then the same fill. A
        // native-only window takes the zero-session path: install its native descriptors
        // into an empty logical host before attaching glass, so restore never invents a
        // PTY merely to satisfy legacy mirrors.
        // A native-only window, or one whose bootstrap shell another
        // window already adopted (a co-view, round six finding 10).
        if self.restores_without_bootstrap(&wl) {
            let previous_front = self.frontmost_window;
            let outer = (wl.outer_x, wl.outer_y);
            let maximized = wl.maximized;
            let show = wl.show;
            let wid = self.create_native_restore_window(wl.rows, wl.cols);
            if let Some(ws) = self.windows.get_mut(&wid) {
                let frame = crate::app_window::CarriedFrame::of(&wl);
                ws.take_carried_frame(frame.map(|frame| frame.inner_px));
            }
            // Windows (and X11, its position): placed while still hidden, by the
            // attach below.
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
            // No glass: the window goes. A co-view window (round six,
            // finding 10) may hold shells adopted only in it; each one its
            // close drops degrades the handoff as a terminal window's
            // failure does (`App::note_handed_shell_lost`, round eight,
            // finding 58), rather than leaving a proof over a subset.
            if !self.headless && !self.attach_os_window(el, wid) {
                self.close_window_logical(wid);
                return std::ops::ControlFlow::Continue(());
            }
            placed.shown.push((wid, show));
            // W4: validated against the LIVE monitor set first — same
            // contract as the terminal-window arm below. (Windows placed it
            // before its reveal.)
            if !cfg!(windows)
                && let (Some(x), Some(y)) = outer
                && let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref())
                && crate::app_window::restored_position_on_screen(w, x, y)
            {
                w.set_outer_position(aterm_window::dpi::PhysicalPosition::new(x, y));
            }
            // W3: maximized LAST — see the ordering note below.
            if !cfg!(windows)
                && maximized == Some(true)
                && let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref())
            {
                w.set_maximized(true);
            }
            self.sync_active_session();
            return std::ops::ControlFlow::Continue(());
        }
        // The bootstrap starts in the directory of the pane it will fill — the
        // leaf `bootstrap_local_id` names, by the same pick.
        let cwd0: Option<String> = wl.bootstrap_cwd();
        // SEAMLESS: adopt this window's first-leaf shell as its bootstrap session
        // (matched by id), so the reopened window comes up with its LIVE shell rather
        // than a fresh one. Consumed (removed) so it is never adopted twice; a cold
        // restore's `seamless_adopt` is empty, so this is always `None` there. The
        // same pick `main_entry` makes for window 0 (`take_session0_shell`), so the
        // graft's leaf names this shell. An accepted handoff layout names each shell
        // once, so it is still waiting here; were it taken, the window's bootstrap
        // would be a fresh stand-in, which `carry_restored_identity` gives no identity.
        let adopt = take_handed_off_shell(&mut self.seamless_adopt, wl.bootstrap_local_id());
        let outer = (wl.outer_x, wl.outer_y);
        let maximized = wl.maximized;
        let show = wl.show;
        // EVERY WINDOW COMES BACK AT ITS OWN FRAME: its row's grid (an
        // update's adopted shells have it) and the raw size it had (a zoom
        // back out re-grids it, P6 finding 4), when the row carried that
        // size — an update's layout and a cold restore's alike. Any other
        // row (an older build's, a maximized or cold full-screen window's)
        // inherits the front window's grid, as Cmd-N does.
        let Some(wid) = self.create_window_internal_connected(
            el,
            cwd0.as_deref(),
            adopt,
            None,
            None,
            crate::app_window::CarriedFrame::of(&wl),
            (outer, maximized),
        ) else {
            // A window create fails on spawn or GPU-surface trouble — both likely to
            // repeat. Keep what restored rather than looping on failures. If this
            // window carried an adopted shell as its first leaf, this process loses
            // its copy with the failed window (its OTHER panes' shells stay in
            // `seamless_adopt` and the orphan net below re-homes them as tabs) — but
            // the shell itself is not signalled: before Commit it is the parked
            // parent's (`Session::handoff_commit`, round seven, item 20), and the
            // rollback below resumes it.
            crate::logging::stderr_line!(
                "aterm-gui: session restore: could not create a window; stopping here"
            );
            self.post_message(crate::message_reporters::restore_stopped_early());
            // OVERLAP HANDOFF: a carried window is now LOST (and possibly
            // its adopted shell with it). Withhold the readiness byte —
            // the parked parent's timeout + rollback recovers EVERY shell
            // from its own still-open fds, strictly better than exiting
            // under this degraded boot (see `App::handoff_degraded`).
            if self.handoff_ready.is_some() {
                self.handoff_degraded = true;
            }
            return std::ops::ControlFlow::Break(());
        };
        // SEAMLESS/RESTORE: put the reopened window back where it was (before its
        // first present, so there is no visible hop). Best-effort — a missing carry
        // or an off-desktop coordinate just leaves the cascade position the attach
        // applied. The off-desktop half is ENFORCED, not assumed (W4): the persisted
        // point is validated against the monitors that exist NOW, because the set
        // that existed at capture time is exactly what an undock/unplug changes —
        // a raw `set_outer_position` here used to reopen those windows entirely
        // outside the desktop. Windows did both inside the attach, before the
        // reveal (`App::stage_restore_placement`): here it is already on glass.
        if !cfg!(windows)
            && let (Some(x), Some(y)) = outer
            && let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref())
            && crate::app_window::restored_position_on_screen(w, x, y)
        {
            w.set_outer_position(aterm_window::dpi::PhysicalPosition::new(x, y));
        }
        // W3: re-maximize AFTER the position lands. The order is load-bearing
        // twice over: winit's Windows backend clears its MAXIMIZED window flag
        // inside `set_outer_position` (a moved window is definitionally not
        // maximized to it), and maximizing FIRST would anchor the frame to
        // whatever monitor the cascade picked rather than the one the persisted
        // point names. `Some(true)` only — `None` (an old manifest, or a
        // platform that does not capture the state) must never force a change.
        if !cfg!(windows)
            && maximized == Some(true)
            && let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref())
        {
            w.set_maximized(true);
        }
        placed.shown.push((wid, show));
        self.fill_restored_window(wid, wl, sliced);
        // OVERLAP HANDOFF: this extra window was created HIDDEN
        // (reveal-at-first-present) and a hidden macOS window is not
        // reliably given `RedrawRequested` — drive its first present
        // directly, exactly like the bootstrap window in `resumed`. The
        // post-present hook reveals it with the carried pixels already on.
        if self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.pending_reveal.is_some())
        {
            self.redraw_window(wid);
        }
        std::ops::ControlFlow::Continue(())
    }

    /// The first manifest window, rebuilt around the bootstrap window's live
    /// session ([`Self::restore_window`]).
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

    /// Fill window `wid` from its manifest layout: whole, or — a cold drain's
    /// window (`sliced`) — begun, with the rest of its tabs left in the slot.
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

    /// Install an empty logical host for a native-only restored window. No
    /// [`Session`], terminal, PTY fd, sink, terminal view, or pane tree is created.
    /// Native installation fills the canonical `TabSet` before glass attach.
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
        self.windows.insert(wid, Box::new(ws));
        self.frontmost_window = Some(wid);
        wid
    }

    /// Place any still-unadopted handed-off shells ([`Self::seamless_adopt`] left over
    /// after the layout was rebuilt) as fresh single-pane tabs in the front window, so no
    /// live shell handed across a seamless update is ever lost. Empty ⇒ no-op (every cold
    /// restore, and the normal seamless case where the layout placed them all).
    ///
    /// Each orphan comes back wearing the USER identity (`meta set` fields) of the
    /// leaf in `handed_off_leaves` that names it — kept from the manifest the
    /// rebuild consumed ([`restore::RestoreManifest::handed_off_leaves`]) and put on
    /// through [`Self::carry_restored_identity`], before the orphan is registered. A
    /// shell is left over because the leaf naming it never rebuilt (its tab failed
    /// to build, or its window could not be created) or was filled by another shell
    /// (the bootstrap a failed tab handed on, which gets none of that leaf's
    /// identity); either way the identity reached nobody before this net. An orphan
    /// no leaf names — there was no layout — comes back bare.
    fn adopt_orphan_shells_as_tabs(&mut self, handed_off_leaves: &[restore::TerminalLeafRestore]) {
        if self.seamless_adopt.is_empty() {
            return;
        }
        let orphans = std::mem::take(&mut self.seamless_adopt);
        // The headless test harness has no proxy; `spawn_orphan_shell` stubs the spawn.
        let can_spawn = self.proxy.is_some() || (cfg!(test) && self.headless);
        let Some(wid) = self.frontmost_window.filter(|_| can_spawn) else {
            // No window/proxy to place them in (should not happen post-restore): drop the
            // Adopted holders, which closes each handed master now rather than leaving it
            // open and readerless for this process's lifetime. The adoption proof no
            // longer covers them, so the outgoing process keeps its own copies. A shell
            // the PTY keeper offered is released back to it by the same drop
            // (`keeper_link::Claim`), so the keeper closes its copy too and the shell
            // is hung up rather than left running unseen.
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
                    Self::register_session(&self.store, &s, None);
                    let tree = pane::PaneTree::new(id);
                    let tab = crate::register_terminal_tab(
                        &mut self.tab_ids,
                        &mut self.view_store,
                        &tree,
                    )
                    .expect("restored orphan tab identity space");
                    self.pool_insert(s);
                    if let Some(ws) = self.windows.get_mut(&wid) {
                        ws.layouts.push(tree);
                        ws.tabs.add();
                        ws.tab_set.push(tab).expect("fresh restored tab id");
                    }
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
        // A newly appended tab may need the window re-mirrored (active-tab chrome, pane
        // geometry). Cheap and idempotent; the front window is the one that changed.
        if self.frontmost_window == Some(wid) {
            self.sync_active_session();
        } else {
            self.sync_window(wid);
        }
    }

    /// Re-adopt one orphaned handed-off shell as session `id` of window `wid`
    /// — [`Self::adopt_orphan_shells_as_tabs`]'s spawn.
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
            // The adoption `spawn_session` performs below, as far as a stub can:
            // the session's sink takes the handed master over through the same
            // `HandedMaster::into_sink` (so it closes it when the session drops),
            // the id it records from the `Adopted` handle, and whether the shell's
            // PATH is frozen (2026-09-16) — what `register_session` marks on the
            // registry, so a test can pin that adoption reaches the count.
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
            // CELL-PX-1: the host window's real cell box for the newborn engine.
            self.spawn_cell_px(wid),
            &self.session_factory,
            &proxy,
            None,
            None, // not a connected controller spawn
            None, // no worker's task
            Some(adopted),
        )
    }

    /// Fill window `wid` from a validated mixed-tab restore record. Terminal trees keep
    /// their RESTORE-1 compatibility projection; native descriptors mint fresh runtime
    /// identities and are interleaved afterward by stable kind/URI order.
    pub(crate) fn restore_into_window(&mut self, wid: WindowId, wl: restore::WindowLayout) {
        if !wl.restored_tabs.is_empty() {
            self.restore_recursive_into_window(wid, wl);
            return;
        }
        let Some(order) = wl.canonical_order() else {
            crate::logging::stderr_line!(
                "aterm-gui: session restore: invalid mixed-tab ordering; keeping bootstrap"
            );
            return;
        };
        let active_item = wl.canonical_active(&order);
        let restore::WindowLayout {
            active_tab: _,
            outer_x: _,
            outer_y: _,
            // Applied by the caller (`apply_restore_manifest`) before the fill,
            // like the outer position above — window STATE, not tab topology.
            maximized: _,
            // Taken by the caller at the window's create (`CarriedFrame`).
            inner_px: _,
            presence_row: _,
            // Recorded by the caller before the fill and applied once every
            // carried window is on glass (`crate::window_show`).
            show: _,
            rows: _,
            cols: _,
            tabs: terminal_layouts,
            native_tabs,
            tab_order: _,
            active_item: _,
            restored_tabs: _,
        } = wl;
        let had_bootstrap_terminal = self
            .windows
            .get(&wid)
            .is_some_and(|ws| !ws.layouts.is_empty());
        let mut tabs = terminal_layouts.iter().cloned();
        // Tab 0: the window's existing session is its first (tree-order) leaf — it was
        // spawned in that leaf's cwd. A single-leaf tab 0 is already the right shape;
        // a split tab 0 rebuilds the tree around the kept session.
        if let Some(layout0) = tabs.next()
            && layout0.leaf_count() > 1
            && let Some(keep) = self
                .windows
                .get(&wid)
                .and_then(|ws| ws.layouts.first().map(pane::PaneTree::focus))
            && let Some(tree) = self.restore_build_tree(wid, &layout0, Some(keep))
        {
            for session in tree.sessions() {
                let known = self
                    .view_store
                    .iter()
                    .any(|(_, view)| view.terminal_session() == Some(session));
                if !known {
                    self.view_store
                        .insert_terminal(session)
                        .expect("restored split view identity space");
                }
            }
            if let Some(ws) = self.windows.get_mut(&wid)
                && let Some(slot) = ws.layouts.first_mut()
            {
                *slot = tree;
            }
            let synced = self.sync_tab_model_from_layout(wid, 0);
            debug_assert!(synced);
        }
        // Remaining tabs: every leaf spawns fresh in its persisted cwd.
        for layout in tabs {
            let Some(tree) = self.restore_build_tree(wid, &layout, None) else {
                crate::logging::stderr_line!(
                    "aterm-gui: session restore: could not respawn a tab; keeping what restored"
                );
                break;
            };
            let tab = crate::register_terminal_tab(&mut self.tab_ids, &mut self.view_store, &tree)
                .expect("restored tab identity space");
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.layouts.push(tree);
                ws.tabs.add();
                ws.tab_set.push(tab).expect("fresh restored tab id");
            }
        }

        // Native reopening is intentionally descriptor-driven. Each success returns the
        // freshly allocated canonical tab id; a missing file or invalid route occupies no
        // slot and cannot redirect a delayed completion to an old view generation.
        self.frontmost_window = Some(wid);
        let mut native_ids = Vec::with_capacity(native_tabs.len());
        for descriptor in &native_tabs {
            let candidate = self.restore_native_tab_into_window(wid, descriptor);
            let aliases_prior = candidate.is_some_and(|id| native_ids.contains(&Some(id)));
            if aliases_prior {
                crate::logging::stderr_line!(
                    "aterm-gui: session restore: native descriptor aliased an existing tab; skipped"
                );
                native_ids.push(None);
            } else {
                native_ids.push(candidate);
            }
        }

        // A cold native-only first window starts with the application's unavoidable
        // process bootstrap shell, and so does a handoff successor's whose window 0 has
        // no terminal leaf (`take_session0_shell`). Once at least one real native
        // descriptor has reopened, retire that terminal completely; later native-only
        // windows take the zero-session creation path above and never need this
        // conversion. A bootstrap that IS a handed-off shell is never retired.
        if terminal_layouts.is_empty()
            && had_bootstrap_terminal
            && !self.window_holds_handed_off_shell(wid)
            && native_ids.iter().any(Option::is_some)
        {
            self.remove_restore_bootstrap_terminals(wid);
        }

        let terminal_ids = self
            .windows
            .get(&wid)
            .map(|ws| {
                ws.tab_set
                    .tabs()
                    .iter()
                    .filter(|tab| {
                        matches!(
                            self.view_store.get(tab.focus),
                            Some(crate::tab_model::View::Terminal(_))
                        )
                    })
                    .map(|tab| tab.id)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let restored = order
            .iter()
            .enumerate()
            .filter_map(|(original, entry)| {
                let id = match *entry {
                    restore::TabOrderEntry::Terminal { index } => terminal_ids.get(index).copied(),
                    restore::TabOrderEntry::Native { index } => {
                        native_ids.get(index).copied().flatten()
                    }
                }?;
                Some((original, id))
            })
            .collect::<Vec<_>>();

        if let Some(ws) = self.windows.get_mut(&wid) {
            for (to, (_, id)) in restored.iter().enumerate() {
                let moved = ws.tab_set.reorder(*id, to);
                debug_assert!(moved);
            }
        }
        let active_id = restored
            .iter()
            .find_map(|(original, id)| (*original == active_item).then_some(*id))
            .or_else(|| {
                restored
                    .iter()
                    .min_by_key(|(original, _)| original.abs_diff(active_item))
                    .map(|(_, id)| *id)
            });
        if let Some(active_id) = active_id
            && let Some(index) = self
                .windows
                .get(&wid)
                .and_then(|ws| ws.tab_set.tabs().iter().position(|tab| tab.id == active_id))
        {
            self.switch_tab_in(wid, index);
        } else if self.frontmost_window == Some(wid) {
            self.sync_active_session();
        } else {
            self.sync_window(wid);
        }
        if self
            .windows
            .get(&wid)
            .is_some_and(|ws| !ws.tab_set.is_empty())
        {
            debug_assert!(
                self.structural_invariants_ok(),
                "{}",
                self.structural_invariant_violation().unwrap_or_default(),
            );
        }
    }

    /// Reconstruct the canonical RESTORE-2 tree. Native installers briefly publish a
    /// one-leaf staging tab, which is immediately detached again; the fully assembled
    /// recursive tab is the only topology ever committed to the window. Keeping every
    /// completed tab detached until the whole window is built also permits multiple
    /// independent views of the same singleton/document instance without the ordinary
    /// "focus the existing tab" affordance aliasing a restore leaf.
    fn restore_recursive_into_window(&mut self, wid: WindowId, wl: restore::WindowLayout) {
        let restore::WindowLayout {
            active_item,
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

        // A bootstrap retained for possible terminal reuse but not consumed by a valid
        // leaf is not user content on a cold native-only restore.
        if let Some((view, session)) = reusable_terminal.take() {
            self.view_store.remove(view);
            self.teardown_session(session);
        }

        let restored_ids = built.iter().map(|(tab, _)| tab.id).collect::<Vec<_>>();
        if let Some(window) = self.windows.get_mut(&wid) {
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
                // The live handoff shell is extra recovery content, not a persisted
                // position. Put every restored tab before it in its original order.
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
                        tab.root.leaves().into_iter().all(|view| {
                            matches!(
                                self.view_store.get(view),
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
        // NO ALIASED SHELL. Every leaf of a MANIFEST rebuild owns its own session:
        // the graft consumes the bootstrap exactly once, and each remaining leaf
        // forks or re-adopts a distinct one. Nothing the manifest says can put two
        // of them on one PTY, so a repeat here is two tabs pretending to be
        // separate — the failure this pass exists to make unrepresentable.
        // (Deliberate sharing does exist — `Open Session in New Window`, and
        // reopening a tab closed off a still-shared session — but it is minted from
        // LIVE ids, never rebuilt from a retired manifest.)
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

    /// True iff no live session backs two leaves of `wid` — the post-condition of a
    /// MANIFEST rebuild, checked where a stale-id graft would once have broken it.
    /// Not a standing window invariant: a deliberate live share can put one session
    /// in two leaves, which is why only the retired-id path asserts this. The
    /// one share a manifest carries is a HANDED shell its handoff layout named
    /// twice (round six, finding 10), shown again on purpose: a repeat of an
    /// adopted shell is that, never an alias of two shells.
    fn restored_window_sessions_are_distinct(&self, wid: WindowId) -> bool {
        self.windows.get(&wid).is_none_or(|window| {
            let mut seen = Vec::new();
            window.tab_set.tabs().iter().all(|tab| {
                tab.root.leaves().into_iter().all(|view| {
                    self.view_store
                        .get(view)
                        .copied()
                        .and_then(crate::tab_model::View::terminal_session)
                        .is_none_or(|session| {
                            let fresh = !seen.contains(&session)
                                || self
                                    .pool
                                    .get(session)
                                    .is_some_and(|live| live.handoff_local_id.is_some());
                            seen.push(session);
                            fresh
                        })
                })
            })
        })
    }

    /// Whether a terminal view of window `wid` shows a shell handed across a
    /// seamless update (`Session::handoff_local_id`): a live pre-update shell
    /// that must stay reachable even where the layout has no terminal slot for
    /// it. A window's bootstrap is one only when it was adopted. A successor
    /// whose window 0 has no terminal leaf spawns session 0 fresh
    /// (`take_session0_shell`), and that bootstrap is as throwaway as a cold
    /// one.
    fn window_holds_handed_off_shell(&self, wid: WindowId) -> bool {
        self.windows.get(&wid).is_some_and(|window| {
            window.tab_set.tabs().iter().any(|tab| {
                tab.root.leaves().into_iter().any(|view| {
                    self.view_store
                        .get(view)
                        .copied()
                        .and_then(crate::tab_model::View::terminal_session)
                        .and_then(|session| self.pool.get(session))
                        .is_some_and(|session| session.handoff_local_id.is_some())
                })
            })
        })
    }

    /// Remove the bootstrap projection while retaining at most one live terminal view
    /// for exact grafting into the first recursive terminal leaf. Every other ownership
    /// edge is retired before native staging begins.
    fn clear_restore_target(
        &mut self,
        wid: WindowId,
        retain_terminal: bool,
    ) -> Option<(crate::tab_model::ViewId, u64)> {
        let old_tabs = self.windows.get_mut(&wid).map(|window| {
            let old = std::mem::take(&mut window.tab_set);
            window.layouts.clear();
            window.tabs = TabIndex::new(0, 0);
            window.front_content = None;
            window.active_terminal = None;
            window.window_focus = crate::front_content::WindowFocus::Host;
            old.tabs().to_vec()
        })?;
        let reusable = if retain_terminal {
            old_tabs.iter().find_map(|tab| {
                tab.root.leaves().into_iter().find_map(|view| {
                    let crate::tab_model::View::Terminal(terminal) =
                        self.view_store.get(view).copied()?
                    else {
                        return None;
                    };
                    self.pool
                        .get(terminal.session)
                        .is_some()
                        .then_some((view, terminal.session))
                })
            })
        } else {
            None
        };
        for tab in old_tabs {
            for view in tab.root.leaves() {
                if reusable.is_some_and(|(kept, _)| kept == view) {
                    continue;
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
            }
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
                self.rollback_recursive_restore_leaves(&leaves, reusable_before);
                if reusable_before
                    .is_some_and(|(view, _)| leaves.iter().any(|(created, _, _)| *created == view))
                {
                    *reusable_terminal = reusable_before;
                }
                return Err("tab identity space exhausted".to_string());
            }
        };
        Ok((
            crate::tab_model::Tab::from_root(tab_id, root, focus, record.zoomed, presentation),
            pane_tree,
        ))
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
                let (view, presentation, staging_tab) =
                    self.build_recursive_restore_leaf(wid, view, reusable_terminal, ids)?;
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
                            // Restored fresh: any prior connection died with
                            // the old process's tables (§1.4#6).
                            conn: None,
                            closable: true,
                            tooltip: terminal.cwd.as_ref().map(|cwd| format!("Terminal · {cwd}")),
                        },
                        None,
                    )),
                    Err(error) => self.restore_recovery_leaf(
                        wid,
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
            restore::RestoredView::Native(native) => match self.restore_native_leaf(wid, native) {
                Ok(restored) => Ok(restored),
                Err(error) => {
                    // The view is not reopened, so neither is any draft it
                    // carried: said, with the text, rather than dropped — once
                    // the restore settles.
                    self.carried_settings_drafts
                        .lost
                        .extend(native.settings_drafts.iter().cloned());
                    self.carried_settings_drafts.unreadable = self
                        .carried_settings_drafts
                        .unreadable
                        .saturating_add(native.settings_drafts_unreadable);
                    self.restore_recovery_leaf_with_capability(
                        wid,
                        &restore::PlaceholderLeafRestore {
                            restore_tag: native.restore_tag.clone(),
                            reason: Self::bounded_recovery_text(&error),
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
            },
            restore::RestoredView::Placeholder(placeholder) => {
                // A pane the outgoing process kept open after its command
                // exited, whose final screen crossed (round five, item 19):
                // shown read-only where its placeholder stands. A pane open in
                // two windows names one held pane at both leaves: the first
                // shows its screen, and every later one shows that same
                // session again, as a co-viewed live shell's later leaves do
                // (round eight, finding 59).
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
                self.restore_recovery_leaf(wid, placeholder)
            }
        }
    }

    /// Remove and return the held pane's screen the outgoing process knew as
    /// `local_id`, when it is still waiting for its placeholder (round five,
    /// item 19). Each is taken once.
    fn take_held_pane(&mut self, local_id: u64) -> Option<crate::seamless::HeldPane> {
        let index = self
            .seamless_held
            .iter()
            .position(|held| held.local_id == local_id)?;
        Some(self.seamless_held.remove(index))
    }

    /// The pool session an earlier leaf of this restore made of the held pane
    /// the outgoing process knew as `local_id` ([`Self::restore_held_leaf`]):
    /// the pane is co-viewed, and this leaf is a later view of it (round
    /// eight, finding 59).
    fn held_shown_in_this_restore(&self, local_id: u64) -> Option<u64> {
        self.seamless_held_shown
            .iter()
            .find(|(held, _)| *held == local_id)
            .map(|(_, session)| *session)
            .filter(|session| self.pool.get(*session).is_some())
    }

    /// A LATER VIEW OF A CO-VIEWED HELD PANE (round eight, finding 59): the
    /// session the first leaf restored, shown again — one more view of it, as
    /// `Open Session in New Window` made it, and as a co-viewed live shell's
    /// later leaves show the adopted shell again — rather than the 'not
    /// carried' placeholder for a screen that did cross.
    fn show_held_pane_again(&mut self, session: u64) -> Result<BuiltRestoreLeaf, String> {
        self.pool.attach(session);
        let Ok(view) = self.view_store.insert_terminal(session) else {
            self.detach_session_view(session);
            return Err("terminal view identity space exhausted".to_string());
        };
        Ok((view, self.held_pane_presentation(session), None))
    }

    /// A held pane's tab: its title as its final screen set it, closable.
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

    /// A HELD PANE'S TERMINAL (round five, item 19): the pane the outgoing
    /// process kept open after its command exited, back with its final screen,
    /// read-only — a session with no PTY and no process
    /// ([`crate::held_pane_session`]), registered and marked `Exited` as the
    /// pane it stands for was, so the next update carries it the same way and
    /// the session verbs report it exited. Closing it is closing any exited
    /// pane.
    ///
    /// SHOWN AS THE PANE IT STANDS FOR (round six of the update audit,
    /// findings 46, 49 and 35): its engine takes this build's terminal config
    /// and appearance, as every pooled engine does
    /// ([`crate::held_pane_session`]); its exit reaches the status observer
    /// the way a live pane's does ([`Self::note_session_exit`], with the exit
    /// status the outgoing process carried), so its tab and `status` say
    /// exited rather than idle — as a carried exit, not news
    /// ([`Self::note_carried_exit`]; round eight, U34); and `leaf` — the
    /// terminal leaf its placeholder retired — gives it back its user identity
    /// (the `meta set` fields), so the next update
    /// carries them on.
    /// No agent the leaf names is relaunched: nothing runs in a held pane.
    fn restore_held_leaf(
        &mut self,
        held: crate::seamless::HeldPane,
        leaf: &restore::TerminalLeafRestore,
    ) -> Result<BuiltRestoreLeaf, String> {
        let id = self.next_session_id;
        // ITS ADDRESS GOES ON (round eight, finding 21): the sid the outgoing
        // process knew the pane by, as an adopted shell keeps its own — so
        // `aterm ctl @<sid>` read/status reach it after the update — unless
        // a session here already answers to it.
        let sid = held
            .sid
            .filter(|sid| {
                self.store
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .by_sid(sid)
                    .is_none()
            })
            .unwrap_or_else(aterm_session::SessionId::generate);
        let mut session = crate::held_pane_session(
            id,
            sid,
            &held.checkpoint,
            self.session_factory.terminal_config.as_ref(),
        );
        if let Some(exit) = held.exit {
            let _ = session.child_exit.set(exit);
        }
        // ITS SCROLLBACK CROSSES AS AN ADOPTED SESSION'S DOES (round eight,
        // finding 39): the sidecar opened before the proof is imported in
        // front of the restored screen after Commit, by the same worker
        // (`App::start_handoff_history_imports`), its keys reserved here
        // under the engine's first lock, before anything else reads it.
        // What did not cross is said on the band with the handed tabs' and
        // counted on `status`.
        session.handoff_history = crate::held_pane_history(&session.term, held.history);
        // The block the shell last reported, read while the engine is still
        // this function's alone (no reader runs in a held pane, and it is not
        // pooled yet): the carried exit names it, so the sweeps that read it
        // again are not a new failure (round eight, U34's review).
        let carried_block =
            crate::session_status::completed_block(&crate::term_lock(&session.term));
        // ITS TURN LEDGER AND ARCHIVE TAIL CROSS AS AN ADOPTED SESSION'S DO
        // (round eight, finding 730): `history`, the report reader and a
        // resumed `since-turn=` still read the turns a manager drove into the
        // pane before its command ended. A pane the outgoing process carried
        // none for (an older one, or a carry it left out) starts with a ledger
        // that names the turn ids it cannot vouch for, so its empty `history`
        // never reads as a session nobody drove.
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
        let view = self
            .view_store
            .insert_terminal(id)
            .map_err(|_| "terminal view identity space exhausted".to_string())?;
        self.next_session_id = self.next_session_id.saturating_add(1);
        Self::register_session(&self.store, &session, None);
        self.store
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .set_state(id, crate::session_store::SessionState::Exited);
        self.pool_insert(session);
        self.carry_restored_theme(id, leaf);
        // The timer it carried, indexed from what was stored (design §9.3.6,
        // item 2), as every restore path indexes its pair.
        self.index_autopilot_timer(id, Some(crate::autopilot_timers::ClosedOver::Aterm));
        self.seamless_held_shown.push((held.local_id, id));
        // The exit it carried, shown as the status it is — not news, and as
        // seen as the person left it (round eight, U34).
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
        // `meta set` in the previous process exists only in this leaf. Returning
        // the view without it is how the 0.82.0 -> 0.83.0 handoff (measured
        // 2026-09-12) came back with `user_title=- description=- icon=- role=-
        // attention=-` on BOTH of its sessions: two windows, one terminal each,
        // so every session was a graft, and only a leaf that spawned or
        // re-adopted a session below was ever re-seeded. Which shell the leaf's
        // identity lands on is `carry_restored_identity`'s business, and what it
        // may overwrite there is `graft_restored_user_meta`'s.
        if let Some((view, session)) = reusable_terminal.take() {
            self.carry_restored_identity(terminal, FillingShell::Registered(session), ids);
            self.carry_restored_theme(session, terminal);
            return Ok(view);
        }
        // A SESSION THE HANDOFF LAYOUT NAMES TWICE (round six, finding 10): one
        // shell `Open Session in New Window` showed in two views. The first leaf
        // rebuilt that names it adopted it; this one shows the same session
        // again — a second view, as the menu command made it — rather than a
        // stand-in shell, a placeholder, or (as before) no layout at all. The
        // leaf's identity lands on that shell, as the first leaf's did (the
        // agent it may owe is queued once per shell), and every tab already
        // showing the shell — rebuilt before this leaf — repaints with it.
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
        // RE-ATTACH ONLY TO AN ID THIS PROCESS MINTED. A closed-tab record names a
        // session of OURS, and ids are never reused within a run, so a pool hit is
        // the very shell that tab was showing — reopening a tab closed off a shared
        // session must join it again rather than fork a stranger.
        //
        // A RETIRED manifest gets no such lookup. Its ids are the previous
        // process's namespace and this one re-mints from 0, so a hit is never
        // recognition — it is a COLLISION with a shell this same restore pass just
        // forked. Attaching to it aliased two tabs onto one PTY (typing in one
        // appeared in the other) and silently dropped the saved shell the leaf
        // stood for. No exotic layout is needed to reach it: capture walks tabs in
        // tree order, so a window of `[0] [1|3] [2]` persists the descending run
        // `0,1,3,2`, and by the time leaf `2` is rebuilt the pass has already
        // minted ids 1 and 2. The one live thing such a manifest can name is a
        // handed-off fd, matched through `seamless_adopt` in the spawn below —
        // which the collision could also hijack, stranding the real shell in the
        // adopt list for the orphan net to re-home as a stray tab.
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
            let mut session = crate::stub_session(id);
            // The adoption `spawn_session` performs below, as far as a stub can:
            // the same match, the id it records from the `Adopted` handle,
            // whether the shell's PATH is frozen (2026-09-16), and the fabric
            // its handle carried — a PTY keeper's META hold among it — through
            // the same seed, before the leaf's identity is carried.
            if let Some(shell) = take_handed_off_shell(&mut self.seamless_adopt, terminal.local_id)
            {
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
        // A cold restore's shell starts on the side of the theme its leaf will
        // draw (`carry_restored_theme`, after the spawn), as a booked tab's does.
        // An adopted shell keeps the env it has.
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
            // CELL-PX-1: the host window's real cell box for the newborn engine.
            self.spawn_cell_px(wid),
            factory,
            &proxy,
            terminal.start_cwd().as_deref(),
            None, // not a connected controller spawn
            None, // no worker's task
            adopt,
        )
        .map_err(|error| error.to_string())?;
        // A new shell for a restored leaf: the session log opens it `restore
        // prev= lineage=` (`carry_restored_lineage`, under this call).
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

    /// Give session `id` the theme its restored leaf carried, as origin
    /// `restored`, and install it. A selector that no longer draws is kept:
    /// the session draws the instance theme and says why until it does.
    fn carry_restored_theme(&mut self, id: u64, leaf: &restore::TerminalLeafRestore) {
        if let Some(selector) = leaf.theme.clone() {
            self.set_session_theme(
                id,
                Some(crate::session_theme::SessionTheme {
                    selector,
                    origin: crate::session_theme::SessionThemeOrigin::Restored,
                }),
            );
        }
    }

    /// The pool session a handoff successor already adopted for the shell
    /// `leaf` names (`Session::handoff_local_id`), when this is a handoff
    /// layout's leaf ([`LeafIds::Retired`]): the leaf is a second view of a
    /// co-viewed session (round six, finding 10). `None` for a cold restore,
    /// whose leaves name no handed shell, and for the first leaf naming it.
    fn adopted_in_this_restore(
        &self,
        leaf: &restore::TerminalLeafRestore,
        ids: LeafIds,
    ) -> Option<u64> {
        let local_id = leaf.local_id.filter(|_| ids == LeafIds::Retired)?;
        self.adopted_handoff_session(local_id)
    }

    /// The pool session adopted, on this handoff successor, for the shell the
    /// outgoing process knew as `local_id`.
    fn adopted_handoff_session(&self, local_id: u64) -> Option<u64> {
        if !self.handoff_successor {
            return None;
        }
        self.pool
            .iter()
            .find(|session| session.handoff_local_id == Some(local_id))
            .map(|session| session.id)
    }

    /// Whether manifest window `wl` is rebuilt into an EMPTY logical host —
    /// no bootstrap session — rather than around one: a window of native
    /// views only, or (round six, finding 10) a handoff window whose
    /// bootstrap leaf names a shell an earlier window already adopted. That
    /// window co-viewed the session; forking a bootstrap for it would put a
    /// stand-in shell where the co-view was, while the overlap forbids a
    /// fresh spawn. Its leaves are all rebuilt by the leaf builders, which
    /// show the adopted shell again ([`Self::adopted_in_this_restore`]).
    pub(crate) fn restores_without_bootstrap(&self, wl: &restore::WindowLayout) -> bool {
        let native_only = if wl.restored_tabs.is_empty() {
            wl.tabs.is_empty() && !wl.native_tabs.is_empty()
        } else {
            wl.restored_tabs
                .iter()
                .all(|tab| tab.root.first_terminal_leaf().is_none())
        };
        native_only
            || (!wl.restored_tabs.is_empty()
                && wl
                    .bootstrap_local_id()
                    .and_then(|local_id| self.adopted_handoff_session(local_id))
                    .is_some())
    }

    /// Re-seed a freshly-respawned session's USER metadata (`meta set` fields)
    /// from its restore leaf, BEFORE the session is registered/visible — so a
    /// restored tab reappears under the operator-chosen title (which outranks
    /// the OSC title in tab labels) with its description/icon intact. A leaf
    /// with no captured metadata is a no-op (no lock churn on the common path).
    ///
    /// The Autopilot word and its timer are stored as ONE step
    /// (`SessionMeta::set_autopilot`; design §9.3.6, item 2), and the caller
    /// indexes the timer ([`Self::index_autopilot_timer_of`]): a time that
    /// passed while aterm was closed lands at once.
    fn seed_restored_user_meta(session: &crate::Session, leaf: &restore::TerminalLeafRestore) {
        if leaf.user_title.is_none()
            && leaf.description.is_none()
            && leaf.icon.is_none()
            && leaf.role.is_none()
            && leaf.attention.is_none()
            && leaf.questions.is_none()
            && leaf.autopilot.is_none()
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
        let _ = meta.set_autopilot(leaf.autopilot.clone(), leaf.carried_land_at());
        // Who set it, beside it (§11.7.3, item 5): none where the word was
        // not stored (one this build cannot read), and no revision steps,
        // since the write it names was made in another process.
        meta.autopilot_set_by = leaf.carried_set_by().filter(|_| meta.autopilot.is_some());
        drop(meta);
        // A worker an older build started, its identity naming no word.
        crate::session_timeline::keep_worker_autopilot(&session.ctx);
    }

    /// Put `leaf`'s USER identity (its six `meta set` fields) on the shell
    /// the leaf NAMES — on a handoff, not necessarily the session `filling` its
    /// pane. A peer reads a session's identity before typing into it: the
    /// `role=` it checks (the label the status item names its tab by) and
    /// the `attention=` it answers (a need the status item counts). So the
    /// identity must describe the process that runs under the sid it is read
    /// from.
    ///
    /// * A COLD restore's leaf, or one from this process's own closed-tab
    ///   ledger ([`LeafIds::Live`]), names no running shell to look for. The
    ///   session filling the pane becomes that leaf's shell — a cold bootstrap
    ///   exists to become the shell of the pane it fills, and a respawn stands
    ///   for the tab that was closed — so it takes the identity.
    /// * A HANDOFF layout's leaf names a handed-over shell by the id the
    ///   outgoing process knew it by, and the live session adopted under that
    ///   id (`Session::handoff_local_id`) takes the identity, wherever it runs.
    ///   Whenever the layout is honored that is the session filling the pane:
    ///   each shell is adopted by the leaf that names it, or, for a window's
    ///   bootstrap leaf, as that window's bootstrap ([`take_session0_shell`]
    ///   for window 0, `apply_restore_manifest` for the rest). It is not when
    ///   a tab failed to build and handed its window's bootstrap on to the
    ///   next terminal leaf, whose pane another adopted shell then fills — nor
    ///   were a shell ever taken before its own leaf, when a fresh STAND-IN
    ///   would fill it, with its own sid and none of the named shell's
    ///   history. Neither takes the leaf's identity. The named shell does: at
    ///   once, through the live door, if it already runs here; otherwise it is
    ///   still waiting in `seamless_adopt`, and the orphan net puts the
    ///   identity on it when it places it
    ///   ([`Self::adopt_orphan_shells_as_tabs`]). One stderr line reports the
    ///   mismatch, and names where the identity went only when it moved or is
    ///   still to move.
    fn carry_restored_identity(
        &mut self,
        leaf: &restore::TerminalLeafRestore,
        filling: FillingShell<'_>,
        ids: LeafIds,
    ) {
        self.carry_restored_lineage(leaf, filling);
        // The held tab's entry this leaf added (`restored_held`), whose hold
        // rows are written once the owner's is put back, below.
        let mut held_at = None;
        // THE AGENT THE LEAF HOSTED (2026-09-27): a cold restore of the live
        // layout names the agent a crash of aterm took from this tab; the
        // session that fills the leaf is where it is relaunched, once the
        // pass is over ([`Self::hand_restored_agents`]).
        //
        // ON A SEAMLESS HANDOFF (round four, plan item 7) the leaf names one
        // only when the outgoing instance's cold-restore queue still owed it
        // at the park — an older producer never writes one on a handoff leaf,
        // so `Some` means exactly that. It is owed to the ADOPTED shell the
        // leaf names (where the outgoing instance was relaunching it), found
        // by that name at this process's Commit
        // ([`Self::hand_carried_restored_agents`]): never to a stand-in, and
        // never before the Commit, while the outgoing instance still owns the
        // sessions.
        if ids == LeafIds::Retired
            && let Some(agent) = &leaf.agent
        {
            // A leaf the PTY KEEPER's recovery filled with the very shell
            // that ran the agent (P3: an adopted shell on a cold restore) still
            // runs it: relaunching would type the agent's command into it.
            // So does a co-viewed session's other leaf, rebuilt before this
            // one and given that shell (restore audit, 2026-10-02): relaunched
            // here, one conversation ran in two tabs. (No shell is adopted on
            // a cold restore without the keeper.)
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
            if !self.handoff_successor && filled_by_a_live_shell {
                aterm_log::info!(
                    "restore: the agent of a reattached shell is still running; not relaunched"
                );
            } else if !self.handoff_successor {
                let session = match &filling {
                    FillingShell::Registered(session) => *session,
                    FillingShell::Unregistered(session) => session.id,
                };
                // A HELD TAB'S AGENT IS THE HOLDER'S (LC-8, ruling 23): never
                // relaunched, said once the pass is over.
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
        // THE AGENT THE OUTGOING INSTANCE'S HOST SUPERVISED in the shell the
        // leaf names (round eighteen; a handoff leaf alone, where nothing is
        // owed): watched by this process's host from its Commit
        // ([`Self::hand_carried_restored_agents`]), whose exit nobody else
        // will act on — never before the Commit, while the outgoing instance
        // still owns the sessions.
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
        // THE OUTGOING HOST'S WORD THAT THE TAB'S AGENT CANNOT BE BROUGHT
        // BACK (LC-4 of ATERM-KI-1016-1; a handoff leaf alone): stood again
        // by this process's host at its Commit, before its first pass
        // ([`Self::adopt_carried_lost_words`]).
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
        // THE OWNER'S HOLD COMES BACK ON THE TAB (LC-8, ruling 23): a cold
        // restore's leaf carries the tab's holds
        // ([`restore::TerminalLeafRestore::holds`]), never a handoff's, whose
        // session record carries every hold. The owner's slot alone is put
        // back; a fleet's is the process's ([`crate::fabric`]'s header). Onto
        // a FRESH shell standing for the tab, and onto the very session the
        // leaf names when the PTY keeper gave it back — which carries its own
        // holds, both origins, from its keeper META
        // (`spawn::seed_adopted_fabric`, before it was registered): the
        // owner's is added where that carry left the slot empty, since
        // neither sample can be ordered before the other, and a slot that
        // stands takes nothing and records nothing
        // ([`crate::fabric::SessionFabric::seed_holds`]). A kept shell that
        // fills a leaf naming another session keeps its own carry alone. A
        // REGISTERED shell (a window's bootstrap shell the restore grafts onto
        // the leaf) takes the hold's own path instead
        // ([`crate::fabric::carry_hold`], the review of LC-8): seeded, it
        // skipped a committing update's fence and the band's wake. A slot that
        // stands there takes nothing, as a seed's does.
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
        // …AND THE HOLD ROWS IT COULD NOT PUT BACK ride the held tab's agent
        // (the merge review's GUI-M7-1): a fleet's, never put back on a fresh
        // or bootstrap shell, and the owner's where a committing update's
        // fence refused it ([`crate::fabric::carry_hold`]). The leaf an end
        // inside the drain writes names the agent
        // ([`Self::unhanded_restored_agent`]) beside them
        // ([`Self::unhanded_restored_holds`]), so the next launch keeps the
        // tab held and the agent down; read off the session alone, the leaf
        // named the agent with no hold, and the next launch relaunched it. A
        // row whose slot stands on the session is the session's to carry: an
        // owner's hold put back and then lifted stays lifted.
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
                // Its window's own rebuild repaints the chrome that shows it.
                FillingShell::Registered(session) => {
                    self.graft_restored_user_meta(session, leaf);
                }
                FillingShell::Unregistered(session) => {
                    Self::seed_restored_user_meta(session, leaf);
                    // Its timer, from what was just stored (design §9.3.6,
                    // item 2): a time that passed meanwhile is due at once —
                    // while aterm was closed (a restore of a retired
                    // process's manifest), or only the tab, with aterm
                    // running (this process's own closed-tab ledger).
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
                    // That shell's window may be rebuilt and showing it already,
                    // so fan the change out the way a driver's `meta set` does
                    // (`Wake::MetaChanged`): its tab chrome and the operator item.
                    self.refresh_meta_dependent_chrome(named);
                    self.mark_status_dirty();
                    "; that pane's identity went to the shell it names, which runs in another pane"
                } else {
                    // Nothing moved: the leaf carries no identity, the shell
                    // already wears it, or a driver wrote every field it differs in.
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

    /// THE LINEAGE A LEAF NAMES (session log phase 2A), on the shell filling
    /// it. The shell the leaf names (its own sid, or the handed shell its
    /// `local_id` names: a seamless update's adoption, the keeper's
    /// recovery) is the same session going on, so it only keeps the leaf's
    /// lineage. Any other shell that never ran elsewhere (a cold restore's
    /// new shell, a stand-in, a window's bootstrap filling the leaf) stands
    /// in for the session the leaf was: its log says `restore prev=<that
    /// sid> lineage=<the chain's first>`. An adopted shell the leaf does not
    /// name is its own session and takes nothing; the leaf's lineage goes to
    /// the shell it names if that one runs here.
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
        if named(shell) {
            shell
                .ctx
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .slog_mut()
                .seed_lineage(leaf.lineage.as_deref());
        } else if shell.handoff_local_id.is_none() {
            // A bootstrap already served under its sid may have readers: an owed
            // log wake rang its watchers under the timeline lock.
            let _ = note_restored(shell, leaf.sid.as_deref(), leaf.lineage.as_deref());
        } else if let Some(other) = leaf
            .local_id
            .and_then(|l| self.adopted_session(l))
            .and_then(|s| self.pool.get(s))
        {
            other
                .ctx
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .slog_mut()
                .seed_lineage(leaf.lineage.as_deref());
        }
    }

    /// The live session adopted as the shell the outgoing process knew as
    /// `local_id`, if one is running here.
    fn adopted_session(&self, local_id: u64) -> Option<u64> {
        self.pool.sessions.iter().find_map(|(&session, pooled)| {
            (pooled.session.handoff_local_id == Some(local_id)).then_some(session)
        })
    }

    /// Put `leaf`'s USER identity on LIVE session `session`, the shell
    /// [`Self::carry_restored_identity`] found the leaf names. Unlike
    /// [`Self::seed_restored_user_meta`]'s session, this one is registered and
    /// served under its sid before the deferred pass reaches the leaf: a
    /// window's bootstrap, or a named shell running in another pane. On a
    /// handoff the child serves session 0's sid from its control-socket bind in
    /// `main_entry`, which repoints `aterm.sock` and the sid's graph entry at
    /// itself before `first_present_done` lets this pass run. So a reader may
    /// have seen the session without its identity, and a driver may have
    /// written it. Two rules follow.
    ///
    /// * A DRIVER'S WRITE WINS. A field `meta set`, `meta unset` or the GUI
    ///   rename wrote on this process keeps that value
    ///   ([`crate::session_timeline::restore_carried_meta`]). It was answered
    ///   `OK` after the leaf was captured; the parent's Commit-time comparison
    ///   cannot see it, so nothing else would keep it.
    /// * Every other field takes the leaf's value exactly — a field the leaf
    ///   does not carry is stored unset, which is what clears an identity an
    ///   earlier graft stamped on a cold bootstrap before that tab failed to
    ///   build — and each move is an ordinary `meta-change`: recorded on the
    ///   timeline (which also moves `composed_session_chrome`'s `high_id` key,
    ///   so the tooltip the first present cached is recomposed) and pushed to
    ///   the session's `events` watchers.
    ///
    /// Returns whether any field moved.
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
        // A worker an older build started, its identity naming no word.
        moved |= crate::session_timeline::keep_worker_autopilot(&ctx);
        // A carried word is no move into Land: the landing began before this
        // process (the A/B book's time from Land to landed, §11.6.4).
        self.harness_mode_carried(session, lands);
        // Its timer, from the pair now stored (design §9.3.6, item 2): the
        // carried one, or a driver's written here first.
        self.index_autopilot_timer(session, Some(crate::autopilot_timers::ClosedOver::Aterm));
        moved
    }

    fn restore_native_leaf(
        &mut self,
        wid: WindowId,
        descriptor: &restore::NativeLeafRestore,
    ) -> Result<BuiltRestoreLeaf, String> {
        // Settings ▸ Harness brought back reads the ledgers and the other
        // processes' A/B books once, as its arrival does.
        let mut reads_harness = false;
        match descriptor.restore_tag.as_str() {
            "system-state" => {
                self.stage_system_state_tab(wid)
                    .map_err(|why| format!("System State could not be reopened: {why:?}"))?;
            }
            "settings" => {
                // Every route is a page, Manual included: going to one opens
                // nothing, so each restores as it was captured.
                let route = descriptor
                    .route
                    .as_deref()
                    .and_then(crate::native_settings::SettingsRoute::from_path)
                    .ok_or_else(|| "Settings route is unavailable".to_string())?;
                self.stage_settings_restore_view(wid, route)?;
                // Tab Control needs none: its fleet sweep reads at the first
                // wake that sees it shown (`App::service_vitals`).
                reads_harness = route == crate::native_settings::SettingsRoute::Harness;
            }
            "markdown" | "editor" => {
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
                    self.ensure_and_open_config_editor_in_window(wid)?;
                } else {
                    self.stage_document_restore_view(wid, kind, uri)?;
                }
            }
            _ => return Err("This app is unavailable in this build".to_string()),
        }
        let (view, mut presentation, staging_tab) = self.detach_restore_staging_tab(wid)?;
        if !self.apply_native_view_restore(view, descriptor) {
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
        if reads_harness && !self.incoming_handoff_pending {
            // THE RESTORED PAGE READS AS AN OPENED ONE (the post-landing
            // check's behaviour-2): staging only navigates the view, which
            // asks the host for nothing, so without this the page drew this
            // process's live book alone — `Ends: none yet` over a book on
            // disk that holds the done — until a Refresh. One off-loop
            // reading, the Refresh arm's (`execute_native_harness`); a
            // second restored page joins the one running. Not in an update's
            // successor before its Commit, which drops the reading's wake
            // (`drops_wake_before_commit`) and would leave it running for
            // good, so that no later reading begins: its Commit reads
            // instead (`App::read_harness_pages_at_commit`).
            self.start_harness_stats_refresh();
            self.publish_native_harness_state();
        }
        Ok((view, presentation, Some(staging_tab)))
    }

    /// Put the unsaved Settings drafts a seamless update carried
    /// ([`restore::NativeLeafRestore::settings_drafts`]) back into the view the
    /// restore just reopened, and SAY which could not be reopened — a key this
    /// build no longer edits as text, or a carry it could not read
    /// (`settings_drafts_unreadable`). A draft that does not land costs that
    /// draft, named in the message center with its text; never the view and
    /// never the handoff (the lenient consumer, plan P2-2). Returns whether any
    /// draft landed, i.e. whether the view's presentation changed.
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

    /// THE RESTORE IS DONE: say which carried Settings drafts did not survive
    /// it — ONCE, in one row, each with its text, so the person can type it
    /// again (see [`CarriedDraftsLedger`] for why not leaf by leaf). A draft
    /// that landed survived only if its view is still here and still holds
    /// exactly that text; a view the restore closed after it took its drafts
    /// took them with it. Silent when nothing was lost. Drains the ledger, so a
    /// second call says nothing again.
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

    /// The loss half of [`Self::settle_carried_settings_drafts`]: every
    /// carried draft that did not survive the restore is said, once, with its
    /// text. Silent when nothing was lost.
    ///
    /// SAVED WHOLE FIRST (round eight of the update audit, lead U61): the row
    /// cannot hold what the person typed — each line is cut to the band's
    /// line cap with its newlines and tabs gone, and the drafts past
    /// `SETTINGS_DRAFTS_SHOWN` are only counted — and the carry is consumed, so
    /// every lost draft goes to a file beside `messages.log`
    /// ([`save_lost_settings_drafts`]), which the row and the log name. With no
    /// messages log there is no folder of this process's records to put it in,
    /// and the row says what it can, as before.
    ///
    /// NOT BEFORE COMMIT (the review of U61): a seamless successor settles its
    /// restore before the proof and the Commit, while its parent still holds
    /// every one of these drafts, and a successor stood down then must have
    /// written nothing ([`Self::message_persist_frozen`]). There the loss is kept
    /// ([`CarriedDraftsLedger::owed`]) and said, file and row, at Commit
    /// ([`Self::say_carried_settings_drafts_loss`]).
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

    /// Whether this successor settled a loss of carried Settings drafts that it
    /// says at Commit ([`CarriedDraftsLedger::owed`]): the conformance rig's
    /// projection of the model's `Surfaced` before Commit.
    #[cfg(all(test, unix))]
    pub(crate) fn carried_settings_drafts_loss_owed(&self) -> bool {
        let owed = &self.carried_settings_drafts.owed;
        !owed.lost.is_empty() || owed.unreadable > 0
    }

    /// AT COMMIT (`App::release_successor_commit_freeze`): the carried Settings
    /// drafts this successor's restore could not put back
    /// ([`CarriedDraftsLedger::owed`]) are its to lose now — the parent that held
    /// them is gone — so they are saved whole and said, as a restore settled
    /// after Commit says them at once. Silent when nothing was lost.
    #[cfg(unix)]
    pub(crate) fn say_carried_settings_drafts_loss(&mut self) {
        let OwedDraftsLoss { lost, unreadable } =
            std::mem::take(&mut self.carried_settings_drafts.owed);
        self.report_unreopened_settings_drafts(&lost, unreadable);
    }

    fn stage_settings_restore_view(
        &mut self,
        wid: WindowId,
        route: crate::native_settings::SettingsRoute,
    ) -> Result<(), String> {
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
            self.install_native_tab(wid, instance, view_state, presentation)
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
            self.install_new_native_tab(wid, app, view_state, presentation)
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
        let existing = self.document_store.id_for_uri(uri).and_then(|document| {
            self.native_runtime
                .instance_for_document(kind, document)
                .map(|instance| (document, instance))
        });
        let Some((document, instance)) = existing else {
            return self.open_document_tab_in_window(wid, kind, uri).map(|_| ());
        };
        let title = match self.native_runtime.app(instance) {
            Some(crate::native_app::NativeApp::Markdown(app)) => app.title.clone(),
            Some(crate::native_app::NativeApp::Editor(app)) => app.title.clone(),
            _ => return Err("document restore instance changed kind".to_string()),
        };
        let state = match kind {
            crate::native_app::AppKind::Markdown => crate::native_app::AppViewState::Markdown(
                crate::native_app::MarkdownViewState::default(),
            ),
            crate::native_app::AppKind::Editor => {
                crate::native_app::AppViewState::Editor(Box::default())
            }
            crate::native_app::AppKind::Settings
            | crate::native_app::AppKind::SystemState
            | crate::native_app::AppKind::Recovery => {
                return Err("restore descriptor is not a document app".to_string());
            }
        };
        let (tab, view) = self
            .install_native_tab(
                wid,
                instance,
                state,
                crate::tab_model::TabPresentation {
                    title,
                    icon: Some(kind.icon()),
                    indicators: crate::tab_model::TabIndicators::default(),
                    conn: None,
                    closable: true,
                    tooltip: Some(format!("{} · {uri}", kind.as_str())),
                },
            )
            .map_err(|error| {
                aterm_log::warn!("document restore view could not be installed: {error:?}");
                "This document could not be reopened".to_string()
            })?;
        if let Err(error) = self.attach_document_view(kind, document, view) {
            let removed = self
                .windows
                .get_mut(&wid)
                .and_then(|window| window.tab_set.remove(tab));
            if let Some(tab) = removed {
                self.remove_tab_views(&tab);
            }
            self.resync_after_restore_candidate(wid);
            return Err(error);
        }
        self.refresh_native_presentation(wid, instance, view);
        Ok(())
    }

    fn restore_recovery_leaf(
        &mut self,
        wid: WindowId,
        placeholder: &restore::PlaceholderLeafRestore,
    ) -> Result<BuiltRestoreLeaf, String> {
        self.restore_recovery_leaf_with_capability(wid, placeholder, None)
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
        wid: WindowId,
        placeholder: &restore::PlaceholderLeafRestore,
        capability: Option<crate::native_app::RecoveryCapability>,
    ) -> Result<BuiltRestoreLeaf, String> {
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
            wid,
            crate::native_app::NativeApp::Recovery(crate::native_app::RecoveryApp {
                restore_tag: placeholder.restore_tag.clone(),
                reason: placeholder.reason.clone(),
                metadata: placeholder.metadata.clone(),
                capability,
            }),
            crate::native_app::AppViewState::Recovery(
                crate::native_app::RecoveryViewState::default(),
            ),
            presentation,
        )
        .map_err(|error| format!("Recovery view could not be installed: {error:?}"))?;
        self.detach_restore_staging_tab(wid)
            .map(|(view, presentation, tab)| (view, presentation, Some(tab)))
    }

    fn detach_restore_staging_tab(
        &mut self,
        wid: WindowId,
    ) -> Result<
        (
            crate::tab_model::ViewId,
            crate::tab_model::TabPresentation,
            crate::tab_model::TabId,
        ),
        String,
    > {
        let tab_id = self
            .windows
            .get(&wid)
            .and_then(|window| window.tab_set.active_id())
            .ok_or_else(|| "native staging tab did not become active".to_string())?;
        let tab = self
            .windows
            .get_mut(&wid)
            .and_then(|window| window.tab_set.remove(tab_id))
            .ok_or_else(|| "native staging tab disappeared".to_string())?;
        let views = tab.root.leaves();
        let [view] = views.as_slice() else {
            self.remove_tab_views(&tab);
            self.resync_after_restore_candidate(wid);
            return Err("native staging tab was not a single leaf".to_string());
        };
        let staged = (*view, tab.presentation, tab.id);
        // Installing the staging tab publishes it as front content. Once the tab
        // is detached into an unmounted restore leaf, immediately republish the
        // surviving canonical front (or Host for an empty restore target). Every
        // later build failure can now return without leaving a removed view in
        // `front_content` or a stale global ActiveSession capability.
        self.resync_after_restore_candidate(wid);
        Ok(staged)
    }

    /// Re-publish the canonical front after a staged restore candidate is removed.
    /// A recursive restore builds its trees off-window, so its host is deliberately
    /// empty between candidates; that transaction boundary is not a stable App state
    /// and must not invoke `sync_active_session`'s whole-App invariant oracle yet.
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
                // PaneLayout names the divider orientation; SplitAxis names the
                // geometric child axis, so the compatibility projection is inverse.
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
            let terminal = self
                .view_store
                .get(*view)
                .copied()
                .and_then(crate::tab_model::View::terminal_session);
            self.remove_view_link(*view);
            if let Some(session) = terminal {
                self.teardown_session(session);
            }
        }
    }

    fn bounded_recovery_text(value: &str) -> String {
        value.chars().take(120).collect()
    }

    pub(crate) fn restore_closed_view_leaf(
        &mut self,
        wid: WindowId,
        descriptor: &restore::RestoredView,
    ) -> Result<(crate::tab_model::ViewId, crate::tab_model::TabPresentation), String> {
        let mut reusable = None;
        self.build_recursive_restore_leaf(wid, descriptor, &mut reusable, LeafIds::Live)
            .map(|(view, presentation, _)| (view, presentation))
    }

    /// Reconstruct one closed recursive tab without consuming its ledger entry. The
    /// caller commits the candidate token only after this returns success. Reopening in
    /// the original live window restores the canonical chrome position; if that window
    /// no longer exists the caller supplies its chosen fallback window.
    pub(crate) fn restore_closed_tab_into_window(
        &mut self,
        wid: WindowId,
        record: &restore::RestoredTab,
        canonical_index: usize,
    ) -> Result<crate::tab_model::TabId, String> {
        if !self.windows.contains_key(&wid) {
            return Err("closed-tab target window disappeared".to_string());
        }
        self.frontmost_window = Some(wid);
        let mut reusable = None;
        let (tab, layout) =
            self.build_recursive_restore_tab(wid, record, &mut reusable, LeafIds::Live)?;
        debug_assert!(reusable.is_none());
        let id = tab.id;
        let terminal = layout.is_some();
        let window = self
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
        let target = canonical_index.min(window.tab_set.len().saturating_sub(1));
        let moved = window.tab_set.reorder(id, target);
        debug_assert!(moved);
        if terminal {
            let projection = window
                .tab_set
                .tabs()
                .iter()
                .take(target)
                .filter(|tab| {
                    tab.root.leaves().into_iter().all(|view| {
                        matches!(
                            self.view_store.get(view),
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
        self.resync_active_or_window(wid);
        debug_assert!(
            self.structural_invariants_ok(),
            "{}",
            self.structural_invariant_violation().unwrap_or_default(),
        );
        Ok(id)
    }

    fn restore_native_tab_into_window(
        &mut self,
        wid: WindowId,
        descriptor: &restore::NativeTabRestore,
    ) -> Option<crate::tab_model::TabId> {
        let result = match descriptor {
            restore::NativeTabRestore::SystemState => self
                .open_system_state_tab_in_window(wid)
                .then_some(())
                .ok_or_else(|| "could not install System State".to_string()),
            restore::NativeTabRestore::Settings { route } => {
                let Some(route) = crate::native_settings::SettingsRoute::from_path(route) else {
                    crate::logging::stderr_line!(
                        "aterm-gui: session restore: invalid Settings route"
                    );
                    return None;
                };
                self.open_settings_tab(route)
                    .then_some(())
                    .ok_or_else(|| "could not install Settings".to_string())
            }
            restore::NativeTabRestore::Markdown { uri } => self
                .open_document_tab_in_window(wid, crate::native_app::AppKind::Markdown, uri)
                .map(|_| ()),
            restore::NativeTabRestore::Editor { uri } => self
                .open_document_tab_in_window(wid, crate::native_app::AppKind::Editor, uri)
                .map(|_| ()),
        };
        match result {
            Ok(()) => self.windows.get(&wid)?.tab_set.active_id(),
            Err(error) => {
                crate::logging::stderr_line!(
                    "aterm-gui: session restore: native tab skipped: {error}"
                );
                None
            }
        }
    }

    fn remove_restore_bootstrap_terminals(&mut self, wid: WindowId) {
        let (tabs, sessions) = self.windows.get(&wid).map_or_else(
            || (Vec::new(), Vec::new()),
            |ws| {
                let tabs = ws
                    .tab_set
                    .tabs()
                    .iter()
                    .filter(|tab| {
                        matches!(
                            self.view_store.get(tab.focus),
                            Some(crate::tab_model::View::Terminal(_))
                        )
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                let sessions = ws
                    .layouts
                    .iter()
                    .flat_map(pane::PaneTree::sessions)
                    .collect::<Vec<_>>();
                (tabs, sessions)
            },
        );
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.layouts.clear();
            ws.tabs = TabIndex::new(0, 0);
            for tab in &tabs {
                ws.tab_set.remove(tab.id);
            }
        }
        for tab in &tabs {
            self.remove_tab_views(tab);
        }
        for session in sessions {
            self.teardown_session(session);
        }
    }

    /// Spawn a persisted tab's sessions (one per leaf, each in its own cwd, in the same
    /// tree order [`pane::PaneTree::rebuild`] assigns ids) and rebuild its pane tree.
    /// `reuse_first` grafts an already-live session in as the first leaf (the bootstrap
    /// session of a just-created window) instead of spawning one.
    ///
    /// ALL-OR-NOTHING per tab: on any spawn failure the already-spawned sessions are
    /// dropped (`Session::drop` hangs up and closes off-thread) and `None` is returned
    /// — a tab either restores whole or not at all, never as a half-built split. The
    /// spawned sessions register + enter the pool only after the whole tab succeeds.
    fn restore_build_tree(
        &mut self,
        wid: WindowId,
        layout: &restore::PaneLayout,
        reuse_first: Option<u64>,
    ) -> Option<pane::PaneTree> {
        let (rows, cols) = self.windows.get(&wid).map(|ws| (ws.rows, ws.cols))?;
        // CELL-PX-1: the host window's real cell box for every newborn engine.
        let cell_px = self.spawn_cell_px(wid);
        // A real run always has a proxy; the headless test harness (None) never gets
        // here (a headless run takes a restore only under the lab's seam, `desk::Desk`,
        // and that run is a real one). Guard, don't panic.
        let proxy = self.proxy.clone()?;
        let leaves = layout.leaves();
        let mut ids: Vec<u64> = Vec::with_capacity(leaves.len());
        let mut fresh: Vec<Session> = Vec::new();
        for (i, leaf) in leaves.iter().enumerate() {
            if i == 0
                && let Some(keep) = reuse_first
            {
                ids.push(keep);
                continue;
            }
            let id = self.next_session_id;
            // SEAMLESS: if this pane's original shell was handed off live (its id is in
            // `seamless_adopt`), RE-ADOPT the running shell into this pane instead of
            // forking a fresh one — the shell keeps going across the update. A cold
            // restore's `seamless_adopt` is empty, so this is always `None` there (fresh
            // fork, unchanged). Consumed (removed) so a shell is never adopted twice.
            let adopt = take_handed_off_shell(&mut self.seamless_adopt, leaf.local_id());
            let cold = adopt.is_none();
            match spawn_session(
                id,
                wid,
                rows,
                cols,
                cell_px,
                &self.session_factory,
                &proxy,
                leaf.cwd(),
                None, // not a connected controller spawn
                None, // no worker's task
                adopt,
            ) {
                Ok(s) => {
                    // A new shell for a restored leaf: the session log opens
                    // it `restore` (a legacy layout names no session it was).
                    if cold {
                        note_restored(&s, None, None);
                    }
                    self.next_session_id += 1;
                    ids.push(id);
                    fresh.push(s);
                }
                Err(e) => {
                    crate::logging::stderr_line!("aterm-gui: session restore: spawn failed: {e}");
                    self.post_message(crate::message_reporters::restored_tab_failed(
                        &e.to_string(),
                    ));
                    return None; // drops `fresh` → clean hang-up of the partial tab
                }
            }
        }
        // `ids` covers every leaf by construction, so this cannot be `None`; stay
        // fail-safe anyway (a `None` here also drops `fresh` cleanly).
        let tree = pane::PaneTree::rebuild(layout, &ids)?;
        // Commit: register each fresh session (a restored pane is a new family-tree
        // root — its pre-quit parent no longer exists) and hand ownership to the pool.
        for s in fresh {
            Self::register_session(&self.store, &s, None);
            self.pool_insert(s);
        }
        Some(tree)
    }
}

/// The session log's `restore` for `session`, a shell standing in for a
/// restored layout leaf that was session `prev` (first of its chain:
/// `lineage`): its `open - restore prev= lineage=` when said before its
/// registration opens it, else an `amend - restore` on the open session
/// (`SessionLog::note_restored`).
///
/// Returns whether the log's readers were owed a wake (phase 3A), which rang
/// the session's watchers under the timeline lock (`slog_take_wake`).
fn note_restored(session: &crate::Session, prev: Option<&str>, lineage: Option<&str>) -> bool {
    let mut tl = session
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let at = tl.slog().now();
    tl.slog_mut().note_restored(prev, lineage, at);
    tl.slog_take_wake()
}

/// The file name every saved set of lost Settings drafts starts with
/// ([`save_lost_settings_drafts`]).
const LOST_SETTINGS_DRAFTS_PREFIX: &str = "settings-drafts-";

/// How many saved sets of lost Settings drafts the folder keeps; older ones go.
/// Far more than updates that lose a draft in a row: only a Settings row a new
/// build no longer has loses one.
const LOST_SETTINGS_DRAFTS_KEPT: usize = 8;

/// THE CARRIED TEXT NO BUILD COULD PUT BACK, SAVED WHOLE (round eight of the
/// update audit, lead U61): every draft in `lost`, its field and its text as
/// typed — newlines, tabs and length kept — in a new private file in `folder`
/// (the folder of `messages.log`). The carry was consumed, so this is the text's
/// one copy; the loss row says where it is. The carry bounds it
/// (`CARRIED_SETTINGS_DRAFTS_CAP_BYTES`), and the folder keeps the newest
/// [`LOST_SETTINGS_DRAFTS_KEPT`] such files.
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
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options.open(&path)?.write_all(text.as_bytes())?;
    prune_lost_settings_drafts(folder);
    Ok(path)
}

/// Keep the newest [`LOST_SETTINGS_DRAFTS_KEPT`] saved sets of lost Settings
/// drafts in `folder`. Best effort.
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

/// Whether a restored agents' row that retired `how` was seen through by the
/// owner ([`App::post_restored_row`]): they dismissed it or answered it, its
/// reporter resolved it, or it folded — on the glass for its whole span, the
/// read the message center itself counts a fold as (a standing row's quiet
/// span). A row that never reached the glass (unseen), was pushed off a full
/// band (evicted), went stale or was withdrawn was not: the misses it named
/// are named again in the next row. (Superseded, it was by this process's
/// next row, which names them; carried or quit, this process is ending.)
fn restored_row_seen_through(how: &aterm_messages::Retired) -> bool {
    matches!(
        how,
        aterm_messages::Retired::Dismissed
            | aterm_messages::Retired::Answered { .. }
            | aterm_messages::Retired::Resolved(_)
            | aterm_messages::Retired::Folded
    )
}

/// The restored agents' row this process last showed
/// ([`App::post_restored_row`]).
#[derive(Debug, Default)]
pub(crate) struct RestoredShown {
    /// The last row's [`crate::harness_host::RestoredRow::seq`] (`0`: none yet).
    seq: u64,
    /// The row it posted for it, while it may still be up.
    id: Option<aterm_messages::MessageId>,
    /// The misses that row names, each `(tab session, when said)`.
    named: Vec<(String, u64)>,
    /// The misses of the rows the owner saw through
    /// ([`restored_row_seen_through`]).
    seen: std::collections::BTreeSet<(String, u64)>,
}

#[cfg(test)]
#[path = "app_restore_tests.rs"]
mod tests;

/// THE COLD DRAIN ([`RestoreDrain`]): a cold launch's rebuild a slice per
/// park — the order its tabs and windows land in, what a person does
/// meanwhile, and what the crash journal and a quit see of it.
#[cfg(test)]
#[path = "app_restore_drain_tests.rs"]
mod drain_tests;

/// THE CARRIED SETTINGS DRAFTS ARE SETTLED ONCE, AFTER THE WHOLE RESTORE
/// (round three of the 2026-09 update robustness work): every draft a seamless
/// update carried either stays in a view that holds it, or is said — in ONE
/// row, with its text — whatever leaf, window or step lost it.
#[cfg(test)]
#[path = "app_restore_carried_draft_settle_tests.rs"]
mod carried_draft_settle_tests;

/// THE JOURNAL'S TEST FOR "A PROGRAM RAN" (D16 review R1–R3, ruling 292),
/// over the pure decision and over REAL sessions: real processes on real
/// pseudo-terminals, each the session leader of its own PTY exactly as a
/// spawned tab's shell is. Every child is this test's own, killed by
/// `SIGKILL` at the end.
#[cfg(all(test, unix))]
#[path = "app_restore_crash_journal_program_tests.rs"]
mod crash_journal_program_tests;
