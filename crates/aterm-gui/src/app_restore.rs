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
                        // The spawn-time identity, so the respawn is under it.
                        identity: self
                            .pool
                            .get(terminal.session)
                            .and_then(|s| s.identity.clone()),
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
        let mut handed_off_leaves = Vec::new();
        if let Some(manifest) = self.pending_restore.take() {
            // The leaves naming handed-off shells outlive the manifest the rebuild
            // consumes, so a shell the rebuild leaves over still gets its identity.
            // A cold restore hands nothing off and keeps none.
            if !self.seamless_adopt.is_empty() {
                handed_off_leaves = manifest.handed_off_leaves();
            }
            self.apply_restore_manifest(el, manifest);
        }
        // SEAMLESS orphan safety net: any handed-off live shell the layout did NOT place
        // (a stale/inconsistent/absent manifest, or a leaf that failed to rebuild) is
        // adopted anyway as a fresh tab in the front window. A live shell is NEVER dropped
        // just because its exact pane could not be reconstructed. A no-op on a cold
        // restore (`seamless_adopt` is empty).
        self.adopt_orphan_shells_as_tabs(&handed_off_leaves);
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
                .find_map(|tab| tab.root.first_terminal_leaf());
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
            // The bootstrap starts in the directory of the pane it will fill — the
            // leaf `bootstrap_local_id` names, by the same pick.
            let cwd0: Option<String> = wl.bootstrap_cwd().map(String::from);
            // SEAMLESS: adopt this window's first-leaf shell as its bootstrap session
            // (matched by id), so the reopened window comes up with its LIVE shell rather
            // than a fresh one. Consumed (removed) so it is never adopted twice; a cold
            // restore's `seamless_adopt` is empty, so this is always `None` there. The
            // same pick `main_entry` makes for window 0 (`take_session0_shell`), so the
            // graft's leaf names this shell. An accepted handoff layout names each shell
            // once, so it is still waiting here; were it taken, the window's bootstrap
            // would be a fresh stand-in, which `carry_restored_identity` gives no identity.
            let adopt = take_handed_off_shell(&mut self.seamless_adopt, wl.bootstrap_local_id());
            // IDENTITY (session identities; review 2026-09-17): a bootstrap that
            // is FORKED here runs under the identity the pane it fills names —
            // the same pick as its cwd — while that identity still exists
            // (`restorable`: create = false, one stderr line when it is gone).
            // The graft refuses a bootstrap that does not wear what its pane
            // names, so a bootstrap forked without this came back as the
            // human's login under a `worker` tab. An adopted shell's env is
            // what it was; its label rides `Adopted::identity`.
            let identity0: Option<String> = if adopt.is_none() {
                wl.bootstrap_identity()
                    .and_then(crate::agent_identity::restorable)
            } else {
                None
            };
            let outer = (wl.outer_x, wl.outer_y);
            let maximized = wl.maximized;
            let Some(wid) = self.create_window_internal_connected(
                el,
                cwd0.as_deref(),
                adopt,
                None,
                None,
                identity0.as_deref(),
            ) else {
                // A window create fails on spawn or GPU-surface trouble — both likely to
                // repeat. Keep what restored rather than looping on failures. RESIDUAL
                // RISK (rare): if this window carried an adopted shell as its first leaf,
                // that shell is lost with the failed window (its OTHER panes' shells stay
                // in `seamless_adopt` and the orphan net below re-homes them as tabs).
                // Fully protecting the first-leaf shell here needs recovering it from the
                // half-built window — the "adopt into the pool first, then build windows"
                // rework — deferred; window-surface failure on an already-running app is
                // exceptional.
                crate::logging::stderr_line!(
                    "aterm-gui: session restore: could not create a window; stopping here"
                );
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
            // Adopted holders — their raw fds close with the process, ending the shells.
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
                    self.pool.insert(s);
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
            // the id it records from the `Adopted` handle, and whether the shell's
            // PATH is frozen (2026-09-16) — what `register_session` marks on the
            // registry, so a test can pin that adoption reaches the count.
            let mut session = crate::stub_session(id);
            session.handoff_local_id = Some(adopted.local_id);
            session.frozen_path = adopted.frozen_path;
            session.identity = adopted.identity;
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
            None, // adopted: the shell keeps its env; the label rides `Adopted::identity`
            Some(adopted),
        )
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
    /// in two leaves, which is why only the retired-id path asserts this.
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
                            let fresh = !seen.contains(&session);
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
        ids: LeafIds,
    ) -> Result<crate::tab_model::ViewId, String> {
        // THE GRAFT CARRIES THE LEAF'S USER IDENTITY, not just its position. The
        // reusable terminal is the window's already-running BOOTSTRAP session —
        // on a seamless update, the adopted shell itself (session 0 is adopted
        // in `main_entry`, every further window's in `create_window_internal`, each
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
        //
        // THE AGENT IDENTITY IS THE SHELL'S, NOT THE LEAF'S. A shell's env is
        // set at its fork and cannot be re-injected, so on a COLD restore the
        // bootstrap fills this pane only if it already runs under the identity
        // the leaf names (`main_entry` and `apply_restore_manifest` fork it
        // under `bootstrap_identity`, by the same pick as its cwd). Measured
        // 2026-09-17 (review): grafting regardless put a `worker` tab's first
        // leaf on the human's login, labeled `identity=-`. A bootstrap that
        // wears something else stays in `reusable_terminal` for a later pane
        // that names what it wears, or is retired unconsumed
        // (`restore_recursive_into_window`); this pane forks its own shell
        // below. A handoff's bootstrap is the adopted shell its leaf names
        // (or a stand-in forked under that leaf's identity), and its label is
        // the shell's own — grafted as before.
        if let Some((view, session)) = *reusable_terminal {
            let from_handoff = ids == LeafIds::Retired && self.handoff_successor;
            if from_handoff || self.bootstrap_wears_leaf_identity(session, terminal) {
                *reusable_terminal = None;
                self.carry_restored_identity(terminal, FillingShell::Registered(session), ids);
                return Ok(view);
            }
            crate::logging::stderr_line!(
                "aterm-gui: session restore: the window's bootstrap shell runs under identity \
                 {} and the pane names {}; the pane gets its own shell, and the bootstrap \
                 fills a later pane that names its identity or is retired",
                self.pool
                    .get(session)
                    .and_then(|live| live.identity.as_deref())
                    .unwrap_or("-"),
                terminal.identity.as_deref().unwrap_or("-")
            );
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
            // the same match, the id it records from the `Adopted` handle, and
            // whether the shell's PATH is frozen (2026-09-16).
            if let Some(shell) = take_handed_off_shell(&mut self.seamless_adopt, terminal.local_id)
            {
                session.handoff_local_id = Some(shell.local_id);
                session.frozen_path = shell.frozen_path;
                session.identity = shell.identity;
            } else {
                // A cold restore's stand-in: the leaf's identity, create=false.
                session.identity = terminal
                    .identity
                    .as_deref()
                    .and_then(crate::agent_identity::restorable);
            }
            self.carry_restored_identity(terminal, FillingShell::Unregistered(&session), ids);
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
        let adopt = take_handed_off_shell(&mut self.seamless_adopt, terminal.local_id);
        // IDENTITY on a COLD restore (session identities, 2026-09-17): the leaf
        // names the identity its shell ran under; respawn under it only if it
        // still exists (`create = false` — restore never creates one; a
        // forgotten identity is a default shell and one stderr line). An
        // adopted shell needs none of this: its env is what it was.
        let identity = if adopt.is_none() {
            terminal
                .identity
                .as_deref()
                .and_then(crate::agent_identity::restorable)
        } else {
            None
        };
        let session = spawn_session(
            id,
            wid,
            rows,
            cols,
            // CELL-PX-1: the host window's real cell box for the newborn engine.
            self.spawn_cell_px(wid),
            &self.session_factory,
            &proxy,
            terminal.cwd.as_deref(),
            None, // not a connected controller spawn
            identity.as_deref(),
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
        self.pool.insert(session);
        Ok(view)
    }

    /// Whether the window's bootstrap session `session` runs under the agent
    /// identity restore leaf `leaf` names — what a COLD restore may graft. The
    /// leaf's name counts only while the identity still exists
    /// (`agent_identity::existing`, the silent read: `main_entry` already
    /// said so when it forked the bootstrap); a forgotten identity is a
    /// default shell on both sides, so they match.
    fn bootstrap_wears_leaf_identity(
        &self,
        session: u64,
        leaf: &restore::TerminalLeafRestore,
    ) -> bool {
        let wears = self
            .pool
            .get(session)
            .and_then(|live| live.identity.as_deref());
        let names = leaf
            .identity
            .as_deref()
            .and_then(crate::agent_identity::existing);
        wears == names.as_deref()
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

    /// Put `leaf`'s USER identity (its five `meta set` fields) on the shell
    /// the leaf NAMES — on a handoff, not necessarily the session `filling` its
    /// pane. A peer reads a session's identity before typing into it: the
    /// `role=` it checks, the `attention=` it answers, and `role=operator`,
    /// which moves the status item's operator election and its Stop
    /// confirm-suppression. So the identity must describe the process that
    /// runs under the sid it is read from.
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
        let filling_adopted_as = match filling {
            FillingShell::Registered(session) => self
                .pool
                .get(session)
                .and_then(|live| live.handoff_local_id),
            FillingShell::Unregistered(session) => session.handoff_local_id,
        };
        let from_handoff = ids == LeafIds::Retired && self.handoff_successor;
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
                    self.refresh_operator_status_item();
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
    fn graft_restored_user_meta(&self, session: u64, leaf: &restore::TerminalLeafRestore) -> bool {
        let Some(live) = self.pool.get(session) else {
            return false;
        };
        let ctx = live.ctx.clone();
        let moved = crate::session_timeline::restore_carried_meta(&ctx, &leaf.carried_user_meta());
        if moved && self.subscribers.any() {
            self.subscribers
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .notify(session);
        }
        moved
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
                    conn: None,
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
            let adopt = take_handed_off_shell(&mut self.seamless_adopt, leaf.local_id());
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
                None, // a legacy (RESTORE-1) layout carries no identity
                adopt,
            ) {
                Ok(s) => {
                    self.next_session_id += 1;
                    ids.push(id);
                    fresh.push(s);
                }
                Err(e) => {
                    crate::logging::stderr_line!("aterm-gui: session restore: spawn failed: {e}");
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
            identity: None,
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
            identity: None,
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
            identity: None,
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
        app.handoff_successor = true;
        adopt_as(&mut app, 0, 0);
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

    /// A handoff successor whose window 0 has no terminal leaf spawns session 0
    /// FRESH (`take_session0_shell`): no handed-off shell belongs in window 0.
    /// That bootstrap is as throwaway as a cold one, so a native-only rebuild
    /// retires it — on the canonical path a handoff layout takes and on the
    /// legacy one — instead of keeping it as an extra tab the layout never had.
    #[test]
    fn a_successors_fresh_bootstrap_is_retired_by_a_native_only_window_0() {
        let legacy = restore::WindowLayout {
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
        for (path, layout) in [
            ("canonical", window_of(vec![settings_leaf()])),
            ("legacy", legacy),
        ] {
            let mut app = App::headless_for_test();
            app.handoff_successor = true;
            app.restore_into_window(WindowId(0), layout);
            let window = &app.windows[&WindowId(0)];
            assert_eq!(window.tab_set.len(), 1, "{path}: only the Settings tab");
            assert!(window.layouts.is_empty(), "{path}");
            assert!(
                app.pool.get(0).is_none(),
                "{path}: the fresh bootstrap retired"
            );
            assert!(app.structural_invariants_ok(), "{path}");
        }
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
                        identity: None,
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

    /// One terminal leaf, exactly as `capture_restore_manifest` writes it.
    fn restored_terminal(local: u64, title: &str) -> restore::RestoredSplitTree {
        restore::RestoredSplitTree::leaf(restore::RestoredView::Terminal(
            restore::TerminalLeafRestore {
                cwd: Some("/tmp".to_string()),
                title: title.to_string(),
                profile: None,
                local_id: Some(local),
                user_title: Some(title.to_string()),
                description: None,
                icon: None,
                role: None,
                attention: None,
                identity: None,
            },
        ))
    }

    /// THE ALIASING REGRESSION. Reproduced from a cold launch on 2026-08-26: open
    /// three tabs, split the MIDDLE one, quit, relaunch. Capture walks the tabs in
    /// tree order, so the four shells persist as the DESCENDING run `0, 1, 3, 2`
    /// — and the old `restore_terminal_leaf` treated "the pool already holds id 2"
    /// as recognition, even though that id belonged to the fresh shell this same
    /// pass had just forked for the split. The third tab grafted itself onto the
    /// split's new pane: three tabs came back over three shells instead of four,
    /// CHARLIE's saved shell was never started, and typing in the third tab
    /// appeared in the second. The manifest's ids are a DEAD process's namespace;
    /// nothing here may consult them for a live handle.
    #[test]
    fn a_descending_leaf_run_restores_every_tab_onto_its_own_shell() {
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
                restored_tabs: vec![
                    restore::RestoredTab {
                        root: restored_terminal(0, "ALPHA"),
                        focused_path: Vec::new(),
                        zoomed: false,
                    },
                    restore::RestoredTab {
                        root: restore::RestoredSplitTree::Split {
                            axis: restore::SplitKind::Vertical,
                            ratio: 0.5,
                            first: Box::new(restored_terminal(1, "BRAVO")),
                            second: Box::new(restored_terminal(3, "DELTA")),
                        },
                        focused_path: vec![restore::RestoreBranch::Second],
                        zoomed: false,
                    },
                    restore::RestoredTab {
                        root: restored_terminal(2, "CHARLIE"),
                        focused_path: Vec::new(),
                        zoomed: false,
                    },
                ],
            },
        );

        let tabs = app.windows[&wid].tab_set.tabs().to_vec();
        assert_eq!(tabs.len(), 3, "every saved tab came back");
        let leaves = tabs
            .iter()
            .flat_map(|tab| tab.root.leaves())
            .collect::<Vec<_>>();
        assert_eq!(leaves.len(), 4, "the split tab kept both of its panes");
        let sessions = leaves
            .iter()
            .map(|view| {
                app.view_store
                    .get(*view)
                    .copied()
                    .and_then(crate::tab_model::View::terminal_session)
                    .expect("every restored leaf is a terminal")
            })
            .collect::<Vec<_>>();
        assert_eq!(
            sessions
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            sessions.len(),
            "two leaves share one shell: {sessions:?}"
        );
        for id in &sessions {
            assert!(app.pool.get(*id).is_some(), "restored session {id} is live");
            assert_eq!(
                app.pool.views(*id),
                Some(1),
                "restored session {id} backs exactly one leaf"
            );
        }
        assert!(app.restored_window_sessions_are_distinct(wid));

        // WRITE ISOLATION — the symptom a user actually meets. Bytes fed to one
        // leaf's engine, exactly as its PTY would, must land on that leaf's screen
        // and on NO other's.
        for (index, id) in sessions.iter().enumerate() {
            let session = app.pool.get(*id).expect("restored session");
            crate::term_lock(&session.term).process(format!("MARKER{index}").as_bytes());
        }
        for (index, id) in sessions.iter().enumerate() {
            let session = app.pool.get(*id).expect("restored session");
            let terminal = crate::term_lock(&session.term);
            let screen = (0..i32::from(terminal.rows()))
                .filter_map(|row| terminal.get_line_text(row, None))
                .collect::<String>();
            assert!(
                screen.contains(&format!("MARKER{index}")),
                "leaf {index} lost its own typing"
            );
            for other in (0..sessions.len()).filter(|other| *other != index) {
                assert!(
                    !screen.contains(&format!("MARKER{other}")),
                    "leaf {index} is showing leaf {other}'s typing — they share a PTY"
                );
            }
        }

        // Each respawned leaf also carries ITS OWN operator title, so the saved
        // work contexts stayed separate all the way to the tab strip. (Tab 0's
        // shell is the grafted bootstrap, which keeps the live session it already
        // had rather than being re-seeded.)
        let titles = sessions
            .iter()
            .skip(1)
            .map(|id| {
                app.pool
                    .get(*id)
                    .expect("restored session")
                    .ctx
                    .meta
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .user_title
                    .clone()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            titles,
            vec![
                Some("BRAVO".to_string()),
                Some("DELTA".to_string()),
                Some("CHARLIE".to_string()),
            ],
            "a respawned leaf must wear the metadata of the shell it stands for"
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

    /// The ADOPTION-SIDE RE-MINT (design §1.4#6): carried tokenless triples
    /// become live rows + a rebuilt `ConnectionRecord` through the one
    /// kind-bounded helper — while a lone push half, an op the vocabulary
    /// cannot spell, and a triple whose endpoint did not survive the swap are
    /// all dropped fail-soft (authority only ever shrinks). One-shot: the
    /// carry drains, so a second pass is a no-op.
    #[test]
    fn remint_carried_connections_remints_kinds_and_drops_the_unmintable() {
        let carry = |src: &str, dst: &str, op: &str| crate::session_store::ConnectionCarry {
            src: src.to_string(),
            dst: dst.to_string(),
            op: op.to_string(),
        };
        let mut app = App::headless_for_test();
        add_stub(&mut app, 1);
        let (a, b) = {
            let g = app.store.read().unwrap();
            (
                g.by_local(0).expect("session 0").sid.clone(),
                g.by_local(1).expect("session 1").sid.clone(),
            )
        };
        app.pending_conn_carry = vec![
            // A full `both` set a → b: re-mints as ONE Both connection.
            carry(a.as_str(), b.as_str(), "read-screen"),
            carry(a.as_str(), b.as_str(), "signal"),
            carry(a.as_str(), b.as_str(), "write-input"),
            // A lone push half b → a: NEVER rounded up to a full seat — drops.
            carry(b.as_str(), a.as_str(), "write-input"),
            // An op no connection can spell (§1.4#2): drops.
            carry(a.as_str(), b.as_str(), "config-write"),
            // An endpoint that did not survive the swap: drops.
            carry("s-gone", b.as_str(), "read-screen"),
        ];

        app.remint_carried_connections();

        assert!(
            app.pending_conn_carry.is_empty(),
            "the carry drains (one-shot)"
        );
        // b's table holds exactly the re-minted `both` rows, all src = a.
        let rows = {
            let ctx = {
                let g = app.store.read().unwrap();
                g.by_sid(&b).unwrap().ctx.clone()
            };
            let edges = ctx.edges.lock().unwrap();
            edges.edges()
        };
        assert_eq!(rows.len(), 3, "both = 3 ops, nothing widened: {rows:?}");
        assert!(rows.iter().all(|e| e.src == a && e.dst == b));
        // a's table stays EMPTY: the lone half was dropped, not widened.
        let a_rows = {
            let ctx = {
                let g = app.store.read().unwrap();
                g.by_sid(&a).unwrap().ctx.clone()
            };
            let edges = ctx.edges.lock().unwrap();
            edges.edges()
        };
        assert!(
            a_rows.is_empty(),
            "the lone push half must not mint: {a_rows:?}"
        );
        // The record store was REBUILT (dissolution stays pair-precise).
        {
            let records = app.connections.records();
            assert_eq!(records.len(), 1);
            let rec = records
                .get(&(a.clone(), b.clone()))
                .expect("the a → b record is back");
            assert_eq!(rec.kind(), Some(aterm_session::ConnectionKind::Both));
        }
        // And the ❯ count agrees with the map's arrow fold: one connection.
        assert_eq!(crate::connections::connection_count(&app.store), 1);

        // Second pass: nothing pending, nothing changes.
        app.remint_carried_connections();
        assert_eq!(crate::connections::connection_count(&app.store), 1);
    }

    /// A one-window, one-terminal-leaf layout whose leaf carries `leaf`'s USER
    /// fields — the shape `capture_restore_manifest` writes for a window that
    /// holds a single session.
    fn single_leaf_window(leaf: restore::TerminalLeafRestore) -> restore::WindowLayout {
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
                root: restore::RestoredSplitTree::leaf(restore::RestoredView::Terminal(leaf)),
                focused_path: Vec::new(),
                zoomed: false,
            }],
        }
    }

    fn bare_leaf() -> restore::TerminalLeafRestore {
        restore::TerminalLeafRestore {
            cwd: None,
            title: String::new(),
            profile: None,
            local_id: Some(0),
            user_title: None,
            description: None,
            icon: None,
            role: None,
            attention: None,
            identity: None,
        }
    }

    /// Mark pool session `session` as the shell a seamless handoff ADOPTED
    /// under the parent's id `parent_id` — what `spawn_session` records from
    /// the `Adopted` handle.
    fn adopt_as(app: &mut App, session: u64, parent_id: u64) {
        app.pool
            .sessions
            .get_mut(&session)
            .expect("live session")
            .session
            .handoff_local_id = Some(parent_id);
    }

    /// The live USER metadata of pool session `session`, read the way `@<sid>
    /// meta` reads it: resolve the sid through the registry, then lock the
    /// handle's ctx.
    fn registry_meta(app: &App, session: u64) -> crate::session_timeline::SessionMeta {
        let sid = app
            .pool
            .get(session)
            .expect("the session survived the restore")
            .ctx
            .self_id
            .clone();
        let store = app.store.read().unwrap();
        let handle = store.by_sid(&sid).expect("the sid still resolves");
        handle.ctx.meta.lock().unwrap().clone()
    }

    /// THE SELF-UPDATE HANDOFF DROPPED EVERY SESSION'S USER METADATA. Measured
    /// 2026-09-12 on the 0.82.0 -> 0.83.0 handoff, on a machine with two windows
    /// and one terminal in each: before it `meta` read `role=agent:claude-driver-
    /// fable` on one session and `role=worker:claude-satcomp` plus an
    /// `attention=` on the other; after it both — the same sids, `attribution=
    /// adopted` — read `user_title=- description=- icon=- role=- attention=-`.
    ///
    /// This drives every stage that shape crosses, in order: `meta set`'s own
    /// write ladder, the post-park capture (`capture_restore_manifest`, the call
    /// `start_unix_update_handoff` makes), the exact sidecar bytes the parent
    /// writes (`to_toml`) and the child's acceptance of them (`from_toml` plus
    /// `covers_exact_seamless_ids`), then the successor's per-window apply onto
    /// the sessions it ADOPTED — each window's bootstrap, which is what fills a
    /// window's first terminal leaf. Capture, wire and parse all carried the
    /// fields; the graft in `restore_terminal_leaf` dropped them.
    #[test]
    fn a_handoff_keeps_user_metadata_on_every_adopted_window_bootstrap() {
        use crate::session_timeline::{MetaEdit, MetaField, SessionMeta, write_session_meta};

        let stamp = |app: &App, session: u64, field: MetaField, value: &str| {
            let ctx = app.pool.get(session).expect("live session").ctx.clone();
            assert_eq!(
                write_session_meta(&ctx, field, MetaEdit::Set(value)),
                Ok(true),
                "`meta set {}` stores",
                field.wire_name()
            );
        };

        // PREDECESSOR: two windows, one terminal each — the measured layout.
        let mut old = App::headless_for_test();
        let second = crate::stub_session(1);
        App::register_session(&old.store, &second, None);
        let old_second_window = old.insert_logical_window(second, 24, 80);
        // Session 0 carries all five fields; session 1 carries two, and its
        // title, description and icon are never set.
        stamp(&old, 0, MetaField::Title, "fable driver");
        stamp(&old, 0, MetaField::Description, "drives the satcomp worker");
        stamp(&old, 0, MetaField::Icon, "🦊");
        stamp(&old, 0, MetaField::Role, "agent:claude-driver-fable");
        stamp(&old, 0, MetaField::Attention, "waiting on a human review");
        stamp(&old, 1, MetaField::Role, "worker:claude-satcomp");
        stamp(
            &old,
            1,
            MetaField::Attention,
            "SAT-COMP campaign docs handoff",
        );
        let driver = SessionMeta {
            user_title: Some("fable driver".to_string()),
            description: Some("drives the satcomp worker".to_string()),
            icon: Some("🦊".to_string()),
            role: Some("agent:claude-driver-fable".to_string()),
            attention: Some("waiting on a human review".to_string()),
            ..SessionMeta::default()
        };
        let worker = SessionMeta {
            role: Some("worker:claude-satcomp".to_string()),
            attention: Some("SAT-COMP campaign docs handoff".to_string()),
            ..SessionMeta::default()
        };
        assert_eq!(registry_meta(&old, 0), driver, "PRECONDITION: stamped");
        assert_eq!(registry_meta(&old, 1), worker, "PRECONDITION: stamped");

        // CAPTURE, then the WIRE: the parent writes exactly `to_toml()` to the
        // layout sidecar (and hashes those bytes); the child parses the bytes it
        // read and refuses a layout that does not name every handed-off id once.
        let captured = old.capture_restore_manifest();
        let wire = captured
            .to_toml()
            .expect("the parent serializes its layout");
        for spelled in [
            "user_title = \"fable driver\"",
            "role = \"agent:claude-driver-fable\"",
            "role = \"worker:claude-satcomp\"",
            "attention = \"SAT-COMP campaign docs handoff\"",
        ] {
            assert!(
                wire.contains(spelled),
                "the sidecar spells {spelled}: {wire}"
            );
        }
        let carried = restore::RestoreManifest::from_toml(&wire)
            .filter(|layout| layout.covers_exact_seamless_ids(&[0, 1]))
            .expect("the child accepts the sidecar");
        assert_eq!(
            carried, captured,
            "neither the wire nor the parse drops a field"
        );

        // SUCCESSOR: window 0's bootstrap is the shell adopted in `main_entry`, window
        // 1's the one adopted in `create_window_internal` — each picked by
        // `bootstrap_local_id`, and each keeping the id the parent knew it by.
        // Adoption mints a fresh ctx, so both start with no USER metadata.
        let mut new = App::headless_for_test();
        new.handoff_successor = true;
        let picked: Vec<Option<u64>> = carried
            .windows
            .iter()
            .map(restore::WindowLayout::bootstrap_local_id)
            .collect();
        assert_eq!(picked, vec![Some(0), Some(1)]);
        adopt_as(&mut new, 0, 0);
        let adopted = crate::stub_session(1);
        App::register_session(&new.store, &adopted, None);
        let new_second_window = new.insert_logical_window(adopted, 24, 80);
        adopt_as(&mut new, 1, 1);
        assert_eq!(new_second_window, old_second_window);
        assert!(
            !registry_meta(&new, 0).any_set() && !registry_meta(&new, 1).any_set(),
            "PRECONDITION: an adopted session arrives with a fresh ctx"
        );

        // APPLY: what `apply_restore_manifest` runs for each window once its
        // host (and so its bootstrap session) exists.
        let mut windows = carried.windows.into_iter();
        new.restore_into_window(WindowId(0), windows.next().expect("window 0"));
        new.restore_into_window(new_second_window, windows.next().expect("window 1"));

        for (window, session) in [(WindowId(0), 0), (new_second_window, 1)] {
            assert_eq!(
                new.focused_session_id(window),
                Some(session),
                "the leaf was filled by the ADOPTED session, not a respawn"
            );
        }
        assert_eq!(
            registry_meta(&new, 0),
            driver,
            "all five fields survive onto the adopted session"
        );
        assert_eq!(
            registry_meta(&new, 1),
            worker,
            "the two set fields survive and the three unset ones stay unset"
        );
        // And the successor now carries exactly the layout the parent's Commit
        // compared — USER metadata included, which `commit_layout_topology`
        // deliberately keeps in that comparison.
        assert_eq!(new.capture_restore_manifest(), captured);
        assert!(new.structural_invariants_ok());
    }

    /// The graft stores the leaf's USER fields EXACTLY over anything a driver
    /// did not write: an unset leaf field is unset on the session afterwards
    /// even when the session held a value — as it does on a cold restore when
    /// an earlier graft's tab failed to build and handed the bootstrap on to
    /// the next terminal leaf. (`stale` is assigned raw, the way that earlier
    /// graft stored it: no driver wrote it.)
    #[test]
    fn a_graft_leaves_every_field_the_leaf_does_not_carry_unset() {
        use crate::session_timeline::SessionMeta;

        let stale = SessionMeta {
            user_title: Some("someone else".to_string()),
            description: Some("a leaf that failed to build".to_string()),
            icon: Some("🧟".to_string()),
            role: Some("operator".to_string()),
            attention: Some("stale escalation".to_string()),
            ..SessionMeta::default()
        };
        let mut app = App::headless_for_test();
        *app.pool.get(0).unwrap().ctx.meta.lock().unwrap() = stale.clone();
        let mut leaf = bare_leaf();
        leaf.role = Some("worker:claude-satcomp".to_string());
        app.restore_into_window(WindowId(0), single_leaf_window(leaf));
        assert_eq!(app.focused_session_id(WindowId(0)), Some(0));
        assert_eq!(
            registry_meta(&app, 0),
            SessionMeta {
                role: Some("worker:claude-satcomp".to_string()),
                ..SessionMeta::default()
            },
            "only the leaf's own field is set"
        );

        // A leaf carrying nothing leaves a grafted session with nothing.
        let mut app = App::headless_for_test();
        *app.pool.get(0).unwrap().ctx.meta.lock().unwrap() = stale;
        app.restore_into_window(WindowId(0), single_leaf_window(bare_leaf()));
        assert!(
            !registry_meta(&app, 0).any_set(),
            "{:?}",
            registry_meta(&app, 0)
        );
    }

    /// The bootstrap window presents BEFORE the deferred restore runs, so its
    /// session's tooltip is already composed and cached by the time the graft
    /// seeds it. An icon-only seed moves no label, activity or connection
    /// revision the cache is keyed on; it recomposes only because the graft
    /// records its `meta-change`, which moves the timeline high-water mark.
    /// (Seeded without that record, the tooltip stayed
    /// "zsh\nstate: spawning\n\nspawned · just now".)
    #[test]
    fn a_graft_recomposes_the_chrome_the_first_present_cached() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        // A live OSC title outranks the tab's fallback, so the label — one of
        // the cache's keys — is the same before and after the rebuild.
        feed_session0(&app, b"\x1b]2;zsh\x07");
        app.refresh_window_tabs(wid);
        let tooltip = |app: &App| {
            app.windows[&wid]
                .tab_set
                .active()
                .and_then(|tab| tab.presentation.tooltip.clone())
        };
        assert!(
            !tooltip(&app).is_some_and(|text| text.contains('🚀')),
            "PRECONDITION: {:?}",
            tooltip(&app)
        );

        let mut leaf = bare_leaf();
        leaf.title = "zsh".to_string();
        leaf.icon = Some("🚀".to_string());
        app.restore_into_window(wid, single_leaf_window(leaf));
        app.refresh_window_tabs(wid);
        assert_eq!(app.tab_titles(wid), vec!["zsh".to_string()]);
        assert!(
            tooltip(&app).is_some_and(|text| text.starts_with('🚀')),
            "the seeded icon reaches the tooltip: {:?}",
            tooltip(&app)
        );
    }

    /// A driver's `aterm ctl @<sid> meta set|unset` on pool session `session`:
    /// resolve the sid through the registry the way the control socket does,
    /// then take the write door the verb's handler takes (`cmd_meta` calls
    /// `write_session_meta` for a set and `apply_meta_value` for an unset,
    /// which is what `write_session_meta` does with a `Clear`). `Ok` is the
    /// `OK` the verb answers.
    fn driver_meta(
        app: &App,
        session: u64,
        field: crate::session_timeline::MetaField,
        edit: crate::session_timeline::MetaEdit<'_>,
    ) {
        let sid = app
            .pool
            .get(session)
            .expect("live session")
            .ctx
            .self_id
            .clone();
        let ctx = app
            .store
            .read()
            .unwrap()
            .by_sid(&sid)
            .expect("the sid resolves")
            .ctx
            .clone();
        assert!(
            crate::session_timeline::write_session_meta(&ctx, field, edit).is_ok(),
            "`meta` answers OK to {field:?} {edit:?}"
        );
    }

    /// A WRITE THE SUCCESSOR ANSWERED `OK` IS NEVER UNDONE BY ITS OWN RESTORE.
    /// A handoff child binds its control socket in `main_entry` — repointing
    /// `aterm.sock` and the adopted session's graph entry at itself — but runs
    /// the deferred restore only after its first present, so a `meta set` can
    /// land on the adopted bootstrap before the graft reaches it. On a cold
    /// restore the bootstrap shell's own startup can do the same. The graft
    /// keeps every field such a write touched — set, or cleared even where the
    /// clear moved nothing — and fills only the others from the leaf.
    #[test]
    fn a_graft_keeps_every_field_a_driver_wrote_before_the_restore_reached_it() {
        use crate::session_timeline::{MetaEdit, MetaField, SessionMeta};

        for adopted in [true, false] {
            let mut app = App::headless_for_test();
            if adopted {
                app.handoff_successor = true;
                adopt_as(&mut app, 0, 0);
            }
            driver_meta(&app, 0, MetaField::Role, MetaEdit::Set("worker:new"));
            driver_meta(
                &app,
                0,
                MetaField::Attention,
                MetaEdit::Set("fresh escalation"),
            );
            driver_meta(&app, 0, MetaField::Description, MetaEdit::Clear);
            let mut leaf = bare_leaf();
            leaf.user_title = Some("carried title".to_string());
            leaf.description = Some("carried notes".to_string());
            leaf.role = Some("worker:old".to_string());
            leaf.attention = Some("old escalation".to_string());
            app.restore_into_window(WindowId(0), single_leaf_window(leaf));
            assert_eq!(app.focused_session_id(WindowId(0)), Some(0));

            let meta = registry_meta(&app, 0);
            assert_eq!(
                (meta.role.as_deref(), meta.attention.as_deref()),
                (Some("worker:new"), Some("fresh escalation")),
                "adopted={adopted}: the two `meta set`s stand"
            );
            assert_eq!(
                meta,
                SessionMeta {
                    user_title: Some("carried title".to_string()),
                    role: Some("worker:new".to_string()),
                    attention: Some("fresh escalation".to_string()),
                    ..SessionMeta::default()
                },
                "adopted={adopted}: the `meta unset description` stands too, and \
                 only the title nobody wrote is carried"
            );
        }
    }

    /// THE LEAF MUST NAME THE SHELL IT IS GRAFTED ONTO. A tab split between a
    /// native view and a terminal has no entry in the legacy `tabs` mirror (the
    /// capture writes only all-terminal tabs there), so when such a tab comes
    /// first, the mirror's first leaf and the recursive tree's first terminal
    /// leaf — the one the graft fills — name different shells. `main_entry` adopted
    /// window 0's bootstrap by the mirror; with the graft carrying identity,
    /// that stamped one shell's role on another shell's live sid.
    #[test]
    fn a_graft_carries_a_leaf_only_onto_the_shell_that_leaf_names() {
        use crate::session_timeline::{MetaEdit, MetaField, SessionMeta};

        // PREDECESSOR: tab A splits a recovery placeholder with shell 0 (role
        // `operator`); tab B holds shell 1 alone (role `worker`). Built by the
        // real restore, stamped by the real verb, captured by the real capture.
        let mut old = App::headless_for_test();
        let mut layout = single_leaf_window(bare_leaf());
        layout.restored_tabs = vec![
            restore::RestoredTab {
                root: restore::RestoredSplitTree::Split {
                    axis: restore::SplitKind::Horizontal,
                    ratio: 0.5,
                    first: Box::new(restore::RestoredSplitTree::leaf(
                        restore::RestoredView::Placeholder(restore::PlaceholderLeafRestore {
                            restore_tag: "markdown".to_string(),
                            reason: "Document could not be reopened".to_string(),
                            metadata: String::new(),
                        }),
                    )),
                    second: Box::new(restore::RestoredSplitTree::leaf(
                        restore::RestoredView::Terminal(bare_leaf()),
                    )),
                },
                focused_path: vec![restore::RestoreBranch::Second],
                zoomed: false,
            },
            restore::RestoredTab {
                root: restore::RestoredSplitTree::leaf(
                    restore::RestoredView::Terminal(bare_leaf()),
                ),
                focused_path: Vec::new(),
                zoomed: false,
            },
        ];
        old.restore_into_window(WindowId(0), layout);
        driver_meta(&old, 0, MetaField::Role, MetaEdit::Set("operator"));
        driver_meta(&old, 1, MetaField::Role, MetaEdit::Set("worker"));
        let wire = old
            .capture_restore_manifest()
            .to_toml()
            .expect("the parent serializes its layout");
        let carried = restore::RestoreManifest::from_toml(&wire)
            .filter(|layout| layout.covers_exact_seamless_ids(&[0, 1]))
            .expect("the child accepts the sidecar");
        let window = carried.windows[0].clone();
        let mirror_first = window
            .tabs
            .first()
            .and_then(|tab| tab.leaves().first().and_then(|leaf| leaf.local_id()));
        assert_eq!(
            (mirror_first, window.bootstrap_local_id()),
            (Some(1), Some(0)),
            "PRECONDITION: the mirror's first leaf is tab B's shell; the graft fills \
             tab A's"
        );

        let operator = SessionMeta {
            role: Some("operator".to_string()),
            ..SessionMeta::default()
        };
        let worker = SessionMeta {
            role: Some("worker".to_string()),
            ..SessionMeta::default()
        };
        // The session each of window 0's tabs shows, in tab order.
        let tab_sessions = |app: &App| -> Vec<Vec<u64>> {
            app.windows[&WindowId(0)]
                .tab_set
                .tabs()
                .iter()
                .map(|tab| {
                    tab.root
                        .leaves()
                        .into_iter()
                        .filter_map(|view| {
                            app.view_store
                                .get(view)
                                .copied()
                                .and_then(crate::tab_model::View::terminal_session)
                        })
                        .collect()
                })
                .collect()
        };

        // `main_entry` picks by `bootstrap_local_id`: session 0 IS shell 0, fills tab
        // A's terminal pane, and takes shell 0's identity; tab B adopts shell 1.
        let mut new = App::headless_for_test();
        new.handoff_successor = true;
        adopt_as(&mut new, 0, 0);
        new.seamless_adopt = vec![handed_off_shell(1)];
        new.restore_into_window(WindowId(0), window.clone());
        let placed = tab_sessions(&new);
        assert_eq!(placed[0], vec![0], "tab A's terminal pane is session 0");
        assert_eq!(registry_meta(&new, 0), operator);
        assert_eq!(
            new.pool.get(placed[1][0]).and_then(|s| s.handoff_local_id),
            Some(1),
            "tab B's pane adopted shell 1"
        );
        assert_eq!(registry_meta(&new, placed[1][0]), worker);

        // The mirror's pick: session 0 is shell 1. The graft still fills tab
        // A's pane with it, but tab A's leaf names shell 0, so none of that
        // leaf's identity may land on this one. Tab B's leaf names shell 1,
        // which is taken, so a fresh stand-in fills that pane: shell 1's
        // identity goes to session 0, the shell it describes, and the
        // stand-in wears none.
        let mut new = App::headless_for_test();
        new.handoff_successor = true;
        adopt_as(&mut new, 0, 1);
        new.seamless_adopt = vec![handed_off_shell(0)];
        new.restore_into_window(WindowId(0), window.clone());
        let placed = tab_sessions(&new);
        assert_eq!(placed[0], vec![0]);
        assert_eq!(
            registry_meta(&new, 0),
            worker,
            "a leaf naming shell 0 stamped its identity on shell 1, or shell 1 lost its own"
        );
        assert_eq!(
            new.pool.get(placed[1][0]).and_then(|s| s.handoff_local_id),
            None,
            "PRECONDITION: tab B's pane is a stand-in"
        );
        assert_eq!(
            registry_meta(&new, placed[1][0]),
            SessionMeta::default(),
            "a stand-in wore the identity of the shell it stands in for"
        );

        // A handoff child's bootstrap it did NOT adopt stands in for a shell
        // running elsewhere; it is not the one the leaf names either.
        let mut new = App::headless_for_test();
        new.handoff_successor = true;
        new.seamless_adopt = vec![handed_off_shell(1)];
        new.restore_into_window(WindowId(0), window);
        let placed = tab_sessions(&new);
        assert_eq!(placed[0], vec![0]);
        assert_eq!(registry_meta(&new, 0), SessionMeta::default());
        assert_eq!(registry_meta(&new, placed[1][0]), worker);
    }

    /// A FROZEN SHELL'S ADOPTION REACHES THE REGISTRY'S COUNT (2026-09-16), end to
    /// end through the spawn seam: `seamless::take_incoming` decides
    /// `Adopted::frozen_path` (the manifest's `outgoing_build` absent, or the
    /// record's own flag), `spawn_session` copies it onto `Session::frozen_path`,
    /// and `App::register_session` marks it on the store — where
    /// `SessionStore::frozen_path_tabs` counts it for the managed row's "N tabs
    /// from before this update" note and the seed pill's narrowing. Both adoption
    /// paths are driven: the restore of a leaf naming the shell, and the orphan
    /// net. Dropping `Session::frozen_path` (or the copy in either path) fails
    /// this, and with it the note that keeps the "this one too" claim honest.
    #[test]
    fn a_frozen_shells_adoption_reaches_the_registrys_frozen_tab_count() {
        let frozen_shell = |local_id: u64| {
            let mut shell = handed_off_shell(local_id);
            shell.frozen_path = true;
            shell
        };
        let frozen_sessions = |app: &App| -> Vec<(Option<u64>, bool, bool)> {
            let store = app.store.read().unwrap();
            let mut rows: Vec<_> = app
                .pool
                .sessions
                .iter()
                .map(|(&session, pooled)| {
                    (
                        pooled.session.handoff_local_id,
                        pooled.session.frozen_path,
                        store.has_frozen_path(session),
                    )
                })
                .collect();
            rows.sort_unstable();
            rows
        };

        // The restore path: tab B's leaf names shell 3, handed across frozen; tab A
        // is a stand-in (no shell adopted, never frozen).
        let mut new = App::headless_for_test();
        new.handoff_successor = true;
        new.seamless_adopt = vec![frozen_shell(3)];
        new.restore_into_window(WindowId(0), window_of(vec![leaf_naming(0), leaf_naming(3)]));
        assert!(new.seamless_adopt.is_empty(), "the leaf adopted shell 3");
        assert_eq!(
            frozen_sessions(&new),
            vec![(None, false, false), (Some(3), true, true)],
            "the session adopted from shell 3 is frozen on the session AND the registry"
        );
        assert_eq!(new.store.read().unwrap().frozen_path_tabs(), 1);

        // The orphan net: shell 1 frozen, shell 2 not — each counted as it came.
        let mut new = App::headless_for_test();
        new.handoff_successor = true;
        new.restore_into_window(WindowId(0), window_of(vec![leaf_naming(0)]));
        new.seamless_adopt = vec![frozen_shell(1), handed_off_shell(2)];
        new.adopt_orphan_shells_as_tabs(&[]);
        assert!(new.seamless_adopt.is_empty(), "the net placed every orphan");
        assert_eq!(
            frozen_sessions(&new),
            vec![
                (None, false, false),
                (Some(1), true, true),
                (Some(2), false, false)
            ]
        );
        assert_eq!(new.store.read().unwrap().frozen_path_tabs(), 1);
    }

    /// THE IDENTITY CARRY (session identities, 2026-09-17), end to end. Across
    /// a seamless update the record's label reaches the session and the
    /// registry handle through both adoption paths (the leaf naming the shell,
    /// and the orphan net) — the shell keeps the identity's env, the label is
    /// what rides. Across a quit, a leaf naming an identity respawns under it
    /// only if it still exists: `create = false` — a leaf naming a forgotten
    /// identity comes back as a default shell, and nothing is created for it.
    #[test]
    fn an_identitys_label_rides_the_adoption_and_a_cold_restore_never_creates_one() {
        let state = std::env::temp_dir().join(format!(
            "aterm-restore-identity-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&state).unwrap();
        let labels = |app: &App| -> Vec<(Option<u64>, Option<String>, Option<String>)> {
            let store = app.store.read().unwrap();
            let mut rows: Vec<_> = app
                .pool
                .sessions
                .iter()
                .map(|(&session, pooled)| {
                    (
                        pooled.session.handoff_local_id,
                        pooled.session.identity.clone(),
                        store
                            .by_local(session)
                            .and_then(|h| h.identity.as_deref().map(str::to_owned)),
                    )
                })
                .collect();
            rows.sort_unstable();
            rows
        };
        let under = |shell: u64, name: &str| {
            restore::RestoredSplitTree::leaf(restore::RestoredView::Terminal(
                restore::TerminalLeafRestore {
                    local_id: Some(shell),
                    identity: Some(name.to_string()),
                    ..bare_leaf()
                },
            ))
        };
        aterm_log::env::scoped("ATERM_STATE_HOME", &state, || {
            // Seamless, the leaf path: the record says `worker`, the session and
            // its handle say `worker`; the stand-in for leaf 0 says nothing.
            let mut new = App::headless_for_test();
            new.handoff_successor = true;
            let mut shell = handed_off_shell(3);
            shell.identity = Some("worker".to_string());
            new.seamless_adopt = vec![shell];
            new.restore_into_window(WindowId(0), window_of(vec![leaf_naming(0), leaf_naming(3)]));
            assert!(new.seamless_adopt.is_empty(), "the leaf adopted shell 3");
            assert_eq!(
                labels(&new),
                vec![
                    (None, None, None),
                    (
                        Some(3),
                        Some("worker".to_string()),
                        Some("worker".to_string())
                    ),
                ]
            );
            // Seamless, the orphan net: the same carry.
            let mut new = App::headless_for_test();
            new.handoff_successor = true;
            new.restore_into_window(WindowId(0), window_of(vec![leaf_naming(0)]));
            let mut shell = handed_off_shell(1);
            shell.identity = Some("reviewer".to_string());
            new.seamless_adopt = vec![shell, handed_off_shell(2)];
            new.adopt_orphan_shells_as_tabs(&[]);
            assert_eq!(
                labels(&new),
                vec![
                    (None, None, None),
                    (
                        Some(1),
                        Some("reviewer".to_string()),
                        Some("reviewer".to_string())
                    ),
                    (Some(2), None, None),
                ]
            );
            // Cold: `worker` exists (the verb created it); `ghost` never did.
            crate::agent_identity::ensure("worker", true).expect("the verb's create");
            let mut cold = App::headless_for_test();
            cold.restore_into_window(
                WindowId(0),
                window_of(vec![leaf_naming(0), under(7, "worker"), under(8, "ghost")]),
            );
            assert_eq!(
                labels(&cold),
                vec![
                    (None, None, None),
                    (None, None, None),
                    (None, Some("worker".to_string()), Some("worker".to_string())),
                ],
                "the leaf under `worker` respawns under it; the one under `ghost` is a default shell"
            );
            assert!(
                state.join("identities").join("worker").is_dir()
                    && !state.join("identities").join("ghost").exists(),
                "restore never creates an identity"
            );
        });
        let _ = std::fs::remove_dir_all(&state);
    }

    /// REVIEW (separation lens, 2026-09-17): ON A COLD RESTORE, THE WINDOW'S
    /// FIRST LEAF KEEPS ITS IDENTITY. The window's bootstrap shell is grafted
    /// onto its first terminal leaf, and the graft carried only the USER meta:
    /// a tab that was `identity=worker` in window 0's first leaf came back as
    /// `identity=-`, its bootstrap forked under the human's login — measured
    /// `[(None, None), (Some("worker"), Some("worker"))]` for two leaves both
    /// naming `worker`. The agent identity is the SHELL's: the graft now
    /// takes the bootstrap only when it wears what the pane names
    /// (`main_entry` forks it under `first_leaf_identity` so it does), a
    /// bootstrap wearing something else fills a later pane that names its
    /// identity or is retired, and the pane forks its own shell under the
    /// identity — in every case each pane's shell wears what its leaf names.
    #[test]
    fn a_cold_restore_keeps_the_identity_of_the_windows_first_leaf() {
        let state = std::env::temp_dir().join(format!(
            "aterm-restore-first-leaf-identity-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&state).unwrap();
        // `(local id, the session's label, the registry handle's label)`, by id.
        let rows = |app: &App| -> Vec<(u64, Option<String>, Option<String>)> {
            let store = app.store.read().unwrap();
            let mut rows: Vec<_> = app
                .pool
                .sessions
                .iter()
                .map(|(&session, pooled)| {
                    (
                        session,
                        pooled.session.identity.clone(),
                        store
                            .by_local(session)
                            .and_then(|h| h.identity.as_deref().map(str::to_owned)),
                    )
                })
                .collect();
            rows.sort_unstable();
            rows
        };
        let under = |shell: u64, name: Option<&str>| {
            restore::RestoredSplitTree::leaf(restore::RestoredView::Terminal(
                restore::TerminalLeafRestore {
                    local_id: Some(shell),
                    identity: name.map(str::to_owned),
                    ..bare_leaf()
                },
            ))
        };
        let worker = || Some("worker".to_string());
        aterm_log::env::scoped("ATERM_STATE_HOME", &state, || {
            crate::agent_identity::ensure("worker", true).expect("the verb's create");
            // The reviewer's shape: both leaves name `worker`; the bootstrap (a
            // stub, the human's own) wears none. Neither pane takes it — each
            // forks its own shell under `worker` — and it is retired unconsumed.
            let mut cold = App::headless_for_test();
            cold.restore_into_window(
                WindowId(0),
                window_of(vec![under(7, Some("worker")), under(8, Some("worker"))]),
            );
            assert_eq!(
                rows(&cold),
                vec![(1, worker(), worker()), (2, worker(), worker())],
                "both panes under `worker`; the bootstrap that wore none is gone"
            );
            // A later pane names what the bootstrap wears: it fills THAT pane.
            let mut cold = App::headless_for_test();
            cold.restore_into_window(
                WindowId(0),
                window_of(vec![under(7, Some("worker")), under(8, None)]),
            );
            assert_eq!(
                rows(&cold),
                vec![(0, None, None), (1, worker(), worker())],
                "the bootstrap (session 0) fills the second pane; the first forked its own"
            );
            // A bootstrap already under `worker` — what `main_entry` forks for
            // this layout, by `first_leaf_identity` — is grafted: no extra fork.
            let mut cold = App::headless_for_test();
            cold.pool
                .sessions
                .get_mut(&0)
                .expect("the headless bootstrap")
                .session
                .identity = worker();
            cold.restore_into_window(
                WindowId(0),
                window_of(vec![under(7, Some("worker")), under(8, Some("worker"))]),
            );
            let mut ids: Vec<u64> = cold.pool.sessions.keys().copied().collect();
            ids.sort_unstable();
            assert_eq!(
                ids,
                vec![0, 1],
                "session 0 was grafted, one fork for the second pane"
            );
            assert!(
                cold.pool
                    .sessions
                    .values()
                    .all(|p| p.session.identity == worker()),
                "every pane's shell wears `worker`"
            );
            // A forgotten identity is a default shell on both sides: the leaf
            // naming `ghost` takes the bootstrap that wears none.
            let mut cold = App::headless_for_test();
            cold.restore_into_window(
                WindowId(0),
                window_of(vec![under(7, Some("ghost")), under(8, Some("worker"))]),
            );
            assert_eq!(rows(&cold), vec![(0, None, None), (1, worker(), worker())]);
            assert!(!state.join("identities").join("ghost").exists());
        });
        let _ = std::fs::remove_dir_all(&state);
    }

    /// THE BOOTSTRAP IS FORKED UNDER THE IDENTITY OF THE PANE IT FILLS. The
    /// pick is `bootstrap_local_id`'s — the canonical tree's first terminal
    /// leaf in rebuild order, behind native and placeholder leaves — so
    /// `main_entry` (`first_leaf_identity`) and `apply_restore_manifest`
    /// (`bootstrap_identity`) fork each window's bootstrap under exactly the
    /// identity the graft will look for. The legacy mirror names none.
    #[test]
    fn the_bootstrap_identity_is_the_identity_of_the_leaf_the_bootstrap_fills() {
        let under = |shell: u64, name: &str| {
            restore::RestoredSplitTree::leaf(restore::RestoredView::Terminal(
                restore::TerminalLeafRestore {
                    local_id: Some(shell),
                    identity: Some(name.to_string()),
                    ..bare_leaf()
                },
            ))
        };
        let layout = window_of(vec![
            settings_leaf(),
            split_of(
                restore::RestoredSplitTree::leaf(restore::RestoredView::Placeholder(
                    restore::PlaceholderLeafRestore {
                        restore_tag: "markdown".to_string(),
                        reason: "Document could not be reopened".to_string(),
                        metadata: String::new(),
                    },
                )),
                under(4, "worker"),
            ),
            under(5, "reviewer"),
        ]);
        assert_eq!(
            (layout.bootstrap_local_id(), layout.bootstrap_identity()),
            (Some(4), Some("worker")),
            "the same leaf, behind the settings tab and the placeholder"
        );
        let mut manifest = App::headless_for_test().capture_restore_manifest();
        manifest.windows = vec![layout];
        assert_eq!(manifest.first_leaf_identity(), Some("worker"));
        // A pane naming no identity, and a legacy (mirror-only) layout — a
        // capture with its canonical tabs dropped, so only the `tabs` mirror
        // names the bootstrap's pane: none.
        let plain = window_of(vec![leaf_naming(0)]);
        assert_eq!(plain.bootstrap_identity(), None);
        let mut legacy = App::headless_for_test()
            .capture_restore_manifest()
            .windows
            .remove(0);
        legacy.restored_tabs.clear();
        assert_eq!(
            (legacy.bootstrap_local_id(), legacy.bootstrap_identity()),
            (Some(0), None)
        );
    }

    /// THE ROW BUILT BEFORE THE ADOPTION NAMES THE FROZEN TAB (review,
    /// 2026-09-16). The atpkg launch pass starts from `main_entry` before the
    /// event loop, and on a warm machine prints `managed-current:` within tens
    /// of milliseconds — before the first park after first paint adopts the
    /// handed-off shells (`apply_pending_restore`). Counted from the registry
    /// alone, that first row read "this one too" in exactly the tab the owner
    /// had complained about, and the honest row came seconds later (a double
    /// post) or six hours later. `App::frozen_path_tabs` counts the pending
    /// adoptees, so the marker that arrives first is built right; the restore's
    /// refresh (`StatusBars::refresh_managed_current`) then finds the registry
    /// agreeing and posts nothing — one row, the true one.
    #[test]
    fn a_managed_row_built_before_the_adoption_counts_the_frozen_shell_and_reposts_nothing() {
        use std::time::{Duration, Instant};
        let wire = "claude 2.1.273 (build 2026091601); codex 0.154.0 (build 2026091001)";
        let now = Instant::now();
        let mut new = App::headless_for_test();
        new.handoff_successor = true;
        let mut frozen = handed_off_shell(3);
        frozen.frozen_path = true;
        new.seamless_adopt = vec![frozen, handed_off_shell(4)];
        // Before the adoption: the registry knows nothing, the count is 1.
        assert_eq!(new.store.read().unwrap().frozen_path_tabs(), 0);
        assert_eq!(new.frozen_path_tabs(), 1, "the pending adoptee is counted");
        // The marker lands now — what `Wake::PkgManagedCurrent` does.
        let frozen_tabs = new.frozen_path_tabs();
        let hooked = new.this_tab_hooked();
        new.status_bars
            .toolchain_managed_current(wire, frozen_tabs, hooked, now);
        let (_, bar) = new.status_bars.bars().next().expect("the row posts");
        assert!(
            bar.text.detail.ends_with(
                "\u{00b7} 1 tab from before this update picks them up with \
                 `. ~/.aterm/shell.d/00-atpkg.zsh`"
            ) && !bar.text.detail.contains("this one too"),
            "{}",
            bar.text.detail
        );
        // The adoption: the leaf places shell 3, the net places shell 4.
        new.restore_into_window(WindowId(0), window_of(vec![leaf_naming(0), leaf_naming(3)]));
        new.adopt_orphan_shells_as_tabs(&[]);
        assert!(new.seamless_adopt.is_empty());
        assert_eq!(new.store.read().unwrap().frozen_path_tabs(), 1);
        assert_eq!(new.frozen_path_tabs(), 1, "registered now, counted once");
        // The refresh the restore runs: the count agrees, nothing posts.
        let frozen_tabs = new.frozen_path_tabs();
        assert!(
            !new.status_bars
                .refresh_managed_current(frozen_tabs, hooked, now),
            "the row built before the adoption was already the true one"
        );
        assert_eq!(
            new.status_bars.rows(),
            1,
            "one row, not a false-then-true pair"
        );
        // The backstop: an adoptee counted but never registered (dropped by the
        // net) is settled by the refresh — the count went down, the row reposts.
        let mut new = App::headless_for_test();
        new.handoff_successor = true;
        let mut frozen = handed_off_shell(5);
        frozen.frozen_path = true;
        new.seamless_adopt = vec![frozen];
        new.status_bars
            .toolchain_managed_current(wire, new.frozen_path_tabs(), true, now);
        new.seamless_adopt.clear();
        assert_eq!(new.frozen_path_tabs(), 0);
        assert!(new.status_bars.refresh_managed_current(
            new.frozen_path_tabs(),
            true,
            now + Duration::from_secs(1)
        ));
    }

    /// A shell the outgoing process handed across as `local_id`, the way
    /// `seamless::take_incoming` gives it to `main_entry`. A headless restore
    /// reads only the id: the stub spawn never touches the fd or the pid.
    fn handed_off_shell(local_id: u64) -> crate::spawn::Adopted {
        crate::spawn::Adopted {
            local_id,
            master: -1,
            pid: -1,
            sid: aterm_session::SessionId::generate(),
            nonce: aterm_session::LaunchNonce::generate(),
            checkpoint: None,
            control: None,
            frozen_path: false,
            identity: None,
            topics: Vec::new(),
        }
    }

    /// The five USER fields handed-off shell `shell` wore in the predecessor.
    /// Every value names its shell, so a value on the wrong session says whose
    /// it was.
    fn identity_of(shell: u64) -> crate::session_timeline::SessionMeta {
        crate::session_timeline::SessionMeta {
            user_title: Some(format!("shell {shell}")),
            description: Some(format!("the work shell {shell} was doing")),
            icon: Some(format!("🐚{shell}")),
            role: Some(format!("worker:shell-{shell}")),
            attention: Some(format!("shell {shell} waits on a review")),
            ..crate::session_timeline::SessionMeta::default()
        }
    }

    /// The terminal leaf a capture writes for handed-off shell `shell`,
    /// carrying the identity [`identity_of`] gives that shell.
    fn leaf_naming(shell: u64) -> restore::RestoredSplitTree {
        let identity = identity_of(shell);
        restore::RestoredSplitTree::leaf(restore::RestoredView::Terminal(
            restore::TerminalLeafRestore {
                cwd: Some(format!("/tmp/shell-{shell}")),
                title: format!("zsh {shell}"),
                profile: None,
                local_id: Some(shell),
                user_title: identity.user_title,
                description: identity.description,
                icon: identity.icon,
                role: identity.role,
                attention: identity.attention,
                identity: None,
            },
        ))
    }

    fn settings_leaf() -> restore::RestoredSplitTree {
        restore::RestoredSplitTree::leaf(restore::RestoredView::Native(
            restore::NativeLeafRestore::settings("/about".to_string()),
        ))
    }

    fn split_of(
        first: restore::RestoredSplitTree,
        second: restore::RestoredSplitTree,
    ) -> restore::RestoredSplitTree {
        restore::RestoredSplitTree::Split {
            axis: restore::SplitKind::Horizontal,
            ratio: 0.5,
            first: Box::new(first),
            second: Box::new(second),
        }
    }

    /// A window of one tab per root, each focused on its first pane (a
    /// focus path must name a leaf, or the parse refuses the tab).
    fn window_of(roots: Vec<restore::RestoredSplitTree>) -> restore::WindowLayout {
        let mut layout = single_leaf_window(bare_leaf());
        layout.restored_tabs = roots
            .into_iter()
            .map(|root| {
                let mut focused_path = Vec::new();
                let mut node = &root;
                while let restore::RestoredSplitTree::Split { first, .. } = node {
                    focused_path.push(restore::RestoreBranch::First);
                    node = first;
                }
                restore::RestoredTab {
                    root,
                    focused_path,
                    zoomed: false,
                }
            })
            .collect();
        layout
    }

    /// `main_entry`'s pick for session 0 before `take_session0_shell`: the shell
    /// window 0's bootstrap leaf names or, with no such leaf, the first shell the
    /// handoff lists (`SessionHandoff::from_store` lists them in local-id order).
    fn first_listed_session0_shell(
        handed_off: &mut Vec<crate::spawn::Adopted>,
        layout: &restore::RestoreManifest,
    ) -> Option<crate::spawn::Adopted> {
        super::take_handed_off_shell(handed_off, layout.windows[0].bootstrap_local_id())
            .or_else(|| Some(handed_off.remove(0)))
    }

    /// A handoff SUCCESSOR rebuilt from `windows` the way the child runs. The
    /// layout crosses the wire (`to_toml`, then `from_toml` plus
    /// `covers_exact_seamless_ids`, as `take_incoming` accepts it) with shells
    /// `0..shells` handed off; session 0 adopts by `session0`'s pick; window 0
    /// is rebuilt around it, and every further window is created around the
    /// shell its bootstrap leaf names — `apply_restore_manifest`'s pick — or a
    /// fresh one when that shell is taken. The restore itself runs unchanged,
    /// and must place every shell: this harness leaves the orphan net nothing.
    fn successor_of(
        windows: Vec<restore::WindowLayout>,
        shells: u64,
        session0: fn(
            &mut Vec<crate::spawn::Adopted>,
            &restore::RestoreManifest,
        ) -> Option<crate::spawn::Adopted>,
    ) -> App {
        let ids = (0..shells).collect::<Vec<_>>();
        let wire = restore::RestoreManifest::new(windows)
            .to_toml()
            .expect("the parent serializes its layout");
        let carried = restore::RestoreManifest::from_toml(&wire)
            .filter(|layout| layout.covers_exact_seamless_ids(&ids))
            .expect("the child accepts the sidecar");
        let mut new = App::headless_for_test();
        new.handoff_successor = true;
        let mut handed_off = ids.iter().copied().map(handed_off_shell).collect();
        if let Some(shell) = session0(&mut handed_off, &carried) {
            adopt_as(&mut new, 0, shell.local_id);
        }
        new.seamless_adopt = handed_off;
        let mut windows = carried.windows.into_iter();
        new.restore_into_window(WindowId(0), windows.next().expect("window 0"));
        for layout in windows {
            let mut bootstrap = crate::stub_session(new.next_session_id);
            bootstrap.handoff_local_id =
                super::take_handed_off_shell(&mut new.seamless_adopt, layout.bootstrap_local_id())
                    .map(|shell| shell.local_id);
            App::register_session(&new.store, &bootstrap, None);
            let wid = new.insert_logical_window(bootstrap, 24, 80);
            new.restore_into_window(wid, layout);
        }
        assert!(
            new.seamless_adopt.is_empty(),
            "PRECONDITION: the layout placed every handed-off shell"
        );
        new
    }

    /// What every leaf of every window shows, tab by tab: the handed-off
    /// shell a terminal pane runs (`Some(None)` for a shell nobody handed
    /// off), `None` for a native view.
    fn shown_shells(app: &App) -> Vec<Vec<Vec<Option<Option<u64>>>>> {
        app.windows
            .values()
            .map(|window| {
                window
                    .tab_set
                    .tabs()
                    .iter()
                    .map(|tab| {
                        tab.root
                            .leaves()
                            .into_iter()
                            .map(|view| {
                                let session = app
                                    .view_store
                                    .get(view)
                                    .copied()
                                    .and_then(crate::tab_model::View::terminal_session)?;
                                Some(app.pool.get(session)?.handoff_local_id)
                            })
                            .collect()
                    })
                    .collect()
            })
            .collect()
    }

    /// Every tab focused on a handed-off shell shows that shell's icon
    /// ([`identity_of`]) at the head of its tooltip.
    fn assert_tooltips_name_their_shells(app: &App, shape: &str) {
        for window in app.windows.values() {
            for tab in window.tab_set.tabs() {
                let Some(shell) = app
                    .view_store
                    .get(tab.focus)
                    .copied()
                    .and_then(crate::tab_model::View::terminal_session)
                    .and_then(|session| app.pool.get(session))
                    .and_then(|session| session.handoff_local_id)
                else {
                    continue;
                };
                assert!(
                    tab.presentation
                        .tooltip
                        .as_deref()
                        .is_some_and(|tip| tip.starts_with(&format!("🐚{shell}"))),
                    "{shape}: shell {shell}'s tab tooltip {:?}",
                    tab.presentation.tooltip
                );
            }
        }
    }

    /// EVERY SHELL COMES BACK IN THE PANE ITS LEAF NAMES, WEARING THAT LEAF'S
    /// IDENTITY, AND NO STAND-IN IS SPAWNED. When window 0 has no terminal leaf,
    /// `main_entry` once adopted the first shell the handoff listed as session 0
    /// anyway. That shell's own leaf lies in a later window, whose rebuild found
    /// it taken and spawned a FRESH shell in its pane — a stand-in, with a new
    /// sid and no history — while the shell itself stayed in window 0 as an
    /// extra tab. The stand-in was also seeded with the leaf's identity
    /// (`role=` included, which peers read before typing) while the shell that
    /// identity describes came back bare; when the leaf was its window's first
    /// pane, the graft refused the stand-in and the identity reached nobody.
    /// `take_session0_shell` now adopts nothing for such a window 0: session 0
    /// is a fresh bootstrap its native-only rebuild retires, and each shell is
    /// adopted by its own leaf. The third shape — a native first tab with the
    /// terminal leaf later in the same window — was always right; it is the
    /// control.
    ///
    /// HOW IT FAILS WITHOUT THE FIXES, as measured. With the old pick
    /// ([`first_listed_session0_shell`]) in place of `take_session0_shell`, the
    /// first two shapes fail the placement check: shell 0 is an extra tab of
    /// window 0 and a stand-in fills its pane
    /// (`a_handoff_keeps_each_identity_on_its_shell_when_a_leaf_names_a_shell_running_elsewhere`
    /// keeps those shapes, to hold the identity rule there). Further back —
    /// against the code before the identity fix, whose headless stub never
    /// adopted from `seamless_adopt` — this test's first form fails at its
    /// PRECONDITION that the layout placed every handed-off shell, not at an
    /// identity check, since every stub stands in for its shell (as the review
    /// of that fix measured). The identity failures described above were
    /// measured with the stub adopting and the old identity rule.
    #[test]
    fn a_handoff_carries_each_identity_onto_the_shell_its_leaf_names_and_never_a_stand_in() {
        let shapes = [
            (
                "window 0 is one native tab; shell 0's leaf is a later pane of window 1",
                vec![
                    window_of(vec![settings_leaf()]),
                    window_of(vec![
                        split_of(leaf_naming(2), leaf_naming(0)),
                        leaf_naming(1),
                    ]),
                ],
            ),
            (
                "window 0 is one native tab; shell 0's leaf is window 1's first pane",
                vec![
                    window_of(vec![settings_leaf()]),
                    window_of(vec![
                        leaf_naming(0),
                        split_of(leaf_naming(1), leaf_naming(2)),
                    ]),
                ],
            ),
            (
                "window 0 opens on a native tab and has its terminal leaf later",
                vec![
                    window_of(vec![
                        settings_leaf(),
                        split_of(leaf_naming(1), leaf_naming(2)),
                    ]),
                    window_of(vec![leaf_naming(0)]),
                ],
            ),
        ];
        for (shape, windows) in shapes {
            // Where the layout puts each shell: the pane its leaf names.
            let named = windows
                .iter()
                .map(|window| {
                    window
                        .restored_tabs
                        .iter()
                        .map(|tab| {
                            let mut leaves = Vec::new();
                            let mut stack = vec![&tab.root];
                            while let Some(node) = stack.pop() {
                                match node {
                                    restore::RestoredSplitTree::Leaf {
                                        view: restore::RestoredView::Terminal(leaf),
                                    } => leaves.push(Some(leaf.local_id)),
                                    restore::RestoredSplitTree::Leaf { .. } => leaves.push(None),
                                    restore::RestoredSplitTree::Split { first, second, .. } => {
                                        stack.push(second);
                                        stack.push(first);
                                    }
                                }
                            }
                            leaves
                        })
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();
            let new = successor_of(windows, 3, |handed_off, layout| {
                super::take_session0_shell(handed_off, Some(layout))
            });

            assert_eq!(
                shown_shells(&new),
                named,
                "{shape}: every shell is in the pane its leaf names, and no window holds a \
                 pane or tab the layout did not"
            );
            let mut adopted = Vec::new();
            for (&session, pooled) in &new.pool.sessions {
                let shell = pooled.session.handoff_local_id.unwrap_or_else(|| {
                    panic!("{shape}: session {session} is no handed-off shell — a stand-in")
                });
                adopted.push(shell);
                assert_eq!(
                    registry_meta(&new, session),
                    identity_of(shell),
                    "{shape}: shell {shell} (session {session}) wears exactly the identity of \
                     the leaf that names it"
                );
            }
            adopted.sort_unstable();
            assert_eq!(adopted, vec![0, 1, 2], "{shape}: every shell is live");
            assert_tooltips_name_their_shells(&new, shape);
            assert!(new.structural_invariants_ok(), "{shape}");
        }

        // With no layout to place by, session 0 still takes a shell.
        let mut handed_off = (0..3).map(handed_off_shell).collect::<Vec<_>>();
        assert_eq!(
            super::take_session0_shell(&mut handed_off, None).map(|shell| shell.local_id),
            Some(0)
        );
        assert_eq!(handed_off.len(), 2);
    }

    /// A LEAF NAMING A SHELL THAT RUNS ELSEWHERE GIVES ITS IDENTITY TO THAT
    /// SHELL, NOT TO THE STAND-IN FILLING ITS PANE. An honored handoff layout
    /// never reaches this: each shell is adopted by its own leaf or as its own
    /// window's bootstrap. The old session-0 pick reached it
    /// ([`first_listed_session0_shell`]), and a tab that fails to build hands its
    /// window's bootstrap on to another shell's pane; `carry_restored_identity`
    /// must stay right there. Session 0 is shell 0, kept as an extra tab in
    /// native-only window 0 because it IS a handed-off shell; its own pane is
    /// filled by a stand-in, whose identity it takes through the live door, and
    /// window 0's tab chrome repaints to show it.
    #[test]
    fn a_handoff_keeps_each_identity_on_its_shell_when_a_leaf_names_a_shell_running_elsewhere() {
        use crate::session_timeline::SessionMeta;

        for (shape, windows) in [
            (
                "shell 0's leaf is a later pane of window 1",
                vec![
                    window_of(vec![settings_leaf()]),
                    window_of(vec![
                        split_of(leaf_naming(2), leaf_naming(0)),
                        leaf_naming(1),
                    ]),
                ],
            ),
            (
                "shell 0's leaf is window 1's first pane",
                vec![
                    window_of(vec![settings_leaf()]),
                    window_of(vec![
                        leaf_naming(0),
                        split_of(leaf_naming(1), leaf_naming(2)),
                    ]),
                ],
            ),
        ] {
            let new = successor_of(windows, 3, first_listed_session0_shell);
            assert_eq!(
                new.pool.get(0).and_then(|session| session.handoff_local_id),
                Some(0),
                "PRECONDITION {shape}: session 0 is shell 0"
            );
            let mut stand_ins = Vec::new();
            for (&session, pooled) in &new.pool.sessions {
                match pooled.session.handoff_local_id {
                    Some(shell) => assert_eq!(
                        registry_meta(&new, session),
                        identity_of(shell),
                        "{shape}: shell {shell} (session {session}) wears exactly the identity \
                         of the leaf that names it"
                    ),
                    None => {
                        stand_ins.push(session);
                        assert_eq!(
                            registry_meta(&new, session),
                            SessionMeta::default(),
                            "{shape}: stand-in session {session} is no handed-off shell, so \
                             it wears no identity"
                        );
                    }
                }
            }
            assert_eq!(stand_ins.len(), 1, "{shape}: stand-ins {stand_ins:?}");
            // Window 0 was rebuilt before shell 0's identity arrived; its tab
            // shows it all the same.
            assert_tooltips_name_their_shells(&new, shape);
            assert!(new.structural_invariants_ok(), "{shape}");
        }
    }

    /// A SHELL THE REBUILD LEAVES OVER STILL COMES BACK WEARING ITS IDENTITY.
    /// The orphan net re-homes every handed-off shell no leaf took as a tab of
    /// the front window, so none is lost. Such a shell is left over because the
    /// leaf naming it never rebuilt — its tab failed to build, or, as here, its
    /// window could not be created and `apply_restore_manifest` stopped — or
    /// was filled by another shell. Its identity reached nobody during the
    /// rebuild, and the net used to place it bare: the manifest was consumed by
    /// then. The leaves naming handed-off shells now outlive it
    /// (`handed_off_leaves`), and the net puts each one's identity on its
    /// shell before registering it.
    #[test]
    fn an_orphaned_shell_comes_back_wearing_the_identity_of_the_leaf_that_names_it() {
        let wire = restore::RestoreManifest::new(vec![
            window_of(vec![leaf_naming(0)]),
            window_of(vec![
                leaf_naming(1),
                split_of(leaf_naming(2), leaf_naming(3)),
            ]),
        ])
        .to_toml()
        .expect("the parent serializes its layout");
        let carried = restore::RestoreManifest::from_toml(&wire)
            .filter(|layout| layout.covers_exact_seamless_ids(&[0, 1, 2, 3]))
            .expect("the child accepts the sidecar");
        let mut new = App::headless_for_test();
        new.handoff_successor = true;
        let mut handed_off = (0..4).map(handed_off_shell).collect();
        let shell0 = super::take_session0_shell(&mut handed_off, Some(&carried))
            .expect("window 0's bootstrap leaf names shell 0");
        adopt_as(&mut new, 0, shell0.local_id);
        new.seamless_adopt = handed_off;
        let leaves = carried.handed_off_leaves();
        // Window 0 rebuilds; window 1 is never created.
        new.restore_into_window(WindowId(0), carried.windows[0].clone());
        assert_eq!(
            new.seamless_adopt.len(),
            3,
            "PRECONDITION: window 1's shells are left over"
        );

        new.adopt_orphan_shells_as_tabs(&leaves);
        assert!(new.seamless_adopt.is_empty(), "the net placed every orphan");
        let mut adopted = Vec::new();
        for (&session, pooled) in &new.pool.sessions {
            let shell = pooled
                .session
                .handoff_local_id
                .expect("every session is a handed-off shell");
            adopted.push(shell);
            assert_eq!(
                registry_meta(&new, session),
                identity_of(shell),
                "shell {shell} (session {session}) wears the identity of the leaf that names it"
            );
        }
        adopted.sort_unstable();
        assert_eq!(adopted, vec![0, 1, 2, 3], "every shell is live");
        assert_eq!(
            shown_shells(&new),
            vec![vec![
                vec![Some(Some(0))],
                vec![Some(Some(1))],
                vec![Some(Some(2))],
                vec![Some(Some(3))],
            ]],
            "each orphan is a tab of its own in the front window"
        );
        assert_tooltips_name_their_shells(&new, "orphans");
        assert!(new.structural_invariants_ok());
    }

    /// THE BOOTSTRAP STARTS IN THE DIRECTORY OF THE PANE IT FILLS. A cold
    /// restore spawns session 0 in `first_leaf_cwd` before the deferred pass
    /// grafts it onto the canonical tree's first terminal leaf — the leaf
    /// `bootstrap_local_id` names. `first_leaf_cwd` read the first TAB, and on a
    /// native-only first tab fell back to the legacy `tabs` mirror, which lists
    /// all-terminal tabs only: behind a tab splitting a native view with a
    /// terminal, it named another tab's pane. A pane that recorded no directory
    /// starts where another pane of its OWN tab was — what that first-tab read
    /// did for a first tab — but never where another tab's was.
    #[test]
    fn the_bootstrap_cwd_is_the_directory_of_the_leaf_the_bootstrap_fills() {
        // PREDECESSOR: a Settings tab, a tab splitting a recovery placeholder
        // with session 0, then session 1's all-terminal tab.
        let mut old = App::headless_for_test();
        old.restore_into_window(
            WindowId(0),
            window_of(vec![
                settings_leaf(),
                split_of(
                    restore::RestoredSplitTree::leaf(restore::RestoredView::Placeholder(
                        restore::PlaceholderLeafRestore {
                            restore_tag: "markdown".to_string(),
                            reason: "Document could not be reopened".to_string(),
                            metadata: String::new(),
                        },
                    )),
                    restore::RestoredSplitTree::leaf(restore::RestoredView::Terminal(bare_leaf())),
                ),
                restore::RestoredSplitTree::leaf(restore::RestoredView::Terminal(bare_leaf())),
            ]),
        );
        let feed = |session: u64, bytes: &[u8]| {
            crate::term_lock(&old.pool.get(session).expect("live session").term).process(bytes);
        };
        feed(1, b"\x1b]7;file://localhost/aterm-proof/other-tab\x07");
        let carried = |app: &App| {
            restore::RestoreManifest::from_toml(
                &app.capture_restore_manifest()
                    .to_toml()
                    .expect("the layout serializes"),
            )
            .expect("the layout parses")
        };

        let manifest = carried(&old);
        let window = &manifest.windows[0];
        let mirror_first = window
            .tabs
            .first()
            .and_then(|tab| tab.leaves().first().copied())
            .map(|leaf| (leaf.local_id(), leaf.cwd().map(str::to_owned)));
        assert_eq!(
            (mirror_first, window.bootstrap_local_id()),
            (
                Some((Some(1), Some("/aterm-proof/other-tab".to_string()))),
                Some(0)
            ),
            "PRECONDITION: the mirror's first leaf is session 1's tab; the bootstrap fills \
             session 0's pane"
        );
        assert_eq!(
            manifest.first_leaf_cwd(),
            None,
            "no pane of session 0's tab recorded a directory, so the bootstrap starts in \
             the default one, not in session 1's tab's"
        );

        feed(0, b"\x1b]7;file://localhost/aterm-proof/split-pane\x07");
        let manifest = carried(&old);
        assert_eq!(
            manifest.first_leaf_cwd(),
            Some("/aterm-proof/split-pane"),
            "the bootstrap starts in the directory of the pane it fills"
        );
        assert_eq!(
            manifest.windows[0].bootstrap_cwd(),
            manifest.first_leaf_cwd(),
            "every window's bootstrap takes its cwd by the one pick"
        );

        // A pane that recorded no directory — its program sent no OSC 7, or
        // the capture found its terminal locked — takes the first directory
        // another terminal pane of its own tab recorded, in rebuild order.
        let pane_in = |cwd: Option<&str>| {
            restore::RestoredSplitTree::leaf(restore::RestoredView::Terminal(
                restore::TerminalLeafRestore {
                    cwd: cwd.map(str::to_owned),
                    ..bare_leaf()
                },
            ))
        };
        for (shape, window, expected) in [
            (
                "the bootstrap's pane recorded one",
                window_of(vec![split_of(pane_in(Some("/x")), pane_in(Some("/y")))]),
                Some("/x"),
            ),
            (
                "split first tab, its first pane recorded none",
                window_of(vec![split_of(pane_in(None), pane_in(Some("/y")))]),
                Some("/y"),
            ),
            (
                "nested split first tab, its first pane recorded none",
                window_of(vec![split_of(
                    split_of(pane_in(None), pane_in(Some("/y"))),
                    pane_in(Some("/z")),
                )]),
                Some("/y"),
            ),
            (
                "a native view first in the bootstrap's tab",
                window_of(vec![split_of(settings_leaf(), pane_in(Some("/y")))]),
                Some("/y"),
            ),
            (
                "no pane of the bootstrap's tab recorded one",
                window_of(vec![
                    split_of(pane_in(None), pane_in(None)),
                    pane_in(Some("/other-tab")),
                ]),
                None,
            ),
            (
                "behind a native-only first tab, still only the bootstrap's own tab",
                window_of(vec![
                    settings_leaf(),
                    split_of(settings_leaf(), pane_in(None)),
                    pane_in(Some("/other-tab")),
                ]),
                None,
            ),
        ] {
            let manifest = restore::RestoreManifest::new(vec![window]);
            assert_eq!(manifest.first_leaf_cwd(), expected, "{shape}");
        }
    }

    /// THE GRAFT IS A CHANGE A WATCHER CAN SEE. The grafted session is already
    /// registered and served under its sid when the deferred restore reaches
    /// it, so a `meta` reader or a `subscribe … events` watcher may already have
    /// seen it without its identity. Each field the graft moves lands on the
    /// session's timeline as the ordinary `meta-change` — what the digest pushes
    /// as `EVENT <sid> meta …` — and the watcher is woken for it.
    #[test]
    fn a_graft_announces_every_field_it_moves_to_an_events_watcher() {
        let mut app = App::headless_for_test();
        let ctx = app.pool.get(0).expect("the bootstrap").ctx.clone();
        // Where a `subscribe … events` digest seeds its watermark.
        let watermark = ctx.timeline.lock().unwrap().high_id();
        let watch = crate::subscribe::SubscriberSet::register(&app.subscribers, &[0]);
        while watch.wait(std::time::Duration::ZERO) {}

        let mut leaf = bare_leaf();
        leaf.user_title = Some("fable driver".to_string());
        leaf.role = Some("agent:claude-driver-fable".to_string());
        app.restore_into_window(WindowId(0), single_leaf_window(leaf));

        let pushed: Vec<String> = ctx
            .timeline
            .lock()
            .unwrap()
            .since(watermark)
            .filter(|event| event.kind == "meta-change")
            .map(|event| event.payload.clone())
            .collect();
        assert_eq!(
            pushed,
            vec![
                "field=title value=fable%20driver".to_string(),
                "field=role value=agent:claude-driver-fable".to_string(),
            ]
        );
        assert!(
            watch.wait(std::time::Duration::ZERO),
            "the session's `events` watcher was woken for them"
        );
    }
}
