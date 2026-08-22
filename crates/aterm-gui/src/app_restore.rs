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
use winit::event_loop::ActiveEventLoop;

type BuiltRestoreTab = (crate::tab_model::Tab, Option<pane::PaneTree>);
type BuiltRestoreLeaf = (
    crate::tab_model::ViewId,
    crate::tab_model::TabPresentation,
    Option<crate::tab_model::TabId>,
);

/// Mint recovery authority only from the already-validated typed descriptor.
/// Its copyable metadata is deliberately never parsed to recover a path/route.
fn recovery_capability(
    native: &restore::NativeLeafRestore,
) -> Option<crate::native_app::RecoveryCapability> {
    match native.restore_tag.as_str() {
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
    /// deterministic; the frontmost-window choice is not persisted (current scope —
    /// the last restored window ends frontmost, matching a plain multi-window open).
    pub(crate) fn capture_restore_manifest(&self) -> restore::RestoreManifest {
        let windows = self
            .windows
            .values()
            .map(|ws| {
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
                    let placement = ws
                        .os_window
                        .as_ref()
                        .and_then(|w| crate::app_window::placement::read(w));
                    let pos = ws.os_window.as_ref().and_then(|w| w.outer_position().ok());
                    match placement {
                        Some((true, (x, y))) => {
                            (Some(winit::dpi::PhysicalPosition::new(x, y)), Some(true))
                        }
                        Some((false, _)) => (pos, Some(false)),
                        // Placement unreadable (no HWND yet, or the call failed):
                        // degrade to exactly the old capture — position only.
                        None => (pos, None),
                    }
                };
                // Off Windows the field stays un-captured (`None`): the macOS
                // strip keeps its zoom/solo-band semantics untouched, and the
                // Unix seamless commit's topology equality must never see a live
                // show-state bit it did not normalize (see
                // `commit_layout_topology`). Wiring those platforms up is a
                // deliberate follow-up, not an oversight.
                #[cfg(not(windows))]
                let (pos, maximized) = (
                    ws.os_window.as_ref().and_then(|w| w.outer_position().ok()),
                    None::<bool>,
                );
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
                    let Some(restored) = self.tab_restore_descriptor(tab) else {
                        continue;
                    };
                    if ws.tab_set.active_id() == Some(tab.id) {
                        active_item = Some(restored_tabs.len());
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
                        self.native_restore_descriptor(tab).map(|descriptor| {
                            let index = native_tabs.len();
                            native_tabs.push(descriptor);
                            restore::TabOrderEntry::Native { index }
                        })
                    };
                    if let Some(entry) = entry {
                        tab_order.push(entry);
                    }
                }
                restore::WindowLayout {
                    rows: ws.rows,
                    cols: ws.cols,
                    active_tab: ws.tabs.active,
                    outer_x: pos.map(|p| p.x),
                    outer_y: pos.map(|p| p.y),
                    maximized,
                    tabs: terminal_tabs,
                    native_tabs,
                    tab_order,
                    active_item,
                    restored_tabs,
                }
            })
            .collect();
        restore::RestoreManifest::new(windows)
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
                    crate::native_app::AppKind::Settings => unreachable!(),
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
        fn capture(
            app: &App,
            node: &crate::tab_model::SplitTree<crate::tab_model::ViewId>,
            focus: crate::tab_model::ViewId,
            path: &mut Vec<restore::RestoreBranch>,
            focused_path: &mut Option<Vec<restore::RestoreBranch>>,
        ) -> Option<restore::RestoredSplitTree> {
            match node {
                crate::tab_model::SplitTree::Leaf(view) => {
                    if *view == focus {
                        *focused_path = Some(path.clone());
                    }
                    Some(restore::RestoredSplitTree::leaf(
                        app.view_restore_descriptor(*view)?,
                    ))
                }
                crate::tab_model::SplitTree::Split {
                    axis,
                    ratio,
                    first,
                    second,
                } => {
                    path.push(restore::RestoreBranch::First);
                    let first = capture(app, first, focus, path, focused_path)?;
                    path.pop();
                    path.push(restore::RestoreBranch::Second);
                    let second = capture(app, second, focus, path, focused_path)?;
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
                        },
                    ));
                }
                let mut descriptor = match (app.kind(), state) {
                    (
                        crate::native_app::AppKind::Settings,
                        crate::native_app::AppViewState::Settings(settings),
                    ) => restore::NativeLeafRestore::settings(
                        if settings.route == crate::native_settings::SettingsRoute::Manual {
                            // Manual is a launcher for the separately persisted
                            // config Editor tab, never a restorable interstitial.
                            crate::native_settings::SettingsRoute::Home.path()
                        } else {
                            settings.route.path()
                        }
                        .to_string(),
                    ),
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
                    crate::native_app::AppViewState::Settings(_) => {}
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
            crate::native_app::AppViewState::Settings(_) => true,
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

    /// Rebuild the previous quit's layout (one-shot: drains `pending_restore`; a second
    /// `resumed` no-ops). Called after the first OS window is attached, so extra
    /// windows can create their surfaces and every spawned pane has a live event loop.
    pub(crate) fn apply_pending_restore(&mut self, el: &ActiveEventLoop) {
        // Rebuild the persisted layout when there is one. A SEAMLESS adopt with NO
        // manifest (its layout write failed) still falls through to the orphan net below,
        // so its handed-off shells are placed as tabs rather than stranded — the layout is
        // recoverable, a lost shell is not.
        if let Some(manifest) = self.pending_restore.take() {
            self.apply_restore_manifest(el, manifest);
        }
        // SEAMLESS orphan safety net: any handed-off live shell the layout did NOT place
        // (a stale/inconsistent/absent manifest, or a leaf that failed to rebuild) is
        // adopted anyway as a fresh tab in the front window. A live shell is NEVER dropped
        // just because its exact pane could not be reconstructed. A no-op on a cold
        // restore (`seamless_adopt` is empty).
        self.adopt_orphan_shells_as_tabs();
    }

    /// Rebuild one consumed [`restore::RestoreManifest`] into live windows/tabs/panes
    /// (adopting handed-off shells where a leaf's `local_id` matches, forking fresh
    /// otherwise). Split out from [`Self::apply_pending_restore`] so the orphan net there
    /// runs on every path, including a seamless adopt whose manifest was absent.
    fn apply_restore_manifest(&mut self, el: &ActiveEventLoop, manifest: restore::RestoreManifest) {
        let mut windows = manifest.windows.into_iter();
        // The first persisted window maps onto the bootstrap window: session 0 already
        // runs in the persisted first-leaf cwd (seeded in `main`), so its layout is
        // rebuilt in place around that live session. Its GEOMETRY is deliberately not
        // applied here: the grid seeded window 1's creation (`run`), and the
        // position/maximized state applied post-attach in `resumed` — both strictly
        // earlier than this deferred (post-first-present) pass, so the bootstrap frame
        // never visibly hops the way a here-applied move would.
        if let Some(wl) = windows.next()
            && let Some(front) = self.frontmost_window
        {
            self.frontmost_window = Some(front);
            self.restore_into_window(front, wl);
        }
        // Every further persisted window: a full window create — its first session
        // spawns straight into the persisted first-leaf cwd — then the same fill. A
        // native-only window takes the zero-session path: install its native descriptors
        // into an empty logical host before attaching glass, so restore never invents a
        // PTY merely to satisfy legacy mirrors.
        for wl in windows {
            let recursive_terminal = wl
                .restored_tabs
                .iter()
                .find_map(|tab| Self::first_terminal_restore_leaf(&tab.root));
            let native_only = if wl.restored_tabs.is_empty() {
                wl.tabs.is_empty() && !wl.native_tabs.is_empty()
            } else {
                recursive_terminal.is_none()
            };
            if native_only {
                let previous_front = self.frontmost_window;
                let outer = (wl.outer_x, wl.outer_y);
                let maximized = wl.maximized;
                let wid = self.create_native_restore_window(wl.rows, wl.cols);
                self.restore_into_window(wid, wl);
                let restored = self
                    .windows
                    .get(&wid)
                    .is_some_and(|ws| !ws.tab_set.is_empty());
                if !restored {
                    self.windows.remove(&wid);
                    self.frontmost_window = previous_front;
                    continue;
                }
                if !self.headless && !self.attach_os_window(el, wid) {
                    self.close_window_logical(wid);
                    continue;
                }
                // W4: validated against the LIVE monitor set first — same
                // contract as the terminal-window arm below.
                if let (Some(x), Some(y)) = outer
                    && let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref())
                    && crate::app_window::restored_position_on_screen(w, x, y)
                {
                    w.set_outer_position(winit::dpi::PhysicalPosition::new(x, y));
                }
                // W3: maximized LAST — see the ordering note below.
                if maximized == Some(true)
                    && let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref())
                {
                    w.set_maximized(true);
                }
                self.sync_active_session();
                continue;
            }
            let first_leaf = wl.tabs.first().and_then(|t| t.leaves().first().copied());
            let cwd0: Option<String> = recursive_terminal
                .and_then(|leaf| leaf.cwd.clone())
                .or_else(|| first_leaf.and_then(|leaf| leaf.cwd()).map(String::from));
            // SEAMLESS: adopt this window's first-leaf shell as its bootstrap session
            // (matched by id), so the reopened window comes up with its LIVE shell rather
            // than a fresh one. Consumed (removed) so it is never adopted twice; a cold
            // restore's `seamless_adopt` is empty, so this is always `None` there.
            let local_id = recursive_terminal
                .and_then(|leaf| leaf.local_id)
                .or_else(|| first_leaf.and_then(|leaf| leaf.local_id()));
            let adopt = local_id
                .and_then(|id| self.seamless_adopt.iter().position(|a| a.local_id == id))
                .map(|pos| self.seamless_adopt.remove(pos));
            let outer = (wl.outer_x, wl.outer_y);
            let maximized = wl.maximized;
            let Some(wid) = self.create_window_internal(el, cwd0.as_deref(), adopt) else {
                // A window create fails on spawn or GPU-surface trouble — both likely to
                // repeat. Keep what restored rather than looping on failures. RESIDUAL
                // RISK (rare): if this window carried an adopted shell as its first leaf,
                // that shell is lost with the failed window (its OTHER panes' shells stay
                // in `seamless_adopt` and the orphan net below re-homes them as tabs).
                // Fully protecting the first-leaf shell here needs recovering it from the
                // half-built window — the "adopt into the pool first, then build windows"
                // rework — deferred; window-surface failure on an already-running app is
                // exceptional.
                eprintln!("aterm-gui: session restore: could not create a window; stopping here");
                self.surface_gesture_failure(
                    "✕ Restore stopped early — some saved tabs were not reopened",
                );
                // OVERLAP HANDOFF: a carried window is now LOST (and possibly
                // its adopted shell with it). Withhold the readiness byte —
                // the parked parent's timeout + rollback recovers EVERY shell
                // from its own still-open fds, strictly better than exiting
                // under this degraded boot (see `App::handoff_degraded`).
                if self.handoff_ready.is_some() {
                    self.handoff_degraded = true;
                }
                break;
            };
            // SEAMLESS/RESTORE: put the reopened window back where it was (before its
            // first present, so there is no visible hop). Best-effort — a missing carry
            // or an off-desktop coordinate just leaves the cascade position the attach
            // applied. The off-desktop half is ENFORCED, not assumed (W4): the persisted
            // point is validated against the monitors that exist NOW, because the set
            // that existed at capture time is exactly what an undock/unplug changes —
            // a raw `set_outer_position` here used to reopen those windows entirely
            // outside the desktop.
            if let (Some(x), Some(y)) = outer
                && let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref())
                && crate::app_window::restored_position_on_screen(w, x, y)
            {
                w.set_outer_position(winit::dpi::PhysicalPosition::new(x, y));
            }
            // W3: re-maximize AFTER the position lands. The order is load-bearing
            // twice over: winit's Windows backend clears its MAXIMIZED window flag
            // inside `set_outer_position` (a moved window is definitionally not
            // maximized to it), and maximizing FIRST would anchor the frame to
            // whatever monitor the cascade picked rather than the one the persisted
            // point names. `Some(true)` only — `None` (an old manifest, or a
            // platform that does not capture the state) must never force a change.
            if maximized == Some(true)
                && let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref())
            {
                w.set_maximized(true);
            }
            self.restore_into_window(wid, wl);
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
        self.windows.insert(wid, ws);
        self.frontmost_window = Some(wid);
        wid
    }

    /// Place any still-unadopted handed-off shells ([`Self::seamless_adopt`] left over
    /// after the layout was rebuilt) as fresh single-pane tabs in the front window, so no
    /// live shell handed across a seamless update is ever lost. Empty ⇒ no-op (every cold
    /// restore, and the normal seamless case where the layout placed them all).
    fn adopt_orphan_shells_as_tabs(&mut self) {
        if self.seamless_adopt.is_empty() {
            return;
        }
        let orphans = std::mem::take(&mut self.seamless_adopt);
        let (Some(wid), Some(proxy)) = (self.frontmost_window, self.proxy.clone()) else {
            // No window/proxy to place them in (should not happen post-restore): drop the
            // Adopted holders — their raw fds close with the process, ending the shells.
            eprintln!(
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
            match spawn_session(
                id,
                wid,
                rows,
                cols,
                &self.session_factory,
                &proxy,
                None,
                Some(adopted),
            ) {
                Ok(s) => {
                    self.next_session_id += 1;
                    Self::register_session(&self.store, &s, None);
                    let tree = pane::PaneTree::new(id);
                    let tab = crate::register_terminal_tab(
                        &mut self.tab_ids,
                        &mut self.view_store,
                        &tree,
                    )
                    .expect("restored orphan tab identity space");
                    self.pool.insert(s);
                    if let Some(ws) = self.windows.get_mut(&wid) {
                        ws.layouts.push(tree);
                        ws.tabs.add();
                        ws.tab_set.push(tab).expect("fresh restored tab id");
                    }
                }
                Err(e) => {
                    eprintln!("aterm-gui: seamless: could not adopt an orphan shell: {e}");
                    self.surface_gesture_failure(&format!(
                        "✕ A live shell was lost across the update: {e}"
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

    /// Fill window `wid` from a validated mixed-tab restore record. Terminal trees keep
    /// their RESTORE-1 compatibility projection; native descriptors mint fresh runtime
    /// identities and are interleaved afterward by stable kind/URI order.
    fn restore_into_window(&mut self, wid: WindowId, wl: restore::WindowLayout) {
        if !wl.restored_tabs.is_empty() {
            self.restore_recursive_into_window(wid, wl);
            return;
        }
        let Some(order) = wl.canonical_order() else {
            eprintln!("aterm-gui: session restore: invalid mixed-tab ordering; keeping bootstrap");
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
                eprintln!(
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
                eprintln!(
                    "aterm-gui: session restore: native descriptor aliased an existing tab; skipped"
                );
                native_ids.push(None);
            } else {
                native_ids.push(candidate);
            }
        }

        // A cold native-only first window starts with the application's unavoidable
        // process bootstrap shell. Once at least one real native descriptor has reopened,
        // retire that terminal completely; later native-only windows take the zero-session
        // creation path above and never need this conversion.
        if terminal_layouts.is_empty()
            && had_bootstrap_terminal
            && !self.bootstrap_session_adopted
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
            debug_assert!(self.structural_invariants_ok());
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
            .any(|tab| Self::first_terminal_restore_leaf(&tab.root).is_some());
        let preserve_adopted_bootstrap =
            !has_terminal && self.bootstrap_session_adopted && self.window_has_terminal_tab(wid);
        let mut reusable_terminal = if preserve_adopted_bootstrap {
            None
        } else {
            self.clear_restore_target(wid, has_terminal)
        };

        self.frontmost_window = Some(wid);
        let mut built = Vec::with_capacity(restored_tabs.len());
        for record in &restored_tabs {
            match self.build_recursive_restore_tab(wid, record, &mut reusable_terminal) {
                Ok(tab) => built.push(tab),
                Err(error) => eprintln!(
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
        if self
            .windows
            .get(&wid)
            .is_some_and(|window| !window.tab_set.is_empty())
        {
            debug_assert!(self.structural_invariants_ok());
        }
    }

    fn window_has_terminal_tab(&self, wid: WindowId) -> bool {
        self.windows.get(&wid).is_some_and(|window| {
            window.tab_set.tabs().iter().any(|tab| {
                tab.root.leaves().into_iter().any(|view| {
                    matches!(
                        self.view_store.get(view),
                        Some(crate::tab_model::View::Terminal(_))
                    )
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

    fn first_terminal_restore_leaf(
        tree: &restore::RestoredSplitTree,
    ) -> Option<&restore::TerminalLeafRestore> {
        match tree {
            restore::RestoredSplitTree::Leaf {
                view: restore::RestoredView::Terminal(terminal),
            } => Some(terminal),
            restore::RestoredSplitTree::Leaf { .. } => None,
            restore::RestoredSplitTree::Split { first, second, .. } => {
                Self::first_terminal_restore_leaf(first)
                    .or_else(|| Self::first_terminal_restore_leaf(second))
            }
        }
    }

    fn build_recursive_restore_tab(
        &mut self,
        wid: WindowId,
        record: &restore::RestoredTab,
        reusable_terminal: &mut Option<(crate::tab_model::ViewId, u64)>,
    ) -> Result<BuiltRestoreTab, String> {
        let reusable_before = *reusable_terminal;
        let mut leaves = Vec::with_capacity(record.root.leaf_count());
        let root = match self.build_recursive_restore_tree(
            wid,
            &record.root,
            reusable_terminal,
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
        leaves: &mut Vec<BuiltRestoreLeaf>,
    ) -> Result<crate::tab_model::SplitTree<crate::tab_model::ViewId>, String> {
        match tree {
            restore::RestoredSplitTree::Leaf { view } => {
                let (view, presentation, staging_tab) =
                    self.build_recursive_restore_leaf(wid, view, reusable_terminal)?;
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
                    leaves,
                )?),
                second: Box::new(self.build_recursive_restore_tree(
                    wid,
                    second,
                    reusable_terminal,
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
    ) -> Result<BuiltRestoreLeaf, String> {
        match descriptor {
            restore::RestoredView::Terminal(terminal) => {
                match self.restore_terminal_leaf(wid, terminal, reusable_terminal) {
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
                                "cwd={:?}\ntitle={:?}",
                                terminal.cwd, terminal.title
                            )),
                        },
                    ),
                }
            }
            restore::RestoredView::Native(native) => match self.restore_native_leaf(wid, native) {
                Ok(restored) => Ok(restored),
                Err(error) => self.restore_recovery_leaf_with_capability(
                    wid,
                    &restore::PlaceholderLeafRestore {
                        restore_tag: native.restore_tag.clone(),
                        reason: Self::bounded_recovery_text(&error),
                        metadata: Self::bounded_recovery_text(&format!(
                            "route={:?}\nuri={:?}\ndurable_seq={}",
                            native.route, native.uri, native.durable_seq
                        )),
                    },
                    recovery_capability(native),
                ),
            },
            restore::RestoredView::Placeholder(placeholder) => {
                self.restore_recovery_leaf(wid, placeholder)
            }
        }
    }

    fn restore_terminal_leaf(
        &mut self,
        wid: WindowId,
        terminal: &restore::TerminalLeafRestore,
        reusable_terminal: &mut Option<(crate::tab_model::ViewId, u64)>,
    ) -> Result<crate::tab_model::ViewId, String> {
        if let Some((view, _)) = reusable_terminal.take() {
            return Ok(view);
        }
        if let Some(session) = terminal
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
            let session = crate::stub_session(id);
            Self::seed_restored_user_meta(&session, terminal);
            let view = self
                .view_store
                .insert_terminal(id)
                .map_err(|_| "terminal view identity space exhausted".to_string())?;
            self.next_session_id = self.next_session_id.saturating_add(1);
            Self::register_session(&self.store, &session, None);
            self.pool.insert(session);
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
        let adopt = terminal
            .local_id
            .and_then(|local| {
                self.seamless_adopt
                    .iter()
                    .position(|item| item.local_id == local)
            })
            .map(|index| self.seamless_adopt.remove(index));
        let session = spawn_session(
            id,
            wid,
            rows,
            cols,
            &self.session_factory,
            &proxy,
            terminal.cwd.as_deref(),
            adopt,
        )
        .map_err(|error| error.to_string())?;
        Self::seed_restored_user_meta(&session, terminal);
        self.next_session_id = self.next_session_id.saturating_add(1);
        let view = self
            .view_store
            .insert_terminal(id)
            .map_err(|_| "terminal view identity space exhausted".to_string())?;
        Self::register_session(&self.store, &session, None);
        self.pool.insert(session);
        Ok(view)
    }

    /// Re-seed a freshly-respawned session's USER metadata (`meta set` fields)
    /// from its restore leaf, BEFORE the session is registered/visible — so a
    /// restored tab reappears under the operator-chosen title (which outranks
    /// the OSC title in tab labels) with its description/icon intact. A leaf
    /// with no captured metadata is a no-op (no lock churn on the common path).
    fn seed_restored_user_meta(session: &crate::Session, leaf: &restore::TerminalLeafRestore) {
        if leaf.user_title.is_none()
            && leaf.description.is_none()
            && leaf.icon.is_none()
            && leaf.role.is_none()
            && leaf.attention.is_none()
        {
            return;
        }
        let mut meta = session.ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
        let _ = meta.set("title", leaf.user_title.clone());
        let _ = meta.set("description", leaf.description.clone());
        let _ = meta.set("icon", leaf.icon.clone());
        let _ = meta.set("role", leaf.role.clone());
        let _ = meta.set("attention", leaf.attention.clone());
    }

    fn restore_native_leaf(
        &mut self,
        wid: WindowId,
        descriptor: &restore::NativeLeafRestore,
    ) -> Result<BuiltRestoreLeaf, String> {
        match descriptor.restore_tag.as_str() {
            "settings" => {
                let mut route = descriptor
                    .route
                    .as_deref()
                    .and_then(crate::native_settings::SettingsRoute::from_path)
                    .ok_or_else(|| "Settings route is unavailable".to_string())?;
                if route == crate::native_settings::SettingsRoute::Manual {
                    // Compatibility with manifests captured before Manual became
                    // a real Editor: restore the Settings launcher to Top Settings.
                    route = crate::native_settings::SettingsRoute::Home;
                }
                self.stage_settings_restore_view(wid, route)?;
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
        let (view, presentation, staging_tab) = self.detach_restore_staging_tab(wid)?;
        if !self.apply_native_view_restore(view, descriptor) {
            self.remove_view_link(view);
            return Err("The saved view state did not match the reopened app".to_string());
        }
        Ok((view, presentation, Some(staging_tab)))
    }

    fn stage_settings_restore_view(
        &mut self,
        wid: WindowId,
        route: crate::native_settings::SettingsRoute,
    ) -> Result<(), String> {
        let snapshot = self.native_config_service.snapshot();
        let mut state = crate::native_settings::SettingsViewState::from_snapshot(&snapshot)
            .expect("versioned config service snapshots are valid Settings input");
        let _ = state.replace_title_summary_health(self.title_summary_health());
        state.navigate(route);
        let view_state = crate::native_app::AppViewState::Settings(Box::new(state));
        let presentation = crate::tab_model::TabPresentation {
            title: "Settings".to_string(),
            icon: Some(crate::tab_model::TabIconKind::Settings),
            indicators: crate::tab_model::TabIndicators::default(),
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
            let app = crate::native_app::NativeApp::Settings(
                crate::native_settings::SettingsApp::new_at_config_revision(
                    self.update_snapshot(checking),
                    snapshot.revision,
                ),
            );
            self.install_new_native_tab(wid, app, view_state, presentation)
                .map(|_| ())
        };
        install.map_err(|error| format!("Settings restore view could not be installed: {error:?}"))
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
            crate::native_app::AppKind::Settings | crate::native_app::AppKind::Recovery => {
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
                    icon: Some(match kind {
                        crate::native_app::AppKind::Markdown => {
                            crate::tab_model::TabIconKind::Markdown
                        }
                        crate::native_app::AppKind::Editor => crate::tab_model::TabIconKind::Editor,
                        crate::native_app::AppKind::Settings => {
                            crate::tab_model::TabIconKind::Settings
                        }
                        crate::native_app::AppKind::Recovery => {
                            crate::tab_model::TabIconKind::Recovery
                        }
                    }),
                    indicators: crate::tab_model::TabIndicators::default(),
                    closable: true,
                    tooltip: Some(format!("{} · {uri}", kind.as_str())),
                },
            )
            .map_err(|error| format!("document restore view could not be installed: {error:?}"))?;
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
        self.build_recursive_restore_leaf(wid, descriptor, &mut reusable)
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
        let (tab, layout) = self.build_recursive_restore_tab(wid, record, &mut reusable)?;
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
        debug_assert!(self.structural_invariants_ok());
        Ok(id)
    }

    fn restore_native_tab_into_window(
        &mut self,
        wid: WindowId,
        descriptor: &restore::NativeTabRestore,
    ) -> Option<crate::tab_model::TabId> {
        let result = match descriptor {
            restore::NativeTabRestore::Settings { route } => {
                let Some(route) = crate::native_settings::SettingsRoute::from_path(route) else {
                    eprintln!("aterm-gui: session restore: invalid Settings route");
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
                eprintln!("aterm-gui: session restore: native tab skipped: {error}");
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
        // A real run always has a proxy; the headless test harness (None) never gets
        // here (restore is never taken for headless runs). Guard, don't panic.
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
            let adopt = leaf
                .local_id()
                .and_then(|lid| self.seamless_adopt.iter().position(|a| a.local_id == lid))
                .map(|pos| self.seamless_adopt.remove(pos));
            match spawn_session(
                id,
                wid,
                rows,
                cols,
                &self.session_factory,
                &proxy,
                leaf.cwd(),
                adopt,
            ) {
                Ok(s) => {
                    self.next_session_id += 1;
                    ids.push(id);
                    fresh.push(s);
                }
                Err(e) => {
                    eprintln!("aterm-gui: session restore: spawn failed: {e}");
                    self.surface_gesture_failure(&format!(
                        "✕ A restored tab could not start its shell: {e}"
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
            self.pool.insert(s);
        }
        Some(tree)
    }
}

#[cfg(test)]
mod tests {
    use crate::{App, CloseOutcome, WindowId, pane, restore};

    #[test]
    fn recovery_authority_comes_only_from_typed_restore_fields() {
        let valid =
            restore::NativeLeafRestore::document("markdown", "file:///tmp/Guide.md".to_string());
        assert!(matches!(
            super::recovery_capability(&valid),
            Some(crate::native_app::RecoveryCapability::Document {
                kind: crate::native_app::AppKind::Markdown,
                ref uri,
                config_editor: false,
            }) if uri == "file:///tmp/Guide.md"
        ));

        let mut manual =
            restore::NativeLeafRestore::document("editor", "file:///tmp/aterm.toml".to_string());
        manual.config_editor = true;
        assert!(matches!(
            super::recovery_capability(&manual),
            Some(crate::native_app::RecoveryCapability::Document {
                kind: crate::native_app::AppKind::Editor,
                ref uri,
                config_editor: true,
            }) if uri == "file:///tmp/aterm.toml"
        ));

        let mut diagnostics_only = restore::NativeLeafRestore::document(
            "future.canvas",
            "file:///tmp/ignored".to_string(),
        );
        diagnostics_only.metadata = "uri=Some(\"file:///tmp/secret\")".to_string();
        assert_eq!(super::recovery_capability(&diagnostics_only), None);

        let unsafe_uri = restore::NativeLeafRestore::document(
            "editor",
            "file:///tmp/ok\nhttps://attacker.example".to_string(),
        );
        assert_eq!(super::recovery_capability(&unsafe_uri), None);
    }

    fn file_uri(path: &std::path::Path) -> String {
        format!("file://{}", path.to_string_lossy().replace(' ', "%20"))
    }

    /// Install a stub session `id` in the pool + registry (the headless analogue of a
    /// real spawn — see `stub_session`).
    fn add_stub(app: &mut App, id: u64) {
        let s = crate::stub_session(id);
        App::register_session(&app.store, &s, None);
        app.pool.insert(s);
        app.next_session_id = app.next_session_id.max(id + 1);
    }

    /// Quit-side capture: a live App with a split first tab + a second tab yields a
    /// manifest with the same shape (tab count, split structure, focus tagging, grid
    /// size, active tab) — and it round-trips through the persisted TOML byte-for-byte.
    #[test]
    fn capture_reflects_the_live_layout() {
        let mut app = App::headless_for_test();
        // Split tab 0 (stub session 1 joins session 0)…
        add_stub(&mut app, 1);
        app.view_store
            .insert_terminal(1)
            .expect("split view identity");
        {
            let ws = app.windows.get_mut(&WindowId(0)).expect("window 0");
            assert!(ws.layouts[0].split_focused(pane::SplitDir::Vertical, 1));
        }
        assert!(app.sync_tab_model_from_layout(WindowId(0), 0));
        // …and append a second tab (stub session 2), which becomes active.
        add_stub(&mut app, 2);
        let tree = pane::PaneTree::new(2);
        let tab = crate::register_terminal_tab(&mut app.tab_ids, &mut app.view_store, &tree)
            .expect("second tab identity");
        let ws = app.windows.get_mut(&WindowId(0)).expect("window 0");
        ws.layouts.push(tree);
        ws.tabs.add();
        ws.tab_set.push(tab).expect("fresh tab id");
        app.resync_active_or_window(WindowId(0));
        assert!(app.structural_invariants_ok());

        let m = app.capture_restore_manifest();
        assert_eq!(m.windows.len(), 1);
        let w = &m.windows[0];
        assert_eq!((w.rows, w.cols), (24, 80), "headless harness grid");
        assert_eq!(w.active_tab, 1, "the appended tab was active");
        assert_eq!(w.tabs.len(), 2);
        assert_eq!(w.tabs[0].leaf_count(), 2, "tab 0 kept its split");
        assert_eq!(w.tabs[1].leaf_count(), 1);
        // Focus tagging: `split_focused` moved focus to the NEW pane (session 1), the
        // second tree-order leaf of tab 0.
        let leaves = w.tabs[0].leaves();
        assert!(
            matches!(leaves[1], restore::PaneLayout::Leaf { focused: true, .. }),
            "the split's new pane holds tab 0's focus",
        );
        assert!(
            matches!(leaves[0], restore::PaneLayout::Leaf { focused: false, .. }),
            "the original pane lost it",
        );
        // And the capture survives the on-disk format round trip.
        let back =
            restore::RestoreManifest::from_toml(&m.to_toml().expect("serialize")).expect("parse");
        assert_eq!(m, back);
    }

    /// CONTENT capture: a session that reported its cwd via OSC 7 (the shell-integration
    /// mechanism) yields a manifest leaf carrying that exact cwd. This is the end-to-end
    /// half of the RESTORE-1 promise the v0.26 demo-day battery found broken — not in
    /// this code, but because the shipped zsh integration aborted before emitting OSC 7
    /// (fixed in `aterm_shell_integration.zsh`). Here we drive the engine's OSC 7 handler
    /// directly, proving `capture_restore_manifest` reads the cwd through to the manifest.
    #[test]
    fn capture_records_the_osc7_cwd() {
        let app = App::headless_for_test();
        // Feed session 0's engine a real OSC 7 cwd report, exactly as the shell emits it:
        // ESC ] 7 ; file://host/abs/path BEL.
        {
            let s = app.pool.get(0).expect("session 0");
            let mut term = crate::term_lock(&s.term);
            term.process(b"\x1b]7;file://localhost/tmp/demo-cwd\x07");
            assert_eq!(
                term.current_working_directory(),
                Some("/tmp/demo-cwd"),
                "OSC 7 must set the engine cwd (pre-req for capture)"
            );
        }
        let m = app.capture_restore_manifest();
        let leaves = m.windows[0].tabs[0].leaves();
        assert_eq!(
            leaves[0].cwd(),
            Some("/tmp/demo-cwd"),
            "the manifest leaf must carry the session's OSC-7 cwd — the flagship \
             'reopen where you were' content",
        );
    }

    /// Drive session 0's engine with raw escape bytes, exactly as a PTY would.
    /// The tab-label tests below all report titles/cwds this way so they prove
    /// the REAL OSC 0/2/7 ingestion path, not a state poke.
    fn feed_session0(app: &App, bytes: &[u8]) {
        let s = app.pool.get(0).expect("session 0");
        crate::term_lock(&s.term).process(bytes);
    }

    /// TAB-LABEL DEFAULT (cwd-as-title): a program-set OSC 0/2 title always
    /// outranks the shell-reported cwd — the cwd is the DEFAULT label, never a
    /// displacement of an explicit title.
    #[test]
    fn tab_title_prefers_the_live_title_over_the_reported_cwd() {
        let mut app = App::headless_for_test();
        feed_session0(&app, b"\x1b]2;vim src/main.rs\x07");
        feed_session0(&app, b"\x1b]7;file://localhost/aterm-proof/cwd-loses\x07");
        assert_eq!(
            app.tab_titles(WindowId(0))[0],
            "vim src/main.rs",
            "a live OSC title must win over the reported cwd"
        );
    }

    /// TAB-LABEL DEFAULT (cwd-as-title): with NO program-set title, a terminal
    /// tab is labeled with its session's shell-reported cwd — the feature's
    /// core promise (each tab shows WHERE it is, Ghostty/iTerm style, without
    /// needing the integration's OSC 0 title line).
    #[test]
    fn tab_title_defaults_to_the_reported_cwd_when_no_title_is_set() {
        let mut app = App::headless_for_test();
        // A path outside `$HOME` must read VERBATIM. Assert the disjointness
        // precondition so a bizarre `$HOME` cannot silently turn this into an
        // abbreviation test.
        let cwd = "/aterm-proof/outside-home";
        assert_eq!(
            crate::app_tabs::cached_home()
                .and_then(|home| crate::app_tabs::home_relative_suffix(cwd, home)),
            None,
            "fixture path must not live under $HOME"
        );
        feed_session0(
            &app,
            b"\x1b]7;file://localhost/aterm-proof/outside-home\x07",
        );
        assert_eq!(app.tab_titles(WindowId(0))[0], cwd);
    }

    /// TAB-LABEL DEFAULT (cwd-as-title): a cwd under `$HOME` reads in the `~…`
    /// form — byte-matching what the zsh integration's precmd puts in OSC 0
    /// titles (`~` for home itself, `~/sub` below it) — so the no-integration
    /// default label is indistinguishable from the integrated one.
    #[test]
    fn tab_title_abbreviates_a_home_cwd_like_the_shell_integration() {
        let Some(home) = crate::app_tabs::cached_home() else {
            return; // no $HOME in this environment -> nothing to abbreviate
        };
        let mut app = App::headless_for_test();
        feed_session0(
            &app,
            format!("\x1b]7;file://localhost{home}/aterm-abbrev-proof\x07").as_bytes(),
        );
        assert_eq!(app.tab_titles(WindowId(0))[0], "~/aterm-abbrev-proof");
        feed_session0(
            &app,
            format!("\x1b]7;file://localhost{home}\x07").as_bytes(),
        );
        assert_eq!(
            app.tab_titles(WindowId(0))[0],
            "~",
            "home itself reads as the bare ~"
        );
    }

    /// TAB-LABEL DEFAULT (cwd-as-title): the cwd outranks the tab PRESENTATION
    /// title, and an unset/cleared cwd falls through to it (and finally to the
    /// literal "aterm") — the pre-feature fallback rungs survive verbatim
    /// below the new cwd rung, so a session with no shell integration at all
    /// labels exactly as before.
    #[test]
    fn tab_title_cwd_outranks_presentation_and_clearing_falls_back() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        // Titleless, cwd-less baseline: a plain terminal tab's presentation
        // title is empty, so the label is the literal "aterm".
        assert_eq!(app.tab_titles(wid)[0], "aterm");
        // A presentation title takes the slot while nothing livelier exists.
        app.windows
            .get_mut(&wid)
            .expect("window 0")
            .tab_set
            .active_mut()
            .expect("active tab")
            .presentation
            .title = "Renamed".to_string();
        assert_eq!(app.tab_titles(wid)[0], "Renamed");
        // A reported cwd outranks the presentation…
        feed_session0(&app, b"\x1b]7;file://localhost/aterm-proof/live\x07");
        assert_eq!(app.tab_titles(wid)[0], "/aterm-proof/live");
        // …and CLEARING it (empty OSC 7 — integration torn down) falls back to
        // the presentation again, never to a stale path.
        feed_session0(&app, b"\x1b]7;\x07");
        assert_eq!(app.tab_titles(wid)[0], "Renamed");
    }

    /// TAB-LABEL DEFAULT (cwd-as-title): NON-terminal tabs are untouched by
    /// the cwd rung — a native tab keeps its presentation title even while a
    /// terminal sibling in the SAME window is labeled by its session's cwd
    /// (native views own no session, so no cwd can ever apply to them).
    #[test]
    fn native_tab_titles_ignore_the_terminal_cwd_default() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(
            app.open_settings_tab(crate::native_settings::SettingsRoute::default()),
            "settings tab installs in the headless fixture"
        );
        feed_session0(
            &app,
            b"\x1b]7;file://localhost/aterm-proof/terminal-only\x07",
        );
        assert_eq!(
            app.tab_titles(wid),
            vec!["/aterm-proof/terminal-only", "Settings"],
            "the cwd labels ONLY the terminal tab; the native tab keeps its \
             presentation title"
        );
    }

    /// TAB-LABEL DEFAULT (cwd-as-title): a cwd-ONLY change moves the tab-strip
    /// fingerprint, because the fingerprint hashes the exact `tab_titles`
    /// bytes — this is what makes a titleless `cd` repaint the strip through
    /// the ordinary RepaintKey path with no extra wiring.
    #[test]
    fn cwd_only_change_moves_the_tab_strip_fingerprint() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.tab_strip_rows = 1; // enable the strip (the fingerprint is pinned 0 when disabled)
        let fingerprint = |app: &mut App| {
            let titles = app.tab_titles(wid);
            let metadata = app.tab_strip_metadata(wid);
            app.tab_strip_fingerprint_from_parts(wid, &titles, &metadata, 0)
        };
        let before = fingerprint(&mut app);
        feed_session0(&app, b"\x1b]7;file://localhost/aterm-proof/fingerprint\x07");
        assert_ne!(
            before,
            fingerprint(&mut app),
            "a cwd-only change must move the strip fingerprint (it repaints the strip)"
        );
    }

    /// SESSION-METADATA stage 1 — the USER title (`meta set title`) is the TOP
    /// tab-label rung: it outranks a live OSC 0/2 title AND the reported cwd,
    /// and CLEARING it (`meta unset title`) falls back to the live title — the
    /// pre-existing chain survives verbatim beneath the new rung. Driven through
    /// the real ctx the label chain reads (the same object the `meta` verb
    /// mutates), against real OSC ingestion.
    #[test]
    fn user_title_outranks_the_live_osc_title_and_clearing_falls_back() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        feed_session0(&app, b"\x1b]2;vim src/main.rs\x07");
        feed_session0(&app, b"\x1b]7;file://localhost/aterm-proof/meta\x07");
        assert_eq!(app.tab_titles(wid)[0], "vim src/main.rs", "baseline: OSC");

        // Clone the ctx handle out so the pool borrow doesn't pin `app`
        // (`tab_titles` needs `&mut self` for its keep-stale cache).
        let ctx = app.pool.get(0).expect("session 0").ctx.clone();
        ctx.meta.lock().unwrap().user_title = Some("build agent".to_string());
        assert_eq!(
            app.tab_titles(wid)[0],
            "build agent",
            "the user title outranks the live OSC title"
        );
        // An EMPTY user title is treated as unset (never a blank tab label).
        ctx.meta.lock().unwrap().user_title = Some(String::new());
        assert_eq!(app.tab_titles(wid)[0], "vim src/main.rs");
        // Clearing falls back to the live title, then (title cleared too) the cwd.
        ctx.meta.lock().unwrap().user_title = None;
        assert_eq!(app.tab_titles(wid)[0], "vim src/main.rs");
        feed_session0(&app, b"\x1b]2;\x07");
        assert_eq!(
            app.tab_titles(wid)[0],
            "/aterm-proof/meta",
            "beneath the user rung the old chain survives verbatim"
        );
    }

    /// SESSION-METADATA stage 1 — a user-title change moves the tab-strip
    /// fingerprint (the fingerprint hashes the exact `tab_titles` bytes), so the
    /// `Wake::MetaChanged` refresh repaints the strip through the ordinary
    /// RepaintKey path — the meta twin of the cwd fingerprint proof above.
    #[test]
    fn user_title_change_moves_the_tab_strip_fingerprint() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.tab_strip_rows = 1; // enable the strip (fingerprint pinned 0 when disabled)
        let fingerprint = |app: &mut App| {
            let titles = app.tab_titles(wid);
            let metadata = app.tab_strip_metadata(wid);
            app.tab_strip_fingerprint_from_parts(wid, &titles, &metadata, 0)
        };
        let before = fingerprint(&mut app);
        app.pool
            .get(0)
            .expect("session 0")
            .ctx
            .meta
            .lock()
            .unwrap()
            .user_title = Some("build agent".to_string());
        let with_user_title = fingerprint(&mut app);
        assert_ne!(
            before, with_user_title,
            "a user-title change must move the strip fingerprint"
        );
        // And unsetting moves it back to the original label bytes.
        app.pool
            .get(0)
            .expect("session 0")
            .ctx
            .meta
            .lock()
            .unwrap()
            .user_title = None;
        assert_eq!(fingerprint(&mut app), before, "clearing restores the label");
    }

    /// SESSION-METADATA stage 1 — a restore leaf carrying user metadata re-seeds
    /// the respawned session's ctx, and a leaf without any is a no-op (the meta
    /// mutex is untouched on the common path).
    #[test]
    fn restore_leaf_reseeds_user_metadata_onto_the_respawned_session() {
        let leaf = restore::TerminalLeafRestore {
            cwd: Some("/tmp".to_string()),
            title: "shell".to_string(),
            profile: None,
            local_id: None,
            user_title: Some("release builder".to_string()),
            description: Some("cuts the v0.56 release".to_string()),
            icon: Some("🚀".to_string()),
            role: Some("operator".to_string()),
            attention: Some("⚠ waiting on approval".to_string()),
        };
        let session = crate::stub_session(9);
        App::seed_restored_user_meta(&session, &leaf);
        let meta = session.ctx.meta.lock().unwrap().clone();
        assert_eq!(meta.user_title.as_deref(), Some("release builder"));
        assert_eq!(meta.description.as_deref(), Some("cuts the v0.56 release"));
        assert_eq!(meta.icon.as_deref(), Some("🚀"));
        assert_eq!(meta.role.as_deref(), Some("operator"));
        assert_eq!(meta.attention.as_deref(), Some("⚠ waiting on approval"));

        // A metadata-less leaf leaves an already-seeded ctx untouched (no wipe).
        let bare = restore::TerminalLeafRestore {
            cwd: None,
            title: String::new(),
            profile: None,
            local_id: None,
            user_title: None,
            description: None,
            icon: None,
            role: None,
            attention: None,
        };
        App::seed_restored_user_meta(&session, &bare);
        assert_eq!(
            session.ctx.meta.lock().unwrap().user_title.as_deref(),
            Some("release builder"),
            "a leaf with no metadata never clears live metadata"
        );
    }

    #[test]
    fn restored_and_legacy_raw_metadata_is_sanitized_before_chrome() {
        let family = "👨‍👩‍👧‍👦";
        let leaf = restore::TerminalLeafRestore {
            cwd: None,
            title: "shell".to_string(),
            profile: None,
            local_id: None,
            user_title: Some("  release\n\u{202e}builder  ".to_string()),
            description: Some(format!("cuts\u{2029}the release{}", "x".repeat(1100))),
            icon: Some(format!("\u{2066}{family}\u{2069}")),
            role: Some("operator\u{200b}".to_string()),
            attention: Some("  needs\u{2028}human  ".to_string()),
        };
        let session = crate::stub_session(9);
        App::seed_restored_user_meta(&session, &leaf);
        let meta = session.ctx.meta.lock().unwrap().clone();
        assert_eq!(meta.user_title.as_deref(), Some("releasebuilder"));
        assert_eq!(
            meta.icon.as_deref(),
            Some(family),
            "ZWJ cluster stays intact"
        );
        assert_eq!(
            meta.role.as_deref(),
            Some("operator"),
            "invisible formatting is stripped from a restored role"
        );
        assert_eq!(
            meta.attention.as_deref(),
            Some("needshuman"),
            "line separators are stripped from a restored attention message"
        );
        assert!(
            meta.description.as_ref().is_some_and(|value| {
                value.starts_with("cutsthe release")
                    && value.len() <= crate::session_timeline::META_DESCRIPTION_MAX
                    && !crate::session_timeline::metadata_has_forbidden_formatting(value)
            }),
            "restore fields are bounded and single-line: {:?}",
            meta.description
        );

        // Defense at the presentation seam also covers an old/internal caller
        // that assigned the public compatibility fields directly.
        let mut app = App::headless_for_test();
        let ctx = app.pool.get(0).expect("session 0").ctx.clone();
        {
            let mut raw = ctx.meta.lock().unwrap();
            raw.user_title = Some("raw\u{202e}title".to_string());
            raw.description = Some("first\nsecond".to_string());
            raw.icon = Some(format!("\u{2066}{family}\u{2069}"));
        }
        assert_eq!(
            app.tab_titles(WindowId(0))[0],
            "rawtitle · firstsecond",
            "both title-format fields cross the same sanitizer"
        );
        app.refresh_window_tabs(WindowId(0));
        let tooltip = app.windows[&WindowId(0)].tab_set.tabs()[0]
            .presentation
            .tooltip
            .as_deref()
            .expect("composed terminal tooltip");
        assert!(
            tooltip
                .lines()
                .all(|line| { !crate::session_timeline::metadata_has_forbidden_formatting(line) })
        );
        assert!(tooltip.contains("description: firstsecond"), "{tooltip:?}");
        assert!(tooltip.starts_with(family), "{tooltip:?}");
    }

    /// The last-window close DECIDES to exit and stashes the manifest BEFORE the
    /// teardown drains `windows`/`pool` — the seam the post-loop writer depends on
    /// (after `close_window_logical` there is nothing left to capture).
    #[test]
    fn last_window_close_stashes_the_quit_capture() {
        let mut app = App::headless_for_test();
        assert!(app.quit_capture.is_none());
        assert!(matches!(
            app.close_window_logical(WindowId(0)),
            CloseOutcome::Exit
        ));
        assert!(app.windows.is_empty(), "teardown drained the windows");
        let cap = app
            .quit_capture
            .take()
            .expect("stashed at close-decision time");
        assert_eq!(
            cap.windows.len(),
            1,
            "captured the window teardown destroyed"
        );
        assert_eq!(cap.windows[0].tabs.len(), 1);
        assert!(!cap.is_empty());
    }

    #[test]
    fn capture_preserves_mixed_order_and_only_stable_native_descriptors() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-mixed-restore-capture-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("notes.md");
        std::fs::write(&path, "# Durable title\nsecret draft sentinel\n").unwrap();

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));
        app.open_document_tab(crate::native_app::AppKind::Editor, &file_uri(&path))
            .unwrap();
        let (editor, _) = app.active_native_view(wid).expect("Editor active");
        let document = app
            .native_runtime
            .document_id(editor)
            .expect("Editor document");
        let canonical_uri = app
            .document_store
            .canonical_uri(document)
            .expect("canonical document URI")
            .to_string();
        // [terminal, Settings, Editor] -> [Settings, terminal, Editor]. Relative
        // terminal/native projection order remains monotonic while canonical chrome mixes.
        app.move_tab(wid, 1, 0);
        let manifest = app.capture_restore_manifest();
        let window = &manifest.windows[0];
        assert_eq!(
            window.tab_order,
            vec![
                restore::TabOrderEntry::Native { index: 0 },
                restore::TabOrderEntry::Terminal { index: 0 },
                restore::TabOrderEntry::Native { index: 1 },
            ]
        );
        assert_eq!(window.active_item, Some(2));
        assert_eq!(
            window.native_tabs,
            vec![
                restore::NativeTabRestore::Settings {
                    route: "/about".to_string(),
                },
                restore::NativeTabRestore::Editor { uri: canonical_uri },
            ]
        );
        let encoded = manifest.to_toml().unwrap();
        assert!(
            !encoded.contains("secret draft sentinel"),
            "document bytes must never enter session restore"
        );
        assert!(restore::RestoreManifest::from_toml(&encoded).is_some());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn heterogeneous_active_tab_round_trips_by_recursive_index() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let _ = app.active_native_view(wid).expect("Settings active");
        let _ = app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
        assert_eq!(app.windows[&wid].tab_set.active_index(), Some(1));

        let captured = app.capture_restore_manifest();
        let window = &captured.windows[0];
        assert_eq!(
            window.tab_order,
            vec![restore::TabOrderEntry::Terminal { index: 0 }],
            "the legacy mirror deliberately cannot encode the heterogeneous tab"
        );
        assert_eq!(window.restored_tabs.len(), 2);
        assert_eq!(
            window.active_item,
            Some(1),
            "active_item indexes the authoritative recursive tabs, not the compressed legacy mirror"
        );

        let decoded = restore::RestoreManifest::from_toml(
            &captured.to_toml().expect("serialize recursive restore"),
        )
        .expect("decode recursive restore");
        assert_eq!(decoded.windows[0].active_item, Some(1));

        let mut reopened = App::headless_for_test();
        reopened.restore_into_window(wid, decoded.windows.into_iter().next().unwrap());
        let state = &reopened.windows[&wid];
        assert_eq!(state.tab_set.active_index(), Some(1));
        let active = state.tab_set.active().expect("restored active tab");
        assert_eq!(active.root.len(), 2);
        assert!(active.root.leaves().into_iter().any(|view| matches!(
            reopened.view_store.get(view),
            Some(crate::tab_model::View::Native(_))
        )));
        assert!(active.root.leaves().into_iter().any(|view| matches!(
            reopened.view_store.get(view),
            Some(crate::tab_model::View::Terminal(_))
        )));
        assert!(reopened.structural_invariants_ok());
    }

    #[test]
    fn native_only_restore_retires_bootstrap_without_fabricating_a_session() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let layout = restore::WindowLayout {
            rows: 24,
            cols: 80,
            active_tab: 0,
            outer_x: None,
            outer_y: None,
            maximized: None,
            tabs: Vec::new(),
            native_tabs: vec![restore::NativeTabRestore::Settings {
                route: "/updates".to_string(),
            }],
            tab_order: vec![restore::TabOrderEntry::Native { index: 0 }],
            active_item: Some(0),
            restored_tabs: Vec::new(),
        };
        app.restore_into_window(wid, layout);
        let ws = &app.windows[&wid];
        assert_eq!(ws.tab_set.len(), 1);
        assert!(ws.layouts.is_empty());
        assert_eq!(ws.tabs, crate::TabIndex::new(0, 0));
        assert!(ws.active_terminal.is_none());
        assert!(ws.front_terminal().is_none());
        assert_eq!(app.focused_session_id(wid), None);
        assert!(app.pool.get(0).is_none(), "bootstrap session retired");
        assert!(app.store.read().unwrap().snapshot().is_empty());
        let (_, view) = app.active_native_view(wid).expect("native Settings");
        assert_eq!(
            ws.window_focus,
            crate::front_content::WindowFocus::Content(view),
            "the first restored native view owns window focus"
        );
        assert!(matches!(
            app.native_runtime.view_state(view),
            Some(crate::native_app::AppViewState::Settings(state))
                if state.route == crate::native_settings::SettingsRoute::SoftwareUpdate
        ));
        assert!(app.structural_invariants_ok());
    }

    #[test]
    fn failed_native_restore_staging_republishes_the_surviving_terminal() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let original_view = app.windows[&wid]
            .tab_set
            .active()
            .expect("bootstrap tab")
            .focus;
        assert_eq!(app.focused_session_id(wid), Some(0));
        assert!(app.active_handle.lock().unwrap().is_some());

        app.stage_settings_restore_view(wid, crate::native_settings::SettingsRoute::About)
            .expect("native restore staging view");
        assert_eq!(app.focused_session_id(wid), None);
        assert!(app.active_handle.lock().unwrap().is_none());

        let (staged_view, _, _) = app
            .detach_restore_staging_tab(wid)
            .expect("detach staged leaf");
        assert_eq!(
            app.windows[&wid].tab_set.active().unwrap().focus,
            original_view
        );
        assert_eq!(
            app.windows[&wid].front_terminal().map(|term| term.session),
            Some(0)
        );
        assert_eq!(app.focused_session_id(wid), Some(0));
        assert!(
            app.active_handle.lock().unwrap().is_some(),
            "detaching a candidate immediately republishes the live front capability"
        );
        assert_eq!(
            app.windows[&wid].window_focus,
            crate::front_content::WindowFocus::Content(original_view)
        );

        // Negative restore outcome: the unmounted candidate is discarded after
        // validation fails. The already-republished front must remain unchanged.
        assert!(app.remove_view_link(staged_view).is_some());
        assert!(app.view_store.get(staged_view).is_none());
        assert_eq!(app.focused_session_id(wid), Some(0));
        assert!(app.active_handle.lock().unwrap().is_some());
        assert!(app.structural_invariants_ok());
    }

    #[test]
    fn mixed_restore_rebuilds_canonical_order_active_item_and_derived_titles() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-mixed-restore-apply-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("restored-notes.md");
        std::fs::write(&path, "# Restored\n").unwrap();

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let layout = restore::WindowLayout {
            rows: 24,
            cols: 80,
            active_tab: 0,
            outer_x: None,
            outer_y: None,
            maximized: None,
            tabs: vec![restore::PaneLayout::leaf(
                Some("/tmp".to_string()),
                "shell title is not native identity".to_string(),
                true,
            )],
            native_tabs: vec![
                restore::NativeTabRestore::Settings {
                    route: "/about".to_string(),
                },
                restore::NativeTabRestore::Editor {
                    uri: file_uri(&path),
                },
            ],
            tab_order: vec![
                restore::TabOrderEntry::Native { index: 0 },
                restore::TabOrderEntry::Terminal { index: 0 },
                restore::TabOrderEntry::Native { index: 1 },
            ],
            active_item: Some(2),
            restored_tabs: Vec::new(),
        };
        app.restore_into_window(wid, layout);

        let ws = &app.windows[&wid];
        let kinds = ws
            .tab_set
            .tabs()
            .iter()
            .map(|tab| match app.view_store.get(tab.focus) {
                Some(crate::tab_model::View::Terminal(_)) => "terminal",
                Some(crate::tab_model::View::Native(native)) => app
                    .native_runtime
                    .app(native.instance)
                    .expect("live restored native instance")
                    .kind()
                    .as_str(),
                None => "missing",
            })
            .collect::<Vec<_>>();
        assert_eq!(kinds, vec!["settings", "terminal", "editor"]);
        assert_eq!(ws.tab_set.active_index(), Some(2));
        assert_eq!(app.tab_titles(wid)[0], "Settings");
        assert_eq!(app.tab_titles(wid)[2], "restored-notes.md");
        let (_, active_view) = app.active_native_view(wid).expect("restored editor active");
        assert_eq!(
            app.native_runtime
                .view_state(active_view)
                .map(|state| state.kind()),
            Some(crate::native_app::AppKind::Editor)
        );
        assert!(app.structural_invariants_ok());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn native_only_restore_never_discards_a_seamlessly_adopted_bootstrap() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.bootstrap_session_adopted = true;
        let original_terminal = app.windows[&wid].tab_set.active_id().unwrap();
        let layout = restore::WindowLayout {
            rows: 24,
            cols: 80,
            active_tab: 0,
            outer_x: None,
            outer_y: None,
            maximized: None,
            tabs: Vec::new(),
            native_tabs: vec![restore::NativeTabRestore::Settings {
                route: "/updates".to_string(),
            }],
            tab_order: vec![restore::TabOrderEntry::Native { index: 0 }],
            active_item: Some(0),
            restored_tabs: Vec::new(),
        };
        app.restore_into_window(wid, layout);

        let ws = &app.windows[&wid];
        assert_eq!(
            ws.tab_set.len(),
            2,
            "live handed-off shell remains reachable"
        );
        assert_eq!(
            ws.tab_set.active_index(),
            Some(0),
            "persisted native tab stays active"
        );
        assert!(ws.tab_set.get(original_terminal).is_some());
        assert_eq!(ws.layouts.len(), 1);
        assert!(
            app.pool.get(0).is_some(),
            "adopted session is never torn down"
        );
        assert!(app.structural_invariants_ok());
    }

    #[test]
    fn missing_native_document_skips_without_placeholder_or_id_reuse() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let old_active = app.windows[&wid].tab_set.active_id();
        let layout = restore::WindowLayout {
            rows: 24,
            cols: 80,
            active_tab: 0,
            outer_x: None,
            outer_y: None,
            maximized: None,
            tabs: Vec::new(),
            native_tabs: vec![restore::NativeTabRestore::Markdown {
                uri: "file:///definitely/missing/aterm-restore-document.md".to_string(),
            }],
            tab_order: vec![restore::TabOrderEntry::Native { index: 0 }],
            active_item: Some(0),
            restored_tabs: Vec::new(),
        };
        app.restore_into_window(wid, layout);
        let ws = &app.windows[&wid];
        assert_eq!(ws.tab_set.len(), 1, "safe bootstrap remains usable");
        assert_eq!(ws.tab_set.active_id(), old_active);
        assert_eq!(ws.layouts.len(), 1);
        assert!(
            app.native_runtime
                .instance_by_kind(crate::native_app::AppKind::Markdown)
                .is_none()
        );
        assert!(app.structural_invariants_ok());
    }

    #[test]
    fn recursive_mixed_restore_keeps_topology_and_substitutes_only_failed_leaf() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let restored = restore::RestoredTab {
            root: restore::RestoredSplitTree::Split {
                axis: restore::SplitKind::Horizontal,
                ratio: 0.72,
                first: Box::new(restore::RestoredSplitTree::leaf(
                    restore::RestoredView::Terminal(restore::TerminalLeafRestore {
                        cwd: Some("/tmp".to_string()),
                        title: "Build shell".to_string(),
                        profile: None,
                        local_id: None,
                        user_title: None,
                        description: None,
                        icon: None,
                        role: None,
                        attention: None,
                    }),
                )),
                second: Box::new(restore::RestoredSplitTree::leaf(
                    restore::RestoredView::Native(restore::NativeLeafRestore::document(
                        "markdown",
                        "file:///definitely/missing/recursive-restore.md".to_string(),
                    )),
                )),
            },
            focused_path: vec![restore::RestoreBranch::Second],
            zoomed: true,
        };
        app.restore_into_window(
            wid,
            restore::WindowLayout {
                rows: 24,
                cols: 80,
                active_tab: 0,
                outer_x: None,
                outer_y: None,
                maximized: None,
                tabs: Vec::new(),
                native_tabs: Vec::new(),
                tab_order: Vec::new(),
                active_item: Some(0),
                restored_tabs: vec![restored],
            },
        );

        let tab = app.windows[&wid].tab_set.active().expect("restored tab");
        let crate::tab_model::SplitTree::Split {
            axis,
            ratio,
            first,
            second,
        } = &tab.root
        else {
            panic!("recursive split topology was flattened");
        };
        assert_eq!(*axis, crate::tab_model::SplitAxis::Horizontal);
        assert!((*ratio - 0.72).abs() < f32::EPSILON);
        let crate::tab_model::SplitTree::Leaf(terminal_view) = &**first else {
            panic!("healthy terminal sibling moved");
        };
        let crate::tab_model::SplitTree::Leaf(recovery_view) = &**second else {
            panic!("failed document position moved");
        };
        assert!(matches!(
            app.view_store.get(*terminal_view),
            Some(crate::tab_model::View::Terminal(_))
        ));
        let crate::tab_model::View::Native(recovery) =
            app.view_store.get(*recovery_view).copied().unwrap()
        else {
            panic!("missing document did not become a renderable native placeholder");
        };
        assert_eq!(
            app.native_runtime
                .app(recovery.instance)
                .map(|app| app.kind()),
            Some(crate::native_app::AppKind::Recovery)
        );
        assert_eq!(tab.focus, *recovery_view);
        assert!(tab.zoomed);
        assert!(tab.presentation.indicators.attention);
        assert_eq!(
            app.windows[&wid].layouts.len(),
            0,
            "mixed trees are canonical"
        );
        assert!(
            app.pool.get(0).is_some(),
            "healthy bootstrap shell was grafted"
        );
        assert!(app.structural_invariants_ok());
    }

    #[test]
    fn recursive_native_only_restore_retires_the_cold_bootstrap() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.restore_into_window(
            wid,
            restore::WindowLayout {
                rows: 24,
                cols: 80,
                active_tab: 0,
                outer_x: None,
                outer_y: None,
                maximized: None,
                tabs: Vec::new(),
                native_tabs: Vec::new(),
                tab_order: Vec::new(),
                active_item: Some(0),
                restored_tabs: vec![restore::RestoredTab {
                    root: restore::RestoredSplitTree::leaf(restore::RestoredView::Native(
                        restore::NativeLeafRestore::settings("/about".to_string()),
                    )),
                    focused_path: Vec::new(),
                    zoomed: false,
                }],
            },
        );
        let window = &app.windows[&wid];
        assert_eq!(window.tab_set.len(), 1);
        assert!(window.layouts.is_empty());
        assert!(app.pool.get(0).is_none());
        assert!(app.store.read().unwrap().snapshot().is_empty());
        let (_, view) = app.active_native_view(wid).expect("Settings restored");
        assert_eq!(
            window.window_focus,
            crate::front_content::WindowFocus::Content(view),
            "recursive native-only restore mounts its selected content"
        );
        assert!(matches!(
            app.native_runtime.view_state(view),
            Some(crate::native_app::AppViewState::Settings(state))
                if state.route == crate::native_settings::SettingsRoute::About
        ));
        assert!(app.structural_invariants_ok());
    }

    #[test]
    fn manual_editor_identity_survives_capture_restore_and_blocks_malformed_save() {
        const CHILD: &str = "ATERM_CONFIG_RESTORE_IDENTITY_CHILD";
        const ROOT: &str = "ATERM_CONFIG_RESTORE_IDENTITY_ROOT";
        const EXACT: &str = "app_restore::tests::manual_editor_identity_survives_capture_restore_and_blocks_malformed_save";
        if std::env::var_os(CHILD).is_none() {
            let root = std::env::temp_dir().join(format!(
                "aterm-config-restore-identity-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", EXACT, "--nocapture"])
                .env(CHILD, "1")
                .env(ROOT, &root)
                .env("XDG_CONFIG_HOME", &root)
                .env("RUST_TEST_THREADS", "1")
                .status()
                .expect("launch isolated config-restore test");
            let _ = std::fs::remove_dir_all(root);
            assert!(status.success());
            return;
        }

        let root = std::path::PathBuf::from(std::env::var_os(ROOT).unwrap());
        let config = root.join("aterm/aterm.toml");
        let mut source = App::headless_for_test();
        assert!(source.open_settings_tab(crate::native_settings::SettingsRoute::Manual));
        assert!(config.is_file(), "Manual resolves the isolated aterm.toml");
        let (_, source_view) = source.active_native_view(WindowId(0)).unwrap();
        let descriptor = match source.view_restore_descriptor(source_view).unwrap() {
            restore::RestoredView::Native(descriptor) => descriptor,
            other => panic!("expected native config Editor, got {other:?}"),
        };
        assert_eq!(descriptor.restore_tag, "editor");
        assert!(descriptor.config_editor);

        let mut retry = App::headless_for_test();
        let retried = retry.execute_recovery_request(
            WindowId(0),
            crate::native_app::RecoveryRequest::Retry(
                crate::native_app::RecoveryCapability::Document {
                    kind: crate::native_app::AppKind::Editor,
                    uri: descriptor.uri.clone().unwrap(),
                    config_editor: true,
                },
            ),
        );
        assert!(matches!(
            retried,
            crate::native_app::RecoveryOutcome::Opened { .. }
        ));
        let (retry_instance, _) = retry.active_native_view(WindowId(0)).unwrap();
        assert!(
            retry.native_runtime.config_editor_enabled(retry_instance),
            "Recovery Retry preserves Manual's privileged editor reducer"
        );

        let mut restored = App::headless_for_test();
        restored.restore_into_window(
            WindowId(0),
            restore::WindowLayout {
                rows: 24,
                cols: 80,
                active_tab: 0,
                outer_x: None,
                outer_y: None,
                maximized: None,
                tabs: Vec::new(),
                native_tabs: Vec::new(),
                tab_order: Vec::new(),
                active_item: Some(0),
                restored_tabs: vec![restore::RestoredTab {
                    root: restore::RestoredSplitTree::leaf(restore::RestoredView::Native(
                        descriptor,
                    )),
                    focused_path: Vec::new(),
                    zoomed: false,
                }],
            },
        );
        let (instance, view) = restored
            .active_native_view(WindowId(0))
            .expect("config Editor restored");
        assert!(restored.native_runtime.config_editor_enabled(instance));
        let document = restored.native_runtime.document_id(instance).unwrap();
        restored
            .dispatch_native_event(
                WindowId(0),
                crate::native_app::AppEvent::TextInput(crate::native_app::TextInputEvent::Commit(
                    "font_px = \"not a number\"\n".to_string(),
                )),
            )
            .unwrap();
        assert!(
            restored
                .native_runtime
                .config_editor_save_error(document)
                .is_some(),
            "restored Manual retains the config save gate"
        );
        let save = restored
            .native_runtime
            .commands(instance, view)
            .unwrap()
            .into_iter()
            .find(|command| command.id.as_str() == "editor/save")
            .unwrap();
        assert!(!save.enabled, "malformed restored Manual cannot Save");
    }

    #[test]
    fn legacy_manual_settings_restore_normalizes_with_or_without_singleton() {
        let wid = WindowId(0);
        let descriptor = restore::NativeLeafRestore::settings("/manual".to_string());
        for preinstall_singleton in [false, true] {
            let mut app = App::headless_for_test();
            if preinstall_singleton {
                assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
            }
            let (view, _, _) = app.restore_native_leaf(wid, &descriptor).unwrap();
            assert!(matches!(
                app.native_runtime.view_state(view),
                Some(crate::native_app::AppViewState::Settings(state))
                    if state.route == crate::native_settings::SettingsRoute::Home
            ));
            app.remove_view_link(view);
        }
    }

    #[test]
    fn recursive_editor_restore_clamps_utf8_view_offsets() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-recursive-editor-restore-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("utf8.md");
        std::fs::write(&path, "éx").unwrap();
        let mut descriptor = restore::NativeLeafRestore::document("editor", file_uri(&path));
        descriptor.editor_selections = vec![restore::RestoreSelection {
            anchor: 1,
            head: usize::MAX,
        }];
        descriptor.viewport_anchor = 1;

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.restore_into_window(
            wid,
            restore::WindowLayout {
                rows: 24,
                cols: 80,
                active_tab: 0,
                outer_x: None,
                outer_y: None,
                maximized: None,
                tabs: Vec::new(),
                native_tabs: Vec::new(),
                tab_order: Vec::new(),
                active_item: Some(0),
                restored_tabs: vec![restore::RestoredTab {
                    root: restore::RestoredSplitTree::leaf(restore::RestoredView::Native(
                        descriptor,
                    )),
                    focused_path: Vec::new(),
                    zoomed: false,
                }],
            },
        );
        let (_, view) = app.active_native_view(wid).expect("Editor restored");
        let Some(crate::native_app::AppViewState::Editor(editor)) =
            app.native_runtime.view_state(view)
        else {
            panic!("restored view is not Editor");
        };
        let buffer = editor.buffer.as_ref().expect("Editor buffer attached");
        assert_eq!(buffer.selections[0].anchor, 0, "mid-codepoint clamps down");
        assert_eq!(
            buffer.selections[0].head, 3,
            "oversized offset clamps to EOF"
        );
        assert_eq!(buffer.viewport_anchor, 0);
        let _ = std::fs::remove_dir_all(dir);
    }
}
