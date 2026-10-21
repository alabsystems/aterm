// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tab + session-view management: the windows/pool/focus_order-mutating cluster
//! moved as ONE unit so the invariant-maintaining set stays cohesive. Open/
//! switch/cycle/move/close tabs, detach/migrate a tab to a new window, the
//! tab-strip title/fingerprint/hit-test helpers, session teardown, the close
//! outcome funnel, and the live structural-invariant oracle. A verbatim
//! inherent-impl split of `App`.

use aterm_window::event_loop::ActiveEventLoop;

use crate::platform::AppRt;
use crate::{
    App, TabAction, TabIndex, WindowId, WindowState, pane, session_store, tab_bar, term_lock,
};

/// How soon after its spawn a failed interactive shell can count as one that
/// never started: room for a slow rc file's `exit 1` or a moved dylib's abort.
/// A shell that drew a prompt or was written to within it did start
/// ([`App::shell_failed_at_start`]).
///
/// A HANG DETECTOR, not a latency budget (2026-10-01). It was 2 s, and the
/// child's own start counts against it — exec, rc, exit — which a loaded
/// machine starves: measured at load ~70-90, a shell that fails at once took
/// 2.0-4.9 s from fork to EOF (aterm's reader, loop and status reads all
/// prompt), was judged a later exit, closed the only tab and quit the instance
/// silently (78 of 128 stressed runs of `shell_exit_at_start_live_headless`) —
/// the very quit this exists to stop; a slow zsh rc can do the same unloaded.
/// The trade is lopsided: a pane wrongly kept (an untyped shell with no
/// integration that dies within the window) costs one ⌘W; a wrong close loses
/// the window.
pub(crate) const SHELL_START_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);

/// The most the exit path's FIRST look ([`ExitLook::First`]) waits for the
/// status of a shell that may have failed at start
/// ([`App::unstarted_shell`]): its PTY can close a moment before the kernel
/// retires the process. It waits whether the exit turns out clean or not, since
/// nothing knows until the status is in. Every other exit is one non-blocking
/// look. Not on unix, where the reader's gather waits for the retirement off
/// the UI thread before it posts the exit (`spawn::SHELL_RETIRE_WAIT`), and
/// every look is a non-blocking one.
#[cfg(not(unix))]
const SHELL_START_STATUS_WAIT: std::time::Duration = std::time::Duration::from_millis(20);

/// Which look at an exited child's status [`App::exit_status`] is making.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExitLook {
    /// The first on the exit path: the status classifier, or the exit ledger
    /// when `tab_status` is off. Off unix it waits up to
    /// `SHELL_START_STATUS_WAIT` for a shell that may have failed at start; on
    /// unix the reader waited before it posted the exit.
    First,
    /// The ledger's second chance after the classifier found nothing: one more
    /// non-blocking look, except for a shell the first look already waited on,
    /// whose "unknown" then stands for every reader.
    Retry,
}

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

/// Type `line` and Enter into a fresh session's shell, through its canonical
/// input receipt, as ONE frame (`line + CR`): the `send` verb's write path,
/// whose bytes wait in the kernel while the shell boots. An AI CLI's sign-in
/// (`App::ai_sign_in`) is typed so.
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
    // A stock prompt's `user@host:` says only where the user already is, and
    // it says it identically in every tab; ConPTY's program-path title says
    // only which shell was launched. Dropped here (never for another host, and
    // never a title a program chose), an OSC title that carried NOTHING else is
    // now empty and falls through to the cwd rung below — the rung that
    // actually tells tabs apart.
    let live_title = crate::tab_label::informative(live_title);
    user_title
        .filter(|title| !title.is_empty())
        .map(str::to_owned)
        .or_else(|| (!live_title.is_empty()).then(|| live_title.to_string()))
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
thread_local! {
    /// The herald's notifications on this thread (test-only): what
    /// `App::post_herald_notice` would have queued for the notifier.
    pub(crate) static HERALD_POSTS: std::cell::RefCell<Vec<crate::status_item::HeraldNotice>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// Why the notifier's queue refuses this thread's NEXT herald post
    /// (test-only): `App::post_herald_notice` takes it and returns it as its
    /// `Err`, recording nothing — the full queue a test cannot otherwise
    /// fill without running the notifier.
    pub(crate) static HERALD_REFUSAL: std::cell::Cell<Option<&'static str>> =
        const { std::cell::Cell::new(None) };
    /// The windows `App::raise_window` brought forward on this thread
    /// (test-only).
    static RAISED_WINDOWS: std::cell::RefCell<Vec<WindowId>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// The windows this thread's `App::raise_window` calls named, drained
/// (test-only).
#[cfg(test)]
pub(crate) fn take_raised_windows() -> Vec<WindowId> {
    RAISED_WINDOWS.with(|raised| std::mem::take(&mut *raised.borrow_mut()))
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

    /// The strip's title rung never paints ConPTY's program path (audit
    /// 2026-09-22: a fresh split pane's chip read the full `pwsh.exe` path):
    /// the path falls through to the cwd rung, `cmd.exe`'s running command
    /// survives it, and a title a program chose is untouched.
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

/// Cancel a DEFERRED last-tab close on `ws`: drop the flag AND the attribution
/// stashed beside it, because the two are one fact. A window whose
/// `pending_close` is cleared without a teardown never reaches
/// [`crate::App::close_window_logical`], the consumer of a deferred close's
/// [`crate::WindowState::pending_close_attribution`] — so an attribution left
/// behind outlives the close that captured it and gets journalled on the
/// window's NEXT scope-less close, which is how a cancelled ctl `close` could
/// make a later shell exit read `reason=ctl-close`.
///
/// EXACTLY ONE production site calls this, and this names it rather than
/// claiming every abandonment does: `App::open_settings_tab`, where installing
/// or refocusing a real tab in a window parked behind `pending_close` revokes
/// that parked teardown. `escalate_pending_close` is deliberately not a caller
/// — it clears the flag in order to RUN the teardown that consumes the
/// attribution — and the seam that leaves is bounded: `close_window` can return
/// without tearing down, so the stash sits on a window carrying no flag. That
/// is correct for the barriers that RE-DRIVE this same close (a coalesced
/// update handoff replays `close_window` from `finish_update_handoff`; a
/// document checkpoint resumes through `take_ready_document_shutdowns`), which
/// still reach `close_window_logical` and consume the stash — clearing it here
/// would strip the attribution off a close that is merely late. It is NOT
/// correct for a barrier that abandons the close instead (a native reducer's
/// `Blocked` verdict, answered by a recovery palette and a fresh human
/// gesture): that attribution stays parked until the window's next close.
///
/// With NO teardown parked there is nothing to cancel, and the attribution is
/// left alone: the one other writer of the slot is the in-window close banner
/// of Windows and Linux (`App::present_close_banner`), whose question is still
/// standing and whose `confirm yes` replay is the close that consumes it
/// (`close_window_logical` for a window or quit, `replay_confirmed_view_close`
/// for a tab or pane, which leaves the window standing). Clearing it here
/// unconditionally was a live defect (measured 2026-09-27 on 0.95.0, 3 of 3):
/// `invoke CloseTab` on a busy window, then `settings`, then `confirm yes`
/// journalled `reason=unknown exit_code=- by=-` for a close the wire had asked
/// for and answered, where every other path writes `reason=ctl-close by=ctl`.
pub(crate) fn cancel_pending_close(ws: &mut WindowState) {
    if std::mem::take(&mut ws.pending_close) {
        ws.pending_close_attribution = None;
    }
}

/// How far [`App::close_session_by_id`] got — NOT the wire verdict. A last-tab
/// close only FLAGS its window (`pending_close`); the teardown that retires the
/// session, and journals its exit, runs on a caller holding an
/// `ActiveEventLoop`. Reading the store between those two moments says the
/// session is still registered, which is why the answer is
/// [`App::close_session_verdict`] AFTER the escalation and never before it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CloseProgress {
    /// The session left the registry inside the close: nothing is pending.
    Retired,
    /// The close dropped window `.0`'s LAST tab, so THAT window's teardown was
    /// deferred behind its `pending_close`. The id rides along because it is the
    /// only thing that tells the deferred outcomes apart afterwards: a
    /// teardown that never ran leaves the window (and its still-attached tab)
    /// standing, while one that ran and left the session alive did so because
    /// another viewer holds it (or, with no viewer left to name, the session
    /// simply outlived it) — and the verdict must not report the first when
    /// it is looking at the second.
    Deferred(WindowId),
}

/// The wire refusal of a busy PANE close — a pane whose close would hang up its
/// own foreground job, in a tab that survives it. Its own sentence, not
/// [`crate::app_window::WIRE_CLOSE_REFUSED`], because that one names the
/// last-tab confirm and there is no last tab here: the tab stands, the sibling
/// panes stand, only the aimed pane's job stood in the way.
pub(crate) const WIRE_PANE_CLOSE_REFUSED: &str = "close refused (a running job in that pane)";

/// The flagless wire `tab` verb's refusal when no window is front.
pub(crate) const NO_FOCUSED_WINDOW: &str = "no window is focused";

/// Every edge table's rows at one moment ([`crate::connections::all_edges`]),
/// with the connections revision read just before them: what one chrome pass
/// gathers every recomposed tab's connection facts from
/// ([`App::composed_session_chrome`]).
pub(crate) struct EdgeSnapshot {
    revision: u64,
    edges: Vec<aterm_session::Edge>,
}

/// A connected spawn's inputs, checked before anything is created
/// ([`App::connected_spawn_plan`]).
struct ConnectedPlan {
    origin_local: u64,
    /// The window a TAB lands in (`None` for a window).
    host: Option<WindowId>,
    /// The identity the newborn wears, checked on the spawner.
    identity: Option<String>,
    /// Whether `identity` is the origin's, passed down (`identity=` absent).
    inherited: bool,
    /// The folder the caller named (`cwd=`).
    cwd: Option<String>,
    /// Without `cwd=`, the origin's folder, unjudged: a held origin's is
    /// checked where the spawn may wait.
    origin_folder: Option<crate::spawn_folder::PaneFolder>,
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

/// THE INSTANT A TASK'S END WAS TOLD AT, unix ms, for the menu bar's Done
/// for you row (`docs/DESIGN-harness-autopilot-2026-10-15.md` §11.4.3, item
/// 1): `told`, the time the timeline holds for the end's face (the face, or
/// the one held across the host's "unknown" between loop runs;
/// `SessionTimeline::task_face_at`) — the instant the end's own clock
/// (`EndGlass::at`) was read from (`harness_glass::on_task`) — else, where
/// the timeline holds no time for that face, the end's own minute
/// (`Stamp::unix_ms`); `0` where neither is known, and the row says no age.
/// A compare on the common path: the zone is read only in that corner.
pub(crate) fn end_wall_ms(end: crate::harness_glass::EndGlass, told: Option<i64>) -> u64 {
    told.or_else(|| end.at.and_then(crate::harness_glass::Stamp::unix_ms))
        .and_then(|ms| u64::try_from(ms).ok())
        .unwrap_or(0)
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
    /// Every production teardown path funnels through this method so per-session
    /// chrome state (presented labels, status, presence, caches and debounce
    /// deadlines) cannot outlive the PTY it describes. Shared sessions remain live
    /// until their last view closes.
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
            // The owner's upgrade view hands over only LIVE tabs' rows, and
            // the host looks again only when a supervised tab or a step moves
            // it: a closed tab's row (a holder the host does not supervise)
            // stood until the next timed look, and its record kept offering
            // words for a session that was gone (ruling 315, day eight E3).
            crate::harness_host::look_at_upgrades_soon();
            // These caches can carry authored descriptions, status activity,
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

    /// AN ADOPTED SHELL THAT LEAVES THE POOL BEFORE THE ADOPTION PROOF
    /// DEGRADES THE HANDOFF (round eight, finding 58). The proof names every
    /// adopted session still pooled, and the parked parent expects every shell
    /// it handed, so a successor that lost one — a co-view window whose glass
    /// would not attach, a tab whose leaves were rolled back, an unconsumed
    /// bootstrap — could only prove a subset: filed `AdoptionMismatch`, a
    /// STRUCTURAL verdict on the new build, for what was this machine's
    /// moment. It withholds the proof instead, as a terminal window that
    /// failed to open always has: the parent times out, rolls back and resumes
    /// every shell from its own descriptors (the dropped one's included,
    /// `Session::drop` before Commit), and the attempt is filed transient.
    /// Past the proof (`handoff_ready` taken) a drop changes no proof, and
    /// nothing is said.
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
        self.closed_view_record_for(window, tab.id, tab.focus)
    }

    /// The reopen record for closing leaf `view` of tab `tab_id` in `window`: what
    /// Cmd-W keeps for the FOCUSED leaf of the ACTIVE tab, for ANY leaf — the wire
    /// `close` aims by session at a pane that need not be focused nor in the
    /// active tab, and a pane it closes must be as reopenable as one Cmd-W closed.
    /// `None` for a single-leaf tab (that close is a whole-tab record,
    /// `retain_closed_tab`) and for a leaf the tab does not hold.
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
                    icon: Some(presentation.icon),
                    indicators: crate::tab_model::TabIndicators {
                        dirty: presentation.indicators.dirty,
                        busy: presentation.indicators.busy,
                        // A native leaf is the OUT-OF-BAND attention owner; it
                        // has no session and therefore no classified status.
                        attention: presentation.indicators.attention
                            || self.edit_view_attention(view),
                        status_attention: false,
                    },
                    // No session, no edge table, no connection role either.
                    conn: None,
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
    /// focused canonical leaf: the content-agnostic split the mixed-leaf tests
    /// build their layouts with (no shipping command duplicates a native view
    /// yet). It never fabricates a terminal compatibility tree.
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
            // Its document may be held by nothing now; the next park looks.
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

    /// Whether the visible tab strip is enabled (`tab_strip_rows > 0`). GLOBAL. The
    /// whole strip path (splice + paint + hit-test) is gated on this; `false` is the
    /// byte-identical no-strip path.
    pub(crate) fn tab_strip_enabled(&self) -> bool {
        self.tab_strip_rows > 0
    }

    /// THE CHROME ROWS above the terminal grid: the in-grid tab strip's rows plus
    /// the message band's (`message_band_rows`, committed by
    /// `sync_message_band_rows`). This — never `tab_strip_rows` alone — is what
    /// every geometry law reads: the grid fit (`grid_dims_for`), the frame size
    /// (`window_frame_px_for`), the pointer's cell mapping, the effects origin, the
    /// accessibility snapshot. The strip painter alone still reads
    /// `tab_strip_rows`, because that is how many of these rows are ITS. Every
    /// window has the same: presence owns no row (owner surfaces §5).
    pub(crate) fn chrome_rows(&self, wid: WindowId) -> u16 {
        let _ = wid;
        self.chrome_rows_shared()
    }

    /// The chrome rows every window shares — the strip and the message band.
    /// For geometry computed before a window exists (`window_frame_px_for` at
    /// creation); every sited law reads [`Self::chrome_rows`].
    pub(crate) fn chrome_rows_shared(&self) -> u16 {
        self.tab_strip_rows.saturating_add(self.message_band_rows)
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
                    return (fallback, None, None);
                };
                let Some(s) = self.pool.get(view.session) else {
                    return (fallback, None, None);
                };
                // TOP RUNG: the operator's `meta set title` (leaf lock, dropped
                // before the term lock is ever taken; contended only by an actual
                // `meta set`). When it hits, the term try-lock is skipped.
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
                    // The cwd rung is a LABEL: it must read as a path on this
                    // platform, so it goes through the native conversion rather
                    // than showing the engine's RFC 8089 `/C:/…` URI path — a
                    // report from another machine excepted, which keeps it, as
                    // every consumer does (`crate::cwd_native`).
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
                (fallback, Some(view.session), fresh)
            })
            .collect();
        // Pass 2: fold through the per-window keep-stale cache, pruned to the
        // sessions that still label a tab so it stays bounded.
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
                // The label is the identity title alone, sanitized for the
                // strip (no cap: the strip fits its own labels) through the
                // per-session cache `refill_strip_titles` shares.
                chrome_labels.present(
                    Some(sid),
                    &raw,
                    crate::chrome_label::ChromeSurface::TabStrip,
                )
            })
            .collect()
    }

    /// Stamp each tab's §4 connection role onto its presentation from the live
    /// edge tables ([`crate::connections::roles_by_local`] — the §4.1 predicate
    /// over the registry, tables only, no liveness qualifier). Runs inside
    /// [`Self::refresh_window_tabs`] — the one funnel every structural change,
    /// prompt relabel, and connect/disconnect repaint flows through — so both
    /// strip renderers read the mark out of the same canonical presentation
    /// snapshot as every other indicator, and a stale mark cannot outlive the
    /// next strip push. `tab_connection_badge = false` stamps `None`: the MARK
    /// is quieted, the connections themselves (and their non-visual `chrome`/
    /// `edges` surfaces) are untouched.
    fn stamp_tab_connection_roles(&mut self, wid: WindowId) {
        let roles = if self.config.tab_connection_badge_or_default() {
            crate::connections::roles_by_local(&self.store)
        } else {
            std::collections::HashMap::new()
        };
        let Some(ws) = self.windows.get(&wid) else {
            return;
        };
        // OR-fold panes per tab (the `aggregate_presentations` discipline): a
        // background pane's direction never hides behind the focused pane's —
        // outbound + inbound across a split reads as Both.
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
    /// The presentation is the stable tab model's own value, never inferred from a
    /// title, PTY, URI, or native reducer state at paint time.
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
        // The tab marks (owner surfaces §5): the published need's `◇`/`◆`, the
        // person's hand, a finished task's dot — folded over the tab's own
        // note (a failed command, a notice), never a need mark itself.
        self.stamp_presence_chips(wid, &mut metadata);
        metadata
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
                        // Only native tabs read the snapshot tooltip back.
                        let tooltip = if session.is_none() {
                            tab.presentation.tooltip.clone()
                        } else {
                            None
                        };
                        (session, tooltip)
                    })
                    .collect()
            });
        // One edge snapshot for every tab this pass recomposes: a connection's
        // revision bump (a tab closing, a grant) misses every tab at once, and
        // each tab walked every table, so a refresh of N tabs cost N^2.
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

    /// THE SESSION CHROME'S WAKE, for the event loop to fold at `now`
    /// (`now_ms` on [`crate::turn_ledger::now_ms`]'s clock): one wake at the
    /// earliest instant the clock moves a cached composition
    /// ([`crate::session_chrome::clock_expiry_ms`]: an upgrade's words);
    /// once due, at most one window refreshed per turn
    /// ([`Self::advance_session_chrome_expiry`]) with a 1 ms follow-up for
    /// the remainder. A cache the clock cannot move — every tab with no
    /// upgrade, its menu's ages read where they are shown — and an empty one
    /// arm nothing.
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
        // Cache stamps come from the same monotonic process clock and cannot
        // legitimately be more than one TTL in the future. The cap keeps a
        // corrupted/internal future stamp from overflowing `Instant +
        // Duration`; an early wake simply re-evaluates it.
        let expiry_ms = chrome::next_cache_expiry_ms(&self.session_chrome)?;
        let remaining_ms = expiry_ms
            .saturating_sub(now_ms)
            .min(chrome::CACHE_MAX_AGE_MS);
        Some(now + std::time::Duration::from_millis(remaining_ms))
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

    /// THE SESSION LOG'S CLAUSE as `session`'s chrome is composed with it
    /// (phase 3B; addendum A12, ruling S16), only with `[session_log] clause`
    /// on: a `try_lock` on `timeline`, taken after the blocking `high_id` read
    /// as a separate acquisition. Contended, the cached clause is served and
    /// the session enlisted in the retry lane, as a contended terminal is
    /// ([`Self::composed_session_chrome`]). Off, no acquisition at all.
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
    /// (c) the session-status revision is unchanged — the activity line is
    /// Status's words, which move independently of the durable session
    /// timeline and the label. Age
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
    /// title-drift tail retry, so a chrome refresh never parks the event loop.
    ///
    /// `edges` is the caller's one snapshot of the edge tables for a pass over
    /// many tabs, taken here at the first miss with the connections revision
    /// read before it ([`EdgeSnapshot`]); `&mut None` for one tab.
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
        // Read the connections epoch BEFORE any fact is gathered: a mutation
        // racing this compose then leaves the cached entry already stale, so
        // the next refresh recomposes — never the inverse (fresh epoch over
        // stale facts, which would serve a revoked menu for 30 s, §2.4).
        let connections_revision = self.connections.revision();
        // The same order for the upgrade view's revision (gap #21): read
        // before the row it composes from.
        let upgrade_revision = self.upgrade_view.revision();
        let activity_revision = self.session_status_revision(session);
        let high_id = ctx
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .high_id()
            .unwrap_or(0);
        let now = crate::turn_ledger::now_ms();
        // THE SESSION LOG'S CLAUSE, after the blocking `high_id` read
        // ([`Self::chrome_clause_read`]).
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
        // Tab Subject & Status owns the live activity line, and says nothing when
        // it has nothing honest to say (RFC §11).
        let activity = self.session_status_text(session);
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
        // Connection facts AFTER the store guard above is dropped (the gather
        // re-reads the registry itself — the clone-then-release discipline),
        // from the pass's snapshot. A snapshot taken before this compose read
        // its epoch carries an older revision, and the entry keeps the older of
        // the two, so it can only be recomposed early, never serve facts older
        // than its epoch.
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
            // Walk the retained deque BACKWARDS and stop after the tail we keep:
            // the newest-first order the tooltip/menu contract wants, and the
            // rows the chrome leaves out (an agent's verdict moves, typing, bus
            // traffic) are cut BEFORE the cap, or a burst of them would leave
            // no row at all. The walk stops at the fifth kept row, so the leaf
            // mutex is held for a handful of steps, the whole ring at worst.
            // Each row keeps its stamp: a surface ages it as it shows it.
            chrome::timeline_tail(tl.since(None).rev().map(|e| (e.kind, e.t_ms)))
        };
        let can_rename = self.can_rename_session(retry_window);
        let upgrade = sid.as_ref().and_then(|sid| {
            self.upgrade_view
                .menu_for(sid.as_str(), crate::upgrade_host::now_s())
        });
        // The AGENT section's facts, off the one team table.
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

    /// Gather one session's §2.3 connection FACTS from the live edge tables —
    /// the tables and ONLY the tables (the §4.1 mark rule: they are the
    /// authority record, so a wire `grant` with no [`ConnectionRecord`] still
    /// lists honestly). Per peer, both halves are folded: rows with `src = S`
    /// in other sessions' tables are S's OUTBOUND authority, rows in S's own
    /// table are INBOUND; both present ⇒ the `⇆` peer pair. The flow rung is
    /// the family-row convention (write-input ⇒ push). Peer titles resolve
    /// user meta title ▸ registry title (the fleet-glance rung); a foreign
    /// src no local session owns titles `unknown`. Sorted by peer sid for a
    /// stable menu. `live` is best-effort `false` throughout: lease/watcher
    /// identity names control connections, not peer sessions, so per-peer
    /// liveness cannot be attributed honestly today (absent = not live).
    ///
    /// Runs only on a chrome-cache MISS (epoch-gated by the connections
    /// revision), so the table walk is authority-act rate, never per frame.
    /// `edges` is a snapshot of every table ([`crate::connections::all_edges`]).
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
        // Keyed by the sid STRING (SessionId is not Ord); BTreeMap gives the
        // stable listing order.
        let mut peers: std::collections::BTreeMap<String, (aterm_session::SessionId, Halves)> =
            std::collections::BTreeMap::new();
        for edge in edges {
            if edge.src == edge.dst {
                continue; // self-loops spell no connection (§1.5)
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
                    // (false, false) is unreachable: an entry exists only
                    // because at least one half was folded.
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
                .map(|tab| {
                    // The hover's presence lines: the tab's need, said once,
                    // and its agent's state (`App::presence_hover`).
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
    ///
    /// And it carries the strip's POINTER HOVER — which chip reveals its `✕`, and
    /// whether the `+` or the trailing version label wears the hover wash — for
    /// the same reason: each is painted into the strip and dirties no terminal
    /// cell, so with this value standing still the RepaintKey early-out swallowed
    /// the frame `track_strip_hover` asks for, and a wash reached the glass only
    /// with the next unrelated present (a blink flip, output) — never, with the
    /// cursor not blinking. Measured 2026-10-01 on Windows (a release build,
    /// `cursor_blink = false`): after `pointer move` onto the version label,
    /// `chrome` said `hovered=true` while the window still showed bare band
    /// 1.5 s later, and after `pointer leave` the stale wash stayed. Hashed only
    /// while something is lit, so a resting strip's value is byte-identical to
    /// the one before hover was folded in.
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
        // The pointer's hover (see the doc): a chip's `✕`, the `+`'s wash, the
        // version label's wash, a window button's wash. Absent while nothing is lit.
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
        // The window's own buttons, where the band is its header: which ones, and
        // maximize or restore. Absent where the window system decorates.
        if let Some(frame) = self.windows.get(&wid).and_then(|ws| ws.client_frame) {
            "client-frame".hash(&mut h);
            frame.hash(&mut h);
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
        self.windows.insert(wid_b, Box::new(ws_b));
        self.frontmost_window = Some(wid_b);
        // Re-mirror BOTH: A's active tab changed (it lost its old active), and B is
        // the new frontmost (also re-points the global control/notify handle to B).
        // A moved terminal's reader thread remains stamped with old window A, but
        // output routing resolves its live view and therefore finds B. Native
        // content has no reader/PTY stamp at all.
        self.sync_window(wid_a);
        self.sync_active_session(); // frontmost = B
        debug_assert!(
            self.structural_invariants_ok(),
            "window/session structural invariants violated after tearing a tab into its own window: {}",
            self.structural_invariant_violation().unwrap_or_default(),
        );
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
    ///
    /// Returns the REASON it declined rather than `()`. Every bail below is a
    /// state the user can actually be in, and the menu seam
    /// (`invoke_menu_action_by_name`) used to answer OK for all of them — a
    /// silent success for work that never happened. Key-path callers, for which
    /// a refusal has nowhere to go, discard it explicitly.
    pub(crate) fn migrate_active_tab_to_next_window(&mut self) -> Result<(), &'static str> {
        let Some(wid_a) = self.frontmost_window else {
            return Err("no window is focused");
        };
        // The next window after A in id order, wrapping to the first. Every window is
        // now a normal mixed-tab host; there is no Settings-only accessory kind.
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
            // never move a tab onto its own window (also: sole terminal window)
            return Err("there is no other window to move the tab to");
        }
        // Pull A's canonical active tab. A terminal tab additionally moves its
        // compatibility PaneTree; native content never acquires a sentinel tree.
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
            "window/session structural invariants violated after \
             migrate_active_tab_to_next_window: {}",
            self.structural_invariant_violation().unwrap_or_default(),
        );
        Ok(())
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
        // Re-mirror BOTH viewers: B is the new frontmost (also re-points the global
        // control/notify handle to B). A is unchanged — it still displays `s`. NOTE:
        // `s`'s reader thread stamps its `Wake::Output` with ONE owning window, but the
        // `Output` arm routes via `windows_displaying(s)` — now BOTH A and B, since both
        // canonically contain `s` — so the shared session's output repaints both viewers
        // with no re-stamp (the multi-viewer fan-out is now genuinely exercised).
        self.sync_active_session(); // frontmost = B
        debug_assert!(
            self.structural_invariants_ok(),
            "window/session structural invariants violated after tearing a tab into its own window: {}",
            self.structural_invariant_violation().unwrap_or_default(),
        );
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
    /// switch to it — the shipping [`Self::install_tab_session`] body a tab lands
    /// through ([`Self::install_new_tab`]), fed a stub session instead of a real PTY. `session.id` MUST
    /// equal `self.next_session_id` (the test builds it that way), which is then
    /// bumped. Used to stage multi-tab windows; the appended terminal becomes the
    /// window's canonical front capability.
    #[cfg(test)]
    pub(crate) fn push_stub_tab(&mut self, wid: WindowId, session: crate::Session) {
        self.install_new_tab(wid, session);
    }

    /// Test-only: split window `wid`'s ACTIVE tab into a 2-pane vertical split,
    /// spawning a fresh stub session for the new (now-focused) pane — the shipping
    /// [`Self::install_split_session`] body a split lands through
    /// ([`Self::install_split_pane`]), fed a stub
    /// session instead of a real PTY (the pre-spawn refusals are the caller's to
    /// stage). Returns the new pane's session id.
    #[cfg(test)]
    pub(crate) fn split_active_stub_tab(&mut self, wid: WindowId) -> u64 {
        self.split_active_stub_tab_dir(wid, pane::SplitDir::Vertical)
    }

    /// [`Self::split_active_stub_tab`] in a named direction — the minimum-pane
    /// tests need the audit's ALTERNATING gesture (left/right, top/bottom, …),
    /// which is what drove the tree deep enough to mint off-grid panes.
    #[cfg(test)]
    pub(crate) fn split_active_stub_tab_dir(&mut self, wid: WindowId, dir: pane::SplitDir) -> u64 {
        self.split_active_stub_tab_dir_with_sink(
            wid,
            dir,
            std::sync::Arc::new(aterm_session::sink::SinkWriter::new(-1)),
        )
    }

    /// [`Self::split_active_stub_tab_dir`] with a caller-provided sink for the NEW
    /// pane's session, so a test can observe bytes crossing that pane's own egress
    /// boundary — a window-level mirror reports silence for a session the window
    /// is showing but not fronting, which is exactly the DEC 1004 focus-report
    /// case (the pane that LOST the keyboard is the one with something to say).
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

    /// Test-only heterogeneous counterpart: insert a real stable terminal view
    /// and pool session into the canonical active tree without requiring a PTY.
    #[cfg(test)]
    pub(crate) fn split_active_with_stub_terminal(
        &mut self,
        wid: WindowId,
        axis: crate::tab_model::SplitAxis,
    ) -> (u64, crate::tab_model::ViewId) {
        // THIS IS THE CANONICAL ARM ONLY: it splits `tab_set` and never mirrors
        // into `layouts`. That is correct exactly when the active tab owns no
        // compatibility entry — a native or heterogeneous tab. An ALL-TERMINAL
        // tab DOES own one and must go through `split_active_stub_tab*`, which
        // splits the legacy tree and calls `sync_tab_model_from_layout`.
        //
        // Without this guard the two topologies desync silently and the failure
        // surfaces later as `structural_invariants_ok` firing inside
        // `sync_active_session`, naming neither this helper nor the caller.
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

    /// Revalidate the booked authority before attaching a reader or registering
    /// the worker's newborn. Main-thread booking and landing cannot interleave
    /// another lifecycle mutation between this check and the install. `host` is
    /// the window a connected TAB lands in, which must still host its origin; a
    /// connected WINDOW (`None`) needs only the origin alive.
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
            // Summarize's worker is single-flight while it books
            // (`TabSpawns::summary_pending`); its landing admits like a plain
            // tab's.
            crate::tab_spawn::SpawnAdmission::Summary => {}
        }
        Ok(())
    }

    /// Resolve + validate a CONNECTED spawn's origin and place BEFORE anything
    /// is created (design §6 `spawn connected=` / the §2.3 presets) — the
    /// headless-testable half of [`Self::spawn_connected_session`] (the spawn
    /// itself needs the event loop). Returns the origin's local id and, for
    /// `place=tab`, the window hosting it (the newborn lands beside the
    /// origin). `place=window` under headless is the NEW `ERR headless` reply
    /// (design §1.4#7): the `Wake::CreateWindow` arm silently ignores, but a
    /// reply-bearing, authority-minting spawn must refuse honestly — and
    /// BEFORE any create. `place=tab` stays headless-legal (tabs are logical).
    pub(crate) fn connected_spawn_precheck(
        &self,
        place: crate::connections::ConnectedSpawnPlace,
        origin: &aterm_session::SessionId,
    ) -> Result<(u64, Option<WindowId>), String> {
        let origin_local = {
            let g = self.store.read().unwrap_or_else(|p| p.into_inner());
            match g.by_sid(origin) {
                None => return Err("no such session".to_string()),
                // A dead origin can neither drive nor be driven; its table's
                // nonce already fails closed — refuse honestly (the `connect`
                // verb's rule).
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

    /// The folder the session with registry LOCAL id `local` hands a spawn
    /// ([`Self::session_folder`], the New Tab's own reader: the native path,
    /// with whether a program other than its shell holds it) — the connected
    /// spawn's origin-cwd default (design §6: `cwd=` overrides it). Such a
    /// folder (ssh, any program) that is not here is not taken, as for a New
    /// Tab: the check that judges it
    /// ([`crate::spawn_folder::PaneFolder::resolve`]) runs where the spawn may
    /// wait, not here.
    fn session_folder_by_local(&self, local: u64) -> Option<crate::spawn_folder::PaneFolder> {
        self.pool.get(local).and_then(Self::session_folder)
    }

    /// Mint a CONNECTED spawn's `both` session connection through the ONE mint
    /// path ([`crate::connections::connect_in`], design §1.4#2): controlled ⇒
    /// the origin drives the newborn (`src=origin, dst=newborn`); controller ⇒
    /// the newborn drives the origin. Split from the spawn so the mint half is
    /// unit-testable against registered stub sessions (no PTY, no event loop).
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
            // `connect_in` refuses only a self-loop — unreachable here (the
            // newborn's sid is freshly generated), kept fail-closed.
            Err("self-loop".to_string())
        }
    }

    /// THE CONNECTED SPAWN end-to-end (design §2.3 / §6 `spawn connected=`),
    /// the wire handler's (`Wake::SpawnConnectedSession`; no menu row starts
    /// one any more, `docs/DESIGN-owner-surfaces-2026-10-16.md` S12): precheck,
    /// spawn the newborn at `place` (lineage parent = origin for CONTROLLED;
    /// `ATERM_OBSERVE_SESSION_ID` env for CONTROLLER), then mint the ONE
    /// `both` connection and refresh the §4 marks ([`Self::connect_newborn`]).
    /// `done` hears the newborn's sid (the wire `OK <sid>` body) or why not.
    /// The shell spawns off the event loop (`tab_spawn`), a TAB's and a
    /// WINDOW's alike, and is connected as it lands, under the sid it was
    /// spawned with; the window is built around it then.
    #[allow(
        clippy::too_many_arguments,
        reason = "the connected spawn's birth facts — kind, place, origin, cwd, the \
                  agent identity and the theme — are each a per-spawn datum, and `done` \
                  is its answer; a wrapper struct would relocate the list, not simplify it"
    )]
    pub(crate) fn spawn_connected_session(
        &mut self,
        kind: crate::connections::ConnectedSpawnKind,
        place: crate::connections::ConnectedSpawnPlace,
        origin: &aterm_session::SessionId,
        cwd: Option<String>,
        identity: Option<crate::control::IdentitySpec>,
        theme: Option<crate::session_theme::SessionTheme>,
        done: crate::tab_spawn::SpawnDone,
    ) {
        use crate::connections::{ConnectedSpawnKind as Kind, ConnectedSpawnPlace as Place};
        let ConnectedPlan {
            origin_local,
            host,
            identity,
            inherited,
            cwd,
            origin_folder,
        } = match self.connected_spawn_plan(place, origin, cwd, identity) {
            Ok(plan) => plan,
            Err(why) => return done(self, Err(why)),
        };
        let lineage_parent = matches!(kind, Kind::Controlled).then(|| origin.clone());
        let observe = matches!(kind, Kind::Controller).then(|| origin.clone());
        // The origin's folder is judged on the spawner, as the focused pane's
        // is for any tab or window, and so is the identity.
        let ask = crate::tab_spawn::SpawnAsk {
            admission: crate::tab_spawn::SpawnAdmission::Connected {
                origin: origin.clone(),
                local: origin_local,
            },
            folder_of: origin_folder,
            observe,
            theme,
            ..crate::tab_spawn::SpawnAsk::requested(cwd, identity, inherited)
        };
        let then = Self::connected_landing(kind, origin.clone(), done);
        match place {
            // A controlled window's first session is its origin's child.
            Place::Window => self.open_window_off_loop(ask, lineage_parent, then),
            Place::Tab => {
                // Precheck resolved the hosting window for every Tab place.
                let host = host.expect("tab precheck resolves a host window");
                self.open_off_loop(host, crate::tab_spawn::SpawnPlace::Tab, ask, then);
            }
        }
    }

    /// What a connected tab or window does as it lands: connect the newborn to
    /// `origin` under the sid it was spawned with ([`Self::connect_newborn`]),
    /// then tell `done`. One that never landed tells `done` why.
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

    /// A connected spawn's checks and inputs, before anything is created:
    /// [`Self::connected_spawn_precheck`], the identity the newborn wears (the
    /// spawner checks it), and its folder.
    fn connected_spawn_plan(
        &self,
        place: crate::connections::ConnectedSpawnPlace,
        origin: &aterm_session::SessionId,
        cwd: Option<String>,
        identity: Option<crate::control::IdentitySpec>,
    ) -> Result<ConnectedPlan, String> {
        let (origin_local, host) = self.connected_spawn_precheck(place, origin)?;
        // IDENTITY (session identities, 2026-09-17): the ORIGIN is the aimed
        // session of a connected spawn (the selector is discarded), so the
        // newborn inherits its identity unless `identity=` says otherwise —
        // the same rule as `@<sid> spawn`, the same only-create path: the
        // spawner makes a named one, before the fork, once this precheck has
        // admitted the spawn, and checks an inherited one
        // ([`crate::tab_spawn::IdentityPrep::Admit`]), never this turn.
        let inherited = identity.is_none();
        let identity = crate::control::resolve_spawn_identity(
            identity,
            self.identity_of_local(origin_local).as_deref(),
        );
        // Default the newborn's cwd to the ORIGIN's (design §6): the `of=`
        // target need not be the front pane the plain-spawn default would read.
        let origin_folder = if cwd.is_none() {
            self.session_folder_by_local(origin_local)
        } else {
            None
        };
        Ok(ConnectedPlan {
            origin_local,
            host,
            identity,
            inherited,
            cwd,
            origin_folder,
        })
    }

    /// Mint the `both` connection between a connected spawn's `origin` and its
    /// registered `newborn` (audited `surface=wire`: the wire caller reads
    /// its reply, so no first-use notice is said) and refresh the §4 marks.
    /// Returns the newborn's sid, the wire `OK <sid>` body.
    fn connect_newborn(
        &mut self,
        kind: crate::connections::ConnectedSpawnKind,
        origin: &aterm_session::SessionId,
        newborn: &aterm_session::SessionId,
    ) -> Result<String, String> {
        self.mint_connected(kind, origin, newborn, "wire")?;
        // The tables changed ON the main thread — no wake round-trip needed:
        // run the `Wake::ConnectionsChanged` body directly.
        self.refresh_connection_surfaces();
        Ok(newborn.as_str().to_string())
    }

    /// Re-stamp every window's §4 tab marks through the one strip funnel and
    /// repaint — the `Wake::ConnectionsChanged` body, also run directly by
    /// main-thread minters (the connected spawn) whose authority act
    /// never crossed the control thread. Authority-act rate, all windows (a
    /// connection's endpoints may sit in different windows).
    pub(crate) fn refresh_connection_surfaces(&mut self) {
        let wids: Vec<WindowId> = self.windows.keys().copied().collect();
        for wid in wids {
            let _ = self.refresh_window_tabs(wid);
        }
        // The §5 map is a connection surface too: any OPEN map re-snapshots
        // the graph now (selection surviving by identity) rather than serving
        // revoked arrows until its next open.
        self.overview_refresh_all();
        // So is the ❯ status item (§5.1 — its menu renders the connections
        // count). The per-window loop above reaches it through the
        // `refresh_window_tabs` title funnel, but the bar item OUTLIVES
        // windows — a wire mint after the last window closed would otherwise
        // serve the stale count. Fingerprint-gated, so the double visit on the
        // windowed path costs one string compare.
        self.mark_status_dirty();
        self.request_redraw_all_windows();
    }

    /// One registry handle as the status item sees it
    /// (`docs/DESIGN-owner-surfaces-2026-10-16.md` §3): the tab's label as
    /// every Agents surface names it ([`Self::tab_label_of_session`]: the
    /// pill, the Overview, the tab menu and the long-hold entry name it the
    /// same), the session's PUBLISHED verdict ([`Self::publish_need`]), its
    /// agent word, its task record and what the person's hold refused —
    /// leaf locks one at a time, the terminal's only by a try-lock. The
    /// caller holds NO registry guard (the label reads the registry again).
    /// The item classifies nothing itself (§2.3), so its rows and its
    /// notifications say what `status needs=` and the survey say. Its
    /// supervised agent's task END (`docs/DESIGN-harness-autopilot-2026-10-15.md`
    /// §11.4.3, item 1) is read off the harness's glass, an App field, and
    /// its instant off the timeline under the lock already taken. Whether its
    /// Autopilot chip's face is LANDED (§12.4.3, item 1) is the chip's own
    /// fold of the same glass ([`Self::chip_face_landed`]), so This Mac
    /// counts a landed tab that is no worker. Its place is the glance's to
    /// fill ([`Self::status_glance`]), only where two rows share a label.
    pub(crate) fn status_session_row(
        &self,
        h: &crate::session_store::SessionHandle,
    ) -> crate::status_item::SessionRow {
        let glass_end = self
            .harness_glass
            .get(&h.local_id)
            .and_then(crate::harness_glass::SessionGlass::end);
        let landed = self.chip_face_landed(h.local_id);
        // LANDING (§12.4.3 item 1, slice 3c): Land's instruction typed to it
        // and a hold standing, under `⤓ land` ([`Self::shown_hold`]); its
        // tip what holds it, with no clock.
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

    /// Whether session `session`'s Autopilot chip wears `✓ landed`
    /// ([`crate::claude_lights::ChipFace::of`] over its view and its glass,
    /// the fold the frame draws): Land, its task's face landed. The glass's
    /// face is read first, one map lookup, so only a landed session reads
    /// its Autopilot (leaf locks).
    pub(crate) fn chip_face_landed(&self, session: u64) -> bool {
        let glass = self.chip_glass(session);
        glass.task == Some(aterm_agent::harness::acts::TaskFace::Landed)
            && crate::claude_lights::ChipFace::of(self.session_autopilot(session).1, &glass)
                == crate::claude_lights::ChipFace::Landed
    }

    /// ROWS THAT SHARE A LABEL SAY WHERE THEY ARE (§12.4.1): each row whose
    /// label another row of `rows` also carries is given its place
    /// ([`Self::session_place`]: `in tab 2`, `in tab 3, left pane`), and
    /// where two of them still read alike (an agent and a shell in one tab,
    /// which `session_place` does not tell apart) the pane every pane counts
    /// in ([`Self::pane_place`]). A unique label names no place, so a glance
    /// with none alike walks no window.
    fn place_alike_rows(&self, rows: &mut [crate::status_item::SessionRow]) {
        // Compared in place: a glance whose labels are all unique allocates
        // nothing here.
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

    /// Fold one session's PUBLISHED need into the herald
    /// ([`crate::status_item::Herald`], §9 S4: the herald is fed by the
    /// need): remember since when its need and hold stand (the glance's ages,
    /// headless too), dirty the menu-bar item where what it shows of the
    /// session moved, and post the one native notification a transition
    /// earns. The decision is one `aterm.log` line, written after the post so
    /// it says what the notifier's queue did with it — `info` for a
    /// transition's (queued, or why not), a `warn` for a notice the queue
    /// refused, and no text ([`crate::status_item::herald_log_line`]). A
    /// refused notice is handed back to the herald
    /// ([`crate::status_item::Herald::refused`]), which owes it again.
    /// Called from [`Self::refresh_presence_session`], i.e. at change rate
    /// (a verdict move from the status sweep, a `meta set`, a lease wake).
    /// Leaf locks only, each released before the next; the notification is
    /// a `try_send` onto `notify.rs`'s bounded queue, so nothing here blocks
    /// the event loop. A headless instance has no menu bar and posts nothing:
    /// it never reaches the notifier subprocesses (AGENTS.md).
    pub(crate) fn herald_session(&mut self, session: u64) {
        // A fleet need's key (the herald's timer re-heralds an owed one):
        // the fleet's notices are re-read from the last fleet read.
        if crate::fleet_glance::is_herald_key(session) {
            self.herald_fleet();
            return;
        }
        // The handle is copied out of the registry's guard first: the row's
        // label reads the registry again.
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

    /// A SEAMLESS UPDATE'S STATUS CARRY, at `session`'s first publication
    /// here (`row`, read at `now_unix`): what the outgoing process's status
    /// item knew of it (`SessionRecord::status_seen`, by sid). The done the
    /// owner had looked at stays seen — the same task entry, by the
    /// wall-clock instant it was entered, which the task carry keeps exactly
    /// — and the herald takes the told need and the ages
    /// (`status_item::Herald::adopt`), so the update tells no standing need
    /// again, shows no read Done for you row again, and restarts no age.
    /// Spent at once: a session publishes many times, and only its first
    /// meets the carry.
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

    /// A SEAMLESS UPDATE'S CARRIED END, at `session`'s first publication
    /// here (`docs/DESIGN-harness-autopilot-2026-10-15.md` §11.2.3, item 8;
    /// slice 2b): what the outgoing process held of its Autopilot task
    /// (`SessionRecord::autopilot_end`, by sid `sid`). Beside
    /// [`Self::adopt_status_carry`], and spent at once as it is:
    ///
    /// * THE WORK goes into the A/B book (`AbBook::carry_work`), so the
    ///   task's next end reads the work since it opened across the update
    ///   (R12: a move from "unknown" zeroes nothing).
    /// * THE END goes onto the session's glass as it stood
    ///   (`SessionGlass::carry_end`): its kind, its own clock, seen or not —
    ///   so an end nobody had looked at wears the tab's `•` here until
    ///   someone does. It is no new end (`harness_task::is_a_task_end`): it
    ///   counts nothing and records nothing. Dropped, fail-closed, where the
    ///   loop has already restated a face that is not the one the timeline
    ///   was seeded with at its very time (`CarriedEnd::told`): another face
    ///   (`open`: it moved on), or the same face at another time (a landing
    ///   withdrawn and told again: another landing, no end) — a face
    ///   restated later is `harness_glass::on_task`'s, which clears an end at
    ///   open or theirs.
    /// * JUDGED AT ONCE by the one looking rule against the frontmost window
    ///   (`App::see_front`), as an end told here is
    ///   (`App::note_harness_end`): one in a pane someone looks at is seen
    ///   now and marks nothing. One left unseen re-stamps every strip and
    ///   dirties the glance (`App::restamp_for_end`), once.
    ///
    /// Not the restore's user-meta seed (`App::seed_restored_user_meta`):
    /// it returns early where a leaf carries no user field — the owner's
    /// usual tab, which follows the default (the review's R6).
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

    /// Each session's Autopilot end and its task's work, as the rows a
    /// seamless update's park stamps on its manifest
    /// (`SessionRecord::autopilot_end`, `app_update_handoff::stamp_autopilot_end`;
    /// §11.2.3, item 8): the end standing on the glass — its kind, the wall
    /// time the timeline holds for that face, and whether it was seen — and
    /// the A/B book's work since the task opened, at the park's instant.
    /// An end where the timeline holds no time for its face (a face moved
    /// under it that is no end's) is not carried, fail-closed. A session
    /// with neither — every shell — has no row; the timeline's leaf lock is
    /// taken only where an end stands.
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

    /// What the status item knows of each session, as the rows a seamless
    /// update's park stamps on its manifest (`SessionRecord::status_seen`,
    /// `app_update_handoff::stamp_status_seen`): the herald's need, whether
    /// it was told, the hold, with their ages, and the done the owner looked
    /// at — by its task's `since_unix_ms`, since the entry rev `done_seen`
    /// keeps is not carried. A session it knows nothing of has no row.
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

    /// The herald's decision for `session` (a local id or a fleet need's
    /// key) at `current`, and what it posts: the notice through `notify.rs`'s
    /// queue, the one `aterm.log` line, the refusal handed back, and the
    /// menu-bar item dirtied when the row moved ([`Self::herald_session`]).
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
        // Posted BEFORE the line is written: `queued` is only true once the
        // queue took it, and a refusal is the decision's one line instead.
        // `desktop_alerts = false` offers the queue nothing: the need still
        // has its menu-bar row and tab mark, and the line says the setting
        // held the banner back.
        let post = match outcome.notice.as_ref() {
            Some(_) if !alerts => crate::status_item::HeraldPost::Suppressed,
            Some(notice) => match self.post_herald_notice(notice) {
                Ok(()) => crate::status_item::HeraldPost::Queued,
                Err(why) => crate::status_item::HeraldPost::Refused(why),
            },
            None => crate::status_item::HeraldPost::Queued,
        };
        // A refused notice told nobody: the herald takes the post back, so
        // it stays owed (the App's timer retries it) and a machine's fact
        // does not quiet the other tabs as told. A suppressed one is not
        // owed: the person turned the banner off.
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

    /// Post one herald notification through `notify.rs`'s bounded queue — the
    /// consent notice's path: `try_send`, dropped when full. `Err` says why
    /// the queue refused it, for the herald's one `aterm.log` line
    /// ([`crate::status_item::herald_log_line`] makes it a `warn`) — never
    /// logged here with the body, which can carry a box's command (a bug
    /// report attaches the log). Not behind `allow_notifications`: that
    /// opt-in guards PROGRAM-originated escapes, and this text is aterm's
    /// own — behind `desktop_alerts` instead, which [`Self::herald_session`]
    /// checks before calling here (and the delivery thread again, for every
    /// [`crate::notify::NotifyMsg::herald`]). Under `cfg(test)` the notice is recorded instead (and counts as
    /// queued, unless a test armed `HERALD_REFUSAL`), so no test ever runs a
    /// notifier subprocess.
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

    /// THE STATUS GLANCE (§3): the REGISTRY snapshot → one
    /// [`crate::status_item::SessionRow`] per live session
    /// ([`Self::status_session_row`], leaf locks only) → the pure
    /// [`crate::status_item::Glance::build`], with the last sibling scan, the
    /// digest's entries and the alerts switch. No blocking `Terminal` lock
    /// (this runs on chrome-refresh paths): a label's live rungs are read
    /// under a try-lock, the strip's last-read label standing in on
    /// contention. `Exited` sessions are excluded.
    pub(crate) fn status_glance(&self) -> crate::status_item::Glance {
        // What one glance costs (`metrics percentiles`' `status_glance_*`).
        let started = std::time::Instant::now();
        let glance = self.status_glance_built();
        crate::metrics::note_status_glance(
            u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
        );
        glance
    }

    /// [`Self::status_glance`]'s build.
    fn status_glance_built(&self) -> crate::status_item::Glance {
        // The handles are copied out of the registry's guard, which is
        // released before any row is read: a row's label reads it again.
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
        // What needs a look, not now: the digest's entries by SUBJECT (a menu
        // is no place for a 200-character line; Settings ▸ Messages, one click
        // away, shows the whole sentence and its button). The menu and the
        // page are two views of one digest.
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

    /// The menu-bar item owes a rebuild: one is made on the event loop's way
    /// to its next wait (`about_to_wait`), whatever number of changes marked
    /// it this turn (`docs/DESIGN-owner-surfaces-2026-10-16.md` §9 S3). Every
    /// change the item shows marks it here rather than rebuilding the glance
    /// at its own rate.
    pub(crate) fn mark_status_dirty(&mut self) {
        self.status_dirty = true;
    }

    /// Whether the menu-bar item owes a rebuild ([`Self::mark_status_dirty`]),
    /// taking the debt: the event loop's way to its wait asks once.
    pub(crate) fn take_status_rebuild(&mut self) -> bool {
        std::mem::take(&mut self.status_dirty)
    }

    /// Recompute the glance and hand it to the menu-bar status item —
    /// fingerprint-gated, so a change it does not render (an age, a title the
    /// label does not show) hands nothing over. Every glance handed over gets
    /// the next generation and is kept, with the one before it, for the
    /// clicks its menu carries ([`Self::status_pick`]). The status item keeps
    /// the glance and paints its menu from it only when the menu opens, and
    /// re-faces its button only when the count or its colour changes
    /// (`status_item::update`). The done the owner is looking at is marked
    /// seen first ([`Self::note_done_seen_looking`]). No-op with no status
    /// item (headless, off macOS, or pre-first-window).
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

    /// `glance` as the next drawn generation: stamped, and kept with the one
    /// before it ([`Self::status_pick`]).
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

    /// Install the menu-bar status item with the first glance (the first OS
    /// window's attach; never headless) and seed its sibling rows with a
    /// background scan so the first open has them.
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
            // Whose banner a notice arrives as, read off the event loop: the
            // Notify Me hover says it from the next rebuild.
            #[cfg(target_os = "macos")]
            self.learn_notice_voice();
        }
    }

    /// The drawn glance a click's `pick` was painted from — the one the
    /// menu's last open was painted from ([`Self::note_status_painted`]), else
    /// one of the last two handed over — or `None`: a generation no longer
    /// kept, so the click is stale and does nothing.
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

    /// The status menu opened painted from `painted` (`None`: no paint ran,
    /// and the menu still shows the last one): keep it until the next open.
    /// Every tag in the open menu indexes it, and the App hands glances over
    /// while the menu is open — opening it can start a fleet read, whose
    /// start and end are two — so the last two handed over are not enough.
    #[cfg_attr(
        all(not(target_os = "macos"), not(test)),
        expect(dead_code, reason = "the menu-bar status item is macOS's")
    )]
    pub(crate) fn note_status_painted(&mut self, painted: Option<crate::status_item::Glance>) {
        if let Some(glance) = painted {
            self.status_painted = Some(glance);
        }
    }

    /// Read again whose banner aterm's own notices arrive as
    /// (`notify::learn_voice`, off the event loop), and say it: the Notify Me
    /// hover from the next rebuild, and the center's delegate where a center
    /// opened (`Wake::NoticeVoice`). At the status item's install, at each of
    /// its menu's opens (the owner may have changed System Settings ▸
    /// Notifications meanwhile; the next open shows it) and once macOS has
    /// answered Notify Me's ask.
    #[cfg(target_os = "macos")]
    fn learn_notice_voice(&self) {
        let Some(proxy) = self.proxy.clone() else {
            return;
        };
        crate::notify::learn_voice(self.notice_center, move || {
            let _ = proxy.send_event(crate::Wake::NoticeVoice);
        });
    }

    /// The owner went to session `session` from the menu bar or a notice: a
    /// done its task stands at is seen ([`Self::mark_done_seen`]), and leaves
    /// Done for you and its tab's `•`; so does its supervised agent's task
    /// END on the glass (§11.2.3, item 6; [`Self::see_session_end`]).
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

    /// FOCUSING THE TAB ANY OTHER WAY MARKS IT SEEN (§3), by the one rule the
    /// tab's `•` reads too ([`Self::note_done_seen`]): every pane the front
    /// tab of the frontmost window shows, while that window is on screen.
    /// Run on the way to every rebuild of the glance: a tab switch, a window
    /// focus and a task's move each dirty it.
    pub(crate) fn note_done_seen_looking(&mut self) {
        if let Some(front) = self.frontmost_window {
            self.note_done_seen(front);
        }
        // A closed session's memory goes with it.
        let pool = &self.pool;
        self.done_seen
            .retain(|session, _| pool.get(*session).is_some());
    }

    /// The menu-bar item's mark as `aterm ctl chrome`'s `statusitem` line
    /// ([`Self::status_item_line_of`]) of a glance built now.
    #[cfg(test)]
    pub(crate) fn status_item_chrome_line(&self) -> String {
        self.status_item_line_of(&self.status_glance())
    }

    /// The menu-bar item as `aterm ctl chrome` prints it (§3): its mark
    /// ([`Self::status_item_line_of`]), then the menu's rows as the menu's
    /// own builder composes them from that same glance
    /// ([`crate::status_item::compose_status_menu`]; the Fleet row's submenu
    /// from the fleet glance's slot, [`crate::status_item::compose_fleet_menu`],
    /// as that submenu's open composes it), one `statusmenu` line each
    /// ([`crate::status_item::menu_lines`]): what a click on ❯ would show now,
    /// read with no click and nothing of the menu changed — headless too,
    /// where no item is drawn. Off macOS there is no such menu, and the mark
    /// alone is printed.
    pub(crate) fn status_item_chrome_lines(&self) -> Vec<String> {
        self.status_item_chrome_lines_at(
            aterm_time::unix_secs().map_or(0, |s| i64::try_from(s).unwrap_or(0)),
        )
    }

    /// [`Self::status_item_chrome_lines`], the menu's ages as of `now_unix`.
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

    /// The menu-bar item's mark as one introspection line (`aterm ctl
    /// chrome`, §3), from `glance`: the title it shows, how many needs it
    /// counts and how many of them are stuck, how many entries the digest
    /// holds, how many rows stand under Done for you (a worker's done, a
    /// supervised agent's end nobody has looked at: `done=`,
    /// `docs/DESIGN-harness-autopilot-2026-10-15.md` §11.4.1), and the fleet
    /// read's state. The same glance the item is drawn
    /// from, so the cue the owner sees is a cue an agent can read — headless
    /// too, which has no menu bar. `fleet=off` where no fleet glance runs — in
    /// no fleet, headless, or before the item installs — since no fleet is
    /// read there, whatever the book says.
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

    /// Hand Back All (§3's You hold row): lift every person hold, each
    /// through Take Over's own act (`App::hand_back_session`, S5), so the
    /// status item can never lift a slot the person's own Hand Back could
    /// not. A refusal is logged: the row was drawn from a glance that may be
    /// older than the holds. Built off macOS for its test alone: only the
    /// status item calls it.
    #[cfg(any(target_os = "macos", test))]
    fn hand_back_all(&mut self) {
        let held: Vec<u64> = self.person_holds().iter().map(|h| h.session).collect();
        for session in held {
            if let Err(why) = self.hand_back_session(session) {
                aterm_log::info!("status item: Hand Back All: {session}: {why}");
            }
        }
    }

    /// Notify Me When I'm Needed: flip `desktop_alerts` now, and persist it
    /// through the config lane Settings writes (`settings set desktop_alerts
    /// …`). A save the lane refuses is one `aterm.log` line; the live flip
    /// stands for this run.
    #[cfg(target_os = "macos")]
    fn toggle_desktop_alerts(&mut self) {
        let desired = !self.config.desktop_alerts_or_default();
        self.config.desktop_alerts = Some(desired);
        self.apply_desktop_alerts(desired);
        // Turning it ON is the one gesture that asks macOS to let aterm post
        // as itself (`notice_center`): never at launch, never while it is off.
        // (The first notice while it is on asks too, where it was turned on
        // another way: `notify::way_for`.) macOS prompts the first time only;
        // once it has answered, the voice is read again for the hover.
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

    /// Read Result: a finished worker's result as a Markdown document tab on
    /// the front window, raised ([`Self::raise_window`]), which marks its done
    /// seen, and the captured supervised agent's task end with it
    /// ([`Self::see_session_end`]) — only once the tab is asked for: a result
    /// that did not open was not looked at, so its Done for you row and the
    /// bar's `❯•` stay.
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
        // A painted result names the end it shared, not an end that arrived
        // while that menu stayed open. No end captured means none was read.
        if result.end_serial.is_some_and(|serial| {
            self.harness_glass
                .get(&result.session)
                .and_then(crate::harness_glass::SessionGlass::end)
                .is_some_and(|end| end.serial == serial)
        }) {
            self.see_session_end(result.session);
        }
    }

    /// Someone looked at session `session`'s supervised agent's task END on
    /// the glass (§11.2.3, item 6): the menu bar's Go, or its Read Result on
    /// a worker whose task row is that end's Done for you row. Seen; where
    /// that moved anything, every strip is stamped again and the glance
    /// dirtied ([`Self::restamp_for_end`]).
    fn see_session_end(&mut self, session: u64) {
        if self
            .harness_glass
            .get_mut(&session)
            .is_some_and(crate::harness_glass::SessionGlass::see_end)
        {
            self.restamp_for_end();
        }
    }

    /// The status menu is opening (`Wake::StatusMenuOpening`) painted from
    /// `painted`, which its clicks resolve against until the next open
    /// ([`Self::note_status_painted`]); a fresh sibling scan, and a fleet
    /// read when the last is stale, whose results the NEXT open shows.
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

    /// A DEVELOPMENT BUILD'S STAND-IN FOR THE PERSON CLICKS THE MENU-BAR ROW
    /// AT `at` (`aterm ctl person menubar press <at>`,
    /// `docs/DESIGN-harness-autopilot-2026-10-15.md` §12.5.2): the menu
    /// composed as `chrome` composes it ([`Self::status_item_chrome_lines`]),
    /// from a glance stamped with the next generation and kept as the one
    /// the menu was painted from ([`Self::note_status_painted`], what a
    /// menu's open keeps); the row at that path ([`crate::status_item::row_at`],
    /// the path `chrome` prints) taken, its action round-tripped through its
    /// tag as an `NSMenuItem` carries it, and posted as the AppKit relay
    /// posts a click (`post`, `Wake::StatusAction`), so
    /// [`Self::dispatch_status_action`] runs for it as for a click, on the
    /// loop's next turn. The reply names the row's label. Refused: no row
    /// there, a row that does not act (an information line, a separator, a
    /// submenu's row), a disabled one, and off macOS, where there is no
    /// menu-bar item. A menu open's other work (a sibling scan, a fleet
    /// read, the notice voice) is not a click's, and is not done.
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

    /// Dispatch one click from the menu-bar status item (the
    /// `dispatch_menu_action` twin for `Wake::StatusAction`). Every payload is
    /// an index into the glance its menu was painted from
    /// ([`Self::status_pick`]): one that does not resolve does nothing.
    #[cfg(target_os = "macos")]
    pub(crate) fn dispatch_status_action(&mut self, action: crate::status_item::StatusAction) {
        use crate::status_item::StatusAction;
        match action {
            // The card on the frontmost window, raised: a status item click
            // never makes aterm the active app.
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
            // `Dismiss all` on the digest: the same filing the Settings page
            // and `notice dismiss review` make.
            StatusAction::DismissExceptions => {
                let _ = self.dismiss_exceptions();
            }
            // A "To look at" line: the page at that entry (expanded, past a
            // filter that would hide it, scrolled to it), in the frontmost
            // window, else the first.
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

    /// A "To look at" line: Settings ▸ Messages at that entry, in the front
    /// window (else the first), and that window raised — a status item click
    /// never activates aterm, so from another app nothing would seem to
    /// happen ([`Self::raise_window`]). `false` when it opened nowhere.
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn show_status_exception(&mut self, id: u64) -> bool {
        let Some(wid) = self.open_messages_entry_anywhere(id) else {
            return false;
        };
        self.raise_window(wid);
        true
    }

    /// TAKE ME THERE: the ❯ menu's Go, and a click on a notice about
    /// `session` (`Wake::NoticeClicked`: aterm's own macOS notice, a Windows
    /// balloon) — its window raised, its tab selected, its pane given the
    /// keyboard ([`Self::focus_session_window`]), and a done its task stands
    /// at seen. A session no window shows goes nowhere.
    #[cfg_attr(
        all(not(target_os = "macos"), not(windows), not(test)),
        expect(
            dead_code,
            reason = "the menu-bar status item and a notice's click are macOS's and Windows'"
        )
    )]
    pub(crate) fn go_to_session(&mut self, session: u64) {
        self.focus_session_window(session);
        self.note_session_done_seen(session);
    }

    /// Focus the tab displaying `session`, and the pane showing it there,
    /// and raise its window — the body every session-focusing row, the
    /// status item's Go and the needs pill share, so the keyboard lands on
    /// the session named (an answer typed to an ask in a split goes to its
    /// pane). Returns whether the session was found in any window.
    pub(crate) fn focus_session_window(&mut self, session: u64) -> bool {
        self.focus_session_preferring(None, session)
    }

    /// [`Self::focus_session_window`], in window `prefer` first where it
    /// shows `session`: the needs pill names a tab of the window it is drawn
    /// in, so a session shown in two windows is focused where its pill was
    /// clicked, and in the first window showing it only where that one no
    /// longer does.
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

    /// Give `session`'s pane (its view `view`) the keyboard in window
    /// `wid`'s active tab, the way a click in it does
    /// (`focus_pane_under_pointer_from_plan`): selecting a split tab leaves
    /// the keyboard on whichever pane last had it, so a row that goes to a
    /// need would land the owner beside it — a "yes" typed to the ask would
    /// reach another session, maybe another agent — and the needy pane would
    /// never count as looked at: its notices would keep posting and its
    /// done would stay unseen. A pane that already has the keyboard (a
    /// single-pane tab) moves nothing.
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

    /// Bring window `wid` to the front of every app and Space: de-miniaturize
    /// it, then focus it, which activates aterm (`focus_window` sends
    /// `activateIgnoringOtherApps`). A status item click does not make aterm
    /// the active app, so every act of the item that opens or selects a tab —
    /// Go, and each Read that opens a document tab — raises through here, or
    /// the owner, in another app, sees nothing happen (§3); a "To look at"
    /// line too ([`Self::show_status_exception`]). A window with no
    /// OS surface (headless) raises nothing. A test build records `wid`
    /// ([`take_raised_windows`]): a test App has no OS window to ask.
    pub(crate) fn raise_window(&mut self, wid: WindowId) {
        #[cfg(test)]
        RAISED_WINDOWS.with(|raised| raised.borrow_mut().push(wid));
        if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.clone()) {
            if w.is_minimized().unwrap_or(false) {
                self.pending_deminiaturize_focus = Some(wid);
            }
            w.set_minimized(false);
            w.focus_window();
        }
    }

    /// `raise <sid>` (the `Wake::RaiseSession` body): the status item's Go
    /// shape ([`Self::focus_session_window`]) for an ARBITRARY session —
    /// find the window hosting it, select its tab, focus the window. `Err` when
    /// no window shows the session (a view the strips dropped) so the wire verb
    /// reports honestly instead of silently succeeding. On a headless instance
    /// the one logical window DOES host the session, so this selects its tab and
    /// the OS-focus half is simply a no-op (there is no surface to raise) — the
    /// consistent twin of an aimed `spawn`/`tab`, which likewise resolve window
    /// 0 there rather than refuse. ONE body with the menu path
    /// ([`Self::focus_session_window`]) — a raise from the wire and a raise from
    /// a menu row cannot drift, de-miniaturize belt included.
    ///
    /// SPEC (design §9: the window-routing obligations): this real seam IS the
    /// `WindowRouting.RaiseSession` action — a pure focus re-point of
    /// `frontmost` onto an already-allocated id (the host window's), minting
    /// and destroying nothing and never exiting. The `#[refines]` keeps
    /// `window_routing` fully bound now that the act exists; Tier-1 drives this
    /// seam in `window_routing_conformance` (headless, the OS-focus half is a
    /// windowed concern, so the projected step may be the model-admitted
    /// `frontmost' = frontmost` stutter). PROJECTION
    /// (`aterm_gui::App::project_window_routing`): the
    /// `window_routing_conformance::project` tuple.
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

    /// SHOW (Settings ▸ Harness's session row, design §10.3.1): raise
    /// `session`'s window and tab and focus ITS pane — two agents in one tab
    /// are two rows, and Show on the right one's row must not land the person
    /// on the left one. That is [`Self::focus_session_window`], the body every
    /// session-focusing row shares: its pane takes the keyboard as a click on
    /// it would. `false` where no window shows the session.
    pub(crate) fn raise_session_pane(&mut self, session: u64) -> bool {
        self.focus_session_window(session)
    }

    /// Close the PANE showing `session` — its tab only when that pane was the
    /// tab's last — reporting how far the close got.
    ///
    /// ONE PANE, NOT THE TAB. Measured 2026-09-22 (audit): in a tab split three
    /// ways, `@<pane> close` answered `OK closed s-…` and the WHOLE tab went —
    /// `panes` lost the tab (15 → 14), both siblings answered `ERR no such
    /// session`, and `exits` had three `ctl-close` rows within 4 ms. One close on
    /// one pane killed three shells: the verb ran the tab strip's ✕
    /// (`close_tab_at`, every pane as a unit — right when a tab had one pane, and
    /// never revisited once it could hold three), and because that close never
    /// exits a window with other tabs, no busy guard stood between it and the
    /// siblings' jobs. A split pane now takes the Cmd-W collapse
    /// ([`Self::close_split_pane_by_id`]), which retires that pane alone; the
    /// whole-tab path is kept for the tab whose ONE pane this is, where the two
    /// closes are the same close and the last-tab deferral below lives.
    ///
    /// THE GUARD IS THE PANE'S. Every close here that leaves its window standing
    /// — a split collapse, a terminal leaf beside a native view, the one pane of
    /// a tab that is not the window's last — reads the job of the pane being
    /// closed ([`Self::pane_close_hangs_up_a_job`]) and nothing else, before
    /// anything moves. The close that exits the window keeps the window-exit
    /// confirm inside `close_tab_at`, which reads a window holding exactly this
    /// pane: the same guard, in the last-tab confirm's words.
    ///
    /// DIFF (like spawn): the store is the evidence — if the session is gone
    /// afterward the close happened; if it survives, a guard refused it and we
    /// say so. The one case that diff cannot read is a LAST-tab
    /// close: it is DEFERRED (the window teardown that retires the session needs
    /// an `ActiveEventLoop` the close paths do not hold), so the session is still
    /// registered here through no fault of its own. That is
    /// [`CloseProgress::Deferred`], not a refusal — answer it with
    /// [`Self::close_session_verdict`] after running the escalation, or an idle
    /// session gets `ERR close refused (a running job armed the last-tab
    /// confirm)` for a close that had simply not happened yet. A session that
    /// survives a pane that DID go is held by another view of it, and the reply
    /// names that viewer ([`Self::surviving_session_reason`]) rather than a
    /// refusal nothing raised.
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
            // The tab's one pane: closing it IS closing the tab.
            ws.pending_close = true; // last tab → the window closes with it
            deferred = Some(wid);
        }
        if !self.session_registered(session) {
            Ok(CloseProgress::Retired)
        } else if let Some(wid) = deferred {
            Ok(CloseProgress::Deferred(wid))
        } else if self.view_store.get(location.view).is_none() {
            // The pane's view was retired, and its session with it only if this
            // was the last view: another viewer keeps it (one close per viewer).
            Err(self.surviving_session_reason(wid, session))
        } else {
            Err(crate::app_window::WIRE_CLOSE_REFUSED.to_string())
        }
    }

    /// Whether tab `tab_id` of window `wid` holds more than one leaf — read from
    /// the canonical tree, the fact that makes a pane close a collapse (Cmd-W's)
    /// rather than the tab's close (the strip ✕'s).
    fn tab_has_sibling_panes(&self, wid: WindowId, tab_id: crate::tab_model::TabId) -> bool {
        self.windows
            .get(&wid)
            .and_then(|ws| ws.tab_set.get(tab_id))
            .is_some_and(|tab| tab.root.len() > 1)
    }

    /// The wire `close` of one pane of a SPLIT terminal tab: the Cmd-W collapse
    /// ([`Self::close_active_tab`]) aimed by session instead of by focus, so the
    /// pane need not be focused nor its tab active. It shares the gesture's code
    /// path — [`pane::PaneTree::close_pane`], then [`Self::apply_close_outcome`]
    /// (the sibling grows into the space with its shell untouched; the view, the
    /// pool and the registry retire in that order) — and its reopen record, so a
    /// pane the wire closed collapses, tears down and comes back exactly as one
    /// Cmd-W closed. The tab survives by construction (a sibling remains), so
    /// nothing here can exit the window; the busy guard is the PANE's own and
    /// ran in the caller before this. `Ok(())` = the pane collapsed; whether its
    /// session then left the registry is the caller's diff. `Err` = it did not,
    /// and why.
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
            // QUEUED onto the pending handoff's teardown and applied once that
            // cancellation lands; the pane is still here right now, so the reply
            // must not read as "closed" (`close_tab_at` says the same of a tab).
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
        // `close_pane` falls back to the FOCUSED pane when the session it is handed
        // is not in the tree. Aimed by session, a projection that has lost this
        // session must close nothing, never whichever pane happens to have focus.
        let outcome = self.windows.get_mut(&wid).and_then(|ws| {
            let tree = ws.layouts.get_mut(tab)?;
            tree.contains(session).then(|| tree.close_pane(session))
        });
        // A `LastPane` here would mean the projection lost its siblings between
        // the canonical check and this collapse; `close_pane` mutates nothing on
        // that path, so nothing happened and the reply says so.
        let Some(outcome @ pane::CloseOutcome::Collapsed { .. }) = outcome else {
            return Err("session view changed during close".to_string());
        };
        // `Collapsed` never closes the window: the bool is the `LastPane` arm's.
        let _ = self.apply_close_outcome(wid, tab, outcome);
        // The arm REFUSES the retirement when the closed leaf survived the
        // re-projection (the ownership-order seam it documents): the pane, its
        // view and its PTY are all still here, so say so rather than leave the
        // caller's diff to blame a co-viewer. It is the same `ViewId` the close
        // aimed at — a Cmd-Shift-O twin of the session is a different view.
        if self.view_store.get(location.view).is_some() {
            return Err("close refused (the pane is still open)".to_string());
        }
        if let Some(recovery) = recovery {
            self.retain_closed_view(recovery);
        }
        Ok(())
    }

    /// Whether closing the pane showing `session` would hang up a foreground JOB:
    /// the session is busy by the one predicate every close path reads
    /// (`session_foreground_busy`: the PTY's foreground pgrp, or the shell's open
    /// 133;C block) AND this pane holds its last pool view, so the collapse drops
    /// the `Session` and its PTY master with it. A Cmd-Shift-O co-viewed session
    /// keeps running in its other viewer — closing this pane only detaches a
    /// view — so it never counts, exactly as `window_has_foreground_job` counts
    /// only the sessions a window close would actually SIGHUP.
    fn pane_close_hangs_up_a_job(&self, session: u64) -> bool {
        self.pool.views(session).is_some_and(|views| views <= 1)
            && self.session_foreground_busy(session)
    }

    /// [`Self::pane_close_hangs_up_a_job`] for every pane of window `wid`'s tab
    /// `i` at once — the whole-tab close `tab close` makes. Read from the
    /// canonical tree, so a terminal leaf beside a native view counts as well.
    /// A session is at risk when this tab holds ALL its pool views (a twin in
    /// another tab or window keeps it alive), the rule `window_has_foreground_job`
    /// applies to a window. An index no tab holds hangs up nothing.
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

    /// Quit-safety for a PANE close that leaves its window standing: a split
    /// collapse, a terminal leaf beside a native view, or the one pane of a tab
    /// that is not the window's last. A UI gesture asks through
    /// `view_close_allowed` (off macOS) instead of this wire guard, a `--headless` instance never
    /// confirms anything, and `Programmatic` (the operator's typed Stop) is an
    /// explicit instruction, busy or not. The wire `close` is the one policy that
    /// REFUSES instead of asking (`WireRefuseBusy`, `confirm_destructive_close`),
    /// and it used to reach a pane only through the whole-tab close, whose guard
    /// read the WINDOW — every pane's job, or none at all when the tab was not
    /// the last. With the pane close its own gesture the guard is the pane's:
    /// `busy` is [`Self::pane_close_hangs_up_a_job`] of the pane being closed,
    /// never a sibling's job, which the collapse leaves running.
    fn pane_close_allowed(&self, busy: bool) -> bool {
        if self.headless {
            return true;
        }
        !(busy && self.close_confirm == crate::app_window::CloseConfirm::WireRefuseBusy)
    }

    /// Why a close that DID retire its pane in window `wid` left `session`
    /// registered: another pool view still holds it (one close per viewer).
    /// Another WINDOW is worded as the last-tab path words it
    /// ([`Self::deferred_close_reason`]), so a driver matching on the reply
    /// meets one spelling of that fact. A view in the SAME window is the one
    /// the wire cannot otherwise tell apart — a Cmd-Shift-O share moved back
    /// with Cmd-Shift-M leaves two tabs of one window on one session — and "another
    /// window" would send the caller looking for a window that is not there.
    /// The last arm is that path's last arm for a pane: registered, shown nowhere.
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

    /// The wire verdict of a close whose [`CloseProgress`] is known, read AFTER
    /// the caller ran the deferred teardown (`escalate_pending_close`). The
    /// store is still the evidence: a `Deferred` close that retired its session
    /// is `OK closed`, and one that did not says WHICH of the three deferred
    /// outcomes it is looking at (see [`Self::deferred_close_reason`]) instead
    /// of blaming the last-tab confirm, which never fired.
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

    /// Why a DEFERRED close left `session` registered, worded from what this
    /// process can actually SEE rather than from the one case that was first
    /// implemented.
    ///
    /// The distinction is load-bearing because a Cmd-Shift-O co-viewed session
    /// takes the SAME path as a stuck teardown: `close_session_by_id` aims at
    /// the LOWEST window id holding the session, that window's teardown runs to
    /// completion, and the session is STILL registered — `close_window_logical`
    /// deregisters only when its `detach_session_view` dropped the last pool
    /// view, and the other window's view is still attached. Answering "the
    /// window teardown has not run" there states a falsehood about a teardown
    /// that did run, and points the caller at a window that no longer exists.
    ///
    /// So: another window holding the session is the reason the close did not
    /// take, whether or not the teardown is also outstanding — it is the one
    /// that survives the escalation and the one the caller must act on. Failing
    /// that, the deferred window still being here IS the outstanding teardown (a
    /// native or document shutdown barrier held `close_window`). The last arm is
    /// neither: the window is gone and no window shows the session, so a pool
    /// view outlived every viewer — reported as what it is instead of borrowing
    /// one of the other two explanations.
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

    /// Whether `session` is still in the process-wide registry — the diff both
    /// close paths read to decide whether anything actually died.
    fn session_registered(&self, session: u64) -> bool {
        self.store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .by_local(session)
            .is_some()
    }

    /// Open a new tab in window `owner` (window-aware: the tab-strip `+` of a
    /// non-frontmost window opens there, not in the frontmost). The new session is
    /// stamped with `owner` so its output/exit/bell route back to THIS window.
    ///
    /// TRUST anchor: the `NewTab` action of the ty-proven `tab_strip` machine
    /// (`tab_strip_model()`) — appends a tab and re-syncs the owner's native strip,
    /// once the shell spawned off the loop is up ([`Self::install_tab_session`]).
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

    /// [`open_tab_in`] with an optional CWD override. `None` keeps the default:
    /// inherit the focused pane's cwd so a Cmd-T tab opens where the user is.
    ///
    /// The shell is spawned off the event loop ([`crate::tab_spawn`]): this turn
    /// books the tab, and the tab is appended when its session comes back.
    pub(crate) fn open_tab_in_cwd(&mut self, owner: WindowId, cwd_override: Option<&str>) {
        self.open_tab_off_loop(owner, cwd_override, crate::tab_spawn::TabSpawnThen::Nothing);
    }

    /// Install an already-spawned `session` as a NEW last tab of window `owner` and
    /// switch to it — what a tab spawned off the loop lands as
    /// ([`Self::install_tab_session`] under the focused pane's parent), for the
    /// headless tests that drive that SAME body with a stub session
    /// (`push_stub_tab`). `session.id` must be the minted `next_session_id`, which
    /// this bumps.
    ///
    /// The new tab moves the owner's front session, so when `owner` is the front
    /// window the global control/notify handle follows it here, in the seam that
    /// moved it (`active_handle_model`; Tier-1: `active_handle_conformance`).
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

    /// The family-tree parent of a tab opened in `owner`: the FOCUSED pane's
    /// session of that window (a user-opened tab is a child of the pane it was
    /// opened from).
    pub(crate) fn new_tab_parent(&self, owner: WindowId) -> Option<aterm_session::SessionId> {
        self.front_terminal(owner)
            .and_then(|terminal| self.pool.get(terminal.session))
            .map(|s| s.ctx.self_id.clone())
    }

    /// [`Self::install_new_tab`] for a session whose id is already reserved (a tab
    /// spawned off the event loop, [`crate::tab_spawn`]): register it under
    /// `parent`, append its single-pane tab to `owner` and switch to it.
    pub(crate) fn install_tab_session(
        &mut self,
        owner: WindowId,
        session: crate::Session,
        parent: Option<aterm_session::SessionId>,
    ) {
        let id = session.id;
        // P1.1: register in the process-wide registry (additive index) so a
        // cross-session `@<selector>` verb can reach this tab.
        Self::register_session(&self.store, &session, parent);
        let layout = pane::PaneTree::new(id);
        let tab = crate::register_terminal_tab(&mut self.tab_ids, &mut self.view_store, &layout)
            .expect("tab/view identity space");
        self.pool_insert(session);
        // Append a fresh single-pane tree (one leaf) and bump the owner window's
        // index in lockstep (keeps `layouts.len() == tabs.count`).
        if let Some(ws) = self.windows.get_mut(&owner) {
            ws.layouts.push(layout);
            ws.tabs.add();
            ws.tab_set.push(tab).expect("fresh tab id");
        }
        self.resync_active_or_window(owner);
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

    /// [`Self::cycle_tab`] on an explicit window — the aimed `@<sid> tab next|prev`
    /// (design S3) cycles the window hosting the session, front or not. The same
    /// deterministic wrap; re-mirrors like [`Self::switch_tab_in`] (the global
    /// handles follow only when `wid` IS the front window).
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

    /// Apply a control-socket `tab` verb ([`TabAction`]) to the FRONT window and
    /// return the resulting `(active_index, tab_count)` for the verb's reply. Driven
    /// by [`Wake::TabCmd`] on the main loop (the sole `App` mutator). Each action
    /// reuses the EXISTING command path — `New` => [`Self::open_tab`] (same as File ▸
    /// New Tab / the toolbar "+"), `Select(n)` => [`Self::switch_tab`], `Next`/`Prev`
    /// => [`Self::cycle_tab`] — so the verb adds no parallel tab logic. With no front
    /// window it REFUSES rather than reporting the `(0, 0)` it used to, which a
    /// caller reads as a tab count.
    pub(crate) fn apply_tab_cmd(&mut self, action: TabAction) -> Result<(usize, usize), String> {
        match self.frontmost_window {
            Some(front) => self.apply_tab_cmd_in(front, action),
            None => Err(NO_FOCUSED_WINDOW.to_string()),
        }
    }

    /// [`Self::apply_tab_cmd`] on an explicit window — the aimed `@<sid> tab …`
    /// (design S3) drives the window hosting the session, front or not. Each
    /// action reuses the window-aware command path its front-window twin
    /// delegates to (`open_tab_in` / `switch_tab_in` / `cycle_tab_in` /
    /// `close_tab_via_verb` / `move_tab`), so the aimed verb adds no
    /// parallel tab logic either; an unknown `wid` reports `(0, 0)`.
    ///
    /// REFUSALS REACH THE WIRE. This used to return the post-state pair and
    /// nothing else, so `cmd_tab` minted `OK <active> <count>` from it no matter
    /// what happened — and two different declines hid behind that OK. An
    /// out-of-range index was a silent no-op whose reply, for `move`, is
    /// BYTE-IDENTICAL to an honoured move; and a tab host that refused the close
    /// (a native document waiting on a durable checkpoint) had its reason dropped
    /// by `close_tab_at`, whose `bool` means "was the last canonical tab", not
    /// "it closed". The GUI user was told both times, through the tab's attention
    /// indicator and banner; the socket caller was told `OK`.
    ///
    /// The index checks are done HERE rather than inside each command path
    /// because those paths are shared with the keyboard, menu and tab-strip
    /// gestures, where a stale index IS an ordinary no-op and must stay one. It
    /// is only the wire that needs to hear about it.
    pub(crate) fn apply_tab_cmd_in(
        &mut self,
        wid: WindowId,
        action: TabAction,
    ) -> Result<(usize, usize), String> {
        // An unknown window keeps its documented `(0, 0)`: the aimed verb
        // resolves the window before it gets here (`hosting_window`), so this is
        // an internal contract, not a wire answer.
        let Some(count) = self.windows.get(&wid).map(|ws| ws.tab_set.len()) else {
            return Ok((0, 0));
        };
        let no_such = |i: usize| crate::control::no_such_tab(i, count);
        // A refusal raised anywhere beneath this action lands in the same channel
        // the menu `invoke` seam uses; clear it first so an EARLIER action's
        // refusal is never reported against this one.
        self.pending_action_refusal = None;
        match action {
            // The wire verb counts the new tab once it lands
            // (`App::tab_new_answered`); here it is booked like Cmd-T's.
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
                // `from == to` is not a refusal: the arrangement the caller asked
                // for is the one that holds.
                self.move_tab(wid, from, to);
            }
        }
        if let Some(why) = self.pending_action_refusal.take() {
            return Err(why);
        }
        // Report the canonical mixed-tab state. The terminal-only `TabIndex` is a
        // compatibility projection and can have a different count/index whenever
        // native tabs are present.
        Ok(self.windows.get(&wid).map_or((0, 0), |ws| {
            (ws.tab_set.active_index().unwrap_or(0), ws.tab_set.len())
        }))
    }

    /// Close window `wid`'s tab `which` (or its ACTIVE tab when `None`) for the
    /// `tab close [N]` verb and the native × button's [`Wake::CloseTab`]. `wid` is
    /// the front window for the flagless verb and the window hosting `@<sid>` for
    /// the aimed one (design S3). Reuses [`Self::close_tab_at`] (the SAME whole-tab
    /// close the renderer strip's `✕` and the tab-strip click take); if that was
    /// the window's LAST tab it flags THAT window's `pending_close` so the `Wake`
    /// handler's `escalate_pending_close(el)` tears the window down (the verb /
    /// button paths have no `ActiveEventLoop`), exactly like a tab-strip close.
    ///
    /// A tab that is NOT the window's last gets the `close` verb's pane guard
    /// first ([`Self::pane_close_allowed`] over [`Self::tab_close_hangs_up_a_job`]):
    /// `close_tab_at` guards only the close that exits the window, so on a busy
    /// non-last tab `tab close N` answered `OK` and hung up the job that `close`
    /// of the same pane refuses (review of the 2026-09-22 guard). Refused, it
    /// answers [`crate::app_window::WIRE_TAB_CLOSE_REFUSED`] and nothing moves.
    pub(crate) fn close_tab_via_verb(&mut self, wid: WindowId, which: Option<usize>) {
        // Exit-ledger attribution: a control-socket `tab close`. The wire bracket
        // (`App::tab_cmd_bracketed`) has already opened the scope with the
        // caller, and the outermost scope wins, so this `by=-` stands only for a
        // caller that brought none; the native ✕'s `Wake::CloseTab` takes
        // `close_tab_at`.
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

    /// Windows showing this session as some tab's FOCUS — the tabs whose label
    /// and composed tooltip are built from it.
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
    /// title rung, and presents the label — none of which should happen
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
        // WHOLE-STRING SURFACES. `titles` are the strip's labels — presented
        // with no title cap, because the strip fits them itself, sibling-aware
        // (`ChromeSurface::TabStrip`). Everything pushed from here paints a
        // label with no fit of its own — the native toolbar's chip and its
        // accessibility string, the tooltip / context-menu header, the `tabs`
        // and `chrome` introspection lines — so those take the cap here, once
        // (`capped_labels`: borrowed, no allocation, unless a label is actually
        // past it). The strip's own vector is what this function returns for
        // the in-grid fingerprint, and what the drift cache
        // (`tab_chrome_titles`) compares against the resolved rung; both keep
        // the uncapped label, or a long clean title would never compare equal
        // to itself and every epoch tick would rebuild the chrome.
        let mut chrome_titles = crate::chrome_label::capped_labels(&titles);
        // Composed per-tab chrome (tooltip + context-menu model) rides the same
        // refresh: epoch-cached, so a steady prompt pays one leaf-lock read per
        // terminal tab (see `composed_session_chrome`), and the strip receives
        // titles/metadata/chrome as ONE consistent snapshot.
        let ext = self.tab_chrome_ext(wid, &chrome_titles);
        // The §4 connection mark rides the same consistent snapshot: stamped
        // from the live edge tables here, so `metadata` below carries it to
        // both strips (and into the in-grid fingerprint) at once.
        self.stamp_tab_connection_roles(wid);
        let metadata = self.tab_strip_metadata(wid);
        // WHO DRIVES (owner surfaces §5, S9): the team marks ride the titles
        // the strip shows — `↳ ` before a worker's, ` ⇉N` after a manager's —
        // and the exact per-tab vector inspection reads, never the tab's own
        // name (`titles`: the drift cache, the hover's identity line above).
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
            // This is the strip's exact title vector (the toolbar below receives
            // its whole-string projection). Cache it at the existing push point so
            // a later background spinner phase can be rejected before
            // `refresh_window_tabs` takes every tab's terminal lock and allocates
            // another title/tooltip/metadata set. Structural refreshes also land
            // here, ensuring the first post-switch phase is coalescible.
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
            // exact per-tab vector handed to the toolbar independently.
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
            // The window's needs pill rides the same snapshot (§5).
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
        // What was pushed, for a headless `chrome` to print (dev builds).
        #[cfg(any(test, debug_assertions))]
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.tooltips_pushed = tooltips;
        }
        // Title drift funnels through here (every structural change + each
        // prompt relabel), so this is where the menu-bar status item's window
        // rows and the operator's running detail follow the titles.
        // Fingerprint-gated: a steady prompt costs one glance compare, no
        // AppKit work.
        self.mark_status_dirty();
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

    /// Cmd-W: close the FOCUSED pane of the FRONTMOST window's active tab. Returns
    /// `Some(window)` — the window whose last tab just closed — iff that was the LAST
    /// pane of the LAST tab, so the caller escalates to closing THAT window (the
    /// frontmost), not whichever window an input event was stamped for. Returns
    /// `None` otherwise. Closing a pane in a SPLIT tab collapses the split onto its
    /// sibling (the sibling — and its reader thread — survive); closing the only pane
    /// of a non-last tab closes the tab. Honors `--hold` ONLY for the implicit close
    /// on a session's own EOF (see `close_session`); an explicit Cmd-W always closes.
    pub(crate) fn close_active_tab(&mut self) -> Option<WindowId> {
        // Exit-ledger attribution: Cmd-W / menu Close, by the human.
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
        // M2 quit-safety: this closes the focused PANE, which exits the window only
        // when it is the last pane of the last tab. Ask before such a close while a
        // job runs, exactly like the red ✕ / Cmd-Q — so Cmd-W
        // can't silently SIGHUP an in-flight build/AI run. A split-pane collapse or a
        // non-last tab asks the same while its own pane is busy (`view_close_allowed`).
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
            // The one pane of its tab: closing it closes the tab, and says so.
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
        // `true` = the frontmost window's last tab closed → tell the caller WHICH
        // window to escalate-close (always the frontmost we operated on).
        let closes_window = self.apply_close_outcome(window, tab, outcome);
        if collapsed && let Some(recovery) = recovery {
            self.retain_closed_view(recovery);
        }
        if closes_window {
            // Same deferral as `close_tab_at`: the caller sets `pending_close`
            // and the window tears down a later turn, so carry the Cmd-W / menu
            // Close attribution (the live `CloseAttribution` scope opened at the
            // top of this fn) onto the window for the deregister to re-enter.
            self.stash_deferred_close_attribution(window);
        }
        closes_window.then_some(window)
    }

    /// Replay a pre-rollback focused-view close against stable identities.  The
    /// user may have changed focus while the worker was aborting, so temporarily
    /// target the originally requested leaf, run the ordinary close transaction,
    /// then restore any still-live newer selection. Also the replay of a busy
    /// pane's close once its confirm banner is answered (`CloseReplay::Pane`).
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
    /// selection/reordering cannot redirect it to a different tab. Also the
    /// replay of a busy tab's close once its confirm banner is answered
    /// (`CloseReplay::Tab`).
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

    /// Close one whole native or heterogeneous tab. Every leaf's app readiness and the
    /// all-document durability batch are proven before any topology, runtime, pool, or
    /// document ownership edge changes.
    pub(crate) fn close_active_native_tab(&mut self, wid: WindowId) -> Result<(), String> {
        self.close_active_native_tab_keeping(wid, None)
    }

    /// [`Self::close_active_native_tab`], then `keep` in front where it is given and
    /// still stands: a BACKGROUND tab closed ([`Self::close_tab_at`] selects it first,
    /// so its app's close preflight and any refusal banner show on it) hands the front
    /// back to the tab that was there, in the same topology edit, before any sync. Left
    /// to the removal's neighbour rule, the neighbour came to the front: the strip's ✕,
    /// the tab menu and `ctl tab close` on a Settings tab behind moved the selection,
    /// and a looked-at window saw the neighbour's unseen end (the final check of
    /// 2026-10-20, W2).
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
            // Deferred last-tab close (native/heterogeneous tab): carry the
            // gesture's attribution for the later `close_window` teardown.
            self.stash_deferred_close_attribution(wid);
        }
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
            // The attempt's completion replays this exit, and the replay's
            // retire writes the log's close. That completion settles it,
            // not the end of this exit's handling.
            self.log_closes_deferred.insert(id);
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
    /// `Exited`, wake every control verb parked on it, then close it in EVERY
    /// window that views it. A CO-VIEWED
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
    /// An interactive shell that failed at start closes nothing: its pane stays
    /// with one line saying how it ended ([`Self::shell_failed_at_start`]).
    ///
    /// THE SESSION LOG'S CLOSE IS DUE while this runs (`SessionLog::note_close_due`,
    /// told before the session reads `Exited`), so an `await log` parked on it
    /// waits for the close the retire writes rather than answer `ERR exited`
    /// over it. When nothing it returns will retire the session (a kept pane,
    /// a pane no window shows) it settles that at its end
    /// ([`Self::settle_log_close`]). A caller that closes the returned windows
    /// settles after closing them, in case a barrier refused a close. An exit
    /// that a pending update handoff deferred stays due past both
    /// (`App::log_closes_deferred`): the attempt's completion replays it, and
    /// that retire writes the close. The completion settles it after its
    /// replay, whether or not the replay ran (`App::finish_update_handoff`).
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
        // Every parked observer (`await`, `ready`, `wait`, `subscribe`) was woken
        // by the `state-change` record that write made, and reads `Exited`, a
        // kept pane's too.
        // Its program is gone, kept pane or not: what is parked on it ends now
        // (`SessionFabric::close`).
        if let Some(ended) = self.pool.get(session) {
            ended.ctx.fabric.close();
        }
        // The exit ledger takes the child's status now, before teardown discards it.
        self.note_shell_exit_for_ledger(session);
        // A shell that failed at start keeps its pane in every viewer, the way
        // `--hold` keeps one; otherwise its error shows for a frame and the tab
        // (with one tab, aterm itself) is gone with nothing saying why.
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
        // A window to close retires the session with it. Anything else is
        // over now, except an exit a handoff deferred, which its completion
        // settles (`settle_log_close` passes over it).
        if to_close.is_empty() {
            self.settle_log_close(session);
        }
        to_close
    }

    /// THE EXIT HANDLING OF `session` IS OVER ([`Self::exit_session_logical`]
    /// and, after it, the closes of the windows it returned): a close the
    /// retire did not write by now is not coming from it. Clears the log's
    /// `close due` and, if it still held (nothing retired the session), wakes
    /// the verbs parked on it, so an `await log` answers `ERR exited` now
    /// instead of at its deadline. A retired session is not pooled any more
    /// and its close already cleared it: nothing to do. An exit that an update
    /// handoff deferred is not settled here (`App::log_closes_deferred`): the
    /// attempt's completion replays it and settles it after that replay, so a
    /// waiter is answered by the close the replay writes.
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

    /// The start of a fresh interactive shell that may have failed at start: it
    /// closed its PTY within [`SHELL_START_WINDOW`] of its spawn, never drew a
    /// prompt (no shell-integration prompt mark), and no input was driven into
    /// its PTY since the spawn — no key, paste, report or control verb's bytes
    /// (`SinkWriter::driven_input`). aterm's own replies to the shell's queries
    /// do not count: fish and powerlevel10k ask for DA, the cursor position and
    /// colours as they start, and a shell whose rc fails after asking still
    /// failed at start. `None` for everything else: a `-e` command or a shell
    /// adopted across an update (neither records a start), a later exit, a
    /// shell someone typed into or that got to its prompt, and every pane under
    /// `--hold` (which keeps every pane its own way).
    ///
    /// Measured to the reader's EOF stamp (`Session::pty_closed_at`), not to
    /// now, so the delay between the close and this exit handler (aterm's own
    /// cold launch, a busy loop) never counts. The child's own slow start does,
    /// which is why the window is a hang detector ([`SHELL_START_WINDOW`]).
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

    /// How `session`'s child ended, for every reader on the exit path: the
    /// classifier, the exit ledger and the failed-start line all read this one
    /// status. A child an earlier look reaped answers with what that look kept
    /// (`Session::child_exit`); otherwise this reaps without blocking — on
    /// unix the reader posted the exit only once the shell was collectable
    /// (or `spawn::SHELL_RETIRE_WAIT` passed), so the UI thread never waits
    /// here; elsewhere a [`ExitLook::First`] look at a shell that may have
    /// failed at start ([`Self::unstarted_shell`]) tries again for at most
    /// `SHELL_START_STATUS_WAIT`. `None`: no status (an adopted session with no
    /// exit witness, a child not yet reapable, or one this process cannot wait for).
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

    /// The line a pane keeps when its interactive shell failed at start
    /// ([`Self::unstarted_shell`]) and ended non-zero or by a signal:
    /// `[zsh ended at start: exit 1]`, `[fish ended at start: signal 6]`.
    /// `None` for everything else, a clean exit and a status nobody could
    /// collect included. Reads the status the exit path's looks kept
    /// (`Session::child_exit`); it never reaps.
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

    /// Keep the pane of a shell that failed at start
    /// ([`Self::shell_failed_at_start`]) and write its line into it. Returns
    /// whether it kept it. A kept pane then closes the way a `--hold` pane does:
    /// by Cmd-W, the tab's close button, or a `close` verb.
    fn keep_shell_failed_at_start(&mut self, session: u64) -> bool {
        let Some(line) = self.shell_failed_at_start(session) else {
            return false;
        };
        if let Some(pooled) = self.pool.get(session) {
            let mut term = term_lock(&pooled.term);
            // On a line of its own: after whatever the shell printed last.
            let lead = if term.cursor().col == 0 { "" } else { "\r\n" };
            term.process(format!("{lead}{line}\r\n").as_bytes());
        }
        let _ = self.admit_output_redraws(session, std::time::Instant::now());
        true
    }

    /// A click in window `wid`'s tab strip at column `col`: resolve it against that
    /// window's cached segments ([`WindowState::tab_segments`]) and SWITCH / CLOSE /
    /// open a tab. A click on bare strip background is ignored. The CLOSE of the last
    /// tab signals the window to close via `ws.pending_close` (the mouse handler has
    /// no `ActiveEventLoop`), mirroring Cmd-W. Repaints after any state change.
    ///
    /// A SECOND press on the same chip within [`crate::multi_click_ms`] opens the
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
                    .is_some_and(|at| now.duration_since(at).as_millis() <= crate::multi_click_ms())
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
            // drags/zooms natively), and on Linux the window system's own frame
            // has them, except where the band IS the frame (the Linux arm
            // below). Ordering trap the FSM avoids: the FIRST
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
            // LW.5: on Linux the band is caption where it is the window's own header
            // (`crate::client_frame`, a Wayland window the compositor leaves
            // undecorated) — the same two reflexes, with the move started by the
            // press's first real motion (`App::route_frame_motion`) rather than on
            // the press, so a click or a double-click starts no compositor grab and
            // the second press of a double-click reaches the band. The window
            // system's own frame keeps them everywhere else, and a fullscreen
            // window has no header to be caption.
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
                                && now.duration_since(at).as_millis() <= crate::multi_click_ms()
                        })
                    })
                });
                // A click on the chip already being edited is not leaving the
                // field (read BEFORE the arm below, which needs it too).
                let editing_this = self
                    .inline_rename_edit(wid)
                    .map(|edit| edit.tab)
                    .is_some_and(|edited| Some(edited) == tab);
                if let Some(ws) = self.windows.get_mut(&wid) {
                    ws.strip_press = tab.map(|tab| (now, tab));
                    // A chip press is not part of a BAND double-click either.
                    ws.strip_band_press = None;
                    // DRAG-TO-REORDER arms HERE and travels with the pointer from
                    // here ([`App::advance_strip_drag`]): the press still selects,
                    // exactly as Explorer / Edge / Windows Terminal do, and only
                    // motion reorders. NOT on the repeat press — that one opens
                    // the inline rename editor, and a rename gesture must not also
                    // be able to reshuffle the strip out from under the field —
                    // and NOT while that editor is already OPEN on this chip: a
                    // stale (past the double-click window) press there is "keep
                    // editing", and the same reshuffle-under-the-field hazard.
                    ws.strip_drag = match (repeat || editing_this, tab) {
                        (false, Some(tab)) => Some(crate::app_mouse::StripDrag {
                            tab,
                            origin_col: col,
                        }),
                        _ => None,
                    };
                }
                // A click on a DIFFERENT chip is a click away and COMMITS, matching
                // the macOS `Wake::SelectTab` arm and every other exit.
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
            // old click-to-details flow was the "too many clicking" complaint), unless
            // it is on hold (`latched_activation_trial_spent`), which opens the details
            // like an empty stage. With nothing actually staged it falls back to the
            // details overlay — never a blind restart, never a dead click. Details stay
            // reachable via the App menu's "Check for Updates…".
            tab_bar::TabHit::Update => {
                self.clear_strip_press(wid);
                self.settle_rename_edit(wid);
                self.apply_update_or_details();
            }
            // The trailing VERSION LABEL (Windows): a button, committed on the
            // press like the `+` and the `↻`, and never caption — no window drag,
            // no maximize. It posts the menu action the macOS Version menu's
            // About row fires (`menu::version_slot_click`: Settings ▸ About, or
            // the update details where an updater has a staged build to
            // describe), through the palette's own path — `Wake::MenuAction` →
            // `dispatch_menu_action` → `fence_menu_action` — so it is fenced and
            // rename-diverted exactly as a menu choice is. A second press posts
            // it again, which focuses the same Settings tab.
            tab_bar::TabHit::Version { attention, .. } => {
                self.clear_strip_press(wid);
                self.settle_rename_edit(wid);
                self.post_menu_action(crate::menu::version_slot_click(attention));
            }
            // The window's NEEDS PILL (owner surfaces §5), where the in-grid
            // strip carries it: a button committed on the press like the `+`,
            // never caption. The click carries the session the drawn pill
            // named, so one on a pill that has moved since acts on nothing
            // (`App::on_pill_act`, the macOS `NeedsPill` view's own path).
            tab_bar::TabHit::Pill(hit) => {
                self.clear_strip_press(wid);
                self.settle_rename_edit(wid);
                if let Some(session) = self.windows.get(&wid).and_then(|ws| ws.strip_pill_session) {
                    self.on_pill_act(wid, session, hit);
                }
            }
            tab_bar::TabHit::Close(i) => self.close_tab_from_strip(wid, i),
            // A WINDOW BUTTON of the band as the window's own header
            // (`crate::client_frame`): a button, committed on the press like the
            // `+`, and never caption (no move, no maximize by double-click).
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

    /// A press on one of the window's own buttons ([`tab_bar::TabHit::Caption`]):
    /// minimize, maximize or restore through the window, or close — which the
    /// event-loop arm runs as the window system's own close (`CloseRequested`, its
    /// confirmation included) once it has an `ActiveEventLoop`
    /// ([`App::escalate_caption_close`]).
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

    /// A MIDDLE press on the in-grid tab strip at `col`: on a chip (its label
    /// or ✕) it closes that tab, the GTK/browser/Windows Terminal
    /// convention, through the chip ✕'s own path (`close_tab_at`, so a busy tab
    /// asks first exactly as its ✕ does). Anywhere else on the strip it does
    /// nothing.
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

    /// Close tab `i` from a strip gesture (its ✕, or a middle press on it).
    fn close_tab_from_strip(&mut self, wid: WindowId, i: usize) {
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

    /// Break the strip's double-click streaks — the chip one AND the bare-band
    /// one. Any hit that is not "the same chip again" ends the chip streak, so a
    /// Close/`+`/`↻`/background click between two chip presses cannot be counted
    /// as a double-click on the chip; symmetrically, any press that is not bare
    /// band ends the band streak (W2's double-click-maximize), so
    /// chip-then-band cannot read as a band double-click. The band arm in
    /// `handle_tab_strip_click` samples its streak BEFORE calling this and
    /// re-arms after.
    ///
    /// It also drops any in-flight DRAG-TO-REORDER, for the same reason: every
    /// caller is a press that is not "grab this chip" — bare band, `✕`, `+`, `↻`,
    /// a click away, or the right-press that pops the context menu — and a chip
    /// left held across one of those must not keep stepping. (The ordinary end of
    /// a drag is its RELEASE, in `on_mouse_input`.)
    pub(crate) fn clear_strip_press(&mut self, wid: WindowId) {
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.strip_press = None;
            ws.strip_band_press = None;
            ws.strip_drag = None;
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
        // The menu header paints the label WHOLE — the same projection
        // `refresh_window_tabs` hands the tooltip composer, so the two callers
        // of one chrome cache agree on the label they key it with.
        let label = crate::chrome_label::capped_label(titles.get(index).map_or("", String::as_str));
        // Its timeline rows aged as the card opens (`TabMenuEntry::shown_at`).
        crate::session_chrome::menu_shown_at(
            &self
                .composed_session_chrome(wid, session, &label, &mut None)
                .menu,
            crate::turn_ledger::now_ms(),
        )
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
    /// The RIGHT-PRESS and keyboard callers (the strip's right press,
    /// Shift+F10 / the Menu key) are gated to the in-grid-strip platforms,
    /// Windows and Linux — macOS pops a real `NSMenu` off the native strip
    /// instead (see the `Chrome` arm in `app_mouse`). A left press anywhere
    /// on a chip, its status marks included, selects the tab and opens
    /// nothing (`docs/DESIGN-owner-surfaces-2026-10-16.md` §5).
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
        if self
            .windows
            .get(&wid)
            .and_then(|ws| ws.tab_set.active_index())
            != Some(index)
        {
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
            // The modal owns the wheel. Neither a fraction banked on the
            // previous surface nor one swallowed under this card may pay for
            // a notch on the next surface.
            ws.scroll_residual = 0.0;
            ws.scroll_residual_x = 0.0;
            // Stale from a previous pop; the next splice records the real one.
            ws.tab_menu_rect = None;
            if let Some(w) = &ws.os_window {
                w.request_redraw();
            }
        }
        true
    }

    /// C5 — the KEYBOARD entry point (the Menu key by default; Shift+F10 too
    /// under `tab_menu_chord = "on"`): pop the context menu for the window's
    /// ACTIVE tab, anchored at that chip's own left edge rather than at a
    /// pointer that was never involved.
    ///
    /// This is what makes the popup usable without a mouse, and it is not a
    /// nicety: a surface that has a right-click menu but no keyboard route to
    /// it is unreachable for anyone driving by keyboard. The Menu key is the
    /// default spelling because no legacy keyboard mode delivers it to an
    /// application; Shift+F10 (the OS-wide Windows chord) is terminfo `kf22`,
    /// so it is claimed only when a config asks. Returns whether a menu opened,
    /// so the caller can let the chord fall through when it did not (a native
    /// app tab, no strip, no tabs).
    ///
    /// Its real callers are the two in-grid-strip chord arms (Windows and
    /// Linux) — `on_key`'s and the convergence seam's `tab_menu_input_event` —
    /// which is what makes `aterm ctl key menu` exercise the same path the
    /// physical key does. ⇧F10 is not a menu chord on macOS, whose chips pop a
    /// native `NSMenu` instead.
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

    /// C5 — the chip's own start column, read off the SAME laid-out segments
    /// the strip painted, so a card opened without a pointer still lines up
    /// with the tab it belongs to. A window whose segments have not been laid
    /// out yet (no frame drawn) falls back to column 0 — flush left, still
    /// under the band.
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

    /// C5 — open the context menu for chip `index` with no pointer column of
    /// its own (the tests' right press): anchored at the chip, not
    /// keyboard-seeded (the gesture it stands for is a press), selecting the
    /// tab first as the right press does.
    #[cfg(test)]
    pub(crate) fn open_tab_context_menu_at_chip(&mut self, wid: WindowId, index: usize) -> bool {
        let anchor = self.tab_chip_anchor_col(wid, index);
        self.open_tab_context_menu(wid, index, anchor, false)
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
        ws.scroll_residual = 0.0;
        ws.scroll_residual_x = 0.0;
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
    ///
    /// An agent-upgrade row (gap #21) needs no event loop: it is dispatched
    /// here, through the one dispatcher the macOS relay's
    /// `Wake::TabMenuUpgrade` reaches too ([`App::dispatch_tab_menu_upgrade`]),
    /// with the pop-time tab and the session and build the card showed.
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
        // A session-argument row (the AGENT section's Go to <manager>, a
        // worker row, Stop Worker…) needs no event loop either: the one
        // dispatcher the macOS relay's `Wake::TabMenuConnection` reaches.
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
        // Take Over and Hand Back run NOW, in the gesture's turn, where a
        // control client's key is still marked and refused
        // (`App::person_gesture_refused`); a posted action would run a turn
        // later, unmarked.
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

    /// Stash the CURRENT close gesture's attribution on window `wid`, at the
    /// moment a last-tab close is DEFERRED (the window's teardown, and the
    /// exit-journalling deregistration, run a later turn with no
    /// [`session_store::CloseAttribution`] scope open). Read back and re-entered
    /// by [`crate::App::close_window_logical`]. A no-op if the window is already
    /// gone. See [`crate::WindowState::pending_close_attribution`].
    pub(crate) fn stash_deferred_close_attribution(&mut self, wid: WindowId) {
        let attribution = session_store::current_close_attribution();
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.pending_close_attribution = Some(attribution);
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
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "tab_strip",
            action = "Close",
            project = "aterm_gui::tab_strip_conformance::project"
        )
    )]
    pub(crate) fn close_tab_at(&mut self, wid: WindowId, i: usize) -> bool {
        // Exit-ledger attribution for the gestures that reach here directly (tab
        // ✕, native ✕, the tab menu); an outer scope (a control `close`) keeps its own.
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
            // DEFERRED, not done: the mutation is queued onto the pending
            // handoff's teardown and applied once that cancellation lands. The
            // tab count a caller reads right now is the UNCHANGED one, so
            // answering `OK <active> <count>` would read as "closed, and here is
            // the state". Say what actually happened instead.
            self.pending_action_refusal = Some(
                "tab close deferred: a pending update handoff is being cancelled first".to_string(),
            );
            return false;
        }
        if is_native {
            // The tab in front, where it is another: it stays in front once the
            // close succeeds ([`Self::close_active_native_tab_keeping`]).
            let mut front = None;
            if let Some(ws) = self.windows.get_mut(&wid) {
                front = ws.tab_set.active_id().filter(|front| *front != tab_id);
                ws.tab_set.switch_to(tab_id);
            }
            return match self.close_active_native_tab_keeping(wid, front) {
                Ok(()) => canonical_count == 1,
                Err(why) => {
                    // Keep the refused tab selected: its app-owned banner and
                    // the exact recovery palette explain why topology stayed
                    // intact. Restoring the prior tab made close look inert.
                    self.resync_active_or_window(wid);
                    // AND SAY WHY, for the seams that mint a reply. `bool` here
                    // means "was the last canonical tab", never "it closed" (see
                    // this function's own doc), so every caller that treated
                    // `false` as a quiet no-op threw this sentence away — and
                    // `tab close` answered `OK <active> <count>` over it while
                    // the GUI user got the banner. The two refusals this arm
                    // collapses ("waiting for a durable checkpoint" and the
                    // update-child rollback deferral) are distinct and both
                    // actionable, so the reason travels rather than the bool.
                    self.pending_action_refusal = Some(why);
                    false
                }
            };
        }
        let Some(projection_index) = terminal_index else {
            return false;
        };
        // M2 quit-safety: closing the window's LAST tab (whole tab, all its panes)
        // exits the window — ask before such a close while a job runs, mirroring
        // the red ✕ / Cmd-Q. `false` here means "did not close" (the question is
        // parked, or was answered no); the caller leaves the window open. Closing one tab
        // among several asks only while that tab holds a job (`view_close_allowed`).
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
        // Preserve the historical last-tab handoff: the caller closes the whole
        // window, whose teardown drains this terminal and its views exactly once.
        if exits_window {
            if let Some(tab) = recovery {
                self.retain_closed_tab(wid, i, tab);
            }
            // The window's deregistration runs a LATER turn (the caller sets
            // `pending_close`; `escalate_pending_close`/`close_window` tears it
            // down), after this gesture's `CloseAttribution` scope has dropped.
            // Carry the reason/actor on the window so `close_window_logical` can
            // re-enter it around the deregister — otherwise the exit journals
            // `reason=unknown by=-`.
            self.stash_deferred_close_attribution(wid);
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
            if self.detach_session_view(sid) {
                self.retire_session_registration(sid);
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
    // E0599 — and those same call sites keep it live for the dead-code lint.
    pub(crate) fn structural_invariants_ok(&self) -> bool {
        self.structural_invariant_violation().is_none()
    }

    /// The same oracle as [`Self::structural_invariants_ok`], but NAMING the first
    /// clause that is false — and the values that make it false — instead of only
    /// reporting that one was. Every `debug_assert!` on the bool passes this as its
    /// panic message, so a trip arrives as a diagnosis rather than as a fact still
    /// to be diagnosed.
    ///
    /// An oracle over a model this wide is only worth the sentence it can produce
    /// when it fires: it guards eleven distinct relationships across `tab_set`,
    /// `layouts`, `tabs`, `front_content`, `active_terminal`, `window_focus`, the
    /// view store, the session pool and the native runtime, and it fires at seams
    /// (`sync_active_session`, `close_window_logical`, session restore) reached
    /// from every tab add/switch/cycle/close — so a bare bool names neither the
    /// broken relationship nor the window it broke in. The message costs a passing
    /// frame nothing: `assert!` evaluates its format arguments ONLY on the failing
    /// branch.
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
            // An all-terminal active tab has a matching compatibility entry
            // and the optional front capability names its focused session.
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
                // OWNERSHIP ORDER — topology first, identity second. A `ViewId` may
                // only be retired once the canonical tree has stopped naming it: a
                // leaf that outlives its `ViewStore` entry is UNRESOLVABLE, and
                // `active_visible_content_route` fails closed on exactly that, which
                // returns from `redraw_window` before any present and freezes the
                // window for the rest of its life (`pane::PaneTree::close_pane`
                // documents the release-only elision that used to reach this state).
                // So a re-projection that still names the leaf means the topology
                // change did NOT happen: refuse the whole retirement — the session
                // keeps its view and its PTY — rather than trade a live pane for a
                // dead window. `close_focused_mixed_leaf` proves the same order on
                // the heterogeneous side: it refuses to detach ownership unless the
                // tree removal returned `Removed`.
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
                // The tab survives (a sibling remained). Detach just the closed
                // pane's view; the sibling's reader thread stays alive.
                self.teardown_session(outcome.closed());
                // The active tab's geometry changed (a sibling grew); the closed
                // tab may not be the active one (background EOF), but re-laying the
                // active tab is cheap and correct. Resize panes to the new layout.
                // This pass is the COLLAPSE the split's take-back hold judges
                // (`crate::split_regrid_hold`): a sibling a split shrank inside
                // the quiet does not grow straight back to its pre-split grid.
                self.split_regrid_cause = crate::split_regrid_hold::RelayCause::Collapse;
                self.resize_panes(wid);
                self.split_regrid_cause = crate::split_regrid_hold::RelayCause::Other;
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
        self.split_regrid_holds.forget(id);
        if self.detach_session_view(id) {
            self.retire_session_registration(id);
        }
    }

    /// Retire everything keyed by a DROPPED session's stable sid — the shared tail
    /// of every mid-run close path, called only after `detach_session_view`
    /// returned `true` (the view count actually hit 0): deregister from the
    /// process-wide registry (P1.1), remove a worker's task file
    /// (`task_launch::retire_task_file`), retire the sibling-discovery graph
    /// entry, then run the close-time CONNECTION sweep (design §1.4#4) — drop the closed
    /// session's connection records and revoke every edge row it was the SOURCE
    /// of in the surviving sessions' tables. The sweep is a security OBLIGATION,
    /// not bookkeeping: a row's nonce is the DESTINATION's, so source death does
    /// not fail closed by nonce, and token hexes are non-enumerable — this sweep
    /// is those rows' only dissolution path. The deregister is a bound statement
    /// (not an `if let` scrutinee) so the store write guard is DROPPED before the
    /// sweep re-reads the registry for the survivors' tables.
    pub(crate) fn retire_session_registration(&mut self, id: u64) {
        self.retire_session_registration_keeping(
            id,
            Some(crate::session_log::ClosedKeep::Skeleton),
        );
    }

    /// [`Self::retire_session_registration`], its closed-session index entry
    /// as `keep` says (`None`: none at all, for a close that ends the app,
    /// whose index nobody would read): a batch close keeps the skeletons of
    /// its last closes only (`session_log::ClosedKeep::plan`, ranked by the
    /// closes the batch makes, not its views).
    pub(crate) fn retire_session_registration_keeping(
        &mut self,
        id: u64,
        keep: Option<crate::session_log::ClosedKeep>,
    ) {
        #[cfg(target_os = "macos")]
        self.withdraw_askpass_session(id);
        // The exit ledger's tale: the gesture that started this close said why.
        let (reason, by) = session_store::current_close_attribution();
        // THE CLOSED-SESSION INDEX's input (`session_log::ClosedLogs`): the
        // session's ctx, read under the store's read lock and released, with
        // the instance's watcher list.
        let (closing, instance) = {
            let store = self.store.read().unwrap_or_else(|p| p.into_inner());
            let ctx = store.by_local(id).map(|h| std::sync::Arc::clone(&h.ctx));
            (ctx, store.instance_watchers())
        };
        // A session with a task record may be a worker with a task file.
        let tasked = closing.as_ref().is_some_and(|ctx| {
            ctx.timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .task()
                .is_some()
        });
        // The deregister's `closing` and `state-change` records ring the
        // session's watchers, and its roster exit the instance's: both held
        // until the closed-session entry below is linked.
        let held = (
            closing.as_ref().map(|ctx| ctx.watchers.hold_rings()),
            instance.hold_rings(),
        );
        let stable = self
            .store
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .deregister_local_as(id, reason, by);
        // Keep the same authority table for already-resolved revokers and
        // closed-log readers. Prune write grants under its lock; retaining
        // reads in another table would let a pending disconnect revoke the
        // emptied live table while the closed log still admitted its token.
        let reads = closing.as_ref().filter(|_| keep.is_some()).map(|ctx| {
            let mut edges = ctx.edges.lock().unwrap_or_else(|p| p.into_inner());
            edges.keep_only(aterm_session::Op::ReadScreen);
            (std::sync::Arc::clone(&ctx.edges), edges.len())
        });
        // The index's entry, built from the log the deregister just closed
        // under that session's timeline lock alone, then linked under the
        // store's write lock (no other lock held); what the link frees is
        // dropped after.
        if let (Some(stable), Some(ctx), Some((reads, read_rows)), Some(keep)) =
            (stable.as_ref(), closing, reads, keep)
        {
            let entry = {
                let tl = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
                crate::session_log::ClosedLog::kept_from(
                    tl.slog().log(),
                    stable.clone(),
                    id,
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
                .link_closed_log(entry);
            drop(freed);
        }
        // THE WAKE: every verb parked on the session (`await`, `ready`, `wait`,
        // `subscribe`) and every `sessions` stream reads that it ended. After
        // the link, so a verb it wakes finds the session's log already
        // answering.
        drop(held);
        // The harness's glass and Autopilot's timer go with the session
        // (design §9.2.3, §9.3.6): no label outlives it, and no deadline of
        // its is folded again.
        self.forget_harness_glass(id);
        self.autopilot_timers.forget(id);
        // …and what its Autopilot records said (§11.7.3, item 3).
        self.autopilot_recorded.remove(&id);
        // The front pane may be another now: a warm spare follows it (`spare`).
        self.spare_poke();
        if let Some(stable) = stable {
            // A positively retired session no longer owns a process-local login
            // story. Its persisted notice is neither resolved nor rewritten.
            self.resume_stories.remove(stable.as_str());
            // A worker's task file goes with it: its agent took the task as
            // argv, and nothing reads the file after.
            if tasked {
                crate::task_launch::retire_task_file(&stable);
            }
            crate::proxy::unpublish_session(&stable);
            // Through the App's own store handle (the process singleton in a
            // real run) so the §2.4 freshness bump lands where the chrome
            // cache reads it — private-store tests included.
            crate::connections::sweep_session_closed_in(&self.connections, &stable, &self.store);
            // The sweep dissolves edges with NO wake funnel of its own: the
            // marks and menus recompose through the close path's later strip
            // refresh (revision-gated), but an OPEN §5 map re-snapshots only
            // when told — retarget it now or it serves the dead session's
            // chip and arrows until the next unrelated authority act.
            self.overview_refresh_all();
        }
    }
}

#[cfg(test)]
mod mixed_tab_tests {
    use super::*;

    /// A disabled agent's question belongs to the owner, while the other
    /// agent's question remains its live harness's (agy beside Claude Code: a
    /// vendor Codex is no harness's since the Codex screen lane's
    /// retirement, 2026-10-16). Use the real timeline →
    /// host projection → herald classifier, without a native window.
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
            // `adopt_harness_policy`'s republish, the host's config set here
            // by hand.
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
            // The wire's claim posts `Wake::MetaChanged`, which publishes.
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

    /// Address a real temp-dir file the way the SHIPPING code does.
    ///
    /// A hand-rolled `format!("file://{}", path.display())` is malformed on
    /// Windows: `C:\Users\...` lands straight after the authority slashes, so
    /// `C:` is parsed as the authority and the backslashes are never escaped.
    /// The document host rejects that with "malformed file URI" before a single
    /// byte is read, which is a fixture defect and not a product one — every
    /// product seam that turns a path into a URI (the file picker in
    /// `app_input::open_local_document_path`, the drag-and-drop seam in
    /// `lib::drop_file`, the config editor in `app_documents`) already funnels
    /// through this same encoder, which emits `file:///C:/Users/...` with
    /// percent-escapes. Tests must use it too, or they test a URI no user can
    /// ever produce.
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
        // The font row's text entry (what "Other…" opens), then a name.
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

    /// A Settings draft in a mixed [Settings | terminal] tab blocks the close
    /// of either leaf, through Cmd-W and through the replay of a close an update
    /// handoff's rollback deferred (after the user selected another tab): the
    /// tab keeps both leaves, the blocking view takes the front, and only the
    /// app-supplied recovery is surfaced. A refused replay must not restore the
    /// newer selection over the reducer-owned recovery surface.
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

    /// THE `tab` VERB SAYS WHEN IT DID NOTHING. Every `tab` action funnelled
    /// through one post-state read, so `cmd_tab` minted `OK <active> <count>`
    /// whatever happened. Two declines hid behind that: an out-of-range index
    /// (for `move`, the OK is BYTE-IDENTICAL to an honoured move), and a tab host
    /// that refused the close.
    ///
    /// Each refusal is paired with its POSITIVE control in the same test, because
    /// "always refuses" would pass a one-sided version of this and be a worse bug
    /// than the silence it replaces.
    #[test]
    fn an_out_of_range_tab_action_refuses_instead_of_reporting_ok() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        // `open_tab_in` cannot stage this: it needs a real `proxy` to spawn a
        // PTY and returns having done NOTHING under the headless harness, which
        // left the fixture with one tab and no destination for the move below.
        // `push_stub_tab` is the harness's own second-tab idiom.
        app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
        let count = app.windows[&wid].tab_set.len();
        assert!(count >= 2, "two tabs so a real move has somewhere to go");

        // SELECT past the end: a silent no-op that reported the unchanged state.
        app.apply_tab_cmd_in(wid, TabAction::Select(count))
            .expect_err("an index no tab holds must refuse");

        // CLOSE past the end.
        app.apply_tab_cmd_in(wid, TabAction::Close(Some(count + 3)))
            .expect_err("closing an index no tab holds must refuse");

        // MOVE past the end — the case whose OK was indistinguishable from a
        // move that really happened.
        app.apply_tab_cmd_in(
            wid,
            TabAction::Move {
                from: 0,
                to: count + 1,
            },
        )
        .expect_err("moving to an index no tab holds must refuse");

        // POSITIVE CONTROLS: the same verbs, in range, must still act.
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
        // ...and a close in range really closes.
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

        // THE REFUSED CLOSE TRAVELS AS A REASON. This used to read `Ok((1, 2))`
        // — the verb's old shape, which minted an OK carrying the UNCHANGED
        // count over a close the host had declined. Since the `tab` verb learned
        // to refuse, the same decline is an `Err` naming the blocker, and every
        // consequence below (the recovery surface, the surviving pair, the
        // re-focused draft) is unchanged.
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

    /// The barrier answers the SAME question two ways: a person gets the recovery
    /// surface, background policy gets only the verdict. The window and quit
    /// barriers refuse on the same draft, and none of them lets go of any tab
    /// until the draft is explicitly discarded.
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
        // The reducer really ran and really still holds the draft — a barrier
        // that had been short-circuited would also be silent.
        assert!(
            !app.native_runtime
                .presentation(instance, settings)
                .unwrap()
                .closable,
            "the draft is still what refused"
        );

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

        // The window and quit barriers hold the same draft, and every barrier
        // retains all topology until the draft is explicitly discarded.
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

        // Which leaves retain rasters across zoom and tab switches is
        // `native_leaf_cache_lifetime_conforms_across_split_zoom_tab_and_capture`'s.
        assert!(app.prepare_heterogeneous_input_scratch(wid).is_some());
        let window = &app.windows[&wid];

        // View-local damage stays on its own leaf. (The regional patch itself,
        // bytes outside the tile and the paint-identity widening, is
        // `single_native_retains_and_patches_one_device_tile`'s.)
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

        // Canonical truth is [terminal, Settings], active Settings. The legacy
        // projection deliberately still contains only terminal index zero.
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

        // Closing canonical index zero removes the terminal but not its native
        // sibling. The compatibility projection becomes genuinely empty.
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
        // The source's terminal projection and both pool counts, unmoved.
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

        // Move Settings across both terminals in canonical chrome. Neither
        // terminal-only vector is permuted because terminal relative order did
        // not change.
        app.move_tab(source, 2, 0);
        assert_eq!(app.windows[&source].tab_set.tabs()[0].id, native_id);
        assert_eq!(
            app.windows[&source].tabs.active, 1,
            "parked terminal identity is unchanged"
        );
        unmoved(&app, "after the reorder");

        // Detach the canonical active native tab into a real native-only logical
        // window. Source terminal projections and pool counts stay byte-identical.
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

        // The same stable native tab can migrate into an existing terminal
        // window; closing its now-empty source still moves no terminal session.
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

        // A fresh TabId and the ledger's bookkeeping are the reopen ledger
        // conformance's (`native_reopen_ledger_tier1_conforms_and_negative_controls_are_rejected`).
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

        // The ledger at the model's `Cap`: Cap + 1 closes through the shipping
        // path saturate it, because a full push evicts the oldest descriptor.
        // A push that evicted one too few (`len > capacity`) would retain Cap + 1.
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

/// Boundary proofs for the `~` cwd abbreviation (the tab-label form of a
/// session cwd — see [`home_relative_suffix`] / [`App::tab_titles`]). Pure
/// (explicit `home`), so every component-boundary case is provable without
/// touching the process environment; the App-level fallback-order tests in
/// `app_restore.rs` cover the same helper wired through `tab_titles` against
/// the REAL `$HOME`.
#[cfg(test)]
mod home_abbreviation_tests {
    use super::home_relative_suffix;

    /// Home itself reads as the bare `~` (empty suffix) and a child keeps its
    /// `/`-prefixed remainder; a sibling sharing the prefix (`/Users//foobar`
    /// must never read as `~bar`, which is why the zsh integration matches
    /// `$HOME/`) and the degenerate homes (an empty or root `$HOME`) never
    /// match.
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

/// App-level proofs for the composed per-tab session chrome (tooltip +
/// context-menu model, session-metadata stage 2): the live wiring around the
/// pure `session_chrome` composer — label/meta flow into the composed output,
/// the tooltip feeds through `TabPresentation.tooltip` and recomposes on a
/// timeline advance, and the per-tab action payloads follow the clicked tab. All headless — no AppKit; the strip-side `setToolTip`
/// / `NSMenu` pop are the documented manual-verify steps.
#[cfg(test)]
mod session_chrome_app_tests {
    use std::time::Instant;

    use super::*;
    use crate::menu::MenuAction;
    use crate::session_chrome::TabMenuEntry;

    /// The composed chrome reflects live session facts, the menu carries the
    /// pinned actions with honest enabled bits (a stub has no cwd ⇒ `Copy CWD`
    /// greys), a bare live session composes no tooltip (its `alive` state and
    /// `spawned` event are no news), and one fact beyond the label composes a
    /// tooltip that is FED THROUGH the tab's presentation.
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
        // A description is a fact the chip does not carry: it composes, and
        // the tooltip reaches the presentation opening with the chip label.
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

    /// The gather cuts the rows the chrome leaves out BEFORE its cap: a burst
    /// of agent verdict moves and typing after a directory change still shows
    /// the change and the start in the tab menu.
    #[test]
    fn session_chrome_gather_cuts_agent_moves_before_the_tail_cap() {
        let mut app = App::headless_for_test();
        {
            let ctx = &app.pool.get(0).expect("session 0").ctx;
            let mut tl = ctx.timeline.lock().unwrap();
            tl.record("cwd-change", "cwd=/tmp".into());
            for _ in 0..crate::session_chrome::TIMELINE_TAIL + 2 {
                tl.record("agent-change", "agent=busy".into());
                tl.record("human", "bytes=1".into());
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
        // A fact the chrome carries moves (the real `meta set` records it), so
        // the next refresh misses the cache and must read the terminal leaf.
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

        // A refresh that took the parser mutex would deadlock here behind the
        // guard this thread holds; returning at all is the property.
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

    /// One session's registry endpoint identity, as the fixture returns it.
    type Endpoint = (aterm_session::SessionId, std::sync::Arc<crate::SessionCtx>);

    /// Two registered stub sessions in one window plus their `(sid, ctx)`
    /// pairs — the session-connections fixture.
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

    /// §2.4 [v5] + the §9 obligation ("the menu model updates without cache
    /// expiry after a wire grant"): a connection mint/dissolve moves NO other
    /// cache epoch — no timeline record, no label drift, no activity — so the
    /// connections revision must invalidate the cached ext by itself, on BOTH
    /// endpoints' chrome, immediately rather than at the 30 s backstop.
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
        // Mint A → B through the App's OWN store (what the menu paths and —
        // via the shared singleton — the wire `connect` verb do).
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
        // Dissolve — again with no other epoch movement — and both clear.
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

    /// One chrome pass gathers every recomposed tab's connection facts from
    /// ONE snapshot of the edge tables (a revision bump misses every tab, and
    /// each walking every table made a pass of N tabs cost N^2). A snapshot
    /// taken before a mint that landed mid-pass leaves the tab's entry at the
    /// snapshot's revision, so the next pass recomposes it with the mint (the
    /// tooltip's connection line; the menu carries no connection row since
    /// the AGENT section replaced it). NEGATIVE CONTROL: an entry stamped with
    /// the revision its own compose read would keep serving the snapshot's
    /// tooltip, the mint missing.
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

    /// The AGENT section's session rows (`Wake::TabMenuConnection`): Show
    /// raises the named session's tab, resolving the clicked tab by its
    /// pop-time STABLE id; a stale (closed-tab) id drops the act instead of
    /// guessing. Stop Worker on a live worker asks first and stops only on a
    /// yes: Cancel sends no signal, Stop sends one, and with no sheet to ask
    /// on (headless) the Overview opens with that worker's inline confirm
    /// armed and nothing is signalled.
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
        // NEGATIVE CONTROL: from tab 0 again, a stale id raises nothing.
        app.switch_tab_in(wid, 0);
        app.dispatch_tab_menu_connection(
            wid,
            crate::tab_model::TabId::from_stored(u64::MAX),
            &b_sid,
            ConnVerb::Show,
        );
        assert_eq!(app.windows[&wid].tab_set.active_index(), Some(0));
        // B is a live worker, so only the answer decides the stop.
        let b_local = app.store.read().unwrap().by_sid(&b_sid).unwrap().local_id;
        mark_session_task(&b_ctx, b_local, &TaskMark::new(TaskState::Working)).expect("unfenced");
        // No sheet: the Overview's confirm, armed on B, and no signal.
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
        // Cancel: no signal.
        app.agents.owner_sheet_for_test = Some(Some(false));
        app.dispatch_tab_menu_connection(wid, tab_a, &b_sid, ConnVerb::StopWorker);
        assert_eq!(app.agents.stop_signals, 0, "Cancel stops nothing");
        assert!(app.windows[&wid].overview().is_none());
        // Stop: one signal.
        app.agents.owner_sheet_for_test = Some(Some(true));
        app.dispatch_tab_menu_connection(wid, tab_a, &b_sid, ConnVerb::StopWorker);
        assert_eq!(app.agents.stop_signals, 1, "Stop signals once");
    }

    /// REGRESSION (stale-index context menu, close half): when the CLICKED tab
    /// closes while its menu is open, the pop-time id resolves to `None` and
    /// the dispatcher drops the action — it must never fall back to the frozen
    /// index, which is still in range and now names the innocent right
    /// neighbor. (The id following its tab through a shift is
    /// `tab_menu_target_follows_its_stable_id_across_a_mid_menu_reorder`.)
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
    }

    /// REGRESSION (orphan controller release): a `mouse press` arriving off the
    /// card DISMISSES it and is consumed — and because the dismiss CLOSES the
    /// card, the matching RELEASE finds no menu open and would fall through to
    /// a tracking TUI as half a press pair, the same defect the consumed-press
    /// key sets exist to prevent. The dismissing button is latched so its
    /// release is swallowed with it; a button that was never eaten is not.
    /// Runs on both in-grid-strip platforms: the dismiss rule is the same
    /// state machine on each.
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

    use crate::session_pin_for_test as pin;

    /// A press on a background tab's MARKS (the status cell its `◆`, its
    /// spinner or its dot is painted on) takes you there: it selects that tab
    /// and opens no menu, as a press anywhere else on the chip does (§5,
    /// "Selects the tab"). The tab's menu stays the right press's, which
    /// selects it first.
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
        // The marks' cell: one inside the chip's ✕, as the painter puts it.
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

    /// DOUBLE-CLICK OPENS THE EDITOR. One click only switches; the second within
    /// the multi-click interval opens the inline rename over that chip. The
    /// streak is keyed on the STABLE tab, not the column, so two different chips
    /// are two single clicks; and a slow second click is a new gesture.
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

    /// THE VERSION LABEL IS A BUTTON, NOT CAPTION (Windows). A press on it arms
    /// neither double-click streak — not the chip's (rename) and not the bare
    /// band's (maximize) — so it can never drag or maximize the window; it
    /// switches, opens and closes no tab; and like every press away from an
    /// open rename it settles the edit. The `Wake::MenuAction` it posts cannot
    /// be built in libtest (a headless App has no event-loop proxy): the drive
    /// against a real window proves Settings ▸ About opens.
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
            Some(Instant::now() - Duration::from_millis(crate::multi_click_ms() as u64 + 50));
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

/// The status glance end-to-end over the REAL registry — headless, no
/// AppKit: typed meta and task records drive it, and stale clicks resolve to
/// nothing.
#[cfg(test)]
mod status_glance_tests {
    use super::*;

    /// Set a meta field as the wire's `meta set` does, then run the
    /// `Wake::MetaChanged` arm it posts: the need is published there.
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

    /// Set a meta field and publish the session's need, heralding nothing:
    /// what a test of the herald's own step starts from.
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

    /// TYPED ATTENTION DRIVES THE HEADLESS GLANCE: a `meta set attention` on a
    /// registered session is a need row going to that tab, counted on the
    /// badge, and `chrome`'s line says so; a role names nothing. NEGATIVE
    /// CONTROL: before it, nothing needs the owner.
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

    /// `chrome` READS THE ❯ MENU WITHOUT A CLICK: after the `statusitem` line,
    /// one `statusmenu` line per row the menu's own builder composes from
    /// the same glance — the need going to its tab, and Notify Me off, as
    /// `desktop_alerts` is by default — and reading it hands the item no new
    /// glance and repaints nothing. NEGATIVE CONTROL: before the need, no row
    /// names it.
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

    /// HAND BACK ALL IS TAKE OVER'S OWN HAND BACK, ONCE PER PERSON HOLD (§3):
    /// every tab the person holds is handed back through S5's act and leaves
    /// the You hold section. NEGATIVE CONTROL: a client's hold (`aterm ctl
    /// hold`) is not the person's to hand back, and stands, still listed.
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

    /// A WRITE THE PERSON'S HOLD REFUSED REACHES ❯ (§8 scenario 8): the
    /// refusal is counted on the control thread, which wakes nothing; the
    /// way to the next wait sees the count move and dirties the glance once,
    /// and the You hold row says it. NEGATIVE CONTROL: a read verb under the
    /// same hold is answered, counted nowhere, and dirties nothing.
    #[test]
    fn a_write_the_persons_hold_refused_reaches_the_you_hold_row() {
        let mut app = App::headless_for_test();
        app.take_over_session(0).expect("the person takes tab 1");
        let ctx = app.pool.get(0).expect("pooled").ctx.clone();
        let _ = app.take_status_rebuild();
        app.note_person_refusals();
        assert!(!app.take_status_rebuild(), "nothing refused yet");
        assert_eq!(app.status_glance().held[0].refused, 0);
        // NEGATIVE CONTROL: a read is answered under the hold.
        assert_eq!(crate::fabric::halt_refusal(&ctx, "status"), None);
        app.note_person_refusals();
        assert!(!app.take_status_rebuild(), "a read moves nothing");
        assert_eq!(app.status_glance().held[0].refused, 0);
        // A write is refused, and counted.
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

    /// THE STATUS ITEM'S OVERVIEW… OPENS THE CARD RAISED (§3): on the
    /// frontmost window, which comes forward de-miniaturized — a status item
    /// click never makes aterm the active app. NEGATIVE CONTROL: the Agents
    /// menu's row (aterm is already active) opens the same card and raises
    /// nothing.
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

    /// A STALE CLICK FAILS CLOSED (§3): a pick resolves against the glance
    /// its menu was painted from — the current generation or the one before
    /// it — and anything else (an older generation, an index past the table)
    /// resolves to nothing.
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

    /// AN OPEN MENU'S CLICKS RESOLVE AGAINST THE GLANCE IT WAS PAINTED FROM,
    /// however many are handed over while it is open (§3: opening it starts a
    /// fleet read, whose start and end are two hand-overs): the open's paint
    /// is kept until the next open, and a glance drawn before that open is
    /// not. An open with no paint keeps the last one; the next paint
    /// replaces it. NEGATIVE CONTROL: with no paint kept, the same two
    /// hand-overs strand the painted glance's clicks.
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
            // The open's fleet read: it began, it ended.
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

    /// DONE FOR YOU AND THE FOCUS PATH (§3): a finished `ctl` worker stands
    /// under Done for you (the dot) until its tab is looked at — by the one
    /// rule the tab's `•` reads (`App::note_done_seen`: the front tab of the
    /// frontmost window, on screen), on the way to a rebuild — or its result
    /// read; a later done is a new one. NEGATIVE CONTROLS: while nobody can
    /// look at it (a window on no screen, then an occluded one), it stands.
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

    /// AN ADOPTED SESSION'S STANDING NEED IS TOLD ONCE (§3): what the
    /// outgoing status item knew of a session — its row from
    /// `status_carry_rows`, read back — is taken at that session's first
    /// publication in the successor (`adopt_status_carry`): its ask, which
    /// the parent had told, posts nothing and keeps its age, and the done
    /// its owner had read stays seen; a new ask after it posts. NEGATIVE
    /// CONTROL: the same successor without the carry posts the ask's one
    /// notice and lists the done under Done for you. Unix only, as the park
    /// that stamps the rows is.
    #[cfg(unix)]
    #[test]
    fn an_adopted_standing_need_is_told_once() {
        use crate::task_record::TaskState;
        let take_posts = || HERALD_POSTS.with(|p| std::mem::take(&mut *p.borrow_mut()));
        // Session 1: a finished `ctl` worker, now asking; a windowed herald,
        // nobody looking.
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
        // The owner looks at its tab once (the window on screen), then away.
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
                // An age no clock reaches by itself.
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

    /// GO LANDS ON THE NEEDY PANE, not beside it: in a split tab whose
    /// keyboard is on the other pane, the shared focus body gives the named
    /// session's own pane the keyboard, and it joins the looked-at set
    /// (`notify_suppress`) — so a "yes" typed next reaches it, and its
    /// notices stop. NEGATIVE CONTROL: a single-pane tab's focus is the one
    /// pane already, and nothing moves.
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

    /// A CLICK ON aterm's OWN NOTICE GOES TO ITS TAB AND PANE: the delegate
    /// reads the response as the session the notice named
    /// (`notice_center::clicked_session`), and the `Wake::NoticeClicked` arm
    /// takes the ❯ menu's Go ([`App::go_to_session`]): its tab selected, its
    /// pane given the keyboard, its window raised. NEGATIVE CONTROLS: a
    /// dismissal and another process's notice name no session, and a session
    /// no window shows any more moves nothing and raises nothing.
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

    /// NOTIFY ME IS THE ONE GESTURE THAT ASKS macOS (`notice_center`; the
    /// first notice while it is on is the other asker, `notify::way_for`):
    /// turning it on asks to let aterm post as itself, and turning it off asks
    /// nothing. The switch still starts off.
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

    /// `herald_session` hands a refusal back to the herald (review,
    /// 2026-09-28): a notice the notifier's queue refused told nobody, so it
    /// stays owed — due again once [`crate::status_item::NOTIFY_RETRY`] has
    /// passed — where it used to be counted told and lost. NEGATIVE CONTROL:
    /// a notice the queue took is owed no more.
    #[test]
    fn a_refused_herald_notice_stays_owed() {
        let take_posts = || HERALD_POSTS.with(|p| std::mem::take(&mut *p.borrow_mut()));
        let herald = |refusal: Option<&'static str>| {
            let mut app = App::headless_for_test();
            // A windowed instance's herald, and nobody looking at the tab.
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

    /// Disabled transitions cannot spend the instance burst or leave a later
    /// session's notification owed. The previous producer-side check ran after
    /// `note`, so the fourth session silently armed the presence timer anyway.
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
            // A second transition also crosses the per-session floor in the
            // old implementation, even without filling the instance burst.
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

    /// Turning the setting off must cancel debt that predates the config
    /// commit, including a queue refusal. Re-enabling keeps the current row
    /// spent, while a new session's transition still reaches the desktop.
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

/// C5 — the in-grid tab CONTEXT MENU's App-level wiring: the popup carries the
/// SAME model `compose_tab_menu` composes (so the `chrome` verb's `tab-menu`
/// mirror and the glass can never disagree), and right-clicking a background
/// tab SELECTS it first.
///
/// Headless: no OS window, so `request_redraw` is a no-op and no `Wake` proxy
/// is installed. The model's entries and the dispatch itself are proved by
/// `session_chrome_app_tests`.
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
        // The popup IS the composed model — same items, same order, same
        // enabled bits as the `chrome` verb's mirror reads — with its timeline
        // rows aged as the card opened (`TabMenuEntry::shown_at`).
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

/// Now on the wall clock, unix seconds: what the status glance's ages count
/// from (a Mac's sleep does not advance `Instant`).
fn status_unix_now() -> i64 {
    aterm_time::unix_secs().map_or(0, |s| i64::try_from(s).unwrap_or(0))
}

#[cfg(test)]
mod connected_spawn_tests {
    use super::*;
    use crate::connections::{ConnectedSpawnKind, ConnectedSpawnPlace};
    use aterm_session::{Op, SessionId};

    /// Register a second stub session in `app`'s registry (the mint half of a
    /// connected spawn only needs the store) and park it in the pool so its
    /// ctx Arcs stay shared with the registered handle.
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

    /// The §6 precheck contract: `place=window` under headless is the NEW
    /// `ERR headless` refusal (checked BEFORE any create — the wire wrapper
    /// formats this exact `Err` body as `ERR headless: use place=tab`); `place=tab` stays
    /// headless-legal and resolves the window hosting the origin; an unknown
    /// or Exited `of=` fails closed.
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

    /// The mint half of a CONTROLLED spawn: the origin gains a `both`
    /// connection over the newborn — three rows in the NEWBORN's table, all
    /// `src=origin`, exactly the set the `edges` verb (and the §4 roles fold)
    /// reads. A CONTROLLER spawn inverts: the rows land in the ORIGIN's table
    /// with the newborn as src.
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
            // Clone-then-release (the registry discipline): no store lock held
            // across the table lock, even in a test.
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
        // The §4 roles fold sees it (the marks read exactly these tables).
        let roles = crate::connections::roles_by_local(&app.store);
        assert!(roles[&0].outbound && !roles[&0].inbound);
        assert!(roles[&41].inbound && !roles[&41].outbound);

        // CONTROLLER inverts src/dst: rows in the ORIGIN's table.
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

    /// The booked origin must still be live and in the booked host when the
    /// worker returns, before its newborn is installed or gains any authority.
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

    /// A connected WINDOW spawned off the loop is made, and connected, as its
    /// session lands, while its origin lives: a controlled window's first
    /// session is registered as the origin's child, and the caller hears its
    /// sid. The new window need not host the origin (a tab must), so the
    /// landing asks only that the origin still be the one booked and not
    /// exited; one that exited or went is refused before any window is made.
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

    /// A connected TAB spawned off the loop is connected as it lands, under the
    /// sid it was spawned with: the controller newborn drives its origin, and
    /// the caller hears that sid. A tab that never lands mints nothing and the
    /// caller hears why. Unix's, as the booking it drives
    /// (`tab_spawn::tests::book`) is.
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

    /// The reason + actor (`by=`) of the sole `Exited` journal row — a fresh
    /// headless instance hosts one session, so closing it writes exactly one.
    fn sole_exit(app: &App) -> (ExitReason, String) {
        let g = app.store.read().unwrap_or_else(|p| p.into_inner());
        let rec = g
            .roster_since(0)
            .rfind(|r| r.change == RosterChange::Exited)
            .expect("one exit record after the close");
        (rec.reason, rec.actor.as_wire().to_string())
    }

    /// How many sessions a close retired: the `Exited` journal rows.
    fn exit_rows(app: &App) -> usize {
        let g = app.store.read().unwrap_or_else(|p| p.into_inner());
        g.roster_since(0)
            .filter(|r| r.change == RosterChange::Exited)
            .count()
    }

    /// `close_session_by_id` the way the `Wake::CloseSession` arm runs it: under
    /// its attribution scope (dropped at the end of that turn) and its wire
    /// policy, which the arm restores to `Interactive` after the escalation.
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

    /// The `Start-Sleep 60` shape (`wire_tab_close_guard_tests` stages the same
    /// bytes): a command the shell reports running — OSC 133;C opened, no 133;D
    /// — with no child process, so on a stub session (`master`/`pid` = -1) only
    /// the block state can arm the guard.
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

    /// The engine grid `(rows, cols)` of `session`.
    fn grid(app: &App, session: u64) -> (u16, u16) {
        let term = app.pool.get(session).expect("live session").term.clone();
        let t = term.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        (t.rows(), t.cols())
    }

    /// Put `session` on the alternate screen with `ROW<r>` on every row — the
    /// frame a full-screen program (Claude Code) last drew.
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

    /// **SPLIT, THEN CLOSE AT ONCE, IS NO NET-ZERO FLAP.** Confirmed 2026-09-29
    /// by two skeptics: `@<claude> spawn split=h` answers after the Claude pane
    /// shrank N -> ~N/2, and an `@<new> close` straight after grew it back to N
    /// within ~8 ms. With one byte of the program's output between the halves
    /// the alternate screen kept ~N/2 demoted rows, and a `SIGWINCH` handler
    /// reading N (the size it last drew) never repainted. The shrink stays
    /// immediate; the take-back waits until the shrink has stood the quiet, so
    /// the program reads and draws the shrunk size first.
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
        // The program's output lands between the halves.
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
        // A redraw's pass (or any other re-lay to the same target) holds too.
        app.resize_panes(wid);
        assert_eq!(grid(&app, 0), shrunk, "a re-lay inside the quiet holds");
        let due = app
            .split_regrid_hold_deadline()
            .expect("a withheld take-back owes one wake");

        // Not before the quiet ends…
        app.service_split_regrid_holds(due - std::time::Duration::from_millis(1));
        assert_eq!(grid(&app, 0), shrunk);
        // …and at its end the sibling grows back and nothing stays armed.
        app.service_split_regrid_holds(due);
        assert_eq!(grid(&app, 0), full, "the take-back lands after the quiet");
        assert_eq!(app.split_regrid_hold_deadline(), None);
        assert!(due <= std::time::Instant::now() + SPLIT_REGRID_QUIET);
    }

    /// The hold never delays anything but the exact take-back: a window
    /// re-grid (a drag, a scale change, the control `resize` verb) inside the
    /// quiet applies at once and ends the hold, and a close after the quiet
    /// grows the sibling at once.
    #[test]
    fn the_split_hold_withholds_only_the_take_back_inside_the_quiet() {
        use crate::split_regrid_hold::SPLIT_REGRID_QUIET;
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let full = grid(&app, 0);

        // A window re-grid inside the quiet is not held.
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

        // A close after the quiet grows the sibling at once.
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

    /// The split's own NEW pane is never held: it was spawned at the window's
    /// full grid, a size its program never drew, so the resize away from it is
    /// no shrink. Closing the ORIGINAL pane at once grows the newborn to the
    /// whole rect straight away, and no wake is armed.
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

    /// A pane ZOOM inside the quiet is no take-back, even though the zoomed
    /// pane's target is exactly its pre-split grid: it applies at once — for
    /// the original pane and for the newborn — and ends the hold.
    #[test]
    fn a_zoom_inside_the_quiet_is_not_withheld() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let full = grid(&app, 0);

        // The newborn (focused after the split) zooms to the full grid.
        let new = app.split_active_stub_tab_dir(wid, pane::SplitDir::Horizontal);
        let shrunk = grid(&app, 0);
        app.toggle_pane_zoom();
        assert_eq!(grid(&app, new), full, "the newborn's zoom applies at once");
        app.toggle_pane_zoom();

        // The ORIGINAL pane, whose zoom target is its pre-split grid.
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

    /// **ONE PANE, NOT THE TAB.** Measured 2026-09-22 (audit): `@<pane> close` on
    /// one pane of a three-pane tab answered `OK closed s-…` and the whole tab
    /// went — `panes` lost the tab, both siblings answered `ERR no such session`,
    /// `exits` had three `ctl-close` rows within 4 ms. The verb now runs the Cmd-W
    /// collapse: the aimed pane retires, its siblings keep their sessions, their
    /// PTYs and their tab, and the ledger has exactly one row, the wire's.
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
        // A rejected close and a detach that owns no session prove no closure.
        // Only a windowed instance refuses a busy pane: a headless one never
        // confirms anything (`a_wire_close_never_reaches_the_dialog_and_refuses_only_a_busy_job`).
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

    /// The aim is the SESSION, not the focus: a pane that is neither focused nor
    /// in the active tab closes in place — its tab's projection is found by
    /// stable id, as the EOF path finds it — the keyboard stays on the pane it
    /// had, and the active tab does not move.
    #[test]
    fn a_ctl_close_aims_at_the_pane_not_at_the_focus_or_the_active_tab() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let a = app.split_active_stub_tab(wid); // tab 0: [0 | a], focus on a
        assert_eq!(app.windows[&wid].layouts[0].focus(), a);
        let c = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(c)); // tab 1: [c], now active
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

    /// THE GUARD IS THE PANE'S. A `close` on a split pane running a job is
    /// refused on the wire, in a sentence that names the pane and not a last-tab
    /// confirm the tab never armed; the pane, its session and its sibling stay.
    /// A SIBLING's job never refuses a close: the collapse leaves it running, so
    /// the idle pane beside it closes. Once that pane is alone in the window's
    /// last tab the same job is the last-tab confirm's — `close`'s other
    /// sentence — and the close proceeds (defers, being a last tab) the moment
    /// the job finishes. WINDOWED (`headless = false`): the headless
    /// short-circuit never confirms and would keep a dead guard green.
    #[test]
    fn a_ctl_close_of_a_busy_pane_is_refused_and_a_busy_sibling_never_refuses_one() {
        let mut app = App::headless_for_test();
        app.headless = false;
        let wid = WindowId(0);
        let a = app.split_active_stub_tab(wid); // [0 | a]
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

        // `a` is now the last tab's only pane: the same job, the last-tab confirm.
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

    /// The pane guard covers the one pane of a tab that is NOT the window's
    /// last: that close is the tab's, it never exits the window, so the
    /// window-exit confirm inside `close_tab_at` never reads it — and before the
    /// pane guard nothing did, so a busy background tab's job was hung up with
    /// `OK closed`. Refused in the pane's words, nothing moves; the same close
    /// retires the tab once the job is done.
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

    /// A terminal leaf beside a native view is a pane too: its close leaves the
    /// tab (and so the window) standing, and the pane's own job refuses it.
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

    /// Off macOS a person's close of a busy tab or split pane that is not the
    /// window's last asks first, in the window, as the last tab's close does:
    /// Ctrl+Shift+W (`close_active_tab`) and the strip ✕ (`close_tab_at`) park a
    /// question naming what closes (the tab, or the pane of a split); Enter
    /// replays exactly that close, Escape keeps it. An idle one closes at once.
    /// Before, it closed without a word and its job was hung up.
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

        // The strip ✕ on a busy background tab: its own question; Escape keeps it.
        start_in_shell_job(&app, first);
        assert!(!app.close_tab_at(wid, 0));
        let pending = app.close_banner.as_ref().expect("asked again");
        assert_eq!(kind(&pending.prompt), "close-tab");
        assert!(matches!(pending.replay, CloseReplay::Tab { wid: w, .. } if w == wid));
        assert_eq!(app.answer_close_banner(false), None);
        assert_eq!(tabs(&app), 2, "Escape kept it");

        // A busy pane of a split: the pane's question, and Enter closes only it.
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

        // Idle: instant, unasked.
        app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
        assert!(app.close_active_tab().is_none());
        assert!(app.close_banner.is_none());
        assert_eq!(tabs(&app), 2, "an idle tab closes at once");
    }

    /// The reason + actor (`by=`) of the `Exited` journal row for `session`.
    #[cfg(not(target_os = "macos"))]
    fn exit_of(app: &App, session: u64) -> (ExitReason, String) {
        let g = app.store.read().unwrap_or_else(|p| p.into_inner());
        let rec = g
            .roster_since(0)
            .rfind(|r| r.change == RosterChange::Exited && r.local_id == session)
            .expect("an exit record for the session");
        (rec.reason, rec.actor.as_wire().to_string())
    }

    /// A busy tab's question answered with Enter replays the close, and the
    /// window stays, so `close_window_logical` never runs to take the attribution
    /// `present_close_banner` stashed. The replay takes it itself: before, the
    /// stash outlived its close and the window's next scope-less close (its last
    /// shell exiting on its own) was journalled `ui-close by=human`.
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

        // Later the remaining shell ends on its own and the window goes with it.
        app.store
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .set_state(first, crate::session_store::SessionState::Exited);
        assert_eq!(app.close_window_logical(wid), CloseOutcome::Exit);
        assert_eq!(exit_of(&app, first), (ExitReason::ShellExit, "-".into()));
    }

    /// A busy tab's question raised over the wire (`invoke CloseTab`) and answered
    /// there (`confirm yes`) journals the closed session as the wire's close. The
    /// replay runs a later turn with no scope open, so before it re-entered the
    /// stashed attribution `close_tab_at`'s own `ui-close by=human` scope won.
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

    /// A Cmd-Shift-O co-viewed pane: its collapse detaches THIS window's view,
    /// the other window's keeps the session registered, and the reply names that
    /// viewer in the last-tab path's own words — one close per viewer. Such a
    /// pane is never busy either: the collapse hangs nothing up, its job runs on
    /// in the other viewer.
    #[test]
    fn a_ctl_close_of_a_co_viewed_pane_collapses_it_and_names_the_other_viewer() {
        let mut app = App::headless_for_test();
        app.headless = false;
        let wid = WindowId(0);
        let a = app.split_active_stub_tab(wid); // focus on a
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

    /// The same fact for a co-viewed tab that is not its window's last: the
    /// tab went, the other window's view keeps the session, and the reply says
    /// so. It used to blame a last-tab confirm that never fired — `close
    /// refused (a running job armed the last-tab confirm)` over an idle tab
    /// that had in fact closed.
    #[test]
    fn a_ctl_close_of_a_co_viewed_tab_names_the_other_viewer_not_a_refusal() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let c = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(c)); // tab 1: [c], active
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

    /// Two tabs of ONE window can show one session (a Cmd-Shift-O share moved
    /// back with Cmd-Shift-M). Closing the pane in one leaves the session in the
    /// other, and the reply names a tab, not "another window" that is not there.
    #[test]
    fn a_ctl_close_of_a_pane_twinned_in_the_same_window_names_the_other_tab() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let a = app.split_active_stub_tab(wid); // tab 0: [0 | a], focus on a
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

    /// A pane the wire closed is as reopenable as one Cmd-W closed: the collapse
    /// keeps the same closed-view record (`closed_view_record_for`, the FOCUSED
    /// leaf's record generalised to the aimed one) — for a pane that was not
    /// even focused.
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

    /// A ctl `tab close` of a SINGLE-tab window defers the window teardown
    /// (`close_tab_at`'s `exits_window` branch returns true without
    /// deregistering; the caller flags `pending_close`), so the deregistration
    /// runs a later turn — after the gesture's `CloseAttribution` scope has
    /// dropped. The attribution is stashed on the window and re-entered by
    /// `close_window_logical`, so the exit journals `reason=ctl-close`, not the
    /// `reason=unknown by=-` it silently lost before this fix.
    #[test]
    fn a_last_tab_ctl_close_journals_ctl_close() {
        let mut app = App::headless_for_test();
        app.close_tab_via_verb(WindowId(0), None);
        assert!(
            app.windows[&WindowId(0)].pending_close,
            "closing the only tab defers the window close"
        );
        // The deferred teardown (what `escalate_pending_close`/`close_window`
        // reach on a later turn), driven directly since a unit test has no
        // event loop.
        assert_eq!(app.close_window_logical(WindowId(0)), CloseOutcome::Exit);
        let (reason, _by) = sole_exit(&app);
        assert_eq!(reason, ExitReason::CtlClose);
    }

    /// `@<sid> close` of a window's LAST tab: the tab close is DEFERRED (the
    /// window teardown that retires the session needs an `ActiveEventLoop`), so
    /// the store-diff the verb answers from cannot be read until the escalation
    /// has run. It used to be read immediately, which made an idle last-tab
    /// close answer `ERR close refused (a running job armed the last-tab
    /// confirm)` — for a close that was not refused and had not yet happened —
    /// and, because the `Wake::CloseSession` arm never escalated, never happen
    /// at all: nothing was journalled and the tab stayed open. The progress is
    /// `Deferred`, and the verdict is `OK` only once the teardown really ran.
    #[test]
    fn a_last_tab_ctl_close_is_deferred_not_refused_and_answers_after_the_teardown() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let session = 0; // the harness hosts session 0 as window 0's only tab
        let progress = {
            // The `Wake::CloseSession` scope, dropped at the end of that turn.
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

        // The escalation (`escalate_pending_close` -> `close_window`), driven
        // directly since a unit test has no event loop.
        assert_eq!(app.close_window_logical(wid), CloseOutcome::Exit);
        assert_eq!(
            app.close_session_verdict(session, progress),
            Ok(()),
            "the session is gone: the verb answers OK closed"
        );
        let (reason, _by) = sole_exit(&app);
        assert_eq!(reason, ExitReason::CtlClose);
    }

    /// **THE WIRE `close` NEVER REACHES THE DIALOG.** Measured 2026-09-08 on a
    /// windowed instance opened in the background: `@<sid> close` of an idle
    /// last tab answered `ERR main-thread reply did not arrive within 30s`, and a
    /// sample of the main thread showed `close_session_by_id → close_tab_at →
    /// window_exit_close_allowed → confirm_destructive_close → NSAlert runModal`
    /// — the native "quit aterm?" dialog, waiting for a click nobody could give,
    /// with the shell's own `exit` queued behind it. `tab close` had carried a
    /// programmatic bracket since M2; `close` was the one wire path without
    /// one. The `Wake::CloseSession` arm now runs under
    /// [`CloseConfirm::WireRefuseBusy`]: the confirm answers from the policy
    /// before any dialog, an idle close proceeds, a busy one is refused — the
    /// catalog entry's `ERR close refused (a running job armed the last-tab
    /// confirm)`. Pinned WINDOWED (`headless = false`), because the headless
    /// short-circuit is what kept every earlier test green over this hang.
    ///
    /// The wire pane guard answers by the same matrix: only the wire policy
    /// refuses, and only a busy pane; the operator's typed Stop (`Programmatic`)
    /// is an instruction, a `--headless` instance never confirms anything, and
    /// `Interactive` passes it (a person's Cmd-W asks through
    /// `view_close_allowed` instead). (The whole verb, windowed, through this
    /// policy:
    /// `a_ctl_close_of_a_busy_pane_is_refused_and_a_busy_sibling_never_refuses_one`.)
    #[test]
    fn a_wire_close_never_reaches_the_dialog_and_refuses_only_a_busy_job() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        // None of these reaches a dialog: (policy, headless, busy, proceeds).
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

    /// The SAME deferred shape, on a session a second window also displays
    /// (Cmd-Shift-O): the two must not get the same answer. `close_session_by_id`
    /// aims at the LOWEST window id holding the session, that window's teardown
    /// runs in full, and the session is registered afterwards anyway — the other
    /// window's pool view is still attached, so `close_window_logical` never
    /// reached its deregister. Answering "the window teardown has not run" here
    /// states a falsehood about a teardown that ran, and names a window that no
    /// longer exists; the reply must name the co-viewer instead.
    #[test]
    fn a_deferred_close_of_a_co_viewed_session_names_the_other_window() {
        let mut app = App::headless_for_test();
        let session = 0; // the harness hosts session 0 as window 0's only tab
        let second = app
            .open_active_session_in_new_window_logical()
            .expect("Cmd-Shift-O opens a second viewer");
        assert!(app.window_contains_session(second, session));

        let progress = {
            // The `Wake::CloseSession` scope, dropped at the end of that turn.
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
        // Both facts hold before the escalation — the teardown is outstanding AND
        // a second window holds the session — and the co-viewer is the one that
        // survives the teardown, so it is the one named. (The wire never reads
        // here: `Wake::CloseSession` escalates before taking the verdict.)
        assert_eq!(
            app.close_session_verdict(session, progress),
            Err("close deferred (another window still displays this session)".to_string()),
            "the co-viewer is the reason the close cannot take, from the start"
        );

        // The escalation (`escalate_pending_close` -> `close_window`), driven
        // directly since a unit test has no event loop. The second window
        // survives it, so the app stays.
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

    /// A close the WIRE raised as a question keeps its name through a Settings
    /// open while the question stands. Measured 2026-09-27 on 0.95.0, 3 of 3:
    /// `invoke CloseTab` on a busy window, `settings`, `confirm yes` journalled
    /// `reason=unknown exit_code=- by=-`, because opening Settings cancelled a
    /// parked teardown that was not there and took the question's attribution
    /// with it.
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
        // `confirm yes` posts `Wake::ConfirmedClose`, whose replay runs
        // `close_window`, whose teardown is this one.
        assert_eq!(app.close_window_logical(wid), CloseOutcome::Exit);
        let (reason, by) = sole_exit(&app);
        assert_eq!(reason, ExitReason::CtlClose);
        assert_eq!(by, "ctl");
    }

    /// A last-tab UI close (Cmd-W / menu Close, `close_active_tab`) is deferred
    /// the same way (`apply_close_outcome`'s `LastPane` + `canonical_last`
    /// returns true without deregistering), and its `ui-close by=human`
    /// attribution likewise survives to the deferred deregistration.
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

/// THE DAILY GESTURE: closing a split pane must never stop the window painting.
///
/// A canonical leaf that outlives its `ViewStore` entry is unresolvable, so
/// [`App::active_visible_content_route`] fails closed — and `redraw_window`
/// returns on that `None` BEFORE it reaches the present, for the rest of the
/// window's life. Every test here therefore ends at the same seam the frame path
/// enters: resolve the route, then run the layout preparation `redraw_window`
/// performs with it.
///
/// The freeze itself was a release-only state (`PaneTree::close_pane` carried
/// its leaf removal inside a `debug_assert_eq!`, which a shipped build compiles
/// away); these hold the invariant that made the elision fatal: both trees
/// shrink, every leaf resolves, the route survives.
#[cfg(test)]
mod pane_close_render_seam_tests {
    use super::*;

    /// Assert the window can still start a frame: every canonical leaf resolves,
    /// the route the frame path gates on is `Some`, its layout preparation runs,
    /// and the structural invariants hold.
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

    /// THE REPORTED BUG. Split the window, close the focused pane: both trees
    /// must shrink by exactly the closed leaf, and the window must still render.
    /// Before the fix the shipped build left the closed leaf in BOTH trees while
    /// retiring its view, and the window never presented again. The same holds
    /// for the unfocused original pane, for a pane whose sibling is itself a
    /// split (the sub-tree is promoted whole), and for a tab's last pane beside
    /// another tab (the whole tab closes and its neighbour renders).
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

    /// What the `pane` verb is allowed to say, and it is only ever true: focus
    /// moved, or there was no pane that way.
    ///
    /// The bool comes from the traversal itself rather than from a second look at
    /// the layout afterwards, so `moved=1` and the focus actually changing are one
    /// fact. Both refusal shapes are covered — a single-pane tab, and an edge pane
    /// with no neighbour in the requested direction.
    #[test]
    fn pane_focus_reports_only_whether_focus_actually_moved() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);

        // A single-pane tab has nowhere to go, in any direction.
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

        // Split: focus lands on the new pane, so LEFT has a neighbour and moves…
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
        // …and from the leftmost pane, LEFT is an edge: no move, focus unchanged.
        assert!(!app.focus_pane_in(wid, pane::FocusDir::Left));
        assert_eq!(
            app.windows[&wid].layouts[0].focus(),
            0,
            "moved=0 means the focused pane really did not change"
        );
        // Non-vacuity for the edge case: the other direction still moves.
        assert!(app.focus_pane_in(wid, pane::FocusDir::Right));
        assert_eq!(app.windows[&wid].layouts[0].focus(), split);
    }

    /// The seam guard itself: if a re-projection ever leaves the closed leaf in
    /// the canonical tree again, `apply_close_outcome` must REFUSE to retire the
    /// view rather than mint the unresolvable leaf that stops the window
    /// presenting. Staged by desyncing the compatibility tree (which is exactly
    /// the shape the release-only elision produced) and driving the real seam.
    #[test]
    fn a_close_that_did_not_shrink_the_tree_refuses_to_retire_the_view() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let closed = app.split_active_stub_tab(wid);
        let before = canonical_leaves(&app, wid);
        assert_eq!(before.len(), 2);

        // The pane tree keeps BOTH leaves while the outcome claims a collapse —
        // the exact contradiction a shipped `close_pane` used to report.
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

/// A SHELL THAT FAILS AT START KEEPS ITS PANE (audit #7 finding 49). An rc file
/// that runs `exit 1`, or a shell whose dylib moved and aborts, used to take its
/// tab with it — the last tab its window, the last window aterm — with nothing
/// saying why. These drive the reader-EOF path (`Wake::Exit`'s el-free half:
/// classify, retire the input watch, `exit_session_logical`) over a REAL child
/// running a stub shell, so the status the path reaps is the kernel's.
#[cfg(all(test, unix))]
mod shell_start_exit_tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Run `script` under `/bin/sh -c` as this process's child and wait —
    /// WITHOUT reaping — until it has exited, so the exit path finds the zombie
    /// the way a real shell leaves one. `/bin/sh` and never a script written
    /// for the test: a freshly written executable's first exec waits on the
    /// system's code-policy check, which under load has outlasted the minute
    /// (2026-10-02). A minute is a hang detector, not a budget.
    fn run_to_zombie(script: &str) -> i32 {
        let child = aterm_uds::spawnfd::command("/bin/sh")
            .args(["-c", script])
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .stderr(aterm_uds::spawnfd::Stdio::null())
            .spawn()
            .expect("spawn the stand-in shell");
        let pid = i32::try_from(child.id()).expect("pid fits");
        // Forgotten, not dropped: dropping the handle would reap a child that
        // has already exited, and the zombie must stay ours.
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
            // SAFETY: `waitid` either filled a `CLD_*` siginfo or, under
            // `WNOHANG` with nothing waitable, left it zeroed; `si_pid` is
            // defined in both. A method on Linux, a field on macOS: the method
            // compiles on both.
            let exited = unsafe { info.si_pid() };
            if exited == pid {
                return pid;
            }
            assert!(Instant::now() < deadline, "the stand-in shell did not exit");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// What the spawn recorded: the shell's name and when it started. `None`:
    /// no start (a `-e` command or an adopted shell, `records_shell_start`).
    type Start = Option<(&'static str, Instant)>;

    fn started(name: &'static str, at: Instant) -> Start {
        Some((name, at))
    }

    /// The reader's EOF stamp of a shell that ended "at start": moments after
    /// the spawn recorded at `at`, well inside [`SHELL_START_WINDOW`]. A
    /// scenario, not a reading: stamped from the clock after the stub ran, it
    /// measured how long a loaded machine took to spawn and exit `/bin/sh`,
    /// and there (and at background QoS beside 72 spinners, 3 to 5 of 12 per
    /// run) that passed the then-2 s window, so the shells these tests say
    /// failed at start were judged late and their windows closed ("nothing
    /// closes: [WindowId(1)]"). The one case about a late exit stamps its own
    /// ([`an_exit_that_is_no_failure_at_start_closes_as_today`]).
    fn eof_at_start(at: Instant) -> Option<Instant> {
        Some(at + Duration::from_millis(100))
    }

    /// A fresh headless app plus a window whose only pane is `pid`'s session:
    /// `start` is what the spawn recorded, `closed` when the reader saw its PTY
    /// close.
    fn app_with(pid: i32, start: Start, closed: Option<Instant>) -> (App, WindowId, u64) {
        let sink = std::sync::Arc::new(aterm_session::sink::SinkWriter::new(-1));
        app_with_sink(pid, start, closed, sink)
    }

    /// [`app_with`] over the caller's sink, for a test that writes to the PTY.
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
        // A shell this process spawned, as `spawn_session` records one: only
        // its own child is waited for (round seven, finding 25).
        session.shell_identity = aterm_pty::record_spawned_shell(pid);
        // The driven input is read at the spawn, before anything can write.
        session.shell_start = start.map(|(name, at)| crate::spawn::ShellStart {
            at,
            name: name.to_string(),
            input: session.ctx.sink.driven_input(),
        });
        if let Some(at) = closed {
            let _ = session.pty_closed_at.set(at);
        }
        // Registered, as every spawn seam does, so the registry's view shows.
        App::register_session(&app.store, &session, None);
        let wid = app.insert_logical_window(session, 24, 80);
        (app, wid, id)
    }

    /// The exit code the ledger noted for `session` (`None`: `exit_code=-`).
    fn ledger_code(app: &App, session: u64) -> Option<i32> {
        app.store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .noted_exit_code(session)
    }

    /// The reader's EOF, as the `Wake::Exit` arm runs it minus the event loop.
    fn reader_eof(app: &mut App, session: u64) -> Vec<WindowId> {
        app.note_session_exit(session);
        app.retire_input_watch(session);
        app.exit_session_logical(session)
    }

    fn pane_text(app: &App, session: u64) -> String {
        term_lock(&app.pool.get(session).expect("the pane's session").term).visible_content()
    }

    /// Whether the session's log still has its close due once the exit
    /// handling returned (`SessionLog::close_due`): `await log` waits for the
    /// retire's close while it holds, and answers `ERR exited` at once
    /// otherwise.
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

    /// `await log close=session` armed at `session`'s head now, on a thread
    /// of its own (`timeout=600000`); the receiver hears its reply.
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

    /// AN EXIT THAT ESCALATES TO ITS WINDOW'S CLOSE (phase 3A review): the
    /// log's close stays due past `exit_session_logical` until the window's
    /// close retires the session, and the close that retire writes is what a
    /// parked `await log` reads. NEGATIVE CONTROL: a window close a barrier
    /// refused retires nothing, and the settle `Wake::Exit` runs after its
    /// closes (`App::settle_log_close`) ends the wait with `ERR exited` (each
    /// a one-minute hang detector).
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
        // The window's close is refused: nothing retires the session.
        app.settle_log_close(id);
        assert!(!log_close_due(&app, id), "no close is coming");
        assert_eq!(
            waiter
                .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                .expect("answered by the settle"),
            "ERR exited\n"
        );
    }

    /// `await log close=session timeout=0` on `session` now: `OK timeout`
    /// while its close is due, `ERR exited` once none is coming.
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

    /// AN EXIT DEFERRED BEHIND A PENDING UPDATE HANDOFF (phase 3A review).
    /// The handoff holds the pane's close until its attempt completes, and
    /// the completion replays the exit; the replay's retire writes the log's
    /// close (`App::finish_update_handoff`). The close stays due across the
    /// deferral (`App::log_closes_deferred`), through both settles the exit
    /// path runs (its own end and `Wake::Exit`'s after its closes), so a
    /// parked `await log close=session` is answered that close. NEGATIVE
    /// CONTROL: a quit dominates the deferred exit and a barrier refuses it,
    /// so nothing replays the exit; the completion's settle after its replay
    /// ends the wait with `ERR exited` (each wait a one-minute hang detector).
    /// The completion is run here as `finish_update_handoff` runs it, without
    /// the event loop: the record goes, the deferred exits are taken, the
    /// teardown replays, the windows it returns close, and the taken exits
    /// settle.
    ///
    /// RED before the fix: the deferred close returned no window to close, so
    /// the end of the exit handling cleared the close due, and the waiter
    /// answered `ERR exited` before the replay wrote the close.
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
        // A barrier refused the quit: nothing replays the exit.
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

    /// THE BUG: an rc file's `exit 3` within moments of the spawn. The pane
    /// stays, the line names the shell and the code, and the classifier still
    /// publishes the failure.
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
        // One status for every reader: the classifier, the ledger, the line,
        // and any later look.
        assert_eq!(ledger_code(&app, id), Some(3), "the ledger has the code");
        for look in [ExitLook::First, ExitLook::Retry] {
            assert_eq!(
                app.exit_status(id, look),
                Some(aterm_pty::ChildExit::Code(3)),
                "{look:?} reads the status the first look kept"
            );
        }
        // Kept once, it closes the way a held pane does: by an explicit close,
        // which takes its window's last tab and hands the window to teardown.
        assert_eq!(
            app.close_session_by_id(id),
            Ok(CloseProgress::Deferred(wid)),
            "an explicit close still takes it"
        );
        let _ = app.close_window_logical(wid);
        assert!(app.pool.get(id).is_none(), "and the session is retired");
    }

    /// A shell that ends at start keeps its pane with the line naming the
    /// shell and its status, whoever reads that status: the classifier (the
    /// default) or, with `tab_status` off, the ledger's look, which reaps — and
    /// the ledger and the line agree. The window is measured to the reader's
    /// EOF stamp, not to when the main thread got to the exit: a slow launch
    /// that took 3 s to reach the exit, whose shell failed 1 s after its spawn,
    /// still keeps its pane.
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

    /// THE REVIEW'S CASE: a shell that started and was used. An rc file whose
    /// last line leaves `$?` at 1 (`[ -f ~/.fzf.zsh ] && source ~/.fzf.zsh`
    /// with no such file), then a Ctrl-D within 2 s: zsh exits 1, and the tab
    /// closes as the person asked.
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
        // SAFETY: both ends of the pipe opened above; the borrowed sink that
        // used the write end is gone.
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
    }

    /// THE QUERY CASE: fish (or zsh under powerlevel10k) asks for the device
    /// attributes as it starts, aterm's reply writer answers, and then its rc
    /// fails with `exit 1`. The answer is aterm's, not anyone's input: the
    /// pane stays with the line, where it used to close (and with one tab
    /// quit aterm) as if someone had typed into the shell.
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
        // The real reply writer answers `ESC [ c`, as the reader hands it the
        // engine's response; the shell's end of the PTY receives it.
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
        // SAFETY: both ends of the pipe opened above; the borrowed sink that
        // used the write end is gone.
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
    }

    /// NEGATIVE CONTROLS: a clean exit at start, a session with no recorded
    /// start (a `-e` command or an adopted shell, `records_shell_start`), a
    /// shell that fails after the start window, and a shell that drew its
    /// prompt (it started, whatever ends it next — here a `signal kill` sent to
    /// a fresh prompt) all close as today — the window's last tab goes, and the
    /// caller escalates to closing the window.
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
                // The shell-integration prompt marks around a prompt, as zsh
                // draws it.
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

    /// `--hold` keeps every pane its own way: no line is added.
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

    /// A CO-VIEWED shell that fails at start stays in EVERY window that shows
    /// it, and its one engine carries the line once.
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

    /// AN ADOPTED SESSION NEVER WAITS FOR ITS PID (round seven of the update
    /// audit, finding 25). Its shell's parent was the old build, so the number
    /// it carries is not this process's child — unless the kernel reissued it
    /// to one of ours after the shell exited, which is exactly when a `waitpid`
    /// on it reaps a stranger: the stand-in here is OUR zombie (exit 3) sitting
    /// under the adopted session's pid. The exit path must answer "unknown"
    /// and leave the zombie to its owner, whose own wait still reads 3.
    ///
    /// RED before the fix: the exit path reaped the stand-in, booked exit 3 as
    /// the adopted session's, and the owner's `waitpid` read `ECHILD`.
    #[test]
    fn an_adopted_session_never_reaps_the_child_its_freed_pid_now_names() {
        let pid = run_to_zombie("exit 3");
        let (mut app, _wid, id) = app_with(pid, None, None);
        // What `spawn_session` records for an adopted shell that no longer
        // leads its PTY: not our child, no birth.
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

    /// AN ADOPTED SESSION'S EXIT IS READ FROM ITS KERNEL WITNESS (round seven
    /// of the update audit, finding 26). The shell is alive when the session
    /// takes it and is never this process's to wait for — here its parent (the
    /// test, standing in for launchd) reaps it before the exit path looks, so
    /// `waitpid` could not answer even if it were asked. The watch
    /// `adopted_exit_watch` registered at the adoption still reads exit 5, and
    /// the classifier, the ledger and every later look agree on it.
    ///
    /// RED before the fix: `exit_code=-` and `Outcome::None` — every session
    /// alive across an update ended with an unknown status.
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
            // The adoption's record: a birth proved while it led its PTY, and
            // not our child.
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
