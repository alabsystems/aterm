// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The native macOS window chrome attached to each aterm window: a SINGLE compact
//! Ghostty-style row hosting the traffic lights, a full-width VIEW-BASED TAB STRIP, and
//! a trailing "+" New Tab button — all in ONE titlebar row. (The accent "Update"
//! capsule that used to join the "+" is RETIRED: the owner asked for the update
//! affordance to live in the VERSION menu — see `crate::menu::update_version_menu` —
//! where ONE click applies the staged build.)
//!
//! ONE-ROW LAYOUT: a `UnifiedCompact`-style `NSToolbar` (`NSWindowToolbarStyle::
//! UnifiedCompact`) collapses the titlebar + toolbar into a SINGLE short row — the
//! traffic lights, the tab strip, and the trailing button all in line — with the
//! terminal content view sitting BELOW it (no occlusion). The title is hidden
//! (`setTitleVisibility:` Hidden) and the titlebar transparent for the seamless,
//! title-less Ghostty look. The LEADING edge is deliberately clean: nothing sits
//! between the traffic lights and the first tab, so there is no icon to misalign with
//! the stoplights.
//!
//! The toolbar holds exactly ONE custom-view `NSToolbarItem` whose view is the
//! full-width container `NSView`: it hosts one [`TabView`] sub-view PER tab laid out
//! left→right from the leading pad, and the right-pinned "+" [`ChromeButton`]. The tab
//! views let each tab carry an explicit theme-aware selected surface, accent keyline,
//! and semibold label; a per-tab CLOSE ✕ revealed on hover via an `NSTrackingArea`,
//! and a drag-to-reorder gesture.
//!
//! SHAPED LIKE macOS TERMINAL'S TAB BAR, on three counts:
//!   * ALIGNED TO THE STOPLIGHTS. Every vertical position in the strip — chips, the
//!     "+", the ✕, app icons, status dots — is derived from the traffic lights
//!     MEASURED on the live window each refresh ([`macos::strip_metrics`]), not from
//!     the row AppKit happened to hand the toolbar item, and the band's leading edge
//!     is held clear of their trailing edge. (Measured before: the chips floated 2pt
//!     high — the exact misalignment that reads as "the tab bar is bolted on".)
//!   * THE ✕ HIDES. It appears only while the pointer is inside a chip — the selected
//!     tab included, because a permanent ✕ on the tab you are looking at is the one
//!     that gets mis-clicked. Its slot is reserved whether or not it is painted, so
//!     the reveal never reflows the title, and the title is CENTRED in its chip.
//!   * THE BAND IS SPENT, NOT RATIONED. Chips split the whole band into EQUAL shares
//!     with no maximum width, so a wide window buys longer titles rather than bare
//!     titlebar past a capped chip.
//!
//! ONE TAB IS A TITLE, NOT A SWITCHER. With a single tab there is nothing to switch
//! between, so the lone chip drops its pill and its ✕ and becomes the window's title:
//! the name beside its DESCRIPTION (from the same composed session chrome the hover
//! card renders), centred on the WINDOW. Opening a second tab turns both back into
//! chips.
//!
//! Like the menu bar (`menu.rs`), this chrome adds NO new behavior: each affordance is
//! a thin DISPATCH stub that posts a `Wake` the main loop turns into an existing `App`
//! command — never a parallel path:
//!   * a `mouseDown:` on a [`TabView`] posts
//!     [`Wake::SelectTab`](crate::Wake) `{ window, index }` → `App::switch_tab_in`;
//!   * a click on a tab's CLOSE × posts
//!     [`Wake::CloseTab`](crate::Wake) `{ window, index }` → `App::close_tab_at`;
//!   * a `mouseDragged:` reorder posts a `tab move`-equivalent
//!     [`Wake::TabCmd`](crate::Wake) `{ Move }` → `App::move_tab`;
//!   * a click on the "+" [`ChromeButton`] posts the SAME
//!     [`Wake::MenuAction`](crate::Wake) File ▸ New Tab
//!     ([`MenuAction::NewTab`](crate::menu::MenuAction::NewTab)) → `App::open_tab`.
//!
//! Three small Objective-C objects back the chrome, mirroring `menu.rs`:
//!   * a [`ChromeButton`] — a custom `NSView` that draws a NATIVE button affordance
//!     (a rounded hover highlight + a pressed state for the quiet "+" icon; an accent
//!     capsule style is kept as infra for future CTA buttons), centers its label,
//!     tracks hover/press via an `NSTrackingArea` + the mouse responder chain, and
//!     relays its `Wake::MenuAction` on click. It owns the proxy +
//!     [`WindowId`](crate::WindowId) and its bound action, so the button looks and
//!     behaves like a real button;
//!   * a [`TabView`] — a custom `NSView` subclass, ONE per tab, owning the proxy +
//!     window + its tab index + active flag, plus its title `NSTextField` label, its
//!     solo description label, and a close `NSButton`. It draws the (in)active
//!     background/accent in `drawRect:`, tracks hover (`mouseEntered:`/`mouseExited:`)
//!     to reveal the ✕, and turns a `mouseDown:`/`mouseDragged:` gesture into select /
//!     reorder `Wake`s. ALL of its geometry — both modes — is placed by the single
//!     `TabView::relayout` seam, so the painted ornaments can never disagree with the
//!     laid-out text;
//!   * a [`ToolbarDelegate`] — an `NSObject` conforming to `NSToolbarDelegate` that
//!     vends the single strip-item identifier and builds its custom-view item.
//!
//! ALWAYS-ON CHROME: the toolbar and every live tab identity are ALWAYS visible. A
//! one-tab Settings or terminal window therefore still says what it is, while the
//! trailing "+" keeps New Tab one click away. Chips fill the band from the leading pad
//! up to that pinned action. The strip is kept in sync with app state by
//! [`set_window_tabs`], called from `App::sync_window` (via `App::refresh_window_tabs`)
//! after every tab open/close/switch/detach/migrate.
//!
//! AppKit holds a toolbar's delegate and an item's view only WEAKLY, so
//! [`install_window_toolbar`] returns a [`ToolbarHandle`] retaining the delegate, the
//! toolbar, the container view, the live `TabView`s, and the "+" [`ChromeButton`];
//! `App` keeps it in a field for the window's life so the callbacks/actions stay live
//! (and so `set_window_tabs` can reach the container to rebuild it) — mirrors
//! `MenuHandle`.
//!
//! NEVER CRASH: an Objective-C exception crossing a Rust frame is a
//! non-unwinding abort, so this module only ever calls initializers documented
//! NOT to raise. Text fields come from `+labelWithString:` /
//! `+textFieldWithString:`, buttons from `+buttonWithTitle:target:action:`,
//! views from `-initWithFrame:`, and every AppKit call is on the main thread
//! behind an [`aterm_objc::MainThread`]. The rule outlived the binding crate
//! that motivated it: W7 ported this module off `objc2-app-kit` entirely, and
//! the constructor discipline is unchanged because it is a property of AppKit,
//! not of the binding.
//!
//! Everything imperative is `#[cfg(target_os = "macos")]`; off macOS no-op
//! [`install_window_toolbar`] / [`set_window_tabs`] and a unit [`ToolbarHandle`]
//! keep the workspace building everywhere, exactly like `menu.rs`.

// macOS-only window chrome: on Linux the install/handle/chrome helpers are no-op
// stubs and intentionally unused there.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

#[cfg(target_os = "macos")]
pub(crate) use macos::native_strip_container;
#[cfg(target_os = "macos")]
pub use macos::{
    ToolbarHandle, begin_tab_rename, can_present_tab_rename, end_tab_rename,
    install_window_toolbar, read_tab_chrome, read_tab_menus, rename_editor_edit,
    rename_editor_text, set_active_tab_color, set_strip_dark, set_update_available,
    set_window_tabs,
};

#[cfg(not(target_os = "macos"))]
pub use non_macos::{
    ToolbarHandle, install_window_toolbar, read_tab_chrome, read_tab_menus, set_active_tab_color,
    set_strip_dark, set_update_available, set_window_tabs,
};

use crate::tab_bar::TabStripMetadata;

/// Whether `next` differs from `current` only by the animation phase of a
/// conventional leading busy spinner: the braille `dots` cycle used by Codex
/// and similar TUIs, or the circle-fill "moon" cycle used by Claude Code.
///
/// The semantic suffix remains byte-identical. Chrome can therefore keep one stable
/// busy frame instead of asking AppKit to relayout the window title, tab label,
/// tooltip, and accessibility tree 6–10 times per second. A first spinner title, a
/// changed suffix, and the final non-spinner title are never coalesced.
#[must_use]
pub(crate) fn busy_spinner_phase_only_change(current: &str, next: &str) -> bool {
    if current == next {
        return false;
    }
    matches!(
        (busy_spinner_title_parts(current), busy_spinner_title_parts(next)),
        (Some((a, left)), Some((b, right))) if a != b && left == right
    )
}

pub(crate) fn busy_spinner_title_parts(title: &str) -> Option<(char, &str)> {
    let mut chars = title.chars();
    let frame = chars.next()?;
    let suffix = chars.as_str();
    (is_busy_spinner_frame(frame) && suffix.starts_with(' ')).then_some((frame, suffix))
}

#[must_use]
fn is_busy_spinner_frame(ch: char) -> bool {
    // cli-spinners' `dots` sequence, which is also the sequence Codex exposes in its
    // OSC title while work is in flight; plus the circle-fill "moon" phases
    // ◐◑◒◓◔◕ that Claude Code animates at ~1Hz. Any two distinct in-set frames
    // coalesce, across families too — same busy state, same semantic suffix.
    matches!(
        ch,
        '\u{280b}'
            | '\u{2819}'
            | '\u{2839}'
            | '\u{2838}'
            | '\u{283c}'
            | '\u{2834}'
            | '\u{2826}'
            | '\u{2827}'
            | '\u{2807}'
            | '\u{280f}'
            | '\u{25d0}'..='\u{25d5}'
    )
}

/// A single tab is still visible title chrome and owns the same context menu as a
/// tab in a larger strip.  Only an empty model has no menu to introspect.
#[must_use]
const fn tab_menu_introspection_visible(tab_count: usize) -> bool {
    tab_count != 0
}

/// Format the toolbar tab-switcher introspection line from the same complete state
/// that pixels and accessibility consume. A single tab remains visible and inspectable:
/// the active app/session identity is title chrome, not merely a multi-tab switcher.
/// `active` is clamped so stale input never publishes a bogus selection.
///
/// PURE: no I/O, no AppKit/winit — just string formatting from the tab model, so it
/// is unit-tested directly (see the `non_macos_tests` module). `icons` makes the
/// title-space policy observable: terminal entries are `None`, while native app
/// entries expose their stable identity. The macOS strip
/// computes the per-tab "title  ⌘N" label inside its `NSView` build; here the labels
/// are the raw session titles the caller passes (a full GTK4 header bar would render
/// the same ⌘-hint decoration, deferred — see [`install_window_toolbar`]).
///
/// This feeds [`read_tab_chrome`], whose consumer is the `chrome` verb's
/// platform-neutral `App::read_native_chrome` path. The live line is therefore
/// observable on non-macOS hosts as well as through the retained macOS views.
#[must_use]
pub(crate) fn format_tab_chrome(
    titles: &[String],
    metadata: &[TabStripMetadata],
    tooltips: &[Option<String>],
    active: usize,
) -> Option<String> {
    let count = titles.len();
    if count == 0 {
        return None;
    }
    let selected = active.min(count - 1) as isize;
    let states = (0..count)
        .map(|index| {
            let mut state = Vec::with_capacity(6);
            if index == selected as usize {
                state.push("selected");
            }
            if let Some(metadata) = metadata.get(index) {
                if metadata.dirty {
                    state.push("dirty");
                }
                if metadata.busy {
                    state.push("busy");
                }
                if metadata.attention {
                    state.push("attention");
                }
                // `conn-out` / `conn-in`; a both-ways connection reports BOTH
                // tokens, so the states list stays total per direction (§6:
                // the mark is never visual-only).
                if let Some(conn) = metadata.conn {
                    state.extend_from_slice(conn.chrome_states());
                }
            }
            state
        })
        .collect::<Vec<_>>();
    let tooltips = (0..count)
        .map(|index| tooltips.get(index).and_then(Option::as_deref))
        .collect::<Vec<_>>();
    let icons = (0..count)
        .map(|index| {
            metadata
                .get(index)
                .and_then(|item| item.icon)
                .map(crate::tab_bar::TabIconKind::semantic_name)
        })
        .collect::<Vec<_>>();
    Some(format!(
        "toolbar-tabs count={count} selected={selected} labels={titles:?} icons={icons:?} states={states:?} tooltips={tooltips:?}"
    ))
}

/// Route-aware hover/help text, augmented with state that must never be visual-only.
#[must_use]
fn tab_help(
    title: &str,
    tooltip: Option<&str>,
    metadata: TabStripMetadata,
    index: usize,
) -> String {
    let mut parts = vec![tooltip.unwrap_or(title).to_string()];
    if let Some(icon) = metadata.icon {
        parts.push(format!("App: {}", icon.semantic_name()));
    }
    if metadata.dirty {
        parts.push("Unsaved changes".to_string());
    }
    if metadata.busy {
        parts.push("Working".to_string());
    }
    if metadata.attention {
        parts.push("Needs attention".to_string());
    }
    // The §4 connection mark's non-visual twin, role-level like the mark
    // itself (per-peer detail belongs to the tooltip/menu composition).
    match metadata.conn {
        Some(crate::tab_bar::TabConnRole::Outbound) => {
            parts.push("Connects into a peer session".to_string());
        }
        Some(crate::tab_bar::TabConnRole::Inbound) => {
            parts.push("A peer session connects in".to_string());
        }
        Some(crate::tab_bar::TabConnRole::Both) => {
            parts.push("Connected both ways with peers".to_string());
        }
        None => {}
    }
    if index < 9 {
        parts.push(format!("⌘{}", index + 1));
    }
    parts.join(" · ")
}

#[must_use]
fn tab_display_label(title: &str, index: usize, available_width: f64) -> String {
    // The shortcut remains in hover help and accessibility at every size. Render it
    // inline only when doing so cannot steal the canonical tab identity. This estimate
    // is deliberately conservative for the 12 pt system font and Unicode titles.
    let title_width = title.chars().count() as f64 * 7.0;
    let shortcut_width = 34.0;
    if index < 9 && available_width >= title_width + shortcut_width {
        format!("{title}  \u{2318}{}", index + 1)
    } else {
        title.to_string()
    }
}

#[must_use]
fn tab_close_accessibility_label(title: &str) -> String {
    format!("Close {title} Tab")
}

// Below 64 pt, equal two-tab cells cannot preserve the measured 55 pt active
// "Settings" identity after gutters. Enter the active-priority layout first.
const PREFERRED_MIN_TAB_WIDTH: f64 = 64.0;
const TAB_CELL_GUTTER: f64 = 1.0;

/// Lay out a native tab band without ever overlapping the trailing New Tab action.
///
/// EQUAL SHARES THAT FILL THE BAND (macOS Terminal's tab bar): every tab takes
/// `band / count`, so two tabs are two half-width chips and eight tabs are eight
/// eighths — the whole strip is spent on titles instead of leaving bare titlebar
/// past a capped chip. There is deliberately no maximum width: an unread title is
/// the only thing a wide window can spend that space on.
///
/// Under pressure — once an equal share would drop below the legibility floor —
/// the selected identity gets the useful share and inactive tabs compress while
/// remaining ordered/reachable.
#[must_use]
fn native_tab_cells(band_width: f64, count: usize, active: usize) -> Vec<(f64, f64)> {
    if count == 0 {
        return Vec::new();
    }
    let band_width = band_width.max(1.0);
    let ideal = band_width / count as f64;
    let active = active.min(count - 1);
    let widths = if ideal >= PREFERRED_MIN_TAB_WIDTH {
        vec![ideal; count]
    } else {
        // Once equal tabs would become illegible, reserve up to 60% of the band (capped
        // at a useful 96pt) for the selected identity and share the remainder. Hidden
        // context belongs on inactive tabs before the thing the user is looking at.
        let selected = (band_width * 0.6).min(96.0).max(ideal);
        let inactive = if count > 1 {
            (band_width - selected) / (count - 1) as f64
        } else {
            selected
        };
        (0..count)
            .map(|index| if index == active { selected } else { inactive })
            .collect()
    };
    let mut cells = Vec::with_capacity(count);
    let mut x = 0.0;
    for width in widths {
        let gutter = TAB_CELL_GUTTER.min(width * 0.2);
        cells.push((x, (width - gutter).max(f64::EPSILON)));
        x += width;
    }
    cells
}

#[cfg(test)]
mod shared_tests {
    use super::*;

    #[test]
    fn context_menu_introspection_keeps_single_tab_identity() {
        assert!(!tab_menu_introspection_visible(0));
        assert!(tab_menu_introspection_visible(1));
        assert!(tab_menu_introspection_visible(2));
    }

    #[test]
    fn single_settings_tab_keeps_route_and_all_independent_states() {
        let settings_metadata = TabStripMetadata {
            icon: Some(crate::tab_bar::TabIconKind::Settings),
            dirty: true,
            busy: true,
            attention: true,
            conn: Some(crate::tab_bar::TabConnRole::Both),
            closable: true,
            drop_target: false,
        };
        let line = format_tab_chrome(
            &["Settings".to_string()],
            &[settings_metadata],
            &[Some("Settings · Cursor & Motion".to_string())],
            0,
        )
        .expect("single tab remains title chrome");
        assert!(line.contains("count=1 selected=0"));
        assert!(line.contains(r#"labels=["Settings"]"#));
        assert!(line.contains(r#"icons=[Some("settings")]"#));
        // A both-ways connection reports BOTH directional tokens, keeping the
        // states list total per direction.
        assert!(
            line.contains(r#"["selected", "dirty", "busy", "attention", "conn-out", "conn-in"]"#)
        );
        assert!(line.contains("Settings · Cursor & Motion"));

        let help = tab_help(
            "Settings",
            Some("Settings · Cursor & Motion"),
            settings_metadata,
            1,
        );
        assert_eq!(
            help,
            "Settings · Cursor & Motion · App: settings · Unsaved changes · Working · Needs attention · Connected both ways with peers · ⌘2"
        );
    }

    /// Each connection role is stated (never visual-only): its own chrome
    /// token(s) and its own hover-help line, one per direction.
    #[test]
    fn connection_roles_state_their_direction_in_chrome_and_help() {
        let conn_only = |conn| TabStripMetadata {
            icon: None,
            dirty: false,
            busy: false,
            attention: false,
            conn,
            closable: true,
            drop_target: false,
        };
        let line = format_tab_chrome(
            &["push-src".to_string(), "push-dst".to_string()],
            &[
                conn_only(Some(crate::tab_bar::TabConnRole::Outbound)),
                conn_only(Some(crate::tab_bar::TabConnRole::Inbound)),
            ],
            &[None, None],
            0,
        )
        .expect("two tabs report chrome");
        assert!(line.contains(r#"["selected", "conn-out"], ["conn-in"]"#));
        assert_eq!(
            tab_help(
                "push-src",
                None,
                conn_only(Some(crate::tab_bar::TabConnRole::Outbound)),
                0
            ),
            "push-src · Connects into a peer session · ⌘1"
        );
        assert_eq!(
            tab_help(
                "push-dst",
                None,
                conn_only(Some(crate::tab_bar::TabConnRole::Inbound)),
                1
            ),
            "push-dst · A peer session connects in · ⌘2"
        );
        assert_eq!(
            tab_help("peer", None, conn_only(None), 0),
            "peer · ⌘1",
            "no connection adds nothing"
        );
    }

    #[test]
    fn terminal_help_and_chrome_have_no_phantom_app_icon() {
        let terminal = TabStripMetadata::from_presentation(
            &crate::tab_model::TabPresentation::terminal("build-server"),
        );
        assert_eq!(terminal.icon, None);
        assert_eq!(
            tab_help("build-server", None, terminal, 0),
            "build-server · ⌘1"
        );
        let line = format_tab_chrome(&["build-server".to_string()], &[terminal], &[None], 0)
            .expect("single terminal is explicit title chrome");
        assert!(line.contains("icons=[None]"));
    }

    #[test]
    fn visible_tab_label_spends_narrow_space_on_identity() {
        assert_eq!(tab_display_label("Settings", 1, 146.0), "Settings  ⌘2");
        assert_eq!(tab_display_label("Settings", 1, 51.0), "Settings");
        assert_eq!(tab_display_label("build-server", 0, 74.0), "build-server");
        assert_eq!(
            tab_close_accessibility_label("Settings"),
            "Close Settings Tab"
        );
    }

    #[test]
    fn busy_spinner_phase_dedup_preserves_semantic_title_changes() {
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
        for pair in frames.windows(2) {
            assert!(busy_spinner_phase_only_change(pair[0], pair[1]));
        }

        // Model a long-running title spinner: one initial chrome update, not one
        // native relayout for every OSC title frame.
        let mut chrome = "aterm".to_string();
        let mut updates = 0;
        for index in 0..100 {
            let next = frames[index % frames.len()];
            if chrome != next && !busy_spinner_phase_only_change(&chrome, next) {
                chrome.clear();
                chrome.push_str(next);
                updates += 1;
            }
        }
        assert_eq!(updates, 1);
        assert_eq!(chrome, "⠋ aterm");

        assert!(!busy_spinner_phase_only_change("⠋ aterm", "⠋ aterm"));
        assert!(!busy_spinner_phase_only_change("⠋ aterm", "⠙ project"));
        assert!(!busy_spinner_phase_only_change("⠋ aterm", "aterm"));
        assert!(!busy_spinner_phase_only_change("aterm", "⠋ aterm"));
        assert!(!busy_spinner_phase_only_change("⠋aterm", "⠙aterm"));
        assert!(!busy_spinner_phase_only_change("⣿ aterm", "⠋ aterm"));
        assert!(!busy_spinner_phase_only_change("prefix ⠋", "prefix ⠙"));

        // Claude Code's circle-fill "moon" spinner coalesces the same way.
        let moons = [
            "◐ Claude Code",
            "◓ Claude Code",
            "◑ Claude Code",
            "◒ Claude Code",
        ];
        for pair in moons.windows(2) {
            assert!(busy_spinner_phase_only_change(pair[0], pair[1]));
        }
        assert!(busy_spinner_phase_only_change("◔ task", "◕ task"));
        // Cross-family frames coalesce too: same busy state, same suffix.
        assert!(busy_spinner_phase_only_change("⠋ aterm", "◐ aterm"));
        // The busy→idle handoff (✳ is not a spinner frame) is always recorded,
        // as are suffix changes and missing separators.
        assert!(!busy_spinner_phase_only_change(
            "◑ Claude Code",
            "✳ Claude Code"
        ));
        assert!(!busy_spinner_phase_only_change("◐ aterm", "◑ project"));
        assert!(!busy_spinner_phase_only_change("◐aterm", "◑aterm"));
    }

    #[test]
    fn native_overflow_cells_never_overlap_or_escape_the_band() {
        let band = 180.0;
        let cells = native_tab_cells(band, 12, 7);
        assert_eq!(cells.len(), 12);
        for pair in cells.windows(2) {
            assert!(pair[0].0 + pair[0].1 <= pair[1].0);
        }
        let last = cells.last().unwrap();
        assert!(last.0 + last.1 <= band);
        assert!(cells.iter().all(|(_, width)| *width > 0.0));

        assert!(
            cells[7].1 > cells[6].1,
            "active title gets overflow priority"
        );

        let phone_pair = native_tab_cells(126.0, 2, 1);
        assert!(
            phone_pair[1].1 > phone_pair[0].1,
            "a selected Settings tab wins space even in the common two-tab case"
        );
        let settings_layout =
            crate::tab_bar::native_tab_content_layout(phone_pair[1].1, 14.0, true, false);
        assert!(
            settings_layout.label[2] >= 55.0,
            "the selected phone tab preserves AppKit's measured Settings title"
        );

        let extreme = native_tab_cells(12.0, 40, 39);
        let last = extreme.last().unwrap();
        assert!(last.0 + last.1 <= 12.0 + f64::EPSILON);
    }

    /// A wide window spends its whole band on titles: equal shares, no cap, no bare
    /// titlebar trailing the last chip. This is the macOS Terminal rule, and the
    /// reason a two-tab window on a 1200 pt display gets two 600 pt titles rather
    /// than two 220 pt chips with 760 pt of nothing beside them.
    #[test]
    fn wide_bands_are_split_evenly_and_spent_to_the_last_point() {
        for count in 1..=8usize {
            let band = 1180.0;
            let cells = native_tab_cells(band, count, 0);
            assert_eq!(cells.len(), count);
            let share = band / count as f64;
            for (index, &(x, width)) in cells.iter().enumerate() {
                assert!(
                    (x - share * index as f64).abs() < 1e-9,
                    "cell {index} of {count} starts on its equal share"
                );
                assert!(
                    (width - (share - TAB_CELL_GUTTER)).abs() < 1e-9,
                    "cell {index} of {count} is one equal share wide"
                );
            }
            let (last_x, last_w) = *cells.last().unwrap();
            assert!(
                band - (last_x + last_w) <= TAB_CELL_GUTTER,
                "nothing but the gutter is left over at {count} tabs"
            );
        }

        // The lone identity is the whole band: a one-tab window has no leftover
        // titlebar to explain, and its title band is what fills it.
        assert_eq!(native_tab_cells(1180.0, 1, 0)[0].0, 0.0);
        assert!(native_tab_cells(1180.0, 1, 0)[0].1 >= 1180.0 - TAB_CELL_GUTTER);
    }
}

/// Compute the window TITLE a tab-aware header bar would show for `titles` with the
/// 0-based `active` index selected: the active tab's own title, with a ` — [i/n]`
/// position suffix when there is more than one tab (so the tab state is legible in
/// the window chrome even before a real tab strip exists). `None` when there is
/// nothing to title (no tabs) or only the bare single tab carries no extra suffix —
/// returns `Some(title)` with no counter so a one-tab window reads cleanly.
///
/// PURE: pure string assembly from the tab model, unit-tested below. The Linux
/// toolbar uses this in [`install_window_toolbar`] to seed the winit window title
/// from the initial tab set; live per-tab title updates continue to flow through the
/// cross-platform `App::apply_title` path (which owns `window.set_title` every
/// frame), so this helper never fights that owner — it only provides the seam's view
/// of what the active tab's title is.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn format_window_title(titles: &[String], active: usize) -> Option<String> {
    let n = titles.len();
    if n == 0 {
        return None;
    }
    let i = active.min(n - 1);
    let base = titles[i].trim();
    let base = if base.is_empty() { "aterm" } else { base };
    if n == 1 {
        Some(base.to_string())
    } else {
        Some(format!("{base} — [{}/{n}]", i + 1))
    }
}

/// The non-macOS toolbar seam: a REAL in-memory tab-chrome model (no GTK system
/// libraries required — see the deferred-work note on [`install_window_toolbar`]).
///
/// Off macOS aterm has no native `NSToolbar`; a full Linux equivalent is a GTK4
/// `GtkHeaderBar` (or a Wayland client-side-decoration tab strip), which needs the
/// gtk4/glib system development libraries that are NOT present on every host. Rather
/// than a dead `()` no-op, this module keeps the seam HONEST: it maintains the same
/// tab-chrome state the macOS strip does (titles + active index), reflects the
/// initial active tab into the winit window title, and serves the `chrome` verb's
/// introspection line from that live model. So [`set_window_tabs`] and
/// [`read_tab_chrome`] are real call sites with observable effect, not silent
/// dead-ends — the seam is ready for a real header bar to slot in behind it.
#[cfg(not(target_os = "macos"))]
mod non_macos {
    use std::cell::RefCell;

    use winit::event_loop::EventLoopProxy;
    use winit::window::Window;

    use super::{format_tab_chrome, format_window_title};
    use crate::session_chrome::TabChromeExt;
    use crate::{Wake, WindowId, tab_bar::TabStripMetadata};

    /// The live tab-chrome model for one window: the per-tab titles (in tab order)
    /// and the 0-based active index. The single source of truth the (future) header
    /// bar would render and that [`read_tab_chrome`] introspects — the Linux analogue
    /// of the macOS handle's retained `Vec<TabView>` + active flag.
    #[derive(Default)]
    pub(super) struct TabChrome {
        /// One label per tab, in tab order — the raw session titles the caller syncs.
        titles: Vec<String>,
        /// Canonical icon/status/close metadata paired with `titles`.
        metadata: Vec<TabStripMetadata>,
        /// Full route/document context paired with `titles`, shown on hover and
        /// exposed to accessibility/introspection without bloating the paint metadata.
        tooltips: Vec<Option<String>>,
        /// Composed per-tab chrome extension (tooltip + context-menu model,
        /// session-metadata stage 2), paired with `titles`. A future header bar
        /// would render the tooltip + pop the menu; today [`read_tab_menus`]
        /// serves the `chrome` mirror from it, so the Linux introspection line
        /// set matches macOS byte-for-byte.
        ext: Vec<TabChromeExt>,
        /// The 0-based index of the active tab (clamped into range on read/format).
        active: usize,
    }

    /// What [`install_window_toolbar`] returns off macOS: a REAL handle wrapping the
    /// interior-mutable [`TabChrome`] model (so [`set_window_tabs`] can update it
    /// through the shared `&self` the seam hands out, exactly like the macOS handle's
    /// `RefCell<Vec<TabView>>`) plus the window's [`WindowId`] and the `Wake` proxy
    /// the future header bar's affordances would relay through. `App` keeps it in its
    /// `_toolbars` map for the window's life, identical to the macOS path.
    pub struct ToolbarHandle {
        /// The live tab-chrome model — updated by [`set_window_tabs`], read by
        /// [`read_tab_chrome`]. `RefCell` because the seam exposes only `&self`.
        chrome: RefCell<TabChrome>,
        /// The window this chrome belongs to, kept so a future header bar addresses
        /// the RIGHT window's tab affordances (the macOS handle holds it for the same
        /// reason). Not yet read on Linux — there is no native control to drive — so
        /// allow it to be dead until the GTK4 header bar lands.
        #[allow(dead_code)]
        window: WindowId,
        /// The `Wake` channel a future header bar's tab clicks / "+" button would
        /// relay through (select / close / new-tab), mirroring the macOS handle's
        /// retained targets. Held now so the seam already owns everything a real
        /// control needs; unused until that control exists.
        #[allow(dead_code)]
        proxy: EventLoopProxy<Wake>,
    }

    /// Install the non-macOS window "toolbar": there is no native control to attach,
    /// so this builds the in-memory [`ToolbarHandle`] model and seeds the winit
    /// window title from the (initially single-tab) state via the pure
    /// [`format_window_title`]. The strip starts empty; the caller's first
    /// `App::sync_window` calls [`set_window_tabs`] to populate it.
    ///
    /// DEFERRED — a full native Linux toolbar: this is where a real
    /// `gtk4::HeaderBar` (or a Wayland client-side-decoration tab strip) would be
    /// constructed and attached — packing one tab widget per title, a trailing "+"
    /// New Tab button relaying `Wake::MenuAction { NewTab }`, and per-tab close/select
    /// gestures relaying `Wake::CloseTab` / `Wake::SelectTab` (exactly the macOS
    /// `toolbar.rs` dispatch). That requires the **gtk4 + glib system development
    /// libraries** (`libgtk-4-dev` / `gtk4` pkg-config) and a `gtk4`/`glib` crate
    /// dependency, NONE of which are available on the macOS build host — so it is
    /// intentionally NOT built here. The seam (this handle + model) is the buildable
    /// scaffolding that header bar slots behind without touching `App`.
    pub fn install_window_toolbar(
        window: &Window,
        proxy: &EventLoopProxy<Wake>,
        wid: WindowId,
    ) -> Option<ToolbarHandle> {
        // Seed the title from the initial (empty) model. A fresh window has no synced
        // tabs yet, so `format_window_title` yields `None` and we fall back to the
        // bare app name — a sensible title before the first `set_window_tabs`. Live
        // per-tab updates are then owned by `App::apply_title`.
        let title = format_window_title(&[], 0).unwrap_or_else(|| "aterm".to_string());
        window.set_title(&title);
        Some(ToolbarHandle {
            chrome: RefCell::new(TabChrome::default()),
            window: wid,
            proxy: proxy.clone(),
        })
    }

    /// Re-sync the non-macOS tab-chrome model to the current app tab state: store
    /// `titles` + the 0-based `active` index in the handle's [`TabChrome`]. This is
    /// the real Linux analogue of the macOS strip rebuild — it keeps the seam's model
    /// in lock-step with `App`'s tabs, so [`read_tab_chrome`] always reports the live
    /// set. A future header bar would, in addition, re-pack its tab widgets here.
    ///
    /// NB: this does NOT call `window.set_title` — the handle holds no `&Window` (the
    /// seam passes only `&self`), and the cross-platform `App::apply_title` path
    /// already owns the live title every frame, so re-titling here would double-write
    /// it. The model update IS the observable effect.
    /// `_ids` (the canonical stable per-tab identities): unused off macOS today —
    /// there is no native context menu to capture them at pop time — but part of
    /// the one uniform seam signature; the future header bar's right-click path
    /// would snapshot the clicked chip's id exactly like the macOS strip does.
    pub fn set_window_tabs(
        handle: &ToolbarHandle,
        titles: &[String],
        _ids: &[crate::tab_model::TabId],
        metadata: &[TabStripMetadata],
        tooltips: &[Option<String>],
        ext: &[TabChromeExt],
        active: usize,
    ) {
        let mut chrome = handle.chrome.borrow_mut();
        chrome.titles.clear();
        chrome.titles.extend_from_slice(titles);
        chrome.metadata.clear();
        chrome.metadata.extend_from_slice(metadata);
        chrome.tooltips.clear();
        chrome.tooltips.extend_from_slice(tooltips);
        chrome.ext.clear();
        chrome.ext.extend_from_slice(ext);
        chrome.active = active;
    }

    /// Read the non-macOS title-chrome introspection line from the live model via the
    /// pure [`format_tab_chrome`]. Every non-empty tab set reports title, selection,
    /// status and full tooltip context. On the non-macOS path the `chrome` verb reads
    /// this through `AppRt::read_toolbar_chrome`, so automation sees this live model
    /// on every supported host even before Linux grows a native header-bar widget.
    #[must_use]
    pub fn read_tab_chrome(handle: &ToolbarHandle) -> Option<String> {
        let chrome = handle.chrome.borrow();
        format_tab_chrome(
            &chrome.titles,
            &chrome.metadata,
            &chrome.tooltips,
            chrome.active,
        )
    }

    /// Read the per-tab CONTEXT-MENU introspection lines (`tab-menu tab=<i>
    /// items=[...]`) from the live model — one line per visible tab, including the
    /// single identity tab. Only an empty model has nothing to right-click.
    /// Serialised by the one pure
    /// `session_chrome::tab_menu_chrome_line`, so the Linux `chrome` mirror is
    /// byte-shaped like the macOS live-strip read.
    #[must_use]
    pub fn read_tab_menus(handle: &ToolbarHandle) -> Vec<String> {
        let chrome = handle.chrome.borrow();
        if !super::tab_menu_introspection_visible(chrome.titles.len()) {
            return Vec::new();
        }
        chrome
            .ext
            .iter()
            .enumerate()
            .map(|(i, e)| crate::session_chrome::tab_menu_chrome_line(i, &e.menu))
            .collect()
    }

    /// Off macOS the ↻ Software-Update affordance has no native control yet (it lands
    /// with the deferred GTK4 header bar — see [`install_window_toolbar`]), so toggling
    /// its REST/ALERT state is a no-op. Kept so the cross-platform `AppRt` seam
    /// (`set_toolbar_update_available`) has one uniform signature everywhere.
    #[allow(dead_code)]
    pub fn set_update_available(_handle: &ToolbarHandle, _available: bool) {}

    /// Off macOS there is no native strip (and no `NSAppearance`), so pinning the
    /// strip's light/dark appearance to the theme is a no-op. Same one-uniform-
    /// signature rationale as [`set_update_available`].
    pub fn set_strip_dark(_handle: &ToolbarHandle, _dark: bool) {}

    /// Off macOS there is no native strip; the selected-tab color override is
    /// carried by the in-grid strip instead (`tab_bar::strip_colors_with_active`).
    pub fn set_active_tab_color(_handle: &ToolbarHandle, _color: Option<[u8; 3]>) {}
}

#[cfg(all(test, not(target_os = "macos")))]
mod non_macos_tests {
    use super::{format_tab_chrome, format_window_title};
    use crate::tab_bar::TabStripMetadata;

    fn metadata(dirty: bool, busy: bool, attention: bool) -> TabStripMetadata {
        TabStripMetadata {
            icon: None,
            dirty,
            busy,
            attention,
            conn: None,
            closable: true,
            drop_target: false,
        }
    }

    /// No tabs means no chrome model; one real tab remains explicit title identity.
    #[test]
    fn chrome_keeps_single_tab_identity() {
        assert_eq!(format_tab_chrome(&[], &[], &[], 0), None);
        assert_eq!(
            format_tab_chrome(
                &["Settings".to_string()],
                &[metadata(false, false, false)],
                &[Some("Settings · Cursor & Motion".to_string())],
                0,
            )
            .as_deref(),
            Some(
                r#"toolbar-tabs count=1 selected=0 labels=["Settings"] icons=[None] states=[["selected"]] tooltips=[Some("Settings · Cursor & Motion")]"#
            )
        );
    }

    /// 2+ tabs report the count / selected / labels line in the EXACT macOS shape, so
    /// the introspection output is platform-stable.
    #[test]
    fn chrome_line_matches_macos_shape() {
        let titles = vec!["zsh".to_string(), "vim".to_string(), "htop".to_string()];
        let metadata = vec![
            metadata(false, false, false),
            metadata(true, true, true),
            metadata(false, false, false),
        ];
        assert_eq!(
            format_tab_chrome(&titles, &metadata, &vec![None; 3], 1).as_deref(),
            Some(
                r#"toolbar-tabs count=3 selected=1 labels=["zsh", "vim", "htop"] icons=[None, None, None] states=[[], ["selected", "dirty", "busy", "attention"], []] tooltips=[None, None, None]"#
            )
        );
    }

    /// An out-of-range active index is clamped to the last tab rather than producing a
    /// bogus `selected` (defensive — a stale index never escapes the formatter).
    #[test]
    fn chrome_clamps_out_of_range_active() {
        let titles = vec!["a".to_string(), "b".to_string()];
        assert_eq!(
            format_tab_chrome(
                &titles,
                &[metadata(false, false, false); 2],
                &[None, None],
                9,
            )
            .as_deref(),
            Some(
                r#"toolbar-tabs count=2 selected=1 labels=["a", "b"] icons=[None, None] states=[[], ["selected"]] tooltips=[None, None]"#
            )
        );
    }

    /// A single tab titles the window with JUST the active title (no `[i/n]`
    /// counter), so a one-tab window reads cleanly.
    #[test]
    fn title_single_tab_has_no_counter() {
        assert_eq!(
            format_window_title(&["vim".to_string()], 0).as_deref(),
            Some("vim")
        );
    }

    /// 2+ tabs append the ` — [i/n]` position suffix from the ACTIVE index (1-based
    /// in the display), so the tab state is legible in the window chrome.
    #[test]
    fn title_multi_tab_has_position_counter() {
        let titles = vec!["zsh".to_string(), "vim".to_string(), "htop".to_string()];
        assert_eq!(
            format_window_title(&titles, 2).as_deref(),
            Some("htop — [3/3]")
        );
    }

    /// An empty active title falls back to "aterm" (never a blank titlebar), and the
    /// out-of-range index is clamped like the chrome line.
    #[test]
    fn title_blank_falls_back_and_clamps() {
        assert_eq!(
            format_window_title(&["   ".to_string()], 0).as_deref(),
            Some("aterm")
        );
        let titles = vec!["a".to_string(), "b".to_string()];
        assert_eq!(
            format_window_title(&titles, 99).as_deref(),
            Some("b — [2/2]")
        );
    }

    /// No tabs at all yields no title (the install seed then defaults to "aterm").
    #[test]
    fn title_no_tabs_is_none() {
        assert_eq!(format_window_title(&[], 0), None);
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::cell::{Cell, RefCell};

    // THE SEAM IS GONE. This module was the last of `aterm-gui`'s AppKit
    // holdouts to speak `objc2` for its BINDINGS: four first-party declared
    // classes whose ~250 AppKit BINDING CALLS reached `objc2_app_kit` (268 by
    // `objc2-send-sites-v1`), crossing back at 43 places — `appkit::objc2_ref`
    // 7, `appkit::id_of` 3, `self.view()` 30, `cg_rect(` 3. The 250 counts the
    // CALLS, not the crossings; an earlier draft of this comment put it on the
    // wrong noun. `id_of` now has no
    // callers anywhere and is deleted; `objc2_ref` has exactly one, in
    // `app_introspect.rs` (still `objc2`, goes whole or not at all), reached
    // through `native_strip_container`. The `NSView` handle the crossings
    // produced (`Self::view`), the `NSRect`↔`CGRect` value conversion
    // (`cg_rect`) and the `MainThreadMarker` re-badge (`witness`) are all gone:
    // every send below goes through `crate::appkit`'s typed prototypes, and the
    // receiver of a declared `NSView` subclass is its own `as_id()`, because the
    // instance IS the view.
    use aterm_objc::{CGPoint, CGRect, CGSize, Id, MainThread, Obj, Retained, class, sel};
    use winit::event_loop::EventLoopProxy;
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    use super::{
        format_tab_chrome, native_tab_cells, tab_close_accessibility_label, tab_display_label,
        tab_help,
    };
    use crate::appkit::{self, consts};
    use crate::menu::MenuAction;
    use crate::session_chrome::{ConnVerb, TabChromeExt, TabMenuEntry};
    use crate::tab_bar::{
        TAB_ICON_DESIGN_SIZE, TAB_ICON_NATIVE_SIZE, TAB_STATUS_KINDS, TabConnRole, TabIconKind,
        TabIconPrimitive, TabStatusKind, TabStripMetadata, native_tab_content_layout,
        tab_icon_primitives, tab_status_center, tab_status_mark_scale,
    };
    use crate::tab_model::TabId;
    use crate::{TabAction, Wake, WindowId};

    /// Height (points) of the title-bar tab strip — the SINGLE chrome row. Roughly one
    /// row of a regular control plus a little breathing room, matching the compact
    /// Ghostty tab bar. The traffic lights are ~14pt tall and center within this row.
    const STRIP_HEIGHT: f64 = 28.0;

    /// Small leading gutter (points) INSIDE the toolbar item, before the first tab chip.
    /// The `UnifiedCompact` toolbar already lays this single custom-view item out AFTER
    /// the window's traffic-light zone (measured: the item's own origin sits at window-x
    /// ≈ 89pt, ~13pt past the stoplights), so — unlike a view pinned to the raw window
    /// edge — we do NOT reserve the full ~70pt light span here; this is just a tidy
    /// gutter so the first tab is composed cleanly past the stoplights, not jammed
    /// against the item edge. The leading edge carries NOTHING but tabs (no icon to
    /// misalign with the stoplights). [`strip_metrics`] can widen it: the band never
    /// starts before the MEASURED right edge of the live traffic lights plus
    /// [`LIGHTS_CLEARANCE`], so an AppKit metrics change can never tuck a chip under
    /// the stoplights.
    const STRIP_LEADING_PAD: f64 = 4.0;

    /// Clear space (points) kept between the trailing edge of the window's traffic
    /// lights and the first tab chip. Only binds when AppKit places the toolbar item
    /// far enough left that [`STRIP_LEADING_PAD`] alone would not clear the lights.
    const LIGHTS_CLEARANCE: f64 = 10.0;

    /// Width (points) of the trailing "+" New Tab button at the right end of the cluster.
    const PLUS_WIDTH: f64 = 28.0;

    /// Trailing gutter (points) between the window's right edge and the "+" — the visual
    /// mirror of the traffic-light inset on the left, so the action cluster is not jammed
    /// against the window corner.
    const TRAILING_PAD: f64 = 8.0;

    /// Gap (points) between the last tab chip and the trailing "+" button, so a chip
    /// never butts straight against it.
    const TAB_GAP: f64 = 6.0;

    /// The [`ChromeButton`] hover/press/accent fill is a rounded rect inset horizontally
    /// from the full button cell by this margin, of this fixed height, CENTRED on the
    /// strip's measured content line (the same one the tab pills use), with this corner
    /// radius. An accent capsule-style pill overrides the radius to half its height.
    const BTN_INSET_X: f64 = 2.0;
    const BTN_PILL_HEIGHT: f64 = 20.0;
    const BTN_RADIUS: f64 = 6.0;

    /// The "+" mark: half-length of each arm and the stroke width, drawn as two
    /// round-capped strokes crossing on the strip's measured centre line.
    const PLUS_ARM: f64 = 5.5;
    const PLUS_STROKE: f64 = 1.6;

    /// Min/max width (points) of the tab-strip toolbar ITEM. A plain custom `NSView`
    /// has no intrinsic size; without these the `UnifiedCompact` toolbar collapses the
    /// strip to zero width and hides it behind an overflow `»` chevron. The small
    /// minimum keeps it present; the very large maximum lets it stretch full-width.
    const STRIP_MIN_WIDTH: f64 = 120.0;
    const STRIP_MAX_WIDTH: f64 = 100_000.0;

    /// Curved-tab "pill" geometry. The active/hovered fill is a rounded rect of this
    /// fixed HEIGHT, centred on the strip's measured content line, inset horizontally
    /// from the full tab cell by this margin (so it floats with breathing room, the
    /// iTerm look) with this corner radius. The horizontal inset also yields the visual
    /// gap between adjacent pills — kept tight, because the cells now fill the band
    /// edge to edge and adjacent chips should read as one segmented bar.
    const TAB_PILL_INSET_X: f64 = 2.0;
    const TAB_PILL_HEIGHT: f64 = 22.0;
    const TAB_PILL_RADIUS: f64 = 7.0;

    /// Height (points) of the hairline rule an unselected chip draws on its leading
    /// edge to separate it from the chip before it. Shorter than the pill on purpose:
    /// it should divide two titles, not draw a box around each.
    const TAB_RULE_HEIGHT: f64 = 14.0;

    /// The §3.1 "small movement threshold" in AppKit points — the native twin
    /// of `conn_drag::CONN_DRAG_THRESHOLD_PX` (which is physical px; at 1×
    /// they coincide, at Retina this is deliberately the finger-feel unit): a
    /// connector press that travels less than this before `mouseUp:` opens the
    /// Connections menu; crossing it posts `Wake::ConnDragBegin` and the press
    /// becomes a connection drag.
    const CONN_DRAG_START_PTS: f64 = 4.0;

    /// Slop (points) around the status-canvas rect that still counts as the
    /// §3.1 connector: the painted marks are small, and a press one point off
    /// the ink is still aimed at the cell.
    const CONN_HIT_SLOP_PTS: f64 = 2.0;

    /// Width cap (points) of the inline rename editor in the SOLO band. The solo
    /// cell spans the whole window, and a full-width field in the titlebar reads
    /// as a dialog rather than as editing the name in place.
    const SOLO_EDITOR_WIDTH: f64 = 280.0;

    /// Gap (points) between the SOLO title and its description, and the minimum clear
    /// space kept at each end of the solo band before the title group is compressed.
    const SOLO_GAP: f64 = 10.0;
    const SOLO_EDGE_PAD: f64 = 12.0;
    /// Height (points) of the solo band's title / description labels, and of the solo
    /// status canvas trailing them.
    const SOLO_LABEL_HEIGHT: f64 = 18.0;
    const SOLO_STATUS_SIZE: f64 = 16.0;

    /// Where a chip sits in the strip, resolved ONCE per refresh by
    /// [`set_window_tabs`] from the measured [`StripMetrics`] and the tab COUNT, then
    /// handed to every [`TabView`]. Bundled so the vertical alignment, the solo/tabbed
    /// mode, and the solo centre travel together and can never be applied half-way.
    #[derive(Clone, Copy, PartialEq, Debug)]
    struct TabGeometry {
        /// The measured content centre line, in the chip's own coordinates.
        center_y: f64,
        /// Exactly one tab in this window: render as the window TITLE, not a chip.
        solo: bool,
        /// The window's horizontal centre, in the chip's own coordinates (solo only).
        solo_center_x: f64,
        /// Draw the leading-edge hairline that divides this chip from the one before
        /// it. False for the first chip (nothing precedes it) and for either chip
        /// touching the selected pill, which draws its own edge.
        separator: bool,
    }

    impl TabGeometry {
        /// Whether the chip at `index` of a strip whose `active` chip is selected
        /// draws a leading divider. PURE, so the rule is stated once and testable.
        #[must_use]
        const fn separates(index: usize, active: usize) -> bool {
            index > 0 && index != active && index != active + 1
        }
    }

    #[cfg(test)]
    mod geometry_tests {
        use super::TabGeometry;

        /// Dividers separate two quiet titles and nothing else: never before the first
        /// chip, and never on either side of the selected pill, which draws its own
        /// edge (a rule beside a pill reads as grime, not as structure).
        #[test]
        fn dividers_separate_quiet_titles_and_never_crowd_the_selected_pill() {
            let drawn = |active: usize| {
                (0..5)
                    .map(|index| TabGeometry::separates(index, active))
                    .collect::<Vec<_>>()
            };
            assert_eq!(drawn(0), [false, false, true, true, true]);
            assert_eq!(drawn(2), [false, true, false, false, true]);
            assert_eq!(drawn(4), [false, true, true, true, false]);
            // A lone chip is a title band; nothing precedes it either way.
            assert!(!TabGeometry::separates(0, 0));
        }
    }

    /// `[x, y, w, h]` (the point-space layout arrays) as an AppKit rect.
    fn rect_of([x, y, w, h]: [f64; 4]) -> CGRect {
        rect(x, y, w.max(0.0), h.max(0.0))
    }

    // ---- THE STRIP'S DRAWING VOCABULARY ----
    //
    // `crate::appkit` is deliberately one function per C PROTOTYPE, not per
    // selector: `send_id_rect_f64_f64` is the shape shared by every
    // `-(id)(NSRect, CGFloat, CGFloat)` in AppKit. These are the other half of
    // that split — the handful of CLASS-specific calls this module makes, named
    // once here instead of spelled out at each of the ~90 sites below. They
    // live in `toolbar.rs` and not in `appkit.rs` for the same reason
    // `aterm-objc` hosts no framework bindings at all: each layer binds only
    // what the layer above it actually reaches.

    /// `CGRect` from four scalars — the shape `NSMakeRect` makes.
    const fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
        CGRect {
            origin: CGPoint { x, y },
            size: CGSize {
                width: w,
                height: h,
            },
        }
    }

    /// `CGPoint` from two scalars — the shape `NSMakePoint` makes.
    const fn point(x: f64, y: f64) -> CGPoint {
        CGPoint { x, y }
    }

    /// One of `NSColor`'s CLASS colour factories, by selector, +0.
    ///
    /// Every one of these (`+labelColor`, `+controlAccentColor`, …) is
    /// `+(NSColor *)` and returns an AUTORELEASED instance — the +0 D5's rule
    /// describes — so the result is BORROWED and lives until the enclosing pool
    /// pops. Every caller either sends it `-set` immediately or hands it
    /// straight to a setter that retains, both inside the same frame. The
    /// `objc2` form wrapped each in a `Retained`, i.e. added a retain/release
    /// pair the semantics never asked for.
    ///
    /// # Safety
    /// `name` must be a no-argument `+(NSColor *)` class method of `NSColor`.
    unsafe fn ns_color(name: aterm_objc::Sel) -> Id {
        // SAFETY: the caller pins the selector to a nullary class factory.
        unsafe { appkit::send_id(class(c"NSColor").as_id(), name) }
    }

    /// `+[NSColor colorWithSRGBRed:green:blue:alpha:]`, +0.
    ///
    /// # Safety
    /// Main thread; the four components are plain `CGFloat`s.
    unsafe fn srgb(r: f64, g: f64, b: f64, a: f64) -> Id {
        // SAFETY: the documented non-raising `+(NSColor *)(CGFloat × 4)`.
        unsafe {
            appkit::send_id_f64_f64_f64_f64(
                class(c"NSColor").as_id(),
                sel!(colorWithSRGBRed:green:blue:alpha:),
                r,
                g,
                b,
                a,
            )
        }
    }

    /// `-[NSColor colorWithAlphaComponent:]`, +0.
    ///
    /// # Safety
    /// `color` must be a live `NSColor`.
    unsafe fn with_alpha(color: Id, alpha: f64) -> Id {
        // SAFETY: `-(NSColor *)(CGFloat)`, non-raising.
        unsafe { appkit::send_id_f64(color, sel!(colorWithAlphaComponent:), alpha) }
    }

    /// `+[NSBezierPath bezierPath]` — a fresh empty path, +0.
    ///
    /// # Safety
    /// Main thread, inside a draw cycle.
    unsafe fn bezier_path() -> Id {
        // SAFETY: the documented non-raising `+(NSBezierPath *)` factory.
        unsafe { appkit::send_id(class(c"NSBezierPath").as_id(), sel!(bezierPath)) }
    }

    /// `+[NSBezierPath bezierPathWithRoundedRect:xRadius:yRadius:]`, +0. The
    /// strip only ever uses EQUAL radii, so the two are one argument here.
    ///
    /// # Safety
    /// Main thread, inside a draw cycle.
    unsafe fn rounded_rect(r: CGRect, radius: f64) -> Id {
        // SAFETY: `+(NSBezierPath *)(NSRect, CGFloat, CGFloat)`, non-raising.
        unsafe {
            appkit::send_id_rect_f64_f64(
                class(c"NSBezierPath").as_id(),
                sel!(bezierPathWithRoundedRect:xRadius:yRadius:),
                r,
                radius,
                radius,
            )
        }
    }

    /// `+[NSBezierPath bezierPathWithOvalInRect:]`, +0.
    ///
    /// # Safety
    /// Main thread, inside a draw cycle.
    unsafe fn oval_in_rect(r: CGRect) -> Id {
        // SAFETY: `+(NSBezierPath *)(NSRect)`, non-raising.
        unsafe {
            appkit::send_id_rect(
                class(c"NSBezierPath").as_id(),
                sel!(bezierPathWithOvalInRect:),
                r,
            )
        }
    }

    /// `+[NSFont systemFontOfSize:]` / `+boldSystemFontOfSize:`, +0.
    ///
    /// # Safety
    /// Main thread.
    unsafe fn system_font(size: f64, bold: bool) -> Id {
        let name = if bold {
            sel!(boldSystemFontOfSize:)
        } else {
            sel!(systemFontOfSize:)
        };
        // SAFETY: both are `+(NSFont *)(CGFloat)`, non-raising.
        unsafe { appkit::send_id_f64(class(c"NSFont").as_id(), name, size) }
    }

    /// `NSLineBreakByTruncatingTail` — a title too long for its chip must end in an
    /// ELLIPSIS, not simply stop: a silently clipped title reads as a complete (and
    /// therefore wrong) name, and equal-share chips truncate routinely. A
    /// `labelWithString:` label clips by default, so every strip label is switched
    /// over explicitly.
    ///
    /// Sent to the field's CELL, which is where `NSLineBreakMode` lives.
    ///
    /// # Safety
    /// `field` must be a live `NSTextField`.
    unsafe fn truncate_tail(field: Id) {
        // SAFETY: `-cell` is a side-effect-free main-thread `-(id)` getter, and
        // every `NSTextField`'s cell is an `NSCell`, which has responded to
        // `-setLineBreakMode:` — an `-(void)(NSLineBreakMode)`, i.e. an
        // `NSUInteger` — since 10.0. A nil cell sends to nil and is inert.
        unsafe {
            let cell = appkit::send_id(field, sel!(cell));
            if !cell.is_null() {
                appkit::send_v_usize(
                    cell,
                    sel!(setLineBreakMode:),
                    consts::NS_LINE_BREAK_BY_TRUNCATING_TAIL,
                );
            }
        }
    }

    /// The strip geometry MEASURED from the live window rather than assumed: where the
    /// window's traffic lights actually sit, expressed in the strip container view's own
    /// coordinates.
    ///
    /// The `UnifiedCompact` toolbar hands our custom view a row whose optical centre is
    /// NOT the stoplights' centre — measured on a stock window the chips floated ~2pt
    /// high, which is exactly the kind of misalignment the eye reads as "the tab bar is
    /// not part of the titlebar". Rather than bake a magic offset that a macOS metrics
    /// change would silently invalidate, every vertical position in the strip is derived
    /// from [`Self::center_y`] — the close button's own centre line — and the band's
    /// leading edge is held clear of [`Self::lights_right`].
    #[derive(Clone, Copy, Debug)]
    struct StripMetrics {
        /// Vertical centre the whole strip's content aligns to: the traffic lights'
        /// centre line, in container coordinates. Falls back to the container's own
        /// midpoint when the window or its buttons cannot be read (headless teardown).
        center_y: f64,
        /// Trailing edge of the rightmost visible traffic light, in container
        /// coordinates. Normally NEGATIVE (the lights sit left of the toolbar item);
        /// `f64::NEG_INFINITY` when no light could be measured, so the `max` that
        /// consumes it degrades to the plain leading pad.
        lights_right: f64,
        /// Horizontal centre of the WINDOW (not of the band), in container
        /// coordinates — what the solo title centres on, so a one-tab window's title
        /// reads centred in its window the way macOS Terminal's does, rather than
        /// centred in the leftover space between the stoplights and the "+".
        window_center_x: f64,
    }

    impl StripMetrics {
        /// The geometry a container with no reachable window falls back to: its own
        /// centre, no measured lights.
        ///
        /// # Safety
        /// `container` must be a live `NSView`.
        unsafe fn unmeasured(container: Id) -> Self {
            // SAFETY: `-bounds` is a side-effect-free `-(NSRect)` getter.
            let bounds = unsafe { appkit::send_rect(container, sel!(bounds)) };
            Self {
                center_y: bounds.origin.y + bounds.size.height * 0.5,
                lights_right: f64::NEG_INFINITY,
                window_center_x: bounds.origin.x + bounds.size.width * 0.5,
            }
        }
    }

    /// Measure the live window's traffic lights in `container`'s coordinate space.
    ///
    /// Returns [`StripMetrics::unmeasured`] whenever the window, its standard buttons,
    /// or its content view cannot be read — a detached or tearing-down container still
    /// lays out, it just centres on itself. `center_y` is clamped so a full-height pill
    /// always lands inside the container even if AppKit hands us a short row.
    ///
    /// # Safety
    /// `container` must be a live `NSView`.
    unsafe fn strip_metrics(container: Id) -> StripMetrics {
        // SAFETY: the caller guarantees a live view.
        let mut metrics = unsafe { StripMetrics::unmeasured(container) };
        // SAFETY: every call here is a side-effect-free main-thread AppKit getter on
        // live objects: `-window`/`-superview`/`-contentView` are `-(id)`,
        // `-standardWindowButton:` is `-(id)(NSWindowButton)` — an NSUInteger
        // enum — `-isHidden` is `-(BOOL)`, `-frame`/`-bounds` are `-(NSRect)`
        // and `-convertRect:fromView:` is `-(NSRect)(NSRect, NSView *)`. Each
        // nil is checked, and a button belonging to the same window is always
        // convertible into this view's space; a NIL source view means WINDOW
        // coordinates, which is the documented meaning `superview` degrades to.
        unsafe {
            let window = appkit::send_id(container, sel!(window));
            if window.is_null() {
                return metrics;
            }
            for button in [
                consts::NS_WINDOW_CLOSE_BUTTON,
                consts::NS_WINDOW_MINIATURIZE_BUTTON,
                consts::NS_WINDOW_ZOOM_BUTTON,
            ] {
                let light = appkit::send_id_usize(window, sel!(standardWindowButton:), button);
                if light.is_null() || appkit::send_bool(light, sel!(isHidden)) {
                    continue;
                }
                let superview = appkit::send_id(light, sel!(superview));
                let rect = appkit::send_rect_rect_id(
                    container,
                    sel!(convertRect:fromView:),
                    appkit::send_rect(light, sel!(frame)),
                    superview,
                );
                if !(rect.size.width.is_finite() && rect.size.height > 0.0) {
                    continue;
                }
                if button == consts::NS_WINDOW_CLOSE_BUTTON {
                    metrics.center_y = rect.origin.y + rect.size.height * 0.5;
                }
                metrics.lights_right = metrics.lights_right.max(rect.origin.x + rect.size.width);
            }
            let content = appkit::send_id(window, sel!(contentView));
            if !content.is_null() {
                let rect = appkit::send_rect_rect_id(
                    container,
                    sel!(convertRect:fromView:),
                    appkit::send_rect(content, sel!(bounds)),
                    content,
                );
                if rect.size.width.is_finite() && rect.size.width > 1.0 {
                    metrics.window_center_x = rect.origin.x + rect.size.width * 0.5;
                }
            }
        }
        // Never let a measurement push the pill outside the row AppKit gave us.
        // SAFETY: as above.
        let bounds = unsafe { appkit::send_rect(container, sel!(bounds)) };
        let lo = bounds.origin.y + TAB_PILL_HEIGHT * 0.5;
        let hi = bounds.origin.y + bounds.size.height - TAB_PILL_HEIGHT * 0.5;
        if lo <= hi {
            metrics.center_y = metrics.center_y.clamp(lo, hi);
        }
        metrics
    }

    /// What [`install_window_toolbar`] returns: the retained backing objects. AppKit
    /// references a toolbar item's view and a toolbar's delegate only WEAKLY, so they
    /// must outlive the window — `App` holds this in a field.
    pub struct ToolbarHandle {
        /// The `NSToolbarDelegate` that vends the strip's single custom-view item. The
        /// toolbar references its delegate only weakly, so retain it here.
        _delegate: aterm_objc::Retained<ToolbarDelegate>,
        /// The `NSToolbar` hosting the strip item, kept alive alongside its delegate.
        /// ALWAYS VISIBLE — it carries the "+" at every tab count, so the titlebar is
        /// never an empty capsule; the tab chips appear before the trailing "+" only at
        /// every non-empty tab set. Retain-only (the `NSWindow` owns its live toolbar strongly, and
        /// nothing here toggles its visibility), so it is `_`-prefixed like the other
        /// retain-only backing objects.
        _toolbar: Obj,
        /// The full-width container `NSView` (the toolbar item's custom view), holding
        /// the per-tab [`TabView`]s and the trailing "+" [`ChromeButton`]. Retained
        /// so [`set_window_tabs`] can rebuild its tab sub-views and so [`read_tab_chrome`]
        /// can read the live tab views.
        container: Obj,
        /// The proxy + window id used to build each rebuilt [`TabView`]'s relays. The
        /// container's tab views are rebuilt on every [`set_window_tabs`], so the
        /// builder needs these; they live as long as the handle.
        proxy: EventLoopProxy<Wake>,
        window: WindowId,
        /// The live [`TabView`]s, one per tab, in tab order — the source of truth for
        /// [`read_tab_chrome`] (count / active / labels) and [`set_window_tabs`]'s
        /// rebuild (it removes the old set as subviews and replaces this Vec). AppKit
        /// holds a subview only via its superview's array; we ALSO retain them here so
        /// the per-tab targets/labels stay live and introspection can read them.
        tabs: RefCell<Vec<aterm_objc::Retained<TabView>>>,
        /// The trailing "+" New Tab [`ChromeButton`], pinned right. Retained because the
        /// container holds its subviews weakly w.r.t. our Rust ownership, and because
        /// [`set_window_tabs`] re-pins it to the live width and puts it on the strip's
        /// measured centre line every refresh. (The accent "Update" capsule that used to
        /// sit left of it is RETIRED — the update affordance lives in the VERSION menu
        /// now; see `crate::menu::update_version_menu`.)
        plus: aterm_objc::Retained<ChromeButton>,
        /// winit's content `NSView` — the app's permanent first responder. Retained
        /// at install so [`end_tab_rename`] can hand key focus BACK to the terminal
        /// deterministically; asking the container for `-window` then
        /// `-contentView` later
        /// is wrong in native fullscreen, where AppKit re-hosts the toolbar in a
        /// SEPARATE auxiliary window.
        content_view: Obj,
        /// The live INLINE SESSION-RENAME editor, or `None`. Owned by the HANDLE, not
        /// by a tab chip: [`set_window_tabs`]'s rebuild path destroys every chip (and
        /// every chip subview) whenever the tab count or the container width changes,
        /// which a background session exiting or any window resize causes — mid-edit,
        /// with no user action. Living beside the chips, the field is repositioned
        /// across a rebuild instead of dying in one.
        rename: RefCell<Option<RenameEditor>>,
    }

    /// The live inline rename editor: an `NSTextField` overlaying one tab chip,
    /// plus the small relay object AppKit calls back on.
    struct RenameEditor {
        /// The editable field. Its `stringValue` is the ONLY home of the in-progress
        /// text — nothing else needs saving across a strip rebuild.
        field: Obj,
        /// The delegate/relay. Retained here because AppKit holds a delegate weakly.
        target: aterm_objc::Retained<TabRenameTarget>,
        /// The STABLE id of the tab the editor is painted over — POSITIONING ONLY.
        /// It follows a reorder and, when it stops resolving, tells the strip the
        /// edited tab is gone (a cancel, never a commit: the user was renaming
        /// something that no longer exists). The COMMIT target is the session id
        /// the relay carries, resolved by `App` at begin.
        tab: TabId,
    }

    /// The live custom-view subtree AppKit places in the unified titlebar.
    ///
    /// Full-window capture uses this only to locate the public titlebar-container
    /// ancestor shared with the standard traffic-light buttons. The returned view
    /// remains owned by `ToolbarHandle`; cloning the retain lets the capture stay
    /// valid while AppKit draws that subtree into a transparent bitmap.
    ///
    /// # THE SEAM MOVED, AND THIS IS WHERE IT WENT
    ///
    /// This used to hand back an `objc2::rc::Retained<NSView>`, and it was the
    /// only value crossing this module's boundary that carried an `objc2` type
    /// at all — `grep -rn native_strip_container` finds exactly one caller.
    /// It now returns the first-party [`Obj`], and the one caller
    /// (`app_introspect.rs`, which is on the module list of what is still
    /// `objc2` and goes whole or not at all) crosses at its own call site with
    /// `appkit::objc2_ref`. That is deliberate: the seam belongs in the
    /// UNPORTED module, where it is one named line, rather than in this one,
    /// where it would be the last `objc2` type in an otherwise clean file.
    pub(crate) fn native_strip_container(handle: &ToolbarHandle) -> Obj {
        handle.container.clone_retained()
    }

    // SAFETY: `ToolbarHandle` is only ever created, read, and dropped on the main
    // thread (the event loop). It holds main-thread-only AppKit objects; `App` stores
    // it in a `BTreeMap` keyed by window and never sends it across threads. The
    // `EventLoopProxy` is `Send`. We assert thread-affinity by construction (every
    // entry point mints an [`aterm_objc::MainThread`]), so the auto-derived
    // non-Send is the safe default and we add no unsafe Send/Sync.

    /// The mutable per-button state a [`ChromeButton`] needs at click/draw time. Held in
    /// `Cell`s/`RefCell`s because AppKit messages the view (`mouseDown:`/`drawRect:`)
    /// through a shared `&self`.
    pub(crate) struct ChromeIvars {
        /// The `Wake` channel a click relays through.
        proxy: EventLoopProxy<Wake>,
        /// The [`MenuAction`] this button fires (File ▸ New Tab for "+") — the SAME
        /// action the menu item / keybinding uses, never a parallel path.
        action: MenuAction,
        /// Style: `false` = a QUIET ICON button (transparent at rest, a subtle rounded
        /// highlight on hover, brighter on press — the "+"); `true` = an ACCENT CAPSULE
        /// call-to-action (a filled `controlAccentColor` pill — currently unused; kept
        /// as ChromeButton infra for future CTA buttons).
        accent: bool,
        /// Whether the pointer is inside the button (drives the hover highlight).
        hovered: Cell<bool>,
        /// Whether the mouse is held down inside the button (drives the pressed state).
        pressed: Cell<bool>,
        /// The strip's MEASURED content centre line ([`StripMetrics::center_y`]) in this
        /// button's own coordinates, so the "+" sits on the same optical row as the
        /// traffic lights and the tab chips instead of on the cell's own midpoint.
        center_y: Cell<f64>,
        /// The retained tracking area, so `updateTrackingAreas` can swap it on a frame
        /// change without leaking the old one.
        tracking: RefCell<Option<Obj>>,
        /// Human action name used by VoiceOver (the drawn glyph alone is not a name).
        accessibility_label: String,
    }

    aterm_objc::declare_class! {
        /// A native-feeling titlebar button: a custom `NSView` that draws its OWN button
        /// affordance (a rounded hover/press highlight for the quiet "+" icon; a filled
        /// accent capsule for a CTA-styled button), centers a text label, tracks
        /// hover/press, and relays a [`Wake::MenuAction`] on a click that both begins AND
        /// ends inside it. Replaces the earlier borderless `NSButton`s, which had no hover
        /// or press affordance and so did not read as buttons.
        ///
        /// Declared with [`aterm_objc::declare_class!`], on `NSView`. This is
        /// one of the three sites carrying the per-arch `BOOL` D3 fixed
        /// (`acceptsFirstMouse:`, `accessibilityPerformPress`), and the first
        /// class in the tree whose superclass is not `NSObject` — which is what
        /// found `alloc_ivars`: `alloc_init` sends `-init`, and an `NSView`
        /// subclass's designated initializer is `-initWithFrame:`.
        ///
        /// The METHOD BODIES are first-party too now: every AppKit send below
        /// goes through `crate::appkit`'s typed prototypes and this module's
        /// drawing vocabulary, and the receiver is `self.as_id()` — this
        /// instance IS the `NSView`, so the `Self::view` handle that used to
        /// mint an `objc2` `&NSView` is gone.
        ///
        /// objc2's `type Mutability = MainThreadOnly` is carried by
        /// [`aterm_objc::Retained`]'s unconditional `!Send` plus the
        /// [`aterm_objc::MainThread`] this module's entry points take — the
        /// same token `status_item.rs` and `menu.rs` take.
        pub(crate) struct ChromeButton: NSView {
            const NAME: &str = "ATermChromeButton";
            type Ivars = ChromeIvars;
            protocols: [NSObject];
            /// Paint the button affordance, then its "+" mark. ICON: nothing at rest
            /// (blends into the titlebar), a subtle translucent-white rounded pill on
            /// hover, a stronger one while pressed — the modern macOS toolbar-button
            /// look. ACCENT: a filled `controlAccentColor` capsule always (the CTA),
            /// darkened while pressed.
            @sel(drawRect:)
            fn drawRect(&self, _dirty: CGRect) {
                // SAFETY: `-bounds` is a side-effect-free `-(NSRect)` getter on
                // this live view, inside AppKit's own draw cycle.
                let bounds = unsafe { appkit::send_rect(self.as_id(), sel!(bounds)) };
                let ivars = self.ivars();
                let accent = ivars.accent;
                // The fill is inset horizontally from the cell and CENTRED on the strip's
                // measured content line, so it floats at exactly the same optical height
                // as the tab pills and the traffic lights (a capsule for accent, a soft
                // rounded rect for the icon hover).
                let pill = rect(
                    bounds.origin.x + BTN_INSET_X,
                    ivars.center_y.get() - BTN_PILL_HEIGHT * 0.5,
                    (bounds.size.width - 2.0 * BTN_INSET_X).max(0.0),
                    BTN_PILL_HEIGHT,
                );
                let radius = if accent { pill.size.height / 2.0 } else { BTN_RADIUS };
                // SAFETY: standard AppKit drawing primitives, on the main thread inside a
                // draw cycle (AppKit has set up the focused graphics context). The colors
                // are AUTORELEASED (+0) and consumed in this frame; `-set`/`-fill` are
                // `-(void)` and none of these raise.
                unsafe {
                    if accent {
                        // The CTA: a filled accent capsule; a translucent-black wash
                        // darkens it while pressed for tactile feedback.
                        appkit::send_v(ns_color(sel!(controlAccentColor)), sel!(set));
                        appkit::send_v(rounded_rect(pill, radius), sel!(fill));
                        if ivars.pressed.get() {
                            appkit::send_v(srgb(0.0, 0.0, 0.0, 0.18), sel!(set));
                            appkit::send_v(rounded_rect(pill, radius), sel!(fill));
                        }
                    } else if ivars.pressed.get() || ivars.hovered.get() {
                        // Quiet icon: a translucent-white rounded highlight — stronger
                        // when pressed than merely hovered. Nothing at rest.
                        let a = if ivars.pressed.get() { 0.16 } else { 0.09 };
                        appkit::send_v(srgb(1.0, 1.0, 1.0, a), sel!(set));
                        appkit::send_v(rounded_rect(pill, radius), sel!(fill));
                    }
                    // The "+" is DRAWN, not set in a font: a glyph is centred on its
                    // line box, and a line box's centre is not the glyph's optical
                    // centre — measured, a 17pt "+" in an `NSTextField` sat ~1.3pt
                    // below the stoplights no matter how the box was placed. Two
                    // strokes crossing exactly at the measured centre line cannot be
                    // off, and read crisper at this size besides.
                    appkit::send_v(Self::glyph_color(accent, ivars.hovered.get()), sel!(set));
                    let cx = bounds.origin.x + bounds.size.width * 0.5;
                    let cy = ivars.center_y.get();
                    let arm = PLUS_ARM;
                    let mark = bezier_path();
                    appkit::send_v_f64(mark, sel!(setLineWidth:), PLUS_STROKE);
                    appkit::send_v_usize(mark, sel!(setLineCapStyle:), consts::NS_LINE_CAP_STYLE_ROUND);
                    appkit::send_v_point(mark, sel!(moveToPoint:), point(cx - arm, cy));
                    appkit::send_v_point(mark, sel!(lineToPoint:), point(cx + arm, cy));
                    appkit::send_v_point(mark, sel!(moveToPoint:), point(cx, cy - arm));
                    appkit::send_v_point(mark, sel!(lineToPoint:), point(cx, cy + arm));
                    appkit::send_v(mark, sel!(stroke));
                }
            }

            /// Press begins: latch the pressed state and repaint (shows the pressed fill).
            @sel(mouseDown:)
            fn mouseDown(&self, _event: Id) {
                self.ivars().pressed.set(true);
                self.mark_dirty();
            }

            /// Drag while pressed: keep the pressed highlight only while the pointer stays
            /// inside the button (native button behavior — dragging off cancels the press).
            @sel(mouseDragged:)
            fn mouseDragged(&self, event: Id) {
                let inside = self.point_inside(event);
                if self.ivars().pressed.get() != inside {
                    self.ivars().pressed.set(inside);
                    self.mark_dirty();
                }
            }

            /// Release: fire the action IFF the release lands inside the button (a click
            /// that began and ended on it), then clear the pressed state.
            @sel(mouseUp:)
            fn mouseUp(&self, event: Id) {
                let was_pressed = self.ivars().pressed.get();
                self.ivars().pressed.set(false);
                self.mark_dirty();
                if was_pressed && self.point_inside(event) {
                    // Fire-and-forget: a closed loop (app shutting down) just drops the
                    // event — mirrors every other `send_event` here.
                    let action = self.ivars().action;
                    let _ = self.ivars().proxy.send_event(Wake::MenuAction { action });
                }
            }

            /// Pointer entered — show the hover highlight and brighten the "+" mark.
            @sel(mouseEntered:)
            fn mouseEntered(&self, _event: Id) {
                self.ivars().hovered.set(true);
                self.mark_dirty();
            }

            /// Pointer left — drop the hover highlight and dim the "+" mark back.
            @sel(mouseExited:)
            fn mouseExited(&self, _event: Id) {
                self.ivars().hovered.set(false);
                self.mark_dirty();
            }

            /// Rebuild the tracking area whenever geometry changes (the "+" is re-pinned on
            /// resize), so hover detection follows. Removes the previous area first.
            @sel(updateTrackingAreas)
            fn updateTrackingAreas(&self) {
                // SAFETY: `[super updateTrackingAreas]` is `-(void)` and
                // `super_receiver()` pairs this live instance with the class
                // that DECLARES this method, so lookup starts at `NSView` — the
                // rule `super_of` states, and the reason it takes the defining
                // class rather than `object_getClass(this)`.
                unsafe {
                    let sup = self.super_receiver();
                    let f: unsafe extern "C" fn(*const aterm_objc::ObjcSuper, aterm_objc::Sel) =
                        aterm_objc::msg_super();
                    f(&raw const sup, sel!(updateTrackingAreas));
                }
                self.install_tracking_area();
            }

            /// Accept the FIRST click even when the window is not key, so clicking the "+"
            /// in a background window works in a single click (native feel).
            @sel(acceptsFirstMouse:)
            fn acceptsFirstMouse(&self, _event: Id) -> aterm_objc::Bool {
                aterm_objc::Bool::YES
            }

            /// VoiceOver's press action dispatches the exact same typed menu command as
            /// a pointer click. Returning true confirms that the action was accepted.
            @sel(accessibilityPerformPress)
            fn accessibilityPerformPress(&self) -> aterm_objc::Bool {
                let _ = self.ivars().proxy.send_event(Wake::MenuAction {
                    action: self.ivars().action,
                });
                aterm_objc::Bool::YES
            }
        }

    }

    impl ChromeButton {
        /// Build a titlebar "+" button of `frame`, wired to relay `action` through
        /// `proxy`. `accent` selects the ACCENT-CAPSULE CTA style vs the QUIET-ICON
        /// style. The mark itself is drawn in [`drawRect:`](Self::drawRect) rather than
        /// set in a font, so it lands exactly on the strip's measured centre line. All
        /// construction is via NON-RAISING factory initializers on the main thread.
        fn build(
            mtm: MainThread,
            proxy: EventLoopProxy<Wake>,
            action: MenuAction,
            accent: bool,
            accessibility_label: &str,
            frame: CGRect,
        ) -> Option<Retained<Self>> {
            let ivars = ChromeIvars {
                proxy,
                action,
                accent,
                hovered: Cell::new(false),
                pressed: Cell::new(false),
                center_y: Cell::new(frame.size.height * 0.5),
                tracking: RefCell::new(None),
                accessibility_label: accessibility_label.to_string(),
            };
            // `alloc_init` sends `-init`; an `NSView` subclass's designated
            // initializer is `-initWithFrame:`, so the two steps are taken
            // apart here — which is the gap this port found and `alloc_ivars`
            // closes. The ivars are stored BEFORE the initializer runs,
            // deliberately: `NSView -initWithFrame:` sends overridden methods
            // back to the instance on some paths (`updateTrackingAreas` is one
            // this very class overrides) and those read `ivars()`.
            //
            // A plain send, not a super send: `ChromeButton` does not implement
            // `initWithFrame:`, so lookup lands on `NSView`'s IMP either way,
            // and the objc2 form's `super(this)` was a consequence of its
            // `Allocated<Self>` type rather than of the dispatch.
            //
            // SAFETY: `alloc_ivars` returns a +1 instance owing exactly one
            // designated initializer, which is the next send. `-initWithFrame:`
            // is `-(id)(NSRect)`, consumes that +1 and returns the initialised
            // +1 that `Retained::from_owned` adopts.
            let this: Retained<Self> = unsafe {
                let raw = Self::alloc_ivars(mtm, ivars);
                if raw.is_null() {
                    return None;
                }
                Retained::from_owned(appkit::send_id_rect(raw, sel!(initWithFrame:), frame))?
            };

            // The custom NSView must opt into accessibility explicitly; otherwise the
            // drawn "+" is invisible to VoiceOver — it is pixels, not a control.
            // SAFETY: NSView implements the NSAccessibility setters on every supported
            // macOS version; plain main-thread metadata writes on the fresh view.
            // `-setAccessibilityElement:`/`-setAccessibilityEnabled:` are
            // `-(void)(BOOL)`, the other two `-(void)(NSString *)`.
            unsafe {
                let role = appkit::nsstring("AXButton")?;
                let name = appkit::nsstring(&this.ivars().accessibility_label)?;
                appkit::send_v_bool(this.as_id(), sel!(setAccessibilityElement:), true);
                appkit::send_v_id(this.as_id(), sel!(setAccessibilityRole:), role.id());
                appkit::send_v_id(this.as_id(), sel!(setAccessibilityLabel:), name.id());
                appkit::send_v_bool(this.as_id(), sel!(setAccessibilityEnabled:), true);
            }
            this.install_tracking_area();
            Some(this)
        }

        /// Ink for the "+" mark: on an ACCENT capsule it is always near-white (it sits
        /// on the accent fill); a QUIET icon is `secondaryLabelColor` at rest and
        /// brightens to `labelColor` on hover.
        ///
        /// # Safety
        /// Main thread; the result is +0 and must be used before the pool pops
        /// — the only caller sends it `-set` on the next line.
        unsafe fn glyph_color(accent: bool, hovered: bool) -> Id {
            // SAFETY: the NSColor class-color factories are main-thread AppKit calls that
            // return an autoreleased instance and never raise; the only caller is
            // `drawRect:`, which runs on the main thread.
            unsafe {
                if accent {
                    ns_color(sel!(whiteColor))
                } else if hovered {
                    ns_color(sel!(labelColor))
                } else {
                    ns_color(sel!(secondaryLabelColor))
                }
            }
        }

        /// Move this button onto the strip's measured content line. No-op when
        /// unchanged, so the per-refresh call costs nothing in the steady state.
        fn set_center_y(&self, center_y: f64) {
            if self.ivars().center_y.get() == center_y {
                return;
            }
            self.ivars().center_y.set(center_y);
            self.mark_dirty();
        }

        /// Whether `event`'s location is within the button's bounds (a click that ends on
        /// it). Converts the window-space event point into this view's coordinates.
        fn point_inside(&self, event: Id) -> bool {
            if event.is_null() {
                return false;
            }
            // SAFETY: `event` is the live NSEvent AppKit passed;
            // `locationInWindow` + `convertPoint_fromView` are side-effect-free
            // getters on the main thread; `None` source view means window
            // coordinates.
            let (p, b) = unsafe {
                let win_pt = appkit::send_point(event, sel!(locationInWindow));
                (
                    appkit::send_point_point_id(
                        self.as_id(),
                        sel!(convertPoint:fromView:),
                        win_pt,
                        Id::NIL,
                    ),
                    appkit::send_rect(self.as_id(), sel!(bounds)),
                )
            };
            p.x >= 0.0 && p.x <= b.size.width && p.y >= 0.0 && p.y <= b.size.height
        }

        /// Request a repaint (after a hover / press change).
        fn mark_dirty(&self) {
            // SAFETY: side-effect-free invalidation request on the live view, main thread.
            unsafe { appkit::send_v_bool(self.as_id(), sel!(setNeedsDisplay:), true) };
        }

        /// (Re)install a `mouseEnteredAndExited` tracking area covering the whole view (it
        /// follows resizes via `InVisibleRect`), removing any prior one.
        fn install_tracking_area(&self) {
            // SAFETY: remove the previous area, then build + add a fresh one covering the
            // current bounds — all standard AppKit calls on the main thread.
            // `-removeTrackingArea:`/`-addTrackingArea:` are `-(void)(id)`;
            // `-initWithRect:options:owner:userInfo:` is
            // `-(id)(NSRect, NSTrackingAreaOptions, id, NSDictionary *)` and
            // consumes the +1 from `+alloc`, which `Obj::from_owned` adopts. The
            // area's OWNER is this view and AppKit holds it weakly, which is
            // why the ivar retains it.
            unsafe {
                if let Some(old) = self.ivars().tracking.borrow_mut().take() {
                    appkit::send_v_id(self.as_id(), sel!(removeTrackingArea:), old.id());
                }
                let area = Obj::from_owned(appkit::send_id_rect_usize_id_id(
                    appkit::alloc(class(c"NSTrackingArea")),
                    sel!(initWithRect:options:owner:userInfo:),
                    appkit::send_rect(self.as_id(), sel!(bounds)),
                    consts::NS_TRACKING_HOVER_IN_VISIBLE_RECT,
                    self.as_id(),
                    Id::NIL,
                ));
                if let Some(area) = area {
                    appkit::send_v_id(self.as_id(), sel!(addTrackingArea:), area.id());
                    *self.ivars().tracking.borrow_mut() = Some(area);
                }
            }
        }
    }

    /// The mutable per-tab state a [`TabView`] needs at click/draw time. Held in
    /// `Cell`s/`RefCell`s because AppKit messages the view (`mouseDown:`/`drawRect:`)
    /// through a shared `&self`, and `set_window_tabs` updates the active flag in
    /// place on a tab switch without rebuilding the whole view.
    pub(crate) struct TabIvars {
        /// The `Wake` channel — clicks/drags relay through it.
        proxy: EventLoopProxy<Wake>,
        /// The window this tab belongs to, so a relayed `Wake` addresses the RIGHT
        /// window (a click on a non-frontmost window's strip acts on THAT window).
        window: WindowId,
        /// This tab's 0-based index, used by the select / close / move relays. Set at
        /// build time; tabs are rebuilt (not re-indexed) on every `set_window_tabs`.
        /// POSITIONAL — the context-menu relay deliberately does NOT use it (a
        /// mid-menu reorder would misdirect the action); that path rides `tab_id`.
        index: Cell<usize>,
        /// This tab's STABLE canonical identity ([`crate::tab_model::TabId`]) — the
        /// chip's link back to the app's tab model that survives reorders. The diff
        /// path RE-STAMPS it per position (a `move_tab` re-labels positions, so the
        /// id at each position changes), and [`Self::show_context_menu`] snapshots
        /// it into `menu_tab` at pop time. Ids come from a never-reusing allocator,
        /// so a stale one can only miss, never alias a different tab.
        tab_id: Cell<TabId>,
        /// The identity captured when the context menu POPPED — what
        /// `tabMenuAction:` posts, NOT the live `tab_id`. The menu's nested
        /// tracking session still delivers winit user events, so a strip refresh
        /// mid-track can re-stamp `tab_id` under the open menu (the menu keeps its
        /// visual snapshot); reading the live cell at CLICK time would then target
        /// whatever tab drifted under the pointer — the exact wrong-tab bug the
        /// stable id exists to prevent. `None` only before the first pop.
        menu_tab: Cell<Option<TabId>>,
        /// Total tab count at build time, so a drag can clamp the destination index.
        count: Cell<usize>,
        /// Whether this is the ACTIVE tab — drives the accent in `drawRect:` and forces
        /// the close × to always show (inactive tabs reveal it only on hover).
        active: Cell<bool>,
        /// Canonical app identity rendered from the shared primitive IR. `None` is an
        /// unknown metadata value and intentionally paints no icon/blank slot.
        icon: Cell<Option<TabIconKind>>,
        /// Unsaved document state: a small accent dot in the reserved status slot.
        dirty: Cell<bool>,
        /// Background work state: a hollow ring in the same compact status canvas.
        busy: Cell<bool>,
        /// User-attention state: an orange diamond, independently visible from dirty
        /// and busy.
        attention: Cell<bool>,
        /// Connection-mark role (design §4): the fourth shape in the status
        /// canvas — ▲ outbound / ▽ inbound / hourglass both, in the muted busy
        /// ink. `None` draws nothing.
        conn: Cell<Option<TabConnRole>>,
        /// App-pushed drag-to-connect DROP-TARGET highlight (design §3.2):
        /// this chip hosts the session under a live connection drag's cursor.
        /// Arrives through the same [`TabStripMetadata`] push as every other
        /// chip fact (`set_metadata`); paints an accent ring in `drawRect:`.
        drop_target: Cell<bool>,
        /// Where the §3.1 connector gesture PRESSED, in this view's own
        /// coordinates — `Some` exactly while a connector press is being
        /// disambiguated (menu vs drag); [`Self::mouseUp`] takes it. The
        /// connector check runs BEFORE select/rename/reorder in `mouseDown:`
        /// and suppresses `SelectTab` for that press (the reorder-latch
        /// precedent, one gesture at a time).
        conn_press: Cell<Option<CGPoint>>,
        /// Whether the connector press crossed [`CONN_DRAG_START_PTS`] and
        /// became a connection drag (posted `Wake::ConnDragBegin`); the
        /// release then posts the drop instead of opening the menu.
        conn_dragging: Cell<bool>,
        /// Canonical close policy. The 24pt close target keeps its geometry, but a
        /// non-closable tab never reveals or dispatches the button.
        closable: Cell<bool>,
        /// Whether the pointer is currently inside this tab (hover) — reveals the × on
        /// an inactive tab. Toggled by the tracking-area enter/exit.
        hovered: Cell<bool>,
        /// The mouse-down location in the tab's own coordinates, kept so `mouseDragged:`
        /// can measure horizontal travel and decide a reorder direction.
        press_x: Cell<f64>,
        /// Whether the current gesture has already fired a reorder (so a long drag
        /// fires at most one `Move` per press — avoids a stutter of swaps).
        dragged: Cell<bool>,
        /// SOLO MODE: this is the window's ONLY tab, so the chip stops being a
        /// switcher and becomes the window title — no pill, no accent keyline, no
        /// close ✕, and the description label beside the title. Set from the tab
        /// COUNT at layout time; a count change always rebuilds the strip, so this
        /// can only flip on a rebuild.
        solo: Cell<bool>,
        /// The strip's MEASURED content centre line ([`StripMetrics::center_y`]) in
        /// this view's own coordinates — every vertical slot derives from it, so the
        /// chip sits on the traffic lights' optical row rather than the cell's own
        /// midpoint.
        center_y: Cell<f64>,
        /// SOLO MODE: the horizontal centre the title group aligns to, in this view's
        /// coordinates — the WINDOW's centre, not the band's, so a one-tab title reads
        /// centred in the window even though the band is offset by the stoplights and
        /// the trailing "+".
        solo_center_x: Cell<f64>,
        /// Draw the leading-edge divider ([`TabGeometry::separates`]) — what tells the
        /// eye that three equal full-width titles are three TABS.
        separator: Cell<bool>,
        /// Whether the current geometry reserves a close slot at all
        /// ([`NativeTabContentLayout::close_available`]), cached by [`Self::relayout`]
        /// so a hover reveal is a single `setHidden:` and never a re-layout.
        close_available: Cell<bool>,
        /// Where [`Self::paint_identity`] draws the app icon and the status canvas,
        /// resolved by [`Self::relayout`] for whichever mode is live. `None` = that
        /// ornament has no room (or no facts) and is not painted; it stays visible in
        /// the tooltip, the context menu, and accessibility either way.
        icon_rect: Cell<Option<[f64; 4]>>,
        status_rect: Cell<Option<[f64; 4]>>,
        /// The retained close `NSButton` and title `NSTextField`, so the view can
        /// show/hide the × on hover and so the handle keeps them alive.
        close_btn: RefCell<Option<Obj>>,
        label: RefCell<Option<Obj>>,
        /// SOLO MODE: the retained DESCRIPTION `NSTextField` drawn beside the title
        /// (hidden in every multi-tab layout). Its text is [`super::solo_subtitle`] of
        /// the composed session chrome — the same facts the hover card shows.
        desc_label: RefCell<Option<Obj>>,
        /// The retained tracking area, so `updateTrackingAreas` can swap it on a frame
        /// change without leaking the old one.
        tracking: RefCell<Option<Obj>>,
        /// Canonical undecorated tab title for accessibility and introspection.
        title: RefCell<String>,
        /// Full route/document context supplied by `TabPresentation::tooltip` /
        /// the composed session chrome (`TabChromeExt::tooltip` — the two agree
        /// by construction: `App::tab_chrome_ext` writes the composed tooltip
        /// back onto the presentation). Doubles as the CHANGE GATE, so a steady
        /// refresh never re-touches AppKit; the on-glass view tooltip itself is
        /// composed by `sync_semantics` (title + this + status).
        tooltip: RefCell<Option<String>>,
        /// The composed CONTEXT-MENU model for this tab (`session_chrome::
        /// compose_tab_menu`) — rendered as a native `NSMenu` on right-click /
        /// ctrl-click and read back verbatim by [`read_tab_menus`] for the
        /// `chrome` mirror. Stored as the MODEL (not a prebuilt `NSMenu`): the
        /// menu is built fresh per pop, so a stale retained menu can never
        /// outlive its facts.
        menu_entries: RefCell<Vec<TabMenuEntry>>,
        /// The CONNECTION rows of the menu snapshot the OPEN context menu was
        /// built from, in encounter order — what a `tabMenuConnection:` click
        /// decodes its item `tag` (an index into this Vec) against. Captured
        /// by [`Self::show_context_menu`] at pop time, the `menu_tab`
        /// discipline: a mid-track strip refresh may rewrite `menu_entries`,
        /// but the popped menu keeps its visual snapshot, so the click must
        /// decode against that SAME snapshot — an index into the live model
        /// could name a different peer.
        menu_conn: RefCell<Vec<(aterm_session::SessionId, ConnVerb)>>,
    }

    aterm_objc::declare_class! {
        /// One tab's view: a custom `NSView` that draws its (in)active background +
        /// accent, hosts the title label + close ×, tracks hover to reveal the ×, and
        /// turns mouse-down/drag into select / reorder `Wake`s. Built fresh per tab on
        /// each [`set_window_tabs`].
        ///
        /// Declared with [`aterm_objc::declare_class!`], on `NSView`. Thirteen
        /// methods — the widest declared class in the tree, and the one that
        /// makes the arity check earn its keep: every `@sel(…)` here is
        /// reconciled against its Rust argument count at COMPILE time, which is
        /// the failure `objc2`'s attribute could not express and the runtime
        /// would otherwise have discovered through `NSMethodSignature`.
        ///
        /// It also carries the third `acceptsFirstMouse:` — the per-arch `BOOL`
        /// D3 fixed — and `accessibilityPerformPress`.
        ///
        /// The METHOD BODIES are first-party too now: every AppKit send below
        /// goes through `crate::appkit`'s typed prototypes and this module's
        /// drawing vocabulary, with `self.as_id()` as the receiver. What was
        /// first-party already — the class pair, the ivars, the `dealloc`, the
        /// thirteen trampolines and every encoding — is untouched.
        pub(crate) struct TabView: NSView {
            const NAME: &str = "ATermTabView";
            type Ivars = TabIvars;
            protocols: [NSObject];
            /// Paint the tab background: the ACTIVE tab is a semantic selected surface
            /// drawn as a CURVED, inset "pill", with a border, accent keyline and
            /// semibold label; an inactive tab is
            /// flat (the seamless terminal-coloured titlebar shows through) so it recedes;
            /// a hovered inactive tab gets a fainter rounded pill. The fill is an
            /// `NSBezierPath` rounded rect — `bezierPathWithRoundedRect:xRadius:yRadius:`
            /// and `fill` are non-raising drawing calls (no raising initializer).
            @sel(drawRect:)
            fn drawRect(&self, _dirty: CGRect) {
                // SAFETY: `-bounds` is a side-effect-free `-(NSRect)` getter on
                // this live view, inside AppKit's own draw cycle.
                let bounds = unsafe { appkit::send_rect(self.as_id(), sel!(bounds)) };
                let ivars = self.ivars();
                if ivars.solo.get() {
                    // SOLO: the window's only tab is its TITLE, not a switcher. There is
                    // nothing to select away from, so no pill, no keyline, and no
                    // selected surface — just the title group over the seamless
                    // terminal-coloured titlebar, exactly like macOS Terminal.
                    //
                    // Except while it is the DROP TARGET of a live connection
                    // drag (design §3.2): the lone tab is still a session, so
                    // the band draws the accent keyline the chip highlight
                    // uses — App-pushed via `set_metadata`, cleared with the
                    // drag.
                    if ivars.drop_target.get() {
                        // SAFETY: standard AppKit drawing on the main thread
                        // inside a draw cycle (as below).
                        unsafe {
                            let bounds = appkit::send_rect(self.as_id(), sel!(bounds));
                            appkit::send_v(ns_color(sel!(controlAccentColor)), sel!(set));
                            let keyline = rect(
                                bounds.origin.x + 8.0,
                                ivars.center_y.get() - TAB_PILL_HEIGHT * 0.5,
                                (bounds.size.width - 16.0).max(0.0),
                                2.0,
                            );
                            appkit::send_v(rounded_rect(keyline, 1.0), sel!(fill));
                        }
                    }
                    self.paint_identity();
                    return;
                }
                // The pill is inset horizontally from the tab cell and CENTRED on the
                // strip's measured content line, so it floats with a small margin (the
                // curved iTerm look) on the traffic lights' optical row.
                let pill = rect(
                    bounds.origin.x + TAB_PILL_INSET_X,
                    ivars.center_y.get() - TAB_PILL_HEIGHT * 0.5,
                    (bounds.size.width - 2.0 * TAB_PILL_INSET_X).max(0.0),
                    TAB_PILL_HEIGHT,
                );
                // SAFETY: standard AppKit drawing primitives, on the main thread inside
                // a draw cycle (AppKit has set up the focused graphics context). The
                // colors are AUTORELEASED (+0) and consumed in this frame; `-set`,
                // `-fill` and `-stroke` are `-(void)` and none of these raise.
                unsafe {
                    if ivars.active.get() {
                        // Active tab: semantic selected surface + border + accent keyline.
                        // These AppKit colours resolve through the strip appearance pinned
                        // by `set_strip_dark`, so selection stays equally explicit in light
                        // and dark terminal themes without hard-coded white-on-dark logic.
                        // A user-picked `active_tab_color` override (the Tab Color
                        // settings page) replaces the translucent system surface with
                        // the exact chosen color; label ink flips by its luminance
                        // (`active_label_color`), so any pick stays readable.
                        let panel = match active_tab_color_override() {
                            Some([r, g, b]) => srgb(
                                f64::from(r) / 255.0,
                                f64::from(g) / 255.0,
                                f64::from(b) / 255.0,
                                1.0,
                            ),
                            None => with_alpha(
                                ns_color(sel!(selectedContentBackgroundColor)),
                                0.42,
                            ),
                        };
                        appkit::send_v(panel, sel!(set));
                        let path = rounded_rect(pill, TAB_PILL_RADIUS);
                        appkit::send_v(path, sel!(fill));
                        appkit::send_v(ns_color(sel!(separatorColor)), sel!(set));
                        appkit::send_v_f64(path, sel!(setLineWidth:), 0.75);
                        appkit::send_v(path, sel!(stroke));

                        let keyline = rect(
                            pill.origin.x + 8.0,
                            pill.origin.y,
                            (pill.size.width - 16.0).max(0.0),
                            2.0,
                        );
                        appkit::send_v(ns_color(sel!(controlAccentColor)), sel!(set));
                        appkit::send_v(rounded_rect(keyline, 1.0), sel!(fill));
                    } else if ivars.hovered.get() {
                        // Inactive but hovered: a fainter rounded pill so the hover target
                        // reads (and the revealed × has a backing).
                        appkit::send_v(with_alpha(ns_color(sel!(labelColor)), 0.07), sel!(set));
                        appkit::send_v(rounded_rect(pill, TAB_PILL_RADIUS), sel!(fill));
                    } else if ivars.separator.get() {
                        // Flat and quiet — the seamless terminal-coloured titlebar shows
                        // through — but a hairline on the leading edge, because equal
                        // full-width cells give three identical titles no other cue that
                        // they are three TABS. macOS Terminal draws the same divider.
                        // Suppressed next to the selected pill and while hovered: both
                        // already draw their own edge, and a rule beside one reads as
                        // grime. Deliberately short, so it separates without boxing.
                        appkit::send_v(ns_color(sel!(separatorColor)), sel!(set));
                        let rule = rect(
                            bounds.origin.x,
                            ivars.center_y.get() - TAB_RULE_HEIGHT * 0.5,
                            1.0,
                            TAB_RULE_HEIGHT,
                        );
                        appkit::send_v(
                            appkit::send_id_rect(
                                class(c"NSBezierPath").as_id(),
                                sel!(bezierPathWithRect:),
                                rule,
                            ),
                            sel!(fill),
                        );
                    }
                    // DROP-TARGET highlight (design §3.2, App-pushed through
                    // `set_metadata`): the chip under a live connection drag
                    // wears an accent ring + faint accent wash over whatever
                    // state it painted above, so the target reads even when
                    // the pointer is captured by another window. Purely
                    // paint — the select/reorder geometry is untouched.
                    if ivars.drop_target.get() {
                        let wash = with_alpha(ns_color(sel!(controlAccentColor)), 0.18);
                        appkit::send_v(wash, sel!(set));
                        let path = rounded_rect(pill, TAB_PILL_RADIUS);
                        appkit::send_v(path, sel!(fill));
                        appkit::send_v(ns_color(sel!(controlAccentColor)), sel!(set));
                        appkit::send_v_f64(path, sel!(setLineWidth:), 2.0);
                        appkit::send_v(path, sel!(stroke));
                    }
                }
                self.paint_identity();
            }

            /// A click anywhere on the tab (that is not the close ×, which is a button
            /// with its own action) selects it: post `Wake::SelectTab { window, index }`.
            /// Also seeds the drag gesture (records the press X, clears the dragged
            /// latch). A CTRL-click is the synthesized right-click (the macOS
            /// convention AppKit itself applies to controls): it pops the session
            /// context menu instead of selecting — checked FIRST so a ctrl-click
            /// never half-runs the select/drag gesture.
            @sel(mouseDown:)
            fn mouseDown(&self, event: Id) {
                // SAFETY: `-modifierFlags` is a side-effect-free
                // `-(NSEventModifierFlags)` — an NSUInteger bitmask — on the
                // live NSEvent AppKit passed, on the main thread.
                let ctrl = unsafe { appkit::send_usize(event, sel!(modifierFlags)) }
                    & consts::NS_EVENT_MODIFIER_FLAG_CONTROL
                    != 0;
                if ctrl {
                    self.show_context_menu(event);
                    return;
                }
                // THE §3.1 CONNECTOR — the status-canvas cell — checked BEFORE
                // the double-click / select / reorder seeds (the design's
                // ordering mandate): a press here ARMS the connect gesture and
                // suppresses `SelectTab` for this press. `mouseUp:` within the
                // movement threshold opens the Connections menu (the same
                // context menu, aimed by the stable tab id); `mouseDragged:`
                // past it becomes a connection drag. Only exists while a
                // status canvas painted (`status_rect` mirrors the pixels), so
                // the hit region can never outrun the ink.
                // SAFETY: `locationInWindow` + `convertPoint_fromView` are
                // side-effect-free getters on the main thread; `None` source
                // view means window coordinates.
                let ivars = self.ivars();
                // SAFETY: `-locationInWindow` is `-(NSPoint)` on the live event
                // and `-convertPoint:fromView:` is `-(NSPoint)(NSPoint, id)`
                // with a NIL source view meaning WINDOW coordinates.
                let p = unsafe { self.point_in_view(event) };
                if let Some([sx, sy, sw, sh]) = ivars.status_rect.get()
                    && p.x >= sx - CONN_HIT_SLOP_PTS
                    && p.x <= sx + sw + CONN_HIT_SLOP_PTS
                    && p.y >= sy - CONN_HIT_SLOP_PTS
                    && p.y <= sy + sh + CONN_HIT_SLOP_PTS
                {
                    ivars.conn_press.set(Some(p));
                    ivars.conn_dragging.set(false);
                    return;
                }
                // A DOUBLE-click renames the tab's focused session in place. AppKit
                // has already applied the system double-click interval and movement
                // slop, so the count IS the whole gesture test — no timer, no
                // per-chip click FSM. Placed after the ctrl guard (a ctrl
                // double-click stays "context menu") and before the drag seed:
                // latching `dragged` makes `mouseDragged:` short-circuit, so a hand
                // wobble during the double-click cannot also post a reorder — which
                // would re-stamp every chip's identity under the opening editor. The
                // first press already posted its `SelectTab`, so this posts no
                // second selection. `== 2`, not `>= 2`: further clicks land on the
                // field once it exists, and an early one is harmless because
                // `begin_session_rename` is idempotent for the same session.
                // SAFETY: `clickCount` is a side-effect-free getter on the live
                // event, on the main thread.
                if unsafe { appkit::send_isize(event, sel!(clickCount)) } == 2 {
                    let ivars = self.ivars();
                    ivars.dragged.set(true);
                    let _ = ivars.proxy.send_event(Wake::BeginSessionRename {
                        window: ivars.window,
                        // Captured NOW: an editor lives for seconds, and the diff
                        // path re-stamps this chip's live id per POSITION.
                        tab: ivars.tab_id.get(),
                    });
                    return;
                }
                // `p` was computed above for the connector check; the press
                // seed reuses it.
                ivars.press_x.set(p.x);
                ivars.dragged.set(false);
                let _ = ivars.proxy.send_event(Wake::SelectTab {
                    window: ivars.window,
                    index: ivars.index.get(),
                });
            }

            /// Right-click on the tab: pop the SESSION CONTEXT MENU at the pointer
            /// (session-metadata stage 2) — identity headers, the recent timeline
            /// tail, and the Copy Session ID / Copy CWD / Close Tab actions, all
            /// composed by `session_chrome::compose_tab_menu` (the same model the
            /// tooltip and the `chrome` mirror read). Deliberately does NOT
            /// select the tab first: inspecting a background session must not
            /// disturb the foreground one (native macOS tab behavior).
            @sel(rightMouseDown:)
            fn rightMouseDown(&self, event: Id) {
                self.show_context_menu(event);
            }

            /// A horizontal drag past one tab-width reorders this tab toward the drag
            /// direction (best-effort): post a `Wake::TabCmd { Move { from, to } }`
            /// exactly ONCE per press (the `dragged` latch), reusing `App::move_tab`.
            /// The reply channel is a throwaway (we don't block the UI thread on it).
            @sel(mouseDragged:)
            fn mouseDragged(&self, event: Id) {
                let ivars = self.ivars();
                // A CONNECTOR press (design §3.1) owns this drag: past the
                // point threshold it becomes a connection drag —
                // `Wake::ConnDragBegin` once, then screen-space tracking
                // (`Wake::ConnDragTo`) so the App-side machine resolves the
                // drop target across windows. Checked BEFORE the reorder
                // latch: the connector press never reorders.
                if let Some(press) = ivars.conn_press.get() {
                    // SAFETY: side-effect-free geometry getters, main thread.
                    let p = unsafe { self.point_in_view(event) };
                    if !ivars.conn_dragging.get() {
                        let (dx, dy) = (p.x - press.x, p.y - press.y);
                        if dx * dx + dy * dy
                            <= CONN_DRAG_START_PTS * CONN_DRAG_START_PTS
                        {
                            return; // §3.1: still within the menu-open slop
                        }
                        ivars.conn_dragging.set(true);
                        let _ = ivars.proxy.send_event(Wake::ConnDragBegin {
                            window: ivars.window,
                            // The STABLE id: the drag lives for seconds and
                            // the positional index re-stamps under it.
                            tab: ivars.tab_id.get(),
                        });
                    }
                    match self.winit_screen_point(event) {
                        Some((x, y)) => {
                            let _ = ivars.proxy.send_event(Wake::ConnDragTo {
                                window: ivars.window,
                                x,
                                y,
                            });
                        }
                        None => {
                            // No window/screen to convert against (teardown
                            // mid-gesture): dissolve rather than track lies.
                            ivars.conn_press.set(None);
                            ivars.conn_dragging.set(false);
                            let _ = ivars.proxy.send_event(Wake::ConnDragCancel {
                                window: ivars.window,
                            });
                        }
                    }
                    return;
                }
                if ivars.dragged.get() {
                    return; // already fired this gesture
                }
                // SAFETY: side-effect-free geometry getters, main thread.
                let (p, width) = unsafe {
                    (
                        self.point_in_view(event),
                        appkit::send_rect(self.as_id(), sel!(bounds))
                            .size
                            .width
                            .max(1.0),
                    )
                };
                let dx = p.x - ivars.press_x.get();
                // Require crossing roughly half a tab to commit a single-step reorder.
                if dx.abs() < width * 0.5 {
                    return;
                }
                let from = ivars.index.get();
                let count = ivars.count.get();
                let to = if dx > 0.0 {
                    (from + 1).min(count.saturating_sub(1))
                } else {
                    from.saturating_sub(1)
                };
                if to == from {
                    return;
                }
                ivars.dragged.set(true);
                let (tx, _rx) = std::sync::mpsc::channel();
                let _ = ivars.proxy.send_event(Wake::TabCmd {
                    action: TabAction::Move { from, to },
                    reply: tx,
                });
            }

            /// The CONNECTOR gesture's commit (design §3.1): a release still
            /// within the movement threshold opens the Connections menu — the
            /// same session context menu, the press-release half of the
            /// disambiguation — while a release past it posts the drop
            /// (`Wake::ConnDragDrop`) for the App-side §3.3 resolution.
            /// Non-connector presses have no release half (select/reorder are
            /// press-committed), so this is a no-op for them.
            @sel(mouseUp:)
            fn mouseUp(&self, event: Id) {
                let ivars = self.ivars();
                let Some(_press) = ivars.conn_press.take() else {
                    return;
                };
                if !ivars.conn_dragging.take() {
                    // Release in place: the §3.1 menu-open.
                    self.show_context_menu(event);
                    return;
                }
                match self.winit_screen_point(event) {
                    Some((x, y)) => {
                        let _ = ivars.proxy.send_event(Wake::ConnDragDrop {
                            window: ivars.window,
                            x,
                            y,
                        });
                    }
                    None => {
                        let _ = ivars.proxy.send_event(Wake::ConnDragCancel {
                            window: ivars.window,
                        });
                    }
                }
            }

            /// Pointer entered the tab — reveal the close ✕ (every tab hides it until
            /// hover, the selected one included) and repaint the faint hover highlight.
            @sel(mouseEntered:)
            fn mouseEntered(&self, _event: Id) {
                self.ivars().hovered.set(true);
                self.refresh_close_visibility();
                self.mark_dirty();
            }

            /// Pointer left the tab — hide the ✕ again (it is a hover-only affordance,
            /// on the selected tab too) and repaint.
            @sel(mouseExited:)
            fn mouseExited(&self, _event: Id) {
                self.ivars().hovered.set(false);
                self.refresh_close_visibility();
                self.mark_dirty();
            }

            /// Rebuild the tracking area whenever the view's geometry changes, so hover
            /// detection follows a resize. Removes the previous area first (no leak).
            @sel(updateTrackingAreas)
            fn updateTrackingAreas(&self) {
                // SAFETY: `[super updateTrackingAreas]` is `-(void)`;
                // `super_receiver()` pairs this live instance with the class
                // that DECLARES this method, so lookup starts at `NSView`.
                unsafe {
                    let sup = self.super_receiver();
                    let f: unsafe extern "C" fn(*const aterm_objc::ObjcSuper, aterm_objc::Sel) =
                        aterm_objc::msg_super();
                    f(&raw const sup, sel!(updateTrackingAreas));
                }
                self.install_tracking_area();
            }

            /// Accept the FIRST click even when the window is not key, so clicking a tab
            /// in a background window both raises it AND selects the tab in one click
            /// (matches native tab behavior).
            @sel(acceptsFirstMouse:)
            fn acceptsFirstMouse(&self, _event: Id) -> aterm_objc::Bool {
                aterm_objc::Bool::YES
            }

            /// VoiceOver press selects this tab through the same typed wake as a click.
            @sel(accessibilityPerformPress)
            fn accessibilityPerformPress(&self) -> aterm_objc::Bool {
                let ivars = self.ivars();
                let _ = ivars.proxy.send_event(Wake::SelectTab {
                    window: ivars.window,
                    index: ivars.index.get(),
                });
                aterm_objc::Bool::YES
            }

            /// `closeTab:` — the action wired to this tab's close × button. Posts a
            /// `Wake::CloseTab { window, index }` so the main loop closes THIS tab via
            /// `App::close_tab_at` (the same whole-tab close the `tab close` verb takes).
            @sel(closeTab:)
            fn close_tab(&self, _sender: Id) {
                let ivars = self.ivars();
                if !ivars.closable.get() {
                    return;
                }
                let _ = ivars.proxy.send_event(Wake::CloseTab {
                    window: ivars.window,
                    index: ivars.index.get(),
                });
            }

            /// `tabMenuAction:` — the action wired to every LIVE item of this tab's
            /// context menu. Mirrors the menu bar's `menuAction:` relay exactly:
            /// the item's `tag` decodes to a [`MenuAction`] (an undecodable tag is
            /// inert), then a `Wake::TabMenuAction` posts it WITH this tab's
            /// POP-TIME stable identity (`menu_tab`, snapshotted by
            /// [`Self::show_context_menu`]) — the clicked tab need not be the
            /// active one, so the plain `Wake::MenuAction` (front-active-tab)
            /// convention would address the wrong session. NEVER the positional
            /// `index` ivar and NEVER the live `tab_id`: both can be re-labeled by
            /// a strip refresh while the menu tracks (see the `menu_tab` ivar doc),
            /// and the user chose an action for the tab the menu was popped ON.
            /// The dispatcher re-resolves the id and no-ops if that tab is gone.
            @sel(tabMenuAction:)
            fn tab_menu_action(&self, sender: Id) {
                if sender.is_null() {
                    return;
                }
                // SAFETY: `sender` is the live NSMenuItem AppKit passed as the
                // action sender; `-tag` is `-(NSInteger)`, side-effect free.
                let tag = unsafe { appkit::send_isize(sender, sel!(tag)) };
                if let Some(action) = MenuAction::from_tag(tag) {
                    let ivars = self.ivars();
                    // `None` cannot happen from a real click (the menu that targets
                    // this selector is only built by `show_context_menu`, which
                    // stamps the capture first) — but a defensive drop beats a
                    // guessed target.
                    let Some(tab) = ivars.menu_tab.get() else { return };
                    // Fire-and-forget: a closed loop (app shutting down) just drops
                    // the event — mirrors every other `send_event` here.
                    let _ = ivars.proxy.send_event(Wake::TabMenuAction {
                        window: ivars.window,
                        tab,
                        action,
                    });
                }
            }

            /// `tabMenuConnection:` — the action wired to every per-peer
            /// CONNECTION row of this tab's context menu. The item's `tag` is
            /// an INDEX into the pop-time row snapshot (`menu_conn`, stamped
            /// by [`Self::show_context_menu`] — the `menu_tab` discipline),
            /// never a `MenuAction` tag: the act needs a peer argument no
            /// fieldless tag can carry. An out-of-range tag is inert
            /// (defensive — only `show_context_menu` builds these items).
            @sel(tabMenuConnection:)
            fn tab_menu_connection(&self, sender: Id) {
                if sender.is_null() {
                    return;
                }
                // SAFETY: `sender` is the live NSMenuItem AppKit passed as the
                // action sender; `-tag` is `-(NSInteger)`, side-effect free.
                let tag = unsafe { appkit::send_isize(sender, sel!(tag)) };
                let ivars = self.ivars();
                let Ok(i) = usize::try_from(tag) else { return };
                let row = ivars.menu_conn.borrow().get(i).cloned();
                let Some((peer_sid, verb)) = row else { return };
                // `None` cannot happen from a real click (see `tabMenuAction:`).
                let Some(tab) = ivars.menu_tab.get() else { return };
                let _ = ivars.proxy.send_event(Wake::TabMenuConnection {
                    window: ivars.window,
                    tab,
                    peer_sid,
                    verb,
                });
            }
        }
    }

    impl TabView {
        /// `event`'s cursor in THIS view's own coordinates — the two-send pair
        /// (`-locationInWindow` then `-convertPoint:fromView:` with a nil
        /// source, i.e. from window space) that four gesture paths make.
        ///
        /// # Safety
        /// `event` must be the live `NSEvent` AppKit passed to a responder
        /// method, which is never nil.
        unsafe fn point_in_view(&self, event: Id) -> CGPoint {
            // SAFETY: `-locationInWindow` is a side-effect-free `-(NSPoint)`
            // on the live event; `-convertPoint:fromView:` is
            // `-(NSPoint)(NSPoint, NSView *)` and a NIL source view is the
            // documented spelling of "these are window coordinates".
            unsafe {
                let win_pt = appkit::send_point(event, sel!(locationInWindow));
                appkit::send_point_point_id(
                    self.as_id(),
                    sel!(convertPoint:fromView:),
                    win_pt,
                    Id::NIL,
                )
            }
        }

        /// The drag event's cursor in WINIT screen space (physical px,
        /// top-left origin of the MAIN display) — the space the App-side
        /// window-frame registry's `inner_position` entries live in, so the
        /// native connector drag and the in-grid one resolve targets through
        /// identical arithmetic. The flip law is winit's own
        /// (`y_top = main_height − y_appkit`, main-display height from the
        /// same CoreGraphics call winit uses — the `platform.rs`
        /// `window_work_area_pts` precedent), then scaled by THIS window's
        /// backing factor exactly as winit scales its position reports.
        /// (Mixed-DPI desktops share winit's own cross-scale imprecision —
        /// recorded §3.2 note.) `None` when the view has no window (teardown).
        fn winit_screen_point(&self, event: Id) -> Option<(f64, f64)> {
            #[link(name = "CoreGraphics", kind = "framework")]
            unsafe extern "C" {
                fn CGMainDisplayID() -> u32;
                fn CGDisplayBounds(display: u32) -> CGRect;
            }
            // SAFETY: geometry getters on the live event/window on the main
            // thread — `-window` is `-(id)`, `-locationInWindow` is
            // `-(NSPoint)`, `-convertPointToScreen:` is `-(NSPoint)(NSPoint)`
            // and `-backingScaleFactor` is `-(CGFloat)`; the CoreGraphics pair
            // is a flat display-bounds getter.
            unsafe {
                let window = appkit::send_id(self.as_id(), sel!(window));
                if window.is_null() {
                    return None;
                }
                let win_pt = appkit::send_point(event, sel!(locationInWindow));
                let screen_pt =
                    appkit::send_point_point(window, sel!(convertPointToScreen:), win_pt);
                let scale = appkit::send_f64(window, sel!(backingScaleFactor));
                let main_h = CGDisplayBounds(CGMainDisplayID()).size.height;
                Some((screen_pt.x * scale, (main_h - screen_pt.y) * scale))
            }
        }

        /// Paint the shared code-native icon IR and independent dirty/busy/
        /// attention/connection status shapes. AppKit's view
        /// coordinate system points upward, so the top-left 16×16 design box is flipped
        /// once here; primitive ordering and dimensions otherwise match the in-grid
        /// RawRgba8 raster exactly.
        fn paint_identity(&self) {
            let ivars = self.ivars();
            let metadata = self.metadata();
            // Geometry is resolved once by `relayout` (which also placed the labels and
            // the ✕) and cached, so the draw cycle never re-derives it — and the painted
            // ornaments are guaranteed to agree with the laid-out text.
            let layout_icon = ivars.icon_rect.get();
            let layout_status = ivars.status_rect.get();
            // SAFETY: all operations are standard AppKit drawing calls made from
            // `drawRect:` on the main thread with an active graphics context.
            unsafe {
                let ink = if ivars.active.get() || ivars.solo.get() {
                    ns_color(sel!(labelColor))
                } else {
                    ns_color(sel!(secondaryLabelColor))
                };
                appkit::send_v(ink, sel!(set));
                if let (Some(kind), Some(icon)) = (ivars.icon.get(), layout_icon) {
                    let scale = (icon[2] / f64::from(TAB_ICON_DESIGN_SIZE))
                        .min(icon[3] / f64::from(TAB_ICON_DESIGN_SIZE));
                    let ox = icon[0] + (icon[2] - f64::from(TAB_ICON_DESIGN_SIZE) * scale) * 0.5;
                    let oy = icon[1] + (icon[3] - f64::from(TAB_ICON_DESIGN_SIZE) * scale) * 0.5;
                    let at = |p: [f32; 2]| {
                        point(
                            ox + f64::from(p[0]) * scale,
                            oy + (f64::from(TAB_ICON_DESIGN_SIZE) - f64::from(p[1])) * scale,
                        )
                    };
                    for primitive in tab_icon_primitives(kind) {
                        match *primitive {
                            TabIconPrimitive::Line { from, to, width } => {
                                let path = bezier_path();
                                appkit::send_v_f64(
                                    path,
                                    sel!(setLineWidth:),
                                    f64::from(width) * scale,
                                );
                                appkit::send_v_usize(
                                    path,
                                    sel!(setLineCapStyle:),
                                    consts::NS_LINE_CAP_STYLE_ROUND,
                                );
                                appkit::send_v_point(path, sel!(moveToPoint:), at(from));
                                appkit::send_v_point(path, sel!(lineToPoint:), at(to));
                                appkit::send_v(path, sel!(stroke));
                            }
                            TabIconPrimitive::RoundedRect {
                                rect: rect_,
                                radius,
                                width,
                            } => {
                                let frame = rect(
                                    ox + f64::from(rect_[0]) * scale,
                                    oy + (f64::from(TAB_ICON_DESIGN_SIZE)
                                        - f64::from(rect_[1] + rect_[3]))
                                        * scale,
                                    f64::from(rect_[2]) * scale,
                                    f64::from(rect_[3]) * scale,
                                );
                                let path = rounded_rect(frame, f64::from(radius) * scale);
                                appkit::send_v_f64(
                                    path,
                                    sel!(setLineWidth:),
                                    f64::from(width) * scale,
                                );
                                appkit::send_v(path, sel!(stroke));
                            }
                            TabIconPrimitive::Dot { center, radius } => {
                                let centre = at(center);
                                let radius = f64::from(radius) * scale;
                                appkit::send_v(
                                    oval_in_rect(rect(
                                        centre.x - radius,
                                        centre.y - radius,
                                        radius * 2.0,
                                        radius * 2.0,
                                    )),
                                    sel!(fill),
                                );
                            }
                            TabIconPrimitive::Triangle { points } => {
                                // No app icon uses a filled triangle today (the
                                // primitive exists for the connection mark, drawn
                                // below from cached status geometry), but the IR
                                // walk stays total so a future icon just works.
                                let path = bezier_path();
                                appkit::send_v_point(path, sel!(moveToPoint:), at(points[0]));
                                appkit::send_v_point(path, sel!(lineToPoint:), at(points[1]));
                                appkit::send_v_point(path, sel!(lineToPoint:), at(points[2]));
                                appkit::send_v(path, sel!(closePath));
                                appkit::send_v(path, sel!(fill));
                            }
                        }
                    }
                }
                if let Some(status) = layout_status {
                    let count = metadata.status_count();
                    let mut ordinal = 0usize;
                    // The same shared shrink the in-grid rasterizer applies for
                    // the four-mark packing, folded into the point scale so
                    // every dimension below tracks it.
                    let scale = status[2] / f64::from(TAB_ICON_DESIGN_SIZE)
                        * f64::from(tab_status_mark_scale(count));
                    for kind in TAB_STATUS_KINDS {
                        if !metadata.has_status_kind(kind) {
                            continue;
                        }
                        let x = status[0]
                            + f64::from(tab_status_center(ordinal, count))
                                * (status[2] / f64::from(TAB_ICON_DESIGN_SIZE));
                        let y = status[1] + status[3] * 0.5;
                        ordinal += 1;
                        match kind {
                            TabStatusKind::Dirty => {
                                appkit::send_v(ns_color(sel!(controlAccentColor)), sel!(set));
                                let radius = 1.75 * scale;
                                appkit::send_v(
                                    oval_in_rect(rect(
                                        x - radius,
                                        y - radius,
                                        radius * 2.0,
                                        radius * 2.0,
                                    )),
                                    sel!(fill),
                                );
                            }
                            TabStatusKind::Busy => {
                                appkit::send_v(ink, sel!(set));
                                let radius = 2.0 * scale;
                                let ring = oval_in_rect(rect(
                                    x - radius,
                                    y - radius,
                                    radius * 2.0,
                                    radius * 2.0,
                                ));
                                appkit::send_v_f64(ring, sel!(setLineWidth:), 1.25 * scale);
                                appkit::send_v(ring, sel!(stroke));
                            }
                            TabStatusKind::Attention => {
                                appkit::send_v(ns_color(sel!(systemOrangeColor)), sel!(set));
                                let radius = 2.5 * scale;
                                let diamond = bezier_path();
                                appkit::send_v_point(
                                    diamond,
                                    sel!(moveToPoint:),
                                    point(x, y + radius),
                                );
                                appkit::send_v_point(
                                    diamond,
                                    sel!(lineToPoint:),
                                    point(x + radius, y),
                                );
                                appkit::send_v_point(
                                    diamond,
                                    sel!(lineToPoint:),
                                    point(x, y - radius),
                                );
                                appkit::send_v_point(
                                    diamond,
                                    sel!(lineToPoint:),
                                    point(x - radius, y),
                                );
                                appkit::send_v(diamond, sel!(closePath));
                                appkit::send_v(diamond, sel!(fill));
                            }
                            // The connection mark (§4), in busy's muted ink and
                            // the exact geometry `tab_bar::status_primitives`
                            // rasterizes in-grid (AppKit's y points up, so the
                            // up-triangle apex is y + height here).
                            TabStatusKind::Connection => {
                                if let Some(role) = metadata.conn {
                                    appkit::send_v(ink, sel!(set));
                                    let (hw, hh) = (2.5 * scale, 2.4 * scale);
                                    let mv = sel!(moveToPoint:);
                                    let ln = sel!(lineToPoint:);
                                    match role {
                                        TabConnRole::Outbound => {
                                            let arrow = bezier_path();
                                            appkit::send_v_point(arrow, mv, point(x, y + hh));
                                            appkit::send_v_point(arrow, ln, point(x + hw, y - hh));
                                            appkit::send_v_point(arrow, ln, point(x - hw, y - hh));
                                            appkit::send_v(arrow, sel!(closePath));
                                            appkit::send_v(arrow, sel!(fill));
                                        }
                                        TabConnRole::Inbound => {
                                            let width = 1.15 * scale;
                                            // Half-stroke inset: the outer ink
                                            // silhouette matches outbound's fill.
                                            let (cw, ch) = (hw - width * 0.5, hh - width * 0.5);
                                            let arrow = bezier_path();
                                            appkit::send_v_f64(arrow, sel!(setLineWidth:), width);
                                            appkit::send_v_usize(
                                                arrow,
                                                sel!(setLineCapStyle:),
                                                consts::NS_LINE_CAP_STYLE_ROUND,
                                            );
                                            appkit::send_v_point(arrow, mv, point(x - cw, y + ch));
                                            appkit::send_v_point(arrow, ln, point(x + cw, y + ch));
                                            appkit::send_v_point(arrow, ln, point(x, y - ch));
                                            appkit::send_v(arrow, sel!(closePath));
                                            appkit::send_v(arrow, sel!(stroke));
                                        }
                                        TabConnRole::Both => {
                                            let hw = 2.2 * scale;
                                            let hourglass = bezier_path();
                                            appkit::send_v_point(
                                                hourglass,
                                                mv,
                                                point(x - hw, y + hh),
                                            );
                                            appkit::send_v_point(
                                                hourglass,
                                                ln,
                                                point(x + hw, y + hh),
                                            );
                                            appkit::send_v_point(
                                                hourglass,
                                                ln,
                                                point(x - hw, y - hh),
                                            );
                                            appkit::send_v_point(
                                                hourglass,
                                                ln,
                                                point(x + hw, y - hh),
                                            );
                                            appkit::send_v(hourglass, sel!(closePath));
                                            appkit::send_v(hourglass, sel!(fill));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        /// Build a fresh tab view for `index`/`count` showing `title`, wired to relay
        /// through `proxy`/`window`. Creates the sub-views (close ✕, title label,
        /// solo description label) and hands ALL geometry to [`Self::relayout`], so
        /// build and every later in-place update place content through exactly one
        /// function. Installs the hover tracking area and sets the active flag (drives
        /// the accent). All construction is via NON-RAISING factory initializers on
        /// the main thread.
        #[allow(
            clippy::too_many_arguments,
            reason = "tab state (id/index/count/text/active/metadata/ext) plus its render context (mtm/proxy/window/geometry); both are needed at construction and splitting them only relocates the list"
        )]
        fn build(
            mtm: MainThread,
            proxy: EventLoopProxy<Wake>,
            window: WindowId,
            tab: TabId,
            index: usize,
            count: usize,
            title: &str,
            tooltip: Option<&str>,
            active: bool,
            metadata: TabStripMetadata,
            ext: &TabChromeExt,
            frame: CGRect,
            geometry: TabGeometry,
        ) -> Option<Retained<Self>> {
            let ivars = TabIvars {
                proxy,
                window,
                index: Cell::new(index),
                tab_id: Cell::new(tab),
                menu_tab: Cell::new(None),
                count: Cell::new(count),
                active: Cell::new(active),
                icon: Cell::new(metadata.icon),
                dirty: Cell::new(metadata.dirty),
                busy: Cell::new(metadata.busy),
                attention: Cell::new(metadata.attention),
                conn: Cell::new(metadata.conn),
                drop_target: Cell::new(metadata.drop_target),
                conn_press: Cell::new(None),
                conn_dragging: Cell::new(false),
                closable: Cell::new(metadata.closable),
                hovered: Cell::new(false),
                press_x: Cell::new(0.0),
                dragged: Cell::new(false),
                solo: Cell::new(geometry.solo),
                center_y: Cell::new(geometry.center_y),
                solo_center_x: Cell::new(geometry.solo_center_x),
                separator: Cell::new(geometry.separator),
                close_available: Cell::new(false),
                icon_rect: Cell::new(None),
                status_rect: Cell::new(None),
                close_btn: RefCell::new(None),
                label: RefCell::new(None),
                desc_label: RefCell::new(None),
                tracking: RefCell::new(None),
                title: RefCell::new(title.to_string()),
                tooltip: RefCell::new(tooltip.map(str::to_string)),
                menu_entries: RefCell::new(Vec::new()),
                menu_conn: RefCell::new(Vec::new()),
            };
            // Two-step, for the reason `ChromeButton::build` gives: `alloc_init`
            // sends `-init` and an `NSView` subclass's designated initializer is
            // `-initWithFrame:`. The ivars are stored first, deliberately.
            // SAFETY: `alloc_ivars` returns a +1 instance owing exactly one
            // designated initializer, which is the next send; `-initWithFrame:`
            // is `-(id)(NSRect)` and consumes that +1, returning the
            // initialised +1 `Retained::from_owned` adopts.
            let this: Retained<Self> = unsafe {
                let raw = Self::alloc_ivars(mtm, ivars);
                if raw.is_null() {
                    return None;
                }
                Retained::from_owned(appkit::send_id_rect(raw, sel!(initWithFrame:), frame))?
            };

            // The close ✕ button: a small borderless title button (factory initializer,
            // NEVER `initWithFrame`). Its action targets THIS view's `closeTab:`. Born
            // HIDDEN — the ✕ is a hover-only reveal (see `refresh_close_visibility`).
            // SAFETY: `buttonWithTitle:target:action:` is the documented factory; plain
            // setters follow on the fresh button; all on the main thread.
            let close = unsafe {
                let btn = appkit::send_id_id_id_sel(
                    class(c"NSButton").as_id(),
                    sel!(buttonWithTitle:target:action:),
                    appkit::nsstring("✕")?.id(),
                    this.as_id(),
                    sel!(closeTab:),
                );
                // `+buttonWithTitle:target:action:` is a FACTORY, so its result
                // is +0 (autoreleased) and this retains it — `Obj::retain`, not
                // `from_owned`, which would over-release at the next pool pop.
                let btn = Obj::retain(btn)?;
                appkit::send_v_bool(btn.id(), sel!(setBordered:), false);
                appkit::send_v_usize(btn.id(), sel!(setImagePosition:), consts::NS_NO_IMAGE);
                appkit::send_v_id(btn.id(), sel!(setFont:), system_font(10.0, false));
                appkit::send_v_bool(btn.id(), sel!(setHidden:), true);
                appkit::send_v_id(
                    btn.id(),
                    sel!(setToolTip:),
                    appkit::nsstring("Close Tab")?.id(),
                );
                appkit::send_v_id(this.as_id(), sel!(addSubview:), btn.id());
                btn
            };

            // The title label and — solo only — the description beside it. Both are
            // non-editable, non-bezeled labels (factory initializer), so they pass the
            // mouse through to this view and the whole chip stays one click target.
            // SAFETY: `labelWithString:` is the documented non-raising factory; plain
            // setters follow; on the main thread.
            let (label, desc) = unsafe {
                // `+labelWithString:` is a FACTORY (+0), so both are retained.
                let lbl = Obj::retain(appkit::send_id_id(
                    class(c"NSTextField").as_id(),
                    sel!(labelWithString:),
                    appkit::nsstring(title)?.id(),
                ))?;
                let desc = Obj::retain(appkit::send_id_id(
                    class(c"NSTextField").as_id(),
                    sel!(labelWithString:),
                    appkit::nsstring("")?.id(),
                ))?;
                for field in [&lbl, &desc] {
                    appkit::send_v_bool(field.id(), sel!(setDrawsBackground:), false);
                    appkit::send_v_bool(field.id(), sel!(setBezeled:), false);
                    appkit::send_v_bool(field.id(), sel!(setEditable:), false);
                    appkit::send_v_bool(field.id(), sel!(setSelectable:), false);
                    // Single line, ellipsised on overflow: explicit on both counts so
                    // the row height is exact and a clipped title never masquerades as
                    // a complete one.
                    appkit::send_v_bool(field.id(), sel!(setUsesSingleLineMode:), true);
                    truncate_tail(field.id());
                }
                appkit::send_v_bool(desc.id(), sel!(setHidden:), true);
                appkit::send_v_id(this.as_id(), sel!(addSubview:), lbl.id());
                appkit::send_v_id(this.as_id(), sel!(addSubview:), desc.id());
                (lbl, desc)
            };

            *this.ivars().close_btn.borrow_mut() = Some(close);
            *this.ivars().label.borrow_mut() = Some(label);
            *this.ivars().desc_label.borrow_mut() = Some(desc);
            // Session chrome (tooltip + context-menu model) — the same in-place
            // applier the diff path uses, so build and diff share one seam.
            this.set_chrome_ext(ext);
            this.sync_close_semantics();
            this.relayout();
            this.sync_semantics();
            this.install_tracking_area();
            Some(this)
        }

        /// THE geometry seam: place every sub-view and cache every painted ornament's
        /// rect from the current bounds, mode, metadata, and measured centre line. Build
        /// and every in-place update (title, metadata, selection, re-centre) route
        /// through here, so the pixels can never disagree with the text — and so the
        /// solo/tabbed split lives in exactly one place.
        fn relayout(&self) {
            let ivars = self.ivars();
            // SAFETY: `-bounds` is a side-effect-free `-(NSRect)` getter.
            let bounds = unsafe { appkit::send_rect(self.as_id(), sel!(bounds)) };
            let center_y = ivars.center_y.get();
            let title = ivars.title.borrow().clone();
            if ivars.solo.get() {
                self.relayout_solo(&title, bounds, center_y);
                self.mark_dirty();
                return;
            }
            let metadata = self.metadata();
            let content = native_tab_content_layout(
                bounds.size.width,
                center_y,
                metadata.icon.is_some(),
                metadata.has_status(),
            );
            ivars.close_available.set(content.close_available);
            ivars.icon_rect.set(content.icon);
            ivars.status_rect.set(content.status);
            let active = ivars.active.get();
            // SAFETY: plain main-thread setters on our retained live sub-views; every
            // rect comes from `native_tab_content_layout` (finite, non-negative).
            unsafe {
                if let Some(btn) = ivars.close_btn.borrow().as_ref() {
                    appkit::send_v_rect(btn.id(), sel!(setFrame:), rect_of(content.close));
                }
                if let Some(desc) = ivars.desc_label.borrow().as_ref() {
                    appkit::send_v_bool(desc.id(), sel!(setHidden:), true);
                }
                if let Some(lbl) = ivars.label.borrow().as_ref() {
                    // Active = full label color (override-aware) and semibold; inactive
                    // = secondary (dim) and regular, Ghostty-like.
                    let color = if active {
                        active_label_color()
                    } else {
                        ns_color(sel!(secondaryLabelColor))
                    };
                    appkit::send_v_isize(
                        lbl.id(),
                        sel!(setAlignment:),
                        consts::NS_TEXT_ALIGNMENT_CENTER,
                    );
                    appkit::send_v_id(lbl.id(), sel!(setTextColor:), color);
                    appkit::send_v_id(lbl.id(), sel!(setFont:), system_font(12.0, active));
                    if let Some(text) = appkit::nsstring(&tab_display_label(
                        &title,
                        ivars.index.get(),
                        content.label[2],
                    )) {
                        appkit::send_v_id(lbl.id(), sel!(setStringValue:), text.id());
                    }
                    appkit::send_v_rect(lbl.id(), sel!(setFrame:), rect_of(content.label));
                }
            }
            self.refresh_close_visibility();
            self.mark_dirty();
        }

        /// Where the INLINE RENAME editor sits for this chip, in the CONTAINER's
        /// coordinates (the editor is a sibling of the chips, not a subview of
        /// one — chips are destroyed on every strip rebuild).
        ///
        /// Tabbed: exactly the pill `drawRect:` fills, so the field lands on the
        /// chip it replaces. Solo: the band is the whole window width, so a
        /// full-width field would read as a dialog — the box is capped and
        /// centred on the same measured centre the solo title group uses.
        fn editor_frame(&self) -> CGRect {
            let ivars = self.ivars();
            // SAFETY: `-frame` is a side-effect-free `-(NSRect)` getter.
            let frame = unsafe { appkit::send_rect(self.as_id(), sel!(frame)) };
            let y = frame.origin.y + ivars.center_y.get() - TAB_PILL_HEIGHT * 0.5;
            if ivars.solo.get() {
                let width = frame.size.width.min(SOLO_EDITOR_WIDTH);
                let center = frame.origin.x + ivars.solo_center_x.get();
                let x = (center - width * 0.5)
                    .max(frame.origin.x)
                    .min(frame.origin.x + frame.size.width - width);
                return rect(x, y, width, TAB_PILL_HEIGHT);
            }
            rect(
                frame.origin.x + TAB_PILL_INSET_X,
                y,
                (frame.size.width - 2.0 * TAB_PILL_INSET_X).max(1.0),
                TAB_PILL_HEIGHT,
            )
        }
        /// SOLO layout: the window's only tab reads as its TITLE — `title` in the
        /// primary ink beside its description in the secondary, centred as ONE group on
        /// the window's centre line, with the app icon leading and the status canvas
        /// trailing. No pill and no ✕: there is nothing to switch to, and the red
        /// traffic light already closes the window.
        ///
        /// The group is measured (`sizeToFit`) rather than guessed, then compressed
        /// description-first if the band cannot hold it, so a long `cwd` never pushes
        /// the title itself off the strip.
        fn relayout_solo(&self, title: &str, bounds: CGRect, center_y: f64) {
            let ivars = self.ivars();
            let metadata = self.metadata();
            let subtitle = crate::tab_bar::solo_subtitle(title, ivars.tooltip.borrow().as_deref());
            let available = (bounds.size.width - 2.0 * SOLO_EDGE_PAD).max(0.0);
            let icon_w = metadata
                .icon
                .map_or(0.0, |_| TAB_ICON_NATIVE_SIZE + SOLO_GAP);
            let status_w = if metadata.has_status() {
                SOLO_STATUS_SIZE + SOLO_GAP
            } else {
                0.0
            };
            ivars.close_available.set(false);
            // SAFETY: plain main-thread AppKit setters/getters on our retained live
            // sub-views; `sizeToFit` only resizes the label it is sent to.
            let (title_w, desc_w) = unsafe {
                if let Some(btn) = ivars.close_btn.borrow().as_ref() {
                    appkit::send_v_bool(btn.id(), sel!(setHidden:), true);
                }
                let mut title_w = 0.0;
                let mut desc_w = 0.0;
                if let Some(lbl) = ivars.label.borrow().as_ref() {
                    appkit::send_v_isize(
                        lbl.id(),
                        sel!(setAlignment:),
                        consts::NS_TEXT_ALIGNMENT_LEFT,
                    );
                    appkit::send_v_id(lbl.id(), sel!(setFont:), system_font(13.0, false));
                    appkit::send_v_id(lbl.id(), sel!(setTextColor:), ns_color(sel!(labelColor)));
                    if let Some(text) = appkit::nsstring(title) {
                        appkit::send_v_id(lbl.id(), sel!(setStringValue:), text.id());
                    }
                    appkit::send_v(lbl.id(), sel!(sizeToFit));
                    title_w = appkit::send_rect(lbl.id(), sel!(frame)).size.width;
                }
                if let Some(desc) = ivars.desc_label.borrow().as_ref() {
                    match subtitle.as_deref() {
                        Some(text) => {
                            appkit::send_v_bool(desc.id(), sel!(setHidden:), false);
                            appkit::send_v_isize(
                                desc.id(),
                                sel!(setAlignment:),
                                consts::NS_TEXT_ALIGNMENT_LEFT,
                            );
                            appkit::send_v_id(desc.id(), sel!(setFont:), system_font(12.0, false));
                            appkit::send_v_id(
                                desc.id(),
                                sel!(setTextColor:),
                                ns_color(sel!(secondaryLabelColor)),
                            );
                            if let Some(ns) = appkit::nsstring(text) {
                                appkit::send_v_id(desc.id(), sel!(setStringValue:), ns.id());
                            }
                            appkit::send_v(desc.id(), sel!(sizeToFit));
                            desc_w = appkit::send_rect(desc.id(), sel!(frame)).size.width;
                        }
                        None => appkit::send_v_bool(desc.id(), sel!(setHidden:), true),
                    }
                }
                (title_w, desc_w)
            };
            let desc_gap = if desc_w > 0.0 { SOLO_GAP } else { 0.0 };
            // Compress the DESCRIPTION first, then the title: the title is the one
            // string the band exists to show.
            let fixed = icon_w + status_w;
            let title_w = title_w.min((available - fixed).max(0.0));
            let desc_w = desc_w.min((available - fixed - title_w - desc_gap).max(0.0));
            let desc_gap = if desc_w > 0.0 { desc_gap } else { 0.0 };
            let total = fixed + title_w + desc_gap + desc_w;
            let mut x = (ivars.solo_center_x.get() - total * 0.5)
                .max(SOLO_EDGE_PAD)
                .min((bounds.size.width - SOLO_EDGE_PAD - total).max(SOLO_EDGE_PAD));

            ivars.icon_rect.set(metadata.icon.map(|_| {
                let rect = [
                    x,
                    center_y - TAB_ICON_NATIVE_SIZE * 0.5,
                    TAB_ICON_NATIVE_SIZE,
                    TAB_ICON_NATIVE_SIZE,
                ];
                x += TAB_ICON_NATIVE_SIZE + SOLO_GAP;
                rect
            }));
            // SAFETY: main-thread geometry setters on the retained live labels.
            unsafe {
                if let Some(lbl) = ivars.label.borrow().as_ref() {
                    appkit::send_v_rect(
                        lbl.id(),
                        sel!(setFrame:),
                        rect_of([
                            x,
                            center_y - SOLO_LABEL_HEIGHT * 0.5,
                            title_w,
                            SOLO_LABEL_HEIGHT,
                        ]),
                    );
                }
                x += title_w;
                if let Some(desc) = ivars.desc_label.borrow().as_ref() {
                    // A description the band could not fit is HIDDEN, not left at its
                    // measured size: a stale frame would paint the old text.
                    appkit::send_v_bool(desc.id(), sel!(setHidden:), desc_w <= 0.0);
                    if desc_w > 0.0 {
                        appkit::send_v_rect(
                            desc.id(),
                            sel!(setFrame:),
                            rect_of([
                                x + desc_gap,
                                center_y - SOLO_LABEL_HEIGHT * 0.5,
                                desc_w,
                                SOLO_LABEL_HEIGHT,
                            ]),
                        );
                    }
                }
                x += desc_gap + desc_w;
            }
            // `status_w` reserved the leading gap as well as the canvas, so spend it
            // here — otherwise a status with no description would sit a gap too far
            // left and pull the whole centred group off centre.
            ivars.status_rect.set(metadata.has_status().then_some([
                x + SOLO_GAP,
                center_y - SOLO_STATUS_SIZE * 0.5,
                SOLO_STATUS_SIZE,
                SOLO_STATUS_SIZE,
            ]));
        }

        /// Move this chip onto the strip's measured content line / window centre and
        /// switch it between the tabbed chip and the solo title band. Relayouts only
        /// when something actually moved, so a steady refresh costs nothing.
        fn set_geometry(&self, geometry: TabGeometry) {
            let ivars = self.ivars();
            let unchanged = ivars.solo.get() == geometry.solo
                && ivars.center_y.get() == geometry.center_y
                && ivars.solo_center_x.get() == geometry.solo_center_x
                && ivars.separator.get() == geometry.separator;
            if unchanged {
                return;
            }
            ivars.solo.set(geometry.solo);
            ivars.center_y.set(geometry.center_y);
            ivars.solo_center_x.set(geometry.solo_center_x);
            ivars.separator.set(geometry.separator);
            self.relayout();
        }

        /// Request a repaint of the whole tab (after a hover / active change). Wraps the
        /// `unsafe` `setNeedsDisplay:` setter, which is sound on the main thread.
        fn mark_dirty(&self) {
            // SAFETY: side-effect-free invalidation request on the live view, main thread.
            unsafe { appkit::send_v_bool(self.as_id(), sel!(setNeedsDisplay:), true) };
        }

        /// Update title + full route/document context in place. The visible label keeps
        /// the ⌘-hint only when it fits; hover, accessibility and introspection retain
        /// the complete semantic strings at every size. A conventional leading busy
        /// spinner stays on its first observed phase until its suffix or settled title
        /// changes, avoiding phase-rate AppKit and accessibility mutations.
        fn set_context(&self, title: &str, tooltip: Option<&str>) {
            let tooltip_changed = self.ivars().tooltip.borrow().as_deref() != tooltip;
            let title_changed = {
                let current = self.ivars().title.borrow();
                current.as_str() != title
                    && !super::busy_spinner_phase_only_change(current.as_str(), title)
            };
            if !title_changed && !tooltip_changed {
                return;
            }
            if tooltip_changed {
                *self.ivars().tooltip.borrow_mut() = tooltip.map(str::to_string);
            }
            if title_changed {
                *self.ivars().title.borrow_mut() = title.to_string();
                self.sync_close_semantics();
            }
            // A SOLO band shows the description too, so a tooltip-only change still
            // moves pixels there; a chip re-lays out only when its title changed.
            if title_changed || self.ivars().solo.get() {
                self.relayout();
            }
            self.sync_semantics();
        }

        /// Give the glyph-only close button a useful native accessibility name.
        /// The title follows live terminal OSC titles and native document routes, so
        /// refresh this alongside the visible tab context instead of freezing it at
        /// construction time.
        fn sync_close_semantics(&self) {
            let title = self.ivars().title.borrow();
            let label_text = tab_close_accessibility_label(&title);
            if let Some(btn) = self.ivars().close_btn.borrow().as_ref() {
                // SAFETY: NSButton inherits the NSAccessibility setters; these are
                // plain main-thread metadata updates on our retained live button.
                unsafe {
                    if let (Some(label), Some(help)) = (
                        appkit::nsstring(&label_text),
                        appkit::nsstring("Closes this tab"),
                    ) {
                        appkit::send_v_id(btn.id(), sel!(setToolTip:), label.id());
                        appkit::send_v_id(btn.id(), sel!(setAccessibilityLabel:), label.id());
                        appkit::send_v_id(btn.id(), sel!(setAccessibilityHelp:), help.id());
                    }
                }
            }
        }

        /// Apply the composed per-tab chrome extension IN PLACE (the stage-2 twin
        /// of [`Self::set_context`], run on both the diff and build paths): fold
        /// the hover tooltip into the semantic help when it CHANGED (the
        /// `tooltip` ivar is the gate — a steady refresh re-touches no AppKit
        /// state; the view's on-glass tooltip is OWNED by [`Self::sync_semantics`],
        /// which composes title + tooltip + status into one help string also used
        /// for accessibility — recomposing instead of raw-setting keeps the two
        /// writers from fighting), and store the context-menu MODEL for the next
        /// right-click / `read_tab_menus` read (plain `RefCell` swap, change-gated
        /// likewise). In practice the gate rarely fires after `set_context`: the
        /// composed `ext.tooltip` is written back onto the presentation by
        /// `App::tab_chrome_ext`, so both feeds agree by construction.
        fn set_chrome_ext(&self, ext: &TabChromeExt) {
            let ivars = self.ivars();
            if *ivars.tooltip.borrow() != ext.tooltip {
                *ivars.tooltip.borrow_mut() = ext.tooltip.clone();
                self.sync_semantics();
                // The SOLO band renders its DESCRIPTION from this very tooltip, so a
                // composed-chrome change is a visible change there (a chip's tooltip
                // is hover-only and needs no re-layout).
                if ivars.solo.get() {
                    self.relayout();
                }
            }
            if *ivars.menu_entries.borrow() != ext.menu {
                *ivars.menu_entries.borrow_mut() = ext.menu.clone();
            }
        }

        /// Re-stamp the STABLE tab identity this chip currently displays (the
        /// diff path's identity twin of [`Self::set_context`]): positions are
        /// re-labeled in place on a count-preserving refresh, so after a
        /// `move_tab` the id AT each position changes even though the views do
        /// not. Deliberately touches only the live `tab_id` cell — an OPEN
        /// context menu keeps its own pop-time `menu_tab` snapshot, which is
        /// the whole point of splitting the two.
        fn set_tab_id(&self, id: TabId) {
            self.ivars().tab_id.set(id);
        }

        /// The STABLE tab identity this chip currently displays. Read only where a
        /// gesture CAPTURES identity at its own start (the context menu's
        /// `menu_tab` snapshot, the rename editor's install) — never re-read to
        /// resolve a gesture already in flight, because the diff path re-stamps
        /// this per POSITION and would hand back the tab that slid into the slot.
        fn tab_id(&self) -> TabId {
            self.ivars().tab_id.get()
        }

        /// Build + pop this tab's native CONTEXT MENU at the pointer from the
        /// stored model. Empty model (a native tab / no chrome composed) pops
        /// nothing. The menu is constructed FRESH per pop — `NSMenu` retains its
        /// items and `popUpContextMenu` retains the menu for the (synchronous)
        /// tracking session, so nothing here outlives the click. Items:
        /// headers/timeline rows are DISABLED informational rows (tag 0 — the
        /// reserved never-dispatches tag), separators are native, and action rows
        /// carry the `MenuAction` tag targeting this view's `tabMenuAction:` (the
        /// same tag→action decode as the menu bar). `setAutoenablesItems(false)`
        /// because enabledness is the COMPOSER's decision (e.g. `Copy CWD` greys
        /// when no cwd is reported), not AppKit's responder-chain guess.
        fn show_context_menu(&self, event: Id) {
            // This custom NSView consumes right/ctrl-click without a winit
            // pointer event. Fence at OPEN, before the synchronous nested menu
            // loop: dismissing the menu produces no TabMenuAction wake at all.
            let _ = self.ivars().proxy.send_event(Wake::TabContextMenuOpening {
                window: self.ivars().window,
            });
            // CLONE the model out of the RefCell (dropping the borrow) BEFORE the
            // tracking session: menu tracking runs a nested run loop that can
            // still deliver winit user events, and a `Wake::Output` strip refresh
            // landing mid-track would `borrow_mut` this same cell in
            // `set_chrome_ext` — a held read borrow here would panic the app.
            let entries = self.ivars().menu_entries.borrow().clone();
            if entries.is_empty() {
                return;
            }
            // Snapshot the clicked tab's STABLE identity NOW, at pop time — the
            // same mid-track refreshes that can rewrite `menu_entries` can also
            // re-stamp `tab_id` (a `move_tab` re-labels positions in place), and
            // the eventual `tabMenuAction:` click must dispatch against the tab
            // this menu was popped ON, not whatever later drifted into the slot.
            self.ivars().menu_tab.set(Some(self.ivars().tab_id.get()));
            // Same pop-time capture for the CONNECTION rows: each row's item
            // tag indexes THIS snapshot (in encounter order, matching the
            // running counter in the build loop below), so a click after a
            // mid-track model rewrite still names the peer the human saw.
            *self.ivars().menu_conn.borrow_mut() = entries
                .iter()
                .filter_map(|e| match e {
                    TabMenuEntry::ConnectionAction { peer_sid, verb, .. } => {
                        Some((peer_sid.clone(), *verb))
                    }
                    _ => None,
                })
                .collect();
            let mut conn_index: isize = 0;
            // SAFETY: `+[NSMenu new]` is `+(id)` and returns a +1 the `Obj`
            // adopts; the menu is released when this frame ends, after the
            // synchronous tracking session `popUpContextMenu:` runs.
            let Some(menu) =
                (unsafe { Obj::from_owned(appkit::send_id(class(c"NSMenu").as_id(), sel!(new))) })
            else {
                return;
            };
            // SAFETY: plain main-thread NSMenu/NSMenuItem construction + setters on
            // fresh instances (the `initWithTitle:action:keyEquivalent:` initializer
            // and `separatorItem` factory are the same non-raising calls `menu.rs`
            // uses); `setTarget:` retains nothing (AppKit menu targets are WEAK —
            // the strip's `handle.tabs` retain normally keeps this view alive
            // through the tracking session, and if a mid-track count-changing
            // REBUILD drops that retain, the zeroed weak target makes the click a
            // clean no-op: the popped menu's facts died with the strip, and a
            // no-op is exactly what the stale-identity contract demands).
            unsafe {
                let Some(empty) = appkit::nsstring("") else {
                    return;
                };
                // `-initWithTitle:action:keyEquivalent:` is
                // `-(id)(NSString *, SEL, NSString *)`, consuming the +1 from
                // `+alloc`; a NULL `SEL` is the documented "no action", which is
                // what an informational header row wants.
                let item_with = |title: &str, action: aterm_objc::Sel| -> Option<Obj> {
                    let ns = appkit::nsstring(title)?;
                    Obj::from_owned(appkit::send_id_id_sel_id(
                        appkit::alloc(class(c"NSMenuItem")),
                        sel!(initWithTitle:action:keyEquivalent:),
                        ns.id(),
                        action,
                        empty.id(),
                    ))
                };
                appkit::send_v_bool(menu.id(), sel!(setAutoenablesItems:), false);
                for entry in entries.iter() {
                    match entry {
                        TabMenuEntry::Header(text) => {
                            let Some(item) = item_with(text, aterm_objc::Sel::NULL) else {
                                continue;
                            };
                            appkit::send_v_bool(item.id(), sel!(setEnabled:), false);
                            appkit::send_v_id(menu.id(), sel!(addItem:), item.id());
                        }
                        TabMenuEntry::Separator => {
                            // `+separatorItem` is a FACTORY (+0); the menu
                            // retains what it is handed, so no retain here.
                            appkit::send_v_id(
                                menu.id(),
                                sel!(addItem:),
                                appkit::send_id(class(c"NSMenuItem").as_id(), sel!(separatorItem)),
                            );
                        }
                        TabMenuEntry::Action {
                            label,
                            action,
                            enabled,
                        } => {
                            let Some(item) = item_with(label, sel!(tabMenuAction:)) else {
                                continue;
                            };
                            appkit::send_v_isize(item.id(), sel!(setTag:), action.tag());
                            appkit::send_v_bool(item.id(), sel!(setEnabled:), *enabled);
                            appkit::send_v_id(item.id(), sel!(setTarget:), self.as_id());
                            appkit::send_v_id(menu.id(), sel!(addItem:), item.id());
                        }
                        TabMenuEntry::ConnectionAction { label, enabled, .. } => {
                            // Tag = index into the pop-time `menu_conn`
                            // snapshot filled above; the running counter walks
                            // the SAME filtered order, so the two agree by
                            // construction.
                            let Some(item) = item_with(label, sel!(tabMenuConnection:)) else {
                                continue;
                            };
                            appkit::send_v_isize(item.id(), sel!(setTag:), conn_index);
                            conn_index += 1;
                            appkit::send_v_bool(item.id(), sel!(setEnabled:), *enabled);
                            appkit::send_v_id(item.id(), sel!(setTarget:), self.as_id());
                            appkit::send_v_id(menu.id(), sel!(addItem:), item.id());
                        }
                    }
                }
                // Pops synchronously at the event's location and runs the nested
                // menu tracking session; `entries` is an owned clone (see above),
                // so a mid-track strip refresh can freely rewrite the ivar model
                // — this popped menu deliberately keeps the snapshot it opened
                // with (a menu that mutates under the pointer is worse UX than a
                // one-frame-old menu).
                appkit::send_v_id_id_id(
                    class(c"NSMenu").as_id(),
                    sel!(popUpContextMenu:withEvent:forView:),
                    menu.id(),
                    event,
                    self.as_id(),
                );
            }
        }

        /// This tab's context-menu introspection line for `read_tab_menus`.
        fn menu_line(&self, index: usize) -> String {
            crate::session_chrome::tab_menu_chrome_line(index, &self.ivars().menu_entries.borrow())
        }

        /// Diff canonical icon/status/close metadata in place. Geometry changes only
        /// when a formerly unknown icon becomes known (or vice versa); title changes
        /// and dirty saves never rebuild the view or move its full select/drag bounds.
        fn set_metadata(&self, metadata: TabStripMetadata) {
            let ivars = self.ivars();
            let old_icon = ivars.icon.replace(metadata.icon);
            let old_dirty = ivars.dirty.replace(metadata.dirty);
            let old_busy = ivars.busy.replace(metadata.busy);
            let old_attention = ivars.attention.replace(metadata.attention);
            let old_conn = ivars.conn.replace(metadata.conn);
            let old_closable = ivars.closable.replace(metadata.closable);
            let old_drop = ivars.drop_target.replace(metadata.drop_target);
            let ornaments_changed = old_icon != metadata.icon
                || old_dirty != metadata.dirty
                || old_busy != metadata.busy
                || old_attention != metadata.attention
                || old_conn != metadata.conn
                || old_closable != metadata.closable;
            if !ornaments_changed {
                // The drag drop-target highlight (§3.2) moves NO geometry —
                // it is an accent wash over the existing pill — so a
                // flag-only flip is a plain repaint, never a relayout.
                if old_drop != metadata.drop_target {
                    self.mark_dirty();
                }
                return;
            }
            // Ornament slots (and, in the solo band, the whole centred group) move when
            // an icon or a status appears or disappears; `relayout` is the one place
            // that decides where.
            self.relayout();
            self.sync_semantics();
        }

        /// Re-resolve this tab's label ink against the CURRENT selected-tab color
        /// override without changing its active state — the repaint half of
        /// [`set_active_tab_color`], so a live config edit recolors the strip
        /// in place (no tab rebuild, no focus change). A SOLO band has no selected
        /// surface to tint, so its title keeps the primary label ink.
        fn refresh_label_ink(&self) {
            let ivars = self.ivars();
            if ivars.solo.get() {
                return;
            }
            if let Some(lbl) = ivars.label.borrow().as_ref() {
                // SAFETY: main-thread `setTextColor:` on the live retained label.
                unsafe {
                    let color = if ivars.active.get() {
                        active_label_color()
                    } else {
                        ns_color(sel!(secondaryLabelColor))
                    };
                    appkit::send_v_id(lbl.id(), sel!(setTextColor:), color);
                }
            }
        }

        /// Flip the ACTIVE flag IN PLACE (the diff path): recolour + re-weight the label
        /// and repaint the accent pill. No-op when unchanged.
        fn set_active(&self, active: bool) {
            if self.ivars().active.get() == active {
                return;
            }
            self.ivars().active.set(active);
            self.relayout();
            self.sync_semantics();
        }

        /// Keep hover text and native accessibility synchronized from canonical tab
        /// presentation. This is intentionally called by every context/state/selection
        /// diff path so VoiceOver can never lag the selected pixels.
        fn sync_semantics(&self) {
            let ivars = self.ivars();
            let metadata = self.metadata();
            let title = ivars.title.borrow();
            let tooltip = ivars.tooltip.borrow();
            let help = tab_help(&title, tooltip.as_deref(), metadata, ivars.index.get());
            // SAFETY: NSView implements the NSAccessibility setters on every supported
            // macOS version. Raw messages avoid coupling the default AccessKit build to
            // the mutually-exclusive `a11y-appkit` Cargo feature.
            unsafe {
                let v = self.as_id();
                let (Some(role), Some(label), Some(help), Some(identifier)) = (
                    appkit::nsstring("AXRadioButton"),
                    appkit::nsstring(&title),
                    appkit::nsstring(&help),
                    appkit::nsstring(&format!("aterm.tab.{}", ivars.index.get())),
                ) else {
                    return;
                };
                appkit::send_v_id(v, sel!(setToolTip:), help.id());
                appkit::send_v_bool(v, sel!(setAccessibilityElement:), true);
                appkit::send_v_id(v, sel!(setAccessibilityRole:), role.id());
                appkit::send_v_id(v, sel!(setAccessibilityLabel:), label.id());
                appkit::send_v_id(v, sel!(setAccessibilityHelp:), help.id());
                appkit::send_v_id(v, sel!(setAccessibilityIdentifier:), identifier.id());
                appkit::send_v_bool(v, sel!(setAccessibilitySelected:), ivars.active.get());
                appkit::send_v_bool(v, sel!(setAccessibilityEnabled:), true);
            }
        }

        /// Show the close ✕ only while the pointer is INSIDE this chip — macOS
        /// Terminal's rule, and the reason the strip reads as titles rather than as a
        /// row of buttons. The selected tab is no exception: a permanent ✕ on the tab
        /// you are looking at is the one that gets mis-clicked, and selection is
        /// already stated by the pill, the accent keyline, and the bolder ink.
        ///
        /// The ✕'s slot is reserved by [`native_tab_content_layout`] whether or not it
        /// is painted, so revealing it never reflows the title. Compact chips reserve
        /// no slot at all and spend those points on identity; a SOLO band has no ✕
        /// (the red traffic light closes the window). Close Tab remains available from
        /// the context menu, the File menu, and ⌘W at every size.
        fn refresh_close_visibility(&self) {
            let ivars = self.ivars();
            let show = ivars.close_available.get()
                && !ivars.solo.get()
                && ivars.closable.get()
                && ivars.hovered.get();
            if let Some(btn) = ivars.close_btn.borrow().as_ref() {
                // SAFETY: `-setHidden:` is a `-(void)(BOOL)` setter on the live
                // retained button, on the main thread.
                unsafe { appkit::send_v_bool(btn.id(), sel!(setHidden:), !show) };
            }
        }

        /// (Re)install a `mouseEnteredAndExited` tracking area covering the whole view
        /// (it follows resizes via `InVisibleRect`), removing any prior one.
        fn install_tracking_area(&self) {
            // SAFETY: remove the previous area, then build + add a fresh one covering the
            // current bounds — all standard AppKit calls on the main thread.
            // `-removeTrackingArea:`/`-addTrackingArea:` are `-(void)(id)`;
            // `-initWithRect:options:owner:userInfo:` is
            // `-(id)(NSRect, NSTrackingAreaOptions, id, NSDictionary *)` and
            // consumes the +1 from `+alloc`, which `Obj::from_owned` adopts. The
            // area's OWNER is this view and AppKit holds it weakly, which is
            // why the ivar retains it.
            unsafe {
                if let Some(old) = self.ivars().tracking.borrow_mut().take() {
                    appkit::send_v_id(self.as_id(), sel!(removeTrackingArea:), old.id());
                }
                let area = Obj::from_owned(appkit::send_id_rect_usize_id_id(
                    appkit::alloc(class(c"NSTrackingArea")),
                    sel!(initWithRect:options:owner:userInfo:),
                    appkit::send_rect(self.as_id(), sel!(bounds)),
                    consts::NS_TRACKING_HOVER_IN_VISIBLE_RECT,
                    self.as_id(),
                    Id::NIL,
                ));
                if let Some(area) = area {
                    appkit::send_v_id(self.as_id(), sel!(addTrackingArea:), area.id());
                    *self.ivars().tracking.borrow_mut() = Some(area);
                }
            }
        }

        /// This tab's canonical, undecorated title for cross-platform introspection.
        fn label_text(&self) -> String {
            self.ivars().title.borrow().clone()
        }

        fn metadata(&self) -> TabStripMetadata {
            let ivars = self.ivars();
            TabStripMetadata {
                icon: ivars.icon.get(),
                dirty: ivars.dirty.get(),
                busy: ivars.busy.get(),
                attention: ivars.attention.get(),
                conn: ivars.conn.get(),
                closable: ivars.closable.get(),
                drop_target: ivars.drop_target.get(),
            }
        }

        fn tooltip(&self) -> Option<String> {
            self.ivars().tooltip.borrow().clone()
        }

        /// Whether this tab is the active one (the selected segment, for introspection).
        fn is_active(&self) -> bool {
            self.ivars().active.get()
        }
    }

    /// The toolbar item identifier for the full-width tab-strip custom view.
    const STRIP_ITEM_ID: &str = "aterm.tabstrip";

    /// The delegate's ivars: the retained strip CONTAINER view (the toolbar item's
    /// custom view), wrapped into the item on demand.
    ///
    /// `Obj` — the first-party owning wrapper — rather than
    /// `objc2::rc::Retained<NSView>`, because the class that owns it is
    /// first-party. Since W7 ported this module's BINDINGS too, the whole file
    /// holds its AppKit objects this way and nothing crosses to `objc2`.
    pub(crate) struct DelegateIvars {
        container: Obj,
    }

    aterm_objc::declare_class! {
        /// The `NSToolbarDelegate`: vends the toolbar's single item identifier and
        /// builds ONE `NSToolbarItem` whose custom `view` IS the full-width tab-strip
        /// container (per-tab views + "+"). `UnifiedCompact` then renders the whole
        /// titlebar+toolbar as a SINGLE compact row.
        ///
        /// Declared with [`aterm_objc::declare_class!`]. This is one of the two
        /// sites D1 BLOCKED — `toolbar:itemForItemIdentifier:willBeInsertedInto
        /// Toolbar:` is a THREE-argument selector, and until the encoding call
        /// emitted one semicolon and a comma list the whole class failed to
        /// compile with `error: no rules expected ';'`. It compiles, registers
        /// `"@@:@@B"` (`"@@:@@c"` on the x86_64 compat slice) and dispatches; the
        /// test below reads that string back out of the runtime.
        ///
        /// The three methods RETURN objects and are not named `new`/`alloc`/
        /// `copy`, so each owes its caller a +0 reference — D5's rule, spelled
        /// here as an explicit `autorelease` or as a `+arrayWith…` that is
        /// autoreleased already. objc2 spelled it `#[method_id(…)]` and did it
        /// for you.
        ///
        /// objc2's `type Mutability = MainThreadOnly` is carried by
        /// [`aterm_objc::Retained`]'s unconditional `!Send` plus the
        /// [`aterm_objc::MainThread`] this module's entry points take, the same
        /// token `status_item.rs` and `menu.rs` take.
        pub(crate) struct ToolbarDelegate: NSObject {
            const NAME: &str = "ATermToolbarDelegate";
            type Ivars = DelegateIvars;
            protocols: [NSObject, NSToolbarDelegate];

            /// The items shown by DEFAULT: just the tab-strip custom-view item.
            @sel(toolbarDefaultItemIdentifiers:)
            fn default_item_identifiers(&self, _toolbar: Id) -> Id {
                Self::item_identifiers()
            }

            /// The items the toolbar MAY contain: the same single set.
            @sel(toolbarAllowedItemIdentifiers:)
            fn allowed_item_identifiers(&self, _toolbar: Id) -> Id {
                Self::item_identifiers()
            }

            /// Build the `NSToolbarItem` for the strip identifier: an item whose custom
            /// `view` is the retained full-width strip container. Any other identifier
            /// yields nil.
            @sel(toolbar:itemForItemIdentifier:willBeInsertedIntoToolbar:)
            fn item_for_identifier(
                &self,
                _toolbar: Id,
                identifier: Id,
                _will_be_inserted: aterm_objc::Bool,
            ) -> Id {
                if identifier.is_null() {
                    return Id::NIL;
                }
                let Some(strip_id) = appkit::nsstring(STRIP_ITEM_ID) else {
                    return Id::NIL;
                };
                // SAFETY: `identifier` is the live NSString AppKit passed and
                // `-isEqualToString:` is a side-effect-free `-(BOOL)(NSString *)`
                // comparison.
                if !unsafe {
                    appkit::send_bool_id(identifier, sel!(isEqualToString:), strip_id.id())
                } {
                    return Id::NIL;
                }
                let ivars = self.ivars();
                // SAFETY: standard NSToolbarItem construction + plain setters on a
                // fresh instance, on the main thread. `-initWithItemIdentifier:` is
                // `-(id)(NSString *)` and consumes the +1 `+alloc`;
                // `-setLabel:`/`-setPaletteLabel:` are `-(void)(NSString *)`;
                // `-setBordered:` is `-(void)(BOOL)`; `-setView:` is
                // `-(void)(NSView *)` and the container view is retained in this
                // ivar (and in the handle), outliving the item;
                // `-setMinSize:`/`-setMaxSize:` are `-(void)(NSSize)`. The item is
                // built +1 and AUTORELEASED on the way out, because a method not
                // named new/alloc/copy owes its caller +0.
                unsafe {
                    let raw = appkit::send_id_id(
                        appkit::alloc(class(c"NSToolbarItem")),
                        sel!(initWithItemIdentifier:),
                        identifier,
                    );
                    let Some(item) = Obj::from_owned(raw) else {
                        return Id::NIL;
                    };
                    let Some(label) = appkit::nsstring("Tabs") else {
                        return Id::NIL;
                    };
                    appkit::send_v_id(item.id(), sel!(setLabel:), label.id());
                    appkit::send_v_id(item.id(), sel!(setPaletteLabel:), label.id());
                    // macOS 26's "Liquid Glass" toolbars wrap items in a rounded glass
                    // CAPSULE by default; on a full-width custom strip that reads as a
                    // big empty pill behind the chrome. `setBordered:NO` opts this
                    // item OUT of the bezel so the strip blends into the transparent
                    // titlebar (the tab chips + buttons draw their own affordances).
                    appkit::send_v_bool(item.id(), sel!(setBordered:), false);
                    appkit::send_v_id(item.id(), sel!(setView:), ivars.container.id());
                    // A plain custom NSView has NO intrinsic size, so without an
                    // explicit min/max the `UnifiedCompact` toolbar collapses it to
                    // zero width and overflows it behind a `»` chevron. Give it a
                    // generous span — a small minimum so it never vanishes, and a
                    // very large maximum so it stretches to fill the whole toolbar
                    // (the container's `WidthSizable` mask + `set_window_tabs`
                    // re-layout then size the tabs to the real width).
                    //
                    // `setMinSize:`/`setMaxSize:` are soft-deprecated in favour of
                    // Auto Layout constraints, but they are the SIMPLEST non-raising
                    // way to size a custom-view toolbar item full-width here (an Auto
                    // Layout width constraint that also stretches inside a toolbar
                    // item is markedly more code + two more AppKit features, for no
                    // user-visible gain). They are plain, crash-safe setters — and
                    // the `#[allow(deprecated)]` the objc2 form needed is gone with
                    // the typed binding that carried the attribute.
                    appkit::send_v_size(
                        item.id(),
                        sel!(setMinSize:),
                        aterm_objc::CGSize {
                            width: STRIP_MIN_WIDTH,
                            height: STRIP_HEIGHT,
                        },
                    );
                    appkit::send_v_size(
                        item.id(),
                        sel!(setMaxSize:),
                        aterm_objc::CGSize {
                            width: STRIP_MAX_WIDTH,
                            height: STRIP_HEIGHT,
                        },
                    );
                    item.autorelease()
                }
            }
        }
    }

    impl ToolbarDelegate {
        fn new(mtm: MainThread, container: Obj) -> Option<Retained<Self>> {
            Self::alloc_init(mtm, DelegateIvars { container })
        }

        /// `@[STRIP_ITEM_ID]`, +0.
        ///
        /// `+arrayWithObject:` returns an AUTORELEASED array, which is exactly
        /// the +0 a declared object-returning method owes its caller — no
        /// explicit `autorelease` needed, and none of the "+1 leaked once per
        /// call" that returning the owned array would be.
        fn item_identifiers() -> Id {
            let Some(strip) = appkit::nsstring(STRIP_ITEM_ID) else {
                return Id::NIL;
            };
            // SAFETY: `+arrayWithObject:` is `-(id)(id)` and RETAINS its element,
            // so the +1 `strip` may drop at the end of this frame.
            unsafe {
                appkit::send_id_id(
                    class(c"NSArray").as_id(),
                    sel!(arrayWithObject:),
                    strip.id(),
                )
            }
        }
    }

    /// Attach the native window chrome to `window`: a SINGLE compact Ghostty-style row
    /// — a full-width VIEW-BASED TAB STRIP (per-tab [`TabView`]s) and a trailing "+"
    /// New Tab [`ChromeButton`], hosted as ONE custom-view `NSToolbarItem` in a
    /// `UnifiedCompact` `NSToolbar`. The toolbar is ALWAYS VISIBLE and starts with just
    /// the pinned "+" (0 tab chips); the caller's first `App::sync_window` calls
    /// [`set_window_tabs`] to populate the first identity chip. (The titlebar "Update"
    /// capsule is RETIRED: the update affordance lives in the VERSION menu — see
    /// `crate::menu::update_version_menu`.)
    ///
    /// Best-effort: off the main thread or with no AppKit `NSWindow`, the chrome is
    /// simply not installed (`None`) — never a panic.
    pub fn install_window_toolbar(
        window: &winit::window::Window,
        proxy: &EventLoopProxy<Wake>,
        wid: WindowId,
    ) -> Option<ToolbarHandle> {
        let mtm = MainThread::new()?;

        let handle = window.window_handle().ok()?;
        let RawWindowHandle::AppKit(h) = handle.as_raw() else {
            return None;
        };
        // `ns_view` points at this window's live NSView, owned by winit for the
        // window's lifetime; we only borrow it — on the main thread — to read
        // its `window` and attach the toolbar.
        let view = Id::from_ptr(h.ns_view.as_ptr().cast());
        // SAFETY: `-window` is `-(id)` and `-frame` is `-(NSRect)`, both
        // side-effect-free main-thread getters on that live view / its window.
        let (ns_window, win_w) = unsafe {
            let w = appkit::send_id(view, sel!(window));
            if w.is_null() {
                return None;
            }
            (w, appkit::send_rect(w, sel!(frame)).size.width.max(200.0))
        };

        // The trailing "+", pinned to the RIGHT end of the strip — a [`ChromeButton`],
        // a custom view that draws a REAL button affordance (a rounded hover/press
        // highlight for the quiet "+" icon). Its frame is seeded here and re-pinned
        // precisely by [`layout_strip`]; `NSViewMinXMargin` keeps it right-anchored
        // between those re-layouts (e.g. a live resize before the next `sync_window`).

        // "+" New Tab — a quiet, bold "+" glyph that lights up on hover/press.
        let plus = ChromeButton::build(
            mtm,
            proxy.clone(),
            MenuAction::NewTab,
            false,
            "New Tab",
            rect(
                win_w - TRAILING_PAD - PLUS_WIDTH,
                0.0,
                PLUS_WIDTH,
                STRIP_HEIGHT,
            ),
        )?;
        // SAFETY: plain main-thread setters on the fresh view —
        // `-setToolTip:` is `-(void)(NSString *)` and
        // `-setAutoresizingMask:` is `-(void)(NSAutoresizingMaskOptions)`, an
        // NSUInteger bitmask.
        unsafe {
            appkit::send_v_id(
                plus.as_id(),
                sel!(setToolTip:),
                appkit::nsstring("New Tab")?.id(),
            );
            appkit::send_v_usize(
                plus.as_id(),
                sel!(setAutoresizingMask:),
                consts::NS_VIEW_MIN_X_MARGIN_MAX_Y_MARGIN,
            );
        }

        // The strip's container view, hosting the per-tab views (built later in
        // [`layout_strip`]) and the trailing "+". ALWAYS VISIBLE (the "+" keeps it
        // from ever being an empty capsule).
        // SAFETY: standard NSView construction + setters on a fresh instance, on the
        // main thread; `addSubview:` takes the live retained button.
        let container = unsafe {
            let v = Obj::from_owned(appkit::send_id_rect(
                appkit::alloc(class(c"NSView")),
                sel!(initWithFrame:),
                rect(0.0, 0.0, win_w, STRIP_HEIGHT),
            ))?;
            appkit::send_v_usize(
                v.id(),
                sel!(setAutoresizingMask:),
                consts::NS_VIEW_WIDTH_SIZABLE,
            );
            appkit::send_v_id(v.id(), sel!(addSubview:), plus.as_id());
            let role = appkit::nsstring("AXTabGroup")?;
            let label = appkit::nsstring("Tabs")?;
            appkit::send_v_bool(v.id(), sel!(setAccessibilityElement:), true);
            appkit::send_v_id(v.id(), sel!(setAccessibilityRole:), role.id());
            appkit::send_v_id(v.id(), sel!(setAccessibilityLabel:), label.id());
            v
        };

        // The delegate owns its own retain of the container; the handle keeps
        // the one built above.
        let delegate = ToolbarDelegate::new(mtm, container.clone_retained())?;

        // SAFETY: standard NSToolbar / NSWindow setup, all on the main thread.
        let toolbar = unsafe {
            let identifier = appkit::nsstring("aterm.toolbar")?;
            let toolbar = Obj::from_owned(appkit::send_id_id(
                appkit::alloc(class(c"NSToolbar")),
                sel!(initWithIdentifier:),
                identifier.id(),
            ))?;
            // `-setDelegate:` is `-(void)(id)`; the runtime requirement is
            // CONFORMANCE, and the declared class carries the protocol
            // (`protocols: [NSObject, NSToolbarDelegate]`) — asserted in the
            // test below rather than assumed. The toolbar holds its delegate
            // WEAKLY, which is why `ToolbarHandle` retains it.
            appkit::send_v_id(toolbar.id(), sel!(setDelegate:), delegate.as_id());
            appkit::send_v_bool(toolbar.id(), sel!(setAllowsUserCustomization:), false);
            appkit::send_v_usize(
                toolbar.id(),
                sel!(setDisplayMode:),
                consts::NS_TOOLBAR_DISPLAY_MODE_ICON_ONLY,
            );
            appkit::send_v_id(ns_window, sel!(setToolbar:), toolbar.id());
            appkit::send_v_isize(
                ns_window,
                sel!(setToolbarStyle:),
                consts::NS_WINDOW_TOOLBAR_STYLE_UNIFIED_COMPACT,
            );
            appkit::send_v_isize(
                ns_window,
                sel!(setTitleVisibility:),
                consts::NS_WINDOW_TITLE_HIDDEN,
            );
            appkit::send_v_bool(ns_window, sel!(setTitlebarAppearsTransparent:), true);
            // NOTE: the WINDOW appearance is deliberately NOT set here. It is owned by
            // the config `window_theme` logic (platform.rs `attach_window_chrome`) —
            // an unconditional darkAqua force here used to FIGHT it: whichever ran
            // last won, so a config hot-reload (re-applying `Auto`) flipped the
            // window to the OS appearance and the strip's semantic label colours
            // (`labelColor`/`secondaryLabelColor`) resolved to translucent BLACK over
            // the terminal-dark transparent titlebar — unreadable tabs whenever the
            // OS was in light mode. The strip's own appearance is instead pinned to
            // the TERMINAL THEME's darkness via [`set_strip_dark`] (its backdrop is
            // the theme-coloured titlebar, not the OS chrome), scoped to `container`
            // so traffic lights and the rest of the titlebar still follow
            // `window_theme`.
            // ALWAYS VISIBLE: the item always carries the "+", so a fresh single-tab
            // window still shows a clean, normal titlebar — never an empty capsule.
            // `set_window_tabs` only adds/removes the tab chips before the trailing
            // "+"; it never hides the toolbar. (The staged-update affordance lives in
            // the VERSION menu now — no Update capsule in the cluster.)
            appkit::send_v_bool(toolbar.id(), sel!(setVisible:), true);
            toolbar
        };

        Some(ToolbarHandle {
            _delegate: delegate,
            _toolbar: toolbar,
            container,
            proxy: proxy.clone(),
            window: wid,
            tabs: RefCell::new(Vec::new()),
            plus,
            // SAFETY: `view` is winit's live content `NSView`; `Obj::retain`
            // takes one +1 of a live object, released when the handle drops.
            content_view: unsafe { Obj::retain(view) }?,
            rename: RefCell::new(None),
        })
    }

    /// Pin the tab STRIP's appearance to the terminal THEME's darkness (`dark` =
    /// theme bg is dark), so every semantic colour in the strip subtree — the tab
    /// labels' `labelColor`/`secondaryLabelColor`, the close `✕`, the `+` icon —
    /// resolves against the strip's ACTUAL backdrop (the theme-coloured transparent
    /// titlebar) instead of the OS/window appearance. Without this, a dark terminal
    /// theme under a light OS appearance rendered the inactive labels as translucent
    /// BLACK on near-black. Scoped to the container view: traffic lights and the
    /// rest of the titlebar keep following config `window_theme`. Idempotent; called
    /// at toolbar install and again whenever the theme changes (config reload, OS
    /// light/dark flip on a split theme). AppKit invalidates the subtree on an
    /// effective-appearance change, so dynamic colours repaint without manual dirtying.
    /// The user's selected-tab color override (config `active_tab_color`),
    /// packed `0xFF_RR_GG_BB`; `0` = no override (today's translucent system
    /// pill — "Transparent white" in the Tab Color settings page). Process-wide
    /// like the strip appearance pin: every window's strip follows one config.
    /// Read at draw time by `TabView::drawRect`/label ink, so it must be plain
    /// atomic state (AppKit messages the views on the main thread only, but the
    /// setter runs before a draw cycle exists).
    static ACTIVE_TAB_COLOR: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

    /// The live override as `[r, g, b]`, if one is pinned.
    fn active_tab_color_override() -> Option<[u8; 3]> {
        let packed = ACTIVE_TAB_COLOR.load(std::sync::atomic::Ordering::Relaxed);
        (packed >> 24 == 0xFF).then_some({
            [
                ((packed >> 16) & 0xff) as u8,
                ((packed >> 8) & 0xff) as u8,
                (packed & 0xff) as u8,
            ]
        })
    }

    /// The ACTIVE tab's label ink for the current override: on a custom pill the
    /// semantic `labelColor` may land white-on-white (or black-on-black), so the
    /// ink flips black/white by the override's own luminance — the SAME
    /// `bg_is_light` classifier the in-grid strip uses, so the two renderers
    /// can never disagree about readability. No override ⇒ the semantic color.
    ///
    /// # Safety
    /// Main thread; the result is +0 and is handed straight to
    /// `-setTextColor:`, which retains it.
    unsafe fn active_label_color() -> Id {
        match active_tab_color_override() {
            // SAFETY: plain color constructors on the main thread.
            Some(rgb) => unsafe {
                if crate::tab_bar::bg_is_light(rgb) {
                    ns_color(sel!(blackColor))
                } else {
                    ns_color(sel!(whiteColor))
                }
            },
            // SAFETY: as above.
            None => unsafe { ns_color(sel!(labelColor)) },
        }
    }

    /// Pin (or clear) the selected-tab color override and repaint the live strip:
    /// every retained [`TabView`] re-resolves its label ink and redraws its pill.
    /// Idempotent; called wherever the theme/config pins are re-synced
    /// (`set_strip_dark`'s call sites) so a config edit applies live.
    pub fn set_active_tab_color(handle: &ToolbarHandle, color: Option<[u8; 3]>) {
        let packed = color.map_or(0, |c| {
            0xFF00_0000 | (u32::from(c[0]) << 16) | (u32::from(c[1]) << 8) | u32::from(c[2])
        });
        if ACTIVE_TAB_COLOR.swap(packed, std::sync::atomic::Ordering::Relaxed) == packed {
            return;
        }
        for tab in handle.tabs.borrow().iter() {
            tab.refresh_label_ink();
            // SAFETY: main-thread `setNeedsDisplay:` on live retained views.
            unsafe { appkit::send_v_bool(tab.as_id(), sel!(setNeedsDisplay:), true) };
        }
        // SAFETY: as above, on the retained container.
        unsafe { appkit::send_v_bool(handle.container.id(), sel!(setNeedsDisplay:), true) };
    }

    pub fn set_strip_dark(handle: &ToolbarHandle, dark: bool) {
        // SAFETY: `+[NSAppearance appearanceNamed:]` is `+(id)(NSAppearanceName)`
        // and returns a +0 built-in appearance (or nil) for a system name;
        // `appkit::appearance_name` reads AppKit's own `NSAppearanceName*`
        // global, the one framework constant in this tree that is a real symbol
        // rather than a header value. `-setAppearance:` (NSAppearanceCustomization,
        // NSView ≥ 10.14) is a plain `-(void)(NSAppearance *)` main-thread
        // setter on our retained container view, and it RETAINS.
        unsafe {
            let appearance = appkit::send_id_id(
                class(c"NSAppearance").as_id(),
                sel!(appearanceNamed:),
                appkit::appearance_name(dark),
            );
            if !appearance.is_null() {
                appkit::send_v_id(handle.container.id(), sel!(setAppearance:), appearance);
            }
        }
    }

    /// Re-sync the title-bar tab STRIP to the current app tab state: `titles` (one
    /// label per tab), `ids` (the canonical STABLE [`TabId`] per tab, paired by
    /// index — what the context menu captures at pop time so its action can be
    /// re-resolved after a mid-menu close/reorder) and the 0-based `active` index.
    /// Called from `App::refresh_window_tabs` after every tab open / close /
    /// switch / detach / migrate.
    ///
    /// ALWAYS-ON: the toolbar, every live identity chip, and the pinned trailing "+"
    /// are never hidden. On any non-empty tab set the per-tab [`TabView`]s are rebuilt
    /// as needed: the old set is removed as subviews + dropped, a fresh view is built per title (laid out
    /// left→right in the band from `STRIP_LEADING_PAD` up to the right-pinned "+"),
    /// with the `active` one accented.
    ///
    /// THE BAND IS SPENT, NOT RATIONED. Chips divide the whole band between the
    /// stoplights and the "+" into equal shares ([`native_tab_cells`]) — no maximum
    /// width, so a wide window buys longer titles rather than bare titlebar. And with
    /// exactly ONE tab there is nothing to switch between, so the lone chip drops its
    /// pill and its ✕ and becomes the window TITLE: the name, its description, centred
    /// on the WINDOW (macOS Terminal's one-tab titlebar).
    ///
    /// Every vertical position — chips, "+", ✕, icons, status dots — is derived from
    /// the traffic lights MEASURED on the live window ([`strip_metrics`]), so the strip
    /// sits on the stoplights' own centre line instead of on whatever row the toolbar
    /// happened to hand us.
    pub fn set_window_tabs(
        handle: &ToolbarHandle,
        titles: &[String],
        ids: &[TabId],
        metadata: &[TabStripMetadata],
        tooltips: &[Option<String>],
        ext: &[TabChromeExt],
        active: usize,
    ) {
        let Some(mtm) = MainThread::new() else {
            return;
        };
        let container = handle.container.id();
        let metadata_at = |i: usize| {
            metadata.get(i).copied().unwrap_or(TabStripMetadata {
                icon: None,
                dirty: false,
                busy: false,
                attention: false,
                conn: None,
                closable: true,
                drop_target: false,
            })
        };
        // A missing id entry (defensive — the caller always pairs `ids` with
        // `titles`) degrades to the NEVER-ALLOCATED id 0: the allocator starts
        // at 1, so a menu popped on such a chip dispatches a logged no-op —
        // honest "unknown tab", never a positional guess at someone else's tab.
        let id_at = |i: usize| ids.get(i).copied().unwrap_or(TabId::from_stored(0));
        // Missing extension entries degrade to the empty chrome (no tooltip, no
        // menu) — the exact pre-stage-2 behavior, mirroring `metadata_at`.
        let default_ext = TabChromeExt::default();
        let ext_at = |i: usize| ext.get(i).unwrap_or(&default_ext);
        let tooltip_at = |i: usize| tooltips.get(i).and_then(Option::as_deref);

        if titles.is_empty() {
            // A transient no-tab window retains only New Tab. Every real tab count,
            // including one, gets a visible identity chip.
            // SAFETY: `removeFromSuperview` is a main-thread mutator.
            {
                let mut tabs = handle.tabs.borrow_mut();
                for old in tabs.drain(..) {
                    unsafe { appkit::send_v(old.as_id(), sel!(removeFromSuperview)) };
                }
            }
            // An editor must not be left floating over an empty strip: with no
            // chips there is no tab to be editing, so this cancels.
            sync_rename_overlay(handle, &[]);
            return;
        }

        // Measure the live window ONCE per refresh: the stoplights' centre line (what
        // every vertical slot aligns to), their trailing edge (what the band must clear),
        // and the window's own centre (what a solo title centres on).
        // SAFETY: `container` is the live retained strip view; `-bounds` is a
        // side-effect-free `-(NSRect)` getter.
        let (metrics, strip_h) = unsafe {
            (
                strip_metrics(container),
                appkit::send_rect(container, sel!(bounds))
                    .size
                    .height
                    .max(STRIP_HEIGHT),
            )
        };

        // Compute the tab band: from the leading pad — never tucked under the traffic
        // lights — to the trailing right-pinned "+", so a chip never draws under either.
        let left = STRIP_LEADING_PAD.max(metrics.lights_right + LIGHTS_CLEARANCE);
        let cluster = TAB_GAP + PLUS_WIDTH + TRAILING_PAD;
        // SAFETY: `-frame` is a side-effect-free `-(NSRect)` getter.
        let total_w = unsafe { appkit::send_rect(container, sel!(frame)) }
            .size
            .width
            .max(left + cluster + 1.0);
        let band_w = (total_w - left - cluster).max(1.0);
        let n = titles.len();
        let active = active.min(n.saturating_sub(1));

        // Keep the "+" on the measured line too, and re-pin it to the live width — its
        // autoresizing mask only carries it between refreshes.
        // SAFETY: main-thread geometry setter on the retained live button.
        unsafe {
            appkit::send_v_rect(
                handle.plus.as_id(),
                sel!(setFrame:),
                rect(
                    total_w - TRAILING_PAD - PLUS_WIDTH,
                    0.0,
                    PLUS_WIDTH,
                    strip_h,
                ),
            );
        }
        handle.plus.set_center_y(metrics.center_y);

        let cells = native_tab_cells(band_w, n, active)
            .into_iter()
            .map(|(x, width)| (left + x, width))
            .collect::<Vec<_>>();
        // ONE tab is the window's title, not a switcher (see the fn doc).
        let geometry_at = |index: usize, cell_x: f64| TabGeometry {
            center_y: metrics.center_y,
            solo: n == 1,
            solo_center_x: metrics.window_center_x - cell_x,
            separator: TabGeometry::separates(index, active),
        };

        // DIFF PATH: same tab COUNT and same GEOMETRY (every live frame matches its
        // computed cell — i.e. the container width is unchanged) ⇒ update the existing
        // views IN PLACE instead of re-allocating the strip. This is the common case —
        // it runs on every title change (each shell prompt / OSC-2 write) and twice per
        // tab switch, touching only the labels + the two tabs whose active flag flipped.
        // The index ivar stays correct: the count is unchanged and the index is
        // positional, so a `move_tab` reorder just re-labels each position — which is
        // exactly why the STABLE id must be re-stamped alongside the label: the view
        // keeps its position, but the tab AT that position changed identity.
        {
            let tabs = handle.tabs.borrow();
            if tabs.len() == n
                && tabs.iter().zip(&cells).all(|(tab, &(cx, cw))| {
                    // SAFETY: `-frame` is a side-effect-free `-(NSRect)` getter
                    // on a live retained chip.
                    let f = unsafe { appkit::send_rect(tab.as_id(), sel!(frame)) };
                    f.origin.x == cx && f.size.width == cw
                })
            {
                for (i, (tab, title)) in tabs.iter().zip(titles).enumerate() {
                    // Geometry FIRST: a window drag between displays, a light/dark flip,
                    // or a full-screen toggle can move the stoplights without changing
                    // any cell width, and the content must follow them.
                    tab.set_geometry(geometry_at(i, cells[i].0));
                    tab.set_context(title, tooltip_at(i));
                    tab.set_tab_id(id_at(i));
                    tab.set_metadata(metadata_at(i));
                    tab.set_chrome_ext(ext_at(i));
                    tab.set_active(i == active);
                }
                drop(tabs);
                sync_rename_overlay(handle, ids);
                return;
            }
        }

        // REBUILD PATH (count or container width changed): remove the previous tab views
        // (drop our retained copies) and build fresh. The "+" button stays (added once
        // at install). SAFETY: `removeFromSuperview` is a main-thread mutator.
        {
            let mut tabs = handle.tabs.borrow_mut();
            for old in tabs.drain(..) {
                unsafe { appkit::send_v(old.as_id(), sel!(removeFromSuperview)) };
            }
        }
        let mut new_tabs: Vec<Retained<TabView>> = Vec::with_capacity(n);
        for (i, title) in titles.iter().enumerate() {
            let (cx, cw) = cells[i];
            // The cell spans the FULL strip height so the whole chip is one
            // select/reorder target; only its CONTENT rides the measured centre line.
            let frame = rect(cx, 0.0, cw, strip_h);
            let tab = TabView::build(
                mtm,
                handle.proxy.clone(),
                handle.window,
                id_at(i),
                i,
                n,
                title,
                tooltip_at(i),
                i == active,
                metadata_at(i),
                ext_at(i),
                frame,
                geometry_at(i, cx),
            );
            // `+alloc`/`-initWithFrame:` can answer nil; a chip that cannot be
            // built is skipped rather than panicking mid-strip-rebuild, which
            // is the same best-effort contract every other AppKit path here has.
            let Some(tab) = tab else { continue };
            // SAFETY: `addSubview:` on the live container, main thread.
            unsafe { appkit::send_v_id(container, sel!(addSubview:), tab.as_id()) };
            new_tabs.push(tab);
        }
        *handle.tabs.borrow_mut() = new_tabs;
        // The chips were just appended, i.e. ON TOP: a live editor is re-framed
        // over its (possibly moved) chip and re-raised above them here, or
        // cancelled if its tab is gone. Never removed — the field's own
        // `stringValue` is the only home of the in-progress text.
        sync_rename_overlay(handle, ids);
    }

    /// Read the title chrome's complete introspection line for the `chrome` verb, or
    /// `None` only when there are no tabs. It reports canonical undecorated titles,
    /// selection, independent states, and full tooltips from the live [`TabView`]s;
    /// the `+` remains a separate action rather than pretending to be a tab.
    pub fn read_tab_chrome(handle: &ToolbarHandle) -> Option<String> {
        MainThread::new()?;
        let tabs = handle.tabs.borrow();
        if tabs.is_empty() {
            return None;
        }
        let active = tabs.iter().position(|t| t.is_active()).unwrap_or(0);
        let labels = tabs.iter().map(|tab| tab.label_text()).collect::<Vec<_>>();
        let metadata = tabs.iter().map(|tab| tab.metadata()).collect::<Vec<_>>();
        let tooltips = tabs.iter().map(|tab| tab.tooltip()).collect::<Vec<_>>();
        format_tab_chrome(&labels, &metadata, &tooltips, active)
    }

    /// Read the per-tab CONTEXT-MENU introspection lines for the `chrome` verb —
    /// `tab-menu tab=<i> items=[...]`, one per live [`TabView`], read off the
    /// SAME stored models a right-click pops (`TabIvars::menu_entries`) and
    /// serialised by the one pure `session_chrome::tab_menu_chrome_line`, so the
    /// listing IS the on-screen menu. Empty only when there are no tab chips;
    /// a single title-identity chip remains inspectable like [`read_tab_chrome`].
    #[must_use]
    pub fn read_tab_menus(handle: &ToolbarHandle) -> Vec<String> {
        if MainThread::new().is_none() {
            return Vec::new();
        }
        let tabs = handle.tabs.borrow();
        if !super::tab_menu_introspection_visible(tabs.len()) {
            return Vec::new();
        }
        tabs.iter()
            .enumerate()
            .map(|(i, t)| t.menu_line(i))
            .collect()
    }

    /// RETIRED: the titlebar "Update" capsule is gone — the owner asked for the update
    /// affordance to live in the VERSION menu (one-click apply; see
    /// `crate::menu::update_version_menu`), which also killed the old
    /// tooltip-says-install-but-click-opened-details mismatch. A documented no-op
    /// (mirroring the off-macOS stub) so the cross-platform
    /// `Apprt::set_toolbar_update_available` seam — owned by concurrent work in
    /// `platform.rs` — keeps one uniform signature; remove them together.
    pub fn set_update_available(_handle: &ToolbarHandle, _available: bool) {}

    /// The mutable state the rename relay needs at callback time.
    pub(crate) struct RenameIvars {
        /// The `Wake` channel every outcome relays through.
        proxy: EventLoopProxy<Wake>,
        /// The window whose strip owns the editor.
        window: WindowId,
        /// The session being renamed — captured by `App` at begin from the tab's
        /// FOCUSED pane, and carried verbatim so the commit `App` finally applies
        /// is validated against the identity the edit started on.
        session: u64,
        /// Byte cap for the edited field, enforced LIVE (see `controlTextDidChange:`).
        cap: usize,
        /// Escape was pressed — the next end-of-editing is a CANCEL, not a commit.
        cancelled: Cell<bool>,
        /// This editor has already reported its outcome. AppKit can end editing more
        /// than once around a teardown (abort, resign, remove-from-superview), and the
        /// strip also ends it deliberately when the edited tab disappears; the latch
        /// makes "exactly one Commit or Cancel per editor" structural.
        done: Cell<bool>,
    }

    aterm_objc::declare_class! {
        /// The inline rename field's delegate: a pure relay from AppKit's text-field
        /// editing callbacks into the typed `Wake` channel, in the shape of
        /// `MenuTarget`. It owns no rename policy — `App` resolves the session,
        /// validates, and writes.
        ///
        /// Declared with [`aterm_objc::declare_class!`]. This is the SECOND site
        /// D1 blocked: `control:textView:doCommandBySelector:` is three
        /// arguments, one of them a `SEL`, which is also why `Sel` had to become
        /// its own type with its own `":"` encoding rather than share
        /// `*const c_void`'s. The registered string is `"B@:@@:"` on arm64
        /// (`"c@:@@:"` on the compat slice) and the test below reads it back.
        ///
        /// THE PROTOCOL IS CLAIMED, AND THE REASON IS THE ROW ABOVE. This read
        /// `protocols: [NSObject]` with a note calling
        /// `NSControlTextEditingDelegate` an "INFORMAL protocol here" — and
        /// that was a measurement, stated as fact, that the runtime disproves:
        /// scanning every loaded protocol for these three selectors finds
        /// `NSControlTextEditingDelegate` (and `NSTextFieldDelegate`, which
        /// inherits it, plus six siblings) declaring all three FORMALLY —
        /// `controlTextDidChange:` and `controlTextDidEndEditing:` as
        /// `v24@0:8@16`, and `control:textView:doCommandBySelector:` as
        /// `B40@0:8@16@24:32`. They are *optional* members of a formal
        /// protocol, which is what makes AppKit's `respondsToSelector:`
        /// dispatch true; "informal protocol" means a category on `NSObject`
        /// with no protocol object at all, and that is not this.
        ///
        /// The distinction is not vocabulary. It is the D1 finding's own shape:
        /// authority for a registered encoding comes from what the class
        /// CLAIMS, so a class that implements three delegate rows while
        /// claiming only `NSObject` leaves them with NO authority — including
        /// the three-argument `SEL`-bearing row that IS D1. Claiming
        /// `NSTextFieldDelegate` (the exact parameter type of
        /// `-[NSTextField setDelegate:]`, which `begin_tab_rename` calls raw)
        /// is one name, and it makes all three rows checkable against the
        /// framework instead of against a literal this file writes itself —
        /// see `the_rename_rows_encode_to_the_protocol_authority` below.
        pub(crate) struct TabRenameTarget: NSObject {
            const NAME: &str = "ATermTabRenameTarget";
            type Ivars = RenameIvars;
            protocols: [NSObject, NSTextFieldDelegate];

            /// `controlTextDidChange:` — canonicalize as the user types.
            ///
            /// The field is held in the SAME representation the store accepts:
            /// forbidden formatting (control/bidi/invisible) is dropped and the
            /// value is clamped to the field's byte cap on a GRAPHEME boundary. The
            /// wire refuses those inputs instead, because a script must learn its
            /// label was rejected; a human typing gets the ordinary macOS
            /// length-limited-field behaviour — the field simply stops taking more,
            /// which IS the visible refusal. The consequence is that what the user
            /// sees is exactly what gets stored, with no rejection dialog and no
            /// divergence between the stored value and the recorded event.
            @sel(controlTextDidChange:)
            fn control_text_did_change(&self, notification: Id) {
                // SAFETY: `-object` is `-(id)` on the live NSNotification AppKit
                // passed; the sender of this notification is always the edited
                // NSControl, and `-stringValue` / `-setStringValue:` are that
                // control's plain main-thread value accessors.
                unsafe {
                    let field = appkit::send_id(notification, sel!(object));
                    if field.is_null() {
                        return;
                    }
                    let current =
                        appkit::nsstring_to_rust(appkit::send_id(field, sel!(stringValue)));
                    let canonical = crate::session_timeline::sanitize_presentation_line(
                        &current,
                        self.ivars().cap,
                    );
                    // Compare before writing: `setStringValue:` while editing resets
                    // the insertion point, so it must run ONLY when something real
                    // changed. The canonical form is trimmed, so compare against the
                    // trimmed input — otherwise a space typed mid-word would yank the
                    // caret. Edge whitespace is trimmed at commit anyway.
                    if current.trim() != canonical
                        && let Some(ns) = appkit::nsstring(&canonical)
                    {
                        appkit::send_v_id(field, sel!(setStringValue:), ns.id());
                    }
                }
            }

            /// `control:textView:doCommandBySelector:` — ESCAPE is the only key this
            /// intercepts. It latches a cancel and aborts the edit, which restores the
            /// field's original value and ends editing; the single exit below then
            /// reports the cancel. Every other command (Return, Tab, motion, deletion)
            /// is left to the field editor, so IME composition, undo, selection and
            /// the emoji picker behave exactly as in any native field.
            ///
            /// Escape during an IME composition never reaches here — the input method
            /// consumes it to cancel the composition, which is correct: the FIRST
            /// Escape ends the composition, the second cancels the rename.
            @sel(control:textView:doCommandBySelector:)
            fn control_text_view_do_command(
                &self,
                control: Id,
                _text_view: Id,
                command: aterm_objc::Sel,
            ) -> aterm_objc::Bool {
                if command != sel!(cancelOperation:) {
                    return aterm_objc::Bool::NO;
                }
                let ivars = self.ivars();
                ivars.cancelled.set(true);
                // Drive the cancel from HERE rather than through
                // `controlTextDidEndEditing:`. `abortEditing` nils the field
                // editor's delegate before it ends editing, so it does NOT
                // deliver that notification (verified against AppKit, not
                // assumed) — relying on it left the dead field painted over the
                // chip, the edit state set forever, and the window's first
                // responder dropped to the NSWindow, which silently kills every
                // keystroke in that window. Latch `done` so a late notification
                // from any other path is ignored, then let `App`'s cancel run
                // the ONE teardown that also restores the responder.
                if !ivars.done.replace(true) {
                    let _ = ivars.proxy.send_event(Wake::CancelSessionRename {
                        window: ivars.window,
                        session: ivars.session,
                    });
                }
                let _ = control;
                aterm_objc::Bool::YES
            }

            /// `controlTextDidEndEditing:` — the COMMIT exit. Return, Tab and a
            /// click away all commit (the Finder/Xcode convention: leaving a field
            /// keeps what you typed). Escape does NOT arrive here: `abortEditing`
            /// suppresses this notification, so the cancel is posted directly by
            /// the selector handler above and this guard's `done` latch only has
            /// to make a late duplicate harmless. `App` re-validates
            /// the session before writing, so a late notification cannot land on the
            /// wrong session even though this fires from inside AppKit's teardown.
            @sel(controlTextDidEndEditing:)
            fn control_text_did_end_editing(&self, notification: Id) {
                if self.ivars().done.replace(true) {
                    return;
                }
                let ivars = self.ivars();
                if ivars.cancelled.get() {
                    let _ = ivars.proxy.send_event(Wake::CancelSessionRename {
                        window: ivars.window,
                        session: ivars.session,
                    });
                    return;
                }
                // SAFETY: `-object` is `-(id)` on the live NSNotification; the
                // sender is the edited NSControl, whose `-stringValue` is a
                // plain `-(NSString *)` getter.
                let text = unsafe {
                    let field = appkit::send_id(notification, sel!(object));
                    appkit::nsstring_to_rust(if field.is_null() {
                        Id::NIL
                    } else {
                        appkit::send_id(field, sel!(stringValue))
                    })
                };
                let _ = ivars.proxy.send_event(Wake::CommitSessionRename {
                    window: ivars.window,
                    session: ivars.session,
                    text,
                });
            }
        }
    }

    impl TabRenameTarget {
        fn build(
            mtm: MainThread,
            proxy: EventLoopProxy<Wake>,
            window: WindowId,
            session: u64,
            cap: usize,
        ) -> Option<Retained<Self>> {
            Self::alloc_init(
                mtm,
                RenameIvars {
                    proxy,
                    window,
                    session,
                    cap,
                    cancelled: Cell::new(false),
                    done: Cell::new(false),
                },
            )
        }
    }

    /// Open the INLINE SESSION-RENAME editor over the chip carrying `tab`.
    ///
    /// The field is a sibling of the chips (a direct subview of the strip
    /// container), because chips are torn down and rebuilt by ordinary background
    /// events — see [`ToolbarHandle::rename`]. `seed` is the CURRENT PIN, not the
    /// chip's text: the chip shows a composed, ⌘-hint-decorated label, and
    /// seeding from it would let the first Return pin an OSC-derived string as if
    /// the user had typed it. `placeholder` is the label the ladder falls back to,
    /// so an empty field visibly says what clearing the pin will show.
    ///
    /// Returns whether an editor is on screen; `App` refuses to hold edit state
    /// nothing is presenting (an invisible modal mode that swallows commands).
    /// Whether a native editor COULD be installed here — the strip needs a live
    /// window to host the field. Deliberately side-effect free, so menu
    /// validation can ask without opening an editor.
    pub fn can_present_tab_rename(handle: &ToolbarHandle) -> bool {
        // SAFETY: `-window` is a side-effect-free `-(id)` getter on the live
        // retained container view.
        MainThread::new().is_some()
            && !unsafe { appkit::send_id(handle.container.id(), sel!(window)) }.is_null()
    }

    pub fn begin_tab_rename(
        handle: &ToolbarHandle,
        tab: TabId,
        session: u64,
        seed: &str,
        placeholder: &str,
    ) -> bool {
        let Some(mtm) = MainThread::new() else {
            return false;
        };
        // Replacing a live editor: end the old one first so AppKit never holds two
        // field editors and the abandoned relay cannot post a late outcome.
        end_tab_rename(handle);
        let frame = {
            let tabs = handle.tabs.borrow();
            let Some(chip) = tabs.iter().find(|t| t.tab_id() == tab) else {
                return false;
            };
            chip.editor_frame()
        };
        // `+alloc`/`-init` can answer nil; `begin_tab_rename` reports whether an
        // editor is on screen, so a failure here is `false`, not a panic.
        let Some(target) = TabRenameTarget::build(
            mtm,
            handle.proxy.clone(),
            handle.window,
            session,
            crate::session_timeline::MetaField::Title.cap(),
        ) else {
            return false;
        };
        // SAFETY: `textFieldWithString:` is the documented non-raising factory; the
        // rest are plain setters on the fresh field, all on the main thread. The
        // delegate is installed with a raw `setDelegate:` because AppKit resolves
        // delegate callbacks by `respondsToSelector:` and the typed setter would
        // demand a formal `NSTextFieldDelegate` conformance for two optional
        // methods, one of which is not surfaced at this binding feature set.
        let Some(field) = (unsafe {
            // `+textFieldWithString:` is a FACTORY (+0), so this retains.
            let Some(seed_ns) = appkit::nsstring(seed) else {
                return false;
            };
            let Some(field) = Obj::retain(appkit::send_id_id(
                class(c"NSTextField").as_id(),
                sel!(textFieldWithString:),
                seed_ns.id(),
            )) else {
                return false;
            };
            appkit::send_v_rect(field.id(), sel!(setFrame:), frame);
            appkit::send_v_bool(field.id(), sel!(setEditable:), true);
            appkit::send_v_bool(field.id(), sel!(setSelectable:), true);
            appkit::send_v_bool(field.id(), sel!(setBezeled:), true);
            appkit::send_v_bool(field.id(), sel!(setDrawsBackground:), true);
            appkit::send_v_bool(field.id(), sel!(setUsesSingleLineMode:), true);
            appkit::send_v_isize(
                field.id(),
                sel!(setAlignment:),
                consts::NS_TEXT_ALIGNMENT_LEFT,
            );
            // Match the chip's own label metrics so the text does not jump size
            // between editing and committed states.
            appkit::send_v_id(field.id(), sel!(setFont:), system_font(12.0, false));
            if !placeholder.is_empty()
                && let Some(ns) = appkit::nsstring(placeholder)
            {
                appkit::send_v_id(field.id(), sel!(setPlaceholderString:), ns.id());
            }
            appkit::send_v_id_isize_id(
                handle.container.id(),
                sel!(addSubview:positioned:relativeTo:),
                field.id(),
                consts::NS_WINDOW_ABOVE,
                Id::NIL,
            );
            Some(field)
        }) else {
            return false;
        };
        // Key focus: AppKit installs the window's shared field editor with this
        // field as its owner. The FIELD's window is resolved from the container,
        // never assumed to be winit's — in native fullscreen the toolbar lives in
        // a separate auxiliary window.
        // SAFETY: `-window` is `-(id)`; `-makeFirstResponder:` is
        // `-(BOOL)(NSResponder *)` and an `NSTextField` IS an `NSResponder`.
        let focused = unsafe {
            let w = appkit::send_id(handle.container.id(), sel!(window));
            !w.is_null() && appkit::send_bool_id(w, sel!(makeFirstResponder:), field.id())
        };
        if !focused {
            // SAFETY: main-thread teardown of the field we just added.
            unsafe { appkit::send_v(field.id(), sel!(removeFromSuperview)) };
            return false;
        }
        // SAFETY: `-selectText:` is `-(void)(id)` on the main thread; selecting
        // all makes the first keystroke replace the old pin, as a rename should.
        unsafe { appkit::send_v_id(field.id(), sel!(selectText:), Id::NIL) };
        // THE RELAY IS ATTACHED LAST, AND THE ORDER IS THE WHOLE POINT.
        //
        // `-setDelegate:` used to run up in the construction block, before
        // `-makeFirstResponder:`. `-[NSTextField selectText:]` re-runs the
        // cell's `-selectWithFrame:…`, which ENDS the editing session
        // `makeFirstResponder:` had just begun — so AppKit delivered
        // `controlTextDidEndEditing:` from inside `begin_tab_rename`, on a
        // relay that was already listening. Measured, on a real window, by
        // `examples/objc_toolbar_drive.rs`:
        //
        //   PLANT before makeFirstResponder
        //   PLANT after makeFirstResponder focused=true
        //   PLANT endEditing done=false      <-- inside -selectText:
        //   PLANT after selectText
        //   EXPE early begin=true wakes=["CommitSessionRename(55,\"vim\")"]
        //
        // The consequences were BOTH exits of the rename, dead: the spurious
        // notification posted a commit of the seed the instant the field
        // opened, and it latched the relay's `done` flag, after which Return
        // (`controlTextDidEndEditing:`) and Escape
        // (`control:textView:doCommandBySelector:`) could never post an
        // outcome again — the driver's real key events answered `[]` for both.
        // With the attach moved here they answer
        // `["CommitSessionRename(77,"vim")"]` and `["CancelSessionRename(77)"]`.
        //
        // NOT a W7 regression: the driver, built against `origin/main`
        // (420d5fbb8) with its `objc2` toolbar, prints the identical spurious
        // commit. This is the AppKit setup order, and it predates the port.
        //
        // SAFETY: `-setDelegate:` is `-(void)(id)` and holds the delegate
        // WEAKLY, which is why `RenameEditor` retains it below.
        unsafe { appkit::send_v_id(field.id(), sel!(setDelegate:), target.as_id()) };
        *handle.rename.borrow_mut() = Some(RenameEditor { field, target, tab });
        true
    }

    /// Remove the inline rename editor and hand key focus back to the terminal.
    /// Idempotent. Latches the relay's `done` flag FIRST, so the end-of-editing
    /// AppKit fires while we dismantle the field cannot post a second outcome.
    pub fn end_tab_rename(handle: &ToolbarHandle) {
        let Some(editor) = handle.rename.borrow_mut().take() else {
            return;
        };
        editor.target.ivars().done.set(true);
        // Restore first responder BEFORE removing the field: removing the current
        // first responder leaves the window with none, and keys then go nowhere.
        // SAFETY: main-thread responder restore, then teardown;
        // `-setDelegate: nil` drops AppKit's weak reference before the relay's
        // last strong reference goes away here.
        unsafe {
            let window = appkit::send_id(handle.container.id(), sel!(window));
            if !window.is_null() {
                // Best-effort, exactly as before: a window that refuses the
                // responder is a teardown race, and the field is removed next
                // either way.
                let _ = appkit::send_bool_id(
                    window,
                    sel!(makeFirstResponder:),
                    handle.content_view.id(),
                );
            }
            appkit::send_v_id(editor.field.id(), sel!(setDelegate:), Id::NIL);
            appkit::send_v(editor.field.id(), sel!(removeFromSuperview));
        }
    }

    /// The live rename field's current text, or `None` when no editor is open.
    /// The field's `stringValue` is the ONE home of the in-progress text, so this
    /// is how a command that must run "outside" an open editor (⌘W, ⌘T, a split)
    /// keeps what the user typed instead of discarding it.
    pub fn rename_editor_text(handle: &ToolbarHandle) -> Option<String> {
        let rename = handle.rename.borrow();
        let editor = rename.as_ref()?;
        // SAFETY: `-stringValue` is a plain `-(NSString *)` main-thread value
        // getter on the live field, and the result is +0 — copied out here.
        Some(unsafe {
            appkit::nsstring_to_rust(appkit::send_id(editor.field.id(), sel!(stringValue)))
        })
    }

    /// Hand one editing command to the live rename field's field editor. macOS
    /// resolves a menu key equivalent BEFORE the first responder sees the key, so
    /// without this ⌘V would paste into the PTY behind an open editor.
    pub fn rename_editor_edit(
        handle: &ToolbarHandle,
        action: crate::platform::RenameEditorEdit,
    ) -> bool {
        use crate::platform::RenameEditorEdit;
        let rename = handle.rename.borrow();
        let Some(editor) = rename.as_ref() else {
            return false;
        };
        // SAFETY: `currentEditor` is a plain main-thread getter; the returned NSText
        // implements the standard editing actions, each taking a `sender`.
        unsafe {
            let text = appkit::send_id(editor.field.id(), sel!(currentEditor));
            if text.is_null() {
                return false;
            }
            // Each standard editing action is `-(void)(id sender)`, and a nil
            // sender is what a programmatic invocation passes.
            let action = match action {
                RenameEditorEdit::Copy => sel!(copy:),
                RenameEditorEdit::Paste => sel!(paste:),
                RenameEditorEdit::SelectAll => sel!(selectAll:),
            };
            appkit::send_v_id(text, action, Id::NIL);
        }
        true
    }

    /// Keep a live rename editor glued to its tab across a strip refresh.
    ///
    /// Called at the END of every [`set_window_tabs`] path, because ALL of them
    /// move or destroy chips: the empty path and the rebuild path drain every
    /// chip, and even the in-place diff path re-stamps identities after a
    /// reorder. `ids` is the refreshed tab order.
    ///
    /// * the edited tab is GONE ⇒ tear the editor down and post a CANCEL. Never a
    ///   commit: the user was naming something that no longer exists.
    /// * the edited tab is PRESENT ⇒ re-frame the field over its (possibly moved)
    ///   chip and re-raise it, because the rebuild path appends the fresh chips
    ///   ON TOP — an editor left underneath would still hold key focus while
    ///   being invisible.
    ///
    /// The field is never removed on the present path, so the in-progress text,
    /// the selection, any IME composition, and first responder are all untouched
    /// by a refresh.
    fn sync_rename_overlay(handle: &ToolbarHandle, ids: &[TabId]) {
        // Resolve everything under one short borrow, so the teardown branch below
        // is free to take `handle.rename` mutably.
        let outcome = {
            let rename = handle.rename.borrow();
            let Some(editor) = rename.as_ref() else {
                return;
            };
            let tabs = handle.tabs.borrow();
            ids.iter()
                .position(|id| *id == editor.tab)
                .and_then(|i| tabs.get(i))
                .map(|chip| chip.editor_frame())
                .ok_or(editor.target.ivars().session)
        };
        let frame = match outcome {
            Ok(frame) => frame,
            Err(session) => {
                let window = handle.window;
                end_tab_rename(handle);
                let _ = handle
                    .proxy
                    .send_event(Wake::CancelSessionRename { window, session });
                return;
            }
        };
        let rename = handle.rename.borrow();
        let Some(editor) = rename.as_ref() else {
            return;
        };
        // SAFETY: main-thread geometry setter + re-insert of a view already in this
        // container; `addSubview:positioned:relativeTo:` moves it to the top of the
        // subview order without disturbing first responder.
        unsafe {
            appkit::send_v_rect(editor.field.id(), sel!(setFrame:), frame);
            appkit::send_v_id_isize_id(
                handle.container.id(),
                sel!(addSubview:positioned:relativeTo:),
                editor.field.id(),
                consts::NS_WINDOW_ABOVE,
                Id::NIL,
            );
        }
    }

    /// THE RUNTIME EVIDENCE for this module's four declared classes.
    ///
    /// "It compiles" is the trap this campaign has already been caught by once
    /// — a Metal pipeline that built, drew zero pixels and reported success —
    /// so nothing here is satisfied by compilation. Every test asks the
    /// OBJECTIVE-C RUNTIME or FOUNDATION what was registered, and the
    /// behavioural ones dispatch through machinery that is not the code under
    /// test.
    ///
    /// # What can and cannot be driven from a libtest process
    ///
    /// `ToolbarDelegate`'s ivars are a retained view and nothing else, so a
    /// REAL instance can be built and its three real methods invoked. The other
    /// three classes own an `EventLoopProxy<Wake>`, which can only be minted
    /// from a live winit `EventLoop` — main-thread-only, once per process, and
    /// libtest runs every test on a spawned thread — so their BEHAVIOUR is
    /// proved on probe classes of the identical declared shape while their
    /// REGISTRATION and ENCODINGS are read off the real ones.
    #[cfg(test)]
    mod objc_tests {
        use std::sync::atomic::{AtomicIsize, AtomicUsize, Ordering};

        use aterm_objc::{
            Bool, ClassType, Encode, Id, Obj, Sel, autoreleasepool, class, class_name,
            method_types, msg, sel,
        };

        use super::{ChromeButton, STRIP_ITEM_ID, TabRenameTarget, TabView, ToolbarDelegate};
        use crate::appkit;

        /// `- (BOOL)…` at THIS target's `@encode(BOOL)`; written from the table
        /// rather than hard-coded so the expectation is right on both arches,
        /// with the arm64 literal pinned separately below.
        fn bool_enc() -> &'static str {
            Bool::ENCODING
        }

        /// A live, empty `NSView`, for the delegate's container ivar.
        fn a_view() -> Obj {
            // SAFETY: `+alloc`/`-init` on `NSView` gives a +1 zero-frame view;
            // `Obj::from_owned` adopts that one +1.
            unsafe { Obj::from_owned(appkit::send_id(appkit::alloc(class(c"NSView")), sel!(init))) }
                .expect("NSView")
        }

        /// All four classes register, under the names AppKit already knows them
        /// by, on the superclasses they had.
        #[test]
        fn all_four_declared_classes_register_as_they_did() {
            for (cls, name, superclass) in [
                (ChromeButton::class(), c"ATermChromeButton", c"NSView"),
                (TabView::class(), c"ATermTabView", c"NSView"),
                (
                    ToolbarDelegate::class(),
                    c"ATermToolbarDelegate",
                    c"NSObject",
                ),
                (
                    TabRenameTarget::class(),
                    c"ATermTabRenameTarget",
                    c"NSObject",
                ),
            ] {
                assert!(!cls.is_null());
                assert_eq!(class(name), cls, "objc_getClass disagrees for {name:?}");
                // SAFETY: `cls` is a class this module registered, so it and its
                // superclass are live, immortal class objects.
                unsafe {
                    assert_eq!(class_name(cls), name);
                    assert_eq!(class_name(aterm_objc::superclass_of(cls)), superclass);
                }
            }
        }

        /// `ToolbarDelegate` CONFORMS to `NSToolbarDelegate`.
        ///
        /// This is not decoration. The objc2 form wired the delegate with a
        /// typed `setDelegate:` taking `ProtocolObject<dyn NSToolbarDelegate>`,
        /// which only an objc2-declared class can be; the port sends
        /// `-setDelegate:` raw, and conformance is the property that made that
        /// substitution legitimate. If `class_addProtocol` had silently not
        /// run, the toolbar would come up EMPTY — no tab strip at all — and
        /// nothing else in this file would notice.
        #[test]
        fn the_toolbar_delegate_conforms_to_nstoolbardelegate() {
            let cls = ToolbarDelegate::class();
            let proto = aterm_objc::protocol(c"NSToolbarDelegate");
            assert!(!proto.is_null(), "AppKit is linked, so is its protocol");
            // SAFETY: `+conformsToProtocol:` and `+instancesRespondToSelector:`
            // are side-effect-free NSObject queries on a live class object.
            unsafe {
                assert!(
                    appkit::send_bool_id(
                        cls.as_id(),
                        sel!(conformsToProtocol:),
                        Id::from_ptr(proto.as_ptr()),
                    ),
                    "the declared class does not answer NSToolbarDelegate"
                );
                for s in [
                    sel!(toolbarDefaultItemIdentifiers:),
                    sel!(toolbarAllowedItemIdentifiers:),
                    sel!(toolbar:itemForItemIdentifier:willBeInsertedIntoToolbar:),
                ] {
                    assert!(appkit::send_bool_sel(
                        cls.as_id(),
                        sel!(instancesRespondToSelector:),
                        s
                    ));
                }
            }
        }

        /// THE RENAME ROWS, CHECKED AGAINST THE FRAMEWORK RATHER THAN AGAINST
        /// THIS FILE.
        ///
        /// `the_declared_encodings_are_what_the_runtime_holds` below is
        /// thirty-two rows of registered encoding against a LITERAL written
        /// here — which is the circularity D1 named: a table that checks a
        /// mirror it declares itself cannot notice that the mirror is wrong.
        /// For the three rename rows there is a real authority, and this test
        /// reads it: `NSTextFieldDelegate` (claimed above, and inheriting
        /// `NSControlTextEditingDelegate`) declares all three, and
        /// [`aterm_objc::protocol_method_types`] answers with the string the
        /// FRAMEWORK will call through — including `B` on the D1 row, whose
        /// eight-byte-vs-one-byte failure mode is silent on arm64.
        ///
        /// It is deliberately the same instrument
        /// `examples/objc_live_class_audit.rs` points at `vendor/winit`'s
        /// classes, aimed at this module's — which that example's five targets
        /// do not cover.
        #[test]
        fn the_rename_rows_encode_to_the_protocol_authority() {
            let cls = TabRenameTarget::class();
            let proto = aterm_objc::protocol(c"NSTextFieldDelegate");
            assert!(!proto.is_null(), "AppKit is linked, so is its protocol");
            // SAFETY: `+conformsToProtocol:` is a side-effect-free `NSObject`
            // query on a live class object.
            assert!(
                unsafe {
                    appkit::send_bool_id(
                        cls.as_id(),
                        sel!(conformsToProtocol:),
                        Id::from_ptr(proto.as_ptr()),
                    )
                },
                "the declared class does not answer NSTextFieldDelegate"
            );
            let mut checked = 0usize;
            for s in [
                sel!(controlTextDidChange:),
                sel!(controlTextDidEndEditing:),
                sel!(control:textView:doCommandBySelector:),
            ] {
                // SAFETY: `proto` is a live protocol object, `cls` a live class
                // this module registered, and both queries are read-only.
                let (want, got) = unsafe {
                    (
                        aterm_objc::protocol_method_types(proto, s, true),
                        method_types(cls, s),
                    )
                };
                let want = want
                    .as_deref()
                    .map(aterm_objc::strip_method_offsets)
                    .unwrap_or_else(|| panic!("{s:?} is not declared by NSTextFieldDelegate"));
                assert_eq!(got.as_deref(), Some(want.as_str()), "{s:?}");
                checked += 1;
            }
            assert_eq!(checked, 3, "every rename row was read from the authority");
        }

        /// THE ENCODINGS, all thirty-two of them, read back out of the runtime
        /// with `method_getTypeEncoding` and checked against the verified table.
        ///
        /// The `BOOL` rows are D3 and the `NSRect` row is D4: `drawRect:` takes
        /// a 32-byte struct by value, which is the shape that needs
        /// `objc_msgSend_stret` on the x86_64 compat slice.
        #[test]
        fn the_runtime_reports_every_encoding_the_table_says() {
            let b = bool_enc();
            let rect = "{CGRect={CGPoint=dd}{CGSize=dd}}";
            let cases: Vec<(aterm_objc::ClassPtr, Sel, String)> = vec![
                // --- ChromeButton, 9 methods + the generated dealloc ---
                (ChromeButton::class(), sel!(drawRect:), format!("v@:{rect}")),
                (ChromeButton::class(), sel!(mouseDown:), "v@:@".into()),
                (ChromeButton::class(), sel!(mouseDragged:), "v@:@".into()),
                (ChromeButton::class(), sel!(mouseUp:), "v@:@".into()),
                (ChromeButton::class(), sel!(mouseEntered:), "v@:@".into()),
                (ChromeButton::class(), sel!(mouseExited:), "v@:@".into()),
                (
                    ChromeButton::class(),
                    sel!(updateTrackingAreas),
                    "v@:".into(),
                ),
                (
                    ChromeButton::class(),
                    sel!(acceptsFirstMouse:),
                    format!("{b}@:@"),
                ),
                (
                    ChromeButton::class(),
                    sel!(accessibilityPerformPress),
                    format!("{b}@:"),
                ),
                (ChromeButton::class(), sel!(dealloc), "v@:".into()),
                // --- TabView, 13 methods + dealloc ---
                (TabView::class(), sel!(drawRect:), format!("v@:{rect}")),
                (TabView::class(), sel!(mouseDown:), "v@:@".into()),
                (TabView::class(), sel!(rightMouseDown:), "v@:@".into()),
                (TabView::class(), sel!(mouseDragged:), "v@:@".into()),
                (TabView::class(), sel!(mouseUp:), "v@:@".into()),
                (TabView::class(), sel!(mouseEntered:), "v@:@".into()),
                (TabView::class(), sel!(mouseExited:), "v@:@".into()),
                (TabView::class(), sel!(updateTrackingAreas), "v@:".into()),
                (
                    TabView::class(),
                    sel!(acceptsFirstMouse:),
                    format!("{b}@:@"),
                ),
                (
                    TabView::class(),
                    sel!(accessibilityPerformPress),
                    format!("{b}@:"),
                ),
                (TabView::class(), sel!(closeTab:), "v@:@".into()),
                (TabView::class(), sel!(tabMenuAction:), "v@:@".into()),
                (TabView::class(), sel!(tabMenuConnection:), "v@:@".into()),
                (TabView::class(), sel!(dealloc), "v@:".into()),
                // --- ToolbarDelegate, 3 methods + dealloc. The third is D1's
                //     three-argument selector, and its LAST argument is a BOOL.
                (
                    ToolbarDelegate::class(),
                    sel!(toolbarDefaultItemIdentifiers:),
                    "@@:@".into(),
                ),
                (
                    ToolbarDelegate::class(),
                    sel!(toolbarAllowedItemIdentifiers:),
                    "@@:@".into(),
                ),
                (
                    ToolbarDelegate::class(),
                    sel!(toolbar:itemForItemIdentifier:willBeInsertedIntoToolbar:),
                    format!("@@:@@{b}"),
                ),
                (ToolbarDelegate::class(), sel!(dealloc), "v@:".into()),
                // --- TabRenameTarget, 3 methods + dealloc. The middle one is
                //     D1's other three-argument selector, and it takes a SEL —
                //     the argument whose encoding is `":"` and which shared an
                //     impl with `void *` until `Sel` became its own type.
                (
                    TabRenameTarget::class(),
                    sel!(controlTextDidChange:),
                    "v@:@".into(),
                ),
                (
                    TabRenameTarget::class(),
                    sel!(control:textView:doCommandBySelector:),
                    format!("{b}@:@@:"),
                ),
                (
                    TabRenameTarget::class(),
                    sel!(controlTextDidEndEditing:),
                    "v@:@".into(),
                ),
                (TabRenameTarget::class(), sel!(dealloc), "v@:".into()),
            ];
            for (cls, s, want) in cases {
                // SAFETY: every `cls` is a live class this module registered.
                let got = unsafe { method_types(cls, s) };
                assert_eq!(
                    got.as_deref(),
                    Some(want.as_str()),
                    "{s:?} on {:?}",
                    // SAFETY: as above.
                    unsafe { class_name(cls) }
                );
            }
            // The arm64 literals, pinned so a `Bool::ENCODING` that went wrong
            // could not make the `format!`s above vacuous.
            #[cfg(target_arch = "aarch64")]
            {
                assert_eq!(b, "B");
                // SAFETY: live registered class.
                assert_eq!(
                    unsafe {
                        method_types(
                            ToolbarDelegate::class(),
                            sel!(toolbar:itemForItemIdentifier:willBeInsertedIntoToolbar:),
                        )
                    }
                    .as_deref(),
                    Some("@@:@@B")
                );
                // SAFETY: live registered class.
                assert_eq!(
                    unsafe {
                        method_types(
                            TabRenameTarget::class(),
                            sel!(control:textView:doCommandBySelector:),
                        )
                    }
                    .as_deref(),
                    Some("B@:@@:")
                );
            }
        }

        /// THE THREE-ARGUMENT SELECTOR, on the REAL class, driven by FOUNDATION.
        ///
        /// D1 is the finding that `declare_class!` could not express a method
        /// with more than one argument at all. This is the proof that it now
        /// can, end to end and on the shipped class rather than a probe:
        /// `NSInvocation` builds the call FROM THE REGISTERED ENCODING — it
        /// reads `numberOfArguments`, the argument types and the return type out
        /// of `NSMethodSignature` — so a wrong encoding fails here rather than
        /// in a user's toolbar.
        ///
        /// Both answers are checked: the strip identifier yields a real
        /// `NSToolbarItem` carrying the container view, and any other identifier
        /// yields nil. A method stuck at "always build one" would pass half of
        /// this.
        #[test]
        fn foundation_invokes_the_three_argument_toolbar_selector() {
            let container = a_view();
            let delegate =
                ToolbarDelegate::new(crate::appkit::test_witness(), container.clone_retained())
                    .expect("delegate");
            autoreleasepool(|_| {
                // SAFETY: every send is cast to the exact prototype named
                // beside it. `-methodSignatureForSelector:` is `-(id)(SEL)`;
                // `+invocationWithMethodSignature:` is `-(id)(id)`;
                // `-setSelector:` is `-(void)(SEL)`; `-setArgument:atIndex:` is
                // `-(void)(void *, NSInteger)` and Foundation COPIES the bytes
                // at the pointer; `-invokeWithTarget:` is `-(void)(id)`;
                // `-getReturnValue:` is `-(void)(void *)` and writes
                // `methodReturnLength` bytes, asserted to be pointer-sized.
                unsafe {
                    let selector = sel!(toolbar:itemForItemIdentifier:willBeInsertedIntoToolbar:);
                    let sig_for: unsafe extern "C" fn(Id, Sel, Sel) -> Id = msg();
                    let sig = sig_for(
                        delegate.as_id(),
                        sel!(methodSignatureForSelector:),
                        selector,
                    );
                    assert!(!sig.is_null(), "no signature for the declared method");
                    // Foundation's own reading of the registered arity: self,
                    // _cmd and THREE arguments.
                    assert_eq!(appkit::send_usize(sig, sel!(numberOfArguments)), 5);
                    assert_eq!(
                        appkit::send_usize(sig, sel!(methodReturnLength)),
                        size_of::<Id>()
                    );

                    let call = |ident: &str| -> Id {
                        let inv = appkit::send_id_id(
                            class(c"NSInvocation").as_id(),
                            sel!(invocationWithMethodSignature:),
                            sig,
                        );
                        assert!(!inv.is_null());
                        let set_sel: unsafe extern "C" fn(Id, Sel, Sel) = msg();
                        set_sel(inv, sel!(setSelector:), selector);
                        let set_arg: unsafe extern "C" fn(Id, Sel, *mut std::ffi::c_void, isize) =
                            msg();
                        // index 2 = the (nil) NSToolbar, 3 = the identifier,
                        // 4 = the BOOL.
                        let mut toolbar = Id::NIL;
                        set_arg(
                            inv,
                            sel!(setArgument:atIndex:),
                            std::ptr::from_mut(&mut toolbar).cast(),
                            2,
                        );
                        let ns = appkit::nsstring(ident).expect("NSString");
                        let mut ident_arg = ns.id();
                        set_arg(
                            inv,
                            sel!(setArgument:atIndex:),
                            std::ptr::from_mut(&mut ident_arg).cast(),
                            3,
                        );
                        let mut inserting = Bool::YES;
                        set_arg(
                            inv,
                            sel!(setArgument:atIndex:),
                            std::ptr::from_mut(&mut inserting).cast(),
                            4,
                        );
                        appkit::send_v_id(inv, sel!(invokeWithTarget:), delegate.as_id());
                        let mut out = Id::NIL;
                        let get_ret: unsafe extern "C" fn(Id, Sel, *mut std::ffi::c_void) = msg();
                        get_ret(
                            inv,
                            sel!(getReturnValue:),
                            std::ptr::from_mut(&mut out).cast(),
                        );
                        out
                    };

                    let item = call(STRIP_ITEM_ID);
                    assert!(
                        !item.is_null(),
                        "the strip identifier did not yield a toolbar item"
                    );
                    assert_eq!(
                        appkit::nsstring_to_rust(appkit::send_id(item, sel!(itemIdentifier))),
                        STRIP_ITEM_ID
                    );
                    assert_eq!(
                        appkit::nsstring_to_rust(appkit::send_id(item, sel!(label))),
                        "Tabs"
                    );
                    assert_eq!(
                        appkit::send_id(item, sel!(view)),
                        container.id(),
                        "the item is not carrying the delegate's container view"
                    );
                    assert!(
                        !appkit::send_bool(item, sel!(isBordered)),
                        "the Liquid Glass bezel was not opted out of"
                    );

                    assert!(
                        call("aterm.notthestrip").is_null(),
                        "an unknown identifier built an item anyway"
                    );
                }
            });
        }

        /// The two `NSArray`-returning methods answer `@[STRIP_ITEM_ID]`, +0.
        ///
        /// D5's rule — an object-returning method not named new/alloc/copy owes
        /// its caller a BORROWED reference — is what makes the pool here load
        /// bearing: the array is autoreleased, and reading it after the pool
        /// popped would be a use-after-free rather than a wrong answer.
        #[test]
        fn the_identifier_methods_vend_exactly_the_strip_item() {
            let delegate =
                ToolbarDelegate::new(crate::appkit::test_witness(), a_view()).expect("delegate");
            autoreleasepool(|_| {
                // SAFETY: `-performSelector:withObject:` is the runtime's own
                // dispatch, `-count` is `-(NSUInteger)` and
                // `-objectAtIndex:` is `-(id)(NSUInteger)` on the returned
                // NSArray, which is alive for this pool.
                unsafe {
                    for s in [
                        sel!(toolbarDefaultItemIdentifiers:),
                        sel!(toolbarAllowedItemIdentifiers:),
                    ] {
                        let perform: unsafe extern "C" fn(Id, Sel, Sel, Id) -> Id = msg();
                        let arr = perform(
                            delegate.as_id(),
                            sel!(performSelector:withObject:),
                            s,
                            Id::NIL,
                        );
                        assert!(!arr.is_null(), "{s:?} returned nil");
                        assert_eq!(appkit::send_usize(arr, sel!(count)), 1);
                        let first: unsafe extern "C" fn(Id, Sel, usize) -> Id = msg();
                        assert_eq!(
                            appkit::nsstring_to_rust(first(arr, sel!(objectAtIndex:), 0)),
                            STRIP_ITEM_ID
                        );
                    }
                }
            });
        }

        // ---- probes: the declared SHAPES whose real ivars need a live winit
        //      event loop, driven through the same runtime machinery. ----

        static ESCAPES: AtomicUsize = AtomicUsize::new(0);
        static FIRST_MOUSE: AtomicIsize = AtomicIsize::new(-1);

        aterm_objc::declare_class! {
            /// `TabRenameTarget`'s three-argument selector, verbatim.
            struct RenameProbe: NSObject {
                const NAME: &str = "ATermRenameProbe";
                type Ivars = ();
                protocols: [NSObject];

                @sel(control:textView:doCommandBySelector:)
                fn control_text_view_do_command(
                    &self,
                    _control: Id,
                    _text_view: Id,
                    command: Sel,
                ) -> Bool {
                    if command != sel!(cancelOperation:) {
                        return Bool::NO;
                    }
                    ESCAPES.fetch_add(1, Ordering::SeqCst);
                    Bool::YES
                }
            }
        }

        aterm_objc::declare_class! {
            /// `ChromeButton`/`TabView`'s `acceptsFirstMouse:` shape, on
            /// `NSView`, so the per-arch `BOOL` return is checked on a class
            /// with the same superclass as the real ones.
            struct FirstMouseProbe: NSView {
                const NAME: &str = "ATermFirstMouseProbe";
                type Ivars = ();
                protocols: [NSObject];

                @sel(acceptsFirstMouse:)
                fn accepts_first_mouse(&self, _event: Id) -> Bool {
                    FIRST_MOUSE.store(1, Ordering::SeqCst);
                    Bool::YES
                }
            }
        }

        /// The `SEL`-taking three-argument selector really discriminates on the
        /// selector it is handed — `":"`-encoded, invoked by Foundation.
        ///
        /// A `Sel` argument that had kept sharing `*const c_void`'s `"^v"`
        /// encoding would still be a pointer-sized word in the right register
        /// on a DIRECT send, so a compile-and-call test would pass. It is
        /// `NSInvocation` that reads the letter, which is why the invocation is
        /// the test.
        #[test]
        fn foundation_invokes_the_selector_taking_three_argument_method() {
            ESCAPES.store(0, Ordering::SeqCst);
            let probe = RenameProbe::alloc_init(crate::appkit::test_witness(), ()).expect("probe");
            autoreleasepool(|_| {
                // SAFETY: as `foundation_invokes_the_three_argument_toolbar_selector`.
                unsafe {
                    let selector = sel!(control:textView:doCommandBySelector:);
                    let sig_for: unsafe extern "C" fn(Id, Sel, Sel) -> Id = msg();
                    let sig = sig_for(probe.as_id(), sel!(methodSignatureForSelector:), selector);
                    assert!(!sig.is_null());
                    assert_eq!(appkit::send_usize(sig, sel!(numberOfArguments)), 5);
                    assert_eq!(
                        appkit::send_usize(sig, sel!(methodReturnLength)),
                        size_of::<Bool>()
                    );

                    let call = |command: Sel| -> bool {
                        let inv = appkit::send_id_id(
                            class(c"NSInvocation").as_id(),
                            sel!(invocationWithMethodSignature:),
                            sig,
                        );
                        let set_sel: unsafe extern "C" fn(Id, Sel, Sel) = msg();
                        set_sel(inv, sel!(setSelector:), selector);
                        let set_arg: unsafe extern "C" fn(Id, Sel, *mut std::ffi::c_void, isize) =
                            msg();
                        let mut nil = Id::NIL;
                        for idx in [2, 3] {
                            set_arg(
                                inv,
                                sel!(setArgument:atIndex:),
                                std::ptr::from_mut(&mut nil).cast(),
                                idx,
                            );
                        }
                        let mut cmd = command;
                        set_arg(
                            inv,
                            sel!(setArgument:atIndex:),
                            std::ptr::from_mut(&mut cmd).cast(),
                            4,
                        );
                        appkit::send_v_id(inv, sel!(invokeWithTarget:), probe.as_id());
                        let mut out = Bool::NO;
                        let get_ret: unsafe extern "C" fn(Id, Sel, *mut std::ffi::c_void) = msg();
                        get_ret(
                            inv,
                            sel!(getReturnValue:),
                            std::ptr::from_mut(&mut out).cast(),
                        );
                        out.as_bool()
                    };

                    assert!(call(sel!(cancelOperation:)), "Escape was not consumed");
                    assert!(
                        !call(sel!(insertNewline:)),
                        "Return was consumed — every other command must pass \
                         through to the field editor"
                    );
                    assert!(!call(sel!(moveLeft:)));
                }
            });
            assert_eq!(
                ESCAPES.load(Ordering::SeqCst),
                1,
                "the body ran for the wrong set of commands"
            );
        }

        /// `acceptsFirstMouse:` on an `NSView` subclass answers YES through the
        /// runtime, and Foundation reads the return at `BOOL` width.
        #[test]
        fn an_nsview_subclass_answers_accepts_first_mouse() {
            FIRST_MOUSE.store(-1, Ordering::SeqCst);
            // `+alloc`/`-initWithFrame:`, the two-step `alloc_ivars` exists for.
            // SAFETY: `alloc_ivars` returns a +1 instance owing exactly one
            // designated initializer; `-initWithFrame:` is `-(id)(NSRect)`.
            let probe: aterm_objc::Retained<FirstMouseProbe> = unsafe {
                let raw = FirstMouseProbe::alloc_ivars(crate::appkit::test_witness(), ());
                assert!(!raw.is_null());
                aterm_objc::Retained::from_owned(appkit::send_id_rect(
                    raw,
                    sel!(initWithFrame:),
                    aterm_objc::CGRect {
                        origin: aterm_objc::CGPoint { x: 0.0, y: 0.0 },
                        size: aterm_objc::CGSize {
                            width: 10.0,
                            height: 10.0,
                        },
                    },
                ))
                .expect("initWithFrame:")
            };
            // SAFETY: `cls` is live; `-performSelector:withObject:` is the
            // runtime's own dispatch of the declared IMP.
            unsafe {
                assert_eq!(
                    method_types(FirstMouseProbe::class(), sel!(acceptsFirstMouse:)).as_deref(),
                    Some(format!("{}@:@", Bool::ENCODING).as_str())
                );
                assert_eq!(
                    class_name(aterm_objc::superclass_of(FirstMouseProbe::class())),
                    c"NSView"
                );
                let perform: unsafe extern "C" fn(Id, Sel, Sel, Id) -> Id = msg();
                perform(
                    probe.as_id(),
                    sel!(performSelector:withObject:),
                    sel!(acceptsFirstMouse:),
                    Id::NIL,
                );
            }
            assert_eq!(FIRST_MOUSE.load(Ordering::SeqCst), 1);
        }

        /// The generated `-dealloc` drops the Rust ivars, on the shape the four
        /// real classes have: an owning value with a `Drop`.
        #[test]
        fn dropping_a_declared_instance_drops_its_ivars() {
            static DROPS: AtomicUsize = AtomicUsize::new(0);
            struct Spy;
            impl Drop for Spy {
                fn drop(&mut self) {
                    DROPS.fetch_add(1, Ordering::SeqCst);
                }
            }
            aterm_objc::declare_class! {
                struct ToolbarDropProbe: NSObject {
                    const NAME: &str = "ATermToolbarDropProbe";
                    type Ivars = Spy;

                    @sel(ping)
                    fn ping(&self) {}
                }
            }
            DROPS.store(0, Ordering::SeqCst);
            let t =
                ToolbarDropProbe::alloc_init(crate::appkit::test_witness(), Spy).expect("probe");
            assert_eq!(DROPS.load(Ordering::SeqCst), 0);
            drop(t);
            assert_eq!(
                DROPS.load(Ordering::SeqCst),
                1,
                "the generated -dealloc did not drop the ivars"
            );
        }

        /// AN NSVIEW SUBCLASS DOES NOT DEALLOC WHEN ITS LAST REFERENCE DROPS ON
        /// A NON-MAIN THREAD — MEASURED, and this test exists to pin the
        /// measurement rather than to bless it.
        ///
        /// The identical probe on `NSObject` (the test above) drops its ivars at
        /// the `drop`. On `NSView` — and on `NSResponder`, so it is the
        /// responder chain and not views specifically — it does not, with
        /// `-retainCount` reading 1 immediately before the release and the
        /// enclosing `autoreleasepool` popping afterwards. AppKit defers
        /// responder deallocation; a libtest thread has no run loop to deliver
        /// the deferred free to, so nothing observes it here.
        ///
        /// Why this is recorded rather than hidden:
        ///
        /// * It is NOT a defect the port introduced. It is a property of the
        ///   superclass, and `objc2`'s `declare_class!` has it too — the objc2
        ///   form of `TabView` had no test that could have seen it either.
        /// * It is not a leak in the shipped app: every `ChromeButton` and
        ///   `TabView` is created and released on the main thread, under
        ///   AppKit's own run loop, which is where the deferred free lands.
        /// * It IS the one part of the ivar-drop contract this wave cannot
        ///   prove for its two `NSView` classes, and the winit port will declare
        ///   `NSView` and `NSWindow` subclasses whose ivars own more than these
        ///   do. It is the observable face of `aterm-objc`'s own UNCLOSED `S3`
        ///   ("dealloc runs on whatever thread performs the last release").
        ///
        /// If a future macOS makes this synchronous, this test fails and the
        /// next reader re-reads the paragraph above instead of inheriting it.
        #[test]
        fn an_nsview_subclass_defers_its_dealloc_off_the_main_thread() {
            static DROPS: AtomicUsize = AtomicUsize::new(0);
            struct Spy;
            impl Drop for Spy {
                fn drop(&mut self) {
                    DROPS.fetch_add(1, Ordering::SeqCst);
                }
            }
            aterm_objc::declare_class! {
                struct ViewDropProbe: NSView {
                    const NAME: &str = "ATermViewDropProbe";
                    type Ivars = Spy;

                    @sel(ping)
                    fn ping(&self) {}
                }
            }
            DROPS.store(0, Ordering::SeqCst);
            autoreleasepool(|_| {
                // SAFETY: `alloc_ivars` returns a +1 instance owing exactly one
                // designated initializer; `-initWithFrame:` is `-(id)(NSRect)`
                // and consumes it.
                let v: aterm_objc::Retained<ViewDropProbe> = unsafe {
                    let raw = ViewDropProbe::alloc_ivars(crate::appkit::test_witness(), Spy);
                    assert!(!raw.is_null());
                    aterm_objc::Retained::from_owned(appkit::send_id_rect(
                        raw,
                        sel!(initWithFrame:),
                        aterm_objc::CGRect {
                            origin: aterm_objc::CGPoint { x: 0.0, y: 0.0 },
                            size: aterm_objc::CGSize {
                                width: 4.0,
                                height: 4.0,
                            },
                        },
                    ))
                    .expect("initWithFrame:")
                };
                // The class really does carry its OWN generated `-dealloc` —
                // this is not "the method was never registered".
                // SAFETY: live registered class.
                assert_eq!(
                    unsafe { method_types(ViewDropProbe::class(), sel!(dealloc)) }.as_deref(),
                    Some("v@:")
                );
                // Sole owner: the release below is the last one.
                // SAFETY: `-retainCount` is `-(NSUInteger)`, side-effect free.
                assert_eq!(
                    unsafe { appkit::send_usize(v.as_id(), sel!(retainCount)) },
                    1
                );
                drop(v);
            });
            assert_eq!(
                DROPS.load(Ordering::SeqCst),
                0,
                "AppKit no longer defers responder deallocation — re-read this \
                 test's note and the `S3` entry in aterm-objc's crate docs"
            );
        }
    }
}
