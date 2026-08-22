// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! RESTORE-2: recursive terminal/native workspace restore across quit + relaunch.
//!
//! The persisted format is deliberately SEPARATE from `session_store::SessionHandoff`
//! (which hands live fds to a re-exec'd process for a seamless self-update — a
//! single-boot handoff that models no window/tab/pane tree and no cwd). This manifest is
//! the durable, human-inspectable layout written on graceful quit and applied on the
//! next cold launch: the window grid, each terminal tab's pane tree, and the stable
//! descriptors needed to reopen first-party native tabs. Native descriptors never
//! contain process-local ids, document bytes, config values, or secrets; bounded
//! source-addressed view selections and viewport anchors are persisted so a reopened
//! document returns to the same reading/editing position.
//!
//! Process-local `u64` session ids are NOT persisted — a Leaf stores the shell's cwd +
//! title so a FRESH session can be respawned in the same place, and the pane tree is
//! rebuilt with new ids in deterministic tree order (see `pane::PaneTree::rebuild`).
//!
//! Parsing is fail-safe: any I/O error, malformed TOML, or schema mismatch yields `None`
//! (fall back to a normal fresh window), and the manifest is single-use (deleted on read)
//! so a crash mid-restore can never loop.

use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// The current on-disk schema. Bumped on any incompatible layout change; a manifest with
/// a different schema is ignored (fresh start), never mis-parsed.
pub(crate) const SCHEMA: u32 = 2;
const LEGACY_SCHEMA: u32 = 1;

const MAX_WINDOWS: usize = 64;
const MAX_TABS_PER_WINDOW: usize = 256;
const MAX_DOCUMENT_URI_BYTES: usize = 8 * 1024;
const MAX_SETTINGS_ROUTE_BYTES: usize = 128;
const MAX_RESTORE_TAG_BYTES: usize = 128;
const MAX_LEAVES_PER_TAB: usize = 256;
const MAX_SPLIT_DEPTH: usize = 32;
const MAX_VIEW_METADATA_BYTES: usize = 16 * 1024;
const MAX_EDITOR_SELECTIONS: usize = 256;

/// serde-stable mirror of [`crate::pane::SplitDir`] — kept separate so the on-disk format
/// never couples to the internal enum's representation.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SplitKind {
    Vertical,
    Horizontal,
}

/// Stable path component used to identify focus and closed-view insertion points without
/// persisting a process-local [`crate::tab_model::ViewId`].
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RestoreBranch {
    First,
    Second,
}

/// One source-addressed selection. Byte offsets are clamped to the reopened document at
/// apply time; the persisted value can never become an unchecked slice index.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct RestoreSelection {
    pub anchor: usize,
    pub head: usize,
}

/// Stable metadata for a terminal split leaf. `local_id` is adoption-only and is ignored
/// on a cold launch; it is never treated as a durable session identity.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub(crate) struct TerminalLeafRestore {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default)]
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_id: Option<u64>,
    /// USER session metadata (session-metadata stage 1; additive, absent
    /// tolerated by older manifests): the operator's `meta set` title/
    /// description/icon/role/attention, captured at quit and RE-SEEDED onto
    /// the respawned session so restore keeps the operator-chosen identity
    /// (the `title` above is the engine/OSC one — a different datum).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention: Option<String>,
}

impl TerminalLeafRestore {
    /// Restore files survive across versions and are operator-writable. Normalize
    /// USER chrome fields before they can be re-seeded onto a live session; the
    /// control path's visible rejection cannot protect this persistence path.
    fn sanitize_user_metadata(&mut self) {
        self.user_title = self
            .user_title
            .as_deref()
            .and_then(|value| crate::session_timeline::sanitize_metadata_value("title", value));
        self.description = self.description.as_deref().and_then(|value| {
            crate::session_timeline::sanitize_metadata_value("description", value)
        });
        self.icon = self
            .icon
            .as_deref()
            .and_then(|value| crate::session_timeline::sanitize_metadata_value("icon", value));
        self.role = self
            .role
            .as_deref()
            .and_then(|value| crate::session_timeline::sanitize_metadata_value("role", value));
        self.attention = self
            .attention
            .as_deref()
            .and_then(|value| crate::session_timeline::sanitize_metadata_value("attention", value));
    }

    fn user_metadata_is_canonical(&self) -> bool {
        let canonical = |field: &str, value: &Option<String>| {
            value.as_ref().is_none_or(|value| {
                crate::session_timeline::sanitize_metadata_value(field, value).as_deref()
                    == Some(value.as_str())
            })
        };
        canonical("title", &self.user_title)
            && canonical("description", &self.description)
            && canonical("icon", &self.icon)
            && canonical("role", &self.role)
            && canonical("attention", &self.attention)
    }
}

/// Per-view native state. Canonical document bytes and draft contents deliberately do not
/// appear here: Editor durability is addressed by the document journal and `durable_seq`.
/// Unknown app versions may put a bounded, copyable description in `metadata`; it is data,
/// never executable input.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub(crate) struct NativeLeafRestore {
    pub restore_tag: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    /// This Editor is the canonical aterm.toml Manual surface. Additive for
    /// backward compatibility; ordinary Editor restores remain `false`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub config_editor: bool,
    #[serde(default)]
    pub source_anchor: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<RestoreSelection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub editor_selections: Vec<RestoreSelection>,
    #[serde(default)]
    pub primary_selection: usize,
    #[serde(default)]
    pub viewport_anchor: usize,
    #[serde(default)]
    pub durable_seq: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub metadata: String,
}

impl NativeLeafRestore {
    pub(crate) fn settings(route: String) -> Self {
        Self {
            restore_tag: "settings".to_string(),
            route: Some(route),
            uri: None,
            config_editor: false,
            source_anchor: 0,
            selection: None,
            editor_selections: Vec::new(),
            primary_selection: 0,
            viewport_anchor: 0,
            durable_seq: 0,
            metadata: String::new(),
        }
    }

    pub(crate) fn document(restore_tag: &str, uri: String) -> Self {
        Self {
            restore_tag: restore_tag.to_string(),
            route: None,
            uri: Some(uri),
            config_editor: false,
            source_anchor: 0,
            selection: None,
            editor_selections: Vec::new(),
            primary_selection: 0,
            viewport_anchor: 0,
            durable_seq: 0,
            metadata: String::new(),
        }
    }

    fn validation_error(&self) -> Option<&'static str> {
        if self.restore_tag.is_empty()
            || self.restore_tag.len() > MAX_RESTORE_TAG_BYTES
            || self.restore_tag.contains('\0')
        {
            return Some("invalid native restore tag");
        }
        if self.metadata.len() > MAX_VIEW_METADATA_BYTES || self.metadata.contains('\0') {
            return Some("invalid native restore metadata");
        }
        if self.editor_selections.len() > MAX_EDITOR_SELECTIONS
            || (!self.editor_selections.is_empty()
                && self.primary_selection >= self.editor_selections.len())
        {
            return Some("invalid editor selection state");
        }
        if self.config_editor && self.restore_tag != "editor" {
            return Some("config-editor identity requires an Editor restore");
        }
        match self.restore_tag.as_str() {
            "settings" => self
                .route
                .as_deref()
                .filter(|route| {
                    !route.is_empty()
                        && route.len() <= MAX_SETTINGS_ROUTE_BYTES
                        && route.starts_with('/')
                        && !route.contains('\0')
                })
                .is_none()
                .then_some("invalid Settings route"),
            "markdown" | "editor" => self
                .uri
                .as_deref()
                .filter(|uri| {
                    !uri.is_empty()
                        && uri.len() <= MAX_DOCUMENT_URI_BYTES
                        && uri.starts_with("file://")
                        && !uri.contains('\0')
                })
                .is_none()
                .then_some("invalid document URI"),
            _ => Some("native app is unavailable in this build"),
        }
    }

    fn copyable_metadata(&self) -> String {
        let mut metadata = format!(
            "restore_tag={:?}\nroute={:?}\nuri={:?}\nconfig_editor={}\nsource_anchor={}\nviewport_anchor={}\ndurable_seq={}",
            self.restore_tag,
            self.route,
            self.uri,
            self.config_editor,
            self.source_anchor,
            self.viewport_anchor,
            self.durable_seq
        );
        if !self.metadata.is_empty() {
            metadata.push_str("\nmetadata=");
            metadata.push_str(&self.metadata);
        }
        metadata.truncate(MAX_VIEW_METADATA_BYTES);
        metadata
    }
}

/// A recoverable leaf occupying the exact split position of an unavailable or malformed
/// app. It retains bounded metadata for diagnostics/copying and never falls back to a
/// shell command or silently deletes a healthy sibling.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub(crate) struct PlaceholderLeafRestore {
    pub restore_tag: String,
    pub reason: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub metadata: String,
}

/// Content descriptor at one recursive restore leaf.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum RestoredView {
    Terminal(TerminalLeafRestore),
    Native(NativeLeafRestore),
    Placeholder(PlaceholderLeafRestore),
}

impl RestoredView {
    fn sanitize(&mut self) {
        match self {
            Self::Terminal(terminal) => terminal.sanitize_user_metadata(),
            Self::Native(native) => {
                let Some(reason) = native.validation_error() else {
                    return;
                };
                *self = Self::Placeholder(PlaceholderLeafRestore {
                    restore_tag: native.restore_tag.clone(),
                    reason: reason.to_string(),
                    metadata: native.copyable_metadata(),
                });
            }
            Self::Placeholder(_) => {}
        }
    }

    fn bounded(&self) -> bool {
        match self {
            Self::Terminal(terminal) => {
                terminal
                    .cwd
                    .as_ref()
                    .is_none_or(|cwd| cwd.len() <= MAX_DOCUMENT_URI_BYTES && !cwd.contains('\0'))
                    && terminal.title.len() <= MAX_DOCUMENT_URI_BYTES
                    && !terminal.title.contains('\0')
                    && terminal.profile.as_ref().is_none_or(|profile| {
                        profile.len() <= MAX_SETTINGS_ROUTE_BYTES && !profile.contains('\0')
                    })
                    && terminal.user_metadata_is_canonical()
            }
            Self::Native(native) => native.validation_error().is_none(),
            Self::Placeholder(placeholder) => {
                !placeholder.restore_tag.is_empty()
                    && placeholder.restore_tag.len() <= MAX_RESTORE_TAG_BYTES
                    && !placeholder.restore_tag.contains('\0')
                    && !placeholder.reason.is_empty()
                    && placeholder.reason.len() <= MAX_SETTINGS_ROUTE_BYTES
                    && !placeholder.reason.contains('\0')
                    && placeholder.metadata.len() <= MAX_VIEW_METADATA_BYTES
                    && !placeholder.metadata.contains('\0')
            }
        }
    }
}

/// Recursive, content-agnostic persisted split tree.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(tag = "node", rename_all = "snake_case")]
pub(crate) enum RestoredSplitTree {
    Leaf {
        view: RestoredView,
    },
    Split {
        axis: SplitKind,
        ratio: f32,
        first: Box<RestoredSplitTree>,
        second: Box<RestoredSplitTree>,
    },
}

impl RestoredSplitTree {
    pub(crate) fn leaf(view: RestoredView) -> Self {
        Self::Leaf { view }
    }

    pub(crate) fn leaf_count(&self) -> usize {
        match self {
            Self::Leaf { .. } => 1,
            Self::Split { first, second, .. } => first.leaf_count() + second.leaf_count(),
        }
    }

    fn depth(&self) -> usize {
        match self {
            Self::Leaf { .. } => 1,
            Self::Split { first, second, .. } => 1 + first.depth().max(second.depth()),
        }
    }

    fn sanitize(&mut self) {
        match self {
            Self::Leaf { view } => view.sanitize(),
            Self::Split {
                ratio,
                first,
                second,
                ..
            } => {
                if !ratio.is_finite() {
                    *ratio = 0.5;
                } else {
                    *ratio = ratio.clamp(0.05, 0.95);
                }
                first.sanitize();
                second.sanitize();
            }
        }
    }

    fn shape_is_bounded(&self) -> bool {
        self.leaf_count() <= MAX_LEAVES_PER_TAB
            && self.depth() <= MAX_SPLIT_DEPTH
            && match self {
                Self::Leaf { view } => view.bounded(),
                Self::Split {
                    ratio,
                    first,
                    second,
                    ..
                } => {
                    ratio.is_finite()
                        && (0.05..=0.95).contains(ratio)
                        && first.shape_is_bounded()
                        && second.shape_is_bounded()
                }
            }
    }

    fn first_terminal_cwd(&self) -> Option<&str> {
        match self {
            Self::Leaf {
                view: RestoredView::Terminal(terminal),
            } => terminal.cwd.as_deref(),
            Self::Leaf { .. } => None,
            Self::Split { first, second, .. } => first
                .first_terminal_cwd()
                .or_else(|| second.first_terminal_cwd()),
        }
    }
}

/// One complete tab recovery record. Focus and zoom are properties of the container,
/// never duplicated into each leaf.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub(crate) struct RestoredTab {
    pub root: RestoredSplitTree,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub focused_path: Vec<RestoreBranch>,
    #[serde(default)]
    pub zoomed: bool,
}

impl RestoredTab {
    fn sanitize(&mut self) {
        self.root.sanitize();
    }

    fn shape_is_valid(&self) -> bool {
        self.focused_path.len() <= MAX_SPLIT_DEPTH
            && self.root.shape_is_bounded()
            && self.path_names_a_leaf()
    }

    fn path_names_a_leaf(&self) -> bool {
        let mut node = &self.root;
        for branch in &self.focused_path {
            let RestoredSplitTree::Split { first, second, .. } = node else {
                return false;
            };
            node = match branch {
                RestoreBranch::First => first,
                RestoreBranch::Second => second,
            };
        }
        matches!(node, RestoredSplitTree::Leaf { .. })
    }
}

/// One node of a persisted tab pane tree. A `Leaf` drops the process-local session id and
/// keeps the shell's `cwd` + `title` (so the session can be respawned there); a `Split`
/// preserves the divider direction + ratio.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum PaneLayout {
    Leaf {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(default)]
        title: String,
        /// The one leaf that held keyboard focus in its tab.
        #[serde(default)]
        focused: bool,
        /// The outgoing session's pool id, captured ONLY so a SEAMLESS-update boot can
        /// re-adopt the live shell for this pane (matched to its handed-off fd by id)
        /// instead of forking a fresh one. A durable cold-quit restore ignores it (there
        /// is no live shell to adopt, so every leaf forks fresh) — it is purely the
        /// layout↔live-fd bridge for the self-update handoff. Additive/optional: absent
        /// in older manifests, harmless when unmatched.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        local_id: Option<u64>,
    },
    Split {
        dir: SplitKind,
        ratio: f32,
        first: Box<PaneLayout>,
        second: Box<PaneLayout>,
    },
}

impl PaneLayout {
    /// A plain single-pane leaf — a test-fixture convenience (production capture goes
    /// through [`crate::pane::PaneTree::to_layout`], which builds variants directly).
    #[cfg(test)]
    pub(crate) fn leaf(cwd: Option<String>, title: String, focused: bool) -> Self {
        PaneLayout::Leaf {
            cwd,
            title,
            focused,
            local_id: None,
        }
    }

    /// The number of Leaf panes in this subtree (== the sessions to respawn).
    pub(crate) fn leaf_count(&self) -> usize {
        match self {
            PaneLayout::Leaf { .. } => 1,
            PaneLayout::Split { first, second, .. } => first.leaf_count() + second.leaf_count(),
        }
    }

    /// Every `Leaf` in this subtree, in tree order (left-to-right / top-to-bottom) —
    /// the SAME order [`crate::pane::PaneTree::rebuild`] assigns fresh session ids, so
    /// `leaves()[i]`'s cwd is the spawn cwd for `fresh[i]`.
    pub(crate) fn leaves(&self) -> Vec<&PaneLayout> {
        let mut out = Vec::with_capacity(self.leaf_count());
        self.collect_leaves(&mut out);
        out
    }

    fn collect_leaves<'a>(&'a self, out: &mut Vec<&'a PaneLayout>) {
        match self {
            PaneLayout::Leaf { .. } => out.push(self),
            PaneLayout::Split { first, second, .. } => {
                first.collect_leaves(out);
                second.collect_leaves(out);
            }
        }
    }

    /// This node's persisted cwd — `None` for a `Split` (only leaves carry one).
    pub(crate) fn cwd(&self) -> Option<&str> {
        match self {
            PaneLayout::Leaf { cwd, .. } => cwd.as_deref(),
            PaneLayout::Split { .. } => None,
        }
    }

    /// This leaf's outgoing session pool id, when captured for a seamless-update handoff
    /// (`None` for a `Split`, and for any cold-quit leaf). The boot adoption matches this
    /// to a handed-off live fd to re-adopt the running shell into this exact pane.
    pub(crate) fn local_id(&self) -> Option<u64> {
        match self {
            PaneLayout::Leaf { local_id, .. } => *local_id,
            PaneLayout::Split { .. } => None,
        }
    }
}

/// Durable identity of one native tab. Titles and indicators are deliberately absent:
/// the live app derives them again from the Settings route or canonical document grant.
/// Likewise, no per-view selection, scroll position, editor draft bytes, or async target
/// identity crosses a process boundary.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum NativeTabRestore {
    Settings { route: String },
    Markdown { uri: String },
    Editor { uri: String },
}

impl NativeTabRestore {
    #[cfg(test)]
    pub(crate) fn document_uri(&self) -> Option<&str> {
        match self {
            Self::Settings { .. } => None,
            Self::Markdown { uri } | Self::Editor { uri } => Some(uri),
        }
    }

    fn shape_is_valid(&self) -> bool {
        match self {
            Self::Settings { route } => {
                !route.is_empty()
                    && route.len() <= MAX_SETTINGS_ROUTE_BYTES
                    && route.starts_with('/')
                    && !route.contains('\0')
            }
            Self::Markdown { uri } | Self::Editor { uri } => {
                !uri.is_empty()
                    && uri.len() <= MAX_DOCUMENT_URI_BYTES
                    && uri.starts_with("file://")
                    && !uri.contains('\0')
            }
        }
    }
}

/// Canonical chrome order without persisting any live [`crate::tab_model::TabId`]. Each
/// entry is an index into the matching compatibility list in [`WindowLayout`].
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum TabOrderEntry {
    Terminal { index: usize },
    Native { index: usize },
}

/// One window's persisted state: its size, position, the active tab index, and one pane
/// tree per tab.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub(crate) struct WindowLayout {
    pub rows: u16,
    pub cols: u16,
    #[serde(default)]
    pub active_tab: usize,
    /// Outer window position (physical px), when known — so a reopened window (cold
    /// restore or a seamless update) comes back exactly where it was instead of at the OS
    /// cascade default. Additive/optional: absent in older manifests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outer_x: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outer_y: Option<i32>,
    /// Whether the window was MAXIMIZED at capture time, so quitting maximized
    /// reopens maximized instead of as a floating default-size frame. When this is
    /// `Some(true)` the Windows capture stores `rcNormalPosition` — the OS's own
    /// restore-down rect — in `outer_x`/`outer_y` rather than the maximized frame's
    /// origin (see `capture_restore_manifest`), so un-maximizing after a restore
    /// lands where un-maximizing before the quit would have. Additive/optional
    /// (`Option`, not `bool`): older manifests parse as `None` = "unknown", which
    /// restore treats exactly like `Some(false)` — never force a state change off
    /// a manifest that predates the field. Currently written on Windows only; the
    /// macOS strip keeps its zoom semantics untouched and the Unix seamless
    /// commit's topology equality never sees a `Some` (belt-and-braces normalized
    /// there anyway, next to `outer_x`/`outer_y`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximized: Option<bool>,
    /// Terminal-only compatibility projection. Kept byte-compatible with RESTORE-1.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tabs: Vec<PaneLayout>,
    /// Stable native app descriptors. No title or process-local identity is persisted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub native_tabs: Vec<NativeTabRestore>,
    /// Canonical interleaving of `tabs` and `native_tabs`. An absent list is the legacy
    /// terminal-only order, allowing pre-native manifests to decode unchanged.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tab_order: Vec<TabOrderEntry>,
    /// Canonical active chrome index. `active_tab` remains the legacy terminal-projection
    /// active index; this additive field disambiguates mixed windows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_item: Option<usize>,
    /// RESTORE-2 canonical tabs. The v1 compatibility projections above remain during
    /// the terminal-host migration and allow a rolling seamless update to decode the
    /// layout; new runtime code must prefer this recursive representation when present.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub restored_tabs: Vec<RestoredTab>,
}

impl WindowLayout {
    /// Validated canonical order. Legacy terminal-only manifests synthesize the identity
    /// order. Duplicate/out-of-range indices fail closed rather than aliasing descriptors.
    pub(crate) fn canonical_order(&self) -> Option<Vec<TabOrderEntry>> {
        if self.tab_order.is_empty() {
            return self.native_tabs.is_empty().then(|| {
                (0..self.tabs.len())
                    .map(|index| TabOrderEntry::Terminal { index })
                    .collect()
            });
        }
        if self.tab_order.len() != self.tabs.len().saturating_add(self.native_tabs.len()) {
            return None;
        }
        let mut terminals = vec![false; self.tabs.len()];
        let mut natives = vec![false; self.native_tabs.len()];
        let mut next_terminal = 0usize;
        let mut next_native = 0usize;
        for entry in &self.tab_order {
            let (seen, index) = match *entry {
                TabOrderEntry::Terminal { index } => {
                    if index != next_terminal {
                        return None;
                    }
                    next_terminal = next_terminal.saturating_add(1);
                    (&mut terminals, index)
                }
                TabOrderEntry::Native { index } => {
                    if index != next_native {
                        return None;
                    }
                    next_native = next_native.saturating_add(1);
                    (&mut natives, index)
                }
            };
            let slot = seen.get_mut(index)?;
            if std::mem::replace(slot, true) {
                return None;
            }
        }
        (terminals.into_iter().all(|seen| seen) && natives.into_iter().all(|seen| seen))
            .then(|| self.tab_order.clone())
    }

    /// Active canonical index, migrating the legacy terminal projection when needed.
    pub(crate) fn canonical_active(&self, order: &[TabOrderEntry]) -> usize {
        self.active_item
            .filter(|index| *index < order.len())
            .or_else(|| {
                order.iter().position(|entry| {
                    matches!(entry, TabOrderEntry::Terminal { index } if *index == self.active_tab)
                })
            })
            .unwrap_or(0)
    }

    fn shape_is_valid(&self) -> bool {
        let native_descriptors_are_unique = self
            .native_tabs
            .iter()
            .enumerate()
            .all(|(index, descriptor)| !self.native_tabs[..index].contains(descriptor));
        let settings_count = self
            .native_tabs
            .iter()
            .filter(|descriptor| matches!(descriptor, NativeTabRestore::Settings { .. }))
            .count();
        let canonical_count = if self.restored_tabs.is_empty() {
            self.tabs.len().saturating_add(self.native_tabs.len())
        } else {
            self.restored_tabs.len()
        };
        canonical_count <= MAX_TABS_PER_WINDOW
            && self
                .native_tabs
                .iter()
                .all(NativeTabRestore::shape_is_valid)
            && native_descriptors_are_unique
            && settings_count <= 1
            && self.canonical_order().is_some()
            && self.active_item.is_none_or(|index| index < canonical_count)
            && self.restored_tabs.iter().all(RestoredTab::shape_is_valid)
    }

    fn migrate_legacy_tabs(&mut self) -> Option<()> {
        if !self.restored_tabs.is_empty() {
            return Some(());
        }
        let order = self.canonical_order()?;
        self.restored_tabs = order
            .iter()
            .map(|entry| match *entry {
                TabOrderEntry::Terminal { index } => {
                    RestoredTab::from_legacy_terminal(self.tabs.get(index)?)
                }
                TabOrderEntry::Native { index } => {
                    RestoredTab::from_legacy_native(self.native_tabs.get(index)?)
                }
            })
            .collect::<Option<Vec<_>>>()?;
        Some(())
    }
}

impl RestoredTab {
    fn from_legacy_terminal(layout: &PaneLayout) -> Option<Self> {
        fn convert(
            layout: &PaneLayout,
            path: &mut Vec<RestoreBranch>,
            focused: &mut Option<Vec<RestoreBranch>>,
        ) -> RestoredSplitTree {
            match layout {
                PaneLayout::Leaf {
                    cwd,
                    title,
                    focused: is_focused,
                    local_id,
                } => {
                    if *is_focused && focused.is_none() {
                        *focused = Some(path.clone());
                    }
                    RestoredSplitTree::leaf(RestoredView::Terminal(TerminalLeafRestore {
                        cwd: cwd.clone(),
                        title: title.clone(),
                        profile: None,
                        local_id: *local_id,
                        user_title: None,
                        description: None,
                        icon: None,
                        role: None,
                        attention: None,
                    }))
                }
                PaneLayout::Split {
                    dir,
                    ratio,
                    first,
                    second,
                } => {
                    path.push(RestoreBranch::First);
                    let first = convert(first, path, focused);
                    path.pop();
                    path.push(RestoreBranch::Second);
                    let second = convert(second, path, focused);
                    path.pop();
                    RestoredSplitTree::Split {
                        // Legacy PaneLayout names the divider orientation, while the
                        // recursive model names the child-placement axis.
                        axis: match dir {
                            SplitKind::Vertical => SplitKind::Horizontal,
                            SplitKind::Horizontal => SplitKind::Vertical,
                        },
                        ratio: *ratio,
                        first: Box::new(first),
                        second: Box::new(second),
                    }
                }
            }
        }

        let mut focused = None;
        let root = convert(layout, &mut Vec::new(), &mut focused);
        Some(Self {
            root,
            focused_path: focused.unwrap_or_default(),
            zoomed: false,
        })
    }

    pub(crate) fn from_legacy_native(descriptor: &NativeTabRestore) -> Option<Self> {
        let native = match descriptor {
            NativeTabRestore::Settings { route } => NativeLeafRestore::settings(route.clone()),
            NativeTabRestore::Markdown { uri } => {
                NativeLeafRestore::document("markdown", uri.clone())
            }
            NativeTabRestore::Editor { uri } => NativeLeafRestore::document("editor", uri.clone()),
        };
        Some(Self {
            root: RestoredSplitTree::leaf(RestoredView::Native(native)),
            focused_path: Vec::new(),
            zoomed: false,
        })
    }
}

/// The persisted session-restore manifest.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub(crate) struct RestoreManifest {
    pub schema: u32,
    pub windows: Vec<WindowLayout>,
}

impl RestoreManifest {
    pub(crate) fn new(mut windows: Vec<WindowLayout>) -> Self {
        for window in &mut windows {
            let _ = window.migrate_legacy_tabs();
            for tab in &mut window.restored_tabs {
                tab.sanitize();
            }
        }
        Self {
            schema: SCHEMA,
            windows,
        }
    }

    /// Whether there is anything worth restoring (at least one window with one tab).
    pub(crate) fn is_empty(&self) -> bool {
        self.windows
            .iter()
            .all(|w| w.tabs.is_empty() && w.native_tabs.is_empty() && w.restored_tabs.is_empty())
    }

    /// Return the exact set of live terminal ids carried by a seamless-update
    /// layout. Durable cold-restore manifests intentionally omit these ids, so
    /// they are not valid handoff authority. A duplicate or missing id is also
    /// rejected: either would make one inherited PTY ambiguous or leave a
    /// terminal leaf backed by a newly spawned shell during the overlap.
    pub(crate) fn seamless_terminal_ids(&self) -> Option<Vec<u64>> {
        fn collect(node: &RestoredSplitTree, ids: &mut Vec<u64>) -> Option<()> {
            match node {
                RestoredSplitTree::Leaf {
                    view: RestoredView::Terminal(terminal),
                } => ids.push(terminal.local_id?),
                RestoredSplitTree::Leaf { .. } => {}
                RestoredSplitTree::Split { first, second, .. } => {
                    collect(first, ids)?;
                    collect(second, ids)?;
                }
            }
            Some(())
        }

        let mut ids = Vec::new();
        for window in &self.windows {
            for tab in &window.restored_tabs {
                collect(&tab.root, &mut ids)?;
            }
        }
        ids.sort_unstable();
        if ids.windows(2).any(|pair| pair[0] == pair[1]) {
            return None;
        }
        Some(ids)
    }

    /// True only when this layout names every authenticated inherited terminal
    /// exactly once and names no additional terminal. Native leaves are allowed:
    /// they have no PTY and are independently restored from bounded descriptors.
    pub(crate) fn covers_exact_seamless_ids(&self, expected: &[u64]) -> bool {
        let mut expected = expected.to_vec();
        expected.sort_unstable();
        !expected.windows(2).any(|pair| pair[0] == pair[1])
            && self.seamless_terminal_ids().as_deref() == Some(expected.as_slice())
    }

    /// The cwd of the FIRST window's FIRST tab's first (tree-order) leaf — the pane
    /// the bootstrap session 0 becomes, so its spawn can start in the right directory
    /// (every other leaf is spawned later by `apply_pending_restore` with its own cwd).
    pub(crate) fn first_leaf_cwd(&self) -> Option<&str> {
        let first = self.windows.first()?;
        if let Some(tab) = first.restored_tabs.first()
            && let Some(cwd) = tab.root.first_terminal_cwd()
        {
            return Some(cwd);
        }
        first.tabs.first()?.leaves().first()?.cwd()
    }

    pub(crate) fn to_toml(&self) -> Result<String, String> {
        toml::to_string(self).map_err(|e| format!("serialize restore manifest: {e}"))
    }

    /// FAIL-SAFE parse: any TOML error or schema mismatch yields `None`, so a corrupt or
    /// old-version manifest never blocks launch — the app just starts fresh.
    pub(crate) fn from_toml(s: &str) -> Option<Self> {
        let mut manifest: Self = toml::from_str(s).ok()?;
        if manifest.schema != LEGACY_SCHEMA && manifest.schema != SCHEMA {
            return None;
        }
        if manifest.windows.len() > MAX_WINDOWS {
            return None;
        }
        for window in &mut manifest.windows {
            window.migrate_legacy_tabs()?;
            for tab in &mut window.restored_tabs {
                tab.sanitize();
            }
        }
        if !manifest.windows.iter().all(WindowLayout::shape_is_valid) {
            return None;
        }
        manifest.schema = SCHEMA;
        Some(manifest)
    }
}

/// The manifest path: `<data_dir>/aterm/session.toml`. `None` when no data dir resolves
/// (e.g. wasm / a stripped environment).
pub(crate) fn manifest_path() -> Option<PathBuf> {
    aterm_types::dirs::data_dir().map(|d| d.join("aterm").join("session.toml"))
}

/// Durably write `manifest` under the process-shared restore lock. Publication
/// uses a unique create-new sibling, file sync, atomic replacement, and directory
/// sync; a fixed `.tmp` name can neither collide nor be truncated.
pub(crate) fn write_to(path: &Path, manifest: &RestoreManifest) -> Result<(), String> {
    let toml = manifest.to_toml()?;
    let parent = path
        .parent()
        .ok_or_else(|| format!("restore manifest {} has no parent", path.display()))?;
    fs::create_dir_all(parent).map_err(|error| format!("mkdir {}: {error}", parent.display()))?;
    with_restore_lock(path, || write_restore_locked(path, toml.as_bytes()))
}

/// Claim the manifest atomically, durably remove its public name, then parse the
/// claimed bytes. The rename is the single-use commit point: a crash during
/// parsing/apply cannot expose the same manifest to the next launch.
///
/// `None` means absent, consumed-but-invalid, or an I/O failure. Failures are
/// logged because this compatibility API intentionally falls back to a fresh
/// window rather than blocking launch.
pub(crate) fn take_from(path: &Path) -> Option<RestoreManifest> {
    match take_from_result(path) {
        Ok(manifest) => manifest,
        Err(error) => {
            eprintln!("aterm-gui: restore manifest not consumed: {error}");
            None
        }
    }
}

fn write_restore_locked(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("restore manifest {} has no parent", path.display()))?;
    let name = path
        .file_name()
        .unwrap_or_else(|| OsStr::new("session.toml"));
    let (temporary, mut file) = create_unique_restore_sibling(parent, name, "write")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("protect {}: {error}", temporary.display()))?;
    }
    let result = (|| {
        file.write_all(bytes)
            .map_err(|error| format!("write {}: {error}", temporary.display()))?;
        file.sync_all()
            .map_err(|error| format!("sync {}: {error}", temporary.display()))?;
        drop(file);
        replace_restore_file(&temporary, path)
            .map_err(|error| format!("publish {}: {error}", path.display()))?;
        sync_restore_directory(parent).map_err(|error| {
            format!(
                "{} was published but directory durability is indeterminate: {error}",
                path.display()
            )
        })
    })();
    if temporary.exists() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn take_from_result(path: &Path) -> Result<Option<RestoreManifest>, String> {
    let Some(parent) = path.parent() else {
        return Err(format!("restore manifest {} has no parent", path.display()));
    };
    if !parent.exists() {
        return Ok(None);
    }
    with_restore_lock(path, || {
        if !path.exists() {
            return Ok(None);
        }
        let name = path
            .file_name()
            .unwrap_or_else(|| OsStr::new("session.toml"));
        let claim = unique_restore_sibling_path(parent, name, "claimed")?;
        claim_restore_file(path, &claim)
            .map_err(|error| format!("claim {}: {error}", path.display()))?;
        sync_restore_directory(parent).map_err(|error| {
            format!(
                "{} was claimed but single-use durability is indeterminate: {error}",
                path.display()
            )
        })?;

        let parsed = match fs::read_to_string(&claim) {
            Ok(text) => RestoreManifest::from_toml(&text),
            Err(error) => {
                eprintln!(
                    "aterm-gui: claimed restore read {} failed: {error}",
                    claim.display()
                );
                None
            }
        };
        if let Err(error) = fs::remove_file(&claim) {
            eprintln!(
                "aterm-gui: claimed restore cleanup {} failed: {error}",
                claim.display()
            );
        } else if let Err(error) = sync_restore_directory(parent) {
            eprintln!(
                "aterm-gui: claimed restore cleanup sync {} failed: {error}",
                parent.display()
            );
        }
        Ok(parsed)
    })
}

fn with_restore_lock<T>(
    path: &Path,
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("restore manifest {} has no parent", path.display()))?;
    let name = path
        .file_name()
        .unwrap_or_else(|| OsStr::new("session.toml"));
    let lock_path = parent.join(format!(".{}.aterm-restore.lock", name.to_string_lossy()));
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|error| format!("open restore lock {}: {error}", lock_path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        lock.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("protect restore lock {}: {error}", lock_path.display()))?;
    }
    lock.lock()
        .map_err(|error| format!("lock restore manifest {}: {error}", path.display()))?;
    operation()
}

fn create_unique_restore_sibling(
    parent: &Path,
    name: &OsStr,
    purpose: &str,
) -> Result<(PathBuf, File), String> {
    let seed = next_restore_seed();
    for attempt in 0_u32..64 {
        let candidate = restore_sibling_path(parent, name, purpose, seed, attempt);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => return Ok((candidate, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("create {}: {error}", candidate.display())),
        }
    }
    Err("could not allocate a unique restore temporary".to_string())
}

fn unique_restore_sibling_path(
    parent: &Path,
    name: &OsStr,
    purpose: &str,
) -> Result<PathBuf, String> {
    let seed = next_restore_seed();
    for attempt in 0_u32..64 {
        let candidate = restore_sibling_path(parent, name, purpose, seed, attempt);
        match fs::symlink_metadata(&candidate) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(candidate),
            Ok(_) => continue,
            Err(error) => return Err(format!("inspect {}: {error}", candidate.display())),
        }
    }
    Err("could not allocate a unique restore claim".to_string())
}

fn restore_sibling_path(
    parent: &Path,
    name: &OsStr,
    purpose: &str,
    seed: u64,
    attempt: u32,
) -> PathBuf {
    parent.join(format!(
        ".{}.aterm-{purpose}-{}-{seed}-{attempt}.tmp",
        name.to_string_lossy(),
        std::process::id()
    ))
}

fn next_restore_seed() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let ordinal = NEXT.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos() as u64);
    ordinal ^ nanos.rotate_left(19)
}

#[cfg(not(windows))]
fn replace_restore_file(temporary: &Path, target: &Path) -> std::io::Result<()> {
    fs::rename(temporary, target)
}

#[cfg(not(windows))]
fn claim_restore_file(source: &Path, claim: &Path) -> std::io::Result<()> {
    fs::rename(source, claim)
}

#[cfg(windows)]
fn replace_restore_file(temporary: &Path, target: &Path) -> std::io::Result<()> {
    move_restore_file(temporary, target, true)
}

#[cfg(windows)]
fn claim_restore_file(source: &Path, claim: &Path) -> std::io::Result<()> {
    move_restore_file(source, claim, false)
}

#[cfg(windows)]
fn move_restore_file(source: &Path, target: &Path, replace: bool) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x0000_0001;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x0000_0008;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(existing: *const u16, replacement: *const u16, flags: u32) -> i32;
    }

    let source = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let target = target
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let flags = MOVEFILE_WRITE_THROUGH
        | if replace {
            MOVEFILE_REPLACE_EXISTING
        } else {
            0
        };
    // SAFETY: both UTF-16 buffers are NUL-terminated and remain live for the call.
    let moved = unsafe { MoveFileExW(source.as_ptr(), target.as_ptr(), flags) };
    if moved == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn sync_restore_directory(path: &Path) -> std::io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_restore_directory(_path: &Path) -> std::io::Result<()> {
    // Windows publication/claim uses MOVEFILE_WRITE_THROUGH above.
    Ok(())
}

/// Write the manifest to the standard [`manifest_path`] (graceful-quit hook).
pub(crate) fn write(manifest: &RestoreManifest) -> Result<(), String> {
    let path = manifest_path().ok_or("no data dir for session restore")?;
    write_to(&path, manifest)
}

/// Read + delete the standard [`manifest_path`] (launch hook).
pub(crate) fn take() -> Option<RestoreManifest> {
    take_from(&manifest_path()?)
}

// ---------------------------------------------------------------------------
// L1 (show the window early): the per-scale REAL cell-metrics cache.
//
// A windowed cold launch sizes its first — hidden — OS window from
// `seed_cell_px`, a 0.6×/1.2× em heuristic, and keeps it hidden until the
// backend build joins (~300 ms) because revealing the heuristic size would
// trade blankness for a visibly JUMPING window. This cache removes the
// heuristic on every launch after the first: `attach_os_window` persists the
// REAL cell metrics the finished backend derived, keyed by display scale
// factor, so the NEXT launch can size the pre-backend window exactly and
// reveal it before the join.
//
// Deliberately NOT part of [`RestoreManifest`]: the manifest is single-use
// (deleted on read) and written only on graceful quit, so folding the metrics
// in would lose them to every crash and every restore-less boot. This is a
// durable many-use sibling in the same data dir, sharing the manifest's lock +
// unique-temp publication machinery so the two persistence lanes cannot round
// atomicity differently.
//
// Staleness is SAFE, not correct-by-construction: a font-file update that
// changes advance widths without touching any config key yields a wrong entry.
// The consumer (`attach_os_window`) treats the cache as a size *prediction*,
// re-derives the truth after the backend joins, logs when they disagree, and
// re-persists — one launch with a one-off post-reveal resize, then converged.

/// On-disk schema for [`CellMetricsCache`]. Bumped on incompatible layout
/// change; a mismatched file is ignored wholesale (cold-launch behaviour),
/// never mis-parsed.
const CELL_METRICS_SCHEMA: u32 = 1;

/// Scales worth remembering. A desktop has a handful of distinct DPI factors;
/// dropping the OLDEST entry beyond this bounds the file against a pathological
/// `$ATERM_FORCE_SCALE` sweep writing unbounded rows.
const MAX_CELL_METRICS_ENTRIES: usize = 16;

/// Sanity bound for one persisted cell edge in device px. A corrupted or
/// hand-edited file must never size a window to absurdity; out-of-band entries
/// read as absent (cold launch), and degenerate measurements are never written.
const MAX_CELL_EDGE_PX: usize = 4096;

/// One scale factor's measured cell geometry.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
pub(crate) struct CellMetricsEntry {
    /// The display scale factor × 1000, rounded — an integer key so lookup is
    /// exact equality, immune to f64 formatting drift through TOML.
    pub scale_milli: u32,
    /// The font size (physical px) the cells were measured at — the launch
    /// default for this scale at write time. A reader predicts its own target
    /// px and rejects the entry on mismatch, so an explicit `font_px` config
    /// change invalidates without a schema bump.
    pub font_px: f32,
    pub cell_w: u32,
    pub cell_h: u32,
}

/// The whole cache: one font-selection fingerprint, then per-scale entries.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
pub(crate) struct CellMetricsCache {
    pub schema: u32,
    /// The cell-GEOMETRY fingerprint (font selection + line height) every entry
    /// was measured under. One key for the whole file, not per entry: a font
    /// change invalidates every scale's metrics at once, which is exactly true.
    pub font_key: String,
    #[serde(default)]
    pub entries: Vec<CellMetricsEntry>,
}

/// The cache path: `<data_dir>/aterm/cell-metrics.toml`, beside `session.toml`.
fn cell_metrics_path() -> Option<PathBuf> {
    aterm_types::dirs::data_dir().map(|d| d.join("aterm").join("cell-metrics.toml"))
}

/// `scale` → the integer entry key. Clamped positive so a hostile/broken scale
/// can neither alias 0 nor overflow.
fn cell_metrics_scale_milli(scale: f64) -> u32 {
    (scale * 1000.0).round().clamp(1.0, 1_000_000.0) as u32
}

/// Fail-safe read: any I/O error, malformed TOML, or schema mismatch is `None`
/// (the caller cold-launches), mirroring the manifest's parse contract.
fn read_cell_metrics(path: &Path) -> Option<CellMetricsCache> {
    let text = fs::read_to_string(path).ok()?;
    let cache: CellMetricsCache = toml::from_str(&text).ok()?;
    (cache.schema == CELL_METRICS_SCHEMA).then_some(cache)
}

/// The persisted cell size for `scale`, iff it was measured under the same
/// font fingerprint at (within rounding) the same font px the caller predicts
/// for this launch. `None` = cold launch (keep the hidden-until-ready path).
/// Unlocked read on the launch critical path: publication is whole-file
/// atomic (unique temp + rename), so a torn read cannot be observed.
#[cfg_attr(not(windows), allow(dead_code))] // consumed by the Windows early-reveal path (macOS adoption is future work — its head_pts measure needs the attach-time chrome calls first)
pub(crate) fn load_cell_metrics(
    font_key: &str,
    scale: f64,
    expected_font_px: f32,
) -> Option<(usize, usize)> {
    load_cell_metrics_from(&cell_metrics_path()?, font_key, scale, expected_font_px)
}

fn load_cell_metrics_from(
    path: &Path,
    font_key: &str,
    scale: f64,
    expected_font_px: f32,
) -> Option<(usize, usize)> {
    let cache = read_cell_metrics(path)?;
    if cache.font_key != font_key {
        return None;
    }
    let key = cell_metrics_scale_milli(scale);
    let entry = cache.entries.iter().find(|e| e.scale_milli == key)?;
    // Same tolerance class as the attach path's own "backend already at the
    // target size" comparison (0.5 px): a real font-px change always exceeds
    // it, while a bit-identical re-derivation always passes.
    if (entry.font_px - expected_font_px).abs() >= 0.5 {
        return None;
    }
    let (cell_w, cell_h) = (entry.cell_w as usize, entry.cell_h as usize);
    ((1..=MAX_CELL_EDGE_PX).contains(&cell_w) && (1..=MAX_CELL_EDGE_PX).contains(&cell_h))
        .then_some((cell_w, cell_h))
}

/// Persist one scale's measured cell metrics. Fire-and-forget on a spawned
/// thread: the caller sits inside the window-attach bracket (squarely between
/// launch and first paint), and a lost write costs exactly one future cold
/// launch — self-healing, so neither the write nor its fsync belongs on the
/// critical path. Owned arguments because the thread outlives the borrow.
pub(crate) fn store_cell_metrics(
    font_key: String,
    scale: f64,
    font_px: f32,
    cell_w: usize,
    cell_h: usize,
) {
    let Some(path) = cell_metrics_path() else {
        return;
    };
    std::thread::spawn(move || {
        if let Err(error) =
            store_cell_metrics_to(&path, &font_key, scale, font_px, cell_w, cell_h)
        {
            eprintln!("aterm-gui: cell-metrics cache not written: {error}");
        }
    });
}

fn store_cell_metrics_to(
    path: &Path,
    font_key: &str,
    scale: f64,
    font_px: f32,
    cell_w: usize,
    cell_h: usize,
) -> Result<(), String> {
    // Never persist a degenerate measurement: a 0-px or absurd cell would make
    // the NEXT launch reveal a broken frame. Skipping is correct — the cache
    // simply stays cold for this scale.
    if !(1..=MAX_CELL_EDGE_PX).contains(&cell_w) || !(1..=MAX_CELL_EDGE_PX).contains(&cell_h) {
        return Ok(());
    }
    let (Ok(cell_w), Ok(cell_h)) = (u32::try_from(cell_w), u32::try_from(cell_h)) else {
        return Ok(());
    };
    let parent = path
        .parent()
        .ok_or_else(|| format!("cell-metrics cache {} has no parent", path.display()))?;
    fs::create_dir_all(parent).map_err(|error| format!("mkdir {}: {error}", parent.display()))?;
    // Read-modify-write UNDER the shared restore lock (keyed per file name, so
    // this never contends with `session.toml` traffic): two live aterm
    // instances finishing backends concurrently must not lose each other's
    // scale entries by racing the whole-file rewrite.
    with_restore_lock(path, || {
        let mut cache = read_cell_metrics(path)
            .filter(|c| c.font_key == font_key)
            .unwrap_or_else(|| CellMetricsCache {
                schema: CELL_METRICS_SCHEMA,
                font_key: font_key.to_string(),
                entries: Vec::new(),
            });
        let entry = CellMetricsEntry {
            scale_milli: cell_metrics_scale_milli(scale),
            font_px,
            cell_w,
            cell_h,
        };
        match cache
            .entries
            .iter_mut()
            .find(|e| e.scale_milli == entry.scale_milli)
        {
            // Steady state (every attach on an already-cached scale): byte-equal
            // entry, no disk write at all.
            Some(existing) if *existing == entry => return Ok(()),
            Some(existing) => *existing = entry,
            None => {
                if cache.entries.len() >= MAX_CELL_METRICS_ENTRIES {
                    cache.entries.remove(0);
                }
                cache.entries.push(entry);
            }
        }
        let toml =
            toml::to_string(&cache).map_err(|e| format!("serialize cell metrics: {e}"))?;
        write_restore_locked(path, toml.as_bytes())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> RestoreManifest {
        // A two-window manifest: window 0 has one plain tab + one split tab; window 1 a
        // single tab. Exercises every variant + per-leaf cwd/title/focus + active_tab.
        RestoreManifest::new(vec![
            WindowLayout {
                rows: 24,
                cols: 80,
                active_tab: 1,
                outer_x: Some(120),
                outer_y: Some(64),
                maximized: Some(true),
                tabs: vec![
                    PaneLayout::leaf(Some("/home/a".into()), "zsh".into(), true),
                    PaneLayout::Split {
                        dir: SplitKind::Vertical,
                        ratio: 0.6,
                        first: Box::new(PaneLayout::leaf(Some("/tmp".into()), "vim".into(), true)),
                        second: Box::new(PaneLayout::leaf(None, "top".into(), false)),
                    },
                ],
                native_tabs: Vec::new(),
                tab_order: Vec::new(),
                active_item: None,
                restored_tabs: Vec::new(),
            },
            WindowLayout {
                rows: 40,
                cols: 132,
                active_tab: 0,
                outer_x: None,
                outer_y: None,
                maximized: None,
                tabs: vec![PaneLayout::leaf(Some("/".into()), "sh".into(), true)],
                native_tabs: Vec::new(),
                tab_order: Vec::new(),
                active_item: None,
                restored_tabs: Vec::new(),
            },
        ])
    }

    fn mixed_sample(uri: &str) -> RestoreManifest {
        RestoreManifest::new(vec![WindowLayout {
            rows: 32,
            cols: 110,
            active_tab: 0,
            outer_x: None,
            outer_y: None,
            maximized: None,
            tabs: vec![PaneLayout::leaf(Some("/tmp".into()), "shell".into(), true)],
            native_tabs: vec![
                NativeTabRestore::Settings {
                    route: "/about".to_string(),
                },
                NativeTabRestore::Editor {
                    uri: uri.to_string(),
                },
            ],
            tab_order: vec![
                TabOrderEntry::Native { index: 0 },
                TabOrderEntry::Terminal { index: 0 },
                TabOrderEntry::Native { index: 1 },
            ],
            active_item: Some(2),
            restored_tabs: Vec::new(),
        }])
    }

    /// The manifest survives a TOML round-trip byte-for-byte (structure + order + cwd +
    /// title + focus + ratio), the persistence contract restore depends on.
    #[test]
    fn manifest_round_trips_through_toml() {
        let m = sample();
        let toml = m.to_toml().expect("serialize");
        let back = RestoreManifest::from_toml(&toml).expect("parse");
        assert_eq!(m, back);
        // leaf_count sanity: window 0 has 1 + 2 = 3 leaves across its 2 tabs.
        assert_eq!(m.windows[0].tabs[1].leaf_count(), 2);
        assert!(matches!(
            m.windows[0].restored_tabs[1].root,
            RestoredSplitTree::Split {
                axis: SplitKind::Horizontal,
                ..
            }
        ));
        assert!(!m.is_empty());
    }

    /// W3 schema compatibility both ways. FORWARD: a pre-`maximized` manifest (the
    /// key simply absent — byte-for-byte what an older build wrote, thanks to
    /// `skip_serializing_if`) parses with `maximized: None`, so an upgrade never
    /// rejects the user's saved session. BACKWARD: a `None` never emits the key,
    /// so a manifest written by this build and read by an older one carries
    /// nothing new unless a window really was maximized (and serde's default
    /// unknown-field tolerance absorbs it there).
    #[test]
    fn manifest_maximized_is_schema_compatible() {
        // sample() has window 0 maximized: the flag must survive the round trip
        // (covered structurally by `manifest_round_trips_through_toml`, asserted
        // by name here so a serde-attr regression fails with a legible message).
        let toml = sample().to_toml().expect("serialize");
        assert!(toml.contains("maximized = true"));
        let back = RestoreManifest::from_toml(&toml).expect("parse");
        assert_eq!(back.windows[0].maximized, Some(true));
        assert_eq!(back.windows[1].maximized, None);

        // The legacy shape: strip the flag and re-serialize — the key vanishes
        // entirely (an old manifest, byte-for-byte in this respect) and parsing
        // yields "unknown", never a forced state.
        let mut legacy = sample();
        for window in &mut legacy.windows {
            window.maximized = None;
        }
        let legacy_toml = legacy.to_toml().expect("serialize legacy");
        assert!(!legacy_toml.contains("maximized"));
        let back = RestoreManifest::from_toml(&legacy_toml).expect("parse legacy");
        assert!(back.windows.iter().all(|w| w.maximized.is_none()));
    }

    #[test]
    fn restore_parse_canonicalizes_hostile_user_chrome_metadata() {
        let mut manifest = sample();
        let RestoredSplitTree::Leaf {
            view: RestoredView::Terminal(terminal),
        } = &mut manifest.windows[0].restored_tabs[0].root
        else {
            panic!("sample first tab is a terminal leaf");
        };
        terminal.user_title = Some("  build\n\u{202e}agent  ".to_string());
        terminal.description = Some(format!("purpose\u{2029}{}", "x".repeat(1100)));
        terminal.icon = Some("\u{2066}👨‍👩‍👧‍👦\u{2069}".to_string());

        // Serialize the deliberately noncanonical structure directly: the
        // ordinary constructor already sanitizes and would not exercise parse.
        let wire = toml::to_string(&manifest).expect("serialize hostile fixture");
        let decoded = RestoreManifest::from_toml(&wire).expect("bounded restore parses");
        let RestoredSplitTree::Leaf {
            view: RestoredView::Terminal(terminal),
        } = &decoded.windows[0].restored_tabs[0].root
        else {
            panic!("decoded first tab is a terminal leaf");
        };
        assert_eq!(terminal.user_title.as_deref(), Some("buildagent"));
        assert_eq!(terminal.icon.as_deref(), Some("👨‍👩‍👧‍👦"));
        let description = terminal
            .description
            .as_deref()
            .expect("description retained");
        assert!(description.starts_with("purpose"));
        assert!(description.len() <= crate::session_timeline::META_DESCRIPTION_MAX);
        assert!(!crate::session_timeline::metadata_has_forbidden_formatting(
            description
        ));
        assert!(terminal.user_metadata_is_canonical());
    }

    #[test]
    fn mixed_native_descriptors_round_trip_without_live_or_transient_state() {
        let manifest = mixed_sample("file:///tmp/readme.md");
        let toml = manifest.to_toml().expect("serialize");
        let decoded = RestoreManifest::from_toml(&toml).expect("decode mixed manifest");
        assert_eq!(decoded, manifest);
        let window = &decoded.windows[0];
        let order = window.canonical_order().expect("validated order");
        assert_eq!(window.canonical_active(&order), 2);
        assert_eq!(
            window.native_tabs[1].document_uri(),
            Some("file:///tmp/readme.md")
        );
        for forbidden in [
            "tab_id",
            "view_id",
            "instance_id",
            "search_input",
            "draft",
            "unsaved",
        ] {
            assert!(
                !toml.contains(forbidden),
                "transient field {forbidden:?} leaked into restore TOML"
            );
        }
    }

    #[test]
    fn legacy_terminal_only_manifest_migrates_to_identity_order() {
        let legacy = r#"
schema = 1

[[windows]]
rows = 24
cols = 80
active_tab = 0

[[windows.tabs]]
kind = "leaf"
cwd = "/tmp"
title = "shell"
focused = true
"#;
        let decoded = RestoreManifest::from_toml(legacy).expect("legacy manifest");
        assert!(decoded.windows[0].native_tabs.is_empty());
        assert_eq!(
            decoded.windows[0].canonical_order(),
            Some(vec![TabOrderEntry::Terminal { index: 0 }])
        );
        assert_eq!(
            decoded.windows[0]
                .canonical_active(&decoded.windows[0].canonical_order().expect("legacy order")),
            0
        );
    }

    #[test]
    fn malformed_native_metadata_and_aliasing_order_fail_closed() {
        let mut malformed_uri = mixed_sample("https://example.invalid/not-local.md");
        let toml = toml::to_string(&malformed_uri).unwrap();
        assert!(RestoreManifest::from_toml(&toml).is_none());

        malformed_uri.windows[0].native_tabs[1] = NativeTabRestore::Editor {
            uri: "file:///tmp/readme.md".to_string(),
        };
        malformed_uri.windows[0].tab_order = vec![
            TabOrderEntry::Native { index: 0 },
            TabOrderEntry::Terminal { index: 0 },
            TabOrderEntry::Native { index: 0 },
        ];
        let toml = toml::to_string(&malformed_uri).unwrap();
        assert!(
            RestoreManifest::from_toml(&toml).is_none(),
            "duplicate descriptor indices may not alias one live tab"
        );

        let mut duplicate_settings = mixed_sample("file:///tmp/readme.md");
        duplicate_settings.windows[0].native_tabs[1] = NativeTabRestore::Settings {
            route: "/updates".to_string(),
        };
        assert!(
            RestoreManifest::from_toml(&duplicate_settings.to_toml().unwrap()).is_none(),
            "one window cannot restore two aliases of its Settings singleton"
        );

        let missing_uri = r#"
schema = 1
[[windows]]
rows = 24
cols = 80
active_tab = 0
[[windows.native_tabs]]
kind = "editor"
[[windows.tab_order]]
kind = "native"
index = 0
active_item = 0
"#;
        assert!(RestoreManifest::from_toml(missing_uri).is_none());
    }

    #[test]
    fn v2_unknown_and_corrupt_native_leaves_become_in_place_placeholders() {
        let input = r#"
schema = 2

[[windows]]
rows = 30
cols = 100
active_item = 0

[[windows.restored_tabs]]
focused_path = ["second"]

[windows.restored_tabs.root]
node = "split"
axis = "vertical"
ratio = 0.61

[windows.restored_tabs.root.first]
node = "leaf"
[windows.restored_tabs.root.first.view]
kind = "terminal"
cwd = "/tmp"
title = "healthy shell"

[windows.restored_tabs.root.second]
node = "leaf"
[windows.restored_tabs.root.second.view]
kind = "native"
restore_tag = "future.diagram"
metadata = "opaque=copy-me"
"#;
        let manifest = RestoreManifest::from_toml(input).expect("v2 manifest remains usable");
        let tab = &manifest.windows[0].restored_tabs[0];
        let RestoredSplitTree::Split { first, second, .. } = &tab.root else {
            panic!("split topology was not preserved");
        };
        assert!(matches!(
            &**first,
            RestoredSplitTree::Leaf {
                view: RestoredView::Terminal(TerminalLeafRestore { title, .. })
            } if title == "healthy shell"
        ));
        assert!(matches!(
            &**second,
            RestoredSplitTree::Leaf {
                view: RestoredView::Placeholder(PlaceholderLeafRestore {
                    restore_tag,
                    reason,
                    metadata,
                })
            } if restore_tag == "future.diagram"
                && reason.contains("unavailable")
                && metadata.contains("copy-me")
        ));

        let corrupt_known = input.replace("future.diagram", "editor").replace(
            "metadata = \"opaque=copy-me\"",
            "uri = \"https://invalid.example\"",
        );
        let recovered = RestoreManifest::from_toml(&corrupt_known)
            .expect("one corrupt known leaf must not discard its sibling");
        let RestoredSplitTree::Split { second, .. } = &recovered.windows[0].restored_tabs[0].root
        else {
            unreachable!()
        };
        assert!(matches!(
            &**second,
            RestoredSplitTree::Leaf {
                view: RestoredView::Placeholder(PlaceholderLeafRestore { reason, .. })
            } if reason == "invalid document URI"
        ));
    }

    #[test]
    fn v2_sanitizes_geometry_but_rejects_resource_exhaustion_shapes() {
        let mut manifest = mixed_sample("file:///tmp/readme.md");
        let mut tab = RestoredTab::from_legacy_native(&NativeTabRestore::Editor {
            uri: "file:///tmp/readme.md".to_string(),
        })
        .unwrap();
        tab.root = RestoredSplitTree::Split {
            axis: SplitKind::Horizontal,
            ratio: f32::NAN,
            first: Box::new(tab.root.clone()),
            second: Box::new(RestoredSplitTree::leaf(RestoredView::Terminal(
                TerminalLeafRestore {
                    cwd: None,
                    title: "shell".to_string(),
                    profile: None,
                    local_id: None,
                    user_title: None,
                    description: None,
                    icon: None,
                    role: None,
                    attention: None,
                },
            ))),
        };
        tab.focused_path = vec![RestoreBranch::First];
        manifest.windows[0].restored_tabs = vec![tab];
        manifest.windows[0].active_item = Some(0);
        let encoded = toml::to_string(&manifest).unwrap();
        let decoded = RestoreManifest::from_toml(&encoded).expect("NaN ratio is recoverable");
        assert!(matches!(
            decoded.windows[0].restored_tabs[0].root,
            RestoredSplitTree::Split { ratio, .. } if ratio == 0.5
        ));

        let mut root = RestoredSplitTree::leaf(RestoredView::Terminal(TerminalLeafRestore {
            cwd: None,
            title: String::new(),
            profile: None,
            local_id: None,
            user_title: None,
            description: None,
            icon: None,
            role: None,
            attention: None,
        }));
        for _ in 0..=MAX_SPLIT_DEPTH {
            root = RestoredSplitTree::Split {
                axis: SplitKind::Horizontal,
                ratio: 0.5,
                first: Box::new(root),
                second: Box::new(RestoredSplitTree::leaf(RestoredView::Terminal(
                    TerminalLeafRestore {
                        cwd: None,
                        title: String::new(),
                        profile: None,
                        local_id: None,
                        user_title: None,
                        description: None,
                        icon: None,
                        role: None,
                        attention: None,
                    },
                ))),
            };
        }
        manifest.windows[0].restored_tabs = vec![RestoredTab {
            root,
            focused_path: Vec::new(),
            zoomed: false,
        }];
        assert!(
            RestoreManifest::from_toml(&toml::to_string(&manifest).unwrap()).is_none(),
            "adversarial depth must fail before runtime allocation"
        );
    }

    /// FAIL-SAFE: corrupt TOML, a wrong schema, and empty input all yield `None` (start
    /// fresh) rather than a partial/misparsed layout.
    #[test]
    fn parse_is_fail_safe() {
        assert!(RestoreManifest::from_toml("this is not toml {{{").is_none());
        assert!(RestoreManifest::from_toml("").is_none());
        // Valid TOML, wrong schema → ignored.
        let mut m = sample();
        m.schema = 999;
        let toml = toml::to_string(&m).unwrap();
        assert!(
            RestoreManifest::from_toml(&toml).is_none(),
            "stale schema is ignored"
        );
    }

    /// `write_to` then `take_from` round-trips through a real file, and the read is
    /// SINGLE-USE — the manifest is deleted, so a second take yields `None`.
    #[test]
    fn write_then_take_is_single_use() {
        let dir = std::env::temp_dir().join(format!("aterm-restore-test-{}", std::process::id()));
        let path = dir.join("session.toml");
        let m = sample();
        write_to(&path, &m).expect("write");
        assert!(path.exists(), "manifest written");
        let back = take_from(&path).expect("take");
        assert_eq!(m, back);
        assert!(!path.exists(), "single-use: manifest deleted on read");
        assert!(take_from(&path).is_none(), "a second take finds nothing");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unique_durable_write_replaces_existing_without_touching_fixed_temp_alias() {
        let dir =
            std::env::temp_dir().join(format!("aterm-restore-replace-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.toml");
        let fixed = path.with_extension("toml.tmp");
        std::fs::write(&fixed, "unrelated").unwrap();
        std::fs::write(&path, "stale").unwrap();

        let manifest = sample();
        write_to(&path, &manifest).unwrap();
        assert_eq!(
            RestoreManifest::from_toml(&std::fs::read_to_string(&path).unwrap()),
            Some(manifest)
        );
        assert_eq!(std::fs::read_to_string(&fixed).unwrap(), "unrelated");
        assert!(std::fs::read_dir(&dir).unwrap().all(|entry| {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            !name.contains("aterm-write")
        }));

        // Tier-1 projection of the genuine locked unique-temp publication.
        let model = aterm_spec::derive::restore_manifest_single_use_model();
        let locked = model.successors("LockWriter", &model.init_state())[0].clone();
        let temporary = model.successors("CreateUniqueTemporary", &locked)[0].clone();
        let published = model.successors("PublishManifest", &temporary)[0].clone();
        assert_eq!(published["visible"], 1);
        assert!(model.check_invariant("UniqueTemporaryNeverAliases", &published));
        let rejected_alias = model.successors("ReuseFixedTemporary", &locked);
        assert_eq!(rejected_alias, vec![locked]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn concurrent_takers_have_exactly_one_committed_consumer() {
        let dir =
            std::env::temp_dir().join(format!("aterm-restore-consume-race-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("session.toml");
        let manifest = sample();
        write_to(&path, &manifest).unwrap();

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let mut takers = Vec::new();
        for _ in 0..2 {
            let path = path.clone();
            let barrier = barrier.clone();
            takers.push(std::thread::spawn(move || {
                barrier.wait();
                take_from(&path)
            }));
        }
        barrier.wait();
        let results = takers
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|result| result.is_some()).count(), 1);
        assert!(results.into_iter().flatten().all(|value| value == manifest));
        assert!(!path.exists());
        assert!(take_from(&path).is_none());

        // Tier-1 projection of the one real winner and absent loser. Returning
        // before SyncClaim is the explicit negative control.
        let model = aterm_spec::derive::restore_manifest_single_use_model();
        let locked = model.successors("LockTakeA", &model.init_state())[0].clone();
        let claimed = model.successors("ClaimA", &locked)[0].clone();
        assert!(model.successors("ReturnA", &claimed).is_empty());
        let synced = model.successors("SyncClaim", &claimed)[0].clone();
        let returned = model.successors("ReturnA", &synced)[0].clone();
        let loser_locked = model.successors("LockTakeB", &returned)[0].clone();
        let absent = model.successors("ObserveAbsentB", &loser_locked)[0].clone();
        assert_eq!(absent["returned"], 1);
        assert!(model.check_invariant("AtMostOneConsumer", &absent));
        assert!(model.check_invariant("ReturnOnlyAfterDurableClaim", &absent));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// L1 cell-metrics cache: store→load round-trips per scale, a second scale
    /// coexists with the first, and re-storing the identical entry is the
    /// steady-state no-op (file bytes untouched).
    #[test]
    fn cell_metrics_round_trip_per_scale() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-cell-metrics-roundtrip-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cell-metrics.toml");

        store_cell_metrics_to(&path, "JetBrains Mono|lh=1", 1.0, 12.0, 7, 15).unwrap();
        store_cell_metrics_to(&path, "JetBrains Mono|lh=1", 1.5, 18.0, 11, 22).unwrap();
        assert_eq!(
            load_cell_metrics_from(&path, "JetBrains Mono|lh=1", 1.0, 12.0),
            Some((7, 15))
        );
        assert_eq!(
            load_cell_metrics_from(&path, "JetBrains Mono|lh=1", 1.5, 18.0),
            Some((11, 22))
        );
        // An uncached scale misses without disturbing the cached ones.
        assert_eq!(
            load_cell_metrics_from(&path, "JetBrains Mono|lh=1", 2.0, 24.0),
            None
        );

        // Steady state: the identical re-store leaves the published file's
        // mtime-bearing BYTES untouched (compare content — mtimes are too
        // coarse on some filesystems to prove a skipped write).
        let before = std::fs::read_to_string(&path).unwrap();
        store_cell_metrics_to(&path, "JetBrains Mono|lh=1", 1.0, 12.0, 7, 15).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);

        // A REAL change on one scale updates that entry and keeps the other.
        store_cell_metrics_to(&path, "JetBrains Mono|lh=1", 1.0, 12.0, 8, 16).unwrap();
        assert_eq!(
            load_cell_metrics_from(&path, "JetBrains Mono|lh=1", 1.0, 12.0),
            Some((8, 16))
        );
        assert_eq!(
            load_cell_metrics_from(&path, "JetBrains Mono|lh=1", 1.5, 18.0),
            Some((11, 22))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// L1 cell-metrics cache: every staleness gate fails CLOSED (a miss, never a
    /// wrong hit) — font-key change, font-px change, degenerate persisted cells,
    /// schema mismatch, and absent/corrupt files.
    #[test]
    fn cell_metrics_staleness_gates_fail_closed() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-cell-metrics-staleness-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cell-metrics.toml");

        // Absent + corrupt files read as cold launches.
        assert_eq!(load_cell_metrics_from(&path, "k", 1.0, 12.0), None);
        std::fs::write(&path, "not toml [").unwrap();
        assert_eq!(load_cell_metrics_from(&path, "k", 1.0, 12.0), None);
        let _ = std::fs::remove_file(&path);

        store_cell_metrics_to(&path, "Cascadia|lh=1", 1.0, 12.0, 7, 15).unwrap();
        // A font change (new fingerprint) misses...
        assert_eq!(load_cell_metrics_from(&path, "Fira|lh=1", 1.0, 12.0), None);
        // ...and the next STORE under the new key drops the stale entries
        // wholesale rather than mixing fingerprints.
        store_cell_metrics_to(&path, "Fira|lh=1", 2.0, 24.0, 13, 29).unwrap();
        assert_eq!(
            load_cell_metrics_from(&path, "Cascadia|lh=1", 1.0, 12.0),
            None
        );
        assert_eq!(
            load_cell_metrics_from(&path, "Fira|lh=1", 2.0, 24.0),
            Some((13, 29))
        );
        // A predicted-font-px drift ≥ the tolerance misses (an explicit
        // `font_px` config change), a sub-tolerance one hits (f32 noise).
        assert_eq!(load_cell_metrics_from(&path, "Fira|lh=1", 2.0, 25.0), None);
        assert_eq!(
            load_cell_metrics_from(&path, "Fira|lh=1", 2.0, 24.2),
            Some((13, 29))
        );

        // Degenerate measurements are never persisted...
        store_cell_metrics_to(&path, "Fira|lh=1", 3.0, 36.0, 0, 15).unwrap();
        assert_eq!(load_cell_metrics_from(&path, "Fira|lh=1", 3.0, 36.0), None);
        // ...and a hand-corrupted absurd entry is rejected at load.
        let text = std::fs::read_to_string(&path)
            .unwrap()
            .replace("cell_w = 13", "cell_w = 99999");
        std::fs::write(&path, text).unwrap();
        assert_eq!(load_cell_metrics_from(&path, "Fira|lh=1", 2.0, 24.0), None);

        // A future schema is ignored wholesale, never mis-parsed.
        let _ = std::fs::remove_file(&path);
        store_cell_metrics_to(&path, "Fira|lh=1", 1.0, 12.0, 7, 15).unwrap();
        let text = std::fs::read_to_string(&path)
            .unwrap()
            .replace("schema = 1", "schema = 999");
        std::fs::write(&path, text).unwrap();
        assert_eq!(load_cell_metrics_from(&path, "Fira|lh=1", 1.0, 12.0), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
