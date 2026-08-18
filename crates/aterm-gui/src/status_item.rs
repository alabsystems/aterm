// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The menu-bar OPERATOR status item (macOS `NSStatusItem`).
//!
//! aterm's fleet can be supervised by an OPERATOR — a designated session running
//! a coding agent that watches and drives the other sessions (the brief lives at
//! `docs/OPERATOR.md`). This module puts a persistent icon in the macOS menu bar,
//! created when the FIRST OS window attaches and alive for the process lifetime,
//! that (a) reflects operator/fleet state at a glance and (b) manages the
//! operator: Start (spawn a session and launch the agent CLI with the brief),
//! Show (focus its tab), Stop (retire its session).
//!
//! State is read from what the instance already knows — session user meta and
//! titles in the in-process registry — never from a socket loop. The TYPED
//! protocol (see `docs/OPERATOR.md`) is `meta set role operator` to name the
//! operator and a non-empty `meta set attention <why>` to escalate; the legacy
//! title conventions (`operator: …` prefix, `⚠`-prefixed titles) remain
//! honored as fallback so older briefs keep working. The classifier here is
//! PURE (rows in, glance out, no locks, no AppKit) so it is exhaustively
//! unit-tested off macOS too.
//!
//! Structure mirrors `menu.rs` exactly: a portable action enum with stable
//! integer tags + a pure model at the top; the `objc2` AppKit half in a
//! `#[cfg(target_os = "macos")]` module below; `()` handle + no-op `install`
//! stubs off macOS so `App`'s field shape is platform-independent.

/// The launch line [`OperatorAction::Start`] types into the freshly spawned shell —
/// exactly what a human would type to stand the operator up. SELF-CONTAINED by
/// design: it references no filesystem path, because the spawned session's cwd
/// is wherever the user's focused pane was and an installed aterm has no repo
/// checkout — the bootstrap brief is `aterm help introspection`, which ships in
/// every binary; `docs/OPERATOR.md` is named as optional enhancement only. The
/// agent CLI authenticates from its own config, so this carries no secrets
/// (aterm strips agent env vars from children by design).
pub const OPERATOR_LAUNCH_LINE: &str = "claude \"You are this machine's aterm fleet operator. Run 'aterm help introspection' to learn how to see and drive sessions, set your role with: aterm ctl @self meta set role operator - then await fleet instructions from the human. If a docs/OPERATOR.md exists in your cwd, read and follow it too.\"";

/// One user action from the status-item menu. Carried by `Wake::OperatorAction`
/// from the AppKit callback to the event loop, which dispatches on `App`.
///
/// Tags are this menu's OWN namespace (the items are wired to `ATermStatusTarget`,
/// never to the main-menu `ATermMenuTarget`, so they can never collide with
/// `MenuAction` tags). Stable once assigned; never reuse a retired tag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperatorAction {
    /// Spawn a fresh tab session and launch the operator agent in it.
    Start,
    /// Focus the operator session's tab and raise its window.
    Show,
    /// Retire the operator session (deliberate, confirm-free close).
    Stop,
    /// Raise a specific window of this instance (payload: logical `WindowId`).
    FocusWindow(u64),
    /// Focus the tab displaying a session (payload: session local id) — what a
    /// click on a needs-attention row does.
    FocusSession(u64),
    /// Activate a sibling aterm instance's windows (payload: its pid).
    RaiseInstance(u32),
}

/// The packed-tag band width for payload-carrying actions: `tag = kind × BAND +
/// payload`. Payloads are small monotonic counters (window/session ids) or pids,
/// so `payload < BAND` always holds in practice; an out-of-range payload encodes
/// to `0` (the inert tag) rather than aliasing another row — fail closed.
const TAG_BAND: isize = 1_000_000_000_000;

impl OperatorAction {
    /// The `NSMenuItem.tag` this action rides in. `0` is never used (an untagged
    /// item decodes to `None` and stays inert). Fixed actions keep their original
    /// small tags; payload actions ride the [`TAG_BAND`] codec.
    pub fn tag(self) -> isize {
        let packed = |kind: isize, payload: u64| -> isize {
            isize::try_from(payload)
                .ok()
                .filter(|p| *p < TAG_BAND)
                .map_or(0, |p| kind * TAG_BAND + p)
        };
        match self {
            OperatorAction::Start => 1,
            OperatorAction::Show => 2,
            OperatorAction::Stop => 3,
            OperatorAction::FocusWindow(id) => packed(1, id),
            OperatorAction::FocusSession(id) => packed(2, id),
            OperatorAction::RaiseInstance(pid) => packed(3, u64::from(pid)),
        }
    }

    /// Inverse of [`tag`](Self::tag); unknown tags are `None` (inert item).
    pub fn from_tag(tag: isize) -> Option<Self> {
        match tag {
            1 => Some(OperatorAction::Start),
            2 => Some(OperatorAction::Show),
            3 => Some(OperatorAction::Stop),
            t if t >= TAG_BAND => {
                let payload = t % TAG_BAND;
                let payload_u64 = u64::try_from(payload).ok()?;
                match t / TAG_BAND {
                    1 => Some(OperatorAction::FocusWindow(payload_u64)),
                    2 => Some(OperatorAction::FocusSession(payload_u64)),
                    3 => u32::try_from(payload_u64)
                        .ok()
                        .map(OperatorAction::RaiseInstance),
                    _ => None,
                }
            }
            _ => None,
        }
    }
}

/// The operator's classified state, derived purely from session titles.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OperatorState {
    /// No session identifies as the operator.
    NotRunning,
    /// An operator session exists; the payload is its self-reported detail (the
    /// part of its title after `operator`, e.g. `: fleet idle`), trimmed.
    Running(String),
}

/// One session as the classifier sees it: the effective title joined with the
/// TYPED user-meta fields. Built by `App::operator_fleet_glance` from the
/// registry snapshot; pure data so [`classify`] stays lock-free and testable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRow {
    /// Process-local session id (the `@<n>` selector and Show/close target).
    pub id: u64,
    /// Effective title: user meta title when set, else the live OSC title.
    pub title: String,
    /// Typed `meta set role` value (recognized: `operator`).
    pub role: Option<String>,
    /// Typed `meta set attention` value — non-empty means needs-human.
    pub attention: Option<String>,
}

/// One window of this instance as the menu shows it (row order = id order,
/// which the windows map already keeps stable and ascending).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowRow {
    /// Logical window id — the [`OperatorAction::FocusWindow`] payload.
    pub id: u64,
    /// The composed chrome title the window shows right now.
    pub title: String,
    /// Tab count, rendered beside the title.
    pub tabs: usize,
    /// Whether this is the frontmost window (rendered with a leading mark).
    pub frontmost: bool,
}

/// A sibling aterm instance discovered through the shared control-socket dir,
/// summarized by the background fleet scan (never dialed on the UI thread).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstanceRow {
    /// The sibling's pid — the [`OperatorAction::RaiseInstance`] payload; `0`
    /// means the graph entry predates pid recording (row renders disabled).
    pub pid: u32,
    /// Live sessions it reported.
    pub sessions: usize,
    /// How many of its sessions are escalating (typed attention or `⚠` title).
    pub warnings: usize,
    /// Whether it reports a running operator.
    pub operator: bool,
}

/// A point-in-time glance at operator + fleet, produced by [`classify`] and
/// rendered verbatim by the AppKit half. Pure data — no handles, no locks.
/// `windows` and `instances` start empty out of [`classify`] (they are not
/// session facts) and are filled by the caller before rendering.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FleetGlance {
    /// Operator state (see [`OperatorState`]).
    pub operator: OperatorState,
    /// The operator session's process-local id, when one is running — what
    /// the menu's Show/Stop actions act on.
    pub operator_session: Option<u64>,
    /// Whether the operator was elected by the TYPED `role=operator` meta
    /// (`true`) or by the legacy title heuristic (`false`). Destructive
    /// authority follows this bit: only a typed operator gets a Stop row and a
    /// confirm-suppressed close — a title match ("operator.md" in an editor is
    /// enough to produce one) must never be silently closable.
    pub operator_typed: bool,
    /// Whether the operator agent CLI is actually launchable on this machine
    /// (caller-owned fact, like `windows`): gates the Start row so clicking
    /// it can never type a command the shell will not find.
    pub start_available: bool,
    /// Total live sessions in this instance (the operator included).
    pub sessions: usize,
    /// Escalating sessions in roster order: `(local_id, display text)`. Typed
    /// `attention` meta escalates (rendered `⚠ <message>`); a `⚠`-prefixed
    /// title still escalates as fallback. Non-empty ⇒ the bar icon badges.
    pub warnings: Vec<(u64, String)>,
    /// This instance's windows, in id order.
    pub windows: Vec<WindowRow>,
    /// Sibling instances from the last background fleet scan, in pid order.
    pub instances: Vec<InstanceRow>,
}

impl FleetGlance {
    /// The menu-bar button title for this glance. `❯` is the operator mark;
    /// a trailing `⚠` is the needs-human badge (AppKit renders color emoji in
    /// menu titles natively — safe here, unlike aterm's own overlay renderer).
    /// Sibling escalations badge too — the icon answers "does ANY aterm need
    /// me", as far as the last background scan saw.
    pub fn button_title(&self) -> &'static str {
        if !self.warnings.is_empty() || self.instances.iter().any(|i| i.warnings > 0) {
            "❯⚠"
        } else {
            "❯"
        }
    }

    /// One line summarizing the operator for the menu header (disabled item).
    pub fn header_line(&self) -> String {
        match &self.operator {
            OperatorState::NotRunning => "Operator: not running".to_string(),
            OperatorState::Running(detail) if detail.is_empty() => {
                "Operator: running".to_string()
            }
            OperatorState::Running(detail) => format!("Operator{detail}"),
        }
    }

    /// A cheap change fingerprint: the caller refreshes AppKit only when this
    /// string differs from the previous glance's (title drift is frequent; menu
    /// rebuilds should not be). Covers EVERY rendered fact — windows and
    /// sibling instances included — and its inputs arrive pre-sorted (windows
    /// in id order, instances in pid order), so it is deterministic.
    pub fn fingerprint(&self) -> String {
        let mut fp = self.header_line();
        fp.push('\u{1f}');
        fp.push_str(self.button_title());
        fp.push('\u{1f}');
        fp.push_str(&self.sessions.to_string());
        // Both bits change what the menu renders (Stop row / Start enablement).
        fp.push('\u{1f}');
        fp.push(if self.operator_typed { 'T' } else { 't' });
        fp.push(if self.start_available { 'S' } else { 's' });
        for (id, title) in &self.warnings {
            fp.push('\u{1f}');
            fp.push_str(&id.to_string());
            fp.push('=');
            fp.push_str(title);
        }
        for w in &self.windows {
            fp.push('\u{1f}');
            fp.push_str(&format!("w{}={}:{}:{}", w.id, w.title, w.tabs, w.frontmost));
        }
        for i in &self.instances {
            fp.push('\u{1f}');
            fp.push_str(&format!(
                "i{}={}:{}:{}",
                i.pid, i.sessions, i.warnings, i.operator
            ));
        }
        fp
    }
}

/// A title after stripping leading non-alphanumeric status glyphs (tab
/// spinners/state marks like `✳`/`◐`) and whitespace — `⚠` survives the strip
/// so escalated titles stay recognizable.
fn stripped_title(title: &str) -> &str {
    title
        .trim_start_matches(|c: char| !c.is_alphanumeric() && c != '⚠')
        .trim_start()
}

/// The legacy title-convention operator test: the stripped title starts with
/// `operator` (ASCII case-insensitive) ending at a word boundary — "operator",
/// "operator:", "operator (2)" match; "operators", "cooperator" never do.
/// Returns the trailing detail on a match.
fn title_operator_detail(title: &str) -> Option<String> {
    let stripped = stripped_title(title);
    match stripped.get(..8) {
        Some(prefix) if prefix.eq_ignore_ascii_case("operator") => stripped[8..]
            .chars()
            .next()
            .is_none_or(|c| !c.is_alphanumeric())
            .then(|| stripped[8..].trim_end().to_string()),
        _ => None,
    }
}

/// Classify the fleet from [`SessionRow`]s (roster order).
///
/// TYPED STATE FIRST, title conventions as fallback:
///
/// * Operator — the first row whose `role` equals `operator` (ASCII
///   case-insensitive) wins outright; only when NO row carries the typed role
///   does the legacy `operator`-title scan run. The running detail comes from
///   the title either way (the title stays the human-readable status line).
/// * Warnings — a row with non-empty typed `attention` escalates and renders
///   `⚠ <message>`; a row without it whose title starts with `⚠` escalates
///   with the title as the message (never both — one row per session).
///
/// An operator whose own row escalates still counts as running (its warning
/// row carries the detail). `windows`/`instances` start empty — the caller
/// owns those facts.
pub fn classify(rows: &[SessionRow]) -> FleetGlance {
    let mut warnings = Vec::new();
    for row in rows {
        match row.attention.as_deref().map(str::trim) {
            Some(message) if !message.is_empty() => {
                let display = if message.starts_with('⚠') {
                    message.to_string()
                } else {
                    format!("⚠ {message}")
                };
                warnings.push((row.id, display));
            }
            _ => {
                let title = stripped_title(&row.title);
                if title.starts_with('⚠') || row.title.trim_start().starts_with('⚠') {
                    warnings.push((row.id, row.title.trim().to_string()));
                }
            }
        }
    }

    let typed = rows.iter().find(|row| {
        row.role
            .as_deref()
            .is_some_and(|r| r.trim().eq_ignore_ascii_case("operator"))
    });
    let by_title = || {
        rows.iter()
            .find_map(|row| title_operator_detail(&row.title).map(|detail| (row, detail)))
    };
    let (operator, operator_session, operator_typed) = match typed {
        Some(row) => {
            // Detail rung: legacy `operator…` title tail when present (the
            // brief's status-line convention), else the whole title.
            let detail = title_operator_detail(&row.title).unwrap_or_else(|| {
                let title = row.title.trim();
                if title.is_empty() {
                    String::new()
                } else {
                    format!(": {title}")
                }
            });
            (OperatorState::Running(detail), Some(row.id), true)
        }
        None => match by_title() {
            Some((row, detail)) => (OperatorState::Running(detail), Some(row.id), false),
            None => (OperatorState::NotRunning, None, false),
        },
    };

    FleetGlance {
        operator,
        operator_session,
        operator_typed,
        start_available: false,
        sessions: rows.len(),
        warnings,
        windows: Vec::new(),
        instances: Vec::new(),
    }
}

/// One row of the rendered status menu — the pure model the AppKit half only
/// paints (the tab context menu's "the description IS the menu" discipline).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StatusRow {
    /// A disabled information line.
    Info(String),
    /// A separator.
    Separator,
    /// A clickable row carrying an [`OperatorAction`].
    Action {
        /// The visible label.
        label: String,
        /// What a click posts.
        action: OperatorAction,
        /// Rendered-but-inert when `false` (e.g. a pid-less instance row).
        enabled: bool,
    },
}

/// Compose the whole status menu from a glance — pure, deterministic, and
/// unit-testable off macOS. Layout: operator header + management actions,
/// this instance's windows (click focuses), escalation rows (click focuses the
/// session's tab), the session count, then sibling instances (click activates).
pub fn compose_status_menu(glance: &FleetGlance) -> Vec<StatusRow> {
    let mut rows = vec![StatusRow::Info(glance.header_line())];
    match glance.operator {
        OperatorState::NotRunning => {
            // Rendered-but-inert when the agent CLI is missing: the affordance
            // stays discoverable, and the Info line says why it is grey.
            rows.push(StatusRow::Action {
                label: "Start Operator".to_string(),
                action: OperatorAction::Start,
                enabled: glance.start_available,
            });
            if !glance.start_available {
                rows.push(StatusRow::Info("(claude CLI not found on PATH)".to_string()));
            }
        }
        OperatorState::Running(_) => {
            rows.push(StatusRow::Action {
                label: "Show Operator".to_string(),
                action: OperatorAction::Show,
                enabled: true,
            });
            // DESTRUCTIVE authority requires the TYPED role: a title-elected
            // "operator" can be an innocent session (vim editing operator.md),
            // so it gets no Stop row at all — Show is the only offer.
            if glance.operator_typed {
                rows.push(StatusRow::Action {
                    label: "Stop Operator".to_string(),
                    action: OperatorAction::Stop,
                    enabled: true,
                });
            } else {
                rows.push(StatusRow::Info(
                    "(detected by title only — stop it from its own tab)".to_string(),
                ));
            }
        }
    }

    if !glance.windows.is_empty() {
        rows.push(StatusRow::Separator);
        for w in &glance.windows {
            let mark = if w.frontmost { "• " } else { "" };
            let tabs = if w.tabs == 1 {
                "1 tab".to_string()
            } else {
                format!("{} tabs", w.tabs)
            };
            rows.push(StatusRow::Action {
                label: format!("{mark}{} — {tabs}", w.title),
                action: OperatorAction::FocusWindow(w.id),
                enabled: true,
            });
        }
    }

    rows.push(StatusRow::Separator);
    for (id, message) in &glance.warnings {
        rows.push(StatusRow::Action {
            label: message.clone(),
            action: OperatorAction::FocusSession(*id),
            enabled: true,
        });
    }
    rows.push(StatusRow::Info(format!("Sessions: {}", glance.sessions)));

    if !glance.instances.is_empty() {
        rows.push(StatusRow::Separator);
        rows.push(StatusRow::Info("Other aterm instances".to_string()));
        for i in &glance.instances {
            let sessions = if i.sessions == 1 {
                "1 session".to_string()
            } else {
                format!("{} sessions", i.sessions)
            };
            let mut label = format!("aterm {} — {sessions}", i.pid);
            if i.warnings > 0 {
                label.push_str(&format!(", ⚠ {}", i.warnings));
            }
            if i.operator {
                label.push_str(" — operator");
            }
            rows.push(StatusRow::Action {
                label,
                action: OperatorAction::RaiseInstance(i.pid),
                enabled: i.pid != 0,
            });
        }
    }
    rows
}

#[cfg(target_os = "macos")]
pub use macos::{StatusItemHandle, install, update};

/// Non-macOS no-op handle: there is no menu-bar status item off macOS. Held by
/// `App` in the same field on every target so the struct shape is
/// platform-independent (the `MenuHandle` pattern).
#[cfg(not(target_os = "macos"))]
pub type StatusItemHandle = ();

/// Non-macOS stub: installing a status item is a no-op that installs nothing.
#[cfg(not(target_os = "macos"))]
pub fn install(
    _proxy: &winit::event_loop::EventLoopProxy<crate::Wake>,
    _glance: &FleetGlance,
) -> Option<StatusItemHandle> {
    None
}

/// Non-macOS stub: nothing to refresh.
#[cfg(not(target_os = "macos"))]
pub fn update(_handle: &StatusItemHandle, _glance: &FleetGlance) {}

#[cfg(target_os = "macos")]
mod macos {
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObjectProtocol, ProtocolObject, Sel};
    use objc2::{ClassType, DeclaredClass, declare_class, msg_send_id, mutability, sel};
    use objc2_app_kit::{
        NSMenu, NSMenuDelegate, NSMenuItem, NSStatusBar, NSStatusItem, NSVariableStatusItemLength,
    };
    use objc2_foundation::{MainThreadMarker, NSString};
    use winit::event_loop::EventLoopProxy;

    use super::{FleetGlance, OperatorAction};
    use crate::Wake;

    /// What [`install`] returns. BOTH fields are load-bearing retentions:
    /// releasing an `NSStatusItem` REMOVES it from the menu bar, and AppKit holds
    /// a menu item's target only weakly — so `App` keeps this handle in a field
    /// for the process lifetime (the `MenuHandle` rule).
    pub struct StatusItemHandle {
        /// The single `statusAction:` relay target every item is wired to.
        target: Retained<StatusTarget>,
        /// The bar item itself; dropping it would vanish the icon.
        item: Retained<NSStatusItem>,
    }

    declare_class!(
        /// The target object for every status-menu item. Owns the
        /// `EventLoopProxy<Wake>` and exposes one `statusAction:` selector that
        /// decodes the sender's tag to an [`OperatorAction`] and posts
        /// [`Wake::OperatorAction`] — a pure relay from AppKit into the `Wake`
        /// channel, exactly like `menu.rs`'s `MenuTarget`.
        pub(crate) struct StatusTarget;

        // SAFETY:
        // - NSObject imposes no subclassing requirements.
        // - MainThreadOnly is required by the NSMenuDelegate protocol bound
        //   (`NSMenuDelegate: IsMainThreadOnly`) and is sound: the target is
        //   created and only ever messaged on the main thread.
        // - StatusTarget has no Drop impl beyond the auto-generated ivar drop.
        unsafe impl ClassType for StatusTarget {
            type Super = objc2::runtime::NSObject;
            type Mutability = mutability::MainThreadOnly;
            const NAME: &'static str = "ATermStatusTarget";
        }

        impl DeclaredClass for StatusTarget {
            type Ivars = EventLoopProxy<Wake>;
        }

        unsafe impl StatusTarget {
            /// `statusAction:` — the one selector wired to every actionable item.
            /// A tag that doesn't decode is inert (never fires a wrong command).
            #[method(statusAction:)]
            fn status_action(&self, sender: Option<&NSMenuItem>) {
                let Some(item) = sender else { return };
                // SAFETY: `item` is the live NSMenuItem AppKit passed as the
                // action sender; `tag` is a plain getter with no side effects.
                let tag = unsafe { item.tag() };
                if let Some(action) = OperatorAction::from_tag(tag) {
                    // Fire-and-forget: a closed loop (app shutting down) just
                    // drops the event — mirrors menu.rs.
                    let _ = self.ivars().send_event(Wake::OperatorAction { action });
                }
            }
        }

        unsafe impl NSObjectProtocol for StatusTarget {}

        unsafe impl NSMenuDelegate for StatusTarget {
            /// The menu is about to track: RELAY ONLY. This runs inside
            /// AppKit's nested menu-tracking run loop, so it must never touch
            /// `App` — it posts a wake and the event loop kicks the background
            /// sibling scan, whose result freshens the NEXT open (the
            /// `toolbar.rs` mid-track discipline).
            #[method(menuWillOpen:)]
            fn menu_will_open(&self, _menu: &NSMenu) {
                let _ = self.ivars().send_event(Wake::OperatorMenuOpening);
            }
        }
    );

    impl StatusTarget {
        /// Allocate a target owning `proxy`, on the main thread (`mtm`).
        fn new(mtm: MainThreadMarker, proxy: EventLoopProxy<Wake>) -> Retained<Self> {
            let this = mtm.alloc().set_ivars(proxy);
            // SAFETY: plain `[super init]` on a freshly allocated instance.
            unsafe { msg_send_id![super(this), init] }
        }
    }

    /// Create the menu-bar status item and its menu, rendered from `glance`.
    /// Called once when the first OS window attaches (never headless). Returns
    /// the retained handle for `App` to keep alive; best-effort `None` off the
    /// main thread — never a panic (the `menu::install` contract).
    pub fn install(
        proxy: &EventLoopProxy<Wake>,
        glance: &FleetGlance,
    ) -> Option<StatusItemHandle> {
        let mtm = MainThreadMarker::new()?;
        let target = StatusTarget::new(mtm, proxy.clone());
        // SAFETY: systemStatusBar/statusItemWithLength are main-thread AppKit
        // calls (proved by `mtm`); the returned item is retained in the handle.
        let item: Retained<NSStatusItem> = unsafe {
            NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength)
        };
        let handle = StatusItemHandle { target, item };
        update(&handle, glance);
        Some(handle)
    }

    /// Re-render the bar button title and rebuild the menu wholesale from
    /// `glance` — the `update_version_menu` mutation pattern. Main-thread
    /// guarded: a call off the main thread is a silent no-op.
    pub fn update(handle: &StatusItemHandle, glance: &FleetGlance) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        // SAFETY: all calls are plain AppKit setters/constructors on live
        // receivers, on the main thread. The button exists for a bar item we
        // created with a variable length; the delegate (the retained target)
        // outlives the menu because the handle retains it.
        unsafe {
            if let Some(button) = handle.item.button(mtm) {
                button.setTitle(&NSString::from_str(glance.button_title()));
            }
            let menu = build_menu(mtm, &handle.target, glance);
            menu.setDelegate(Some(ProtocolObject::from_ref(&*handle.target)));
            handle.item.setMenu(Some(&menu));
        }
    }

    /// Build the status menu for `glance` by PAINTING the pure model from
    /// `compose_status_menu` — native code renders rows, never decides them.
    /// Auto-enable is off so the model's `enabled` flags are authoritative.
    fn build_menu(
        mtm: MainThreadMarker,
        target: &StatusTarget,
        glance: &FleetGlance,
    ) -> Retained<NSMenu> {
        let menu = NSMenu::new(mtm);
        // SAFETY: plain setter on the fresh menu, on the main thread.
        unsafe { menu.setAutoenablesItems(false) };
        for row in super::compose_status_menu(glance) {
            match row {
                super::StatusRow::Info(text) => add_info(mtm, &menu, &text),
                super::StatusRow::Separator => add_separator(mtm, &menu),
                super::StatusRow::Action {
                    label,
                    action,
                    enabled,
                } => add_action(mtm, &menu, target, &label, action, enabled),
            }
        }
        menu
    }

    /// Append an actionable item wired to `target` carrying `action`'s tag.
    fn add_action(
        mtm: MainThreadMarker,
        menu: &NSMenu,
        target: &StatusTarget,
        title: &str,
        action: OperatorAction,
        enabled: bool,
    ) {
        let title = NSString::from_str(title);
        let sel: Sel = sel!(statusAction:);
        // SAFETY: standard NSMenuItem construction + plain setters on a fresh
        // instance, all on the main thread (`mtm`) — the menu.rs add_item shape.
        unsafe {
            let item: Retained<NSMenuItem> = NSMenuItem::initWithTitle_action_keyEquivalent(
                mtm.alloc(),
                &title,
                Some(sel),
                &NSString::from_str(""),
            );
            let target_obj: &AnyObject = target;
            item.setTarget(Some(target_obj));
            item.setTag(action.tag());
            item.setEnabled(enabled);
            menu.addItem(&item);
        }
    }

    /// Append a disabled information row (explicit — auto-enable is off).
    fn add_info(mtm: MainThreadMarker, menu: &NSMenu, title: &str) {
        let title = NSString::from_str(title);
        // SAFETY: plain item construction with no action, on the main thread.
        unsafe {
            let item: Retained<NSMenuItem> = NSMenuItem::initWithTitle_action_keyEquivalent(
                mtm.alloc(),
                &title,
                None,
                &NSString::from_str(""),
            );
            item.setEnabled(false);
            menu.addItem(&item);
        }
    }

    /// Append a separator line.
    fn add_separator(mtm: MainThreadMarker, menu: &NSMenu) {
        let sep = NSMenuItem::separatorItem(mtm);
        menu.addItem(&sep);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(v: &[(u64, &str)]) -> Vec<SessionRow> {
        v.iter()
            .map(|(i, s)| SessionRow {
                id: *i,
                title: s.to_string(),
                role: None,
                attention: None,
            })
            .collect()
    }

    #[test]
    fn tags_round_trip_and_unknown_is_inert() {
        for a in [
            OperatorAction::Start,
            OperatorAction::Show,
            OperatorAction::Stop,
        ] {
            assert_eq!(OperatorAction::from_tag(a.tag()), Some(a));
        }
        assert_eq!(OperatorAction::from_tag(0), None);
        assert_eq!(OperatorAction::from_tag(99), None);
    }

    #[test]
    fn empty_fleet_is_not_running() {
        let g = classify(&[]);
        assert_eq!(g.operator, OperatorState::NotRunning);
        assert_eq!(g.operator_session, None);
        assert_eq!(g.sessions, 0);
        assert!(g.warnings.is_empty());
        assert_eq!(g.button_title(), "❯");
        assert_eq!(g.header_line(), "Operator: not running");
    }

    #[test]
    fn operator_found_by_title_prefix_case_insensitive() {
        let g = classify(&rows(&[(3, "zsh"), (7, "Operator: fleet idle"), (9, "vim")]));
        assert_eq!(g.operator_session, Some(7));
        assert_eq!(g.operator, OperatorState::Running(": fleet idle".into()));
        assert_eq!(g.header_line(), "Operator: fleet idle");
        assert_eq!(g.sessions, 3);
    }

    #[test]
    fn status_glyph_prefixes_are_stripped() {
        // Tab titles carry spinner/state glyphs (e.g. "✳ operator: busy").
        let g = classify(&rows(&[(2, "✳ operator: driving 2 workers")]));
        assert_eq!(g.operator_session, Some(2));
        assert_eq!(g.operator, OperatorState::Running(": driving 2 workers".into()));
    }

    #[test]
    fn warning_titles_badge_the_button() {
        let g = classify(&rows(&[(1, "operator: fleet idle"), (4, "⚠ needs human: approval")]));
        assert_eq!(g.warnings, vec![(4, "⚠ needs human: approval".to_string())]);
        assert_eq!(g.button_title(), "❯⚠");
    }

    #[test]
    fn first_operator_wins_and_bare_operator_word_matches() {
        let g = classify(&rows(&[(1, "operator"), (2, "operator: second")]));
        assert_eq!(g.operator_session, Some(1));
        assert_eq!(g.operator, OperatorState::Running(String::new()));
        assert_eq!(g.header_line(), "Operator: running");
    }

    #[test]
    fn non_operator_titles_do_not_match() {
        // "operators guide" begins with the word but not the identity prefix
        // boundary we accept ("operator" + non-alnum); it still must not match.
        let g = classify(&rows(&[(1, "cooperator"), (2, "operators guide"), (3, "vim operators.txt")]));
        assert_eq!(g.operator_session, None);
        assert_eq!(g.operator, OperatorState::NotRunning);
    }

    #[test]
    fn fingerprint_changes_with_state_not_with_noise() {
        let a = classify(&rows(&[(1, "operator: fleet idle"), (2, "zsh")]));
        let b = classify(&rows(&[(1, "operator: fleet idle"), (2, "zsh")]));
        assert_eq!(a.fingerprint(), b.fingerprint());
        let c = classify(&rows(&[(1, "operator: fleet idle"), (2, "⚠ stuck")]));
        assert_ne!(a.fingerprint(), c.fingerprint());
    }

    fn row(id: u64, title: &str, role: Option<&str>, attention: Option<&str>) -> SessionRow {
        SessionRow {
            id,
            title: title.to_string(),
            role: role.map(str::to_string),
            attention: attention.map(str::to_string),
        }
    }

    #[test]
    fn typed_role_outranks_the_title_convention_in_both_directions() {
        // A LATER typed role beats an EARLIER title-convention candidate…
        let g = classify(&[
            row(2, "operator: legacy imposter", None, None),
            row(5, "fleet brain", Some("operator"), None),
        ]);
        assert_eq!(g.operator_session, Some(5));
        assert_eq!(g.operator, OperatorState::Running(": fleet brain".into()));
        // …and with NO typed role anywhere, the title scan still wins (the
        // fallback that keeps older briefs working).
        let g = classify(&[row(1, "zsh", None, None), row(2, "operator: legacy", None, None)]);
        assert_eq!(g.operator_session, Some(2));
        // Typed role is case-insensitive and trimmed; other roles never match.
        let g = classify(&[row(3, "worker", Some(" Operator "), None)]);
        assert_eq!(g.operator_session, Some(3));
        let g = classify(&[row(3, "worker", Some("supervisor"), None)]);
        assert_eq!(g.operator_session, None);
    }

    #[test]
    fn typed_operator_with_operator_title_keeps_the_legacy_detail() {
        let g = classify(&[row(7, "✳ operator: driving 2 workers", Some("operator"), None)]);
        assert_eq!(g.operator, OperatorState::Running(": driving 2 workers".into()));
        // An empty title renders as the bare running header.
        let g = classify(&[row(7, "", Some("operator"), None)]);
        assert_eq!(g.operator, OperatorState::Running(String::new()));
        assert_eq!(g.header_line(), "Operator: running");
    }

    #[test]
    fn typed_attention_escalates_dedups_and_never_double_prefixes() {
        // Typed attention escalates with the ⚠-prefixed message.
        let g = classify(&[row(4, "zsh", None, Some("needs approval"))]);
        assert_eq!(g.warnings, vec![(4, "⚠ needs approval".to_string())]);
        assert_eq!(g.button_title(), "❯⚠");
        // Typed attention + ⚠ title on ONE session ⇒ exactly one row, typed wins.
        let g = classify(&[row(4, "⚠ stale title", None, Some("real reason"))]);
        assert_eq!(g.warnings, vec![(4, "⚠ real reason".to_string())]);
        // A message already carrying ⚠ is not double-prefixed.
        let g = classify(&[row(4, "zsh", None, Some("⚠ already marked"))]);
        assert_eq!(g.warnings, vec![(4, "⚠ already marked".to_string())]);
        // Whitespace-only attention is unset in spirit: fall back to the title
        // scan (here: nothing).
        let g = classify(&[row(4, "zsh", None, Some("   "))]);
        assert!(g.warnings.is_empty());
        // An escalated operator still counts as running.
        let g = classify(&[row(1, "operator: stuck", Some("operator"), Some("wedged on CI"))]);
        assert_eq!(g.operator_session, Some(1));
        assert_eq!(g.warnings, vec![(1, "⚠ wedged on CI".to_string())]);
    }

    #[test]
    fn windows_and_instances_ride_the_fingerprint_and_badge() {
        let base = classify(&rows(&[(1, "zsh")]));
        let mut with_window = base.clone();
        with_window.windows.push(WindowRow {
            id: 1,
            title: "aterm".into(),
            tabs: 2,
            frontmost: true,
        });
        assert_ne!(base.fingerprint(), with_window.fingerprint());
        let mut refocused = with_window.clone();
        refocused.windows[0].frontmost = false;
        assert_ne!(with_window.fingerprint(), refocused.fingerprint());
        // Sibling escalations badge the button and move the fingerprint.
        let mut with_instance = base.clone();
        with_instance.instances.push(InstanceRow {
            pid: 4242,
            sessions: 3,
            warnings: 1,
            operator: false,
        });
        assert_eq!(with_instance.button_title(), "❯⚠");
        assert_ne!(base.fingerprint(), with_instance.fingerprint());
        // Determinism: identical content ⇒ identical fingerprint.
        assert_eq!(with_instance.fingerprint(), with_instance.fingerprint());
    }

    #[test]
    fn compose_layout_not_running_is_minimal() {
        let mut g = classify(&[]);
        g.start_available = true;
        let rows = compose_status_menu(&g);
        assert_eq!(
            rows,
            vec![
                StatusRow::Info("Operator: not running".into()),
                StatusRow::Action {
                    label: "Start Operator".into(),
                    action: OperatorAction::Start,
                    enabled: true,
                },
                StatusRow::Separator,
                StatusRow::Info("Sessions: 0".into()),
            ]
        );
    }

    #[test]
    fn start_is_inert_and_explained_without_the_cli() {
        // classify() leaves start_available=false (caller-owned fact): the
        // Start row renders disabled with the reason line under it.
        let rows = compose_status_menu(&classify(&[]));
        assert_eq!(
            &rows[1..3],
            &[
                StatusRow::Action {
                    label: "Start Operator".into(),
                    action: OperatorAction::Start,
                    enabled: false,
                },
                StatusRow::Info("(claude CLI not found on PATH)".into()),
            ]
        );
    }

    #[test]
    fn title_elected_operator_gets_no_stop_row() {
        // SI-3: vim editing "operator.md" must never receive a silent Stop.
        let rows_for = |role: Option<&str>, title: &str| {
            let mut g = classify(&[SessionRow {
                id: 7,
                title: title.into(),
                role: role.map(str::to_string),
                attention: None,
            }]);
            g.start_available = true;
            compose_status_menu(&g)
        };
        let heuristic = rows_for(None, "operator: fleet idle");
        assert!(
            !heuristic.iter().any(|r| matches!(
                r,
                StatusRow::Action { action: OperatorAction::Stop, .. }
            )),
            "title-elected operator must not be offered Stop: {heuristic:?}"
        );
        let typed = rows_for(Some("operator"), "operator: fleet idle");
        assert!(
            typed.iter().any(|r| matches!(
                r,
                StatusRow::Action { action: OperatorAction::Stop, .. }
            )),
            "typed operator must keep Stop: {typed:?}"
        );
    }

    #[test]
    fn typed_bit_reaches_the_glance_and_fingerprint() {
        let typed = classify(&[SessionRow {
            id: 1,
            title: "operator: x".into(),
            role: Some("operator".into()),
            attention: None,
        }]);
        let heuristic = classify(&[SessionRow {
            id: 1,
            title: "operator: x".into(),
            role: None,
            attention: None,
        }]);
        assert!(typed.operator_typed);
        assert!(!heuristic.operator_typed);
        // Same rendered strings, different authority — the fingerprint must
        // still differ so the menu rebuilds when the election basis changes.
        assert_ne!(typed.fingerprint(), heuristic.fingerprint());
    }

    #[test]
    fn compose_layout_full_glance_orders_every_section() {
        let mut g = classify(&[
            row(1, "operator: busy", Some("operator"), None),
            row(2, "zsh", None, Some("needs approval")),
        ]);
        g.windows = vec![
            WindowRow {
                id: 1,
                title: "build".into(),
                tabs: 1,
                frontmost: false,
            },
            WindowRow {
                id: 3,
                title: "review".into(),
                tabs: 4,
                frontmost: true,
            },
        ];
        g.instances = vec![
            InstanceRow {
                pid: 0,
                sessions: 1,
                warnings: 0,
                operator: false,
            },
            InstanceRow {
                pid: 900,
                sessions: 2,
                warnings: 1,
                operator: true,
            },
        ];
        let rows = compose_status_menu(&g);
        assert_eq!(
            rows,
            vec![
                StatusRow::Info("Operator: busy".into()),
                StatusRow::Action {
                    label: "Show Operator".into(),
                    action: OperatorAction::Show,
                    enabled: true,
                },
                StatusRow::Action {
                    label: "Stop Operator".into(),
                    action: OperatorAction::Stop,
                    enabled: true,
                },
                StatusRow::Separator,
                StatusRow::Action {
                    label: "build — 1 tab".into(),
                    action: OperatorAction::FocusWindow(1),
                    enabled: true,
                },
                StatusRow::Action {
                    label: "• review — 4 tabs".into(),
                    action: OperatorAction::FocusWindow(3),
                    enabled: true,
                },
                StatusRow::Separator,
                StatusRow::Action {
                    label: "⚠ needs approval".into(),
                    action: OperatorAction::FocusSession(2),
                    enabled: true,
                },
                StatusRow::Info("Sessions: 2".into()),
                StatusRow::Separator,
                StatusRow::Info("Other aterm instances".into()),
                StatusRow::Action {
                    label: "aterm 0 — 1 session".into(),
                    action: OperatorAction::RaiseInstance(0),
                    enabled: false,
                },
                StatusRow::Action {
                    label: "aterm 900 — 2 sessions, ⚠ 1 — operator".into(),
                    action: OperatorAction::RaiseInstance(900),
                    enabled: true,
                },
            ]
        );
    }

    #[test]
    fn packed_tags_round_trip_and_out_of_range_is_inert() {
        for a in [
            OperatorAction::FocusWindow(7),
            OperatorAction::FocusSession(3),
            OperatorAction::RaiseInstance(4242),
        ] {
            assert_eq!(OperatorAction::from_tag(a.tag()), Some(a));
        }
        // Payload at the band edge encodes to the inert 0 tag, never aliases.
        assert_eq!(OperatorAction::FocusWindow(u64::MAX).tag(), 0);
        assert_eq!(OperatorAction::from_tag(0), None);
        // An unknown packed kind decodes to None.
        assert_eq!(OperatorAction::from_tag(4 * 1_000_000_000_000 + 5), None);
    }
}
