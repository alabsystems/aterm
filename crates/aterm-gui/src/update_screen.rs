// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The own-rendered, cross-platform SOFTWARE UPDATE overlay: a floating [`DrawPrim`] card
//! (same simple native-window style as [`crate::about`]) that shows the running build, the
//! staged update (if any) with its "what's new" notes rendered from Markdown
//! ([`crate::markdown`]), and the actions — Check for Updates, Install & Relaunch (only
//! when a strictly-newer build is staged), and Close. It is the DETAILED update screen the
//! tab-strip ↻ icon, the App-menu "Software Update…" item, the macOS toolbar ↻ button, and
//! the fading "update ready" nudge all open. Shipping update details now render in the
//! native Settings `/updates` route, where `controls update` serializes that route's exact
//! compiled semantic frame. This former card model remains a regression fixture: ONE
//! structured snapshot ([`UpdateState`], captured from [`aterm_update::status`]) drives its
//! pixels and test projection, and ONE pure [`update_layout`] drives its painter and mouse
//! hit-test.

use aterm_render::Theme;

use crate::settings::{Roles, SettingsGeom, text_w};
use crate::tray_raster::{row_baseline, ui_text_width};
use crate::type_scale::TypeStep;
use crate::widget::{DrawPrim, TextFace, TextWeight, TrayInput, rgba, text_prim};

/// Transient per-window state for the Software Update overlay (mirrors `AboutState`'s slot).
/// A SNAPSHOT of the updater state captured when the overlay opens (or after a check), so
/// the pixels never read a half-written ledger. `checking` reflects a manual check in
/// flight (set true when the user presses Check, cleared when the refresh lands).
pub(crate) struct UpdateState {
    /// The running build's version string (e.g. `0.5.14`).
    current_version: String,
    /// The running build number.
    current_build: u64,
    /// A strictly-newer staged build `(build, version)`, if one is ready to apply.
    staged: Option<(u64, String)>,
    /// The staged build's "what changed" notes, rendered from Markdown to clean lines.
    changelog: Vec<String>,
    /// Whether the in-app updater is enabled on this platform / by config.
    enabled: bool,
    /// The last updater outcome string (from the health ledger) — shown small.
    outcome: String,
    /// The health ledger says this Mac's updates are FAILING PERSISTENTLY (a streak
    /// of one failure class, not a blip): the headline says so instead of "You're
    /// up to date", and `outcome` carries the ledger's own sentence with the cause.
    failing_persistent: bool,
    /// The failing CLASS (`apply`, `pipeline`, …) when `failing_persistent` — the
    /// MOST RECENT one, which is why it cannot decide [`Self::apply_is_failing`].
    failing_kind: String,
    /// Consecutive APPLY failures. `>= PERSISTENT_AFTER` is the exact statement
    /// "the staged build will not start", independent of which class happened to
    /// fail last (2026-08-19 round-4 skeptics).
    failing_applies: u32,
    /// This launch has a bundle the updater could replace at all.
    installable: bool,
    /// A manual "Check for Updates" is running off-thread (shows "Checking…").
    checking: bool,
}

/// Owned, structured read projection shared with the native Settings `/updates`
/// route.  It keeps the updater service state private while avoiding brittle
/// parsing of the legacy `controls update` serialization.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UpdateProjection {
    pub(crate) current_version: String,
    pub(crate) current_build: u64,
    pub(crate) staged: Option<(u64, String)>,
    pub(crate) changelog: Vec<String>,
    pub(crate) enabled: bool,
    pub(crate) outcome: String,
    pub(crate) checking: bool,
    /// The ledger's persistent-failure verdict — the surfaces style the headline
    /// as a warning and never say "current" while it holds.
    pub(crate) failing_persistent: bool,
    /// …and specifically in the APPLY class: the staged build will not start. An
    /// acquisition class (`manifest`, `pipeline`, …) says nothing about the stage
    /// in hand, so the "not applying" wording is reserved for this.
    pub(crate) apply_is_failing: bool,
    pub(crate) headline: String,
    pub(crate) detail: Option<String>,
}

/// The most changelog lines the card renders inline (the rest is elided — the full notes
/// live in `aterm-ctl update status` + the release page). Keeps the card a sane height.
const MAX_NOTES: usize = 12;

impl UpdateState {
    /// Project the process-owned updater reducer into UI state. This is the shipping
    /// path: it is deliberately memory-only so opening Settings, querying
    /// introspection, and repainting can never parse a ledger or launch a bundle
    /// metadata probe on the event-loop thread.
    pub(crate) fn from_service(
        snapshot: &crate::native_updater_service::UpdaterSnapshot,
        checking: bool,
    ) -> Self {
        let staged = snapshot.staged.as_ref().and_then(|staged| {
            (snapshot.enabled && staged.build > snapshot.current_build)
                .then(|| (staged.build, staged.version.clone()))
        });
        let changelog = snapshot
            .staged
            .as_ref()
            .filter(|_| staged.is_some())
            .and_then(|staged| staged.changelog.as_deref())
            .map(str::trim)
            .filter(|notes| !notes.is_empty())
            .map(|notes| {
                crate::markdown::to_plain_text(notes)
                    .lines()
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        Self {
            current_version: snapshot.current_version.clone(),
            current_build: snapshot.current_build,
            staged,
            changelog,
            enabled: snapshot.enabled,
            outcome: snapshot.outcome.clone(),
            failing_persistent: snapshot.failing_persistent,
            failing_kind: snapshot.failing_kind.clone(),
            failing_applies: snapshot.failing_applies,
            installable: snapshot.installable,
            checking,
        }
    }

    /// Snapshot the current updater state. `status` is `aterm_update::status(build)` —
    /// `None` on a platform with no updater (then the card says so). Pure: no I/O.
    #[cfg(test)]
    pub(crate) fn from_status(
        current_build: u64,
        current_version: &str,
        status: Option<&aterm_update::UpdateStatus>,
        checking: bool,
    ) -> Self {
        // "Ready" iff the staged build is strictly newer than what is ACTUALLY RUNNING
        // (`current_build`, from `build_info`) — not merely newer than the ledger's own
        // `current_build` snapshot, which can lag on a machine that staged-then-relaunched.
        let staged = status.and_then(|s| {
            let b = s.staged_build?;
            (b > current_build).then(|| {
                (
                    b,
                    s.staged_version.clone().unwrap_or_else(|| "?".to_string()),
                )
            })
        });
        let changelog = status
            .filter(|_| staged.is_some())
            .and_then(|s| s.changelog.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|md| {
                crate::markdown::to_plain_text(md)
                    .lines()
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        Self {
            current_version: current_version.to_string(),
            current_build,
            staged,
            changelog,
            enabled: status.map(|s| s.enabled).unwrap_or(false),
            outcome: status.map(|s| s.outcome.clone()).unwrap_or_default(),
            failing_persistent: status.is_some_and(|s| s.enabled && s.is_failing_persistently()),
            failing_kind: status.map(|s| s.failing_kind.clone()).unwrap_or_default(),
            failing_applies: status.map_or(0, |s| s.failing_applies),
            installable: status.is_none_or(|s| s.installable),
            checking,
        }
    }

    /// Snapshot the exact state the existing update card paints for native tab
    /// presentation and semantic introspection.
    pub(crate) fn projection(&self) -> UpdateProjection {
        UpdateProjection {
            current_version: self.current_version.clone(),
            current_build: self.current_build,
            staged: self.staged.clone(),
            changelog: self.changelog.clone(),
            enabled: self.enabled,
            outcome: self.outcome.clone(),
            checking: self.checking,
            failing_persistent: self.failing_persistent && self.enabled && !self.checking,
            apply_is_failing: self.apply_is_failing() && self.enabled && !self.checking,
            headline: self.headline(),
            detail: self.detail(),
        }
    }

    /// Whether a strictly-newer build is staged and ready to install.
    pub(crate) fn has_update(&self) -> bool {
        self.staged.is_some()
    }

    /// The action a plain Return fires — the button painted as the highlighted DEFAULT in
    /// the button row (see `update_tray`): Install when a build is staged, else Check for
    /// Updates while checks are enabled, else Close. The paint (`filled` flags) and this
    /// method read the SAME state, so what the user sees as the default IS what Return
    /// triggers. Kept in lockstep with the button-row `filled` flags in `update_tray`.
    #[cfg(any(test, feature = "a11y-accesskit"))]
    pub(crate) fn default_action(&self) -> UpdateHit {
        if self.has_update() {
            UpdateHit::Install
        } else if self.enabled {
            UpdateHit::Check
        } else {
            UpdateHit::Close
        }
    }

    /// A fingerprint of everything the card paints, folded into `RepaintKey` so opening /
    /// checking / a new staged build repaints exactly once. Never `0` (the closed sentinel).
    pub(crate) fn fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.current_version.hash(&mut h);
        self.current_build.hash(&mut h);
        self.staged.hash(&mut h);
        self.changelog.hash(&mut h);
        self.enabled.hash(&mut h);
        self.outcome.hash(&mut h);
        self.checking.hash(&mut h);
        h.finish() | 1
    }

    /// The primary status headline (the big semibold line under the current build). The
    /// version specifics live in [`Self::detail`], so the headline stays a short, strong
    /// statement — "Update ready", not a full version sentence.
    /// The persistent failure is in the APPLY class — the staged build will not
    /// start — as opposed to an acquisition class (`manifest`, `pipeline`,
    /// `network`, `stage`), which is about fetching the NEXT build.
    fn apply_is_failing(&self) -> bool {
        // NOT `failing_kind == "apply"`: that is the last class to fail, so a machine
        // whose apply lane escalated but whose latest failure was a network blip
        // would hide it, and a single apply failure under an escalated `pipeline`
        // streak would wrongly claim it. The apply streak itself is the statement.
        self.failing_applies >= aterm_update::PERSISTENT_AFTER
    }

    fn headline(&self) -> String {
        if self.checking {
            "Checking for updates\u{2026}".to_string()
        } else if self.staged.is_some() && self.enabled && self.apply_is_failing() {
            // A stage that is persistently FAILING TO APPLY (the APPLY class: the
            // handoff keeps ending badly) is not "Update ready" — the health notice
            // points the user here, and this line must not contradict it. Only the
            // apply class earns this sentence: a `manifest`/`pipeline` streak is
            // about ACQUIRING the NEXT build and says nothing about the stage in
            // hand, which is verified and will install (round-4 audit).
            "Update ready, but it keeps failing to apply.".to_string()
        } else if self.staged.is_some() {
            "Update ready".to_string()
        } else if !self.installable {
            // NOT "You're up to date": this copy cannot be replaced at all — it is
            // running from the mounted disk image, from a Gatekeeper-translocated
            // location, or from a dev-marked install, so no check thread ever starts
            // and every ledger field below is the pristine default of a machine that
            // structurally cannot update (2026-08-19 round-5 audit).
            "This copy of aterm can\u{2019}t update itself.".to_string()
        } else if !self.enabled {
            "Automatic updates are off.".to_string()
        } else if self.failing_persistent {
            // NEVER "up to date" while the ledger says otherwise: for eight hours on
            // 2026-08-18 every check was rejected publisher-side and this line said
            // "You're up to date." The cause rides in `outcome` (the same sentence
            // `aterm ctl update status` prints), shown as the detail below.
            "Updates are failing on this Mac.".to_string()
        } else {
            "You\u{2019}re up to date.".to_string()
        }
    }

    /// The secondary detail line under the headline: the staged build's version + number
    /// (`Some` only when a build is ready), rendered small under the accent headline.
    fn detail(&self) -> Option<String> {
        if !self.installable {
            return Some(
                "Move aterm.app to your Applications folder and open it from there. \
                 A copy running from a disk image, a quarantined download, or a local \
                 build is never replaced in place."
                    .to_string(),
            );
        }
        if let Some((b, v)) = self.staged.as_ref() {
            // Not the ledger `outcome`: the check lane rewrites that every cycle with
            // the healthy "staged … ready to apply" sentence while a stage is held.
            // The durable fact is the class that is failing.
            if self.enabled && self.apply_is_failing() {
                return Some(format!(
                    "Version {v} \u{00b7} build {b} \u{00b7} every attempt to start it has \
                     failed; see aterm.log"
                ));
            }
            if self.enabled && self.failing_persistent {
                // The class is named only when it is the one that ESCALATED. A single
                // apply failure under an escalated `pipeline` streak leaves
                // `failing_kind = "apply"` — naming it there told the user their
                // staged build would not start when nothing of the kind had been
                // established (2026-08-19 round-5 audit).
                let named = match self.failing_kind.as_str() {
                    "apply" | "" => String::new(),
                    kind => format!(" ({kind})"),
                };
                return Some(format!(
                    "Version {v} \u{00b7} build {b} \u{00b7} ready to install; but update \
                     CHECKS are failing{named}, so newer builds may not arrive — see aterm.log"
                ));
            }
            return Some(format!("Version {v} \u{00b7} build {b}"));
        }
        if self.failing_persistent && !self.checking && self.enabled {
            let cause = self.outcome.trim();
            return Some(if cause.is_empty() {
                "Every recent check failed the same way; run `aterm ctl update status` for the ledger.".to_string()
            } else {
                cause.to_string()
            });
        }
        None
    }

    /// `(scroll, total, visible)` for `controls front`. Update does not scroll; it shows at
    /// most `MAX_NOTES` changelog lines.
    pub(crate) fn scroll_extent(&self) -> (usize, usize, usize) {
        let total = self.changelog.len();
        (0, total, total.min(MAX_NOTES))
    }

    /// Test projection for the retired standalone update card. The shipping
    /// `controls update` alias serializes the compiled native Settings `/updates` tree.
    #[cfg(test)]
    pub(crate) fn controls_lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        out.push(format!(
            "update current_version={:?} current_build={} enabled={}",
            self.current_version, self.current_build, self.enabled
        ));
        match &self.staged {
            Some((b, v)) => out.push(format!("update staged_build={b} staged_version={v:?}")),
            None => out.push("update staged_build=-".to_string()),
        }
        out.push(format!(
            "update checking={} headline={:?}",
            self.checking,
            self.headline()
        ));
        out.push(format!(
            "update notes_lines={}",
            self.changelog.len().min(MAX_NOTES)
        ));
        // The actions the card offers (so a driver knows what it can PRESS).
        out.push("update action=close".to_string());
        if self.enabled {
            out.push("update action=check".to_string());
        }
        if self.has_update() {
            out.push("update action=install".to_string());
        }
        out
    }

    /// Rows the card wants — enough for the header, headline, the (clamped) notes block,
    /// and the button row. Test-only ceil bound (live sizing is fractional in the layout).
    #[cfg(test)]
    pub(crate) fn card_rows(&self) -> usize {
        let notes = if self.staged.is_some() {
            self.changelog.len().min(MAX_NOTES) + 1
        } else {
            0
        };
        notes + 9
    }
}

/// The FIXED accessibility node id for each action button — the CONTRACT shared by the tree
/// builder ([`update_a11y`]) and the action decoder ([`a11y_hit`]), so an OS `Click` on a
/// button routes to exactly the [`UpdateHit`] the pixels painted there. The ids are fixed
/// (independent of which optional buttons are present) so the decoder is a plain match.
#[cfg(feature = "a11y-accesskit")]
fn a11y_button_id(hit: UpdateHit) -> accesskit::NodeId {
    accesskit::NodeId(match hit {
        UpdateHit::Close => 10,
        UpdateHit::Check => 11,
        UpdateHit::Install => 12,
    })
}

/// Decode an accessibility node id back to the [`UpdateHit`] its button fires — the inverse
/// of [`a11y_button_id`], consulted by the Update branch of `App::on_accessibility_action`.
/// `None` for the root / a static descriptor node (which carry no action).
#[cfg(all(test, feature = "a11y-accesskit"))]
pub(crate) fn a11y_hit(node: accesskit::NodeId) -> Option<UpdateHit> {
    match node.0 {
        10 => Some(UpdateHit::Close),
        11 => Some(UpdateHit::Check),
        12 => Some(UpdateHit::Install),
        _ => None,
    }
}

/// The retired Software Update card's accessibility tree reads the same [`UpdateState`]
/// as its pixels ([`update_tray`]). A window root parents static
/// [`accesskit::Role::Label`] nodes for the current-build line, the status headline, and the
/// (staged-only) version detail, plus one [`accesskit::Role::Button`] per PRESENT action —
/// Close always, Check iff `enabled`, Install iff [`UpdateState::has_update`] — one button
/// per `update action=…` line, each carrying [`accesskit::Action::Click`]. Focus is the
/// [`UpdateState::default_action`] button (the one a plain Return fires), so a screen reader
/// lands on the highlighted default.
///
/// Id contract (shared with [`a11y_hit`]): root `NodeId(0)`; static lines `NodeId(1..=3)`;
/// buttons at FIXED ids `Close=10` / `Check=11` / `Install=12`.
#[cfg(feature = "a11y-accesskit")]
pub(crate) fn update_a11y(state: &UpdateState) -> accesskit::TreeUpdate {
    use accesskit::{Action, Node, NodeId, Role, Tree, TreeId, TreeUpdate};

    let root_id = NodeId(0);
    let mut nodes: Vec<(NodeId, Node)> = Vec::new();
    let mut children: Vec<NodeId> = Vec::new();

    // Static descriptor lines (the same strings the card paints).
    let mut label = |id: u64, key: &str, value: String| {
        let mut n = Node::new(Role::Label);
        n.set_label(key.to_string());
        n.set_value(value);
        nodes.push((NodeId(id), n));
        children.push(NodeId(id));
    };
    label(1, "current", current_line(state));
    label(2, "status", state.headline());
    if let Some(detail) = state.detail() {
        label(3, "detail", detail);
    }

    // One button per PRESENT action (in bijection with the `update action=…` lines).
    let mut button = |hit: UpdateHit, text: &str| {
        let id = a11y_button_id(hit);
        let mut n = Node::new(Role::Button);
        n.set_label(text.to_string());
        n.add_action(Action::Click);
        nodes.push((id, n));
        children.push(id);
    };
    button(UpdateHit::Close, "Close");
    if state.enabled {
        button(UpdateHit::Check, "Check for Updates");
    }
    if state.has_update() {
        button(UpdateHit::Install, "Install & Relaunch");
    }

    let mut root = Node::new(Role::Window);
    root.set_label(TITLE);
    root.set_children(children);
    nodes.push((root_id, root));

    TreeUpdate {
        nodes,
        tree: Some(Tree::new(root_id)),
        tree_id: TreeId::ROOT,
        focus: a11y_button_id(state.default_action()),
    }
}

/// What a left click on the Update overlay hits. The close dot + Close button both close;
/// Check runs a fresh check; Install applies the staged build (re-exec).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum UpdateHit {
    Close,
    Check,
    Install,
}

/// A rect in tray px: `(x, y, w, h)`.
type Rect = (f32, f32, f32, f32);

/// The pixel layout of the Update card, computed ONCE and consumed by BOTH the painter
/// ([`update_tray`]) and the hit-test ([`update_hit`]).
pub(crate) struct UpdateLayout {
    pub(crate) card: Rect,
    pub(crate) title_h: f32,
    pub(crate) close_dot: (f32, f32, f32),
    pub(crate) close: Rect,
    /// Left inset x for body text.
    pub(crate) body_x: f32,
    /// Top-y for the current-build line, the big status headline, and (when a build is
    /// staged) the version detail line under the headline.
    pub(crate) current_y: f32,
    pub(crate) headline_y: f32,
    pub(crate) detail_y: f32,
    /// Top of the notes block (each subsequent line is `+ note_ch`), and the notes line
    /// height (slightly smaller than a grid row).
    pub(crate) notes_y: f32,
    pub(crate) note_ch: f32,
    /// The button rects (present-or-degenerate). `check`/`install` are zero-sized when the
    /// action is unavailable, so the hit-test naturally misses them.
    pub(crate) close_btn: Rect,
    pub(crate) check_btn: Rect,
    pub(crate) install_btn: Rect,
}

const TITLE: &str = "Software Update";

/// Compute the card layout: a content-sized card centred in the `cols × panel_rows` tray,
/// with a right-aligned button row at the bottom.
pub(crate) fn update_layout(state: &UpdateState, g: &SettingsGeom) -> UpdateLayout {
    let (cw, ch, px) = (g.cw, g.ch, g.font_px);
    let tray_w = g.cols as f32 * cw;
    let tray_h = g.panel_rows as f32 * ch;
    let note_ch = ch * 0.82;

    let staged = state.has_update();
    let notes = if staged {
        state.changelog.len().min(MAX_NOTES)
    } else {
        0
    };
    let has_notes = staged && notes > 0;

    // Content width: the widest of the header lines, the notes, and the button row. UI-face
    // strings measure with `ui_text_width`; the monospace changelog with `text_w`.
    let notes_w = state
        .changelog
        .iter()
        .take(MAX_NOTES)
        .map(|l| text_w(l, px * 0.82))
        .fold(0.0, f32::max);
    let btn_row_w = ui_text_width("Check for Updates", px * 0.85)
        + ui_text_width("Install & Relaunch", px * 0.85)
        + ui_text_width("Close", px * 0.85)
        + 12.0 * cw;
    let cur = current_line(state);
    let content_w = [
        ui_text_width(&state.headline(), px * 1.15),
        state.detail().map_or(0.0, |d| ui_text_width(&d, px * 0.9)),
        ui_text_width(&cur, px * 0.9),
        notes_w,
        btn_row_w,
        ui_text_width(TITLE, px * 0.98) + 6.0 * cw,
    ]
    .into_iter()
    .fold(0.0, f32::max);
    let card_w = (content_w + 5.0 * cw)
        .max(32.0 * cw)
        .min(tray_w - cw)
        .max(cw);

    // Vertical budget as RELATIVE offsets from the content top (`title_h` down), so
    // `card_h` is computed before `cy0` (no circular dependency). The detail line + the
    // notes block only take space when a build is staged.
    let title_h = title_h_of(ch);
    let off_current = 0.6 * ch;
    let off_headline = off_current + 1.4 * ch;
    let off_detail = off_headline + 1.55 * ch;
    let off_notes =
        off_detail + if staged { 1.2 * ch } else { 0.0 } + if has_notes { 0.6 * ch } else { 0.0 };
    let notes_block_h = if has_notes {
        1.35 * note_ch + notes as f32 * note_ch + 0.35 * ch
    } else {
        0.0
    };
    let content_off = off_notes + notes_block_h;
    let button_area = 1.95 * ch; // gap + button row + bottom margin
    let card_h = (title_h + content_off + button_area)
        .min(tray_h - 0.4 * ch)
        .max(ch);

    let cx0 = ((tray_w - card_w) * 0.5).max(0.0);
    let cy0 = ((tray_h - card_h) * 0.5).max(0.0);
    let base = cy0 + title_h;
    let r = (0.27 * ch).clamp(4.0, 7.5);
    let (dot_cx, dot_cy) = (cx0 + 1.5 * cw, cy0 + title_h * 0.5);
    let body_x = cx0 + 2.5 * cw;
    let current_y = base + off_current;
    let headline_y = base + off_headline;
    let detail_y = base + off_detail;
    let notes_y = base + off_notes;

    // Button row along the bottom-right.
    let btn_h = 1.2 * ch;
    let by = cy0 + card_h - btn_h - 0.5 * ch;
    let mk = |right: f32, label: &str| -> (Rect, f32) {
        let w = ui_text_width(label, px * 0.85) + 2.6 * cw;
        ((right - w, by, w, btn_h), right - w - 0.8 * cw)
    };
    let right = cx0 + card_w - 1.4 * cw;
    let (close_btn, next) = mk(right, "Close");
    let (install_btn, next) = if staged {
        mk(next, "Install & Relaunch")
    } else {
        ((next, by, 0.0, 0.0), next)
    };
    let (check_btn, _) = if state.enabled {
        mk(next, "Check for Updates")
    } else {
        ((next, by, 0.0, 0.0), next)
    };

    UpdateLayout {
        card: (cx0, cy0, card_w, card_h),
        title_h,
        close_dot: (dot_cx, dot_cy, r),
        close: (dot_cx - 1.8 * r, dot_cy - 1.8 * r, 3.6 * r, 3.6 * r),
        body_x,
        current_y,
        headline_y,
        detail_y,
        notes_y,
        note_ch,
        close_btn,
        check_btn,
        install_btn,
    }
}

/// The running-build line, e.g. `aterm v0.5.14 · build 828`.
fn current_line(state: &UpdateState) -> String {
    format!(
        "aterm v{} \u{00b7} build {}",
        state.current_version, state.current_build
    )
}

fn title_h_of(ch: f32) -> f32 {
    1.4 * ch
}

/// Map a tray-px point to what it hits ([`UpdateHit`]) — the EXACT rects [`update_tray`]
/// painted. Points inside the card but on no control return `None` (still swallowed —
/// modal); points outside the card also return `None`.
pub(crate) fn update_hit(
    state: &UpdateState,
    g: &SettingsGeom,
    x: f32,
    y: f32,
) -> Option<UpdateHit> {
    let l = update_layout(state, g);
    let hit =
        |r: Rect| r.2 > 0.0 && r.3 > 0.0 && x >= r.0 && x < r.0 + r.2 && y >= r.1 && y < r.1 + r.3;
    if !hit(l.card) {
        return None;
    }
    if hit(l.close) || hit(l.close_btn) {
        return Some(UpdateHit::Close);
    }
    if hit(l.install_btn) {
        return Some(UpdateHit::Install);
    }
    if hit(l.check_btn) {
        return Some(UpdateHit::Check);
    }
    None
}

/// The close dot's darker rim.
fn dim(c: [u8; 3]) -> [u8; 3] {
    c.map(|v| (u16::from(v) * 3 / 4) as u8)
}

/// Paint the Software Update card: drop shadow, opaque rounded surface + hairline, a title
/// bar with the close dot + "Software Update", the current-build line, the status headline
/// (accent-tinted when an update is ready), the "What's new" notes, and the button row.
pub(crate) fn update_tray(state: &UpdateState, g: &SettingsGeom, theme: Theme) -> TrayInput {
    let r = Roles::from_theme(theme);
    let l = update_layout(state, g);
    let (cw, ch, px) = (g.cw, g.ch, g.font_px);
    let (cx0, cy0, card_w, card_h) = l.card;
    let radius = (ch * 0.5).min(11.0);
    let mut prims: Vec<DrawPrim> = vec![
        // Two-step drop shadow.
        DrawPrim::Panel {
            x: cx0 - 3.0,
            y: cy0 + 2.0,
            w: card_w + 6.0,
            h: card_h + 6.0,
            radius: radius + 3.0,
            fill: rgba([0, 0, 0], 0x2A),
            blur: false,
        },
        DrawPrim::Panel {
            x: cx0 - 1.0,
            y: cy0 + 2.0,
            w: card_w + 2.0,
            h: card_h + 3.0,
            radius: radius + 1.0,
            fill: rgba([0, 0, 0], 0x30),
            blur: false,
        },
        // Opaque window body.
        DrawPrim::Panel {
            x: cx0,
            y: cy0,
            w: card_w,
            h: card_h,
            radius,
            fill: rgba(r.surface, 0xFF),
            blur: false,
        },
        DrawPrim::ClipPush {
            x: cx0,
            y: cy0,
            w: card_w,
            h: card_h,
        },
        // Title bar + hairline + close dot + caption.
        DrawPrim::ClipPush {
            x: cx0,
            y: cy0,
            w: card_w,
            h: l.title_h,
        },
        DrawPrim::Panel {
            x: cx0,
            y: cy0,
            w: card_w,
            h: card_h,
            radius,
            fill: rgba(r.elevated, 0xFF),
            blur: false,
        },
        DrawPrim::ClipPop,
        DrawPrim::Stroke {
            x: cx0,
            y: cy0 + l.title_h,
            w: card_w,
            h: 1.0,
            radius: 0.0,
            width: 1.0,
            color: rgba(r.separator, 0xFF),
        },
    ];
    let (dot_cx, dot_cy, dot_r) = l.close_dot;
    prims.push(DrawPrim::Dot {
        cx: dot_cx,
        cy: dot_cy,
        r: dot_r,
        color: rgba(r.danger, 0xFF),
        breathe: false,
    });
    prims.push(DrawPrim::Stroke {
        x: dot_cx - dot_r,
        y: dot_cy - dot_r,
        w: 2.0 * dot_r,
        h: 2.0 * dot_r,
        radius: dot_r,
        width: 1.0,
        color: rgba(dim(r.danger), 0xFF),
    });
    // Title bar caption: semibold native face (matches the settings-v2 pane title).
    // Title px*0.98 snaps to the Body step; semibold native face.
    let tsize = TypeStep::Body.px(px);
    let tx = (cx0 + (card_w - ui_text_width(TITLE, tsize.get())) * 0.5).max(cx0 + cw);
    prims.push(text_prim(
        tx,
        row_baseline(cy0, l.title_h, tsize.get()),
        TITLE.to_string(),
        tsize,
        TextWeight::Regular,
        TextFace::UiBold,
        rgba(r.text_primary, 0xFF),
    ));

    // Current build — a secondary native descriptor line.
    let csize = TypeStep::Secondary.px(px);
    prims.push(text_prim(
        l.body_x,
        row_baseline(l.current_y, ch, csize.get()),
        current_line(state),
        csize,
        TextWeight::Regular,
        TextFace::Ui,
        rgba(r.text_secondary, 0xFF),
    ));
    // Status headline — the big semibold statement, accent when an update is READY (so the
    // staged state is unmistakable), otherwise primary/secondary. `Checking…` reads calm.
    let head_color = if state.has_update() {
        r.accent
    } else if state.checking {
        r.text_secondary
    } else {
        r.text_primary
    };
    let hsize = TypeStep::Title.px(px);
    prims.push(text_prim(
        l.body_x,
        row_baseline(l.headline_y, ch, hsize.get()),
        state.headline(),
        hsize,
        TextWeight::Regular,
        TextFace::UiBold,
        rgba(head_color, 0xFF),
    ));
    // Version detail under the headline (staged only).
    if let Some(detail) = state.detail() {
        let dsize = TypeStep::Secondary.px(px);
        prims.push(text_prim(
            l.body_x,
            row_baseline(l.detail_y, ch, dsize.get()),
            detail,
            dsize,
            TextWeight::Regular,
            TextFace::Ui,
            rgba(r.text_secondary, 0xFF),
        ));
    }

    // "What's new" notes (only when a build is staged): a hairline section rule, a native
    // caption, then the changelog body in the MONO terminal face (release notes read as
    // code/prose — monospace is right here).
    if state.has_update() && !state.changelog.is_empty() {
        let rule_y = l.notes_y - 0.35 * ch;
        prims.push(DrawPrim::Stroke {
            x: l.body_x,
            y: rule_y,
            w: (cx0 + card_w - 2.5 * cw) - l.body_x,
            h: 1.0,
            radius: 0.0,
            width: 1.0,
            color: rgba(r.separator, 0xFF),
        });
        // Section header + changelog body share the Caption step (px*0.8 / 0.82).
        let cap = TypeStep::Caption.px(px);
        prims.push(text_prim(
            l.body_x,
            row_baseline(l.notes_y, ch, cap.get()),
            "What\u{2019}s new".to_string(),
            cap,
            TextWeight::Regular,
            TextFace::Ui,
            rgba(r.text_tertiary, 0xFF),
        ));
        let mut ny = l.notes_y + 1.35 * l.note_ch;
        for line in state.changelog.iter().take(MAX_NOTES) {
            prims.push(text_prim(
                l.body_x,
                row_baseline(ny, l.note_ch, cap.get()),
                line.clone(),
                cap,
                TextWeight::Regular,
                TextFace::Mono,
                rgba(r.text_secondary, 0xFF),
            ));
            ny += l.note_ch;
        }
    }

    // Button row — native buttons: accent-filled DEFAULT vs. bordered secondary, native
    // UI labels centred with the matching metric.
    let button = |prims: &mut Vec<DrawPrim>, rect: Rect, label: &str, filled: bool| {
        let (bx, by, bw, bh) = rect;
        if bw <= 0.0 {
            return;
        }
        let (fill, fg) = if filled {
            (rgba(r.accent, 0xFF), r.on_accent)
        } else {
            (rgba(r.elevated, 0xFF), r.text_primary)
        };
        prims.push(DrawPrim::Panel {
            x: bx,
            y: by,
            w: bw,
            h: bh,
            radius: bh * 0.3,
            fill,
            blur: false,
        });
        if !filled {
            prims.push(DrawPrim::Stroke {
                x: bx,
                y: by,
                w: bw,
                h: bh,
                radius: bh * 0.3,
                width: 1.0,
                color: rgba(r.separator, 0xFF),
            });
        }
        // Button labels px*0.85 snap to the Secondary step (matches the About OK button).
        let bsize = TypeStep::Secondary.px(px);
        prims.push(text_prim(
            bx + (bw - ui_text_width(label, bsize.get())) * 0.5,
            row_baseline(by, bh, bsize.get()),
            label.to_string(),
            bsize,
            TextWeight::Regular,
            TextFace::Ui,
            rgba(fg, 0xFF),
        ));
    };
    // Install is the accent-filled DEFAULT action when present; else Check is highlighted.
    button(
        &mut prims,
        l.check_btn,
        "Check for Updates",
        !state.has_update() && state.enabled,
    );
    button(&mut prims, l.install_btn, "Install & Relaunch", true);
    button(&mut prims, l.close_btn, "Close", false);

    prims.push(DrawPrim::ClipPop);
    prims.push(DrawPrim::Stroke {
        x: cx0,
        y: cy0,
        w: card_w,
        h: card_h,
        radius,
        width: 1.0,
        color: rgba(r.separator, 0xFF),
    });

    TrayInput {
        prims,
        card: l.card,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn staged_status() -> aterm_update::UpdateStatus {
        aterm_update::UpdateStatus {
            enabled: true,
            current_build: 828,
            staged_build: Some(830),
            staged_version: Some("0.5.15".to_string()),
            staged_commit: Some("deadbeefcafe".to_string()),
            staged_dmg_sha256: Some("ab".repeat(32)),
            changelog: Some("### Features\n- **DSU**: hot-swap\n- faster startup".to_string()),
            outcome: "ok".to_string(),
            updated_at: String::new(),
            failing_checks: 0,
            failing_kind: String::new(),
            failing_applies: 0,
            installable: true,
            failing_since: String::new(),
            failing_persistent: false,
            rescues: 0,
        }
    }

    fn geom(s: &UpdateState) -> SettingsGeom {
        SettingsGeom {
            cw: 9.0,
            ch: 20.0,
            font_px: 14.0,
            cols: 120,
            panel_rows: s.card_rows() + 8,
        }
    }

    /// A staged build whose apply keeps failing persistently is not "Update ready":
    /// the headline says so and the detail names the failing class, not the
    /// churned ledger outcome.
    /// A copy that structurally CANNOT be replaced — run from the mounted DMG, a
    /// Gatekeeper-translocated download, or a dev-marked install — must say so. No
    /// check thread ever starts in that state, so every ledger field is the pristine
    /// default and the panel otherwise reported the confident "You're up to date" of
    /// a machine that will never update (2026-08-19 round-5 audit).
    #[test]
    fn a_copy_that_cannot_replace_itself_says_so_instead_of_up_to_date() {
        let mut st = staged_status();
        st.staged_build = None;
        st.staged_version = None;
        st.staged_dmg_sha256 = None;
        st.changelog = None;
        st.installable = false;
        let s = UpdateState::from_status(828, "0.5.14", Some(&st), false);
        let p = s.projection();
        assert_eq!(p.headline, "This copy of aterm can\u{2019}t update itself.");
        let detail = p.detail.expect("detail");
        assert!(detail.contains("Applications"), "…and how to fix it: {detail}");
        // The same state WITH a replaceable bundle is the ordinary healthy line.
        st.installable = true;
        let ok = UpdateState::from_status(828, "0.5.14", Some(&st), false).projection();
        assert_eq!(ok.headline, "You\u{2019}re up to date.");
    }

    #[test]
    fn a_staged_build_that_keeps_failing_to_apply_says_so() {
        let mut st = staged_status();
        st.failing_applies = 3;
        st.failing_kind = "apply".to_string();
        st.failing_persistent = true;
        st.outcome = "staged 0.5.15 (build 830) — verified and ready to apply".to_string();
        let s = UpdateState::from_status(828, "0.5.14", Some(&st), false);
        let p = s.projection();
        assert!(p.failing_persistent, "the projection no longer masks a failure while staged");
        assert_eq!(p.headline, "Update ready, but it keeps failing to apply.");
        let detail = p.detail.expect("detail");
        assert!(detail.contains("build 830") && detail.contains("failed"), "{detail}");
        assert!(!detail.contains("ready to apply"), "not the ledger's healthy sentence: {detail}");

        // AN ACQUISITION-CLASS streak is a different statement: the stage in hand is
        // fine and will install; what is failing is fetching the NEXT build.
        let mut acquiring = staged_status();
        acquiring.failing_checks = 3;
        acquiring.failing_kind = "manifest".to_string();
        acquiring.failing_persistent = true;
        let s = UpdateState::from_status(828, "0.5.14", Some(&acquiring), false);
        let p = s.projection();
        assert!(p.failing_persistent && !p.apply_is_failing);
        assert_eq!(p.headline, "Update ready");
        let detail = p.detail.expect("detail");
        assert!(detail.contains("CHECKS are failing") && detail.contains("manifest"), "{detail}");
    }

    #[test]
    fn snapshot_renders_staged_update_notes() {
        let st = staged_status();
        let s = UpdateState::from_status(828, "0.5.14", Some(&st), false);
        assert!(s.has_update());
        // Markdown is rendered to clean lines (no raw `###`/`**`).
        assert!(s.changelog.iter().any(|l| l.contains("DSU: hot-swap")));
        assert!(
            !s.changelog
                .iter()
                .any(|l| l.contains('#') || l.contains("**"))
        );
        // The headline is the short statement; the version lives in the detail line.
        assert_eq!(s.headline(), "Update ready");
        assert!(s.detail().unwrap().contains("0.5.15"));
    }

    #[test]
    fn install_and_check_and_close_hit() {
        let st = staged_status();
        let s = UpdateState::from_status(828, "0.5.14", Some(&st), false);
        let g = geom(&s);
        let l = update_layout(&s, &g);
        let mid = |r: Rect| (r.0 + r.2 * 0.5, r.1 + r.3 * 0.5);
        let (ix, iy) = mid(l.install_btn);
        assert_eq!(update_hit(&s, &g, ix, iy), Some(UpdateHit::Install));
        let (cx, cy) = mid(l.check_btn);
        assert_eq!(update_hit(&s, &g, cx, cy), Some(UpdateHit::Check));
        let (clx, cly) = mid(l.close_btn);
        assert_eq!(update_hit(&s, &g, clx, cly), Some(UpdateHit::Close));
        let (dx, dy, _) = l.close_dot;
        assert_eq!(update_hit(&s, &g, dx, dy), Some(UpdateHit::Close));
    }

    #[test]
    fn default_action_tracks_the_highlighted_button() {
        // The button a plain Return fires must be the one painted as the highlighted
        // default in every state (else the visible default and the keyboard disagree).
        use UpdateHit::{Check, Close, Install};
        let up_to_date = |enabled| {
            UpdateState::from_status(
                828,
                "0.5.14",
                Some(&aterm_update::UpdateStatus {
                    enabled,
                    staged_build: None,
                    staged_version: None,
                    changelog: None,
                    ..staged_status()
                }),
                false,
            )
        };
        // Staged → Install is the accent default; Return installs.
        let staged = UpdateState::from_status(828, "0.5.14", Some(&staged_status()), false);
        assert_eq!(staged.default_action(), Install);
        // Up to date + checks enabled → Check is the highlighted default; Return checks,
        // and the Check button is really painted (a non-degenerate rect to land on).
        let live = up_to_date(true);
        assert_eq!(live.default_action(), Check);
        assert!(update_layout(&live, &geom(&live)).check_btn.2 > 0.0);
        // Up to date + checks disabled → only Close is offered; Return closes.
        let off = up_to_date(false);
        assert_eq!(off.default_action(), Close);
        assert_eq!(
            update_layout(&off, &geom(&off)).check_btn.2,
            0.0,
            "no Check button to default to when checks are disabled"
        );
    }

    #[test]
    fn no_update_hides_install_button() {
        let st = aterm_update::UpdateStatus {
            staged_build: None,
            staged_version: None,
            staged_commit: None,
            staged_dmg_sha256: None,
            changelog: None,
            ..staged_status()
        };
        let s = UpdateState::from_status(828, "0.5.14", Some(&st), false);
        assert!(!s.has_update());
        let g = geom(&s);
        let l = update_layout(&s, &g);
        assert_eq!(
            l.install_btn.2, 0.0,
            "install button is degenerate when nothing staged"
        );
        // A click where Install would be misses (no install action).
        let (ix, iy) = (l.install_btn.0, l.install_btn.1);
        assert_ne!(update_hit(&s, &g, ix, iy), Some(UpdateHit::Install));
        assert!(s.headline().contains("up to date"));
    }

    #[test]
    fn checking_headline_and_controls() {
        let s = UpdateState::from_status(828, "0.5.14", None, true);
        assert!(s.headline().contains("Checking"));
        let lines = s.controls_lines();
        assert!(lines.iter().any(|l| l.contains("checking=true")));
        assert!(lines.iter().any(|l| l.starts_with("update action=close")));
    }

    #[test]
    fn tray_paints_title_and_buttons() {
        let st = staged_status();
        let s = UpdateState::from_status(828, "0.5.14", Some(&st), false);
        let g = geom(&s);
        let t = update_tray(&s, &g, Theme::default());
        let has = |needle: &str| {
            t.prims
                .iter()
                .any(|p| matches!(p, DrawPrim::Text { s, .. } if s == needle))
        };
        assert!(has("Software Update"), "title");
        assert!(has("Install & Relaunch"), "install button");
        assert!(has("Close"), "close button");
        let pushes = t
            .prims
            .iter()
            .filter(|p| matches!(p, DrawPrim::ClipPush { .. }))
            .count();
        let pops = t
            .prims
            .iter()
            .filter(|p| matches!(p, DrawPrim::ClipPop))
            .count();
        assert_eq!(pushes, pops, "clip stack balanced");
    }

    /// ANTI-DIVERGENCE: for each `update action={close|check|install}` line there is exactly
    /// one clickable [`Role::Button`] node (at its fixed id), and NO button for an absent
    /// action — so the a11y buttons are in bijection with the introspected/painted actions.
    /// The focus (default action) differing between the staged and disabled cases is the
    /// non-vacuity control: the tree tracks the model, not a fixed button list.
    #[cfg(feature = "a11y-accesskit")]
    #[test]
    fn update_a11y_actions_match_controls() {
        use accesskit::{Action, Role};

        let count_buttons = |u: &accesskit::TreeUpdate| {
            u.nodes
                .iter()
                .filter(|(_, n)| n.role() == Role::Button)
                .count()
        };
        let assert_bijection = |s: &UpdateState| {
            let lines = s.controls_lines();
            let tree = update_a11y(s);
            let mut action_lines = 0usize;
            for line in &lines {
                let Some(a) = line.strip_prefix("update action=") else {
                    continue;
                };
                action_lines += 1;
                let hit = match a {
                    "close" => UpdateHit::Close,
                    "check" => UpdateHit::Check,
                    "install" => UpdateHit::Install,
                    other => panic!("unknown update action {other}"),
                };
                let id = a11y_button_id(hit);
                let node = tree
                    .nodes
                    .iter()
                    .find(|(nid, _)| *nid == id)
                    .map(|(_, n)| n)
                    .unwrap_or_else(|| panic!("no button node for action {a}"));
                assert_eq!(node.role(), Role::Button);
                assert!(node.supports_action(Action::Click));
                assert_eq!(a11y_hit(id), Some(hit), "decoder inverts the id scheme");
            }
            assert_eq!(
                action_lines,
                count_buttons(&tree),
                "one button per action line, none extra"
            );
        };

        // Staged → Close + Check + Install; Install is the accent default ⇒ focus.
        let staged = UpdateState::from_status(828, "0.5.14", Some(&staged_status()), false);
        assert_bijection(&staged);
        assert_eq!(count_buttons(&update_a11y(&staged)), 3);
        assert_eq!(
            update_a11y(&staged).focus,
            a11y_button_id(UpdateHit::Install)
        );

        // Up to date + checks disabled → ONLY Close; focus falls back to Close.
        let off = UpdateState::from_status(
            828,
            "0.5.14",
            Some(&aterm_update::UpdateStatus {
                enabled: false,
                staged_build: None,
                staged_version: None,
                changelog: None,
                ..staged_status()
            }),
            false,
        );
        assert_bijection(&off);
        let off_tree = update_a11y(&off);
        assert_eq!(count_buttons(&off_tree), 1, "no Check/Install when absent");
        assert!(a11y_hit(off_tree.focus) == Some(UpdateHit::Close));
        assert!(
            !off_tree
                .nodes
                .iter()
                .any(|(nid, _)| *nid == a11y_button_id(UpdateHit::Install)),
            "no Install button node when nothing is staged"
        );
    }

    /// Gated visual preview (`ATERM_UPDATE_PREVIEW=path`) → PNG of the staged (rich) state.
    #[test]
    fn preview_update_overlay() {
        let Ok(path) = std::env::var("ATERM_UPDATE_PREVIEW") else {
            return;
        };
        let st = staged_status();
        let s = UpdateState::from_status(828, "0.5.14", Some(&st), false);
        let (cw, ch, px) = (16.0_f32, 34.0_f32, 26.0_f32);
        let cols = 96usize;
        let panel_rows = s.card_rows() + 4;
        let g = SettingsGeom {
            cw,
            ch,
            font_px: px,
            cols,
            panel_rows,
        };
        let tray = update_tray(&s, &g, Theme::default());
        let (buf, pw, ph) = crate::tray_raster::rasterize_tray(
            &tray.prims,
            (cols as f32 * cw) as u32,
            (panel_rows as f32 * ch) as u32,
            1.0,
            [22, 24, 30, 255],
        );
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, pw, ph);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut wr = enc.write_header().unwrap();
            wr.write_image_data(&buf).unwrap();
        }
        std::fs::write(&path, &out).unwrap();
    }
}
