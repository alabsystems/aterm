// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pure Settings compatibility model and retired card painter. Native Settings views
//! reuse [`SettingsState`] preference metadata/projections, while production pixels,
//! input, accessibility, and machine inspection compile from [`crate::native_ui`]. The
//! former modal painter remains unit-tested during compatibility retirement but can no
//! longer be installed as a shipping window overlay.

#![allow(
    dead_code,
    reason = "legacy SettingsState/painter remains the tested compatibility model while shipping Settings is a native tab"
)]

use aterm_core::terminal::RenderCell;
use aterm_render::Theme;

use crate::app_config::Config;
use crate::chrome_band;
use crate::prefs::{self, EditField, EditKind};
use crate::tray_raster::{row_baseline, ui_text_width};
use crate::type_scale::TypeStep;
use crate::widget::{DrawPrim, TextFace, TextWeight, TrayInput, rgba, text_prim};

/// Shared semantic state embedded in each native Settings tab. The native view owns
/// routing and input; the retired card painter also consumes this model in compatibility
/// tests until that scaffolding is removed.
pub(crate) struct SettingsState {
    /// Snapshot of the editable controls for the live config, in row order. Native
    /// Settings rebuilds it from its config snapshot after every persisted change, so
    /// the displayed value tracks the file. OWNED — [`EditField`] is moved in.
    pub(crate) fields: Vec<EditField>,
    /// Index of the highlighted row in `fields`.
    pub(crate) selected: usize,
    /// First visible control index (scroll offset) when `fields.len()` exceeds the body
    /// band of a short window.
    pub(crate) scroll: usize,
    /// Last save feedback for the footer line (mirrors [`crate::prefs::SaveOutcome`]).
    pub(crate) status: Option<String>,
    /// While the selected row is a FREE-FORM control (Text/Float/Integer) and the user
    /// pressed Enter, the live edit buffer; `None` otherwise. While `Some`, keystrokes
    /// edit this buffer instead of moving the selection, and the row renders it + a caret.
    /// A failed commit (bad number) keeps it `Some` so the user can fix the value.
    pub(crate) editing: Option<String>,
    /// The fuzzy-search query filtering the visible controls (empty ⇒ every control shows).
    /// Matched against each field's label, key, section name, and [`prefs::keywords_of`].
    pub(crate) query: String,
    /// Whether the search bar is FOCUSED: while true, typed keys edit `query` instead of
    /// navigating, `\u{21B5}`/`\u{2193}` drop focus into the (filtered) list, and Esc clears.
    pub(crate) searching: bool,
    /// Retired-card popup state for compatibility interaction and painter tests. Native
    /// Settings owns its choice-picker state separately.
    pub(crate) menu: Option<MenuState>,
    /// Retired-card colour-wheel state for compatibility interaction and painter tests.
    /// It remains mutually exclusive with [`Self::menu`]; native Settings owns its
    /// editor/picker state separately.
    pub(crate) wheel: Option<WheelState>,
    /// Retired preview-card demo phase. Compatibility tests fold it into
    /// [`Self::fingerprint`] to verify that a tick invalidates the pure painter.
    pub(crate) demo_phase: u32,
    /// The ACTIVE sidebar category (design §2.2): the content pane shows only this
    /// [`prefs::Section`]'s group-boxes. Changing it resets `scroll` (per-category
    /// scroll, macOS behavior) and snaps `selected` onto the category's first control.
    pub(crate) category: prefs::Section,
    /// Which PANE owns the keyboard (design §6): the sidebar (↑↓ move the category)
    /// or the content pane (↑↓ move the control selection). Tab toggles; ← never
    /// leaves content (adjust beats navigate — Tab is the pane switcher).
    pub(crate) pane: SettingsPane,
    /// Snapshot used by the retired collection-book painter, kept here so its
    /// compatibility render remains a pure function with no live application reads.
    pub(crate) kitty_log: Box<crate::kitty_log::KittyLogView>,
    /// Whether the LANDING page (design §L) is up INSTEAD of the two-pane panel:
    /// the ⌘, hero — mint ground, colour blotches, "aterm Settings", the
    /// Get-started bubble, and the suggestion box. Retained in the compatibility
    /// model; production Settings now renders from a native tab view.
    pub(crate) landing: bool,
    /// The landing page's suggestion-box buffer ("what should the next update
    /// be?"). ↵ with text opens the prefilled anonymous suggestion form in the
    /// default browser; the buffer never persists anywhere.
    pub(crate) comment: String,
    /// Animation phase for the landing page (blotch drift) and the kitty cameo
    /// (§L.4), bumped ~30fps by the SAME `next_demo_tick` lane the preview demo
    /// uses — folded into [`Self::fingerprint`] only while either is live, so
    /// an idle panel never re-rasterizes for it.
    pub(crate) landing_phase: u32,
    /// The in-flight kitty cameo (§L.4), `Some` while a summoned cat is on
    /// screen; expired by [`Self::tick_landing`] after [`KITTY_POP_TICKS`].
    pub(crate) kitty_pop: Option<KittyPop>,
    /// Shuffled-BAG state for the cameo breeds: every one of the
    /// [`KITTY_BREEDS`] appears within any window of that many summons (no
    /// unlucky streak can hide the rainbow), refilled + reshuffled when empty.
    pub(crate) kitty_bag: Vec<u8>,
    /// Xorshift seed for the bag shuffle and per-summon x placement — advanced
    /// only on SUMMON (input time), so the painter stays a clockless pure
    /// function of state.
    pub(crate) kitty_seed: u32,
    /// How many "kitty" occurrences the comment / query held after the last
    /// edit — a COUNT INCREASE summons a cat (deleting and retyping works;
    /// backspacing alone never does).
    pub(crate) comment_kitties: usize,
    pub(crate) query_kitties: usize,
    /// The ids of the loaded Trail Packs (`cursor_trail_packs`), so the
    /// `cursor_trail_style` picker lists a `pack:<id>` option per loaded pack
    /// (the dynamic twin of the static [`prefs::CURSOR_TRAIL_STYLES`], resolved
    /// like the Theme picker's `builtin_names()`). Sorted; empty when none are
    /// configured, so a pack-free config's picker is byte-identical.
    pub(crate) trail_pack_ids: Vec<String>,
}

/// The in-flight kitty cameo (§L.4): a small cat that pops out from behind the
/// landing suggestion box or above the sidebar search field when the user types
/// "kitty". Pure display state — it never touches the real Kitty Log.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct KittyPop {
    /// Breed index into the bag, `0..KITTY_BREEDS` (6 = the rainbow flyby).
    pub(crate) breed: u8,
    /// Horizontal placement, 0..1 across the host's band (ignored by the flyby,
    /// which travels the full band).
    pub(crate) x_frac: f32,
    /// `landing_phase` at summon time — the cameo's local clock zero.
    pub(crate) start: u32,
    /// Which field summoned it (placement anchor).
    pub(crate) host: KittyHost,
}

/// Where a kitty cameo pops: the landing suggestion box or the sidebar search.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum KittyHost {
    Landing,
    Sidebar,
}

/// Cameo breed count: white, orange, gray, black, calico, magic green, rainbow.
pub(crate) const KITTY_BREEDS: u8 = 7;
/// Cameo lifetime in demo ticks (~30fps ⇒ ≈2.6 s on screen).
pub(crate) const KITTY_POP_TICKS: u32 = 78;

/// The two keyboard-focus zones of the two-pane settings surface. While the search
/// filter is active the flat result list behaves as content regardless of `pane`
/// (the sidebar pill hollows and only re-anchors on clear).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SettingsPane {
    Sidebar,
    Content,
}

/// Retired-card render context kept for compatibility tests: facts the pure painter
/// cannot know on its own. It is threaded through the legacy
/// [`crate::overlay::OverlayModel::tray`] path; production native Settings receives
/// equivalent host facts through its renderer-native view context.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct PreviewCtx {
    /// Whether the OS appearance is currently dark — resolves the `window_theme=auto`
    /// titlebar mock truthfully (the system half of the split leads).
    pub(crate) system_dark: bool,
    /// The window's monitor DPI scale (`WindowState::scale`) — the About dialog sizes
    /// its text NATIVELY (fixed logical pt × this scale, like a real window's chrome)
    /// instead of tracking the terminal font.
    pub(crate) scale: f32,
    /// The configured `cursor_trail_color` override (packed `0x00RRGGBB`), if set
    /// and valid — the demo lane's base hue must honor it exactly like the live
    /// `glow_config` resolution, or the preview plays a different colour than the
    /// effect the user configured. `None` = the per-style default derivation.
    pub(crate) trail_color: Option<u32>,
    /// The configured `cursor_trail_accent` override (packed `0x00RRGGBB`), the
    /// `trail_color` twin. `None` = base brightened ~1.5× (the live default).
    pub(crate) trail_accent: Option<u32>,
}

impl Default for PreviewCtx {
    fn default() -> Self {
        // `scale` defaults to 1× (a plain display), NOT 0 — a zeroed scale would
        // collapse the About dialog's native text to nothing in tests.
        Self {
            system_dark: false,
            scale: 1.0,
            trail_color: None,
            trail_accent: None,
        }
    }
}

/// The open popup menu of a Theme / long-Enum row ([`uses_popup`]). `options` is the FULL
/// list shown: when the configured value is not in the canonical set (a user theme, a
/// `dark:…,light:…` split, or an unrecognized enum spelling) it is listed VERBATIM as
/// entry 0 and highlighted — so opening + Enter is a no-op and the custom value is never
/// silently replaced (see [`popup_options`]).
pub(crate) struct MenuState {
    /// Absolute index (into `SettingsState::fields`) of the anchor row.
    pub(crate) field: usize,
    /// Every selectable option, in menu order (entry 0 may be the raw custom value).
    pub(crate) options: Vec<String>,
    /// Index of the option in effect when the menu opened (gets the check marker;
    /// committing it is a no-op).
    pub(crate) current: usize,
    /// The highlighted option (moved by ↑/↓, clamped — no wrap).
    pub(crate) highlighted: usize,
    /// First visible option when the list overflows the popover (wheel / ↑↓ driven).
    pub(crate) scroll: usize,
}

/// Which sub-control of the colour-wheel popover owns the keyboard (Tab cycles
/// Wheel → Value → Hex, design §7).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum WheelFocus {
    Wheel,
    Value,
    Hex,
}

/// The mouse-drag target inside the wheel popover, `Some` between a press on the
/// disk/slider and its release: motion scrubs h/s (disk) or v (slider)
/// continuously. Transient input state — the h/s/v it writes are the painted
/// (fingerprinted) state, the drag flag itself is not.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum WheelDrag {
    Disk,
    Slider,
}

/// The open colour-wheel POPOVER of a Color row (design §7). `h`/`s`/`v` are the
/// WORKING (uncommitted) colour — hue in 0..1 turns (0 at 12 o'clock, clockwise,
/// the disk's polar convention), saturation as the marker's radius, value as the
/// slider; `hex` is the readout field's live text, kept in sync with the wheel
/// (and re-parsed INTO it while the hex field has focus). Scrubbing only repaints
/// (the preview shows the candidate); the file is untouched until ↵ commits.
pub(crate) struct WheelState {
    /// Absolute index (into `SettingsState::fields`) of the anchor Color row.
    pub(crate) field: usize,
    /// Hue, 0..1 turns.
    pub(crate) h: f32,
    /// Saturation, 0..1.
    pub(crate) s: f32,
    /// Value/brightness, 0..1.
    pub(crate) v: f32,
    /// The hex readout's live text (canonical uppercase `#RRGGBB` when wheel-driven).
    pub(crate) hex: String,
    /// Which sub-control the keyboard drives.
    pub(crate) focus: WheelFocus,
    /// The active mouse drag, if any (see [`WheelDrag`]).
    pub(crate) drag: Option<WheelDrag>,
}

impl SettingsState {
    /// Build the snapshot from the live config (call with `App.config`).
    pub(crate) fn from_config(cfg: &Config) -> Self {
        Self::from_config_with_trail_pack_ids(cfg, &[])
    }

    /// Build with the already-resolved catalog ids owned by the current config
    /// generation. This constructor is intentionally IO-free; callers must not
    /// turn semantic Settings view construction into a manifest loader.
    pub(crate) fn from_config_with_trail_pack_ids(cfg: &Config, trail_pack_ids: &[String]) -> Self {
        let mut s = Self {
            fields: prefs::editable_fields(cfg),
            selected: 0,
            scroll: 0,
            status: None,
            editing: None,
            query: String::new(),
            searching: false,
            menu: None,
            wheel: None,
            demo_phase: 0,
            category: prefs::Section::ORDER[0],
            pane: SettingsPane::Sidebar,
            // Empty until the App snapshots its in-memory log (sync on open).
            kitty_log: Box::default(),
            landing: false, // the WINDOWED open opts in (design §L)
            comment: String::new(),
            landing_phase: 0,
            kitty_pop: None,
            kitty_bag: Vec::new(),
            kitty_seed: 0x9E37_79B9, // any odd constant; advanced per summon
            comment_kitties: 0,
            query_kitties: 0,
            trail_pack_ids: trail_pack_ids.to_vec(),
        };
        // Selection starts on the active category's FIRST control in LAID-OUT
        // order (Theme leads Appearance) — the raw field vec is section-sorted
        // only, so index 0 is whichever row happened to build first.
        s.selected = category_controls(&s.fields, s.category)
            .first()
            .copied()
            .unwrap_or(0);
        s
    }

    /// Non-overlapping, case-insensitive count of "kitty" in `s` — the summon
    /// detector's corpus measure (§L.4).
    pub(crate) fn count_kitty(s: &str) -> usize {
        s.to_lowercase().matches("kitty").count()
    }

    /// Note a comment edit: `true` when a NEW "kitty" completion appeared (the
    /// occurrence count went UP), updating the high-water counter either way.
    pub(crate) fn note_kitty_in_comment(&mut self) -> bool {
        let n = Self::count_kitty(&self.comment);
        let fresh = n > self.comment_kitties;
        self.comment_kitties = n;
        fresh
    }

    /// [`Self::note_kitty_in_comment`] for the sidebar search query.
    pub(crate) fn note_kitty_in_query(&mut self) -> bool {
        let n = Self::count_kitty(&self.query);
        let fresh = n > self.query_kitties;
        self.query_kitties = n;
        fresh
    }

    /// Summon a kitty cameo over `host` (§L.4): draw the next breed from the
    /// shuffled bag (refill + Fisher–Yates when empty, xorshift-driven so the
    /// painter stays clockless) and place it at a bag-seeded x. A cameo already
    /// on screen is replaced — the newest summon wins the one cameo slot.
    pub(crate) fn summon_kitty(&mut self, host: KittyHost) {
        // xorshift32 — cheap, std-only, and deterministic per state; the
        // absorbing 0 never enters (`max(1)` on write-back).
        fn xs(seed: &mut u32) -> u32 {
            let mut x = *seed;
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            *seed = x.max(1);
            x
        }
        let mut seed = self.kitty_seed;
        if self.kitty_bag.is_empty() {
            self.kitty_bag = (0..KITTY_BREEDS).collect();
            // Fisher–Yates, bounded by the constant bag length.
            for i in (1..self.kitty_bag.len()).rev() {
                let j = xs(&mut seed) as usize % (i + 1);
                self.kitty_bag.swap(i, j);
            }
        }
        let breed = self.kitty_bag.pop().unwrap_or(0);
        let x_frac = 0.08 + (xs(&mut seed) % 1000) as f32 / 1000.0 * 0.84;
        self.kitty_seed = seed;
        self.kitty_pop = Some(KittyPop {
            breed,
            x_frac,
            start: self.landing_phase,
            host,
        });
    }

    /// One ~30fps animation tick for the landing page / kitty cameo lane:
    /// advance the phase and expire a finished cameo. Called from the SAME
    /// `next_demo_tick` fire the preview demo uses (`main.rs`), so the painter
    /// itself stays clockless.
    pub(crate) fn tick_landing(&mut self) {
        self.landing_phase = self.landing_phase.wrapping_add(1);
        if let Some(k) = &self.kitty_pop
            && self.landing_phase.wrapping_sub(k.start) > KITTY_POP_TICKS
        {
            self.kitty_pop = None;
        }
    }

    /// Whether the SEARCH FILTER owns the content pane: the search bar is focused or a
    /// query is active. While true the content shows the flat cross-category result
    /// list (the old `body_layout_masked` machinery); otherwise the active category's
    /// group-boxes. One predicate so painter/hit-test/keyboard can never disagree.
    pub(crate) fn filtering(&self) -> bool {
        self.searching || !self.query.trim().is_empty()
    }

    /// The field indices the content pane SHOWS, in painted order: the filtered set
    /// while searching, else the active category's controls in group-laid-out order.
    /// `controls_lines` serializes exactly this, keeping screen == introspection.
    pub(crate) fn shown_indices(&self) -> Vec<usize> {
        if self.filtering() {
            self.visible_indices()
        } else {
            category_controls(&self.fields, self.category)
        }
    }

    /// The absolute field indices currently VISIBLE under the search filter, in row order
    /// (all of them when the query is blank). The single source the painter, the mouse
    /// hit-test, the scroll math, and keyboard navigation share, so they never disagree.
    pub(crate) fn visible_indices(&self) -> Vec<usize> {
        match self.visible_mask() {
            None => (0..self.fields.len()).collect(),
            Some(mask) => (0..self.fields.len()).filter(|&i| mask[i]).collect(),
        }
    }

    /// A per-field visibility mask, or `None` when the query is blank (everything shows).
    pub(crate) fn visible_mask(&self) -> Option<Vec<bool>> {
        let q = self.query.trim().to_lowercase();
        if q.is_empty() {
            return None;
        }
        Some(
            (0..self.fields.len())
                .map(|i| self.matches_query(i, &q))
                .collect(),
        )
    }

    /// Whether control `idx` matches the (already-lowercased) query across its searchable
    /// corpus: label, config key, section name, and the intent keywords.
    fn matches_query(&self, idx: usize, q_lower: &str) -> bool {
        let Some(f) = self.fields.get(idx) else {
            return false;
        };
        f.label.to_lowercase().contains(q_lower)
            || f.key.to_lowercase().contains(q_lower)
            || prefs::section_of(f.key)
                .label()
                .to_lowercase()
                .contains(q_lower)
            || prefs::keywords_of(f.key)
                .iter()
                .any(|k| k.contains(q_lower))
    }

    /// Focus the search bar (subsequent keys edit the query). `scroll` re-zeroes
    /// because its unit changes with the mode: a flat visible-field index while
    /// filtering vs. a [`GroupRow`] index in grouped mode.
    pub(crate) fn search_begin(&mut self) {
        self.searching = true;
        self.status = None;
        self.scroll = 0;
    }

    /// Append to the query, keeping the selection on a still-visible row.
    pub(crate) fn search_push(&mut self, c: char) {
        self.query.push(c);
        self.snap_selection_visible();
    }

    /// Delete the last query char, keeping the selection visible.
    pub(crate) fn search_backspace(&mut self) {
        self.query.pop();
        self.snap_selection_visible();
    }

    /// Drop search focus but KEEP the filter (Enter/↓ from the search bar → the list).
    /// The flat result list is content, so the content pane takes keyboard focus.
    /// Confirming an EMPTIED query lands back in GROUPED mode, so it re-anchors
    /// exactly like [`Self::search_clear`]: the category follows the selection,
    /// `scroll` re-zeroes (its unit flips from flat field index to [`GroupRow`]
    /// index), and the selection snaps into the category — otherwise the stale
    /// flat scroll would blank the band and activation/reset would silently no-op.
    pub(crate) fn search_confirm(&mut self) {
        self.searching = false;
        self.pane = SettingsPane::Content;
        if !self.filtering() {
            if let Some(f) = self.fields.get(self.selected) {
                let sec = prefs::section_of(f.key);
                if sec != self.category {
                    self.category = sec;
                }
            }
            self.scroll = 0; // grouped-mode scroll unit; re-clamped by the caller's band
            self.snap_selection_category();
        }
    }

    /// Clear the filter and leave search (the single Esc level from search/filtered
    /// list). The category FOLLOWS the selection — Esc from a cross-category result
    /// lands in that control's own pane, not wherever the sidebar last pointed.
    pub(crate) fn search_clear(&mut self) {
        self.query.clear();
        self.searching = false;
        self.snap_selection_visible();
        if let Some(f) = self.fields.get(self.selected) {
            let sec = prefs::section_of(f.key);
            if sec != self.category {
                self.category = sec;
            }
        }
        self.scroll = 0; // grouped-mode scroll unit; re-clamped by the caller's band
        self.snap_selection_category();
    }

    /// After a query change, pull `selected` onto the first visible row if it fell out of
    /// the filtered set (so navigation + activation always target a shown control).
    fn snap_selection_visible(&mut self) {
        let vis = self.visible_indices();
        if !vis.contains(&self.selected) {
            self.selected = vis.first().copied().unwrap_or(0);
            self.scroll = 0;
        }
    }

    /// The control an action (activate / reset / edit) may target: the selected row,
    /// but ONLY when it is actually shown — under the search filter while filtering,
    /// inside the ACTIVE CATEGORY otherwise. When a query matches nothing,
    /// `snap_selection_visible` parks `selected` on a hidden row (index 0); returning
    /// `None` here makes the mutators no-op instead of silently persisting a change to
    /// a control the user cannot see (the body shows "No settings match").
    pub(crate) fn action_target(&self) -> Option<usize> {
        let shown = if self.filtering() {
            self.visible_indices().contains(&self.selected)
        } else {
            self.fields
                .get(self.selected)
                .is_some_and(|f| prefs::section_of(f.key) == self.category)
        };
        shown.then_some(self.selected)
    }

    /// ↑/↓ while the SIDEBAR pane is focused: move the active category by `delta`,
    /// CLAMPED at the ends (design §6: no wrap).
    pub(crate) fn sidebar_move(&mut self, delta: isize) {
        let cur = self.category.order_index() as isize;
        let last = prefs::Section::ORDER.len() as isize - 1;
        let next = cur.saturating_add(delta).clamp(0, last) as usize;
        self.set_category(prefs::Section::ORDER[next]);
    }

    /// Activate a sidebar category (keyboard move or mouse click): per-category scroll
    /// resets on a CHANGE only (re-clicking the active category keeps your place), and
    /// the selection snaps onto the category's first laid-out control so the content
    /// pane always has a live target.
    pub(crate) fn set_category(&mut self, sec: prefs::Section) {
        if self.category != sec {
            self.category = sec;
            self.scroll = 0;
        }
        self.status = None; // transient status clears on navigation (design §3.3)
        self.snap_selection_category();
    }

    /// Pull `selected` onto the active category's FIRST control (group-laid-out order)
    /// when it points outside the category — the invariant every grouped-mode painter/
    /// menu/preview path relies on.
    pub(crate) fn snap_selection_category(&mut self) {
        let controls = category_controls(&self.fields, self.category);
        if !controls.contains(&self.selected) {
            self.selected = controls.first().copied().unwrap_or(0);
        }
    }

    /// →/Tab/↵ from the sidebar: focus the content pane (selection already snapped).
    pub(crate) fn focus_content(&mut self) {
        self.pane = SettingsPane::Content;
        self.status = None; // transient status clears on navigation (design §3.3)
        // While the search filter is active the flat result list IS the content and
        // `category` is deliberately stale (it re-anchors only on clear/confirm) —
        // snapping would yank the selection onto the stale category's first control,
        // off the filtered list, leaving it highlight-less and action-dead.
        if !self.filtering() {
            self.snap_selection_category();
        }
    }

    /// Tab/⇧Tab/Esc from the content pane: focus the sidebar (selection stays put, so
    /// Tab-Tab round-trips to the same control).
    pub(crate) fn focus_sidebar(&mut self) {
        self.pane = SettingsPane::Sidebar;
        self.status = None; // transient status clears on navigation (design §3.3)
    }

    /// ↑/↓ over the ACTIVE CATEGORY's control rows (grouped mode): captions, footnotes,
    /// and gaps are skipped by construction (the walk is over control indices), clamped
    /// at the ends, keeping the selected row's 2-cell box inside the `band`-cell window.
    pub(crate) fn move_selection_grouped(&mut self, delta: isize, band: usize, wrap: usize) {
        self.status = None; // transient status clears on navigation (design §3.3)
        let controls = category_controls(&self.fields, self.category);
        if controls.is_empty() {
            return;
        }
        let pos = controls
            .iter()
            .position(|&i| i == self.selected)
            .unwrap_or(0);
        let new = (pos as isize + delta).clamp(0, controls.len() as isize - 1) as usize;
        self.selected = controls[new];
        self.clamp_group_scroll(band, wrap);
    }

    /// Keep `scroll` (a [`GroupRow`] index) so the SELECTED control's full 2-cell row
    /// sits inside the `band`-cell group window; also drags the group caption into view
    /// when the selection lands on a group's first control (macOS reveals the header).
    /// `wrap` is the shared footnote wrap width ([`footnote_wrap_chars`]).
    pub(crate) fn clamp_group_scroll(&mut self, band: usize, wrap: usize) {
        let rows = category_layout(&self.fields, self.category, wrap);
        self.scroll = self.scroll.min(rows.len().saturating_sub(1));
        let Some(sel) = rows
            .iter()
            .position(|r| matches!(r, GroupRow::Control(i) if *i == self.selected))
        else {
            return;
        };
        // Reveal the caption directly above a group-opening control.
        let target_top = if sel > 0 && matches!(rows[sel - 1], GroupRow::Caption(_)) {
            sel - 1
        } else {
            sel
        };
        if target_top < self.scroll {
            self.scroll = target_top;
        }
        while self.scroll < sel && !group_row_fully_visible(&rows, self.scroll, band, sel) {
            self.scroll += 1;
        }
    }

    /// Wheel-scroll the grouped content band by `delta` [`GroupRow`]s, clamped so the
    /// tail never over-scrolls past the band (mirrors `scroll_body` for flat mode).
    pub(crate) fn scroll_grouped(&mut self, delta: isize, band: usize, wrap: usize) {
        let rows = category_layout(&self.fields, self.category, wrap);
        let max = max_group_scroll(&rows, band);
        self.scroll = (self.scroll as isize + delta).clamp(0, max as isize) as usize;
    }

    /// Rebuild the control list from a freshly-loaded config while Settings is open,
    /// preserving the selection and re-clamping `scroll`
    /// PER MODE the way every gesture does: grouped `scroll` is a [`GroupRow`] index
    /// while `selected` stays an absolute field index, so the v1 `scroll.min(selected)`
    /// clamp compared incommensurable units and yanked the band after an in-panel save.
    pub(crate) fn rebuild_fields(&mut self, fields: Vec<EditField>, band: usize, wrap: usize) {
        self.fields = fields;
        self.selected = self.selected.min(self.fields.len().saturating_sub(1));
        if self.filtering() {
            self.clamp_scroll(band);
        } else {
            let rows = category_layout(&self.fields, self.category, wrap);
            self.scroll = self.scroll.min(max_group_scroll(&rows, band));
            self.clamp_group_scroll(band, wrap);
        }
    }

    /// Whether a control kind is FREE-FORM (edited via the in-panel text editor) rather
    /// than activated in place: Text/Float/Integer accept a typed value; a Bool /
    /// short Enum is cycled by [`cycle_edit`], a popup row ([`uses_popup`]) opens the
    /// anchored menu, and a Color row opens the colour wheel ([`Self::wheel_open`]) —
    /// its hex is typed inside the popover's readout field, not here.
    pub(crate) fn is_editable_kind(kind: EditKind) -> bool {
        matches!(kind, EditKind::Text | EditKind::Float | EditKind::Integer)
    }

    /// Begin editing the selected row IF it is a free-form control and not already being
    /// edited. Seeds the buffer with the CONFIGURED value (the seed, not the effective
    /// placeholder), so an unset key starts blank. Returns whether editing began.
    pub(crate) fn edit_begin(&mut self) -> bool {
        if self.editing.is_some() {
            return false;
        }
        // Never open the editor on a control filtered out of the visible set.
        let Some(idx) = self.action_target() else {
            return false;
        };
        let Some(f) = self.fields.get(idx) else {
            return false;
        };
        if !Self::is_editable_kind(f.kind) {
            return false;
        }
        self.editing = Some(f.seed.clone().unwrap_or_default());
        self.status = None;
        true
    }

    /// Append a typed character to the edit buffer (no-op when not editing).
    pub(crate) fn edit_push(&mut self, c: char) {
        if let Some(buf) = self.editing.as_mut() {
            buf.push(c);
        }
    }

    /// Delete the last character of the edit buffer (no-op when not editing/empty).
    pub(crate) fn edit_backspace(&mut self) {
        if let Some(buf) = self.editing.as_mut() {
            buf.pop();
        }
    }

    /// Abandon the in-progress edit, reverting to the displayed value (no-op when idle).
    pub(crate) fn edit_cancel(&mut self) {
        self.editing = None;
    }

    /// The (key, value) edit a commit would persist: the selected row's key, with the
    /// trimmed buffer as the value (blank ⇒ `None`, i.e. remove the key → revert to the
    /// built-in default). `None` when not editing. Does NOT mutate — the caller persists
    /// it and decides whether to clear `editing` (a rejected value stays in edit mode).
    pub(crate) fn edit_pending(&self) -> Option<(&'static str, Option<String>)> {
        let buf = self.editing.as_ref()?;
        let f = self.fields.get(self.selected)?;
        let trimmed = buf.trim();
        Some((f.key, (!trimmed.is_empty()).then(|| trimmed.to_string())))
    }

    /// Open the popup menu on the selected row IF it is a popup-chip row ([`uses_popup`])
    /// and no menu is already open. The current value is highlighted (a custom value is
    /// entry 0, per [`popup_options`]), so opening + Enter is a no-op. Returns whether the
    /// menu opened.
    pub(crate) fn menu_open(&mut self) -> bool {
        if self.menu.is_some() {
            return false;
        }
        // Never open a menu on a control filtered out of the visible set.
        let Some(idx) = self.action_target() else {
            return false;
        };
        let Some(f) = self.fields.get(idx) else {
            return false;
        };
        if !uses_popup(f) {
            return false;
        }
        // The `cursor_trail_style` dropdown lists the loaded Trail Packs too
        // (empty ids for every other row ⇒ byte-identical option list).
        let options = popup_options_with(f, &self.trail_pack_ids);
        if options.is_empty() {
            return false;
        }
        let current = popup_current_index(f, &options);
        // The two anchored popovers are mutually exclusive: opening the menu
        // discards any open colour wheel (nothing was persisted while it browsed).
        self.wheel = None;
        self.menu = Some(MenuState {
            field: idx,
            options,
            current,
            highlighted: current,
            scroll: 0,
        });
        self.status = None;
        true
    }

    /// Close the popup menu with no change (Esc / click-away). No-op when closed.
    pub(crate) fn menu_cancel(&mut self) {
        self.menu = None;
    }

    /// Move the menu highlight by `delta`, CLAMPED at the ends (no wrap), keeping it
    /// inside the `visible`-row popover window ([`menu_geom`] supplies `visible`).
    pub(crate) fn menu_move(&mut self, delta: isize, visible: usize) {
        let Some(m) = self.menu.as_mut() else { return };
        if m.options.is_empty() {
            return;
        }
        let last = m.options.len() as isize - 1;
        m.highlighted = (m.highlighted as isize + delta).clamp(0, last) as usize;
        Self::menu_snap_scroll(m, visible);
    }

    /// Jump the highlight to the NEXT option starting with `c` (case-insensitive),
    /// searching forward from the highlight and wrapping — the type-a-letter fast path.
    pub(crate) fn menu_jump(&mut self, c: char, visible: usize) {
        let Some(m) = self.menu.as_mut() else { return };
        let n = m.options.len();
        if n == 0 {
            return;
        }
        let lc = c.to_ascii_lowercase();
        let starts = |o: &str| {
            o.chars()
                .next()
                .is_some_and(|f| f.to_ascii_lowercase() == lc)
        };
        // Next match strictly after the highlight, wrapping past the end.
        if let Some(hit) = (1..=n)
            .map(|step| (m.highlighted + step) % n)
            .find(|&i| starts(&m.options[i]))
        {
            m.highlighted = hit;
            Self::menu_snap_scroll(m, visible);
        }
    }

    /// Scroll the open menu by `delta` options (mouse wheel), clamped to the list. Does
    /// NOT move the highlight (mirrors the body-scroll gesture).
    pub(crate) fn menu_scroll_by(&mut self, delta: isize, visible: usize) {
        let Some(m) = self.menu.as_mut() else { return };
        let max = m.options.len().saturating_sub(visible.max(1));
        m.scroll = (m.scroll as isize + delta).clamp(0, max as isize) as usize;
    }

    /// Keep `scroll` so the highlighted option stays inside the `visible`-row window.
    fn menu_snap_scroll(m: &mut MenuState, visible: usize) {
        let visible = visible.max(1);
        if m.highlighted < m.scroll {
            m.scroll = m.highlighted;
        } else if m.highlighted >= m.scroll + visible {
            m.scroll = m.highlighted + 1 - visible;
        }
        m.scroll = m.scroll.min(m.options.len().saturating_sub(visible));
    }

    /// The (key, value) edit committing the menu highlight would persist, or `None` when
    /// the menu is closed OR the highlight sits on the already-current entry (Enter on
    /// the current value — including a preserved custom value — is a pure no-op). Does
    /// NOT mutate; the caller closes the menu and persists through the shared seam.
    pub(crate) fn menu_pending(&self) -> Option<(&'static str, Option<String>)> {
        let m = self.menu.as_ref()?;
        if m.highlighted == m.current {
            return None;
        }
        let f = self.fields.get(m.field)?;
        Some((f.key, Some(m.options.get(m.highlighted)?.clone())))
    }

    /// Open the colour wheel on the selected row IF it is a Color row and no wheel is
    /// already open (↵/Space or a widget-region click — the route that replaced the
    /// free-text editor for Color rows). Seeds h/s/v + the hex readout from the row's
    /// EFFECTIVE hex; an unset key has no hex to parse, so the caller supplies the
    /// live theme's colour as `fallback` (the App reads the theme, not this model).
    /// Returns whether the wheel opened.
    pub(crate) fn wheel_open(&mut self, fallback: [u8; 3]) -> bool {
        if self.wheel.is_some() {
            return false;
        }
        // Never open the wheel on a control filtered out of the visible set.
        let Some(idx) = self.action_target() else {
            return false;
        };
        let Some(f) = self.fields.get(idx) else {
            return false;
        };
        if !matches!(f.kind, EditKind::Color) {
            return false;
        }
        let rgb = parse_hex(Self::display_value(f)).unwrap_or(fallback);
        let (h, s, v) = rgb_to_hsv(rgb);
        // Mutually exclusive popovers: the wheel evicts any open menu.
        self.menu = None;
        self.wheel = Some(WheelState {
            field: idx,
            h,
            s,
            v,
            hex: canonical_hex(rgb),
            focus: WheelFocus::Wheel,
            drag: None,
        });
        self.status = None;
        true
    }

    /// Close the colour wheel with NO change (Esc / click-away) — the working colour
    /// is discarded, nothing was persisted while browsing. No-op when closed.
    pub(crate) fn wheel_cancel(&mut self) {
        self.wheel = None;
    }

    /// Set the wheel's hue + saturation (a disk press/drag), re-syncing the hex readout.
    pub(crate) fn wheel_set_hs(&mut self, h: f32, s: f32) {
        if let Some(w) = self.wheel.as_mut() {
            w.h = h.rem_euclid(1.0);
            w.s = s.clamp(0.0, 1.0);
            Self::wheel_sync_hex(w);
        }
    }

    /// Set the wheel's value/brightness (a slider press/drag), re-syncing the hex.
    pub(crate) fn wheel_set_v(&mut self, v: f32) {
        if let Some(w) = self.wheel.as_mut() {
            w.v = v.clamp(0.0, 1.0);
            Self::wheel_sync_hex(w);
        }
    }

    /// Tab inside the popover: cycle the keyboard focus Wheel → Value → Hex → Wheel.
    pub(crate) fn wheel_focus_next(&mut self) {
        if let Some(w) = self.wheel.as_mut() {
            w.focus = match w.focus {
                WheelFocus::Wheel => WheelFocus::Value,
                WheelFocus::Value => WheelFocus::Hex,
                WheelFocus::Hex => WheelFocus::Wheel,
            };
        }
    }

    /// Arrow-key adjust by focus (design §7): on the DISK ←/→ step hue ±3°
    /// (Shift ±15°, wrapping — hue is circular) and ↑/↓ step saturation ±0.03
    /// (Shift ±0.15, clamped); on the VALUE slider ←/→ step ±0.02 (Shift ±0.1,
    /// clamped); the HEX field ignores arrows (its keys type digits). `dx`/`dy`
    /// are −1/0/+1 (→ = +dx, ↑ = +dy).
    pub(crate) fn wheel_arrow(&mut self, dx: f32, dy: f32, big: bool) {
        let Some(w) = self.wheel.as_mut() else { return };
        match w.focus {
            WheelFocus::Wheel => {
                let hue = if big { 15.0 } else { 3.0 } / 360.0;
                let sat = if big { 0.15 } else { 0.03 };
                w.h = (w.h + dx * hue).rem_euclid(1.0);
                w.s = (w.s + dy * sat).clamp(0.0, 1.0);
            }
            WheelFocus::Value => {
                let step = if big { 0.1 } else { 0.02 };
                w.v = (w.v + dx * step).clamp(0.0, 1.0);
            }
            WheelFocus::Hex => return,
        }
        Self::wheel_sync_hex(w);
    }

    /// Type into the hex readout (focus == Hex only): accepts hex digits (stored
    /// uppercase) and one leading `#`, capped at `#RRGGBB` length. A buffer that
    /// parses live-syncs the wheel to the typed colour.
    pub(crate) fn wheel_hex_push(&mut self, c: char) {
        let Some(w) = self.wheel.as_mut() else { return };
        if w.focus != WheelFocus::Hex {
            return;
        }
        let ok = c.is_ascii_hexdigit() || (c == '#' && w.hex.is_empty());
        if !ok || w.hex.trim_start_matches('#').len() >= 6 {
            return;
        }
        w.hex.push(c.to_ascii_uppercase());
        Self::wheel_sync_from_hex(w);
    }

    /// Delete the last hex character (focus == Hex only), live-syncing when it parses.
    pub(crate) fn wheel_hex_backspace(&mut self) {
        let Some(w) = self.wheel.as_mut() else { return };
        if w.focus != WheelFocus::Hex {
            return;
        }
        w.hex.pop();
        Self::wheel_sync_from_hex(w);
    }

    /// Wheel → hex: keep the readout the canonical uppercase form of the working colour.
    fn wheel_sync_hex(w: &mut WheelState) {
        w.hex = canonical_hex(crate::widget::hsv_to_rgb(w.h, w.s, w.v));
    }

    /// Hex → wheel: when the typed buffer parses (`#RGB`/`#RRGGBB`), snap h/s/v to it;
    /// a partial/invalid buffer leaves the wheel on its last good colour.
    fn wheel_sync_from_hex(w: &mut WheelState) {
        if let Some(rgb) = parse_hex(&w.hex) {
            let (h, s, v) = rgb_to_hsv(rgb);
            w.h = h;
            w.s = s;
            w.v = v;
        }
    }

    /// The (key, value) edit committing the wheel would persist (design graft #4):
    /// the typed hex when it parses (byte-exact), else the working colour — either
    /// way a CANONICAL uppercase `#RRGGBB` that `typed_item`'s hex validation accepts
    /// by construction. An EMPTY hex commits `None` (remove the key → back to the
    /// theme default, the same blank-clears contract as the text editor). `None`
    /// when no wheel is open. Does NOT mutate — the caller closes + persists.
    pub(crate) fn wheel_pending(&self) -> Option<(&'static str, Option<String>)> {
        let w = self.wheel.as_ref()?;
        let f = self.fields.get(w.field)?;
        let trimmed = w.hex.trim();
        if trimmed.is_empty() {
            return Some((f.key, None));
        }
        let rgb = parse_hex(trimmed).unwrap_or_else(|| crate::widget::hsv_to_rgb(w.h, w.s, w.v));
        Some((f.key, Some(canonical_hex(rgb))))
    }

    /// Scroll the body band by `delta` VISIBLE controls (mouse wheel): moves `scroll`
    /// without touching the selection, clamped to [`max_scroll`] so the band never shows
    /// trailing blank rows.
    pub(crate) fn scroll_body(&mut self, delta: isize, body: usize) {
        let vis = self.visible_indices();
        if vis.is_empty() {
            return;
        }
        let mask = self.visible_mask();
        let max = max_scroll(&self.fields, mask.as_deref(), body);
        // Step along the VISIBLE indices (a masked-out field is skipped by the layout,
        // so landing on one would make a wheel notch a visual no-op).
        let pos = vis
            .iter()
            .position(|&i| i >= self.scroll)
            .unwrap_or(vis.len() - 1);
        let new = (pos as isize + delta).clamp(0, vis.len() as isize - 1) as usize;
        self.scroll = vis[new].min(max);
    }

    /// The displayed value of control `idx` as an owned string: the live edit buffer when
    /// that row is being edited, else its configured value / effective placeholder. Used
    /// by the accessibility tree (no caret, unlike the painter's `render_value`); only the
    /// non-default `a11y-accesskit` build consumes it.
    #[cfg_attr(not(a11y_tree), allow(dead_code))]
    pub(crate) fn displayed_value(&self, idx: usize) -> String {
        let Some(f) = self.fields.get(idx) else {
            return String::new();
        };
        if idx == self.selected
            && let Some(buf) = &self.editing
        {
            return buf.clone();
        }
        Self::display_value(f).to_string()
    }

    /// The CURRENT displayed value for a row: the configured seed, else the effective
    /// placeholder (so an unset key shows what is actually in effect, never blank).
    /// Authored Enum aliases are projected onto their canonical option so the native
    /// picker does not mislabel a runtime-valid alias as a custom value. Unknown and
    /// dynamic values remain verbatim, and unset placeholders retain their explanatory
    /// `"(default)"` / `"(follow OS)"` annotation.
    pub(crate) fn display_value(f: &EditField) -> &str {
        let raw = f.seed.as_deref().unwrap_or(f.placeholder.as_str());
        if f.seed.is_some() && matches!(f.kind, EditKind::Enum { .. }) {
            enum_recognized(f).unwrap_or(raw)
        } else {
            raw
        }
    }

    /// Move the highlight by `delta` over the VISIBLE (filtered) rows, CLAMPING at the
    /// ends (design §6 mandates no wrap — ↑ at the top / ↓ at the bottom stay put), and
    /// keep `scroll` so the selected row stays inside a `body`-row window.
    pub(crate) fn move_selection(&mut self, delta: isize, body: usize) {
        self.status = None; // transient status clears on navigation (design §3.3)
        let vis = self.visible_indices();
        if vis.is_empty() {
            return;
        }
        let pos = vis.iter().position(|&i| i == self.selected).unwrap_or(0);
        let new = (pos as isize + delta).clamp(0, vis.len() as isize - 1) as usize;
        self.selected = vis[new];
        self.clamp_scroll(body);
    }

    /// Keep `scroll` so the SELECTED control is visible in the `body`-row band — which,
    /// with section headers interleaved (and the search filter applied), holds fewer than
    /// `body` controls, so visibility is decided by the real masked layout the painter
    /// renders, not arithmetic on a header-free window.
    pub(crate) fn clamp_scroll(&mut self, body: usize) {
        if body == 0 || self.fields.is_empty() {
            self.scroll = self.selected;
            return;
        }
        let mask = self.visible_mask();
        if self.selected < self.scroll {
            self.scroll = self.selected;
        }
        // Scroll down until the selected control appears in the laid-out band. Bounded by
        // the field count (each step advances `scroll` toward `selected`).
        while self.scroll < self.selected
            && !body_layout_masked(&self.fields, mask.as_deref(), self.scroll, body)
                .iter()
                .any(|r| matches!(r, BodyRow::Control(i) if *i == self.selected))
        {
            self.scroll += 1;
        }
    }

    /// A fingerprint of everything that affects the retired card painter. Compatibility
    /// tests use it to prove state changes repaint without coupling to PTY writes;
    /// production native Settings has its own damage/repaint authority.
    pub(crate) fn fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.selected.hash(&mut h);
        self.scroll.hash(&mut h);
        self.status.hash(&mut h);
        self.editing.hash(&mut h); // live buffer + caret must repaint as it changes
        self.query.hash(&mut h); // the filter changes which rows + the search bar text
        self.searching.hash(&mut h); // search-bar focus changes the title row + caret
        self.category.order_index().hash(&mut h); // the sidebar pill + content pane
        (self.pane == SettingsPane::Sidebar).hash(&mut h); // the focus ring's zone
        // The popup menu: open/closed, anchor, highlight, and scroll each move pixels.
        // (`options` derive from the anchor field's value, hashed below.)
        if let Some(m) = &self.menu {
            m.field.hash(&mut h);
            m.highlighted.hash(&mut h);
            m.scroll.hash(&mut h);
            m.options.len().hash(&mut h);
        } else {
            usize::MAX.hash(&mut h);
        }
        // The colour wheel: open/closed, anchor, the working colour (h/s/v quantized
        // to 1/256 — design §9), the hex text, and the focused sub-control each move
        // pixels (the marker, the slider, the caret, AND the preview's candidate tint).
        if let Some(w) = &self.wheel {
            w.field.hash(&mut h);
            ((w.h.clamp(0.0, 1.0) * 256.0) as u16).hash(&mut h);
            ((w.s.clamp(0.0, 1.0) * 256.0) as u16).hash(&mut h);
            ((w.v.clamp(0.0, 1.0) * 256.0) as u16).hash(&mut h);
            w.hex.hash(&mut h);
            (w.focus as u8).hash(&mut h);
        } else {
            usize::MAX.hash(&mut h);
        }
        self.demo_phase.hash(&mut h); // each demo tick must re-rasterize the card
        // The landing page (§L): its presence flips the WHOLE card, the comment
        // buffer moves its box + caret, and the animation phase folds only while
        // something is actually animating (the hero's blotch drift or a kitty
        // cameo) — an idle two-pane panel never re-rasterizes for it.
        self.landing.hash(&mut h);
        self.comment.hash(&mut h);
        if self.landing || self.kitty_pop.is_some() {
            self.landing_phase.hash(&mut h);
        }
        if let Some(k) = &self.kitty_pop {
            k.breed.hash(&mut h);
            k.start.hash(&mut h);
            ((k.x_frac.clamp(0.0, 1.0) * 256.0) as u16).hash(&mut h);
            (k.host == KittyHost::Landing).hash(&mut h);
        } else {
            usize::MAX.hash(&mut h);
        }
        // The Kitty Log snapshot paints ONLY on its own category page, so its
        // revision folds only there (§F4.6) — a sighting while another page is
        // up moves no pixels and must not force a present (0%-idle discipline).
        if self.category == prefs::Section::KittyLog {
            self.kitty_log.revision.hash(&mut h);
        }
        self.fields.len().hash(&mut h);
        for f in &self.fields {
            f.key.hash(&mut h);
            Self::display_value(f).hash(&mut h);
        }
        h.finish() | 1 // never 0 while open (0 is the closed sentinel)
    }

    /// `(scroll, total, visible)` for compatibility inspection: rows scrolled past,
    /// the full field count, and the fields projected by this semantic model.
    pub(crate) fn scroll_extent(&self) -> (usize, usize, usize) {
        (self.scroll, self.fields.len(), self.shown_indices().len())
    }

    /// Legacy overlay-model serialization retained for model-level regression tests.
    /// Production `controls prefs` compiles the native Settings semantic tree and never
    /// calls this serializer.
    pub(crate) fn controls_lines(&self) -> Vec<String> {
        let vis = self.shown_indices();
        let mut out = Vec::with_capacity(vis.len() + 3);
        out.push(format!(
            "state open=true landing={} pane={} category={} selected={} scroll={} editing={:?} \
             status={:?} searching={} query={:?} shown={} total={}",
            self.landing,
            match self.pane {
                SettingsPane::Sidebar => "sidebar",
                SettingsPane::Content => "content",
            },
            self.category.label(),
            self.selected,
            self.scroll,
            self.editing.as_deref().unwrap_or(""),
            self.status.as_deref().unwrap_or(""),
            self.searching,
            self.query,
            vis.len(),
            self.fields.len(),
        ));
        // The SIDEBAR as painted (design §4.6): the active category + the full
        // section list, so a driver sees the two-pane structure, not just fields.
        out.push(format!(
            "sidebar selected={} sections=[{}]",
            self.category.label().to_lowercase(),
            prefs::Section::ORDER
                .iter()
                .map(|s| s.label().to_lowercase())
                .collect::<Vec<_>>()
                .join(","),
        ));
        // The pinned preview card's live subject (graft #3): which key drives the
        // interior and which preview arm paints — exactly what is on glass (while
        // filtering the card rests on the default mock, so it reports that).
        let focus = if self.filtering() {
            None
        } else {
            self.action_target()
                .and_then(|i| self.fields.get(i))
                .map(|f| f.key)
        };
        out.push(format!(
            "preview key={} kind={}",
            focus.unwrap_or("none"),
            preview_kind(focus.unwrap_or("")),
        ));
        // The open popup menu (anchor key, highlight, current-value index, scroll, and
        // the exact option list shown — including a preserved custom entry), so a driver
        // can assert the menu it sees on glass. Options are DEBUG-QUOTED: a preserved
        // custom value can itself contain commas ("dark:Nord,light:GitHub Light"), so a
        // bare comma join would be unparseable.
        if let Some(m) = &self.menu {
            let key = self.fields.get(m.field).map_or("?", |f| f.key);
            let options: Vec<String> = m.options.iter().map(|o| format!("{o:?}")).collect();
            out.push(format!(
                "menu key={} highlighted={} current={} scroll={} options=[{}]",
                key,
                m.highlighted,
                m.current,
                m.scroll,
                options.join(","),
            ));
        }
        // The open colour wheel (anchor key, working colour, hex text, focused
        // sub-control) — exactly the uncommitted candidate on glass, so a driver can
        // assert a scrub without pixel-diffing. Hue serializes in degrees (a hex
        // string carries the exact colour; h/s/v carry the wheel's marker).
        if let Some(w) = &self.wheel {
            let key = self.fields.get(w.field).map_or("?", |f| f.key);
            out.push(format!(
                "colorwheel key={} h={:.0} s={:.2} v={:.2} hex={:?} focus={}",
                key,
                f64::from(w.h) * 360.0,
                w.s,
                w.v,
                w.hex,
                match w.focus {
                    WheelFocus::Wheel => "wheel",
                    WheelFocus::Value => "value",
                    WheelFocus::Hex => "hex",
                },
            ));
        }
        out.push(format!("prefs fields={}", vis.len()));
        let field_line = |i: usize| {
            let f = &self.fields[i];
            // `value` is the user's CONFIGURED raw value (blank = unset);
            // `effective` (placeholder) is what is actually in use.
            let value = f.seed.as_deref().unwrap_or("");
            // `kind` makes the control TYPE machine-readable; an Enum/Theme also lists its
            // allowed `options` so a reader knows the exact value domain.
            let kind = match &f.kind {
                EditKind::Float => "float".to_string(),
                EditKind::Integer => "integer".to_string(),
                EditKind::Bool => "bool".to_string(),
                EditKind::Text => "text".to_string(),
                // The `cursor_trail_style` domain also advertises the loaded
                // `pack:<id>` options so an AI driver reads the same set the picker
                // offers; every other Enum keeps its exact static options.
                EditKind::Enum { .. } if f.key == prefs::EDIT_CURSOR_TRAIL_STYLE => format!(
                    "enum options=[{}]",
                    prefs::cursor_trail_style_options(
                        self.trail_pack_ids.iter().map(String::as_str)
                    )
                    .join(",")
                ),
                EditKind::Enum { options } => format!("enum options=[{}]", options.join(",")),
                EditKind::Theme => {
                    format!(
                        "theme options=[{}]",
                        aterm_types::scheme::builtin_names().join(",")
                    )
                }
                EditKind::Color => "color".to_string(),
            };
            format!(
                "field key={} label={:?} value={:?} effective={:?} kind={}",
                f.key, f.label, value, f.placeholder, kind
            )
        };
        if self.filtering() {
            // The flat cross-category result list — no group boxes are painted.
            for &i in &vis {
                out.push(field_line(i));
            }
        } else if self.category == prefs::Section::KittyLog {
            // The collection book (§F4.6): `kittylog …` rows from the SAME
            // snapshot + row model the painter renders (no fields live here,
            // so the grouped walk below would serialize nothing).
            out.extend(crate::kitty_log::book_lines(&self.kitty_log.log));
        } else {
            // Grouped mode: `group label="…"` lines interleave before their fields
            // (design §4.6), from the SAME layout walk the painter renders — the
            // control order equals `vis` (category_controls) by construction.
            for row in category_layout(&self.fields, self.category, usize::MAX) {
                match row {
                    GroupRow::Caption(c) => out.push(format!("group label={c:?}")),
                    GroupRow::Control(i) => out.push(field_line(i)),
                    GroupRow::Footnote(_) | GroupRow::Gap => {}
                }
            }
        }
        out
    }
}

/// Resolve a documented config ALIAS to its canonical option spelling. The config loaders
/// (`app_config`) accept aliases that are NOT in the picker's canonical option set (e.g.
/// cursor_style `beam` == `bar`); without this native Settings would misclassify the
/// authored value as a custom option. Returns `None` when `token` is not a known alias.
fn enum_alias(key: &str, token: &str) -> Option<&'static str> {
    // Trail-style aliases (nyan rainbow → rainbow kitty, ember → fire, …) resolve through the
    // shared table in `prefs` — the same source `--validate-config` and the
    // load-time unknown-style warning consult, so the native row, preview lane,
    // and the live effect can never disagree about an aliased spelling.
    let token = token.trim();
    if key == prefs::EDIT_CURSOR_TRAIL_STYLE {
        return prefs::cursor_trail_style_canonical(token);
    }
    // Typing-sound aliases (water → droplet, mech → mechanical, bell → glass
    // bell, …) resolve through the synth's own parser, so an authored alias
    // projects onto its picker row instead of showing as a custom entry.
    if key == prefs::EDIT_TRAIL_SOUND_STYLE {
        return prefs::trail_sound_style_canonical(token);
    }
    Some(match (key, token.to_ascii_lowercase().as_str()) {
        (prefs::EDIT_CURSOR_STYLE, "beam" | "underline") => "bar",
        (prefs::EDIT_BIDI, "off") => "disabled",
        (prefs::EDIT_BIDI, "on") => "implicit",
        (prefs::EDIT_AMBIGUOUS_WIDTH, "single") => "narrow",
        (prefs::EDIT_AMBIGUOUS_WIDTH, "double") => "wide",
        (prefs::EDIT_PREDICTIVE_ECHO, "auto" | "on" | "true") => "adaptive",
        (prefs::EDIT_PREDICTIVE_ECHO, "force") => "always",
        (prefs::EDIT_TEXT_BLENDING, "linear_corrected") => "linear-corrected",
        (prefs::EDIT_MOTION, "reduce") => "reduced",
        (prefs::EDIT_WINDOW_COLORSPACE, "displayp3" | "p3") => "display-p3",
        (prefs::EDIT_BACKGROUND_MATERIAL, "underwindow" | "under_window") => "under-window",
        (prefs::EDIT_BACKGROUND_MATERIAL, "") => "none",
        _ => return None,
    })
}

/// The canonical current option spelling for compatibility projection and shared stepping.
/// It resolves annotated defaults and documented aliases before falling back for a
/// genuinely unrecognized spelling.
///
/// THE FALLBACK MUST BE WHAT THE RUNTIME DRAWS, not `options[0]`. This overlay is the
/// ONLY Settings surface on Linux and Windows, and its consumers claim to describe the
/// live effect: a segmented row's chip, the Enter/Space cycle anchor, and
/// [`demo_style`]'s animated lane on the Cursor Kitty page. For `cursor_trail_style` the
/// live consumer is that lane — the row carries too many options for the segmented
/// paint, so its own chip and its ←/→ stepping run through [`popup_current_label`] /
/// [`popup_current_index`], which preserve the authored spelling verbatim rather than
/// consulting this (a `pack:<id>` is unrecognized here too, and that is the arm which
/// keeps it intact). An unrecognized spelling resolves to
/// [`prefs::DEFAULT_CURSOR_TRAIL_STYLE`] and RENDERS (`app_config::resolve_trail_style`),
/// so `options[0]` — "phaser" — would animate an effect the engine is not playing. Every
/// other Enum row keeps the defensive first-option fallback; that is what `cursor_style`
/// pins.
pub(crate) fn enum_current(f: &EditField) -> &'static str {
    let EditKind::Enum { options } = f.kind else {
        return "";
    };
    enum_recognized(f).unwrap_or_else(|| {
        if f.key == prefs::EDIT_CURSOR_TRAIL_STYLE {
            prefs::DEFAULT_CURSOR_TRAIL_STYLE
        } else {
            options.first().copied().unwrap_or("")
        }
    })
}

/// The canonical option an Enum row's configured value RESOLVES to (directly or via the
/// alias map), or `None` for a genuinely unrecognized spelling — the case
/// [`popup_options`] must preserve verbatim rather than clobber with `options[0]`.
fn enum_recognized(f: &EditField) -> Option<&'static str> {
    let EditKind::Enum { options } = f.kind else {
        return None;
    };
    let token = enum_candidate(f);
    options
        .iter()
        .find(|o| token.eq_ignore_ascii_case(o))
        .copied()
        .or_else(|| enum_alias(f.key, token).filter(|c| options.contains(c)))
}

/// The semantic Enum spelling before canonical alias resolution. Authored seeds are
/// preserved in full (including multi-word styles and future custom values); only an
/// unset row's human-facing placeholder annotation is removed.
fn enum_candidate(f: &EditField) -> &str {
    let Some(seed) = f.seed.as_deref() else {
        let placeholder = f.placeholder.trim();
        return placeholder
            .split_once(" (")
            .map_or(placeholder, |(value, _)| value)
            .trim();
    };
    seed.trim()
}

/// The (key, value) edit to persist when the user ACTIVATES (Enter/Space/click) the
/// given control: a Bool toggles, an Enum advances to its next option (wrapping). The
/// free-form kinds (Float/Integer/Text) return `None` so the native text editor can own
/// them. Choice-picker rows are routed before this compatibility helper is consulted.
pub(crate) fn cycle_edit(f: &EditField) -> Option<(&'static str, Option<String>)> {
    match f.kind {
        EditKind::Bool => {
            let on = SettingsState::display_value(f)
                .trim()
                .eq_ignore_ascii_case("true");
            Some((f.key, Some((!on).to_string())))
        }
        EditKind::Enum { options } => {
            if options.is_empty() {
                return None;
            }
            let cur = enum_current(f);
            let i = options
                .iter()
                .position(|o| o.eq_ignore_ascii_case(cur))
                .unwrap_or(0);
            let next = options[(i + 1) % options.len()];
            Some((f.key, Some(next.to_string())))
        }
        EditKind::Theme => {
            // Cycle through the built-in colour-scheme registry; each step persists +
            // live-reloads, so the terminal re-themes immediately (that IS the preview).
            let names = aterm_types::scheme::builtin_names();
            if names.is_empty() {
                return None;
            }
            let cur = theme_current(f);
            let i = names
                .iter()
                .position(|n| n.eq_ignore_ascii_case(&cur))
                .unwrap_or(0);
            Some((f.key, Some(names[(i + 1) % names.len()].to_string())))
        }
        _ => None,
    }
}

/// The current theme NAME for a [`EditKind::Theme`] row: the displayed value matched
/// (case-insensitively) against the built-in registry, falling back to the first name
/// ("Default") for the swatch/preview paths. A user theme or `dark:…,light:…` split form
/// not in the registry is NOT clobbered by that fallback: the menu and ←/→ stepping list
/// the raw value as its own entry via [`popup_options`], and the popup chip labels it
/// verbatim ([`popup_current_label`]) — only the swatches degrade to the fallback.
pub(crate) fn theme_current(f: &EditField) -> String {
    let names = aterm_types::scheme::builtin_names();
    let shown = SettingsState::display_value(f);
    names
        .iter()
        .find(|n| shown.eq_ignore_ascii_case(n))
        .copied()
        .unwrap_or_else(|| names.first().copied().unwrap_or("Default"))
        .to_string()
}

/// Whether a row renders a popup chip — and therefore opens the anchored MENU on
/// Enter/Space/click instead of cycling: Theme rows always, Enum rows too long for the
/// segmented control (the SAME [`fits_segmented`] decision the painter takes).
pub(crate) fn uses_popup(f: &EditField) -> bool {
    match f.kind {
        EditKind::Theme => true,
        EditKind::Enum { options } => !fits_segmented(options),
        _ => false,
    }
}

/// The full option list a Theme/Enum row's menu (and ←/→ stepping) offers, in order.
/// CUSTOM-VALUE PRESERVATION: when the configured value is not in the canonical set (a
/// user theme, a `dark:…,light:…` split, or an unrecognized enum spelling), it is
/// prepended VERBATIM as entry 0 — so it is highlighted on open (Enter = no-op) and
/// stepped FROM, never silently replaced. Empty for non-pick kinds.
pub(crate) fn popup_options(f: &EditField) -> Vec<String> {
    popup_options_with(f, &[])
}

/// [`popup_options`] with the loaded Trail Pack ids threaded in: the
/// `cursor_trail_style` picker lists one `pack:<id>` option per loaded pack
/// (the dynamic twin of the static [`prefs::CURSOR_TRAIL_STYLES`], resolved like
/// the Theme picker's `builtin_names()`). `pack_ids` is empty for every other
/// row and for a pack-free config, so their option lists are byte-identical.
pub(crate) fn popup_options_with(f: &EditField, pack_ids: &[String]) -> Vec<String> {
    match f.kind {
        EditKind::Theme => {
            let names = aterm_types::scheme::builtin_names();
            let shown = SettingsState::display_value(f).trim();
            let mut out: Vec<String> = Vec::with_capacity(names.len() + 1);
            if !shown.is_empty() && !names.iter().any(|n| shown.eq_ignore_ascii_case(n)) {
                out.push(shown.to_string());
            }
            out.extend(names.iter().map(|n| (*n).to_string()));
            out
        }
        EditKind::Enum { options } => {
            let mut out: Vec<String> = Vec::with_capacity(options.len() + pack_ids.len() + 1);
            // The configured value, when it is not a canonical option (a user's
            // `pack:<id>` or an unrecognized spelling), leads verbatim + highlighted
            // so opening + Enter is a no-op and it is stepped FROM, never clobbered.
            if enum_recognized(f).is_none() {
                let token = enum_candidate(f);
                if !token.is_empty() {
                    out.push(token.to_string());
                }
            }
            // The `cursor_trail_style` row lists the built-ins THEN one `pack:<id>`
            // per loaded pack (deduped against the verbatim entry above); every
            // other Enum row keeps its exact static options.
            if f.key == crate::prefs::EDIT_CURSOR_TRAIL_STYLE {
                for o in
                    crate::prefs::cursor_trail_style_options(pack_ids.iter().map(String::as_str))
                {
                    if !out.iter().any(|e| e.eq_ignore_ascii_case(&o)) {
                        out.push(o);
                    }
                }
            } else {
                out.extend(options.iter().map(|o| (*o).to_string()));
            }
            out
        }
        _ => Vec::new(),
    }
}

/// Index of the CURRENT value within [`popup_options`]: the preserved custom entry when
/// one was prepended (always index 0), else the canonical current option. Falls back to
/// 0 defensively (an empty list is rejected before this is consulted).
pub(crate) fn popup_current_index(f: &EditField, options: &[String]) -> usize {
    let cur: String = match f.kind {
        EditKind::Theme => {
            let shown = SettingsState::display_value(f).trim();
            let names = aterm_types::scheme::builtin_names();
            if !shown.is_empty() && !names.iter().any(|n| shown.eq_ignore_ascii_case(n)) {
                shown.to_string()
            } else {
                theme_current(f)
            }
        }
        EditKind::Enum { .. } => match enum_recognized(f) {
            Some(o) => o.to_string(),
            None => enum_candidate(f).to_string(),
        },
        _ => return 0,
    };
    options
        .iter()
        .position(|o| o.eq_ignore_ascii_case(&cur))
        .unwrap_or(0)
}

/// The popup chip's label for a Theme/long-Enum row: the CURRENT entry of
/// [`popup_options`] — i.e. the raw custom value verbatim when one is configured, else
/// the canonical current option — so the chip never claims a value that is not in
/// effect. Empty for non-popup rows.
pub(crate) fn popup_current_label(f: &EditField) -> String {
    let options = popup_options(f);
    if options.is_empty() {
        return String::new();
    }
    let i = popup_current_index(f, &options);
    options[i].clone()
}

/// The (key, value) edit a ←/→ IN-PLACE ADJUST persists (design §6), or `None` when the
/// row has nothing to step: a Bool toggles; an Enum/Theme steps to its prev/next
/// [`popup_options`] entry (wrapping, and stepping FROM a preserved custom entry rather
/// than clobbering it); a bounded numeric ([`prefs::range_of`]) moves one `step` (`big`
/// ⇒ ×10, i.e. Shift held), clamped to the range — `None` at a rail so a held key
/// doesn't spam "unchanged" saves. Free-form rows (Text/Color/unbounded numeric) no-op.
pub(crate) fn step_edit(
    f: &EditField,
    delta: isize,
    big: bool,
) -> Option<(&'static str, Option<String>)> {
    step_edit_with(f, delta, big, &[])
}

/// [`step_edit`] with the loaded Trail Pack ids threaded in, so ←/→ on the
/// `cursor_trail_style` row cycles through the loaded `pack:<id>` options too.
/// `pack_ids` is empty for every other row (byte-identical stepping).
pub(crate) fn step_edit_with(
    f: &EditField,
    delta: isize,
    big: bool,
    pack_ids: &[String],
) -> Option<(&'static str, Option<String>)> {
    match f.kind {
        EditKind::Bool => cycle_edit(f),
        EditKind::Enum { .. } | EditKind::Theme => {
            let options = popup_options_with(f, pack_ids);
            if options.len() < 2 {
                return None;
            }
            let cur = popup_current_index(f, &options);
            let next = (cur as isize + delta).rem_euclid(options.len() as isize) as usize;
            (next != cur).then(|| (f.key, Some(options[next].clone())))
        }
        EditKind::Float | EditKind::Integer => {
            let r = prefs::range_of(f.key)?;
            let raw = SettingsState::display_value(f);
            let tok = raw.split_whitespace().next().unwrap_or(raw);
            let v: f64 = tok.parse().ok()?;
            let step = r.step * if big { 10.0 } else { 1.0 } * delta as f64;
            let new = (v + step).clamp(r.min, r.max);
            if new == v {
                return None; // already at the rail
            }
            // f64 Display prints the shortest round-trip form ("14", "13.5") — matching
            // how the seeds/placeholders spell numeric values.
            Some((f.key, Some(new.to_string())))
        }
        EditKind::Text | EditKind::Color => None,
    }
}

/// The translucent accent-wash alpha behind the keyboard-selected row. Unique among
/// the painter's panel alphas so a test can detect the selection highlight.
const SEL_WASH_ALPHA: u8 = 0x22;

/// The accent-wash alpha behind the popup menu's HIGHLIGHTED option — distinct from
/// [`SEL_WASH_ALPHA`] so a test can tell the menu highlight from the row selection.
const MENU_WASH_ALPHA: u8 = 0x30;

/// Theme-derived colour roles for the retired card painter. They are rebuilt from the
/// live [`Theme`] so compatibility snapshots re-tint without a hardcoded palette.
#[derive(Clone, Copy)]
pub(crate) struct Roles {
    pub(crate) surface: [u8; 3],
    pub(crate) text_primary: [u8; 3],
    pub(crate) text_secondary: [u8; 3],
    pub(crate) text_tertiary: [u8; 3],
    pub(crate) accent: [u8; 3],
    pub(crate) on_accent: [u8; 3],
    pub(crate) separator: [u8; 3],
    pub(crate) control_track: [u8; 3],
    pub(crate) elevated: [u8; 3],
    pub(crate) danger: [u8; 3],
    pub(crate) success: [u8; 3],
}

pub(crate) fn u32_rgb(c: u32) -> [u8; 3] {
    [
        ((c >> 16) & 0xff) as u8,
        ((c >> 8) & 0xff) as u8,
        (c & 0xff) as u8,
    ]
}

fn lerp_rgb(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    std::array::from_fn(|i| {
        (f32::from(a[i]) + (f32::from(b[i]) - f32::from(a[i])) * t).round() as u8
    })
}

fn luma(c: [u8; 3]) -> f32 {
    0.2126 * f32::from(c[0]) + 0.7152 * f32::from(c[1]) + 0.0722 * f32::from(c[2])
}

/// Parse `#RGB` / `#RRGGBB` to RGB (for the colour swatch). `None` for anything else.
fn parse_hex(s: &str) -> Option<[u8; 3]> {
    let s = s.trim().trim_start_matches('#');
    let b = s.as_bytes();
    let hx = |c: u8| (c as char).to_digit(16).map(|v| v as u8);
    match s.len() {
        6 => {
            let n = u32::from_str_radix(s, 16).ok()?;
            Some(u32_rgb(n))
        }
        3 => Some([hx(b[0])? * 17, hx(b[1])? * 17, hx(b[2])? * 17]),
        _ => None,
    }
}

/// Canonical uppercase `#RRGGBB` — the ONE spelling the colour wheel displays and
/// commits, always accepted by prefs' save-time hex validation (design graft #4).
pub(crate) fn canonical_hex(c: [u8; 3]) -> String {
    format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2])
}

/// RGB → HSV (`h` in 0..1 turns, `s`/`v` in 0..1) — seeds the wheel from a
/// configured hex. Inverse of [`crate::widget::hsv_to_rgb`] (the disk raster +
/// commit math), so open → ↵ round-trips the same colour.
fn rgb_to_hsv(c: [u8; 3]) -> (f32, f32, f32) {
    let r = f32::from(c[0]) / 255.0;
    let g = f32::from(c[1]) / 255.0;
    let b = f32::from(c[2]) / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    // Grey (d == 0) has no hue — pin it to 0 rather than NaN.
    let h = if d <= f32::EPSILON {
        0.0
    } else if max == r {
        (((g - b) / d).rem_euclid(6.0)) / 6.0
    } else if max == g {
        ((b - r) / d + 2.0) / 6.0
    } else {
        ((r - g) / d + 4.0) / 6.0
    };
    let s = if max <= f32::EPSILON { 0.0 } else { d / max };
    (h, s, max)
}

/// The LIVE theme colour in effect for a Color row's `key` (design §7): an UNSET
/// foreground/background/cursor/selection row falls back to the colour the theme
/// actually renders for THAT key — never a one-size accent. The ONE source for the
/// wheel's seed ([`crate::App::settings_wheel_open`]) and the popover's "old"
/// swatch, so the two can never disagree about what colour is in effect.
pub(crate) fn theme_color_for_key(theme: Theme, key: &str) -> [u8; 3] {
    u32_rgb(match key {
        prefs::EDIT_FOREGROUND => theme.fg,
        prefs::EDIT_BACKGROUND => theme.bg,
        prefs::EDIT_SELECTION_COLOR => theme.selection,
        // cursor_color — and the least-wrong anchor for any future Color key.
        _ => theme.cursor,
    })
}

impl Roles {
    pub(crate) fn from_theme(theme: Theme) -> Self {
        let roles = crate::native_appearance::default_roles(theme);
        Self {
            surface: roles.surface,
            text_primary: roles.text_primary,
            text_secondary: roles.text_secondary,
            text_tertiary: roles.text_tertiary,
            accent: roles.accent,
            on_accent: roles.on_accent,
            separator: roles.separator,
            control_track: roles.control_track,
            elevated: roles.elevated,
            danger: roles.danger,
            success: roles.success,
        }
    }
}

/// The on/off state of a Bool row (its resolved value).
fn bool_on(f: &EditField) -> bool {
    SettingsState::display_value(f)
        .trim()
        .eq_ignore_ascii_case("true")
}

/// Whether the user has explicitly configured this row (drives the "overridden" dot).
/// For free-form / pick kinds an unset key seeds `None`, so a present, non-blank seed
/// reliably means "user set it". Bool rows are EXCLUDED: their seed is always the
/// effective value (never `None`), so the field alone can't distinguish a user override
/// from the default — better to show no dot than a false one on every toggle.
fn is_overridden(f: &EditField) -> bool {
    // Bool seeds always hold the RESOLVED value (never `None`), so a non-blank
    // seed does not mean "user override" for them.
    if matches!(f.kind, EditKind::Bool) {
        return false;
    }
    f.seed
        .as_deref()
        .map(str::trim)
        .is_some_and(|s| !s.is_empty())
}

/// The fraction (0..1) of a bounded numeric row within its range + its display token.
fn slider_frac(f: &EditField) -> Option<(f32, String)> {
    let r = prefs::range_of(f.key)?;
    let raw = SettingsState::display_value(f);
    let tok = raw.split_whitespace().next().unwrap_or(raw);
    let v: f64 = tok.parse().ok()?;
    let frac = ((v - r.min) / (r.max - r.min)).clamp(0.0, 1.0) as f32;
    Some((frac, tok.to_string()))
}

/// An Enum short enough to draw as a SEGMENTED control (else it gets a popup chip).
fn fits_segmented(options: &[&str]) -> bool {
    options.len() <= 3 && options.iter().all(|o| o.chars().count() <= 8)
}

/// The four representative swatch colours for a built-in theme NAME (bg/fg/cursor/
/// selection), shown in the theme popup chip. Empty for an unknown name.
fn theme_swatches(name: &str) -> Vec<[u8; 3]> {
    aterm_types::scheme::builtin(name).map_or_else(Vec::new, |s| {
        let p = s.to_theme_parts();
        vec![
            u32_rgb(p.bg),
            u32_rgb(p.fg),
            u32_rgb(p.cursor),
            u32_rgb(p.selection),
        ]
    })
}

/// The width of `s` at size `px` in the MONO (terminal) face — REAL advances from
/// the chrome font stack (the user's terminal face, DejaVu strictly as coverage
/// fallback), replacing the old hardcoded `0.6 em × chars` estimate that drifted
/// from every non-DejaVu face. Regular weight; measure bold runs via
/// [`crate::tray_raster::measure_text`] directly. UI-face chrome measures with
/// [`ui_text_width`] instead.
pub(crate) fn text_w(s: &str, px: f32) -> f32 {
    crate::tray_raster::measure_text(s, px, TextWeight::Regular)
}

/// Truncate `s` with a trailing ellipsis so it fits `max_w` at size `px` in the
/// UI FACE (the [`ui_text_width`] metric — mono approximation when SF is absent);
/// returned unchanged when it already fits. Keeps a verbatim custom value from
/// growing its chip past the pane it lives in.
fn elide_to(s: &str, px: f32, max_w: f32) -> String {
    if ui_text_width(s, px) <= max_w {
        return s.to_string();
    }
    let ell_w = ui_text_width("\u{2026}", px);
    let mut out: String = s.to_string();
    while out.pop().is_some() {
        if ui_text_width(&out, px) + ell_w <= max_w {
            break;
        }
    }
    out.push('\u{2026}');
    out
}

/// A right-aligned value label ending at `right`, cap-height-centred in the row, in
/// `face` — [`TextFace::Ui`] for readouts, [`TextFace::Mono`] for hex codes — measured
/// with the MATCHING metric so the right edge never drifts from the painted glyphs.
#[allow(clippy::too_many_arguments)]
fn val_text(
    prims: &mut Vec<DrawPrim>,
    right: f32,
    y0: f32,
    ch: f32,
    px: f32,
    s: &str,
    color: [u8; 3],
    face: TextFace,
) {
    let size = TypeStep::Secondary.px(px);
    let w = if face == TextFace::Mono {
        text_w(s, size.get())
    } else {
        ui_text_width(s, size.get())
    };
    prims.push(text_prim(
        right - w,
        row_baseline(y0, ch, size.get()),
        s.to_string(),
        size,
        TextWeight::Regular,
        face,
        rgba(color, 0xFF),
    ));
}

/// A POPUP chip `[ swatches? value ▾ ]` for Enum (long) + Theme rows. Returns the chip's
/// left edge (the hit-test's activation boundary).
#[allow(clippy::too_many_arguments)]
fn popup_chip(
    prims: &mut Vec<DrawPrim>,
    swatches: &[[u8; 3]],
    value: &str,
    r: &Roles,
    cw: f32,
    px: f32,
    wy: f32,
    wh: f32,
    v_left: f32,
    v_right: f32,
) -> f32 {
    let size = TypeStep::Secondary.px(px);
    let dot_r = wh * 0.26;
    let sw_w = if swatches.is_empty() {
        0.0
    } else {
        swatches.len() as f32 * dot_r * 2.1 + cw * 0.3
    };
    // Containment (design §3.2): a verbatim custom value can label the chip with an
    // arbitrarily long string, but the chip must stay inside its group box — clamp
    // the width to the content pane (`v_left..v_right`, never the sidebar) and elide
    // the label to what fits.
    let avail = (v_right - v_left).max(cw * 2.0);
    let value = elide_to(value, size.get(), (avail - sw_w - cw * 1.7).max(0.0));
    let vw = ui_text_width(&value, size.get());
    let chip_w = (sw_w + vw + cw * 1.7).min(avail);
    let x = (v_right - chip_w).max(v_left);
    prims.push(DrawPrim::Panel {
        x,
        y: wy,
        w: chip_w,
        h: wh,
        radius: wh * 0.35,
        fill: rgba(r.elevated, 0xFF),
        blur: false,
    });
    let mut cx = x + cw * 0.5 + dot_r;
    for s in swatches {
        prims.push(DrawPrim::Dot {
            cx,
            cy: wy + wh * 0.5,
            r: dot_r,
            color: rgba(*s, 0xFF),
            breathe: false,
        });
        cx += dot_r * 2.1;
    }
    let text_x = if swatches.is_empty() {
        x + cw * 0.5
    } else {
        cx + cw * 0.2
    };
    // One shared baseline for the chip's value + affordance runs.
    let chip_baseline = row_baseline(wy, wh, size.get());
    prims.push(text_prim(
        text_x,
        chip_baseline,
        value.to_string(),
        size,
        TextWeight::Regular,
        TextFace::Ui,
        rgba(r.text_primary, 0xFF),
    ));
    // Dropdown affordance (a single down triangle; the substrate's Stroke is axis-aligned
    // so a glyph reads cleaner than an approximated chevron here). Mono: it is glyph
    // art (a dingbat), not label text — the bundled face is guaranteed to carry it.
    prims.push(text_prim(
        x + chip_w - cw,
        chip_baseline,
        "\u{25BE}".to_string(),
        size,
        TextWeight::Regular,
        TextFace::Mono,
        rgba(r.text_tertiary, 0xFF),
    ));
    x
}

/// A SEGMENTED control (a sliding chip over N options) for short Enum rows. Returns the
/// track's left edge (the hit-test's activation boundary).
#[allow(clippy::too_many_arguments)]
fn segmented(
    prims: &mut Vec<DrawPrim>,
    options: &[&str],
    current: &str,
    r: &Roles,
    cw: f32,
    px: f32,
    wy: f32,
    wh: f32,
    v_left: f32,
    v_right: f32,
) -> f32 {
    let size = TypeStep::Caption.px(px);
    let seg_w = |o: &str| (ui_text_width(o, size.get()) + cw * 1.0).max(cw * 2.4);
    let total: f32 = options.iter().map(|o| seg_w(o)).sum();
    // Floor at the content pane's left edge (not the pre-v2 `cw*6`): on the icon-
    // strip ladder rung a wide track clips at the box, never over the sidebar.
    let x0 = (v_right - total).max(v_left);
    prims.push(DrawPrim::Panel {
        x: x0,
        y: wy,
        w: total,
        h: wh,
        radius: wh * 0.3,
        fill: rgba(r.control_track, 0x99),
        blur: false,
    });
    let mut x = x0;
    for o in options {
        let sw = seg_w(o);
        let on = o.eq_ignore_ascii_case(current);
        if on {
            prims.push(DrawPrim::Panel {
                x: x + 1.5,
                y: wy + 1.5,
                w: sw - 3.0,
                h: wh - 3.0,
                radius: wh * 0.25,
                fill: rgba(r.elevated, 0xFF),
                blur: false,
            });
        }
        prims.push(text_prim(
            x + (sw - ui_text_width(o, size.get())) * 0.5,
            row_baseline(wy, wh, size.get()),
            (*o).to_string(),
            size,
            TextWeight::Regular,
            TextFace::Ui,
            rgba(if on { r.text_primary } else { r.text_secondary }, 0xFF),
        ));
        x += sw;
    }
    x0
}

/// `f32::clamp(lo, hi)` panics when `lo > hi`. Retired-card geometry can invert
/// bounds in extreme compatibility fixtures — a tiny font drops the max widget below the
/// 8px floor; an ultra-narrow grid (≤15 cols) drops the available input width
/// below the desired minimum. Pin to the achievable (upper) bound in that case
/// instead of panicking.
#[inline]
pub(crate) fn fit(value: f32, lo: f32, hi: f32) -> f32 {
    // Inverted bounds (degenerate layout): pin to the achievable upper bound,
    // floored at zero so a too-narrow window yields a zero-width widget rather
    // than a NEGATIVE length (which would wrap on a later `as u32` downstream).
    if hi <= lo {
        hi.max(0.0)
    } else {
        value.clamp(lo, hi)
    }
}

/// Render the right-aligned WIDGET for control `f` (the heart of the redesign): a real
/// toggle / segmented / popup / slider / stepper / framed input / colour swatch composed
/// from the substrate primitives. `editing` (the live buffer) overrides free-form rows.
/// Returns the widget's leftmost interactive x — the SAME geometry the mouse hit-test
/// consumes ([`widget_hit_left`]), so "click the widget region" can never drift from
/// what is painted.
#[allow(clippy::too_many_arguments)]
fn build_widget(
    prims: &mut Vec<DrawPrim>,
    f: &EditField,
    r: &Roles,
    cw: f32,
    ch: f32,
    px: f32,
    y0: f32,
    v_left: f32,
    v_right: f32,
    selected: bool,
    editing: Option<&str>,
) -> f32 {
    let wh = fit(ch * 0.64, 8.0, px * 1.1);
    let wy = y0 + (ch - wh) * 0.5;
    let cy = y0 + ch * 0.5;

    // An in-progress free-form edit: a focused framed input with a snapped caret.
    if let Some(buf) = editing {
        let fw = fit(cw * 14.0, cw * 6.0, v_right - cw * 8.0);
        let fx = v_right - fw;
        prims.push(DrawPrim::Stroke {
            x: fx,
            y: wy,
            w: fw,
            h: wh,
            radius: wh * 0.35,
            width: 1.5,
            color: rgba(r.accent, 0xFF),
        });
        let size = TypeStep::Secondary.px(px);
        let tx = fx + cw * 0.4;
        prims.push(text_prim(
            tx,
            row_baseline(y0, ch, size.get()),
            buf.to_string(),
            size,
            TextWeight::Regular,
            TextFace::Ui,
            rgba(r.text_primary, 0xFF),
        ));
        prims.push(DrawPrim::Stroke {
            x: tx + ui_text_width(buf, size.get()) + 1.0,
            y: wy + 2.0,
            w: 1.0,
            h: wh - 4.0,
            radius: 0.0,
            width: 1.0,
            color: rgba(r.accent, 0xFF),
        });
        return fx;
    }

    match f.kind {
        EditKind::Bool => {
            let on = bool_on(f);
            let security = prefs::section_of(f.key) == prefs::Section::Security;
            let pill_w = wh * 1.95;
            let x = v_right - pill_w;
            let track = if on {
                if security { r.danger } else { r.accent }
            } else {
                r.control_track
            };
            prims.push(DrawPrim::Panel {
                x,
                y: wy,
                w: pill_w,
                h: wh,
                radius: wh * 0.5,
                fill: rgba(track, 0xFF),
                blur: false,
            });
            let knob_r = wh * 0.5 - 1.5;
            let kcx = if on {
                x + pill_w - knob_r - 2.0
            } else {
                x + knob_r + 2.0
            };
            prims.push(DrawPrim::Dot {
                cx: kcx,
                cy,
                r: knob_r,
                color: rgba(r.on_accent, 0xFF),
                breathe: false,
            });
            x
        }
        EditKind::Enum { options } => {
            if fits_segmented(options) {
                segmented(
                    prims,
                    options,
                    enum_current(f),
                    r,
                    cw,
                    px,
                    wy,
                    wh,
                    v_left,
                    v_right,
                )
            } else {
                // A custom (unrecognized) value labels the chip verbatim, never a
                // substituted `options[0]` (mirrors the menu's preserved entry 0).
                let label = popup_current_label(f);
                popup_chip(prims, &[], &label, r, cw, px, wy, wh, v_left, v_right)
            }
        }
        EditKind::Theme => {
            // A custom theme (user / `dark:…,light:…` split) labels the chip with the
            // raw value and drops the swatches (there is no single scheme to swatch).
            let label = popup_current_label(f);
            let sw = theme_swatches(&label);
            popup_chip(prims, &sw, &label, r, cw, px, wy, wh, v_left, v_right)
        }
        EditKind::Float | EditKind::Integer => {
            if let Some((frac, vs)) = slider_frac(f) {
                let track_w = cw * 9.0;
                // The value reads in the UI face (see val_text below) — measure it
                // with the matching metric at the same TypeStep size.
                let vtw = ui_text_width(&vs, TypeStep::Secondary.px(px).get()) + cw;
                let tx = v_right - vtw - track_w;
                let th = (wh * 0.4).max(4.0);
                prims.push(DrawPrim::Capsule {
                    x: tx,
                    y: cy - th * 0.5,
                    w: track_w,
                    h: th,
                    frac,
                    fill: rgba(r.accent, 0xFF),
                    track: rgba(r.control_track, 0xFF),
                });
                let thumb_x = tx + frac * track_w;
                let tr = wh * 0.45;
                prims.push(DrawPrim::Dot {
                    cx: thumb_x,
                    cy,
                    r: tr,
                    color: rgba(r.on_accent, 0xFF),
                    breathe: false,
                });
                prims.push(DrawPrim::Stroke {
                    x: thumb_x - tr,
                    y: cy - tr,
                    w: tr * 2.0,
                    h: tr * 2.0,
                    radius: tr,
                    width: 1.0,
                    color: rgba(r.accent, 0xFF),
                });
                val_text(
                    prims,
                    v_right,
                    y0,
                    ch,
                    px,
                    &vs,
                    r.text_primary,
                    TextFace::Ui,
                );
                tx
            } else {
                // Unbounded integer → a framed numeric field (typeable at rest).
                let vs = SettingsState::display_value(f).to_string();
                let fw = cw * 7.0;
                let fx = v_right - fw;
                prims.push(DrawPrim::Stroke {
                    x: fx,
                    y: wy,
                    w: fw,
                    h: wh,
                    radius: wh * 0.35,
                    width: 1.0,
                    color: rgba(if selected { r.accent } else { r.control_track }, 0xFF),
                });
                let size = TypeStep::Secondary.px(px);
                prims.push(text_prim(
                    fx + fw - ui_text_width(&vs, size.get()) - cw * 0.4,
                    row_baseline(y0, ch, size.get()),
                    vs,
                    size,
                    TextWeight::Regular,
                    TextFace::Ui,
                    rgba(r.text_primary, 0xFF),
                ));
                fx
            }
        }
        EditKind::Color => {
            // A real hex readout stays MONO (it is a code; macOS sets codes in a
            // fixed-pitch face even inside native panels) — but the unset row's
            // "theme default" placeholder is PROSE, so it reads in the UI face.
            let hex = SettingsState::display_value(f).to_string();
            let parsed = parse_hex(&hex);
            let col = parsed.unwrap_or(r.control_track);
            let face = if parsed.is_some() {
                TextFace::Mono
            } else {
                TextFace::Ui
            };
            val_text(prims, v_right, y0, ch, px, &hex, r.text_primary, face);
            // Measure with the SAME face + TypeStep size val_text paints, so the
            // swatch sits flush left of the readout on both the hex and prose paths.
            let hsize = TypeStep::Secondary.px(px).get();
            let vw = if face == TextFace::Mono {
                text_w(&hex, hsize)
            } else {
                ui_text_width(&hex, hsize)
            };
            let sw_w = cw * 1.7;
            let sx = v_right - vw - cw - sw_w;
            prims.push(DrawPrim::Panel {
                x: sx,
                y: wy,
                w: sw_w,
                h: wh,
                radius: wh * 0.3,
                fill: rgba(col, 0xFF),
                blur: false,
            });
            prims.push(DrawPrim::Stroke {
                x: sx,
                y: wy,
                w: sw_w,
                h: wh,
                radius: wh * 0.3,
                width: 1.0,
                color: rgba(r.separator, 0xAA),
            });
            sx
        }
        EditKind::Text => {
            let vs = SettingsState::display_value(f).to_string();
            let tertiary = f.seed.is_none();
            let fw = fit(cw * 14.0, cw * 6.0, v_right - cw * 8.0);
            let fx = v_right - fw;
            prims.push(DrawPrim::Stroke {
                x: fx,
                y: wy,
                w: fw,
                h: wh,
                radius: wh * 0.35,
                width: 1.0,
                color: rgba(if selected { r.accent } else { r.control_track }, 0xAA),
            });
            let size = TypeStep::Secondary.px(px);
            prims.push(text_prim(
                fx + fw - ui_text_width(&vs, size.get()) - cw * 0.4,
                row_baseline(y0, ch, size.get()),
                vs,
                size,
                TextWeight::Regular,
                TextFace::Ui,
                rgba(
                    if tertiary {
                        r.text_tertiary
                    } else {
                        r.text_primary
                    },
                    0xFF,
                ),
            ));
            fx
        }
    }
}

/// Total height requested by the retired card painter. Kept for compatibility-layout
/// tests; production native Settings uses renderer-native layout constraints.
pub(crate) fn wanted_rows(fields: &[EditField]) -> usize {
    fields.len() + distinct_sections(fields) + 2
}

/// Count the distinct [`prefs::Section`]s among `fields` (one header is drawn per section).
fn distinct_sections(fields: &[EditField]) -> usize {
    // u16: ORDER has grown past 8 sections (a u8 shift would overflow at
    // index 8+ — debug-panic, release silent wrap).
    let mut seen = 0u16;
    for (i, sec) in prefs::Section::ORDER.iter().enumerate() {
        if fields.iter().any(|f| prefs::section_of(f.key) == *sec) {
            seen |= 1 << i;
        }
    }
    seen.count_ones() as usize
}

/// One body row in the retired card painter: a section header or control index.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum BodyRow {
    Header(prefs::Section),
    Control(usize),
}

/// Lay out the body band (all controls visible). Thin wrapper over [`body_layout_masked`]
/// with no filter — a test-only convenience (production callers pass the live mask).
#[cfg(test)]
pub(crate) fn body_layout(fields: &[EditField], scroll: usize, body: usize) -> Vec<BodyRow> {
    body_layout_masked(fields, None, scroll, body)
}

/// Lay out the body band, honoring an optional search-filter `visible` mask: walk
/// `fields[scroll..]`, SKIP masked-out controls, and emit a `Header` whenever the section
/// changes from the previously-emitted control's section (and for the first shown field),
/// then the control, until `body` rows are filled. PURE — the SINGLE source the painter,
/// the mouse hit-test, and the scroll-visibility check share, so what is painted, what a
/// click maps to, and what scroll keeps visible never diverge (filtered or not).
pub(crate) fn body_layout_masked(
    fields: &[EditField],
    visible: Option<&[bool]>,
    scroll: usize,
    body: usize,
) -> Vec<BodyRow> {
    let vis = |i: usize| visible.is_none_or(|m| m.get(i).copied().unwrap_or(false));
    // `body` can be `usize::MAX` (a "count everything" caller); cap the pre-alloc to the
    // real upper bound (every control + a header per control) so it never overflows.
    let mut out = Vec::with_capacity(body.min(fields.len().saturating_mul(2).saturating_add(2)));
    // When the band is too short to hold a header + its first control (`body < 2`), seed
    // `prev` to the first SHOWN field's section so the leading header is SKIPPED and the
    // control itself is emitted — otherwise a 1-row body would only ever show the header
    // and `clamp_scroll` could never bring the selected control into view (a 3-row panel).
    let mut prev: Option<prefs::Section> = if body < 2 {
        (scroll..fields.len())
            .find(|&i| vis(i))
            .map(|i| prefs::section_of(fields[i].key))
    } else {
        None
    };
    let mut i = scroll;
    while out.len() < body && i < fields.len() {
        if !vis(i) {
            i += 1;
            continue;
        }
        let sec = prefs::section_of(fields[i].key);
        if prev != Some(sec) {
            out.push(BodyRow::Header(sec));
            prev = Some(sec);
            if out.len() >= body {
                break; // header took the last row; the control shows next scroll step
            }
        }
        out.push(BodyRow::Control(i));
        i += 1;
    }
    out
}

/// The largest useful `scroll` for the wheel gesture: the first visible-control index
/// at which the remaining rows no longer OVERFLOW the `body`-row band (0 when
/// everything fits), so wheel-scrolling stops once the tail is fully on screen. Walks
/// the same masked layout the painter renders (n ≈ 30 controls — a linear walk is
/// trivial). Advancing past a section's LAST control removes two rows at once (the
/// control and its now-redundant header), so the row count can jump from `body + 1`
/// to `body - 1` with no exact-`body` stop in between — a keep-the-band-full clamp
/// would leave the final control permanently clipped there, so the clamp accepts the
/// first non-overflowing scroll instead (at worst one trailing blank row).
pub(crate) fn max_scroll(fields: &[EditField], visible: Option<&[bool]>, body: usize) -> usize {
    if body == 0 {
        return 0;
    }
    let mut scroll = 0;
    // `body + 1` caps the walk: only "more rows than fit?" matters, not the total.
    while scroll + 1 < fields.len()
        && body_layout_masked(fields, visible, scroll, body + 1).len() > body
    {
        scroll += 1;
    }
    scroll
}

/// The LAID-OUT rows (section headers included) above the flat band's first painted
/// row — the scrollbar thumb's offset. `scroll` is a FIELD index (the unit
/// `scroll_body`/[`max_scroll`] clamp in) while the thumb maps rows of the full
/// masked layout, so this converts: the window opens on its first control's section
/// header when the full layout places one directly above it (elsewhere the window
/// re-emits the header, which adds no offset).
pub(crate) fn flat_rows_before(
    fields: &[EditField],
    visible: Option<&[bool]>,
    scroll: usize,
) -> usize {
    let full = body_layout_masked(fields, visible, 0, usize::MAX);
    let Some(p) = full
        .iter()
        .position(|r| matches!(r, BodyRow::Control(i) if *i >= scroll))
    else {
        return full.len();
    };
    if p > 0 && matches!(full[p - 1], BodyRow::Header(_)) {
        p - 1
    } else {
        p
    }
}

/// Device-pixel geometry for the retired frosted-card painter. It remains compiled for
/// compatibility snapshots and hit-test tests; production native Settings does not use
/// this terminal-grid geometry.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SettingsGeom {
    pub cw: f32,
    pub ch: f32,
    pub font_px: f32,
    pub cols: usize,
    pub panel_rows: usize,
}

// ---- Fixed region map (design §1) -------------------------------------------------
// Every region below is a function of WINDOW SIZE ONLY (cols × panel_rows) — never of
// selection, focus, or search — so arrow keys can move nothing but a selection wash and
// the preview card's interior pixels (the feedback-#2 regression fix, tested by
// `regions_are_invariant_under_selection_change`).

/// Cols at/above which the FULL layout paints (26-cell sidebar + pinned preview card).
pub(crate) const FULL_LAYOUT_MIN_COLS: usize = 96;
/// Cols below which the sidebar collapses to an icon strip (graft #3's fallback ladder).
pub(crate) const SIDEBAR_STRIP_COLS: usize = 64;
/// Width below which the icon strip hides in degenerate retired-card fixtures.
const SIDEBAR_HIDE_COLS: usize = 24;
/// Sidebar width, cells: full (labels) / collapsed (icon strip).
const SIDEBAR_FULL_CELLS: f32 = 26.0;
const SIDEBAR_STRIP_CELLS: f32 = 8.0;
/// First row of the content pane's bands (rows 1-2 are the title / search field).
const CONTENT_TOP_ROW: usize = 3;
/// The pinned preview card's fixed height, rows (rows 3..12 at full layout).
const PREVIEW_CARD_ROWS: usize = 9;
/// Shortest retired card that still reserves the preview band (leaves ≥2 controls).
const PREVIEW_MIN_PANEL_ROWS: usize = 18;
/// First sidebar category row (six 2-cell rows: 4-5, 6-7, … 14-15 — design §2.2).
pub(crate) const SIDEBAR_CAT_ROW0: usize = 4;

/// The fixed region map of the two-pane card, all cell units. PURE function of the
/// window geometry — the single source the painter, the mouse hit-test, the scroll
/// clamp, and the keyboard band math share.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct PaneGeom {
    /// Sidebar width in CELLS (26 full / 8 icon strip / 0 hidden), also the content
    /// pane's left edge.
    pub(crate) sidebar_w_cells: f32,
    /// Whether the sidebar is collapsed to the icon strip (labels + search text hide).
    pub(crate) icon_strip: bool,
    /// The pinned preview card's row band `[start, end)`; EMPTY (start == end) below
    /// the narrow/short breakpoints — the resize-only fallback ladder (graft #3).
    pub(crate) preview: (usize, usize),
    /// The group-box (or flat search-result) band's row range `[start, end)`.
    pub(crate) groups: (usize, usize),
    /// The status footer row (the card's last row).
    pub(crate) footer_row: usize,
}

impl PaneGeom {
    /// The content pane's left edge, device px.
    pub(crate) fn content_x(&self, cw: f32) -> f32 {
        self.sidebar_w_cells * cw
    }
    /// The group band's height, cells (== rows for the 1-cell flat search list).
    pub(crate) fn group_band(&self) -> usize {
        self.groups.1.saturating_sub(self.groups.0)
    }
    /// Whether the preview band is reserved at this geometry.
    pub(crate) fn preview_shown(&self) -> bool {
        self.preview.1 > self.preview.0
    }
}

/// The fixed region map for a card geometry (see [`PaneGeom`]).
pub(crate) fn pane_geom(g: &SettingsGeom) -> PaneGeom {
    pane_geom_cells(g.cols, g.panel_rows)
}

/// [`pane_geom`] on bare cell counts — the App's band math calls this without device
/// metrics (cells are the shared unit of every band).
pub(crate) fn pane_geom_cells(cols: usize, panel_rows: usize) -> PaneGeom {
    let (sidebar_w_cells, icon_strip) = if cols >= SIDEBAR_STRIP_COLS {
        (SIDEBAR_FULL_CELLS, false)
    } else if cols >= SIDEBAR_HIDE_COLS {
        (SIDEBAR_STRIP_CELLS, true)
    } else {
        (0.0, true)
    };
    let footer_row = panel_rows.saturating_sub(1);
    let top = CONTENT_TOP_ROW.min(footer_row);
    let preview_end = if cols >= FULL_LAYOUT_MIN_COLS && panel_rows >= PREVIEW_MIN_PANEL_ROWS {
        (top + PREVIEW_CARD_ROWS).min(footer_row)
    } else {
        top
    };
    PaneGeom {
        sidebar_w_cells,
        icon_strip,
        preview: (top, preview_end),
        groups: (preview_end, footer_row.max(preview_end)),
        footer_row,
    }
}

/// What a point in the SIDEBAR hits, by cell row: the search field or a category row.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SidebarHit {
    Search,
    Category(prefs::Section),
}

/// Map a sidebar cell row to its target (pure — shared by the painter's row placement
/// and the mouse hit-test). `None` on the margins / past the footer.
pub(crate) fn sidebar_hit(row: usize, panel_rows: usize) -> Option<SidebarHit> {
    if row + 1 >= panel_rows {
        return None; // the footer row and below
    }
    if row == 1 || row == 2 {
        return Some(SidebarHit::Search);
    }
    let i = row.checked_sub(SIDEBAR_CAT_ROW0)? / 2;
    // Mirror the painter's clip (`row0 + 2 > footer_row` breaks): a category whose
    // full 2-cell row does not fit above the footer is never painted, so its TOP
    // cell must not hit either — otherwise a short retired card would switch categories
    // on a click over blank sidebar.
    if SIDEBAR_CAT_ROW0 + i * 2 + 2 > panel_rows.saturating_sub(1) {
        return None;
    }
    prefs::Section::ORDER
        .get(i)
        .copied()
        .map(SidebarHit::Category)
}

// ---- Landing page (design §L) -------------------------------------------------------

/// The §L landing palette — the settings site's identity, deliberately CONSTANT in
/// either OS appearance: the hero commits to its light mint world (the two-pane
/// panel behind Get started keeps the normal theme-driven chrome).
const LANDING_MINT: [u8; 3] = [0xED, 0xF6, 0xEC];
const LANDING_INK: [u8; 3] = [0x16, 0x27, 0x1B];
const LANDING_SOFT: [u8; 3] = [0x4B, 0x63, 0x53];
const LANDING_BUBBLE: [u8; 3] = [0x9B, 0xE8, 0x9B];
const LANDING_BUBBLE_INK: [u8; 3] = [0x0F, 0x24, 0x10];
const LANDING_CARD_WHITE: [u8; 3] = [0xFF, 0xFF, 0xFF];
const LANDING_LINE: [u8; 3] = [0xC9, 0xDD, 0xC8];
const LANDING_HINT: [u8; 3] = [0x98, 0xA2, 0x9B];
const BLOB_CORAL: [u8; 3] = [0xFF, 0x51, 0x48];
const BLOB_MARIGOLD: [u8; 3] = [0xFF, 0xC2, 0x2E];
const BLOB_LEAF: [u8; 3] = [0x2F, 0xAE, 0x5B];
const BLOB_COBALT: [u8; 3] = [0x2E, 0x5B, 0xFF];
const BLOB_PINK: [u8; 3] = [0xFF, 0x7B, 0xAC];
const BLOB_VIOLET: [u8; 3] = [0xB0, 0x5C, 0xFF];
const LANDING_CTA: &str = "Get started →";
const LANDING_PLACEHOLDER: &str = "Click here to comment on what should be the next update!";
const LANDING_EYEBROW: &str = "THE ATERM CONFIGURATION GUIDE";
const LANDING_TITLE: &str = "aterm Settings";
const LANDING_LEDE_1: &str = "aterm is a fast, hardened, GPU-accelerated terminal whose live screen is fully introspectable.";
const LANDING_LEDE_2: &str =
    "Every theme, cursor effect, font knob, and security switch — one hot-reloaded aterm.toml.";
const LANDING_FOOT_HINT: &str = "Enter opens the settings  ·  Esc closes";

// ---- §L.5 The rainbow welcome ---------------------------------------------------
// The landing's welcome flourish: a pastel rainbow arch sweeping in behind the
// hero and a small twinkling constellation. Purely decorative — no LandingGeom
// target, so hit-testing never sees any of it.
/// The rainbow arch's sixth stripe (the blob palette covers the other five).
const RAINBOW_TEAL: [u8; 3] = [0x21, 0xC2, 0xB7];

/// Animation clock for the landing's scripted entrances: 0 before `start`,
/// smoothstepping 0→1 over `len` ticks after it. Reduced motion freezes
/// `landing_phase` at 0 (`tick_landing` never fires there), so phase 0
/// short-circuits to the RESTING pose (1.0) — the page must never hide its
/// content from a user who opted out of motion.
fn entrance(phase: u32, start: u32, len: f32) -> f32 {
    if phase == 0 {
        return 1.0;
    }
    let t = (phase.saturating_sub(start) as f32 / len).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Card-relative device-px geometry of the landing page's interactive targets — the
/// ONE source the painter draws from AND `settings_click` hit-tests with, so a press
/// lands exactly on the bubble painted under it (the `pane_geom` discipline, §L).
pub(crate) struct LandingGeom {
    /// The Get-started bubble, `(x, y, w, h)`.
    pub(crate) btn: (f32, f32, f32, f32),
    /// The suggestion box, `(x, y, w, h)`.
    pub(crate) tbox: (f32, f32, f32, f32),
    /// The send bubble inside the box's right end, `(cx, cy, r)`.
    pub(crate) send: (f32, f32, f32),
}

/// Top of the hero title as a fraction of the card height: the TITLE is the
/// centrepiece — at 0.32 the whole stack (eyebrow above, lede/bubble/box below)
/// sits optically centred on the 38-row card.
const LANDING_TITLE_Y_FRAC: f32 = 0.32;
/// The hero title's font-size multiplier over the terminal size (feeds the
/// Display step, so the painted glyphs run ≈ 1.6 × this × the terminal px).
const LANDING_TITLE_SCALE: f32 = 2.6;
/// The title row's height in cells (clears the 4-cell-tall display glyphs).
const LANDING_TITLE_ROWS: f32 = 4.0;

/// The §L fixed hero column: eyebrow → title (the centrepiece) → two lede lines
/// → the Get-started bubble → the suggestion box, everything below the title.
/// PURE function of the window geometry, like [`pane_geom`].
pub(crate) fn landing_geom(g: &SettingsGeom) -> LandingGeom {
    let (cw, ch, px) = (g.cw, g.ch, g.font_px);
    let w = g.cols as f32 * cw;
    let h = g.panel_rows as f32 * ch;
    let title_y = h * LANDING_TITLE_Y_FRAC;
    let lede_end = title_y + ch * (LANDING_TITLE_ROWS + 0.3) + ch * 1.2 + ch;
    let bsize = TypeStep::Title.px(px * 1.12);
    let bw = ui_text_width(LANDING_CTA, bsize.get()) + bsize.get() * 2.6;
    let bh = ch * 2.0;
    let bx = (w - bw) * 0.5;
    let by = lede_end + ch * 0.9;
    let th = ch * 1.8;
    let tw = (w * 0.52).min(cw * 58.0);
    let tx = (w - tw) * 0.5;
    let ty = by + bh + ch * 1.3;
    LandingGeom {
        btn: (bx, by, bw, bh),
        tbox: (tx, ty, tw, th),
        send: (tx + tw - th * 0.5, ty + th * 0.5, th * 0.36),
    }
}

/// Paint the §L landing page over the card: mint ground, five drifting solid
/// colour blotches (Dot pairs — organic, not geometric), the hero type column,
/// the Get-started bubble, and the suggestion box. The kitty cameo (§L.4) paints
/// BEFORE the box so a summoned cat pops out from BEHIND it.
fn paint_landing(prims: &mut Vec<DrawPrim>, state: &SettingsState, g: &SettingsGeom) {
    let (cw, ch, px) = (g.cw, g.ch, g.font_px);
    let w = g.cols as f32 * cw;
    let h = g.panel_rows as f32 * ch;
    let lg = landing_geom(g);
    // Mint ground over the frosted card, same rounding (the §1 card radius).
    prims.push(DrawPrim::Panel {
        x: 0.0,
        y: 0.0,
        w,
        h,
        radius: (ch * 0.6).min(14.0),
        fill: rgba(LANDING_MINT, 0xFF),
        blur: false,
    });
    // Blotches, clipped to the card. The clip is RECTANGULAR while the card is
    // rounded, so every blob placement below keeps clear of the four corners —
    // no square pixel can escape the rounded outline.
    let t = state.landing_phase as f32 / 30.0;
    prims.push(DrawPrim::ClipPush {
        x: 0.0,
        y: 0.0,
        w,
        h,
    });
    // The rainbow arch (§L.5): a wide pastel band cresting behind the hero
    // column, sweeping in left→right on open. Painted FIRST inside the clip so
    // the saturated blobs read as the nearer layer where they overlap.
    paint_rainbow_arch(prims, state.landing_phase, w, h, ch);
    let m = h.min(w);
    let blobs: [([u8; 3], f32, f32, f32, f32); 5] = [
        (BLOB_CORAL, 0.30, -0.08, 0.26, 0.0),
        (BLOB_MARIGOLD, 1.00, 0.32, 0.22, 1.7),
        (BLOB_LEAF, 0.72, 1.04, 0.24, 3.1),
        (BLOB_COBALT, 0.07, 0.66, 0.095, 4.4),
        (BLOB_PINK, 0.21, 0.42, 0.045, 5.2),
    ];
    for (c, fx, fy, fr, off) in blobs {
        // Slow ambient drift (frozen under Reduced motion — the phase never
        // ticks there, exactly like the preview demo, W11).
        let dx = (t * 0.55 + off).sin() * ch * 0.35;
        let dy = (t * 0.42 + off * 1.3).cos() * ch * 0.28;
        let (cx, cy, r) = (fx * w + dx, fy * h + dy, fr * m);
        prims.push(DrawPrim::Dot {
            cx,
            cy,
            r,
            color: rgba(c, 0xFF),
            breathe: false,
        });
        // A second, offset lobe makes each blotch read organic, not geometric.
        prims.push(DrawPrim::Dot {
            cx: cx + r * 0.55,
            cy: cy + r * 0.30,
            r: r * 0.74,
            color: rgba(c, 0xFF),
            breathe: false,
        });
    }
    // Star glints (§L.5): a few tiny twinkling crosses in the mint sky around
    // the hero — alive under motion, a static constellation without it.
    paint_landing_glints(prims, state.landing_phase, w, h, ch);
    prims.push(DrawPrim::ClipPop);

    // The hero type column, centred. One local funnel wrapper (the §2 rule).
    let center = |prims: &mut Vec<DrawPrim>,
                  y0: f32,
                  row_h: f32,
                  size: crate::type_scale::StepPx,
                  weight: TextWeight,
                  face: TextFace,
                  s: &str,
                  color: [u8; 3]| {
        let x = (w - ui_text_width(s, size.get())) * 0.5;
        prims.push(text_prim(
            x,
            row_baseline(y0, row_h, size.get()),
            s.to_string(),
            size,
            weight,
            face,
            rgba(color, 0xFF),
        ));
    };
    let title_y = h * LANDING_TITLE_Y_FRAC;
    center(
        prims,
        title_y - ch * 1.8,
        ch,
        TypeStep::Caption.px(px),
        TextWeight::Regular,
        TextFace::Ui,
        LANDING_EYEBROW,
        LANDING_SOFT,
    );
    center(
        prims,
        title_y - ch * 0.7,
        ch,
        TypeStep::Caption.px(px),
        TextWeight::Regular,
        TextFace::Ui,
        crate::build_info::AUTHOR_COMPANY_BYLINE,
        LANDING_SOFT,
    );
    center(
        prims,
        title_y,
        ch * LANDING_TITLE_ROWS,
        TypeStep::Display.px(px * LANDING_TITLE_SCALE),
        TextWeight::Regular,
        TextFace::UiBold,
        LANDING_TITLE,
        LANDING_INK,
    );
    center(
        prims,
        title_y + ch * (LANDING_TITLE_ROWS + 0.3),
        ch,
        TypeStep::Secondary.px(px),
        TextWeight::Regular,
        TextFace::Ui,
        LANDING_LEDE_1,
        LANDING_SOFT,
    );
    center(
        prims,
        title_y + ch * (LANDING_TITLE_ROWS + 0.3) + ch * 1.2,
        ch,
        TypeStep::Secondary.px(px),
        TextWeight::Regular,
        TextFace::Ui,
        LANDING_LEDE_2,
        LANDING_SOFT,
    );

    // Kitty cameo (§L.4) BEFORE the box/button chrome: a pop summoned from the
    // suggestion box rises from BEHIND its opaque white pill.
    let (tx, ty, tw, th) = lg.tbox;
    if let Some(k) = &state.kitty_pop
        && k.host == KittyHost::Landing
    {
        let r_head = ch * 0.85;
        paint_kitty_cameo(
            prims,
            k,
            state.landing_phase,
            tx + th * 0.4,
            tw - th * 1.6,
            ty + th * 0.55,
            th * 0.55 + r_head * 1.35,
            r_head,
        );
    }

    // The Get-started bubble: faked elevation (§5 — an offset dark panel, no
    // shadow prim), the pill, and its label.
    let (bx, by, bw, bh) = lg.btn;
    prims.push(DrawPrim::Panel {
        x: bx + 2.0,
        y: by + 3.0,
        w: bw,
        h: bh,
        radius: bh * 0.5,
        fill: rgba([0, 0, 0], 0x24),
        blur: false,
    });
    prims.push(DrawPrim::Panel {
        x: bx,
        y: by,
        w: bw,
        h: bh,
        radius: bh * 0.5,
        fill: rgba(LANDING_BUBBLE, 0xFF),
        blur: false,
    });
    let bsize = TypeStep::Title.px(px * 1.12);
    prims.push(text_prim(
        bx + (bw - ui_text_width(LANDING_CTA, bsize.get())) * 0.5,
        row_baseline(by, bh, bsize.get()),
        LANDING_CTA.to_string(),
        bsize,
        TextWeight::Regular,
        TextFace::UiBold,
        rgba(LANDING_BUBBLE_INK, 0xFF),
    ));

    // The suggestion box: a white pill + hairline, the live buffer (front-trimmed
    // to keep the caret in view) or the grey placeholder, and the send bubble.
    prims.push(DrawPrim::Panel {
        x: tx,
        y: ty,
        w: tw,
        h: th,
        radius: th * 0.5,
        fill: rgba(LANDING_CARD_WHITE, 0xFF),
        blur: false,
    });
    prims.push(DrawPrim::Stroke {
        x: tx,
        y: ty,
        w: tw,
        h: th,
        radius: th * 0.5,
        width: 1.0,
        color: rgba(LANDING_LINE, 0xFF),
    });
    let tsize = TypeStep::Secondary.px(px);
    let text_x = tx + th * 0.55;
    let avail = tw - th * 1.7 - th * 0.55; // left pad → send bubble clearance
    if state.comment.is_empty() {
        prims.push(text_prim(
            text_x,
            row_baseline(ty, th, tsize.get()),
            LANDING_PLACEHOLDER.to_string(),
            tsize,
            TextWeight::Regular,
            TextFace::Ui,
            rgba(LANDING_HINT, 0xFF),
        ));
    } else {
        // Front-trim: show the TAIL that fits, so the caret always stays visible.
        // Bounded by the buffer length (each step drops one leading char).
        let mut shown = state.comment.as_str();
        while ui_text_width(shown, tsize.get()) > avail {
            let Some((i, _)) = shown.char_indices().nth(1) else {
                break;
            };
            shown = &shown[i..];
        }
        prims.push(text_prim(
            text_x,
            row_baseline(ty, th, tsize.get()),
            shown.to_string(),
            tsize,
            TextWeight::Regular,
            TextFace::Ui,
            rgba(LANDING_INK, 0xFF),
        ));
        prims.push(DrawPrim::Stroke {
            x: text_x + ui_text_width(shown, tsize.get()) + 1.5,
            y: ty + th * 0.22,
            w: 1.0,
            h: th * 0.56,
            radius: 0.0,
            width: 1.0,
            color: rgba(BLOB_LEAF, 0xFF),
        });
    }
    let (scx, scy, sr) = lg.send;
    prims.push(DrawPrim::Dot {
        cx: scx,
        cy: scy,
        r: sr,
        color: rgba(LANDING_BUBBLE, 0xFF),
        breathe: false,
    });
    let ssize = TypeStep::Caption.px(px);
    prims.push(text_prim(
        scx - ui_text_width("→", ssize.get()) * 0.5,
        row_baseline(scy - sr, sr * 2.0, ssize.get()),
        "→".to_string(),
        ssize,
        TextWeight::Regular,
        TextFace::UiBold,
        rgba(LANDING_BUBBLE_INK, 0xFF),
    ));

    // Transient status under the box (the send confirmation), then the key hint.
    if let Some(mstatus) = &state.status {
        center(
            prims,
            ty + th + ch * 0.35,
            ch,
            TypeStep::Caption.px(px),
            TextWeight::Regular,
            TextFace::Ui,
            mstatus,
            BLOB_LEAF,
        );
    }
    center(
        prims,
        h - ch * 1.6,
        ch,
        TypeStep::Caption.px(px),
        TextWeight::Regular,
        TextFace::Ui,
        LANDING_FOOT_HINT,
        LANDING_HINT,
    );
}

/// The §L.5 rainbow arch: six translucent stripes of overlapping Dots along a
/// half-turn centred below the card, red outermost — the settings site's nod to
/// the rainbow the user lives in. `entrance` sweeps it in left→right; phase 0
/// (reduced motion, and the open's very first frame) paints it complete.
fn paint_rainbow_arch(prims: &mut Vec<DrawPrim>, phase: u32, w: f32, h: f32, ch: f32) {
    let stripes = [
        BLOB_CORAL,
        BLOB_MARIGOLD,
        BLOB_LEAF,
        RAINBOW_TEAL,
        BLOB_COBALT,
        BLOB_VIOLET,
    ];
    let sweep = entrance(phase, 4, 22.0);
    if sweep <= 0.0 {
        return;
    }
    let (cx, cy) = (w * 0.5, h * 0.78);
    let r0 = h * 0.40;
    let st = (ch * 0.5).max(4.0);
    for (i, c) in stripes.iter().enumerate() {
        // Red is stripe 0 and must land OUTERMOST, so radius descends with i.
        let r = (r0 + (stripes.len() - 1 - i) as f32 * st).max(1.0);
        // Dot centres every ~0.8 stripe widths along the arc: near enough to
        // fuse into a band at this alpha, far enough to stay a bounded count —
        // and hard-capped at 512 per stripe so a pathological geometry (the
        // extreme-geometry guard) can never balloon the prim list.
        let step = ((st * 0.8) / (std::f32::consts::PI * r)).max(1.0 / 512.0);
        let mut a = 0.0f32;
        while a <= sweep {
            let th = std::f32::consts::PI * (1.0 + a);
            prims.push(DrawPrim::Dot {
                cx: cx + th.cos() * r,
                cy: cy + th.sin() * r,
                r: st * 0.62,
                color: rgba(*c, 0x3C),
                breathe: false,
            });
            a += step;
        }
    }
}

/// The §L.5 star glints: five fixed twinkling crosses (two hairline Strokes +
/// a centre Dot — the cameo's prim vocabulary) placed in the mint sky clear of
/// the hero column. Alpha breathes on the landing phase; frozen phase 0 leaves
/// each at its own mid-twinkle brightness.
fn paint_landing_glints(prims: &mut Vec<DrawPrim>, phase: u32, w: f32, h: f32, ch: f32) {
    let t = phase as f32 / 30.0;
    let glints: [(f32, f32, f32, [u8; 3], f32); 5] = [
        (0.235, 0.205, 0.55, BLOB_MARIGOLD, 0.0),
        (0.755, 0.165, 0.45, BLOB_VIOLET, 1.9),
        (0.685, 0.335, 0.32, BLOB_MARIGOLD, 3.4),
        (0.300, 0.345, 0.38, RAINBOW_TEAL, 4.6),
        (0.505, 0.115, 0.42, BLOB_CORAL, 5.8),
    ];
    for (fx, fy, sc, c, off) in glints {
        let twinkle = 0.5 + 0.5 * (t * 1.7 + off).sin();
        let a = (0x2A as f32 + twinkle * 96.0) as u8;
        let (sx, sy) = (fx * w, fy * h);
        let len = ch * sc;
        prims.push(DrawPrim::Stroke {
            x: sx - len,
            y: sy - 0.75,
            w: len * 2.0,
            h: 1.5,
            radius: 0.75,
            width: 1.5,
            color: rgba(c, a),
        });
        prims.push(DrawPrim::Stroke {
            x: sx - 0.75,
            y: sy - len,
            w: 1.5,
            h: len * 2.0,
            radius: 0.75,
            width: 1.5,
            color: rgba(c, a),
        });
        prims.push(DrawPrim::Dot {
            cx: sx,
            cy: sy,
            r: len * 0.16,
            color: rgba(c, a),
            breathe: false,
        });
    }
}

/// The Settings surface's aurora: the wallpaper treatment, procedurally — a
/// dim DIAGONAL spectrum wash over the whole canvas, exactly the hue ramp the
/// user's rainbow terminal wears (`hue ∝ x + y`). Painted as a coarse tile
/// grid of translucent Panels over the theme surface: at this alpha adjacent
/// tiles differ by a whisper of hue, so the grid fuses into a smooth wash
/// while staying a bounded, cache-friendly prim count. Deliberately STATIC.
pub(crate) fn paint_settings_aurora(
    prims: &mut Vec<DrawPrim>,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    surface: [u8; 3],
) {
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let cols = 112usize;
    let rows = 64usize;
    let cw = w / cols as f32;
    let ch = h / rows as f32;
    for j in 0..rows {
        for i in 0..cols {
            let hue = (i as f32 / cols as f32 + j as f32 / rows as f32) * 0.5;
            // A gentle value falloff toward the bottom keeps the content zone
            // calmer than the sky above it.
            let v = 0.92 - 0.16 * (j as f32 / rows as f32);
            let c = crate::widget::hsv_to_rgb(hue, 0.86, v);
            // OPAQUE tiles, pre-mixed into the surface: translucent neighbours
            // double up wherever they overlap and etch a lattice into the sky;
            // opaque paint makes the half-pixel seam bleed invisible.
            prims.push(DrawPrim::Panel {
                x: x + i as f32 * cw,
                y: y + j as f32 * ch,
                w: cw + 0.75,
                h: ch + 0.75,
                radius: 0.0,
                fill: rgba(lerp_rgb(surface, c, 0.30), 0xFF),
                blur: false,
            });
        }
    }
}

/// The §L.5 composition as a NATIVE Settings hero banner: a pastel rainbow arch
/// cresting through a small constellation of star glints. Deliberately STATIC:
/// the native scheduler keeps idle routes at 0% and this banner never asks for a
/// frame. Painted through the audited custom-node lowering
/// (`native_ui::RAINBOW_BANNER_AUDIT`); everything clips to `rect`.
///
/// `sky` and `rim` arrive RESOLVED from the caller's role palette, exactly as
/// [`paint_settings_aurora`] takes `surface` — this banner is a Settings CARD,
/// and a card that does not follow the resolved chrome palette is a hole in the
/// page. It used to fill with the landing page's authored `#EDF6EC` mint and a
/// `#C9DDC8` hairline, byte-identical in all four appearance states, which put a
/// near-white slab in the middle of the forced-DARK Settings page (the 2026-08
/// cold visual audit's headline defect, and the last place config `window_theme`
/// was still a no-op).
///
/// The ARCH and the GLINTS keep their authored spectrum — they are the identity,
/// and a rainbow that re-tints per theme stops being a rainbow — but their alpha
/// is conditioned on the sky they land on: saturated trim at ~27 % over a pale
/// sky reads as a pastel wash, while the same alpha over a dark sky all but
/// vanishes, so the dark side gets the deeper pour that lands it in the same
/// place perceptually.
pub(crate) fn paint_rainbow_banner(
    prims: &mut Vec<DrawPrim>,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    sky: [u8; 3],
    rim: [u8; 3],
) {
    let dark_sky = crate::native_appearance::surface_is_dark(sky);
    let (arch_alpha, glint_alpha) = if dark_sky { (0x86, 0xC4) } else { (0x46, 0x6E) };
    prims.push(DrawPrim::Panel {
        x,
        y,
        w,
        h,
        radius: 12.0,
        fill: rgba(sky, 0xFF),
        blur: false,
    });
    prims.push(DrawPrim::Stroke {
        x,
        y,
        w,
        h,
        radius: 12.0,
        width: 1.0,
        color: rgba(rim, 0xFF),
    });
    prims.push(DrawPrim::ClipPush { x, y, w, h });
    // The rainbow arch, cresting inside the banner from its bottom edge.
    let stripes = [
        BLOB_CORAL,
        BLOB_MARIGOLD,
        BLOB_LEAF,
        RAINBOW_TEAL,
        BLOB_COBALT,
        BLOB_VIOLET,
    ];
    let (acx, acy) = (x + w * 0.5, y + h * 1.12);
    let r0 = h * 0.58;
    let st = (h * 0.055).max(3.0);
    for (i, c) in stripes.iter().enumerate() {
        let r = (r0 + (stripes.len() - 1 - i) as f32 * st).max(1.0);
        let step = ((st * 0.8) / (std::f32::consts::PI * r)).max(1.0 / 512.0);
        let mut a = 0.0f32;
        while a <= 1.0 {
            let th = std::f32::consts::PI * (1.0 + a);
            prims.push(DrawPrim::Dot {
                cx: acx + th.cos() * r,
                cy: acy + th.sin() * r,
                r: st * 0.62,
                color: rgba(*c, arch_alpha),
                breathe: false,
            });
            a += step;
        }
    }
    // A small static constellation in the sky, both sides of the arch.
    let glints: [(f32, f32, f32, [u8; 3]); 6] = [
        (0.070, 0.30, 0.16, BLOB_MARIGOLD),
        (0.155, 0.62, 0.11, BLOB_VIOLET),
        (0.330, 0.24, 0.13, RAINBOW_TEAL),
        (0.660, 0.26, 0.12, BLOB_CORAL),
        (0.845, 0.58, 0.11, BLOB_LEAF),
        (0.930, 0.28, 0.15, BLOB_VIOLET),
    ];
    for (fx, fy, sc, c) in glints {
        let (sx, sy) = (x + fx * w, y + fy * h);
        let len = h * sc;
        prims.push(DrawPrim::Stroke {
            x: sx - len,
            y: sy - 0.75,
            w: len * 2.0,
            h: 1.5,
            radius: 0.75,
            width: 1.5,
            color: rgba(c, glint_alpha),
        });
        prims.push(DrawPrim::Stroke {
            x: sx - 0.75,
            y: sy - len,
            w: 1.5,
            h: len * 2.0,
            radius: 0.75,
            width: 1.5,
            color: rgba(c, glint_alpha),
        });
    }
    prims.push(DrawPrim::ClipPop);
}

/// The §L.4 kitty cameo painter: a small vector cat built from the EXISTING prim
/// vocabulary (Dots + hairline Strokes — no new raster op). Breeds 0..6 pop from
/// behind `y_base` (rise → wiggle → sink, with a late alpha fade for hosts with
/// no occluder); breed 6 is the rainbow FLYBY, travelling the whole band with a
/// five-stripe wake — the rainbow kitty cursor-trail homage.
// One free-position painter, two hosts: the geometry really is seven scalars
// (band, base, peak, head radius) — a one-off struct would just rename them.
#[allow(clippy::too_many_arguments)]
fn paint_kitty_cameo(
    prims: &mut Vec<DrawPrim>,
    k: &KittyPop,
    phase: u32,
    band_x: f32,
    band_w: f32,
    y_base: f32,
    peak: f32,
    r_head: f32,
) {
    let t = (phase.wrapping_sub(k.start) as f32 / KITTY_POP_TICKS as f32).clamp(0.0, 1.0);
    let alpha = if t > 0.88 {
        ((1.0 - t) / 0.12).clamp(0.0, 1.0)
    } else {
        1.0
    };
    let a = (alpha * 255.0) as u8;
    if a == 0 {
        return;
    }
    let r = r_head;
    let (coat, face): ([u8; 3], [u8; 3]) = match k.breed {
        1 => ([0xFF, 0xCF, 0x9E], [0x8C, 0x2F, 0x2F]), // orange tabby
        2 => ([0xC9, 0xC9, 0xC9], [0x8C, 0x2F, 0x2F]), // gray
        3 => ([0x3D, 0x3D, 0x3D], [0xFF, 0xD2, 0x5C]), // black, golden eyes
        5 => (LANDING_BUBBLE, [0x8C, 0x2F, 0x2F]),     // magic green
        // 0 white · 4 calico base · 6 rainbow flyby (the wake carries the colour)
        _ => ([0xFF, 0xFF, 0xFF], [0x8C, 0x2F, 0x2F]),
    };
    let (cx, cy);
    if k.breed == 6 {
        // FLYBY: cross the band left→right, bobbing, the wake growing behind.
        cx = band_x - r + (band_w + 2.0 * r) * t;
        cy = y_base - peak - (t * 12.6).sin().abs() * r * 0.35;
        let stripes = [
            BLOB_CORAL,
            BLOB_MARIGOLD,
            BLOB_LEAF,
            BLOB_COBALT,
            BLOB_VIOLET,
        ];
        let sh = (r * 0.16).max(1.5);
        let trail_w = (cx - r * 0.8 - band_x).max(0.0);
        if trail_w > 0.5 {
            for (i, c) in stripes.iter().enumerate() {
                prims.push(DrawPrim::Stroke {
                    x: band_x,
                    y: cy - sh * 2.5 + i as f32 * sh,
                    w: trail_w,
                    h: sh,
                    radius: 0.0,
                    width: sh,
                    color: rgba(*c, (f32::from(a) * 0.85) as u8),
                });
            }
        }
    } else {
        // POP: rise from behind the host, hold with a soft wiggle, sink back.
        let rise = if t < 0.16 {
            let u = t / 0.16;
            u * (2.0 - u)
        } else if t > 0.84 {
            let u = ((1.0 - t) / 0.16).clamp(0.0, 1.0);
            u * (2.0 - u)
        } else {
            1.0 + (t * 40.0).sin() * 0.04
        };
        cx = band_x + k.x_frac * band_w;
        cy = y_base - peak * rise;
    }
    // Ears (behind the head), head, calico patches, eyes, mouth — in paint order.
    let (le, re) = if k.breed == 4 {
        ([0xFF, 0x9D, 0x5C], [0x4A, 0x4A, 0x4A]) // calico's mismatched ears
    } else {
        (coat, coat)
    };
    for (ex, ec) in [(-1.0_f32, le), (1.0_f32, re)] {
        prims.push(DrawPrim::Dot {
            cx: cx + ex * r * 0.62,
            cy: cy - r * 0.72,
            r: r * 0.40,
            color: rgba(ec, a),
            breathe: false,
        });
    }
    prims.push(DrawPrim::Dot {
        cx,
        cy,
        r,
        color: rgba(coat, a),
        breathe: false,
    });
    if k.breed == 4 {
        prims.push(DrawPrim::Dot {
            cx: cx - r * 0.32,
            cy: cy - r * 0.30,
            r: r * 0.34,
            color: rgba([0xFF, 0x9D, 0x5C], a),
            breathe: false,
        });
        prims.push(DrawPrim::Dot {
            cx: cx + r * 0.34,
            cy: cy - r * 0.26,
            r: r * 0.30,
            color: rgba([0x4A, 0x4A, 0x4A], a),
            breathe: false,
        });
    }
    for ex in [-1.0_f32, 1.0_f32] {
        prims.push(DrawPrim::Dot {
            cx: cx + ex * r * 0.36,
            cy: cy - r * 0.06,
            r: (r * 0.13).max(1.0),
            color: rgba(face, a),
            breathe: false,
        });
    }
    prims.push(DrawPrim::Stroke {
        x: cx - r * 0.20,
        y: cy + r * 0.30,
        w: r * 0.40,
        h: 1.0,
        radius: 0.0,
        width: 1.0,
        color: rgba(face, a),
    });
}

// ---- Group-box layout (design §3.2) ------------------------------------------------

/// One laid-out row of the grouped content pane — the straight evolution of
/// [`BodyRow`]: a group caption, a control (2 cells tall), a footnote under a box, or
/// the 1-row gap between groups. The SINGLE layout unit shared by the painter, the
/// mouse hit-test, the wheel clamp, and the keyboard scroll — they can never diverge.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum GroupRow {
    Caption(&'static str),
    Control(usize),
    Footnote(&'static str),
    Gap,
}

/// A [`GroupRow`]'s height in cells: controls are 2-cell macOS-density rows (≈ 32 pt);
/// chrome rows are 1.
pub(crate) fn group_row_cells(r: &GroupRow) -> usize {
    match r {
        GroupRow::Control(_) => 2,
        _ => 1,
    }
}

/// Max footnote CHARACTERS per row at `cols` — the wrap width every
/// [`category_layout`] caller must share (design §3.2 "wrapped to the box
/// width"), so the painter, the mouse hit-test, the scroll clamps, and the
/// keyboard walk agree on where each row sits. Derived from the same §3.2 box
/// arithmetic the painter uses, in cells: box = content pane minus the 2·cw /
/// 2.5·cw margins, text inset 0.6·cw each side. Footnote glyphs run at 0.78·px
/// on the 0.6-em [`text_w`] metric while a grid cell is ≈0.6 em of the terminal
/// font (≈1.28 chars per cell); 1.2 keeps a safety margin — and the painter
/// additionally elides, so a metric drift can never paint past the box.
pub(crate) fn footnote_wrap_chars(cols: usize) -> usize {
    let pg = pane_geom_cells(cols, 2); // the sidebar width only needs `cols`
    let avail_cells = cols as f32 - pg.sidebar_w_cells - 4.5 - 1.2;
    ((avail_cells * 1.2).max(8.0)) as usize
}

/// Split a footnote into AT MOST two rows of ≈`wrap` characters, breaking at a
/// space (footnotes are 1-2 short sentences — design §3.2). Returns the first
/// row and the optional remainder; a remainder still wider than `wrap` is left
/// whole for the painter to elide (2 rows is the cap, by design). Slices of the
/// `'static` footnote table, so [`GroupRow::Footnote`] stays `Copy + 'static`.
fn wrap_footnote(note: &'static str, wrap: usize) -> (&'static str, Option<&'static str>) {
    if wrap == 0 || note.chars().count() <= wrap {
        return (note, None);
    }
    // The last space within the first `wrap` characters (byte index for slicing).
    let mut break_at = None;
    for (n, (i, c)) in note.char_indices().enumerate() {
        if n > wrap {
            break;
        }
        if c == ' ' {
            break_at = Some(i);
        }
    }
    match break_at {
        Some(i) => (note[..i].trim_end(), Some(note[i + 1..].trim_start())),
        None => (note, None), // one unbreakable run — the painter elides it
    }
}

/// The FULL (unscrolled) grouped layout of one category: its fields ordered by
/// (`prefs::group_of` order, build order), with a caption opening each group and its
/// footnote (wrapped to ≤2 rows of ≤`wrap` chars — design §3.2) + a gap closing it.
/// PURE — the single source of truth for the content pane; every caller must pass
/// the SAME `wrap` ([`footnote_wrap_chars`] of the card's `cols`) or the painter
/// and the scroll/hit-test math would disagree about row offsets.
pub(crate) fn category_layout(
    fields: &[EditField],
    category: prefs::Section,
    wrap: usize,
) -> Vec<GroupRow> {
    let mut idxs: Vec<usize> = (0..fields.len())
        .filter(|&i| prefs::section_of(fields[i].key) == category)
        .collect();
    idxs.sort_by_key(|&i| (prefs::group_of(fields[i].key).1, i));
    let mut out = Vec::with_capacity(idxs.len() + 8);
    let push_footnote = |out: &mut Vec<GroupRow>, note: &'static str| {
        let (first, rest) = wrap_footnote(note, wrap);
        out.push(GroupRow::Footnote(first));
        if let Some(rest) = rest {
            out.push(GroupRow::Footnote(rest));
        }
    };
    let mut cur: Option<&'static str> = None;
    for &i in &idxs {
        let (caption, _) = prefs::group_of(fields[i].key);
        if cur != Some(caption) {
            if let Some(prev) = cur {
                if let Some(note) = prefs::group_footnote(prev) {
                    push_footnote(&mut out, note);
                }
                out.push(GroupRow::Gap);
            }
            out.push(GroupRow::Caption(caption));
            cur = Some(caption);
        }
        out.push(GroupRow::Control(i));
    }
    if let Some(prev) = cur
        && let Some(note) = prefs::group_footnote(prev)
    {
        push_footnote(&mut out, note);
    }
    out
}

/// The category's control field indices in LAID-OUT order — the keyboard's ↑/↓ walk.
/// Footnote wrapping never adds/moves controls, so no wrap width is needed here.
pub(crate) fn category_controls(fields: &[EditField], category: prefs::Section) -> Vec<usize> {
    category_layout(fields, category, usize::MAX)
        .into_iter()
        .filter_map(|r| match r {
            GroupRow::Control(i) => Some(i),
            _ => None,
        })
        .collect()
}

/// Whether row `target` fits FULLY inside a `band`-cell window starting at `scroll` —
/// the painter stops at the first partial row, so "fully visible" is the shared test.
fn group_row_fully_visible(rows: &[GroupRow], scroll: usize, band: usize, target: usize) -> bool {
    let mut cells = 0usize;
    for (i, r) in rows.iter().enumerate().skip(scroll) {
        let h = group_row_cells(r);
        if i == target {
            return cells + h <= band;
        }
        cells += h;
        if cells >= band {
            return false;
        }
    }
    false
}

/// The largest useful grouped `scroll`: the first [`GroupRow`] index at which the tail
/// fits in the `band`-cell window (0 when everything fits) — wheel scrolling stops once
/// the last footnote is on screen.
pub(crate) fn max_group_scroll(rows: &[GroupRow], band: usize) -> usize {
    if band == 0 || rows.is_empty() {
        return 0;
    }
    let mut tail: usize = rows.iter().map(group_row_cells).sum();
    for (i, r) in rows.iter().enumerate() {
        if tail <= band {
            return i;
        }
        tail -= group_row_cells(r);
    }
    rows.len() - 1
}

/// Map a cell offset within the group band to the [`GroupRow`] painted there (walking
/// the same accumulate-until-overflow the painter uses). `None` on the gap past the
/// last painted row or outside the band.
pub(crate) fn group_row_at(
    rows: &[GroupRow],
    scroll: usize,
    band: usize,
    rel_cell: usize,
) -> Option<GroupRow> {
    if rel_cell >= band {
        return None;
    }
    let mut cells = 0usize;
    for r in rows.iter().skip(scroll) {
        let h = group_row_cells(r);
        if cells + h > band {
            return None; // the painter stops at the first partial row too
        }
        if rel_cell < cells + h {
            return Some(*r);
        }
        cells += h;
    }
    None
}

/// The right edge widgets right-align to inside a group box (and the flat search list):
/// the box right (`W − 2.5·cw`, design §3.2) minus the 1.2·cw inset. Shared by the
/// painter, [`widget_hit_left`], and [`menu_geom`] so hit == pixels.
pub(crate) fn content_v_right(g: &SettingsGeom) -> f32 {
    g.cols as f32 * g.cw - g.cw * 2.5 - g.cw * 1.2
}

/// The left FLOOR a right-aligned widget may grow to: the group box's left edge
/// (`sidebar + 2·cw`) plus the caption inset. A wide popup chip / segmented control
/// clamps here instead of escaping across the sidebar seam — the pre-v2 `cw*6`
/// floor predates the two-pane layout. Shared by the painter and
/// [`widget_hit_left`] so hit == pixels.
pub(crate) fn content_v_left(g: &SettingsGeom) -> f32 {
    pane_geom(g).content_x(g.cw) + g.cw * 2.6
}

/// Which preview arm the card paints for a focused key — serialized on the
/// `preview kind=` introspection line (graft #3) so a driver can assert the card's
/// subject without pixel-diffing.
pub(crate) fn preview_kind(key: &str) -> &'static str {
    match key {
        prefs::EDIT_THEME => "theme",
        prefs::EDIT_WINDOW_THEME => "window_theme",
        prefs::EDIT_FOREGROUND => "foreground",
        prefs::EDIT_BACKGROUND => "background",
        prefs::EDIT_SELECTION_COLOR => "selection",
        prefs::EDIT_CURSOR_COLOR | prefs::EDIT_CURSOR_STYLE | prefs::EDIT_CURSOR_BLINK => "cursor",
        prefs::EDIT_CURSOR_TRAIL | prefs::EDIT_CURSOR_TRAIL_STYLE | prefs::EDIT_CURSOR_TRAIL_MS => {
            "trail"
        }
        prefs::EDIT_FONT_FAMILY | prefs::EDIT_FONT_PX | prefs::EDIT_LIGATURES => "font",
        // W5: the visual typography knobs drive the sample's row spacing and its
        // selected-text ink/contrast, so the pinned preview card reacts to them
        // (a change re-renders the terminal live behind the card — that IS the preview).
        prefs::EDIT_LINE_HEIGHT => "line_height",
        prefs::EDIT_MINIMUM_CONTRAST => "minimum_contrast",
        _ => "default",
    }
}

/// Which sample ELEMENT the focused colour/appearance control drives — used to draw an
/// `accent` outline around exactly the thing that changes, so "what does this control?" is
/// answered at a glance (design §4 "the driven element in the preview peek gets an accent
/// outline"). `None` ⇒ the whole sample updates (theme / cursor / font / ligatures).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Driven {
    Background,
    Foreground,
    Cursor,
    Selection,
    /// The mock's titlebar strip — driven by `window_theme` (graft #2).
    Titlebar,
}

fn driven_element(key: &str) -> Option<Driven> {
    match key {
        prefs::EDIT_BACKGROUND => Some(Driven::Background),
        prefs::EDIT_FOREGROUND => Some(Driven::Foreground),
        prefs::EDIT_CURSOR_COLOR
        | prefs::EDIT_CURSOR_STYLE
        | prefs::EDIT_CURSOR_BLINK
        | prefs::EDIT_CURSOR_TRAIL
        | prefs::EDIT_CURSOR_TRAIL_STYLE => Some(Driven::Cursor),
        prefs::EDIT_SELECTION_COLOR => Some(Driven::Selection),
        prefs::EDIT_WINDOW_THEME => Some(Driven::Titlebar),
        _ => None,
    }
}

/// The style the preview card's animated DEMO lane plays, or `None` when the demo
/// is idle: the HIGHLIGHTED (uncommitted) option while the "Trail effect" popup is
/// open — browsing the menu live-demos each look (design graft #1) — else the
/// EFFECTIVE style while the trail toggle / trail-effect row is focused. ONE pure
/// predicate shared by the painter's lane and the `next_demo_tick` arming in
/// `main.rs` (`settings_demo_active`), so the tick and the pixels can't disagree.
pub(crate) fn demo_style(state: &SettingsState) -> Option<&str> {
    // The open trail menu first: the highlight demos even from the filtered flat
    // list (the card rests on the default mock there, but the lane still plays).
    if let Some(m) = &state.menu
        && state
            .fields
            .get(m.field)
            .is_some_and(|f| f.key == prefs::EDIT_CURSOR_TRAIL_STYLE)
    {
        return m.options.get(m.highlighted).map(String::as_str);
    }
    if state.filtering() {
        return None; // the resting default mock has no focused subject
    }
    let f = state.action_target().and_then(|i| state.fields.get(i))?;
    match f.key {
        prefs::EDIT_CURSOR_TRAIL | prefs::EDIT_CURSOR_TRAIL_STYLE => state
            .fields
            .iter()
            .find(|g| g.key == prefs::EDIT_CURSOR_TRAIL_STYLE)
            .map(enum_current),
        _ => None,
    }
}

/// Paint the PINNED PREVIEW CARD (design §5): the fixed rows-3..12 band of the content
/// pane, present in EVERY category at all times — a mini aterm window (titlebar strip
/// with traffic lights + title, terminal body sample) rendered in the CURRENT theme +
/// settings. The titlebar tints by the resolved `window_theme`; `auto` paints a
/// vertical half-split whose LEADING half is the truthful system appearance
/// ([`PreviewCtx::system_dark`], graft #2). Pure [`DrawPrim`] — colours come off the
/// live [`Theme`], so scrubbing a control updates the card WYSIWYG. The element the
/// focused control drives gets an `accent` outline; while the search filter is active
/// the card rests on the default mock (`focused` is `None`). The card RECT is fixed by
/// the caller — focus changes only interior pixels.
#[allow(clippy::too_many_arguments)]
fn preview_card(
    prims: &mut Vec<DrawPrim>,
    state: &SettingsState,
    r: &Roles,
    theme: Theme,
    ctx: PreviewCtx,
    x_left: f32,
    x_right: f32,
    y_top: f32,
    y_bot: f32,
    cw: f32,
    ch: f32,
    px: f32,
) {
    let focused = if state.filtering() {
        None
    } else {
        state.action_target().and_then(|i| state.fields.get(i))
    };
    // Menu-highlight preview (graft #2): while the THEME menu is open, the mock
    // re-tints to the HIGHLIGHTED option (uncommitted, via the builtin-scheme path) —
    // scrubbing the list previews live without touching the file. A preserved custom
    // entry has no builtin scheme, so it falls through to the live theme.
    let menu_theme_name = state
        .menu
        .as_ref()
        .filter(|m| {
            state
                .fields
                .get(m.field)
                .is_some_and(|f| matches!(f.kind, EditKind::Theme))
        })
        .and_then(|m| m.options.get(m.highlighted));
    let theme = menu_theme_name
        .and_then(|name| aterm_types::scheme::builtin(name))
        .map_or(theme, |s| {
            let p = s.to_theme_parts();
            Theme {
                fg: p.fg,
                bg: p.bg,
                cursor: p.cursor,
                selection: p.selection,
            }
        });
    // Colour-wheel candidate (design §5.4): while the wheel is open, the mock
    // renders the WORKING (uncommitted) colour on the wheel row's driven element —
    // a scrub is seen in context before anything persists. Same substitution seam
    // as the theme-menu re-tint above: swap the theme part, let the pure mock paint.
    let theme = match &state.wheel {
        Some(w) => {
            let c = crate::widget::hsv_to_rgb(w.h, w.s, w.v);
            let cu = (u32::from(c[0]) << 16) | (u32::from(c[1]) << 8) | u32::from(c[2]);
            match state.fields.get(w.field).map(|f| f.key) {
                Some(prefs::EDIT_FOREGROUND) => Theme { fg: cu, ..theme },
                Some(prefs::EDIT_BACKGROUND) => Theme { bg: cu, ..theme },
                Some(prefs::EDIT_CURSOR_COLOR) => Theme {
                    cursor: cu,
                    ..theme
                },
                Some(prefs::EDIT_SELECTION_COLOR) => Theme {
                    selection: cu,
                    ..theme
                },
                _ => theme,
            }
        }
        None => theme,
    };
    let gx = x_left;
    let gw = x_right - x_left;
    let top = y_top + cw * 0.25;
    let gh = (y_bot - cw * 0.25) - top;
    // Degenerate retired-card band: draw nothing rather than a
    // zero/negative-size panel (`fit` keeps every length non-negative regardless).
    if gw <= cw * 2.0 || gh <= ch * 2.0 {
        return;
    }

    // Card container (an elevated group box, same corner idiom as the boxes below).
    prims.push(DrawPrim::Panel {
        x: gx,
        y: top,
        w: gw,
        h: gh,
        radius: fit(ch * 0.4, 0.0, 12.0),
        fill: rgba(r.elevated, 0xFF),
        blur: false,
    });
    // "PREVIEW" caption (secondary, uppercase, no faux tracking — §2).
    // Section caption in the native UI face (theirs), sized off the type scale (ours).
    let cap_size = TypeStep::Caption.px(px);
    prims.push(text_prim(
        gx + cw * 0.5,
        top + cw * 0.35 + cap_size.get(),
        "PREVIEW".to_string(),
        cap_size,
        TextWeight::Regular,
        TextFace::Ui,
        rgba(r.text_secondary, 0xFF),
    ));

    // Live values that the sample reflects (from the model, not just the focused row).
    let cursor_style = state
        .fields
        .iter()
        .find(|f| f.key == prefs::EDIT_CURSOR_STYLE)
        .map_or("block", |f| enum_current(f));
    let ligatures_on = state
        .fields
        .iter()
        .find(|f| f.key == prefs::EDIT_LIGATURES)
        .is_none_or(bool_on);
    let drives = focused.and_then(|f| driven_element(f.key));

    let bg = u32_rgb(theme.bg);
    let fg = u32_rgb(theme.fg);
    let cur = u32_rgb(theme.cursor);
    let sel = u32_rgb(theme.selection);
    let dim = lerp_rgb(fg, bg, 0.45);

    // The mini aterm window inside the card.
    let spad = cw * 0.6;
    let sx = gx + spad;
    let sw = gw - 2.0 * spad;
    let sy = top + px * 1.1 + cw * 0.4;
    let sh = fit(gh * 0.66, ch * 2.0, gh - (px * 1.1 + cw));
    let mock_r = fit(ch * 0.3, 0.0, 10.0);
    prims.push(DrawPrim::Panel {
        x: sx,
        y: sy,
        w: sw,
        h: sh,
        radius: mock_r,
        fill: rgba(bg, 0xFF),
        blur: false,
    });

    // TITLEBAR STRIP, tinted by the resolved `window_theme`. Chrome tones derive from
    // pure white/black anchored toward the live theme (design §5.3 — not Apple hex).
    let tb_h = fit(ch * 0.8, 6.0, sh * 0.3);
    let light_chrome = lerp_rgb([255, 255, 255], fg, 0.08);
    let dark_chrome = lerp_rgb([0, 0, 0], bg, 0.15);
    let wt = state
        .fields
        .iter()
        .find(|f| f.key == prefs::EDIT_WINDOW_THEME)
        .map_or("auto", |f| enum_current(f));
    let titlebar = |prims: &mut Vec<DrawPrim>, chrome: [u8; 3]| {
        prims.push(DrawPrim::Panel {
            x: sx,
            y: sy,
            w: sw,
            h: tb_h,
            radius: mock_r,
            fill: rgba(chrome, 0xFF),
            blur: false,
        });
    };
    let lead_chrome = match wt {
        "light" => {
            titlebar(prims, light_chrome);
            light_chrome
        }
        "dark" => {
            titlebar(prims, dark_chrome);
            dark_chrome
        }
        // `auto`: a vertical split (ClipPush is rectangular — graft #2). The LEADING
        // side is what auto RESOLVES to right now (the truthful system appearance) and
        // DOMINATES at 72%; the trailing 28% shows the other mode ("follows the
        // system"). A hairline seam marks the split as intent — the borderless 50/50
        // used to read as a rendering hole, not a mode preview.
        _ => {
            let (near, far) = if ctx.system_dark {
                (dark_chrome, light_chrome)
            } else {
                (light_chrome, dark_chrome)
            };
            let split = sw * 0.72;
            titlebar(prims, near);
            prims.push(DrawPrim::ClipPush {
                x: sx + split,
                y: sy,
                w: sw - split,
                h: tb_h,
            });
            titlebar(prims, far);
            prims.push(DrawPrim::ClipPop);
            prims.push(DrawPrim::Stroke {
                x: sx + split - 0.5,
                y: sy,
                w: 1.0,
                h: tb_h,
                radius: 0.0,
                width: 1.0,
                color: rgba(lerp_rgb(near, far, 0.5), 0xAA),
            });
            near
        }
    };
    // Traffic-light dots in the strip (theme-derived, never Apple hex).
    let dr = fit(tb_h * 0.22, 1.5, 5.0);
    let mut dcx = sx + cw * 0.6;
    for c in [r.danger, cur, r.success] {
        prims.push(DrawPrim::Dot {
            cx: dcx,
            cy: sy + tb_h * 0.5,
            r: dr,
            color: rgba(c, 0xFF),
            breathe: false,
        });
        dcx += dr * 2.6;
    }
    // Window title, contrast-picked against the leading chrome tone. Sized off the
    // type scale (Caption), clamped to fit the titlebar strip like the sample below.
    let title_step = TypeStep::Caption.px_clamped(px, 5.0, tb_h * 0.75);
    let title_px = title_step.get();
    let title = "aterm";
    let title_color = if luma(lead_chrome) > 150.0 {
        lerp_rgb(lead_chrome, [0, 0, 0], 0.7)
    } else {
        lerp_rgb(lead_chrome, [255, 255, 255], 0.7)
    };
    // UI face: a real macOS titlebar IS the system font (the terminal body below
    // stays mono — it depicts a terminal).
    prims.push(text_prim(
        sx + (sw - ui_text_width(title, title_px)) * 0.5,
        row_baseline(sy, tb_h, title_px),
        title.to_string(),
        title_step,
        TextWeight::Regular,
        TextFace::Ui,
        rgba(title_color, 0xFF),
    ));

    // Sample text lines (the ONLY text in the terminal font). Kept ASCII so no glyph is
    // font-roulette. `font_px` scrubs the sample size; the chrome around it never reflows.
    let ss_step = TypeStep::Caption.px_clamped(px, 6.0, sh * 0.2);
    let ss = ss_step.get();
    let lh = ss * 1.5;
    let tx = sx + cw * 0.6;
    let line_top = sy + tb_h + ss * 0.5;
    // `y` is passed explicitly (a closure that captured a running `ty` would borrow it for
    // the whole scope, blocking the between-line reads the outline math needs). The runs
    // of one line are all one size, sharing the em-bottom baseline `y + ss` (the cursor /
    // selection geometry below is keyed off the same line tops).
    let push_line = |prims: &mut Vec<DrawPrim>, y: f32, runs: &[(&str, [u8; 3])]| {
        let mut x = tx;
        for (s, color) in runs {
            prims.push(text_prim(
                x,
                y + ss,
                (*s).to_string(),
                ss_step,
                TextWeight::Regular,
                TextFace::Mono,
                rgba(*color, 0xFF),
            ));
            x += text_w(s, ss);
        }
    };
    // Prompt line, with the cursor's colour on the sigil.
    push_line(
        prims,
        line_top,
        &[("~/aterm ", dim), ("$ ", cur), ("cargo run", fg)],
    );
    // A selection-highlighted line: draw the selection wash, then the word over it.
    let sel_word = "aterm";
    let sel_x = tx + text_w("   Running ", ss);
    let sel_row_y = line_top + lh;
    prims.push(DrawPrim::Panel {
        x: sel_x - ss * 0.1,
        y: sel_row_y - ss * 0.1,
        w: text_w(sel_word, ss) + ss * 0.2,
        h: ss * 1.2,
        radius: ss * 0.15,
        fill: rgba(sel, 0xCC),
        blur: false,
    });
    push_line(prims, sel_row_y, &[("   Running ", dim), (sel_word, fg)]);
    // A code line.
    push_line(
        prims,
        line_top + lh * 2.0,
        &[("fn ", cur), ("main", fg), ("() { .. }", dim)],
    );
    // A ligature line: shows the joined forms when ON, spaced when OFF, so toggling reads.
    let liga = if ligatures_on {
        "=> -> != >= |>"
    } else {
        "=  >  -  >  !  ="
    };
    let liga_row_y = line_top + lh * 3.0;
    push_line(prims, liga_row_y, &[(liga, fg)]);

    // The cursor block, at the end of the prompt line (past the whole `~/aterm $ cargo run`
    // run, not just the command). Its SHAPE reflects `cursor_style`.
    let cur_cx = tx + text_w("~/aterm $ cargo run", ss);
    let cur_top = line_top;
    let cell_w = ss * 0.6;
    let (cxr, cyr, cwr, chr) = match cursor_style {
        "underline" => (
            cur_cx,
            cur_top + ss * 0.95,
            cell_w,
            fit(ss * 0.12, 1.5, 4.0),
        ),
        "bar" => (cur_cx, cur_top, fit(ss * 0.12, 1.5, 4.0), ss),
        _ => (cur_cx, cur_top, cell_w, ss),
    };
    prims.push(DrawPrim::Panel {
        x: cxr,
        y: cyr,
        w: cwr,
        h: chr,
        radius: 0.0,
        fill: rgba(cur, 0xFF),
        blur: false,
    });

    // ---- Animated cursor-effect DEMO (the one sanctioned animation) ----
    // While the "Trail effect" MENU is open, a mini wake sweeps a lane under the
    // sample lines playing the HIGHLIGHTED (uncommitted) option — browsing the
    // menu live-demos each look (graft #1); with the menu closed, focusing the
    // trail toggle / trail-effect row demos the effective style. Comet + style
    // particles are coloured by the SAME ramps as the live animator
    // (`style_comet_color` / `style_particle_color`), with a slow intensity pulse
    // demoing the typing-speed heat ramp. Pure function of `state.demo_phase`
    // (bumped ~30fps by the event loop while [`demo_style`] is live — see
    // `next_demo_tick` in `main.rs`).
    let lane_y = liga_row_y + lh;
    if let Some(style_raw) = demo_style(state)
        && !style_raw.trim().eq_ignore_ascii_case("off")
        && lane_y + ss * 1.2 < sy + sh
    {
        use crate::cursor_glow::{
            BEAM_DEFAULT_COLOR, COMET_DEFAULT_COLOR, GlowStyle, LASER_DEFAULT_COLOR,
            style_comet_color, style_particle_color,
        };
        let style = GlowStyle::parse(style_raw.trim());
        let t = state.demo_phase as f32 / 30.0; // seconds at the 30fps demo tick
        // Colour resolution mirrors the live `glow_config` EXACTLY, including the
        // configured overrides (PreviewCtx carries them): an explicit
        // `cursor_trail_color` wins over every per-style default; otherwise
        // Laser mirrors the live default ELECTRIC YELLOW (not the theme cursor),
        // Beam its PHOTON ICE-BLUE, and the comet its GLACIAL BLUE.
        let base = ctx.trail_color.unwrap_or(match style {
            GlowStyle::Laser => LASER_DEFAULT_COLOR,
            GlowStyle::Beam => BEAM_DEFAULT_COLOR,
            GlowStyle::Comet => COMET_DEFAULT_COLOR,
            _ => theme.cursor & 0x00FF_FFFF,
        });
        let brighten = |c: u32| -> u32 {
            let m = |sh: u32| ((((c >> sh) & 0xff) as f32) * 1.5).min(255.0) as u32;
            (m(16) << 16) | (m(8) << 8) | m(0)
        };
        // Accent defaults like `glow_config`: an explicit `cursor_trail_accent`,
        // else the base colour brightened ~1.5×.
        let accent = ctx.trail_accent.unwrap_or_else(|| brighten(base));
        let lane_x0 = tx;
        let lane_w = (sx + sw - cw * 0.6) - tx;
        let frac = (t / 2.4).fract();
        let head_x = lane_x0 + frac * lane_w;
        let head_y = lane_y + ss * 0.5;
        // The heat pulse: brightness swells and cools like sustained fast typing.
        let pulse = 0.45 + 0.55 * (0.5 + 0.5 * (t * 2.6).sin());
        // Phaser cycles hue at 2× (mirrors the live animator's doubled step).
        let hue_rate = if matches!(style, GlowStyle::Phaser) {
            0.30
        } else {
            0.15
        };
        let hue = (t * hue_rate).fract();
        // The swept streak. Water alone draws NO beam (WATER-1) — it is droplets
        // only — so the preview must not paint one either or it drifts from the
        // engine (WATER-3). `comet` keeps a streak here as a stand-in for its
        // opaque `CursorTrail` body (which this preview doesn't separately
        // simulate); every additive beam style keeps its beam.
        let show_streak = !matches!(style, GlowStyle::Water);
        let seg_dx = ss * 0.55;
        // Phaser previews FAT — mirroring the live beam's near-cell-height core
        // so the demo lane is honest about the textbox-height bar it selects.
        // Beam previews as its solid ~1/3-cell TUBE, honest about the rod.
        let streak_h = match style {
            GlowStyle::Phaser => ss * 1.0,
            GlowStyle::Beam => ss * 0.45,
            _ => ss * 0.32,
        };
        for i in 0..12 {
            if !show_streak {
                break;
            }
            let x = head_x - i as f32 * seg_dx;
            if x < lane_x0 {
                break;
            }
            let pos = 1.0 - i as f32 / 12.0;
            let color = style_comet_color(style, base, accent, hue, pos);
            // Beam mirrors its near-constant power profile (tail still ~70%) —
            // a steady rod, not a bright head with a wispy tail.
            let a = if matches!(style, GlowStyle::Beam) {
                ((165.0 + 70.0 * pos) * pulse).min(255.0) as u8
            } else {
                ((40.0 + 175.0 * pos) * pulse).min(255.0) as u8
            };
            prims.push(DrawPrim::Panel {
                x: x - seg_dx * 0.5,
                y: head_y - streak_h * 0.5,
                w: seg_dx,
                h: streak_h,
                radius: streak_h * 0.5,
                fill: rgba(u32_rgb(color), a),
                blur: false,
            });
        }
        // Style particles around the head (sparks / embers / droplets / laser
        // ablation sparks / beam stardust).
        if matches!(
            style,
            GlowStyle::Sparkle
                | GlowStyle::Fire
                | GlowStyle::Water
                | GlowStyle::Laser
                | GlowStyle::Beam
                | GlowStyle::Comet
        ) {
            for j in 0..6u32 {
                let jf = j as f32;
                let life = ((t * (1.15 + 0.13 * jf)) + jf * 0.37).fract();
                let fade = 1.0 - life;
                let ang = jf * 1.256 + t * 0.9;
                let (dx, dy) = match style {
                    // Embers rise; sparks fly radially. Water mirrors the live
                    // art's three populations — BUBBLES rising out of the wake
                    // behind the head, DRIPS sagging then falling below the
                    // lane, SPRAY arcing out and over — the same motion
                    // signatures as the engine (`aterm-effects::cursor_glow`).
                    GlowStyle::Fire => ((jf - 2.5) * ss * 0.22, -life * ss * 1.3),
                    // Laser ablation sparks: a hard sideways ejection off the
                    // impact point, arcing down under gravity — the engine's
                    // grinder-spark signature.
                    GlowStyle::Laser => (
                        (if j % 2 == 0 { 1.0 } else { -1.0 }) * (0.3 + life * 1.6) * ss,
                        (-0.9 * life + 1.9 * life * life) * ss,
                    ),
                    GlowStyle::Water => match j % 3 {
                        0 => (-(0.5 + life * 1.6) * ss, -(0.2 + life * 0.7) * ss),
                        1 => ((jf - 2.5) * ss * 0.12, (0.15 + 1.6 * life * life) * ss),
                        _ => (
                            (if j % 2 == 0 { 1.0 } else { -1.0 }) * life * ss * 1.3,
                            -(0.9 * life - 1.4 * life * life) * ss * 1.5,
                        ),
                    },
                    // Stardust hangs WEIGHTLESS in the wake behind the head —
                    // a slow rearward drift with a gentle bob, mirroring the
                    // engine's zero-gravity motes.
                    GlowStyle::Beam => (
                        -(0.3 + life * 1.5) * ss,
                        (jf * 2.1 + t * 1.3).sin() * ss * 0.28,
                    ),
                    // Comet debris HANGS behind the head: shed grains drift back
                    // along the tail with barely any fall — the engine's
                    // dust-settles signature (`gy` ≈ 0.25 cell there).
                    GlowStyle::Comet => (
                        -(0.4 + life * 1.5) * ss,
                        ((jf - 2.5) * 0.1 + life * 0.3) * ss,
                    ),
                    _ => (ang.cos() * life * ss * 1.4, ang.sin() * life * ss * 1.1),
                };
                let (dot_x, dot_y) = (head_x + dx - jf * ss * 0.1, head_y + dy);
                if dot_x < lane_x0
                    || dot_x > lane_x0 + lane_w
                    || dot_y < sy + tb_h + ss * 0.2
                    || dot_y > sy + sh - ss * 0.2
                {
                    continue;
                }
                let color = style_particle_color(style, base, (0.13 * jf + hue).fract(), fade);
                if matches!(style, GlowStyle::Water) && j % 3 == 1 {
                    // A falling drip draws as a small vertical STREAK — like the
                    // engine's fall-speed-stretched droplets, not a round bead.
                    prims.push(DrawPrim::Panel {
                        x: dot_x - ss * 0.06,
                        y: dot_y - ss * 0.15,
                        w: ss * 0.12,
                        h: ss * 0.3 * (1.0 + life),
                        radius: ss * 0.06,
                        fill: rgba(u32_rgb(color), (220.0 * fade * pulse) as u8),
                        blur: false,
                    });
                } else {
                    // Comet grains TWINKLE on their own phase (glitter catching
                    // the light — the engine's seeded-twinkle signature); every
                    // other style's particles hold a steady fade.
                    let tw = if matches!(style, GlowStyle::Comet) {
                        0.35 + 0.65 * (t * (5.0 + jf) + jf * 2.4).sin().abs()
                    } else {
                        1.0
                    };
                    prims.push(DrawPrim::Dot {
                        cx: dot_x,
                        cy: dot_y,
                        r: ss * 0.13,
                        color: rgba(u32_rgb(color), (220.0 * fade * pulse * tw) as u8),
                        breathe: false,
                    });
                }
            }
        }
    }

    // Theme swatch strip (only when the theme is the focused control): the four
    // representative colours of the current — or menu-highlighted — scheme, under the
    // mock. The detail block that once followed is GONE (design §8: the group row
    // shows label + value already).
    if let Some(f) = focused.filter(|f| f.key == prefs::EDIT_THEME) {
        let name = menu_theme_name.map_or_else(|| theme_current(f), String::clone);
        let sw_y = sy + sh + cw * 0.5;
        let swr = fit(ch * 0.22, 2.0, 7.0);
        if sw_y + swr * 2.0 < top + gh {
            let mut swx = sx + swr;
            for c in &theme_swatches(&name) {
                prims.push(DrawPrim::Dot {
                    cx: swx,
                    cy: sw_y + swr,
                    r: swr,
                    color: rgba(*c, 0xFF),
                    breathe: false,
                });
                swx += swr * 2.6;
            }
        }
    }

    // The `accent` outline around the element the focused control drives.
    let outline = |prims: &mut Vec<DrawPrim>, x: f32, y: f32, w: f32, h: f32| {
        prims.push(DrawPrim::Stroke {
            x,
            y,
            w,
            h,
            radius: fit(h * 0.25, 0.0, 6.0),
            width: 1.5,
            color: rgba(r.accent, 0xFF),
        });
    };
    match drives {
        Some(Driven::Background) => outline(prims, sx, sy, sw, sh),
        Some(Driven::Foreground) => {
            outline(
                prims,
                tx - cw * 0.2,
                liga_row_y - ss * 0.15,
                text_w(liga, ss) + cw * 0.4,
                ss * 1.3,
            );
        }
        Some(Driven::Selection) => {
            outline(
                prims,
                sel_x - ss * 0.2,
                sel_row_y - ss * 0.2,
                text_w(sel_word, ss) + ss * 0.4,
                ss * 1.4,
            );
        }
        Some(Driven::Cursor) => outline(prims, cxr - 2.0, cyr - 2.0, cwr + 4.0, chr + 4.0),
        Some(Driven::Titlebar) => outline(prims, sx - 2.0, sy - 2.0, sw + 4.0, tb_h + 4.0),
        None => {}
    }
}

/// Per-category icon-tile tint — every tone derives from [`Roles`]/theme (the
/// "no Apple hex" rule at [`Roles`]): accent-family for Appearance/Typography,
/// success-family for Cursor/Performance, neutral for Terminal, danger for Security.
fn category_tint(sec: prefs::Section, r: &Roles, theme: Theme) -> [u8; 3] {
    match sec {
        // Graphite, so its fg/bg split-disk pictogram (the appearance concept itself)
        // carries the color story.
        prefs::Section::Appearance => lerp_rgb(r.text_secondary, r.text_primary, 0.35),
        prefs::Section::Cursor => r.accent,
        // Cursor Kitty: the accent pulled toward the cursor's own hue — the
        // cat's tile reads as a sibling of Cursor's full-strength accent (they
        // are the same surface split in two) without repeating it exactly.
        prefs::Section::CursorKitty => lerp_rgb(r.accent, u32_rgb(theme.cursor), 0.45),
        prefs::Section::Typography => u32_rgb(theme.cursor),
        // Window: the selection wash is the theme's "window furniture" tone —
        // distinct from the accent/success families its neighbors wear.
        prefs::Section::Window => lerp_rgb(u32_rgb(theme.selection), r.text_primary, 0.25),
        // Input: accent pulled toward the surface — a quieter sibling of Cursor's
        // full-strength accent (both are "where your typing lands" tabs).
        prefs::Section::Input => lerp_rgb(r.accent, r.text_secondary, 0.45),
        prefs::Section::Performance => r.success,
        // A near-terminal-dark tile (the classic "terminal" app-icon look); its
        // pictogram paints in success-green prompt tones for contrast.
        prefs::Section::Terminal => lerp_rgb(u32_rgb(theme.bg), r.text_primary, 0.22),
        prefs::Section::Security => r.danger,
        // Packages: the parcel tile shares the window-furniture neutrality —
        // toolchain plumbing, not a personalization surface.
        prefs::Section::Packages => lerp_rgb(u32_rgb(theme.selection), r.text_secondary, 0.35),
        // The Kitty Log's fixed rose accent. This is UI branding, not the
        // retired `[sparkle_words.feline] color` compatibility key.
        prefs::Section::KittyLog => [0xF7, 0xA8, 0xB8],
    }
}

/// The icon tile's PICTOGRAM (design §2.2 table), composed from substrate prims inside
/// the `s`-px tile at `(x, y)`. The only glyphs are single ASCII chars ("A", ">") —
/// safe in every font, per the no-glyph-roulette rule. Every shape shares one inner
/// grid (content spans ~0.56–0.62 of the tile, centred) and one stroke weight, so the
/// six tiles read as a FAMILY — mismatched insets/weights were exactly the jank.
fn category_pictogram(
    prims: &mut Vec<DrawPrim>,
    sec: prefs::Section,
    r: &Roles,
    theme: Theme,
    x: f32,
    y: f32,
    s: f32,
) {
    let cx = x + s * 0.5;
    let cy = y + s * 0.5;
    let on = r.on_accent;
    // The one stroke weight every outlined shape shares (scales with the tile,
    // never hairline-thin on a small strip tile).
    let sw = (s * 0.06).max(1.5);
    match sec {
        // Appearance: the light/dark half-split disk (the macOS "appearance" idea) —
        // the LEFT half is the theme's fg (light), the RIGHT its bg (dark), so the
        // disk literally shows the theme's two poles; a thin rim keeps it one shape.
        prefs::Section::Appearance => {
            let dr = s * 0.28;
            prims.push(DrawPrim::Dot {
                cx,
                cy,
                r: dr,
                color: rgba(u32_rgb(theme.fg), 0xFF),
                breathe: false,
            });
            prims.push(DrawPrim::ClipPush {
                x: cx,
                y: cy - dr,
                w: dr,
                h: dr * 2.0,
            });
            prims.push(DrawPrim::Dot {
                cx,
                cy,
                r: dr,
                color: rgba(u32_rgb(theme.bg), 0xFF),
                breathe: false,
            });
            prims.push(DrawPrim::ClipPop);
            prims.push(DrawPrim::Stroke {
                x: cx - dr,
                y: cy - dr,
                w: dr * 2.0,
                h: dr * 2.0,
                radius: dr,
                width: sw * 0.8,
                color: rgba(on, 0xCC),
            });
        }
        // Cursor: an I-beam — vertical bar with top/bottom serifs (unmistakably a
        // text cursor, unlike the lone floating bar it replaces).
        prefs::Section::Cursor => {
            let (bar_w, serif_w, serif_h) = (s * 0.09, s * 0.26, s * 0.09);
            for (sy, sh) in [
                (cy - s * 0.30, serif_h),           // top serif
                (cy + s * 0.30 - serif_h, serif_h), // bottom serif
            ] {
                prims.push(DrawPrim::Panel {
                    x: cx - serif_w * 0.5,
                    y: sy,
                    w: serif_w,
                    h: sh,
                    radius: sh * 0.3,
                    fill: rgba(on, 0xFF),
                    blur: false,
                });
            }
            prims.push(DrawPrim::Panel {
                x: cx - bar_w * 0.5,
                y: cy - s * 0.30,
                w: bar_w,
                h: s * 0.60,
                radius: bar_w * 0.3,
                fill: rgba(on, 0xFF),
                blur: false,
            });
        }
        // Cursor Kitty: the Kitty Log's peeking-cat silhouette WALKING on a
        // rainbow rail — the head/ear vocabulary is shared on purpose (both
        // tiles are the same cat), and the bar under it is what separates the
        // cursor's companion from the collection book.
        prefs::Section::CursorKitty => {
            let hr = s * 0.20; // head radius (smaller than the Log's: it stands ON something)
            let hy = cy - s * 0.02;
            let ew = s * 0.13; // ear nub width
            for side in [-1.0f32, 1.0] {
                prims.push(DrawPrim::Panel {
                    x: cx + side * hr * 0.62 - ew * 0.5,
                    y: hy - hr - s * 0.08,
                    w: ew,
                    h: s * 0.17,
                    radius: ew * 0.35,
                    fill: rgba(on, 0xFF),
                    blur: false,
                });
            }
            prims.push(DrawPrim::Dot {
                cx,
                cy: hy,
                r: hr,
                color: rgba(on, 0xFF),
                breathe: false,
            });
            // The rainbow RAIL it walks: one bar in the tile tint under the cat.
            let tint = category_tint(sec, r, theme);
            prims.push(DrawPrim::Panel {
                x: cx - s * 0.28,
                y: cy + s * 0.24,
                w: s * 0.56,
                h: (s * 0.08).max(1.5),
                radius: (s * 0.04).max(0.75),
                fill: rgba(tint, 0xFF),
                blur: false,
            });
        }
        prefs::Section::Typography => {
            let ts = s * 0.66;
            // Mono: the tile "A" is glyph ART (a pictogram), not label text; sized by
            // the icon geometry (Body.px preserves the exact px through the funnel).
            prims.push(text_prim(
                cx - text_w("A", ts) * 0.5,
                cy - ts * 0.54 + ts,
                "A".to_string(),
                TypeStep::Body.px(ts),
                TextWeight::Regular,
                TextFace::Mono,
                rgba(on, 0xFF),
            ));
        }
        // Window: a window frame — rounded outline with a filled titlebar band
        // (the same one-stroke-weight family grid as every other tile).
        prefs::Section::Window => {
            let (fw, fh) = (s * 0.56, s * 0.44);
            let (fx, fy) = (cx - fw * 0.5, cy - fh * 0.5);
            let bar_h = fh * 0.28;
            prims.push(DrawPrim::Panel {
                x: fx,
                y: fy,
                w: fw,
                h: bar_h,
                radius: s * 0.06,
                fill: rgba(on, 0xFF),
                blur: false,
            });
            prims.push(DrawPrim::Stroke {
                x: fx,
                y: fy,
                w: fw,
                h: fh,
                radius: s * 0.06,
                width: sw,
                color: rgba(on, 0xFF),
            });
        }
        // Input: a keycap with a space-bar slot — outlined cap, filled bar low in
        // the cap (unambiguously "keyboard" at strip size, unlike arrow glyphs).
        prefs::Section::Input => {
            let (kw, kh) = (s * 0.52, s * 0.52);
            let (kx, ky) = (cx - kw * 0.5, cy - kh * 0.5);
            prims.push(DrawPrim::Stroke {
                x: kx,
                y: ky,
                w: kw,
                h: kh,
                radius: s * 0.10,
                width: sw,
                color: rgba(on, 0xFF),
            });
            prims.push(DrawPrim::Panel {
                x: kx + kw * 0.22,
                y: ky + kh * 0.62,
                w: kw * 0.56,
                h: (kh * 0.12).max(2.0),
                radius: 1.5,
                fill: rgba(on, 0xFF),
                blur: false,
            });
        }
        // Performance: three ascending bars (the universal "stats" mark) — crisper at
        // tile size than a gauge arc, which read as a loading spinner.
        prefs::Section::Performance => {
            let bw = s * 0.13;
            let gap = s * 0.075;
            let base = cy + s * 0.28;
            for (i, hfrac) in [0.32f32, 0.5, 0.66].iter().enumerate() {
                let bh = s * hfrac;
                prims.push(DrawPrim::Panel {
                    x: cx - (bw * 1.5 + gap) + i as f32 * (bw + gap),
                    y: base - bh,
                    w: bw,
                    h: bh,
                    radius: bw * 0.3,
                    fill: rgba(on, 0xFF),
                    blur: false,
                });
            }
        }
        // Terminal: the classic prompt on a near-black tile — success-green chevron +
        // blinking-cursor underscore, both on one baseline grid.
        prefs::Section::Terminal => {
            let ts = s * 0.52;
            let pcolor = lerp_rgb(r.success, r.text_primary, 0.25);
            // Mono: the prompt chevron is glyph art on the terminal tile, sized by the
            // icon geometry (Body.px preserves the exact px through the text funnel).
            prims.push(text_prim(
                cx - s * 0.30,
                cy - ts * 0.55 + ts,
                ">".to_string(),
                TypeStep::Body.px(ts),
                TextWeight::Regular,
                TextFace::Mono,
                rgba(pcolor, 0xFF),
            ));
            prims.push(DrawPrim::Panel {
                x: cx + s * 0.04,
                y: cy + s * 0.14,
                w: s * 0.26,
                h: (s * 0.07).max(2.0),
                radius: 1.0,
                fill: rgba(pcolor, 0xFF),
                blur: false,
            });
        }
        // Security: a real padlock — filled rounded body, an arc shackle (a circle
        // outline clipped to the band above the body), and a keyhole "cut out" in
        // the tile's own tint.
        prefs::Section::Security => {
            let (body_w, body_h) = (s * 0.40, s * 0.30);
            let body_y = cy - s * 0.02;
            let shackle_r = s * 0.13;
            prims.push(DrawPrim::ClipPush {
                x: cx - shackle_r - sw,
                y: body_y - shackle_r - s * 0.10,
                w: (shackle_r + sw) * 2.0,
                h: shackle_r + s * 0.10,
            });
            prims.push(DrawPrim::Stroke {
                x: cx - shackle_r,
                y: body_y - shackle_r * 2.0 + s * 0.02,
                w: shackle_r * 2.0,
                h: shackle_r * 2.0,
                radius: shackle_r,
                width: sw,
                color: rgba(on, 0xFF),
            });
            prims.push(DrawPrim::ClipPop);
            prims.push(DrawPrim::Panel {
                x: cx - body_w * 0.5,
                y: body_y,
                w: body_w,
                h: body_h,
                radius: s * 0.07,
                fill: rgba(on, 0xFF),
                blur: false,
            });
            prims.push(DrawPrim::Dot {
                cx,
                cy: body_y + body_h * 0.42,
                r: s * 0.055,
                color: rgba(r.danger, 0xFF),
                breathe: false,
            });
        }
        // Packages: a parcel — the tile outline with a horizontal tape band
        // and a short vertical flap seam. Same substrate vocabulary (no glyphs).
        prefs::Section::Packages => {
            let bw = s * 0.58;
            let bh = s * 0.50;
            let bx = cx - bw * 0.5;
            let by = cy - bh * 0.5;
            prims.push(DrawPrim::Stroke {
                x: bx,
                y: by,
                w: bw,
                h: bh,
                radius: s * 0.07,
                width: sw,
                color: rgba(on, 0xFF),
            });
            // Tape band across the middle + the lid seam down from the top edge.
            prims.push(DrawPrim::Panel {
                x: bx,
                y: cy - sw * 0.5,
                w: bw,
                h: sw,
                radius: 0.0,
                fill: rgba(on, 0xFF),
                blur: false,
            });
            prims.push(DrawPrim::Panel {
                x: cx - sw * 0.5,
                y: by,
                w: sw,
                h: bh * 0.32,
                radius: 0.0,
                fill: rgba(on, 0xFF),
                blur: false,
            });
        }
        // Kitty Log: a peeking cat head — two rounded ear nubs under a head
        // disk (the §5 silhouette), eyes "cut out" in the tile's own tint.
        // Same substrate-prim vocabulary as every other tile (no glyphs).
        prefs::Section::KittyLog => {
            let hr = s * 0.26; // head radius
            let hy = cy + s * 0.04; // head centre, nudged down for the ears
            let ew = s * 0.16; // ear nub width
            for side in [-1.0f32, 1.0] {
                prims.push(DrawPrim::Panel {
                    x: cx + side * hr * 0.62 - ew * 0.5,
                    y: hy - hr - s * 0.10,
                    w: ew,
                    h: s * 0.22,
                    radius: ew * 0.35,
                    fill: rgba(on, 0xFF),
                    blur: false,
                });
            }
            prims.push(DrawPrim::Dot {
                cx,
                cy: hy,
                r: hr,
                color: rgba(on, 0xFF),
                breathe: false,
            });
            // Eyes in the tile tint (the §F4 pink), on the §5 eye-row height.
            let tint = category_tint(sec, r, theme);
            for side in [-1.0f32, 1.0] {
                prims.push(DrawPrim::Dot {
                    cx: cx + side * hr * 0.42,
                    cy: hy - hr * 0.12,
                    r: (s * 0.045).max(1.5),
                    color: rgba(tint, 0xFF),
                    breathe: false,
                });
            }
        }
    }
}

/// Paint the SIDEBAR (design §2): the darker full-height wash + 1 px separator, the
/// search field over rows 1-2 (the search state machine is reused verbatim — only its
/// pixels moved here from the old title row), and six 2-cell category rows: icon tile +
/// pictogram + label + the accent selection pill. Keyboard focus on the sidebar rings
/// the pill; an active search filter hollows it and dims non-matching categories.
fn paint_sidebar(
    prims: &mut Vec<DrawPrim>,
    state: &SettingsState,
    r: &Roles,
    theme: Theme,
    g: &SettingsGeom,
    pg: &PaneGeom,
) {
    let (cw, ch, px) = (g.cw, g.ch, g.font_px);
    let h = g.panel_rows as f32 * ch;
    let sb_w = pg.sidebar_w_cells * cw;
    if sb_w <= 0.0 {
        return;
    }
    // The two-tone split: the sidebar sits in a slightly darker wash than the content
    // surface, separated by a hairline — the System Settings sidebar/content seam.
    prims.push(DrawPrim::Panel {
        x: 0.0,
        y: 0.0,
        w: sb_w,
        h,
        radius: (ch * 0.6).min(14.0),
        fill: rgba(lerp_rgb(r.surface, u32_rgb(theme.bg), 0.35), 0xF4),
        blur: false,
    });
    prims.push(DrawPrim::Stroke {
        x: sb_w,
        y: 0.0,
        w: 1.0,
        h,
        radius: 0.0,
        width: 1.0,
        color: rgba(r.separator, 0x55),
    });

    let filtering = state.filtering();
    // Indices visible under the filter, computed once for the per-category dimming.
    let vis = filtering.then(|| state.visible_indices());

    // Search field, centred over rows 1-2.
    let f_h = ch * 1.4;
    let f_y = 2.0 * ch - f_h * 0.5;
    let f_x = if pg.icon_strip { cw * 0.5 } else { cw };
    let f_w = sb_w - 2.0 * f_x;
    if f_w > cw * 2.0 && g.panel_rows > 4 {
        prims.push(DrawPrim::Stroke {
            x: f_x,
            y: f_y,
            w: f_w,
            h: f_h,
            radius: f_h * 0.4,
            width: if state.searching { 1.5 } else { 1.0 },
            color: rgba(
                if state.searching {
                    r.accent
                } else {
                    r.control_track
                },
                0xFF,
            ),
        });
        // Magnifier from prims: a ring (rounded-square stroke at radius = w/2) + a short
        // Panel tick at its lower right (a 45° stroke is not axis-alignable).
        let mr = f_h * 0.16;
        let mcx = f_x + cw * 0.7;
        let mcy = f_y + f_h * 0.44;
        prims.push(DrawPrim::Stroke {
            x: mcx - mr,
            y: mcy - mr,
            w: mr * 2.0,
            h: mr * 2.0,
            radius: mr,
            width: 1.2,
            color: rgba(r.text_tertiary, 0xFF),
        });
        prims.push(DrawPrim::Panel {
            x: mcx + mr * 0.55,
            y: mcy + mr * 0.55,
            w: mr * 0.9,
            h: 1.4,
            radius: 0.7,
            fill: rgba(r.text_tertiary, 0xFF),
            blur: false,
        });
        if !pg.icon_strip {
            let qstep = TypeStep::Secondary.px(px);
            let qsize = qstep.get();
            let tx = mcx + mr + cw * 0.5;
            let (qtext, qcolor) = if state.query.is_empty() {
                ("Search".to_string(), r.text_tertiary)
            } else {
                (state.query.clone(), r.text_primary)
            };
            prims.push(text_prim(
                tx,
                row_baseline(f_y, f_h, qsize),
                qtext,
                qstep,
                TextWeight::Regular,
                TextFace::Ui,
                rgba(qcolor, 0xFF),
            ));
            if state.searching {
                prims.push(DrawPrim::Stroke {
                    x: tx + ui_text_width(&state.query, qsize) + 1.0,
                    y: f_y + 2.0,
                    w: 1.0,
                    h: f_h - 4.0,
                    radius: 0.0,
                    width: 1.0,
                    color: rgba(r.accent, 0xFF),
                });
            }
        }
    }

    // Six category rows, 2 cells each, zero gap (rows 4-5 … 14-15).
    for (i, sec) in prefs::Section::ORDER.iter().enumerate() {
        let row0 = SIDEBAR_CAT_ROW0 + i * 2;
        if row0 + 2 > pg.footer_row {
            break; // a short retired card clips trailing categories, never the footer
        }
        let y0 = row0 as f32 * ch;
        let rh = 2.0 * ch;
        let selected = *sec == state.category;
        let matches = vis.as_ref().is_none_or(|v| {
            v.iter()
                .any(|&j| prefs::section_of(state.fields[j].key) == *sec)
        });

        // The selection pill: filled at rest, HOLLOW while a search filter is active
        // (results are cross-category); ringed while the sidebar owns keyboard focus.
        let (pl, pt, pw, ph) = (cw * 0.6, y0 + 2.0, sb_w - cw * 1.2, rh - 4.0);
        if selected && !filtering {
            prims.push(DrawPrim::Panel {
                x: pl,
                y: pt,
                w: pw,
                h: ph,
                radius: ch * 0.4,
                fill: rgba(r.accent, 0xFF),
                blur: false,
            });
            if state.pane == SettingsPane::Sidebar {
                prims.push(DrawPrim::Stroke {
                    x: pl - 2.0,
                    y: pt - 2.0,
                    w: pw + 4.0,
                    h: ph + 4.0,
                    radius: ch * 0.45,
                    width: 1.5,
                    color: rgba(r.accent, 0xCC),
                });
            }
        } else if selected {
            prims.push(DrawPrim::Stroke {
                x: pl,
                y: pt,
                w: pw,
                h: ph,
                radius: ch * 0.4,
                width: 1.0,
                color: rgba(r.accent, 0xAA),
            });
        }

        // Icon tile + pictogram (centred in the strip when collapsed).
        let tile = ch * 1.5;
        let tile_x = if pg.icon_strip {
            (sb_w - tile) * 0.5
        } else {
            cw * 1.2
        };
        let tile_y = y0 + (rh - tile) * 0.5;
        let dimmed = filtering && !matches;
        prims.push(DrawPrim::Panel {
            x: tile_x,
            y: tile_y,
            w: tile,
            h: tile,
            radius: tile * 0.28,
            fill: rgba(
                category_tint(*sec, r, theme),
                if dimmed { 0x66 } else { 0xFF },
            ),
            blur: false,
        });
        category_pictogram(prims, *sec, r, theme, tile_x, tile_y, tile);

        if !pg.icon_strip {
            let lcolor = if dimmed {
                r.text_tertiary
            } else if selected && !filtering {
                r.on_accent
            } else {
                r.text_primary
            };
            let lstep = TypeStep::Body.px(px);
            prims.push(text_prim(
                tile_x + tile + cw * 0.6,
                row_baseline(y0, rh, lstep.get()),
                sec.label().to_string(),
                lstep,
                TextWeight::Regular,
                TextFace::Ui,
                rgba(lcolor, 0xFF),
            ));
        }
    }
}

/// Total cells occupied by the rows before `scroll` — the scrollbar thumb's offset.
fn cells_before(rows: &[GroupRow], scroll: usize) -> usize {
    rows.iter().take(scroll).map(group_row_cells).sum()
}

/// A faint track + brighter thumb for an overflowing band. Geometry-only inputs, so a
/// scrollbar can never read (and thus never reflow on) selection state.
#[allow(clippy::too_many_arguments)]
fn paint_scrollbar(
    prims: &mut Vec<DrawPrim>,
    r: &Roles,
    x: f32,
    top: f32,
    track_h: f32,
    sw: f32,
    before: f32,
    visible: f32,
    total: f32,
    ch: f32,
) {
    if total <= 0.0 || track_h <= 0.0 {
        return;
    }
    prims.push(DrawPrim::Panel {
        x,
        y: top,
        w: sw,
        h: track_h,
        radius: sw * 0.5,
        fill: rgba(r.separator, 0x55),
        blur: false,
    });
    let thumb_h = (track_h * visible / total).clamp(ch.min(track_h), track_h);
    // Map the scrolled-past rows onto the thumb's TRAVEL (track minus thumb): at
    // max scroll `before == total - visible`, which must land the thumb at the
    // track END — dividing by `total` would strand it `visible/total` short.
    let scrollable = (total - visible).max(f32::EPSILON);
    let frac = (before / scrollable).clamp(0.0, 1.0);
    prims.push(DrawPrim::Panel {
        x,
        y: top + frac * (track_h - thumb_h),
        w: sw,
        h: thumb_h,
        radius: sw * 0.5,
        fill: rgba(r.text_tertiary, 0xCC),
        blur: false,
    });
}

/// Paint the ACTIVE CATEGORY's group-boxes into the group band (design §3.2): caption /
/// rounded `r.elevated` box of 2-cell control rows with inset separators / footnote /
/// gap, all from the shared [`category_layout`]. The window walks WHOLE rows (no partial
/// row paints), exactly the walk [`group_row_at`] resolves clicks with.
fn paint_group_band(
    prims: &mut Vec<DrawPrim>,
    state: &SettingsState,
    r: &Roles,
    theme: Theme,
    g: &SettingsGeom,
    pg: &PaneGeom,
) {
    let (cw, ch, px) = (g.cw, g.ch, g.font_px);
    let w = g.cols as f32 * cw;
    let band = pg.group_band();
    if band == 0 {
        return;
    }
    let band_top = pg.groups.0 as f32 * ch;
    let box_x = pg.content_x(cw) + cw * 2.0;
    let box_right = w - cw * 2.5;
    let box_w = box_right - box_x;
    if box_w <= cw * 3.0 {
        return; // degenerate width — nothing legible fits
    }
    let v_left = content_v_left(g);
    let v_right = content_v_right(g);
    let label_x = box_x + cw * 1.2;

    let rows = category_layout(&state.fields, state.category, footnote_wrap_chars(g.cols));
    let scroll = state.scroll.min(rows.len());
    // The visible whole-row window: (row index, cell offset) pairs.
    let mut visible: Vec<(usize, usize)> = Vec::new();
    let mut cells = 0usize;
    for (i, row) in rows.iter().enumerate().skip(scroll) {
        let rh = group_row_cells(row);
        if cells + rh > band {
            break;
        }
        visible.push((i, cells));
        cells += rh;
    }

    // Pass 1 — the rounded boxes: one panel per contiguous run of visible control rows,
    // so a scroll-clipped box still closes at a row boundary.
    let push_box = |prims: &mut Vec<DrawPrim>, start_cell: usize, n_cells: usize| {
        prims.push(DrawPrim::Panel {
            x: box_x,
            y: band_top + start_cell as f32 * ch,
            w: box_w,
            h: n_cells as f32 * ch,
            radius: ch * 0.5,
            fill: rgba(r.elevated, 0xFF),
            blur: false,
        });
    };
    let mut run: Option<(usize, usize)> = None;
    for &(i, off) in &visible {
        if matches!(rows[i], GroupRow::Control(_)) {
            run = Some(run.map_or((off, 2), |(s0, n)| (s0, n + 2)));
        } else if let Some((s0, n)) = run.take() {
            push_box(prims, s0, n);
        }
    }
    if let Some((s0, n)) = run.take() {
        push_box(prims, s0, n);
    }

    // Pass 2 — row content over the boxes.
    let mut in_run = false; // whether the previous visible row was a control (same box)
    for &(i, off) in &visible {
        let y0 = band_top + off as f32 * ch;
        match rows[i] {
            GroupRow::Caption(c) => {
                in_run = false;
                // Group caption: the macOS secondary uppercase ramp step (Caption),
                // led by a small rounded index bar in the CATEGORY's tile tint —
                // the sidebar's color story carried into the content pane.
                let cstep = TypeStep::Caption.px(px);
                let tint = category_tint(state.category, r, theme);
                let bar_h = cstep.get() * 0.9;
                prims.push(DrawPrim::Panel {
                    x: box_x + cw * 0.05,
                    y: row_baseline(y0, ch, cstep.get()) - bar_h + cstep.get() * 0.12,
                    w: (cw * 0.28).max(3.0),
                    h: bar_h,
                    radius: (cw * 0.14).max(1.5),
                    fill: rgba(tint, 0xE6),
                    blur: false,
                });
                prims.push(text_prim(
                    box_x + cw * 0.6,
                    row_baseline(y0, ch, cstep.get()),
                    c.to_uppercase(),
                    cstep,
                    TextWeight::Regular,
                    TextFace::Ui,
                    rgba(lerp_rgb(r.text_secondary, tint, 0.3), 0xFF),
                ));
            }
            GroupRow::Footnote(n) => {
                in_run = false;
                // The layout already wrapped footnotes to the box width
                // (`footnote_wrap_chars` rows); the elide is the containment
                // backstop (§3.2) so no metric drift can paint past the box.
                let avail = box_right - cw * 0.6 - (box_x + cw * 0.6);
                let fstep = TypeStep::Caption.px(px);
                prims.push(text_prim(
                    box_x + cw * 0.6,
                    row_baseline(y0, ch, fstep.get()),
                    elide_to(n, fstep.get(), avail.max(0.0)),
                    fstep,
                    TextWeight::Regular,
                    TextFace::Ui,
                    rgba(r.text_tertiary, 0xFF),
                ));
            }
            GroupRow::Gap => in_run = false,
            GroupRow::Control(idx) => {
                let Some(f) = state.fields.get(idx) else {
                    continue;
                };
                // Inset hairline between adjacent rows of one box (macOS look).
                if in_run {
                    prims.push(DrawPrim::Stroke {
                        x: label_x,
                        y: y0.round(),
                        w: (box_right - cw * 0.6 - label_x).max(0.0),
                        h: 1.0,
                        radius: 0.0,
                        width: 1.0,
                        color: rgba(r.separator, 0x33),
                    });
                }
                in_run = true;
                let selected = idx == state.selected;
                if selected {
                    prims.push(DrawPrim::Panel {
                        x: box_x + 2.0,
                        y: y0 + 2.0,
                        w: box_w - 4.0,
                        h: 2.0 * ch - 4.0,
                        radius: ch * 0.4,
                        fill: rgba(r.accent, SEL_WASH_ALPHA),
                        blur: false,
                    });
                    // The focus ring marks the CONTENT pane owning the keyboard;
                    // with the sidebar focused only the wash shows.
                    if state.pane == SettingsPane::Content {
                        prims.push(DrawPrim::Stroke {
                            x: box_x + 2.0,
                            y: y0 + 2.0,
                            w: box_w - 4.0,
                            h: 2.0 * ch - 4.0,
                            radius: ch * 0.4,
                            width: 1.5,
                            color: rgba(r.accent, 0xCC),
                        });
                    }
                }
                if is_overridden(f) {
                    prims.push(DrawPrim::Dot {
                        cx: box_x - cw * 0.8,
                        cy: y0 + ch,
                        r: (px * 0.16).max(2.0),
                        color: rgba(r.accent, 0xFF),
                        breathe: false,
                    });
                }
                let lstep = TypeStep::Body.px(px);
                prims.push(text_prim(
                    label_x,
                    row_baseline(y0, 2.0 * ch, lstep.get()),
                    f.label.to_string(),
                    lstep,
                    TextWeight::Regular,
                    TextFace::Ui,
                    rgba(r.text_primary, 0xFF),
                ));
                let editing = if selected {
                    state.editing.as_deref()
                } else {
                    None
                };
                // Centre the widget over the 2-cell row by feeding build_widget the
                // MIDDLE 1-cell band — widget metrics stay identical to a 1-cell row,
                // so `widget_hit_left`'s scratch replay keeps matching the pixels.
                build_widget(
                    prims,
                    f,
                    r,
                    cw,
                    ch,
                    px,
                    y0 + ch * 0.5,
                    v_left,
                    v_right,
                    selected,
                    editing,
                );
            }
        }
    }

    // Scrollbar on overflow, outside the boxes (appearing moves no content).
    let total: usize = rows.iter().map(group_row_cells).sum();
    if total > band {
        paint_scrollbar(
            prims,
            r,
            w - cw * 1.2,
            band_top,
            band as f32 * ch,
            (cw * 0.2).max(2.0),
            cells_before(&rows, scroll) as f32,
            band as f32,
            total as f32,
            ch,
        );
    }
}

/// Paint the KITTY LOG collection book (§F4.6) into the group band: a header
/// stat line (`Sightings … · Collection …/… · Languages … · Rarest: …`), then
/// the generated, reachable art groups — full-body specials, accessories, and
/// compact head progress — as 1-cell rows:
/// leading tier dot · label · right-aligned language chips, ×count, and
/// first→last dates. Undiscovered cells paint as dimmed `???` silhouette rows.
/// Read-only by construction: no [`EditField`] maps to this category, so the
/// selection/activation/hit paths all no-op. Rows come from the SAME
/// [`crate::kitty_log::kitty_book`] model `controls_lines` serializes
/// (screen == introspection); the data is the [`SettingsState::kitty_log`]
/// snapshot — the pure painter reads no live App state.
fn paint_kitty_book_band(
    prims: &mut Vec<DrawPrim>,
    state: &SettingsState,
    r: &Roles,
    g: &SettingsGeom,
    pg: &PaneGeom,
) {
    let (cw, ch, px) = (g.cw, g.ch, g.font_px);
    let w = g.cols as f32 * cw;
    let band = pg.group_band();
    if band == 0 {
        return;
    }
    let band_top = pg.groups.0 as f32 * ch;
    let box_x = pg.content_x(cw) + cw * 2.0;
    let box_right = w - cw * 2.5;
    if box_right - box_x <= cw * 3.0 {
        return; // degenerate width — nothing legible fits
    }
    let book = crate::kitty_log::kitty_book(&state.kitty_log.log);
    // 1-cell text rows, truncated at the band edge (the book is a fixed 26
    // cells — header + 5 captions + 20 rows — so it fits every full-layout
    // geometry; a short retired-card fixture simply clips the tail).
    let mut cell = 0usize;
    let put = |prims: &mut Vec<DrawPrim>,
               cell: usize,
               x: f32,
               s: String,
               size: crate::type_scale::StepPx,
               color: [u8; 3],
               alpha: u8| {
        prims.push(text_prim(
            x,
            row_baseline(band_top + cell as f32 * ch, ch, size.get()),
            s,
            size,
            TextWeight::Regular,
            TextFace::Ui,
            rgba(color, alpha),
        ));
    };
    // Header stats — the §F4.6 one-liner.
    let langs_suffix = if book.languages.is_empty() {
        String::new()
    } else {
        format!(" ({})", book.languages.join(","))
    };
    let header = format!(
        "Sightings {} · Collection {}/{}{} · Languages {} · Rarest: {}",
        book.sightings,
        book.collected,
        book.denominator,
        langs_suffix,
        book.languages.len(),
        book.rarest.unwrap_or("—"),
    );
    let bstep = TypeStep::Body.px(px);
    let cstep = TypeStep::Caption.px(px);
    put(
        prims,
        cell,
        box_x + cw * 0.6,
        elide_to(&header, bstep.get(), box_right - box_x - cw * 1.2),
        bstep,
        r.text_primary,
        0xFF,
    );
    cell += 1;
    // Reachable v4 art groups, each a caption + rows. The 25 heads stay one
    // compact progress row; special full cats and accessories are individual.
    for (caption, tier) in [
        ("Special Cats", "specials"),
        ("Accessories", "accessories"),
        ("Heads", "heads"),
    ] {
        if cell >= band {
            break;
        }
        put(
            prims,
            cell,
            box_x + cw * 0.6,
            caption.to_uppercase(),
            cstep,
            r.text_secondary,
            0xFF,
        );
        cell += 1;
        for row in book.rows.iter().filter(|row| row.tier == tier) {
            if cell >= band {
                break;
            }
            let y0 = band_top + cell as f32 * ch;
            // The tier dot: pictogram slot — solid for a sighted cell, hollow
            // (dim) for an undiscovered one.
            prims.push(DrawPrim::Dot {
                cx: box_x + cw * 1.0,
                cy: y0 + ch * 0.5,
                r: (px * 0.14).max(2.0),
                color: rgba(
                    if row.seen { r.accent } else { r.text_tertiary },
                    if row.seen { 0xFF } else { 0x66 },
                ),
                breathe: false,
            });
            // Label — or the `???` silhouette for an undiscovered cell.
            let (label, lcolor) = if row.seen {
                (row.label.to_string(), r.text_primary)
            } else {
                ("???".to_string(), r.text_tertiary)
            };
            put(prims, cell, box_x + cw * 1.8, label, bstep, lcolor, 0xFF);
            // Right column: chips · ×count · first→last (dates only), or a
            // quiet placeholder while undiscovered.
            let right = if row.seen {
                let mut s = String::new();
                if !row.langs.is_empty() {
                    s.push_str(&row.langs.join("·"));
                    s.push_str("  ");
                }
                if row.goal > 1 {
                    s.push_str(&format!("{}/{} found", row.count, row.goal));
                } else {
                    s.push_str(&format!("×{}", row.count));
                }
                if !row.first_seen.is_empty() {
                    s.push_str(&format!(
                        "  {} → {}",
                        row.first_seen.get(..10).unwrap_or(&row.first_seen),
                        row.last_seen.get(..10).unwrap_or(&row.last_seen),
                    ));
                }
                s
            } else {
                "not yet sighted".to_string()
            };
            put(
                prims,
                cell,
                box_right - cw * 0.6 - ui_text_width(&right, cstep.get()),
                right,
                cstep,
                r.text_tertiary,
                if row.seen { 0xFF } else { 0xAA },
            );
            cell += 1;
        }
    }
}

/// Paint the flat cross-category SEARCH-RESULT list into the group band (design §4.4):
/// the existing [`body_layout_masked`] machinery — 1-cell rows, section headers as small
/// captions — as one borderless pseudo-group. The preview card above stays.
fn paint_flat_band(
    prims: &mut Vec<DrawPrim>,
    state: &SettingsState,
    r: &Roles,
    g: &SettingsGeom,
    pg: &PaneGeom,
    mask: Option<&[bool]>,
) {
    let (cw, ch, px) = (g.cw, g.ch, g.font_px);
    let w = g.cols as f32 * cw;
    let band = pg.group_band();
    if band == 0 {
        return;
    }
    let band_top = pg.groups.0 as f32 * ch;
    let box_x = pg.content_x(cw) + cw * 2.0;
    let box_right = w - cw * 2.5;
    if box_right - box_x <= cw * 3.0 {
        return;
    }
    let v_left = content_v_left(g);
    let v_right = content_v_right(g);
    let label_x = box_x + cw * 1.2;

    for (i, entry) in body_layout_masked(&state.fields, mask, state.scroll, band)
        .into_iter()
        .enumerate()
    {
        let y0 = band_top + i as f32 * ch;
        match entry {
            BodyRow::Header(sec) => {
                let hstep = TypeStep::Caption.px(px);
                prims.push(text_prim(
                    box_x + cw * 0.6,
                    row_baseline(y0, ch, hstep.get()),
                    sec.label().to_uppercase(),
                    hstep,
                    TextWeight::Regular,
                    TextFace::Ui,
                    rgba(r.text_secondary, 0xFF),
                ));
            }
            BodyRow::Control(idx) => {
                let Some(f) = state.fields.get(idx) else {
                    continue;
                };
                let selected = idx == state.selected;
                if selected {
                    prims.push(DrawPrim::Panel {
                        x: box_x,
                        y: y0 + 1.0,
                        w: box_right - box_x,
                        h: ch - 2.0,
                        radius: ch * 0.32,
                        fill: rgba(r.accent, SEL_WASH_ALPHA),
                        blur: false,
                    });
                    prims.push(DrawPrim::Stroke {
                        x: box_x,
                        y: y0 + 1.0,
                        w: box_right - box_x,
                        h: ch - 2.0,
                        radius: ch * 0.32,
                        width: 1.5,
                        color: rgba(r.accent, 0xCC),
                    });
                }
                if is_overridden(f) {
                    prims.push(DrawPrim::Dot {
                        cx: box_x - cw * 0.8,
                        cy: y0 + ch * 0.5,
                        r: (px * 0.16).max(2.0),
                        color: rgba(r.accent, 0xFF),
                        breathe: false,
                    });
                }
                let lstep = TypeStep::Body.px(px);
                prims.push(text_prim(
                    label_x,
                    row_baseline(y0, ch, lstep.get()),
                    f.label.to_string(),
                    lstep,
                    TextWeight::Regular,
                    TextFace::Ui,
                    rgba(r.text_primary, 0xFF),
                ));
                let editing = if selected {
                    state.editing.as_deref()
                } else {
                    None
                };
                build_widget(
                    prims, f, r, cw, ch, px, y0, v_left, v_right, selected, editing,
                );
            }
        }
    }

    // Empty result: an explanatory line (no rows matched the filter). No key hint —
    // hints are deleted throughout (design §8).
    if mask.is_some_and(|m| !m.iter().any(|b| *b)) {
        let estep = TypeStep::Secondary.px(px);
        prims.push(text_prim(
            label_x,
            row_baseline(band_top, ch, estep.get()),
            format!("No settings match \u{201c}{}\u{201d}", state.query.trim()),
            estep,
            TextWeight::Regular,
            TextFace::Ui,
            rgba(r.text_secondary, 0xFF),
        ));
    }

    // Scrollbar when the filtered list overflows the band. `state.scroll` is a
    // FIELD index while the thumb maps laid-out rows (headers included), so the
    // offset converts through the same full layout `total_rows` counts.
    let total_rows = body_layout_masked(&state.fields, mask, 0, usize::MAX).len();
    if total_rows > band {
        paint_scrollbar(
            prims,
            r,
            w - cw * 1.2,
            band_top,
            band as f32 * ch,
            (cw * 0.2).max(2.0),
            flat_rows_before(&state.fields, mask, state.scroll) as f32,
            band as f32,
            total_rows as f32,
            ch,
        );
    }
}

/// Paint the settings surface as ONE frosted two-pane card (design §1): the colorful-
/// icon SIDEBAR (search + six categories, [`paint_sidebar`]) and the CONTENT pane —
/// category title, the PINNED preview card ([`preview_card`]), the group-boxes (or the
/// flat search-result list), and a blank-at-rest status footer. PURE: no `App`, no GPU;
/// prims are card-relative device px composed from the substrate primitives, so the look
/// is captured WYSIWYG by the introspection path and renders identically on CPU and GPU.
///
/// STABILITY INVARIANT (feedback #2): every region rect comes from [`pane_geom`] — a
/// function of window size ONLY — so selection/focus/search move ZERO layout rectangles;
/// arrow keys repaint only the selection wash, the focus ring, and the preview card's
/// interior pixels. MOTION (design §8): the CPU tray has NO animation clock and SNAPS to
/// the settled end-state; every state change perturbs [`SettingsState::fingerprint`],
/// which forces exactly one present, so idle frames stay byte-identical (deterministic
/// capture). GPU/live animation is a strictly additive layer and NOT part of this
/// pure painter.
pub(crate) fn settings_tray(
    state: &SettingsState,
    g: &SettingsGeom,
    theme: Theme,
    ctx: PreviewCtx,
) -> TrayInput {
    let r = Roles::from_theme(theme);
    let (cw, ch, px) = (g.cw, g.ch, g.font_px);
    let w = g.cols as f32 * cw;
    let h = g.panel_rows as f32 * ch;
    let pg = pane_geom(g);
    let mut prims: Vec<DrawPrim> = Vec::new();

    // The frosted card (always prims[0]).
    prims.push(DrawPrim::Panel {
        x: 0.0,
        y: 0.0,
        w,
        h,
        radius: (ch * 0.6).min(14.0),
        fill: rgba(r.surface, 0xF4),
        blur: true,
    });

    // The LANDING page (§L) replaces the whole two-pane layout while up — the
    // ⌘, hero. Early return: the classic panel below stays byte-identical for
    // every non-landing open (headless drivers, tests, Get-started onward).
    if state.landing {
        paint_landing(&mut prims, state, g);
        return TrayInput {
            prims,
            card: (0.0, 0.0, w, h),
        };
    }

    // A free-positioned text run, cap-height-centred in the `row_h`-tall row at `y0`
    // (`row_baseline` — the leading-trim rule; sizes come off the named type scale),
    // in `face` (theirs' native Ui/UiBold or the Mono terminal face). The v2 painter
    // routes its content-pane title/count runs through this one funnel.
    #[allow(clippy::too_many_arguments)]
    let text = |prims: &mut Vec<DrawPrim>,
                x: f32,
                y0: f32,
                row_h: f32,
                size: crate::type_scale::StepPx,
                weight: TextWeight,
                face: TextFace,
                s: String,
                color: [u8; 3],
                alpha: u8| {
        if s.is_empty() {
            return;
        }
        prims.push(text_prim(
            x,
            row_baseline(y0, row_h, size.get()),
            s,
            size,
            weight,
            face,
            rgba(color, alpha),
        ));
    };

    let filtering = state.filtering();
    let mask = state.visible_mask();

    paint_sidebar(&mut prims, state, &r, theme, g, &pg);

    // ---- Content pane ----
    let content_x = pg.content_x(cw);
    let box_x = content_x + cw * 2.0;
    let box_right = w - cw * 2.5;
    let v_right = content_v_right(g);

    // Title (rows 1-2): the category's name — or the search heading + match count while
    // filtering. The "Settings" wordmark is GONE (the window titlebar already says it).
    if box_right - box_x > cw * 4.0 && g.panel_rows > CONTENT_TOP_ROW {
        let tsize = TypeStep::Title.px(px);
        let title = if filtering {
            "Search"
        } else {
            state.category.label()
        };
        // The pane heading: semibold system face (the System Settings title ramp).
        text(
            &mut prims,
            box_x,
            ch,
            2.0 * ch,
            tsize,
            TextWeight::Regular,
            TextFace::UiBold,
            title.to_string(),
            r.text_primary,
            0xFF,
        );
        if filtering {
            let shown = mask
                .as_ref()
                .map_or(state.fields.len(), |m| m.iter().filter(|b| **b).count());
            let count = format!("{} of {}", shown, state.fields.len());
            let csize = TypeStep::Caption.px(px);
            text(
                &mut prims,
                v_right - ui_text_width(&count, csize.get()),
                ch,
                2.0 * ch,
                csize,
                TextWeight::Regular,
                TextFace::Ui,
                count,
                r.text_tertiary,
                0xFF,
            );
        }
    }

    // The PINNED PREVIEW CARD (rows 3..12): present in EVERY category, always, at full
    // geometry — it never collapses with focus, only with a resize below the ladder.
    if pg.preview_shown() && box_right - box_x > cw * 2.0 {
        preview_card(
            &mut prims,
            state,
            &r,
            theme,
            ctx,
            box_x,
            box_right,
            pg.preview.0 as f32 * ch,
            pg.preview.1 as f32 * ch,
            cw,
            ch,
            px,
        );
    }

    // Group band: the active category's boxes, the Kitty Log collection book
    // (§F4.6 — a read-only page, no group-boxes to paint), or the flat
    // cross-category result list while the search filter is active.
    if filtering {
        paint_flat_band(&mut prims, state, &r, g, &pg, mask.as_deref());
    } else if state.category == prefs::Section::KittyLog {
        paint_kitty_book_band(&mut prims, state, &r, g, &pg);
    } else {
        paint_group_band(&mut prims, state, &r, theme, g, &pg);
    }

    // A kitty cameo summoned from the SEARCH field (§L.4) peeks down over the
    // sidebar from the card's top edge (the clip is its occluder). Panel mode
    // paints only the Sidebar host — the Landing host paints on the hero page.
    if let Some(k) = &state.kitty_pop
        && k.host == KittyHost::Sidebar
        && pg.sidebar_w_cells > 0.0
        && !pg.icon_strip
    {
        let r_head = ch * 0.85;
        prims.push(DrawPrim::ClipPush {
            x: 0.0,
            y: 0.0,
            w,
            h,
        });
        paint_kitty_cameo(
            &mut prims,
            k,
            state.landing_phase,
            cw * 1.2,
            pg.sidebar_w_cells * cw - cw * 2.4,
            -r_head * 1.1,
            -r_head * 2.2,
            r_head,
        );
        prims.push(DrawPrim::ClipPop);
    }

    // Footer (row R-1): transient status ONLY, coloured by outcome — BLANK at rest
    // (the idle "changes save…" tagline and every key hint are deleted, design §8).
    if let Some(m) = &state.status {
        let c = if m.starts_with("saved") {
            r.success
        } else if m.contains("fail") || m.contains("invalid") {
            r.danger
        } else {
            r.text_secondary
        };
        let fstep = TypeStep::Caption.px(px);
        text(
            &mut prims,
            content_x + cw,
            pg.footer_row as f32 * ch,
            ch,
            fstep,
            TextWeight::Regular,
            TextFace::Ui,
            m.clone(),
            c,
            0xFF,
        );
    }

    // The anchored popup MENU (Theme / long Enum) — drawn LAST so it composites over the
    // body, the preview, and the footer, and CLIPPED to the card so a long option list can
    // never spill past the rounded corners. Placement (below the anchor chip, or upward
    // near the bottom) and the option window come from [`menu_geom`] — the SAME geometry
    // the mouse hit-test consumes.
    if let Some(m) = &state.menu
        && let Some(mg) = menu_geom(state, g)
    {
        let is_theme = state
            .fields
            .get(m.field)
            .is_some_and(|f| matches!(f.kind, EditKind::Theme));
        let menu_h = mg.visible as f32 * mg.row_h;
        prims.push(DrawPrim::ClipPush {
            x: 0.0,
            y: 0.0,
            w,
            h,
        });
        // Faked elevation (design §5: no shadow prim — an offset dark panel beneath).
        prims.push(DrawPrim::Panel {
            x: mg.x + 2.0,
            y: mg.y + 3.0,
            w: mg.w,
            h: menu_h,
            radius: ch * 0.3,
            fill: rgba([0, 0, 0], 0x38),
            blur: false,
        });
        prims.push(DrawPrim::Panel {
            x: mg.x,
            y: mg.y,
            w: mg.w,
            h: menu_h,
            radius: ch * 0.3,
            fill: rgba(r.elevated, 0xFF),
            blur: false,
        });
        prims.push(DrawPrim::Stroke {
            x: mg.x,
            y: mg.y,
            w: mg.w,
            h: menu_h,
            radius: ch * 0.3,
            width: 1.0,
            color: rgba(r.separator, 0xAA),
        });
        let size = TypeStep::Secondary.px(px);
        for row in 0..mg.visible {
            let oi = mg.first + row;
            let Some(label) = m.options.get(oi) else {
                break;
            };
            let y0 = mg.y + row as f32 * mg.row_h;
            if oi == m.highlighted {
                prims.push(DrawPrim::Panel {
                    x: mg.x + 1.5,
                    y: y0 + 1.0,
                    w: mg.w - 3.0,
                    h: mg.row_h - 2.0,
                    radius: ch * 0.25,
                    fill: rgba(r.accent, MENU_WASH_ALPHA),
                    blur: false,
                });
            }
            // A leading accent dot marks the value in effect (committing it is a no-op).
            if oi == m.current {
                prims.push(DrawPrim::Dot {
                    cx: mg.x + cw * 0.9,
                    cy: y0 + mg.row_h * 0.5,
                    r: (px * 0.14).max(2.0),
                    color: rgba(r.accent, 0xFF),
                    breathe: false,
                });
            }
            let mut tx = mg.x + cw * 1.6;
            if is_theme {
                // Per-theme swatch strip (bg/fg/cursor/selection) so you pick by look;
                // empty for the preserved custom entry (no single scheme to swatch).
                let dot_r = (ch * 0.16).max(2.0);
                let mut scx = tx + dot_r;
                for c in &theme_swatches(label) {
                    prims.push(DrawPrim::Dot {
                        cx: scx,
                        cy: y0 + mg.row_h * 0.5,
                        r: dot_r,
                        color: rgba(*c, 0xFF),
                        breathe: false,
                    });
                    scx += dot_r * 2.1;
                }
                tx += 4.0 * dot_r * 2.1 + cw * 0.4;
            }
            prims.push(text_prim(
                tx,
                row_baseline(y0, mg.row_h, size.get()),
                label.clone(),
                size,
                TextWeight::Regular,
                TextFace::Ui,
                rgba(
                    if oi == m.highlighted {
                        r.text_primary
                    } else {
                        r.text_secondary
                    },
                    0xFF,
                ),
            ));
        }
        // Overflow thumb when the option list exceeds the popover window.
        if m.options.len() > mg.visible {
            let total = m.options.len() as f32;
            let th = (menu_h * mg.visible as f32 / total).max(ch * 0.5);
            let ty = mg.y + (mg.first as f32 / total) * (menu_h - th);
            prims.push(DrawPrim::Panel {
                x: mg.x + mg.w - cw * 0.35,
                y: ty,
                w: (cw * 0.18).max(2.0),
                h: th,
                radius: cw * 0.09,
                fill: rgba(r.text_tertiary, 0xCC),
                blur: false,
            });
        }
        prims.push(DrawPrim::ClipPop);
    }

    // The COLOUR-WHEEL popover (a Color row's picker, design §7) — drawn last and
    // clipped exactly like the menu above (the two popovers are mutually exclusive),
    // with the same faked elevation + elevated panel + hairline chrome. Placement
    // and every sub-control rect come from [`wheel_geom`] — the SAME geometry the
    // mouse hit-test + drag consume, so a press lands on the control painted there.
    if let Some(wst) = &state.wheel
        && let Some(wg) = wheel_geom(state, g)
    {
        use std::f32::consts::TAU;
        let cand = crate::widget::hsv_to_rgb(wst.h, wst.s, wst.v);
        prims.push(DrawPrim::ClipPush {
            x: 0.0,
            y: 0.0,
            w,
            h,
        });
        prims.push(DrawPrim::Panel {
            x: wg.x + 2.0,
            y: wg.y + 3.0,
            w: wg.w,
            h: wg.h,
            radius: ch * 0.3,
            fill: rgba([0, 0, 0], 0x38),
            blur: false,
        });
        prims.push(DrawPrim::Panel {
            x: wg.x,
            y: wg.y,
            w: wg.w,
            h: wg.h,
            radius: ch * 0.3,
            fill: rgba(r.elevated, 0xFF),
            blur: false,
        });
        prims.push(DrawPrim::Stroke {
            x: wg.x,
            y: wg.y,
            w: wg.w,
            h: wg.h,
            radius: ch * 0.3,
            width: 1.0,
            color: rgba(r.separator, 0xAA),
        });
        // The HSV disk (the one vocabulary addition — a per-pixel raster prim),
        // ringed while it owns the keyboard.
        prims.push(DrawPrim::HsvDisk {
            cx: wg.disk_cx,
            cy: wg.disk_cy,
            r: wg.disk_r,
            value: wst.v,
        });
        if wst.focus == WheelFocus::Wheel {
            prims.push(DrawPrim::Stroke {
                x: wg.disk_cx - wg.disk_r - 2.5,
                y: wg.disk_cy - wg.disk_r - 2.5,
                w: (wg.disk_r + 2.5) * 2.0,
                h: (wg.disk_r + 2.5) * 2.0,
                radius: wg.disk_r + 2.5,
                width: 1.5,
                color: rgba(r.accent, 0xCC),
            });
        }
        // Marker at the (h, s) polar point: the working colour inside an on_accent
        // ring (the slider-thumb idiom), legible over any hue beneath it.
        let mr = (px * 0.3).max(3.0);
        let mx = wg.disk_cx + (wst.h * TAU).sin() * wst.s * wg.disk_r;
        let my = wg.disk_cy - (wst.h * TAU).cos() * wst.s * wg.disk_r;
        prims.push(DrawPrim::Dot {
            cx: mx,
            cy: my,
            r: mr,
            color: rgba(cand, 0xFF),
            breathe: false,
        });
        prims.push(DrawPrim::Stroke {
            x: mx - mr,
            y: my - mr,
            w: mr * 2.0,
            h: mr * 2.0,
            radius: mr,
            width: 1.5,
            color: rgba(r.on_accent, 0xFF),
        });
        // Value slider under the disk: fill = the pure hue at full s/v so the track
        // reads "this hue, dark → bright"; the thumb ring accents while focused.
        let (sx, sy, sw, sh) = wg.slider;
        prims.push(DrawPrim::Capsule {
            x: sx,
            y: sy,
            w: sw,
            h: sh,
            frac: wst.v,
            fill: rgba(crate::widget::hsv_to_rgb(wst.h, 1.0, 1.0), 0xFF),
            track: rgba(r.control_track, 0xFF),
        });
        let tr = (sh * 0.9).max(4.0);
        let thumb_x = sx + wst.v * sw;
        prims.push(DrawPrim::Dot {
            cx: thumb_x,
            cy: sy + sh * 0.5,
            r: tr,
            color: rgba(r.on_accent, 0xFF),
            breathe: false,
        });
        prims.push(DrawPrim::Stroke {
            x: thumb_x - tr,
            y: sy + sh * 0.5 - tr,
            w: tr * 2.0,
            h: tr * 2.0,
            radius: tr,
            width: if wst.focus == WheelFocus::Value {
                1.5
            } else {
                1.0
            },
            color: rgba(
                if wst.focus == WheelFocus::Value {
                    r.accent
                } else {
                    r.separator
                },
                0xFF,
            ),
        });
        // Readout column: the colour ON OPEN over the candidate (old → new), each a
        // hairline-ringed well. Old is recomputed from the row (nothing committed
        // while the wheel is up, so the row still holds the opening value); an
        // UNSET row degrades to the live theme's colour FOR THAT KEY — the same
        // per-key fallback the wheel seeded from (design §7), so the "old" well
        // always shows the colour actually in effect.
        let old = state.fields.get(wst.field).map_or(r.accent, |f| {
            parse_hex(SettingsState::display_value(f))
                .unwrap_or_else(|| theme_color_for_key(theme, f.key))
        });
        for (rect, c) in [(wg.swatch_old, old), (wg.swatch_new, cand)] {
            let (x0, y0, w0, h0) = rect;
            prims.push(DrawPrim::Panel {
                x: x0,
                y: y0,
                w: w0,
                h: h0,
                radius: h0 * 0.3,
                fill: rgba(c, 0xFF),
                blur: false,
            });
            prims.push(DrawPrim::Stroke {
                x: x0,
                y: y0,
                w: w0,
                h: h0,
                radius: h0 * 0.3,
                width: 1.0,
                color: rgba(r.separator, 0xAA),
            });
        }
        // Hex readout: a rounded framed field (accented while focused) + caret.
        let (hx0, hy0, hw0, hh0) = wg.hex;
        let hex_focus = wst.focus == WheelFocus::Hex;
        prims.push(DrawPrim::Stroke {
            x: hx0,
            y: hy0,
            w: hw0,
            h: hh0,
            radius: hh0 * 0.35,
            width: if hex_focus { 1.5 } else { 1.0 },
            color: rgba(if hex_focus { r.accent } else { r.control_track }, 0xFF),
        });
        let hstep = TypeStep::Secondary.px(px);
        let hsize = hstep.get();
        let htx = hx0 + cw * 0.4;
        // Mono: the hex field is a code (same rule as the colour row's readout).
        prims.push(text_prim(
            htx,
            row_baseline(hy0, hh0, hsize),
            wst.hex.clone(),
            hstep,
            TextWeight::Regular,
            TextFace::Mono,
            rgba(r.text_primary, 0xFF),
        ));
        if hex_focus {
            prims.push(DrawPrim::Stroke {
                x: htx + text_w(&wst.hex, hsize) + 1.0,
                y: hy0 + 2.0,
                w: 1.0,
                h: hh0 - 4.0,
                radius: 0.0,
                width: 1.0,
                color: rgba(r.accent, 0xFF),
            });
        }
        // The ONE surviving key hint (design §7/§8): a modal colour wheel has no
        // native muscle memory in a terminal. Sized into the geometry's right
        // column ([`wheel_hint_w`]), so it stays inside the popover. Two prims:
        // the "↵" KEYCAP stays mono (SF Pro has no U+21B5 — it strikes notdef),
        // the words read in the UI face like every other hint line.
        let hint_step = TypeStep::Caption.px(px);
        let hint_px = hint_step.get();
        let hint_baseline = hy0 + hh0 + cw * 0.4 + hint_px;
        prims.push(text_prim(
            hx0,
            hint_baseline,
            WHEEL_HINT_KEY.to_string(),
            hint_step,
            TextWeight::Regular,
            TextFace::Mono,
            rgba(r.text_tertiary, 0xFF),
        ));
        prims.push(text_prim(
            hx0 + text_w(WHEEL_HINT_KEY, hint_px),
            hint_baseline,
            WHEEL_HINT_WORDS.to_string(),
            hint_step,
            TextWeight::Regular,
            TextFace::Ui,
            rgba(r.text_tertiary, 0xFF),
        ));
        prims.push(DrawPrim::ClipPop);
    }

    TrayInput {
        prims,
        card: (0.0, 0.0, w, h),
    }
}

/// Card-relative device-px placement of the open popup menu — the ONE geometry source
/// the painter draws from AND the mouse hit-test maps clicks with, so a click always
/// lands on the option that is painted under it.
pub(crate) struct MenuGeom {
    /// Popover top-left (card-relative device px).
    pub(crate) x: f32,
    pub(crate) y: f32,
    /// Popover width.
    pub(crate) w: f32,
    /// Height of one option row (== the cell row height, so clicks map like body rows).
    pub(crate) row_h: f32,
    /// First option index shown (the menu's clamped scroll offset).
    pub(crate) first: usize,
    /// Number of option rows the popover shows (≤ the option count).
    pub(crate) visible: usize,
}

/// Compute the open menu's placement: anchored to its row's chip (right-aligned at the
/// group box's value edge), opening DOWNWARD when the options fit between the row and
/// the footer, else UPWARD — always inside the card. The anchor row's y comes from the
/// SAME layout walk the painter renders (grouped, or the flat search list while
/// filtering). `None` when no menu is open.
pub(crate) fn menu_geom(state: &SettingsState, g: &SettingsGeom) -> Option<MenuGeom> {
    let m = state.menu.as_ref()?;
    let (cw, ch, px) = (g.cw, g.ch, g.font_px);
    let w = g.cols as f32 * cw;
    let pg = pane_geom(g);
    let band = pg.group_band();
    let band_top = pg.groups.0 as f32 * ch;
    // The anchor row's y + height in the painted band; falls back to the band top
    // defensively (the scroll clamps keep the anchor visible before a menu opens).
    let (anchor_y, anchor_h) = if state.filtering() {
        let mask = state.visible_mask();
        let y = body_layout_masked(&state.fields, mask.as_deref(), state.scroll, band)
            .iter()
            .position(|r| matches!(r, BodyRow::Control(i) if *i == m.field))
            .map_or(band_top, |i| band_top + i as f32 * ch);
        (y, ch)
    } else {
        let rows = category_layout(&state.fields, state.category, footnote_wrap_chars(g.cols));
        let mut cells = 0usize;
        let mut y = None;
        for row in rows.iter().skip(state.scroll.min(rows.len())) {
            let rh = group_row_cells(row);
            if cells + rh > band {
                break;
            }
            if matches!(row, GroupRow::Control(j) if *j == m.field) {
                y = Some(band_top + cells as f32 * ch);
                break;
            }
            cells += rh;
        }
        (y.unwrap_or(band_top), 2.0 * ch)
    };

    // Row budget above/below the anchor, between the title band and the footer.
    let n = m.options.len();
    let footer_y = pg.footer_row as f32 * ch;
    let below_rows = (((footer_y - (anchor_y + anchor_h)) / ch).floor().max(0.0)) as usize;
    let above_rows = ((((anchor_y - ch) / ch).floor()).max(0.0)) as usize;
    let want = n.min(band.max(1));
    let (visible, y) = if below_rows >= want || below_rows >= above_rows {
        let visible = n.min(below_rows).max(1);
        (visible, anchor_y + anchor_h)
    } else {
        let visible = n.min(above_rows).max(1);
        (visible, anchor_y - visible as f32 * ch)
    };

    // Width: leading marker gutter + (theme) swatch strip + the widest option label.
    let size = TypeStep::Secondary.px(px);
    let is_theme = matches!(state.fields.get(m.field)?.kind, EditKind::Theme);
    let dot_r = (ch * 0.16).max(2.0);
    let swatch_w = if is_theme {
        4.0 * dot_r * 2.1 + cw * 0.4
    } else {
        0.0
    };
    let text_max = m
        .options
        .iter()
        .map(|o| ui_text_width(o, size.get()))
        .fold(0.0_f32, f32::max);
    let w_menu = fit(cw * 1.6 + swatch_w + text_max + cw * 1.6, cw * 8.0, w - cw);
    let x = (content_v_right(g) - w_menu).max(cw * 0.5);

    Some(MenuGeom {
        x,
        y,
        w: w_menu,
        row_h: ch,
        first: m.scroll.min(n.saturating_sub(visible)),
        visible,
    })
}

/// Map a card-relative device-px point to the menu OPTION index under it, or `None`
/// when the point is outside the open popover (or no menu is open). Shares
/// [`menu_geom`] with the painter, so hit == pixels.
pub(crate) fn menu_hit(state: &SettingsState, g: &SettingsGeom, x: f32, y: f32) -> Option<usize> {
    let mg = menu_geom(state, g)?;
    let menu_h = mg.visible as f32 * mg.row_h;
    if x < mg.x || x >= mg.x + mg.w || y < mg.y || y >= mg.y + menu_h {
        return None;
    }
    let row = ((y - mg.y) / mg.row_h) as usize;
    let idx = mg.first + row.min(mg.visible.saturating_sub(1));
    (idx < state.menu.as_ref()?.options.len()).then_some(idx)
}

/// Card-relative device-px placement of the open colour-wheel popover — the ONE
/// geometry source the painter draws from AND the mouse hit-test / drag math map
/// clicks with (the [`menu_geom`] pattern), so press == pixels for every
/// sub-control.
pub(crate) struct WheelGeom {
    /// Popover rect (card-relative device px).
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) w: f32,
    pub(crate) h: f32,
    /// The HSV disk's centre + radius.
    pub(crate) disk_cx: f32,
    pub(crate) disk_cy: f32,
    pub(crate) disk_r: f32,
    /// The value slider's track rect `(x, y, w, h)`.
    pub(crate) slider: (f32, f32, f32, f32),
    /// The hex readout's framed rect.
    pub(crate) hex: (f32, f32, f32, f32),
    /// The colour-on-open / candidate swatch wells (old above new).
    pub(crate) swatch_old: (f32, f32, f32, f32),
    pub(crate) swatch_new: (f32, f32, f32, f32),
}

/// The wheel popover's footer hint — the ONE surviving key hint (design §7/§8),
/// sized into [`wheel_geom`]'s right column so it can never spill the popover.
/// Split into the "↵" KEYCAP (mono — SF Pro carries no U+21B5) and the UI-face
/// words, so painter and measure agree per part.
const WHEEL_HINT_KEY: &str = "\u{21B5}";
const WHEEL_HINT_WORDS: &str = " apply  esc cancel";

/// The hint line's painted width at size `px`: the mono keycap + the UI words,
/// each in its own metric (exactly how the painter strikes them).
fn wheel_hint_w(px: f32) -> f32 {
    text_w(WHEEL_HINT_KEY, px) + ui_text_width(WHEEL_HINT_WORDS, px)
}

/// Compute the open wheel's placement: anchored under its Color row's widget
/// (right-aligned at the content value edge, like [`menu_geom`]), flipping ABOVE
/// the row when the footer is too close — always inside the card. The disk
/// shrinks before the popover would spill a short retired-card test fixture.
/// `None` when no wheel is open.
pub(crate) fn wheel_geom(state: &SettingsState, g: &SettingsGeom) -> Option<WheelGeom> {
    let wst = state.wheel.as_ref()?;
    let (cw, ch, px) = (g.cw, g.ch, g.font_px);
    let w = g.cols as f32 * g.cw;
    let pg = pane_geom(g);
    let band = pg.group_band();
    let band_top = pg.groups.0 as f32 * ch;
    // The anchor row's y + height, from the SAME layout walk the painter renders
    // (grouped, or the flat search list while filtering) — mirrors `menu_geom`.
    let (anchor_y, anchor_h) = if state.filtering() {
        let mask = state.visible_mask();
        let y = body_layout_masked(&state.fields, mask.as_deref(), state.scroll, band)
            .iter()
            .position(|r| matches!(r, BodyRow::Control(i) if *i == wst.field))
            .map_or(band_top, |i| band_top + i as f32 * ch);
        (y, ch)
    } else {
        let rows = category_layout(&state.fields, state.category, footnote_wrap_chars(g.cols));
        let mut cells = 0usize;
        let mut y = None;
        for row in rows.iter().skip(state.scroll.min(rows.len())) {
            let rh = group_row_cells(row);
            if cells + rh > band {
                break;
            }
            if matches!(row, GroupRow::Control(j) if *j == wst.field) {
                y = Some(band_top + cells as f32 * ch);
                break;
            }
            cells += rh;
        }
        (y.unwrap_or(band_top), 2.0 * ch)
    };

    let footer_y = pg.footer_row as f32 * ch;
    let pad = cw * 0.8;
    let slider_h = fit(ch * 0.45, 4.0, ch);
    // Disk radius ≈ 4.2·ch (design §7), shrunk so the whole popover still fits
    // between the title band and the footer on a short card.
    let chrome_h = pad * 2.0 + ch * 0.6 + slider_h;
    let disk_r = fit(
        ch * 4.2,
        4.0,
        ((footer_y - 2.0 * ch - chrome_h) * 0.5).max(4.0),
    );
    // Right column: swatch wells + the widest content (hex readout / footer hint) —
    // each measured in the face it PAINTS in (hex is mono, the hint is split).
    let right_w = (text_w("#RRGGBB", px * 0.92) + cw * 2.4)
        .max(wheel_hint_w(px * 0.72) + cw * 1.2)
        .max(cw * 7.0);
    let w_pop = fit(pad * 3.0 + 2.0 * disk_r + right_w, cw * 6.0, w - cw);
    let h_pop = pad * 2.0 + 2.0 * disk_r + ch * 0.6 + slider_h;
    let below_y = anchor_y + anchor_h;
    let y = if below_y + h_pop <= footer_y {
        below_y
    } else {
        // Flip above the anchor; a card too short for either pins below the title.
        fit(anchor_y - h_pop, ch, (footer_y - h_pop).max(ch))
    };
    let x = (content_v_right(g) - w_pop).max(cw * 0.5);

    let disk_cx = x + pad + disk_r;
    let disk_cy = y + pad + disk_r;
    let slider = (
        x + pad,
        y + pad + 2.0 * disk_r + ch * 0.6,
        2.0 * disk_r,
        slider_h,
    );
    let col_x = x + pad * 2.0 + 2.0 * disk_r;
    let col_w = (x + w_pop - pad - col_x).max(cw * 2.0);
    // The right column (swatch pair, hex readout, and the hint the painter draws
    // under it) lays out at natural heights, COMPRESSED to fit whenever the disk
    // shrink makes `h_pop` shorter than the column — `h_pop` is derived from the
    // disk column only, so without this the hex field would paint BELOW the
    // popover where `wheel_hit` (bounded by `h_pop`) cannot reach it and a click
    // on the visible field would cancel the wheel (paint == hit is the invariant).
    let sw_h = ch * 0.8;
    let hint_h = px * 0.72 + cw * 0.4; // the wheel-hint line under the hex field
    let col_natural = 2.0 * sw_h + 3.0 + ch * 0.5 + ch * 0.9;
    let col_avail = (h_pop - pad * 2.0 - hint_h).max(0.0);
    let k = (col_avail / col_natural).min(1.0);
    let swatch_old = (col_x, y + pad, col_w, sw_h * k);
    let swatch_new = (col_x, y + pad + (sw_h + 3.0) * k, col_w, sw_h * k);
    let hex = (
        col_x,
        y + pad + (2.0 * sw_h + 3.0 + ch * 0.5) * k,
        col_w,
        ch * 0.9 * k,
    );
    Some(WheelGeom {
        x,
        y,
        w: w_pop,
        h: h_pop,
        disk_cx,
        disk_cy,
        disk_r,
        slider,
        hex,
        swatch_old,
        swatch_new,
    })
}

/// What a card-relative point hits inside the open wheel popover. `None` when the
/// point is OUTSIDE the popover entirely (a click-away — the caller cancels).
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum WheelHit {
    /// On the disk: the polar (hue, saturation) at the point.
    Disk { h: f32, s: f32 },
    /// On the value slider: the fraction at the point's x.
    Slider { v: f32 },
    /// On the hex readout (focus it).
    Hex,
    /// Inside the popover chrome (swallowed; the popover stays up).
    Body,
}

/// Map a card-relative device-px point to the wheel control under it. Shares
/// [`wheel_geom`] with the painter, so hit == pixels; the disk/slider arms read
/// the SAME [`disk_hs_at`]/[`slider_v_at`] math the mouse DRAG scrubs with.
pub(crate) fn wheel_hit(
    state: &SettingsState,
    g: &SettingsGeom,
    x: f32,
    y: f32,
) -> Option<WheelHit> {
    let wg = wheel_geom(state, g)?;
    if x < wg.x || x >= wg.x + wg.w || y < wg.y || y >= wg.y + wg.h {
        return None;
    }
    let dx = x - wg.disk_cx;
    let dy = y - wg.disk_cy;
    if (dx * dx + dy * dy).sqrt() <= wg.disk_r + 2.0 {
        let (h, s) = disk_hs_at(&wg, x, y);
        return Some(WheelHit::Disk { h, s });
    }
    let (sx, sy, sw, sh) = wg.slider;
    // A vertical slop band makes the thin capsule a fair Fitts target.
    if x >= sx && x < sx + sw && y >= sy - sh && y < sy + sh * 2.0 {
        return Some(WheelHit::Slider {
            v: slider_v_at(&wg, x),
        });
    }
    let (hx, hy, hw, hh) = wg.hex;
    if x >= hx && x < hx + hw && y >= hy && y < hy + hh {
        return Some(WheelHit::Hex);
    }
    Some(WheelHit::Body)
}

/// The (hue, saturation) the disk reads at a card-relative point: angle → hue
/// (0 at 12 o'clock, clockwise — the SAME convention the raster fills with),
/// radius → saturation, CLAMPED to the rim so a drag past the edge stays fully
/// saturated instead of jumping.
pub(crate) fn disk_hs_at(wg: &WheelGeom, x: f32, y: f32) -> (f32, f32) {
    use std::f32::consts::TAU;
    let dx = x - wg.disk_cx;
    let dy = y - wg.disk_cy;
    let h = dx.atan2(-dy).rem_euclid(TAU) / TAU;
    let s = ((dx * dx + dy * dy).sqrt() / wg.disk_r.max(1.0)).clamp(0.0, 1.0);
    (h, s)
}

/// The value fraction the slider reads at a card-relative x, clamped to the track.
pub(crate) fn slider_v_at(wg: &WheelGeom, x: f32) -> f32 {
    let (sx, _, sw, _) = wg.slider;
    ((x - sx) / sw.max(1.0)).clamp(0.0, 1.0)
}

/// The card-relative x where control `idx`'s WIDGET region begins (with a one-cell hit
/// slop): a click at `x >=` this on the already-selected row activates it; left of it
/// (the label region) only selects. Reuses [`build_widget`]'s own returned edge on a
/// scratch prim list, so the hit boundary is BY CONSTRUCTION the painted widget's edge
/// (geometry is theme-independent).
pub(crate) fn widget_hit_left(state: &SettingsState, g: &SettingsGeom, idx: usize) -> Option<f32> {
    let f = state.fields.get(idx)?;
    let r = Roles::from_theme(Theme::default());
    let mut scratch = Vec::new();
    let left = build_widget(
        &mut scratch,
        f,
        &r,
        g.cw,
        g.ch,
        g.font_px,
        0.0,
        content_v_left(g),
        content_v_right(g),
        true,
        None,
    );
    Some((left - g.cw).max(0.0))
}

/// A full-width blank row of `cols` cells in `fg`/`bg` (the `seam` overline marks the
/// panel's top edge on row 0).
pub(crate) fn blank_row(cols: usize, fg: [u8; 3], bg: [u8; 3], seam: bool) -> Vec<RenderCell> {
    vec![chrome_band::cell(' ', fg, bg, false, seam); cols]
}

/// Write `s` into `row` starting at column `col`, clamped to the row width. Each glyph
/// becomes a `chrome_band::cell` in `fg`/`bg`. Multi-cell-wide glyphs are not expected here
/// (labels/values are ASCII + a few BMP arrows), so one char == one cell.
pub(crate) fn write_str(
    row: &mut [RenderCell],
    cols: usize,
    mut col: usize,
    s: &str,
    fg: [u8; 3],
    bg: [u8; 3],
    bold: bool,
) {
    for ch in s.chars() {
        if col >= cols {
            break;
        }
        row[col] = chrome_band::cell(ch, fg, bg, bold, false);
        col += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prefs::CURSOR_TRAIL_STYLES;

    fn cfg() -> Config {
        Config::default()
    }

    #[test]
    fn landing_identifies_author_and_company() {
        let mut s = SettingsState::from_config(&cfg());
        s.landing = true;
        let g = SettingsGeom {
            cw: 9.0,
            ch: 20.0,
            font_px: 15.0,
            cols: 132,
            panel_rows: 38,
        };
        let tray = settings_tray(&s, &g, Theme::default(), PreviewCtx::default());
        assert!(tray.prims.iter().any(
            |p| matches!(p, DrawPrim::Text { s, .. } if s == "By Andrew Yates \u{00b7} ALab")
        ));
    }

    /// §L.4: at the HOLD phase both cameo hosts must paint the cat head INSIDE
    /// the visible card — the landing pop fully above the suggestion box, the
    /// sidebar peek descended below the card's top edge. Guards the rise/peek
    /// envelope signs (a flipped peak hides the cat forever, invisibly).
    #[test]
    fn kitty_cameo_pops_into_view_at_hold_phase() {
        let g = SettingsGeom {
            cw: 9.0,
            ch: 20.0,
            font_px: 15.0,
            cols: 132,
            panel_rows: 38,
        };
        // The head is the one mid-sized Dot (blobs are huge, ears/eyes tiny).
        let head = |t: &TrayInput| {
            t.prims
                .iter()
                .find_map(|p| match p {
                    DrawPrim::Dot { cy, r, .. } if *r > g.ch * 0.7 && *r < g.ch * 1.2 => {
                        Some((*cy, *r))
                    }
                    _ => None,
                })
                .expect("cameo head Dot painted")
        };

        let mut s = SettingsState::from_config(&cfg());
        s.landing = true;
        s.kitty_pop = Some(KittyPop {
            breed: 0,
            x_frac: 0.5,
            start: 0,
            host: KittyHost::Landing,
        });
        s.landing_phase = KITTY_POP_TICKS / 2;
        let (cy, r) = head(&settings_tray(
            &s,
            &g,
            Theme::default(),
            PreviewCtx::default(),
        ));
        let box_top = landing_geom(&g).tbox.1;
        assert!(
            cy + r <= box_top + r * 0.5,
            "landing cameo must rise clear of the box: head bottom {} vs box top {}",
            cy + r,
            box_top
        );

        s.landing = false;
        s.kitty_pop = Some(KittyPop {
            breed: 0,
            x_frac: 0.5,
            start: 0,
            host: KittyHost::Sidebar,
        });
        let (cy, r) = head(&settings_tray(
            &s,
            &g,
            Theme::default(),
            PreviewCtx::default(),
        ));
        assert!(
            cy > r * 0.05 && cy < g.ch * 4.0,
            "sidebar cameo must peek down into the card: cy {} r {}",
            cy,
            r
        );
    }

    /// A configured ENUM ALIAS (e.g. cursor_style "beam" == bar) must display + cycle from
    /// its canonical option, never silently substitute `options[0]` (which would contradict
    /// the effective config). Regression guard for the alias-resolution fix.
    #[test]
    fn enum_current_resolves_documented_aliases() {
        let field = |seed: &str| EditField {
            label: "Cursor style",
            key: crate::prefs::EDIT_CURSOR_STYLE,
            kind: EditKind::Enum {
                options: crate::prefs::CURSOR_STYLES, // [block, bar] — underline retired
            },
            seed: Some(seed.to_string()),
            placeholder: String::new(),
        };
        // "beam" is the documented alias for "bar" — show + cycle from bar, not block.
        assert_eq!(enum_current(&field("beam")), "bar");
        assert_eq!(
            cycle_edit(&field("beam")).unwrap().1.as_deref(),
            Some("block")
        );
        // The loader still accepts retired `underline` and renders it as a bar.
        assert_eq!(enum_current(&field("underline")), "bar");
        // A genuinely unknown spelling falls back to the first option.
        assert_eq!(enum_current(&field("zzz")), "block");

        // Trail-style aliases resolve through the shared prefs table: a
        // configured "rainbow" (or legacy "nyan"/"nyan rainbow") renders the
        // banded ribbon LIVE, so the panel row (and the demo lane + cycle anchor
        // riding on it) must show its canonical name — previously every alias
        // clobbered to options[0] = "phaser" while the glass played a different
        // effect.
        //
        // Those three canonicalise to "rainbow kitty FLYING" since 2026-08-26.
        // They draw the flying head and always have; `rainbow kitty` now draws
        // the walking pet, so pointing them there would have made this row
        // display a companion the engine does not draw for those configs — the
        // display/engine split this alias table exists to prevent.
        let trail = |seed: &str| EditField {
            label: "Trail effect",
            key: crate::prefs::EDIT_CURSOR_TRAIL_STYLE,
            kind: EditKind::Enum {
                options: crate::prefs::CURSOR_TRAIL_STYLES,
            },
            seed: Some(seed.to_string()),
            placeholder: String::new(),
        };
        assert_eq!(enum_current(&trail("rainbow")), "rainbow kitty flying");
        assert_eq!(enum_current(&trail("nyan")), "rainbow kitty flying");
        assert_eq!(enum_current(&trail("nyan rainbow")), "rainbow kitty flying");
        // …and the kitty-named spellings show the resident's canonical name.
        assert_eq!(enum_current(&trail("kitty")), "rainbow kitty");
        assert_eq!(enum_current(&trail("kitty pet")), "rainbow kitty pet");
        assert_eq!(enum_current(&trail("flying kitty")), "rainbow kitty flying");
        assert_eq!(enum_current(&trail("embers")), "fire");
        assert_eq!(enum_current(&trail("ocean")), "water");
        assert_eq!(enum_current(&trail("light-beam")), "beam");
        // An unknown spelling falls back to what the RUNTIME draws for it, not
        // to options[0]: `app_config::resolve_trail_style` substitutes the
        // default style and renders, so the chip, the cycle anchor and the demo
        // lane all name the trail that is actually on screen. (The popup's
        // verbatim-preserve arm lives in `popup_options`.)
        assert_eq!(
            enum_current(&trail("plasma")),
            crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE
        );
        assert_ne!(
            crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE,
            crate::prefs::CURSOR_TRAIL_STYLES[0],
            "the assertion above is vacuous if the default IS options[0]"
        );
    }

    #[test]
    fn enum_runtime_aliases_normalize_in_structured_display() {
        let field = |key: &'static str, seed: &str| EditField {
            label: key,
            key,
            kind: crate::prefs::edit_kind(key),
            seed: Some(seed.to_string()),
            placeholder: String::new(),
        };
        let aliases = [
            (crate::prefs::EDIT_CURSOR_STYLE, "beam", "bar"),
            (crate::prefs::EDIT_CURSOR_STYLE, "underline", "bar"),
            (crate::prefs::EDIT_BIDI, "off", "disabled"),
            (crate::prefs::EDIT_BIDI, "on", "implicit"),
            (crate::prefs::EDIT_AMBIGUOUS_WIDTH, "single", "narrow"),
            (crate::prefs::EDIT_AMBIGUOUS_WIDTH, "double", "wide"),
            (crate::prefs::EDIT_PREDICTIVE_ECHO, "auto", "adaptive"),
            (crate::prefs::EDIT_PREDICTIVE_ECHO, "on", "adaptive"),
            (crate::prefs::EDIT_PREDICTIVE_ECHO, "true", "adaptive"),
            (crate::prefs::EDIT_PREDICTIVE_ECHO, "force", "always"),
            (
                crate::prefs::EDIT_TEXT_BLENDING,
                "linear_corrected",
                "linear-corrected",
            ),
            (crate::prefs::EDIT_MOTION, "reduce", "reduced"),
            (
                crate::prefs::EDIT_WINDOW_COLORSPACE,
                "displayp3",
                "display-p3",
            ),
            (crate::prefs::EDIT_WINDOW_COLORSPACE, "p3", "display-p3"),
            (
                crate::prefs::EDIT_BACKGROUND_MATERIAL,
                "underwindow",
                "under-window",
            ),
            (
                crate::prefs::EDIT_BACKGROUND_MATERIAL,
                "under_window",
                "under-window",
            ),
            (crate::prefs::EDIT_BACKGROUND_MATERIAL, "", "none"),
        ];
        for (key, alias, canonical) in aliases {
            let f = field(key, alias);
            assert_eq!(
                SettingsState::display_value(&f),
                canonical,
                "structured display for {key}={alias:?}"
            );
            assert_eq!(enum_current(&f), canonical, "current {key}={alias:?}");
            let EditKind::Enum { options } = f.kind else {
                panic!("alias key {key} must remain an enum");
            };
            let popup = popup_options(&f);
            assert_eq!(
                popup.len(),
                options.len(),
                "recognized alias {key}={alias:?} must not become a custom option"
            );
            assert_eq!(popup[popup_current_index(&f, &popup)], canonical);
            let canonical_index = options
                .iter()
                .position(|option| option.eq_ignore_ascii_case(canonical))
                .unwrap();
            assert_eq!(
                cycle_edit(&f).unwrap().1.as_deref(),
                Some(options[(canonical_index + 1) % options.len()]),
                "cycling must start from the runtime value of {key}={alias:?}"
            );
        }

        for &(alias, canonical) in crate::prefs::CURSOR_TRAIL_STYLE_ALIASES {
            let f = field(crate::prefs::EDIT_CURSOR_TRAIL_STYLE, alias);
            assert_eq!(SettingsState::display_value(&f), canonical, "{alias}");
            assert_eq!(enum_current(&f), canonical, "{alias}");
            assert_eq!(
                popup_options(&f).len(),
                crate::prefs::CURSOR_TRAIL_STYLES.len(),
                "trail alias {alias:?} must not become a custom option"
            );
        }

        // The typing-sound picker: every synth alias (water → droplet, mech →
        // mechanical, bell → glass bell, …) projects onto its picker row, the
        // row is a POPUP chip (14 options), and ←/→ steps the roster.
        for &(alias, voice) in aterm_effects::trail_sound::SoundVoice::ALIASES {
            let canonical = voice.name();
            let f = field(crate::prefs::EDIT_TRAIL_SOUND_STYLE, alias);
            assert!(uses_popup(&f), "typing sound is a popup chip");
            assert_eq!(SettingsState::display_value(&f), canonical, "{alias}");
            assert_eq!(enum_current(&f), canonical, "{alias}");
            assert_eq!(
                popup_options(&f).len(),
                crate::prefs::TRAIL_SOUND_STYLES.len(),
                "typing-sound alias {alias:?} must not become a custom option"
            );
        }
        let f = field(crate::prefs::EDIT_TRAIL_SOUND_STYLE, "water");
        assert_eq!(enum_current(&f), "droplet");
        assert_eq!(cycle_edit(&f).unwrap().1.as_deref(), Some("pew"));
    }

    #[test]
    fn enum_candidate_preserves_annotated_defaults_and_full_custom_values() {
        let trail = EditField {
            label: "Trail style",
            key: crate::prefs::EDIT_CURSOR_TRAIL_STYLE,
            kind: EditKind::Enum {
                options: crate::prefs::CURSOR_TRAIL_STYLES,
            },
            seed: None,
            placeholder: "rainbow kitty (default)".to_string(),
        };
        assert_eq!(
            SettingsState::display_value(&trail),
            "rainbow kitty (default)",
            "native display keeps the explanatory default annotation"
        );
        assert_eq!(enum_current(&trail), "rainbow kitty");
        assert_eq!(
            popup_options(&trail).len(),
            crate::prefs::CURSOR_TRAIL_STYLES.len(),
            "the multi-word default is canonical, not a custom entry"
        );

        let motion = EditField {
            label: "Motion",
            key: crate::prefs::EDIT_MOTION,
            kind: crate::prefs::edit_kind(crate::prefs::EDIT_MOTION),
            seed: None,
            placeholder: crate::prefs::motion_auto_placeholder().to_string(),
        };
        assert_eq!(enum_current(&motion), "auto");

        let custom = EditField {
            seed: Some("future multi word value".to_string()),
            ..trail
        };
        assert_eq!(
            SettingsState::display_value(&custom),
            "future multi word value"
        );
        let popup = popup_options(&custom);
        assert_eq!(popup[0], "future multi word value");
        assert_eq!(popup_current_index(&custom, &popup), 0);
    }

    fn geom(panel_rows: usize) -> SettingsGeom {
        SettingsGeom {
            cw: 8.0,
            ch: 16.0,
            font_px: 13.0,
            cols: 80,
            panel_rows,
        }
    }

    /// The dedicated window's target geometry (design §1): full layout — 26-cell
    /// sidebar, pinned preview, group band.
    fn full_geom() -> SettingsGeom {
        SettingsGeom {
            cw: 8.0,
            ch: 16.0,
            font_px: 13.0,
            cols: 132,
            panel_rows: 38,
        }
    }

    /// `settings_tray` with the default (light-appearance) render context.
    fn tray(s: &SettingsState, g: &SettingsGeom) -> TrayInput {
        settings_tray(s, g, Theme::default(), PreviewCtx::default())
    }

    fn sel_wash_count(prims: &[DrawPrim]) -> usize {
        prims
            .iter()
            .filter(|p| matches!(p, DrawPrim::Panel { fill, .. } if fill[3] == SEL_WASH_ALPHA))
            .count()
    }

    /// The cfg(test)-only retired overlay painter must not panic at extreme geometry.
    /// Its widget painter clamps several lengths, and `f32::clamp(lo, hi)` aborts
    /// when `lo > hi`: a tiny font drops the max widget height (`px*1.1`) below
    /// the 8px floor, and an ultra-narrow grid (≤15 cols) drops the available
    /// input width below the desired minimum. Either crashed the app the instant
    /// the old card opened. (audit/501cecb clamp panics — exercises sites 664/670/836)
    #[test]
    fn settings_tray_survives_extreme_geometry() {
        let mut s = SettingsState::from_config(&cfg());
        let rows = wanted_rows(&s.fields);

        // Tiny font: px*1.1 < 8.0 (site 664, every row).
        let tiny = SettingsGeom {
            cw: 4.0,
            ch: 9.0,
            font_px: 6.0,
            cols: 80,
            panel_rows: rows,
        };
        let _ = tray(&s, &tiny);

        // Ultra-narrow grid: v_right - cw*8 < cw*6 (site 836, at-rest text rows).
        let narrow = SettingsGeom {
            cw: 8.0,
            ch: 16.0,
            font_px: 13.0,
            cols: 12,
            panel_rows: rows,
        };
        let _ = tray(&s, &narrow);

        // Editing a free-form row in a narrow grid (site 670, the editing branch).
        s.selected = 0;
        s.editing = Some("typed-value".to_string());
        let _ = tray(&s, &narrow);

        // Both pathologies at once — the worst case.
        let both = SettingsGeom {
            cw: 3.0,
            ch: 7.0,
            font_px: 6.0,
            cols: 8,
            panel_rows: rows,
        };
        let _ = tray(&s, &both);

        // A short headless retired-overlay fixture at every layout ladder rung.
        for cols in [132, 96, 80, 64, 50, 24, 12] {
            let short = SettingsGeom {
                cw: 8.0,
                ch: 16.0,
                font_px: 13.0,
                cols,
                panel_rows: 6,
            };
            let _ = tray(&s, &short);
        }
    }

    /// A search filter matching NOTHING parks the selection on a hidden row, so
    /// `action_target` (which gates activate / reset / edit) must return None —
    /// otherwise Del/Enter would mutate and PERSIST a control the user cannot see
    /// (the body shows "No settings match"). (audit/501cecb hidden-control mutation)
    #[test]
    fn no_match_filter_has_no_action_target() {
        let mut s = SettingsState::from_config(&cfg());
        assert!(
            s.action_target().is_some(),
            "unfiltered: selected row is actionable"
        );

        s.search_begin();
        for c in "zzqqxx".chars() {
            s.search_push(c);
        }
        assert!(s.visible_indices().is_empty(), "query matches no control");
        assert_eq!(s.action_target(), None, "no visible row → no action target");
        assert!(
            !s.edit_begin(),
            "editor must not open on a filtered-out control"
        );

        // A matching query restores an actionable target that is genuinely visible.
        s.search_clear();
        s.search_begin();
        for c in "font".chars() {
            s.search_push(c);
        }
        let t = s
            .action_target()
            .expect("matching filter keeps an actionable target");
        assert!(
            s.visible_indices().contains(&t),
            "the action target is a visible row"
        );
    }

    #[test]
    fn tray_card_panel_is_first_prim_and_sized() {
        let s = SettingsState::from_config(&cfg());
        let g = full_geom();
        let t = tray(&s, &g);
        assert!(
            matches!(t.prims.first(), Some(DrawPrim::Panel { .. })),
            "the frosted card panel is prims[0]"
        );
        assert_eq!(t.card, (0.0, 0.0, 132.0 * 8.0, 38.0 * 16.0));
    }

    /// FACE PLUMB (native typography): the settings CHROME paints in the UI face —
    /// the pane title in synthesized-semibold, sidebar/category labels, group
    /// captions and row labels in regular UI — while codes and glyph art stay MONO:
    /// the colour row's hex readout and the preview mock's terminal body. Guards the
    /// whole type ramp against a regression back to the terminal face (the "TUI
    /// mockup" look the redesign removes).
    #[test]
    fn settings_chrome_is_ui_face_and_codes_stay_mono() {
        let mut s = SettingsState::from_config(&cfg());
        s.set_category(prefs::Section::Appearance);
        let g = full_geom();
        let prims = tray(&s, &g).prims;
        let faces_of = |needle: &str| -> Vec<(TextFace, f32)> {
            prims
                .iter()
                .filter_map(|p| {
                    if let DrawPrim::Text {
                        s: txt, face, px, ..
                    } = p
                        && txt == needle
                    {
                        Some((*face, *px))
                    } else {
                        None
                    }
                })
                .collect()
        };
        // "Appearance" paints twice: the sidebar label (regular UI) and the pane
        // TITLE — the biggest text on the card, semibold UI.
        let appearance = faces_of("Appearance");
        let (title_face, title_px) =
            appearance
                .iter()
                .copied()
                .fold(
                    (TextFace::Mono, 0.0_f32),
                    |a, b| if b.1 > a.1 { b } else { a },
                );
        assert_eq!(title_face, TextFace::UiBold, "pane title is semibold UI");
        // Reconciled chrome unifies on OUR named 5-step type scale (P2): the pane
        // title is `TypeStep::Title` (1.15×), not theirs' pre-merge free 1.3× ramp.
        assert!(
            (title_px - TypeStep::Title.px(g.font_px).get()).abs() < 0.01,
            "title rides the TypeStep::Title ramp"
        );
        assert!(
            appearance
                .iter()
                .any(|(f, px)| *f == TextFace::Ui && *px < title_px),
            "sidebar category label is regular UI"
        );
        // Group captions (uppercase, secondary) are UI at the small caption size —
        // OUR `TypeStep::Caption` (0.8×) after the P2 type-scale unification.
        assert_eq!(
            faces_of("THEME"),
            vec![(TextFace::Ui, TypeStep::Caption.px(g.font_px).get())]
        );
        // Row labels are UI.
        let label = s.fields[category_controls(&s.fields, s.category)[0]].label;
        assert!(
            faces_of(label).iter().all(|(f, _)| *f == TextFace::Ui),
            "row labels are UI"
        );
        // Codes stay MONO: a SET colour row's hex readout...
        if let Some(f) = s
            .fields
            .iter_mut()
            .find(|f| f.key == prefs::EDIT_FOREGROUND)
        {
            f.seed = Some("#A6E22E".to_string());
        }
        let prims2 = tray(&s, &g).prims;
        assert!(
            prims2.iter().any(|p| matches!(
                p,
                DrawPrim::Text { s: txt, face: TextFace::Mono, .. } if txt == "#A6E22E"
            )),
            "hex readout stays mono"
        );
        // ...but the UNSET colour row's "theme default" placeholder is prose → UI.
        assert!(
            prims.iter().any(|p| matches!(
                p,
                DrawPrim::Text { s: txt, face: TextFace::Ui, .. } if txt == "theme default"
            )),
            "unset colour placeholder is UI prose"
        );
        // The preview mock: terminal BODY text stays mono; the titlebar title is UI
        // (a real macOS titlebar is the system face).
        assert!(
            faces_of("cargo run")
                .iter()
                .all(|(f, _)| *f == TextFace::Mono),
            "preview terminal body stays mono"
        );
        assert_eq!(
            faces_of("aterm").first().map(|(f, _)| *f),
            Some(TextFace::Ui)
        );
    }

    /// Only the keyboard-selected, ON-SCREEN row gets the accent selection wash (a Panel
    /// with the distinctive `SEL_WASH_ALPHA`); scrolling it out of the band removes it.
    #[test]
    fn selection_wash_only_when_selected_visible() {
        let mut s = SettingsState::from_config(&cfg());
        // `from_config` already selects the Appearance category's FIRST
        // laid-out control (theme) — with the full-coverage registry, raw field
        // index 0 (a Text & Contrast row) can sit below the band, so the test
        // keeps the constructor's top-of-pane selection.
        s.scroll = 0;
        let g = full_geom();
        let visible = tray(&s, &g);
        assert_eq!(
            sel_wash_count(&visible.prims),
            1,
            "exactly one selection wash for the visible selected row"
        );
        // Scroll the selected control's GroupRow out of the band → no wash. A tiny
        // band forces the overflow (the full window fits all of Appearance).
        let short = SettingsGeom {
            panel_rows: 17,
            ..full_geom()
        }; // band of 13 cells, no preview
        let mut hidden = SettingsState::from_config(&cfg());
        hidden.selected = 0;
        hidden.scroll = category_layout(
            &hidden.fields,
            hidden.category,
            footnote_wrap_chars(short.cols),
        )
        .len()
            - 1;
        assert_eq!(
            sel_wash_count(&tray(&hidden, &short).prims),
            0,
            "no wash when the selected row is scrolled away"
        );
    }

    /// The redesigned painter emits REAL widgets, not text values: a bounded numeric row
    /// draws a slider (Capsule + thumb Dot), and framed inputs / focus rings / hairlines
    /// draw Strokes — i.e. the "just text boxes" complaint is gone.
    #[test]
    fn widgets_render_real_controls() {
        let mut s = SettingsState::from_config(&cfg());
        // Seed the bounded numeric row: an UNSET font_px displays "auto (default)"
        // (a framed field, not a slider), and the sidebar icons carry no Capsule
        // of their own any more — this must pin the REAL slider widget.
        let i = s
            .fields
            .iter()
            .position(|f| f.key == crate::prefs::EDIT_FONT_PX)
            .unwrap();
        s.fields[i].seed = Some("13".to_string());
        s.set_category(prefs::Section::Typography); // font_px = a bounded slider row
        let g = full_geom();
        let t = tray(&s, &g);
        assert!(
            t.prims
                .iter()
                .any(|p| matches!(p, DrawPrim::Capsule { .. })),
            "a bounded numeric row (font size) draws a slider Capsule"
        );
        assert!(
            t.prims.iter().any(|p| matches!(p, DrawPrim::Dot { .. })),
            "toggle knobs / slider thumbs draw Dots"
        );
        assert!(
            t.prims.iter().any(|p| matches!(p, DrawPrim::Stroke { .. })),
            "framed inputs / focus rings / hairlines draw Strokes"
        );
    }

    /// The preview card is PINNED (design §5): drawn for a previewable focus, a
    /// non-previewable focus, and while a search filter is active — it NEVER collapses
    /// at full geometry. Only the resize ladder (cols < 96) hides it (graft #3).
    #[test]
    fn preview_card_always_pinned() {
        let has_preview = |s: &SettingsState, g: &SettingsGeom| {
            tray(s, g)
                .prims
                .iter()
                .any(|p| matches!(p, DrawPrim::Text { s, .. } if s == "PREVIEW"))
        };
        let g = full_geom();
        let mut s = SettingsState::from_config(&cfg());
        s.selected = s
            .fields
            .iter()
            .position(|f| f.key == crate::prefs::EDIT_THEME)
            .unwrap();
        assert!(has_preview(&s, &g), "pinned under a theme focus");

        s.set_category(prefs::Section::Terminal);
        s.selected = s
            .fields
            .iter()
            .position(|f| f.key == crate::prefs::EDIT_SCROLLBACK)
            .unwrap();
        assert!(
            has_preview(&s, &g),
            "pinned under a (formerly peek-less) scrollback focus"
        );

        s.searching = true;
        s.query = "theme".to_string();
        assert!(
            has_preview(&s, &g),
            "the preview stays during search (default mock)"
        );

        // The one thing that hides it: a resize below the ladder's 96-col rung.
        let narrow = SettingsGeom {
            cols: 80,
            ..full_geom()
        };
        assert!(
            !has_preview(&s, &narrow),
            "below 96 cols the card yields its rows"
        );
    }

    /// Feedback-#2 regression (graft #4): moving the selection changes ZERO layout
    /// rectangles. Every structural Panel (sidebar wash, boxes, preview container +
    /// mock, widget chips, scrollbar) is byte-identical across a selection move — only
    /// the SEL_WASH panel (excluded), focus ring, and preview-interior text/outline may
    /// differ. `pane_geom` is selection-blind by signature (window size in, rects out).
    #[test]
    fn regions_are_invariant_under_selection_change() {
        let g = full_geom();
        let rects = |s: &SettingsState| -> Vec<(u32, u32, u32, u32)> {
            tray(s, &g)
                .prims
                .iter()
                .filter_map(|p| match p {
                    DrawPrim::Panel {
                        x, y, w, h, fill, ..
                    } if fill[3] != SEL_WASH_ALPHA => {
                        Some((*x as u32, *y as u32, *w as u32, *h as u32))
                    }
                    _ => None,
                })
                .collect()
        };
        let mut s = SettingsState::from_config(&cfg());
        s.pane = SettingsPane::Content;
        let controls = category_controls(&s.fields, s.category);
        assert!(controls.len() >= 3, "Appearance has rows to walk");
        s.selected = controls[0];
        let a = rects(&s);
        s.selected = controls[1];
        let b = rects(&s);
        s.selected = controls[2];
        let c = rects(&s);
        assert_eq!(a, b, "selection move 0→1 moved a structural rect");
        assert_eq!(b, c, "selection move 1→2 moved a structural rect");
    }

    /// The mini window's TITLEBAR resolves `window_theme` (graft #2): `auto` paints a
    /// vertical half-split via a rectangular ClipPush whose LEADING half follows
    /// `PreviewCtx::system_dark` (so the pixels are truthful), an explicit value paints
    /// solid chrome, and the focused row drives a titlebar outline.
    #[test]
    fn preview_titlebar_resolves_window_theme() {
        let g = full_geom();
        let mut s = SettingsState::from_config(&cfg());
        // The titlebar split's clip lives in the CONTENT pane; the sidebar icon
        // tiles keep small clips of their own left of the seam, so the split
        // assertions look only right of the sidebar.
        let sidebar_w = pane_geom(&g).sidebar_w_cells * g.cw;
        let content_clip = |t: &TrayInput| {
            t.prims
                .iter()
                .any(|p| matches!(p, DrawPrim::ClipPush { x, .. } if *x > sidebar_w))
        };
        // Unset window_theme resolves to auto → the split's ClipPush is present.
        let dark = settings_tray(
            &s,
            &g,
            Theme::default(),
            PreviewCtx {
                system_dark: true,
                ..PreviewCtx::default()
            },
        );
        let light = settings_tray(
            &s,
            &g,
            Theme::default(),
            PreviewCtx {
                system_dark: false,
                ..PreviewCtx::default()
            },
        );
        assert!(
            content_clip(&dark),
            "auto titlebar half-splits via ClipPush"
        );
        let fills = |t: &TrayInput| -> Vec<[u8; 4]> {
            t.prims
                .iter()
                .filter_map(|p| match p {
                    DrawPrim::Panel { fill, .. } => Some(*fill),
                    _ => None,
                })
                .collect()
        };
        assert_ne!(
            fills(&dark),
            fills(&light),
            "system_dark flips which chrome leads the auto split"
        );
        // An explicit value paints solid chrome — no split (and no menu is open, so
        // no ClipPush at all).
        let idx = s
            .fields
            .iter()
            .position(|f| f.key == crate::prefs::EDIT_WINDOW_THEME)
            .unwrap();
        s.fields[idx].seed = Some("dark".to_string());
        assert!(
            !content_clip(&tray(&s, &g)),
            "an explicit window_theme needs no split"
        );
        // The row drives the titlebar element (the outline arm exists for it).
        assert!(matches!(
            driven_element(crate::prefs::EDIT_WINDOW_THEME),
            Some(Driven::Titlebar)
        ));
    }

    /// The preview's cursor block reflects the live `cursor_style`: a `bar` cursor is a
    /// thin vertical Panel, a `block` cursor fills the cell — so scrubbing the style is
    /// visible in the pinned card.
    #[test]
    fn preview_cursor_block_reflects_style() {
        let base = SettingsState::from_config(&cfg());
        let g = full_geom();
        let idx = base
            .fields
            .iter()
            .position(|f| f.key == crate::prefs::EDIT_CURSOR_STYLE)
            .unwrap();
        let cur = u32_rgb(Theme::default().cursor);
        let cursor_block_w = |style: &str| {
            let mut s = SettingsState::from_config(&cfg());
            s.set_category(prefs::Section::Cursor);
            s.selected = idx;
            s.fields[idx].seed = Some(style.to_string());
            tray(&s, &g)
                .prims
                .iter()
                .find_map(|p| match p {
                    // The cursor block is the only radius-0 Panel filled with the cursor colour.
                    DrawPrim::Panel {
                        w, fill, radius, ..
                    } if *radius == 0.0 && [fill[0], fill[1], fill[2]] == cur => Some(*w),
                    _ => None,
                })
                .expect("a cursor block is drawn in the preview card")
        };
        assert!(
            cursor_block_w("bar") < cursor_block_w("block"),
            "a bar cursor is narrower than a block cursor in the preview"
        );
    }

    /// A search query narrows the visible set to matching controls, navigation stays inside
    /// that set, and clearing restores the full list.
    #[test]
    fn search_filters_and_navigates_visible() {
        let mut s = SettingsState::from_config(&cfg());
        let all = s.fields.len();
        s.query = "theme".to_string();
        let vis = s.visible_indices();
        assert!(
            !vis.is_empty() && vis.len() < all,
            "filter narrows the list"
        );
        for &i in &vis {
            let f = &s.fields[i];
            let hit = f.label.to_lowercase().contains("theme")
                || f.key.contains("theme")
                || crate::prefs::keywords_of(f.key)
                    .iter()
                    .any(|k| k.contains("theme"));
            assert!(hit, "visible row {} actually matches the query", f.key);
        }
        s.selected = vis[0];
        s.move_selection(1, 10);
        assert!(vis.contains(&s.selected), "move stays on a visible row");
        s.search_clear();
        assert!(s.query.is_empty());
        assert_eq!(
            s.visible_indices().len(),
            all,
            "clearing restores every control"
        );
    }

    /// Typing a query that excludes the current selection snaps it onto a visible row, so
    /// activation/navigation never targets a hidden control.
    #[test]
    fn search_snaps_selection_into_visible_set() {
        let mut s = SettingsState::from_config(&cfg());
        s.selected = s
            .fields
            .iter()
            .position(|f| f.key == crate::prefs::EDIT_SCROLLBACK)
            .expect("scrollback row");
        for c in "theme".chars() {
            s.search_push(c);
        }
        assert!(
            s.visible_indices().contains(&s.selected),
            "selection snapped onto a visible row after filtering"
        );
    }

    /// Visual preview of the two-pane settings surface → a PNG (gated on
    /// `ATERM_SETTINGS_PREVIEW=path`; `ATERM_SETTINGS_PREVIEW_THEME=Name` picks a theme).
    /// Renders at 2×-Retina-ish device metrics at the dedicated window's 132×38 target,
    /// with the theme row selected so the sidebar pill, the pinned preview card, and the
    /// Appearance group-boxes are all visible. `ATERM_SETTINGS_PREVIEW_KEY=<config_key>`
    /// focuses a different control (its category follows); `ATERM_SETTINGS_PREVIEW_DARK=1`
    /// renders the auto titlebar's dark-leading split.
    #[test]
    fn preview_retired_settings_overlay() {
        let Ok(path) = std::env::var("ATERM_SETTINGS_PREVIEW") else {
            return;
        };
        let theme = std::env::var("ATERM_SETTINGS_PREVIEW_THEME")
            .ok()
            .as_deref()
            .and_then(aterm_types::scheme::builtin)
            .map_or_else(Theme::default, |s| {
                let p = s.to_theme_parts();
                Theme {
                    fg: p.fg,
                    bg: p.bg,
                    cursor: p.cursor,
                    selection: p.selection,
                }
            });
        let mut s = SettingsState::from_config(&cfg());
        let focus_key = std::env::var("ATERM_SETTINGS_PREVIEW_KEY")
            .unwrap_or_else(|_| crate::prefs::EDIT_THEME.to_string());
        s.selected = s
            .fields
            .iter()
            .position(|f| f.key == focus_key)
            .unwrap_or(0);
        // The category follows the focused key; content pane holds keyboard focus.
        if let Some(f) = s.fields.get(s.selected) {
            s.set_category(crate::prefs::section_of(f.key));
        }
        s.pane = SettingsPane::Content;
        // Optional search state for the preview (ATERM_SETTINGS_PREVIEW_QUERY=text).
        if let Ok(q) = std::env::var("ATERM_SETTINGS_PREVIEW_QUERY") {
            s.searching = true;
            for c in q.chars() {
                s.search_push(c);
            }
        }
        // Optional popovers: ATERM_SETTINGS_PREVIEW_WHEEL=1 opens the colour wheel
        // on the focused Color row; ATERM_SETTINGS_PREVIEW_MENU=<n> opens the
        // focused row's popup menu with option n highlighted (the live-demo lane).
        if std::env::var("ATERM_SETTINGS_PREVIEW_WHEEL").is_ok() {
            // Same per-key fallback as the live caller (`settings_wheel_open`).
            let key = s.fields.get(s.selected).map_or("", |f| f.key);
            s.wheel_open(theme_color_for_key(theme, key));
        }
        if let Ok(n) = std::env::var("ATERM_SETTINGS_PREVIEW_MENU") {
            s.menu_open();
            if let (Ok(n), Some(m)) = (n.parse::<usize>(), s.menu.as_mut()) {
                m.highlighted = n.min(m.options.len().saturating_sub(1));
            }
            s.demo_phase = 40; // mid-sweep, so the demo lane is visibly populated
        }
        let (cw, ch, px) = (16.0_f32, 34.0_f32, 26.0_f32);
        let cols = 132usize;
        let g = SettingsGeom {
            cw,
            ch,
            font_px: px,
            cols,
            panel_rows: 38,
        };
        let ctx = PreviewCtx {
            system_dark: std::env::var("ATERM_SETTINGS_PREVIEW_DARK").is_ok(),
            ..PreviewCtx::default()
        };
        let tray = settings_tray(&s, &g, theme, ctx);
        let bg = u32_rgb(theme.bg);
        let (rgba, pw, ph) = crate::tray_raster::rasterize_tray(
            &tray.prims,
            (cols as f32 * cw) as u32,
            (g.panel_rows as f32 * ch) as u32,
            1.0,
            [bg[0], bg[1], bg[2], 255],
        );
        let mut out = Vec::new();
        {
            let mut enc = aterm_png::Encoder::new(&mut out, pw, ph);
            enc.set_color(aterm_png::ColorType::Rgba);
            enc.set_depth(aterm_png::BitDepth::Eight);
            let mut wr = enc.write_header().unwrap();
            wr.write_image_data(&rgba).unwrap();
        }
        std::fs::write(&path, &out).unwrap();
    }

    #[test]
    fn tray_emits_a_label_for_every_visible_control() {
        let s = SettingsState::from_config(&cfg());
        let g = full_geom();
        let t = tray(&s, &g);
        let pg = pane_geom(&g);
        // Every control row the grouped band paints has its label Text.
        let rows = category_layout(&s.fields, s.category, footnote_wrap_chars(g.cols));
        let mut cells = 0usize;
        for row in rows.iter().skip(s.scroll) {
            let rh = group_row_cells(row);
            if cells + rh > pg.group_band() {
                break;
            }
            cells += rh;
            if let GroupRow::Control(idx) = row {
                let label = s.fields[*idx].label;
                assert!(
                    t.prims
                        .iter()
                        .any(|p| matches!(p, DrawPrim::Text { s, .. } if s == label)),
                    "missing label Text for {label}"
                );
            }
        }
        // Every sidebar category labels too — including the Cursor Kitty pane
        // added on 2026-08-10, which pushed the strip to eleven rows.
        for sec in prefs::Section::ORDER {
            assert!(
                t.prims
                    .iter()
                    .any(|p| matches!(p, DrawPrim::Text { s, .. } if s == sec.label())),
                "missing sidebar label for {}",
                sec.label()
            );
        }
    }

    #[test]
    fn body_layout_interleaves_section_headers() {
        let s = SettingsState::from_config(&cfg());
        let body = wanted_rows(&s.fields) - 2; // controls + one header per section
        let layout = body_layout(&s.fields, 0, body);
        let headers = layout
            .iter()
            .filter(|r| matches!(r, BodyRow::Header(_)))
            .count();
        let controls: Vec<usize> = layout
            .iter()
            .filter_map(|r| match r {
                BodyRow::Control(i) => Some(*i),
                BodyRow::Header(_) => None,
            })
            .collect();
        assert_eq!(
            headers,
            distinct_sections(&s.fields),
            "one header per section"
        );
        assert_eq!(
            controls,
            (0..s.fields.len()).collect::<Vec<_>>(),
            "every control appears once, in order"
        );
        assert!(
            matches!(layout.first(), Some(BodyRow::Header(_))),
            "the band opens with the first section's header"
        );
    }

    #[test]
    fn clamp_scroll_keeps_selected_visible_with_headers() {
        let mut s = SettingsState::from_config(&cfg());
        let n = s.fields.len();
        s.selected = n - 1; // the last control (bottom section)
        let body = 6; // a short band → must scroll, accounting for headers
        s.clamp_scroll(body);
        assert!(
            body_layout(&s.fields, s.scroll, body)
                .iter()
                .any(|r| matches!(r, BodyRow::Control(i) if *i == n - 1)),
            "the selected control stays visible after scrolling past headers"
        );
    }

    #[test]
    fn cycle_edit_toggles_a_bool() {
        let s = SettingsState::from_config(&cfg());
        let trail = s
            .fields
            .iter()
            .find(|f| f.key == crate::prefs::EDIT_CURSOR_TRAIL)
            .expect("cursor_trail row");
        // A Bool row toggles AWAY FROM ITS RESOLVED SEED, whatever that seed is. The
        // trail's absent-key default is platform-split
        // (`app_config::DEFAULT_DECORATIVE_EFFECTS`), so the mechanic — not a literal —
        // is what this pins, and it is pinned on every platform.
        let flipped = (!crate::app_config::DEFAULT_DECORATIVE_EFFECTS).to_string();
        assert_eq!(
            cycle_edit(trail),
            Some((crate::prefs::EDIT_CURSOR_TRAIL, Some(flipped)))
        );
    }

    /// Graft #1: the 7 per-effect checkbox rows are GONE — `cursor_trail_style` is
    /// ONE "Cursor trail" popup row (Enum over [`CURSOR_TRAIL_STYLES`]) whose open
    /// menu's HIGHLIGHTED option drives the animated demo lane, so browsing the
    /// menu live-demos each look. Nothing persists while browsing; Esc restores.
    #[test]
    fn trail_effect_is_one_popup_row_with_live_demo() {
        let mut s = SettingsState::from_config(&cfg());
        let rows: Vec<usize> = (0..s.fields.len())
            .filter(|&i| s.fields[i].key == crate::prefs::EDIT_CURSOR_TRAIL_STYLE)
            .collect();
        assert_eq!(rows.len(), 1, "exactly one trail-style row");
        let idx = rows[0];
        assert_eq!(s.fields[idx].label, "Cursor trail");
        assert!(
            matches!(s.fields[idx].kind, EditKind::Enum { options } if options == CURSOR_TRAIL_STYLES),
            "the popup offers the whole style list"
        );
        assert!(
            uses_popup(&s.fields[idx]),
            "10 options → popup chip, never segmented"
        );

        // Idle: no demo subject. Focusing the row demos the EFFECTIVE style.
        // The style row lives on the CAT's category since 2026-08-10, and
        // `action_target` only reports a row whose section is the live category.
        s.pane = SettingsPane::Content;
        assert_eq!(demo_style(&s), None, "a non-trail focus has no demo");
        s.set_category(prefs::Section::CursorKitty);
        s.selected = idx;
        assert_eq!(
            demo_style(&s),
            Some(crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE),
            "focus demos the resolved default"
        );
        // The master toggle demos the selected style too — from its own
        // category, which the trail engine kept.
        s.set_category(prefs::Section::Cursor);
        s.selected = s
            .fields
            .iter()
            .position(|f| f.key == crate::prefs::EDIT_CURSOR_TRAIL)
            .unwrap();
        assert_eq!(
            demo_style(&s),
            Some(crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE)
        );

        // Open the menu and move the highlight: the HIGHLIGHTED (uncommitted)
        // option drives the demo — and only Enter would persist it.
        s.set_category(prefs::Section::CursorKitty);
        s.selected = idx;
        assert!(s.menu_open());
        let fire = s
            .menu
            .as_ref()
            .unwrap()
            .options
            .iter()
            .position(|o| o == "fire")
            .unwrap();
        s.menu.as_mut().unwrap().highlighted = fire;
        assert_eq!(
            demo_style(&s),
            Some("fire"),
            "the menu highlight demos live"
        );
        assert_eq!(
            s.menu_pending(),
            Some((
                crate::prefs::EDIT_CURSOR_TRAIL_STYLE,
                Some("fire".to_string())
            )),
            "Enter would commit the browsed style through the shared seam"
        );
        // Esc restores: cancel leaves the effective style in charge again.
        s.menu_cancel();
        assert_eq!(
            demo_style(&s),
            Some(crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE)
        );

        // While filtering (menu closed) the card rests on the default mock: no demo.
        s.searching = true;
        s.query = "trail".to_string();
        assert_eq!(demo_style(&s), None);
    }

    /// The demo phase must perturb the fingerprint (each tick re-rasterizes the
    /// card) — without this the animated preview would freeze on its first frame.
    #[test]
    fn demo_phase_perturbs_fingerprint() {
        let mut s = SettingsState::from_config(&cfg());
        let fp0 = s.fingerprint();
        s.demo_phase = s.demo_phase.wrapping_add(1);
        assert_ne!(fp0, s.fingerprint());
    }

    #[test]
    fn enum_current_strips_default_placeholder_suffix() {
        // An unset style seeds None, so display_value is the
        // "rainbow kitty pet (default)" placeholder and enum_current strips the
        // suffix to the canonical option — this is what the Cursor Kitty page's
        // companion chip and the demo lane resolve.
        let s = SettingsState::from_config(&cfg());
        let f = s
            .fields
            .iter()
            .find(|f| f.key == crate::prefs::EDIT_CURSOR_TRAIL_STYLE)
            .unwrap();
        assert!(f.seed.is_none());
        assert_eq!(enum_current(f), crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE);
    }

    /// A TYPO'D STYLE DEMOS THE TRAIL THAT IS ACTUALLY DRAWN. This overlay is the
    /// only Settings surface on Linux and Windows, so its animated demo lane and
    /// its Enter/Space cycle anchor have to agree with
    /// `app_config::resolve_trail_style`, which substitutes
    /// [`prefs::DEFAULT_CURSOR_TRAIL_STYLE`] and RENDERS. `options[0]` is "phaser",
    /// a wholly different look, and animating it here would have made the panel
    /// contradict the glass.
    #[test]
    fn an_unknown_trail_style_demos_the_runtime_fallback() {
        let typo = "plasma";
        assert_eq!(
            crate::prefs::cursor_trail_style_canonical(typo),
            None,
            "the probe value must really be unrecognized"
        );
        assert_eq!(
            crate::app_config::effective_trail_style_token(typo),
            crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE,
            "the runtime draws the default for it"
        );
        let mut s = SettingsState::from_config(&Config {
            cursor_trail_style: Some(typo.to_string()),
            ..Config::default()
        });
        let idx = s
            .fields
            .iter()
            .position(|f| f.key == crate::prefs::EDIT_CURSOR_TRAIL_STYLE)
            .expect("trail-style row");
        assert_eq!(s.fields[idx].seed.as_deref(), Some(typo));
        assert_eq!(
            enum_current(&s.fields[idx]),
            crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE
        );
        // The animated demo lane reads the same seam.
        s.pane = SettingsPane::Content;
        s.set_category(prefs::Section::CursorKitty);
        s.selected = idx;
        assert_eq!(
            demo_style(&s),
            Some(crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE),
            "the demo lane must play the fallback, not options[0]"
        );
        // `cycle_edit` shares the same seam, so it steps from the fallback
        // rather than from "phaser". This row never takes that path in the
        // shipping UI — it is a popup row, and activating it opens the menu on
        // the preserved spelling — so this pins the shared helper, not this
        // row's behaviour.
        let i = CURSOR_TRAIL_STYLES
            .iter()
            .position(|o| *o == crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE)
            .unwrap();
        assert_eq!(
            cycle_edit(&s.fields[idx]).unwrap().1.as_deref(),
            Some(CURSOR_TRAIL_STYLES[(i + 1) % CURSOR_TRAIL_STYLES.len()])
        );
        // The popup chip still preserves the authored spelling verbatim — that
        // arm is what lets the user step away from a typo without it being
        // clobbered, and it is unchanged.
        assert_eq!(popup_current_label(&s.fields[idx]), typo);
    }

    #[test]
    fn free_form_rows_do_not_cycle() {
        let s = SettingsState::from_config(&cfg());
        // font_family is a Text row — Enter opens the in-panel editor (P1.1), it does
        // not cycle. (The theme row IS now a cycler — see theme_row_cycles_builtin_registry.)
        let ff = s
            .fields
            .iter()
            .find(|f| f.key == crate::prefs::EDIT_FONT_FAMILY)
            .unwrap();
        assert_eq!(cycle_edit(ff), None);
    }

    #[test]
    fn theme_row_cycles_builtin_registry() {
        let s = SettingsState::from_config(&cfg());
        let theme_row = s
            .fields
            .iter()
            .find(|f| f.key == crate::prefs::EDIT_THEME)
            .unwrap();
        assert!(
            matches!(theme_row.kind, EditKind::Theme),
            "theme row is a Theme picker"
        );
        // An unset config resolves the current name to the first registry entry.
        assert_eq!(theme_current(theme_row), "Default");
        let (key, val) = cycle_edit(theme_row).expect("theme cycles");
        assert_eq!(key, crate::prefs::EDIT_THEME);
        let next = val.unwrap();
        let names = aterm_types::scheme::builtin_names();
        assert_eq!(next, names[1], "Default → the next built-in scheme");
        assert!(
            aterm_types::scheme::load(&next).is_ok(),
            "the picker only ever offers loadable scheme names"
        );
    }

    /// ↑ at the top / ↓ at the bottom CLAMP (design §6: no wrap) — the old rem_euclid
    /// wrap jumped the selection across the whole list.
    #[test]
    fn move_selection_clamps_at_ends_and_tracks_scroll() {
        let mut s = SettingsState::from_config(&cfg());
        let n = s.fields.len();
        let body = 3;
        s.selected = 0;
        s.move_selection(-1, body);
        assert_eq!(s.selected, 0, "up from the top stays put (no wrap)");
        // Walk to the bottom; the scroll window tracks the selection all the way.
        for _ in 0..n + 3 {
            s.move_selection(1, body);
        }
        assert_eq!(
            s.selected,
            n - 1,
            "down from the bottom stays put (no wrap)"
        );
        assert!(
            body_layout(&s.fields, s.scroll, body)
                .iter()
                .any(|r| matches!(r, BodyRow::Control(i) if *i == n - 1)),
            "the clamped selection is still scrolled into the band"
        );
    }

    /// Enter on a popup-chip row (Theme / long Enum) OPENS the anchored menu with the
    /// current value highlighted — it must not cycle. Committing the highlighted current
    /// entry is a pure no-op, and non-popup rows never open a menu.
    #[test]
    fn menu_opens_on_popup_rows_with_current_highlighted() {
        let mut s = SettingsState::from_config(&cfg());
        s.selected = s
            .fields
            .iter()
            .position(|f| f.key == crate::prefs::EDIT_THEME)
            .unwrap();
        assert!(
            uses_popup(&s.fields[s.selected]),
            "theme row renders a popup chip"
        );
        let fp = s.fingerprint();
        assert!(s.menu_open(), "Enter opens the menu on a popup row");
        assert_ne!(
            s.fingerprint(),
            fp,
            "opening the menu changes the fingerprint"
        );
        let names = aterm_types::scheme::builtin_names();
        {
            let m = s.menu.as_ref().unwrap();
            assert_eq!(
                m.options.len(),
                names.len(),
                "a builtin value adds no custom entry"
            );
            assert_eq!(
                m.options[m.current], "Default",
                "unset theme is current at Default"
            );
            assert_eq!(
                m.highlighted, m.current,
                "the current value opens highlighted"
            );
        }
        assert_eq!(
            s.menu_pending(),
            None,
            "Enter on the current entry commits nothing"
        );
        s.menu_cancel();
        assert!(s.menu.is_none(), "Esc closes the menu");

        // A Bool row (a toggle, not a popup chip) never opens a menu.
        s.selected = s
            .fields
            .iter()
            .position(|f| f.key == crate::prefs::EDIT_CURSOR_TRAIL)
            .unwrap();
        assert!(!s.menu_open());
        assert!(s.menu.is_none());
    }

    /// ↑/↓ clamp the menu highlight (no wrap), Enter commits the highlighted option
    /// through `menu_pending`, and typing a letter jumps to the next matching option.
    #[test]
    fn menu_moves_commit_and_letter_jump() {
        let mut s = SettingsState::from_config(&cfg());
        s.selected = s
            .fields
            .iter()
            .position(|f| f.key == crate::prefs::EDIT_THEME)
            .unwrap();
        assert!(s.menu_open());
        let names = aterm_types::scheme::builtin_names();
        s.menu_move(1, 100);
        assert_eq!(
            s.menu_pending(),
            Some((crate::prefs::EDIT_THEME, Some(names[1].to_string()))),
            "↓ then Enter commits the next option"
        );
        s.menu_move(-100, 100);
        assert_eq!(
            s.menu.as_ref().unwrap().highlighted,
            0,
            "↑ clamps at the top"
        );
        s.menu_move(isize::MAX, 100);
        assert_eq!(
            s.menu.as_ref().unwrap().highlighted,
            names.len() - 1,
            "↓ clamps at the bottom"
        );
        // First-letter jump: from the top, 'n' lands on the first N… name (Nord).
        s.menu_move(-100, 100);
        let target = names.iter().position(|n| n.starts_with('N')).unwrap();
        s.menu_jump('n', 100);
        assert_eq!(
            s.menu.as_ref().unwrap().highlighted,
            target,
            "letter jump finds Nord"
        );
    }

    /// CUSTOM THEME PRESERVATION: a configured value not in the builtin registry (user
    /// theme / `dark:…,light:…` split) is listed VERBATIM as the menu's first entry and
    /// highlighted — so open + Enter is a no-op — and ←/→ steps FROM it instead of
    /// clobbering it. The popup chip labels the raw value too. Same for an unrecognized
    /// enum spelling.
    #[test]
    fn menu_preserves_custom_values() {
        const SPLIT: &str = "dark:Nord,light:GitHub Light";
        let mut s = SettingsState::from_config(&cfg());
        let idx = s
            .fields
            .iter()
            .position(|f| f.key == crate::prefs::EDIT_THEME)
            .unwrap();
        s.fields[idx].seed = Some(SPLIT.to_string());
        s.selected = idx;
        assert!(s.menu_open());
        {
            let m = s.menu.as_ref().unwrap();
            assert_eq!(m.options[0], SPLIT, "custom value listed verbatim, first");
            assert_eq!(m.current, 0);
            assert_eq!(m.highlighted, 0, "custom value opens highlighted");
            let names = aterm_types::scheme::builtin_names();
            assert_eq!(
                m.options.len(),
                names.len() + 1,
                "custom entry prepends the registry"
            );
        }
        assert_eq!(
            s.menu_pending(),
            None,
            "Enter never replaces the custom value"
        );
        let f = &s.fields[idx];
        assert_eq!(
            popup_current_label(f),
            SPLIT,
            "the chip labels the raw value"
        );
        // → steps FROM the custom entry to the first builtin (an explicit user action).
        let (_k, v) = step_edit(f, 1, false).expect("step from custom");
        assert_eq!(v.as_deref(), Some("Default"));

        // An unrecognized ENUM spelling gets the same treatment.
        let style = |seed: &str| EditField {
            label: "Trail style",
            key: crate::prefs::EDIT_CURSOR_TRAIL_STYLE,
            kind: EditKind::Enum {
                options: CURSOR_TRAIL_STYLES,
            },
            seed: Some(seed.to_string()),
            placeholder: String::new(),
        };
        let zzz = style("zzz");
        let opts = popup_options(&zzz);
        assert_eq!(opts[0], "zzz", "unrecognized enum value preserved verbatim");
        assert_eq!(popup_current_index(&zzz, &opts), 0);
        // A recognized value adds no custom entry.
        let lumen = style("lumen");
        assert_eq!(popup_options(&lumen).len(), CURSOR_TRAIL_STYLES.len());
    }

    /// GAP-4: the `cursor_trail_style` picker lists a `pack:<id>` option per LOADED
    /// Trail Pack (built-ins first, then sorted packs), and ←/→ cycles into them,
    /// while a pack-free config keeps the byte-identical static list.
    #[test]
    fn picker_lists_and_cycles_loaded_trail_packs() {
        let style = |seed: &str| EditField {
            label: "Trail style",
            key: crate::prefs::EDIT_CURSOR_TRAIL_STYLE,
            kind: EditKind::Enum {
                options: CURSOR_TRAIL_STYLES,
            },
            seed: Some(seed.to_string()),
            placeholder: String::new(),
        };
        let ids = vec!["emberfall".to_string(), "synthwave".to_string()];

        // No packs → byte-identical to the static list (and to plain popup_options).
        let lumen = style("lumen");
        assert_eq!(popup_options_with(&lumen, &[]), popup_options(&lumen));
        assert_eq!(
            popup_options_with(&lumen, &[]).len(),
            CURSOR_TRAIL_STYLES.len()
        );
        // A pack-free config's SettingsState carries no pack ids.
        assert!(SettingsState::from_config(&cfg()).trail_pack_ids.is_empty());

        // Loaded packs appear as `pack:<id>` after the built-ins, sorted.
        let with = popup_options_with(&lumen, &ids);
        assert_eq!(with.len(), CURSOR_TRAIL_STYLES.len() + 2);
        let first_pack = with.iter().position(|o| o.starts_with("pack:")).unwrap();
        assert_eq!(
            first_pack,
            CURSOR_TRAIL_STYLES.len(),
            "packs follow all built-ins"
        );
        assert_eq!(with[first_pack], "pack:emberfall", "packs sorted");
        assert_eq!(with[first_pack + 1], "pack:synthwave");

        // A configured pack value leads verbatim + highlighted, listed ONCE.
        let cur = style("pack:synthwave");
        let opts = popup_options_with(&cur, &ids);
        assert_eq!(opts[0], "pack:synthwave", "current pack leads verbatim");
        assert_eq!(popup_current_index(&cur, &opts), 0);
        assert_eq!(
            opts.iter()
                .filter(|o| o.as_str() == "pack:synthwave")
                .count(),
            1,
            "current pack is not duplicated"
        );

        // ←/→ cycles into a loaded pack: +1 from the last built-in ("off") lands
        // on the first pack; a non-pack row ignores the ids (byte-identical step).
        let stepped = step_edit_with(&style("off"), 1, false, &ids).expect("enum steps");
        assert_eq!(stepped.1.as_deref(), Some("pack:emberfall"));
        assert_eq!(
            step_edit_with(&style("phaser"), -1, false, &ids)
                .unwrap()
                .1
                .as_deref(),
            Some("pack:synthwave"),
            "prev from the first built-in wraps to the last pack"
        );
    }

    /// ←/→ adjust IN PLACE (design §6): Bool toggles, Enum/Theme steps (wrapping), a
    /// bounded numeric moves one step (Shift ⇒ ×10) clamped to its range, and free-form
    /// rows no-op.
    #[test]
    fn step_edit_adjusts_in_place() {
        let s = SettingsState::from_config(&cfg());
        let by_key = |k: &str| s.fields.iter().find(|f| f.key == k).unwrap();

        // Bool: either direction toggles AWAY FROM THE RESOLVED SEED — which for the
        // trail is platform-split (`app_config::DEFAULT_DECORATIVE_EFFECTS`), so the
        // expectation is derived rather than typed, and holds on every platform.
        assert_eq!(
            step_edit(by_key(crate::prefs::EDIT_CURSOR_TRAIL), -1, false),
            Some((
                crate::prefs::EDIT_CURSOR_TRAIL,
                Some((!crate::app_config::DEFAULT_DECORATIVE_EFFECTS).to_string())
            ))
        );

        // Bounded numeric: one step, Shift = ×10, clamped; the rail returns None.
        let font = |seed: &str| EditField {
            label: "Font size",
            key: crate::prefs::EDIT_FONT_PX,
            kind: EditKind::Float,
            seed: Some(seed.to_string()),
            placeholder: String::new(),
        };
        let r = crate::prefs::range_of(crate::prefs::EDIT_FONT_PX).unwrap();
        assert_eq!(r.min, 6.0, "test tracks the real range");
        assert_eq!(
            step_edit(&font("13"), 1, false).unwrap().1.as_deref(),
            Some("14")
        );
        assert_eq!(
            step_edit(&font("13"), 1, true).unwrap().1.as_deref(),
            Some("23")
        );
        assert_eq!(r.max, 200.0, "test tracks the full runtime range");
        assert_eq!(
            step_edit(&font("195"), 1, true).unwrap().1.as_deref(),
            Some("200"),
            "a big step clamps to max"
        );
        assert_eq!(
            step_edit(&font("200"), 1, false),
            None,
            "at the max rail: no-op"
        );
        assert_eq!(
            step_edit(&font("6"), -1, false),
            None,
            "at the min rail: no-op"
        );

        // Enum steps backward from the unset default (rainbow kitty PET,
        // options[2] since it became the shipped default) to options[1].
        let (_k, v) = step_edit(by_key(crate::prefs::EDIT_CURSOR_TRAIL_STYLE), -1, false).unwrap();
        assert_eq!(v.as_deref(), Some("rainbow kitty"));
        // …and from options[0] it wraps to the last option.
        let style = |seed: &str| EditField {
            label: "Trail effect",
            key: crate::prefs::EDIT_CURSOR_TRAIL_STYLE,
            kind: EditKind::Enum {
                options: CURSOR_TRAIL_STYLES,
            },
            seed: Some(seed.to_string()),
            placeholder: String::new(),
        };
        let (_k, v) = step_edit(&style("phaser"), -1, false).unwrap();
        assert_eq!(v.as_deref(), CURSOR_TRAIL_STYLES.last().copied());

        // Free-form rows have nothing to step.
        assert_eq!(
            step_edit(by_key(crate::prefs::EDIT_FONT_FAMILY), 1, false),
            None
        );
        assert_eq!(
            step_edit(by_key(crate::prefs::EDIT_SCROLLBACK), 1, false),
            None
        );
    }

    /// The menu popover stays inside the card (between the title underline and the
    /// footer), its hit-test maps a point to exactly the option drawn there, and the
    /// painter emits the clipped popover prims (highlight wash + option labels).
    #[test]
    fn menu_geom_hit_and_painter_agree() {
        let mut s = SettingsState::from_config(&cfg());
        s.selected = s
            .fields
            .iter()
            .position(|f| f.key == crate::prefs::EDIT_THEME)
            .unwrap();
        assert!(s.menu_open());
        let g = geom(wanted_rows(&s.fields));
        let mg = menu_geom(&s, &g).expect("open menu has a placement");
        let footer_y = (g.panel_rows - 1) as f32 * g.ch;
        assert!(mg.y >= g.ch, "popover starts below the title underline");
        assert!(
            mg.y + mg.visible as f32 * mg.row_h <= footer_y,
            "popover ends above the footer"
        );
        assert!(
            mg.x >= 0.0 && mg.x + mg.w <= g.cols as f32 * g.cw,
            "popover inside the card"
        );

        // The hit-test maps the centre of drawn row k to option first+k; outside → None.
        let k = mg.visible / 2;
        let (hx, hy) = (mg.x + mg.w * 0.5, mg.y + (k as f32 + 0.5) * mg.row_h);
        assert_eq!(menu_hit(&s, &g, hx, hy), Some(mg.first + k));
        assert_eq!(
            menu_hit(&s, &g, mg.x - 2.0, hy),
            None,
            "left of the popover misses"
        );

        // Painter: the popover is clipped and draws the highlight wash + option text.
        // (The sidebar icon tiles carry small clips of their own, so the clip
        // assertions COUNT rather than assume the menu owns the only ClipPush.)
        let clips = |t: &TrayInput| {
            t.prims
                .iter()
                .filter(|p| matches!(p, DrawPrim::ClipPush { .. }))
                .count()
        };
        let t = tray(&s, &g);
        let open_clips = clips(&t);
        assert!(open_clips > 0);
        assert!(t.prims.iter().any(|p| matches!(p, DrawPrim::ClipPop)));
        assert_eq!(
            t.prims
                .iter()
                .filter(|p| matches!(p, DrawPrim::Panel { fill, .. } if fill[3] == MENU_WASH_ALPHA))
                .count(),
            1,
            "exactly one menu-highlight wash"
        );
        let names = aterm_types::scheme::builtin_names();
        assert!(
            t.prims
                .iter()
                .any(|p| matches!(p, DrawPrim::Text { s, .. } if s == names[1])),
            "a non-current option name is drawn in the popover"
        );
        // Closing the menu removes the popover prims: exactly ITS clip is gone
        // and no highlight wash survives.
        s.menu_cancel();
        let closed = tray(&s, &g);
        assert_eq!(clips(&closed), open_clips - 1, "the menu's clip is gone");
        assert!(
            !closed
                .prims
                .iter()
                .any(|p| matches!(p, DrawPrim::Panel { fill, .. } if fill[3] == MENU_WASH_ALPHA)),
            "no menu-highlight wash after close"
        );
    }

    /// The legacy overlay serializer exposes the open menu (anchor key, highlight,
    /// options) for exact model-level assertions; closed ⇒ no `menu` line.
    #[test]
    fn controls_lines_expose_menu_state() {
        let mut s = SettingsState::from_config(&cfg());
        assert!(!s.controls_lines().iter().any(|l| l.starts_with("menu ")));
        s.selected = s
            .fields
            .iter()
            .position(|f| f.key == crate::prefs::EDIT_THEME)
            .unwrap();
        assert!(s.menu_open());
        s.menu_move(2, 100);
        let lines = s.controls_lines();
        let menu = lines
            .iter()
            .find(|l| l.starts_with("menu "))
            .expect("open menu serialized");
        assert!(menu.contains("key=theme"), "{menu}");
        assert!(menu.contains("highlighted=2"), "{menu}");
        assert!(menu.contains("current=0"), "{menu}");
        // Options are DEBUG-QUOTED so a custom value containing commas (a split
        // "dark:…,light:…" theme) stays parseable in the flat option list.
        let names: Vec<String> = aterm_types::scheme::builtin_names()
            .iter()
            .map(|n| format!("{n:?}"))
            .collect();
        assert!(
            menu.contains(&format!("options=[{}]", names.join(","))),
            "{menu}"
        );
    }

    /// Wheel scroll of the body band moves the window without touching the selection and
    /// clamps at both ends (no blank over-scroll); the menu's scroll window clamps too.
    #[test]
    fn wheel_scroll_clamps_body_and_menu() {
        let mut s = SettingsState::from_config(&cfg());
        let body = 6;
        let sel = s.selected;
        s.scroll_body(-3, body);
        assert_eq!(s.scroll, 0, "scroll up from the top clamps");
        for _ in 0..100 {
            s.scroll_body(2, body);
        }
        let mask = s.visible_mask();
        assert_eq!(s.scroll, max_scroll(&s.fields, mask.as_deref(), body));
        // MAXIMALITY, which is the property that actually matters: you cannot
        // scroll further without running off the end, and you could not have
        // scrolled less without overflowing the band.
        //
        // This used to assert the band is EXACTLY full at max scroll. That is not
        // an invariant — it is a fact about one particular field roster, and it
        // broke the first time a setting was added. `body_layout` emits a section
        // HEADER whenever the section changes, so a header falling on the boundary
        // costs a row: the smallest scroll whose content fits can leave the last
        // band one row short, and no scroll value yields exactly `body`. Under-
        // filling by a row is cosmetic; over-scrolling into blank space is the bug,
        // and that is what is checked here.
        let rows = body_layout(&s.fields, s.scroll, body).len();
        assert!(
            rows == body || rows + 1 == body,
            "max scroll fills the band, or falls one row short on a section \
             boundary — got {rows} of {body}"
        );
        if s.scroll > 0 {
            assert!(
                body_layout(&s.fields, s.scroll - 1, body + 1).len() > body,
                "max scroll is MAXIMAL: one row less would overflow the band"
            );
        }
        assert_eq!(s.selected, sel, "wheel scrolling never moves the selection");

        // Menu scroll window: clamped to the overflow.
        s.selected = s
            .fields
            .iter()
            .position(|f| f.key == crate::prefs::EDIT_THEME)
            .unwrap();
        s.clamp_scroll(body);
        assert!(s.menu_open());
        let n = s.menu.as_ref().unwrap().options.len();
        let visible = 4;
        for _ in 0..100 {
            s.menu_scroll_by(1, visible);
        }
        assert_eq!(
            s.menu.as_ref().unwrap().scroll,
            n - visible,
            "menu scroll clamps"
        );
        s.menu_scroll_by(-100, visible);
        assert_eq!(s.menu.as_ref().unwrap().scroll, 0);
    }

    /// The click hit boundary comes from the painter's own widget edge: for every
    /// control it sits inside the row — so an x-aware click on the selected row can
    /// always reach the widget region.
    #[test]
    fn widget_hit_left_is_inside_every_row() {
        let s = SettingsState::from_config(&cfg());
        let g = geom(wanted_rows(&s.fields));
        let w = g.cols as f32 * g.cw;
        for idx in 0..s.fields.len() {
            let left = widget_hit_left(&s, &g, idx).expect("hit edge for every row");
            assert!(
                left >= 0.0 && left < w,
                "{}: {left} inside the card",
                s.fields[idx].key
            );
        }
    }

    #[test]
    fn fingerprint_is_nonzero_and_tracks_state() {
        let mut s = SettingsState::from_config(&cfg());
        let base = s.fingerprint();
        assert_ne!(
            base, 0,
            "an OPEN panel never hashes to the closed sentinel 0"
        );
        s.selected += 1;
        let moved = s.fingerprint();
        assert_ne!(moved, base, "moving the selection changes the fingerprint");
        s.status = Some("saved".to_string());
        let after_status = s.fingerprint();
        assert_ne!(
            after_status, moved,
            "a status change changes the fingerprint"
        );
        if let Some(f) = s.fields.get_mut(0) {
            f.seed = Some("zzz".to_string());
        }
        assert_ne!(
            s.fingerprint(),
            after_status,
            "a control value change changes the fingerprint"
        );
    }

    #[test]
    fn in_panel_editor_edits_free_form_rows_only() {
        let mut s = SettingsState::from_config(&cfg());
        // Select a free-form Text row (font_family, unset by default). Its category
        // must be active — actions only target rows the content pane shows.
        s.selected = s
            .fields
            .iter()
            .position(|f| f.key == crate::prefs::EDIT_FONT_FAMILY)
            .expect("font_family row");
        s.set_category(prefs::Section::Typography);
        let fp_before = s.fingerprint();
        assert!(s.edit_begin(), "Enter on a Text row opens the editor");
        assert_eq!(
            s.editing.as_deref(),
            Some(""),
            "an unset key seeds an empty buffer"
        );
        assert_ne!(
            s.fingerprint(),
            fp_before,
            "entering edit mode changes the fingerprint"
        );
        s.edit_push('M');
        s.edit_push('o');
        s.edit_push('o');
        assert_eq!(s.editing.as_deref(), Some("Moo"));
        s.edit_backspace();
        assert_eq!(s.editing.as_deref(), Some("Mo"));
        assert_eq!(
            s.edit_pending(),
            Some((crate::prefs::EDIT_FONT_FAMILY, Some("Mo".to_string())))
        );
        s.edit_cancel();
        assert!(s.editing.is_none(), "Esc abandons the edit");

        // A Bool row cycles, it does NOT open the editor.
        s.selected = s
            .fields
            .iter()
            .position(|f| f.key == crate::prefs::EDIT_CURSOR_TRAIL)
            .expect("cursor_trail row");
        s.set_category(prefs::Section::Cursor);
        assert!(!s.edit_begin(), "Bool rows are not free-form");
        assert!(s.editing.is_none());
    }

    /// The resize-only fallback ladder (graft #3): full layout at ≥ 96 cols; the
    /// preview hides below 96; the sidebar collapses to an 8-cell icon strip below 64
    /// (and hides entirely at degenerate widths). Regions are a pure function of the
    /// window size — no state parameter even exists to leak selection into layout.
    #[test]
    fn pane_geom_follows_the_narrow_ladder() {
        let full = pane_geom_cells(132, 38);
        assert_eq!(full.sidebar_w_cells, 26.0);
        assert!(!full.icon_strip);
        assert_eq!(full.preview, (3, 12), "the pinned card owns rows 3..12");
        assert_eq!(full.groups, (12, 37));
        assert_eq!(full.footer_row, 37);

        let no_preview = pane_geom_cells(80, 38);
        assert!(
            !no_preview.preview_shown(),
            "the preview hides below 96 cols"
        );
        assert_eq!(no_preview.groups.0, 3, "the group band reclaims the rows");
        assert_eq!(
            no_preview.sidebar_w_cells, 26.0,
            "the sidebar is still full at 80"
        );

        let strip = pane_geom_cells(50, 38);
        assert!(strip.icon_strip);
        assert_eq!(strip.sidebar_w_cells, 8.0, "icon strip below 64 cols");

        // Too short for the preview even at full width (retired-overlay test clamp).
        assert!(!pane_geom_cells(132, 12).preview_shown());

        // Degenerate headless geometry stays well-formed (start ≤ end everywhere).
        let tiny = pane_geom_cells(8, 3);
        assert_eq!(tiny.sidebar_w_cells, 0.0);
        assert!(!tiny.preview_shown());
        assert!(tiny.groups.0 <= tiny.groups.1);
        assert!(tiny.preview.0 <= tiny.preview.1);
    }

    /// `category_layout` mirrors the design §3.2 grouping table: captions in order,
    /// every category field exactly once, footnotes where the spec gives them, and the
    /// Theme group leading Appearance (theme + window_theme before the colours).
    #[test]
    fn category_layout_matches_grouping_table() {
        let s = SettingsState::from_config(&cfg());
        let caps = |sec: prefs::Section| -> Vec<&'static str> {
            category_layout(&s.fields, sec, usize::MAX)
                .iter()
                .filter_map(|r| match r {
                    GroupRow::Caption(c) => Some(*c),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(
            caps(prefs::Section::Appearance),
            // Full-coverage growth: the Transparency box, the Wallpaper box
            // (backdrop image + dim), plus one box per decorative nested table
            // (sparkle words / matrix rain) and the Robi helper-robot toggle.
            [
                "Theme",
                "Colors",
                "Text & Contrast",
                "Transparency",
                "Wallpaper",
                "Sparkle words",
                "Matrix rain",
                "Robi the robot"
            ]
        );
        assert_eq!(
            caps(prefs::Section::Cursor),
            // The union layout: the full-coverage extended trail rows ride
            // "Trail effect", the serious-mode master policy gets its own box,
            // colour identity + the sprite get "Trail color", the GPU light
            // knobs "Light & GPU", and M2 stream fade closes.
            //
            // "Sound" opens right after "Trail effect" (owner ask: "add the
            // volume and SFX menu to settings" — ONE coherent box holding the
            // master volume slider and every SFX toggle, instead of sound rows
            // scattered through the trail box and, for the bonk, a different
            // pane entirely).
            [
                "Cursor",
                "Effect policy",
                "Trail effect",
                "Sound",
                "Motion",
                "Trail color",
                "Light & GPU",
                "Stream fade"
            ]
        );
        // The cat's own category (2026-08-10): the companion picker, the rainbow
        // wake dial, and the sprite art moved off "Trail effect"/"Trail color"
        // into three boxes that belong to the KITTY rather than the trail engine.
        assert_eq!(
            caps(prefs::Section::CursorKitty),
            ["Companion", "Rainbow wake", "Kitty art"]
        );
        assert_eq!(
            caps(prefs::Section::Typography),
            ["Font", "Shaping", "Line layout", "Rendering"]
        );
        assert_eq!(
            caps(prefs::Section::Window),
            [
                "Size",
                "Smart Titles",
                "Tab Status",
                "Window padding",
                "Chrome",
                "Session"
            ]
        );
        assert_eq!(
            caps(prefs::Section::Input),
            ["Clipboard", "Paste safety", "Keyboard"]
        );
        assert_eq!(caps(prefs::Section::Performance), ["System"]);
        assert_eq!(
            caps(prefs::Section::Terminal),
            ["Scrollback", "Text direction & width", "Shell", "Updates"]
        );
        assert_eq!(
            caps(prefs::Section::Security),
            ["Permissions", "Network drive"]
        );
        // The Kitty Log page is READ-ONLY (§F4.6): no editable key ever maps
        // to it, so its grouped layout is empty — the painter renders the
        // collection book instead of group-boxes.
        assert!(
            caps(prefs::Section::KittyLog).is_empty(),
            "no group-boxes on the Kitty Log page"
        );
        assert!(
            category_controls(&s.fields, prefs::Section::KittyLog).is_empty(),
            "no editable controls on the Kitty Log page"
        );

        let controls: Vec<&str> = category_controls(&s.fields, prefs::Section::Appearance)
            .iter()
            .map(|&i| s.fields[i].key)
            .collect();
        let head = [
            prefs::EDIT_THEME,
            prefs::EDIT_WINDOW_THEME,
            // The GPU-present tag rides the Theme box after window_theme.
            prefs::EDIT_WINDOW_COLORSPACE,
            prefs::EDIT_FOREGROUND,
            prefs::EDIT_BACKGROUND,
            prefs::EDIT_CURSOR_COLOR,
            prefs::EDIT_SELECTION_COLOR,
            // W5c: the explicit selected-text foreground — a real colour
            // control, so it rides the Colors group after selection_color.
            prefs::EDIT_SELECTION_FOREGROUND,
            // The indexed ANSI palette closes the Colors box (build order).
            prefs::EDIT_PALETTE,
            // The "how color behaves" group — Text & Contrast (order 2) sorts
            // after the Colors box, in field build order. The split's focus mark
            // rides beside the selection's because they answer the same question
            // in two places: what does this window do about the thing you are
            // not currently working in.
            prefs::EDIT_MINIMUM_CONTRAST,
            prefs::EDIT_SELECTION_INACTIVE,
            prefs::EDIT_SPLIT_FOCUS_MARK,
            prefs::EDIT_BOLD_IS_BRIGHT,
            prefs::EDIT_FAINT_OPACITY,
            // Transparency: opacity then material.
            prefs::EDIT_BACKGROUND_OPACITY,
            prefs::EDIT_BACKGROUND_MATERIAL,
            // Wallpaper: the backdrop image, its legibility dim, then the
            // backdrop-hue glyph tint.
            prefs::EDIT_WALLPAPER,
            prefs::EDIT_WALLPAPER_DIM,
            prefs::EDIT_WALLPAPER_TEXT_TINT,
        ];
        assert_eq!(&controls[..head.len()], head);
        // The decorative nested tables close the pane: every sparkle-words leaf
        // then every matrix-rain leaf, in NESTED_LEAVES (registry) order.
        //
        // The section filter is not decoration: the two BONK leaves are
        // `sparkle_words.` keys that now route to the Sound menu on the Cursor
        // pane, so a prefix-only expectation would demand them back here. Asking
        // `section_of` keeps this test honest against the router rather than
        // hard-coding which leaves left.
        let expected_tail: Vec<&str> = prefs::NESTED_LEAVES
            .iter()
            .filter(|l| {
                l.key.starts_with("sparkle_words.")
                    && prefs::section_of(l.key) == prefs::Section::Appearance
            })
            .chain(
                prefs::NESTED_LEAVES
                    .iter()
                    .filter(|l| l.key.starts_with("matrix_rain.")),
            )
            .map(|l| l.key)
            // …and the Robi helper-robot toggle closes the pane (its own
            // one-key group after the two decorative tables).
            .chain(std::iter::once(prefs::EDIT_ROBI))
            .collect();
        assert_eq!(controls[head.len()..].to_vec(), expected_tail);

        // Every field lands in exactly one category's layout (nothing vanishes).
        let total: usize = prefs::Section::ORDER
            .iter()
            .map(|&sec| category_controls(&s.fields, sec).len())
            .sum();
        assert_eq!(total, s.fields.len());

        // Footnotes ride their group (the Colors note from the spec table).
        assert!(
            category_layout(&s.fields, prefs::Section::Appearance, usize::MAX)
                .iter()
                .any(|r| matches!(r, GroupRow::Footnote(n) if n.contains("theme's color")))
        );
    }

    /// THE SOUND MENU (owner ask: "add the volume and SFX menu to settings").
    ///
    /// Non-vacuous by construction: it asserts its own precondition — that the
    /// registry actually still holds every audible key — before asserting they
    /// paint as ONE consecutive run under ONE "Sound" caption. If a future key
    /// is dropped from the registry the first block fails loudly instead of the
    /// grouping check passing over a shorter list.
    #[test]
    fn every_audible_key_paints_in_one_sound_box() {
        use std::collections::BTreeSet;
        let s = SettingsState::from_config(&cfg());

        // Precondition: each Sound-menu key is a real, reachable registry row.
        for key in prefs::SOUND_MENU_KEYS {
            assert!(
                s.fields.iter().any(|f| &f.key == key),
                "{key} left the registry; the Sound menu would silently shrink"
            );
        }
        assert!(
            prefs::SOUND_MENU_KEYS.len() >= 9,
            "the Sound menu lost members: {:?}",
            prefs::SOUND_MENU_KEYS
        );

        let rows = category_layout(&s.fields, prefs::Section::Cursor, usize::MAX);
        // Walk the painted rows and collect the keys under the Sound caption,
        // exactly as the pane draws them.
        let mut caption: Option<&str> = None;
        let mut in_sound: Vec<&str> = Vec::new();
        let mut sound_captions = 0usize;
        let mut elsewhere: Vec<&str> = Vec::new();
        for row in &rows {
            match row {
                GroupRow::Caption(c) => {
                    caption = Some(c);
                    if *c == "Sound" {
                        sound_captions += 1;
                    }
                }
                GroupRow::Control(i) => {
                    let key = s.fields[*i].key;
                    if caption == Some("Sound") {
                        in_sound.push(key);
                    } else if prefs::SOUND_MENU_KEYS.contains(&key) {
                        elsewhere.push(key);
                    }
                }
                GroupRow::Footnote(_) | GroupRow::Gap => {}
            }
        }
        assert_eq!(sound_captions, 1, "the Sound box must not be split in two");
        assert!(
            elsewhere.is_empty(),
            "audible keys still scattered outside the Sound box: {elsewhere:?}"
        );

        // THE CENSUS IS SPELLED OUT, NOT DERIVED. Deriving it from
        // `SOUND_MENU_KEYS` (filtered through the very `section_of` under test)
        // would let a misrouted key vanish from both sides of the comparison and
        // pass. These are the ten audible keys by name; the box holds exactly
        // them, in this exact painted order.
        assert_eq!(
            in_sound,
            [
                // The master switch opens the box and the slider it scales
                // follows it — the owner's "master volume slider plus the SFX
                // toggles", in that order.
                "trail_sounds",
                "trail_sound_volume",
                // …then one row per voice, coarse to fine.
                "tone_melody",
                "trail_sound_bed",
                "trail_sound_style",
                "trail_sound_riff",
                "bell_sound",
                // The `[sparkle_words]` leaves land last: nested leaves are
                // registered after the top-level rows and keep build order.
                "sparkle_words.profanity.bonk",
                "sparkle_words.profanity.bonk_detonation",
                // PRISM WAKE's pip — the newest voice and, by the loudness
                // ladder, the quietest; it paints last as the newest row.
                "output_streak.sound",
            ],
            "the Sound box holds exactly the ten audible keys, in painted order"
        );
        // A duplicate would survive the set-shaped checks above, so compare
        // lengths against the deduplicated view explicitly.
        assert_eq!(
            in_sound.len(),
            in_sound.iter().copied().collect::<BTreeSet<_>>().len(),
            "a key is painted into the Sound box twice: {in_sound:?}"
        );
        // …and the spelled-out census must agree with the routing list, or one
        // of the two is stale.
        assert_eq!(
            in_sound.iter().copied().collect::<BTreeSet<_>>(),
            prefs::SOUND_MENU_KEYS
                .iter()
                .copied()
                .collect::<BTreeSet<_>>(),
            "SOUND_MENU_KEYS and the painted Sound box disagree"
        );

        // The box carries consequence copy: where the master lives (it is a Top
        // Setting on the native surface, not a row here) and the one thing
        // Volume does not reach.
        let note = prefs::group_footnote("Sound").expect("the Sound box needs consequence copy");
        assert!(note.contains("Music effects"), "{note}");
        assert!(note.contains("Volume"), "{note}");
        assert!(note.contains("bell"), "{note}");
    }

    /// The two-pane keyboard model's pure state (design §6): sidebar ↑↓ clamp over the
    /// six categories; a category change resets scroll + snaps the selection onto the
    /// category's first control; focus round-trips between the panes.
    #[test]
    fn sidebar_navigation_clamps_and_snaps_selection() {
        let mut s = SettingsState::from_config(&cfg());
        assert_eq!(s.pane, SettingsPane::Sidebar, "the sidebar opens focused");
        assert_eq!(s.category, prefs::Section::Appearance);
        s.sidebar_move(-1);
        assert_eq!(s.category, prefs::Section::Appearance, "clamped at the top");
        s.sidebar_move(1);
        assert_eq!(s.category, prefs::Section::Cursor);
        assert_eq!(
            s.selected,
            category_controls(&s.fields, prefs::Section::Cursor)[0],
            "selection snaps to the category's first control"
        );
        assert_eq!(s.scroll, 0, "per-category scroll resets on change");
        s.sidebar_move(100);
        assert_eq!(
            s.category,
            prefs::Section::KittyLog,
            "clamped at the bottom (the 7th, read-only category)"
        );
        s.focus_content();
        assert_eq!(s.pane, SettingsPane::Content);
        s.focus_sidebar();
        assert_eq!(s.pane, SettingsPane::Sidebar);
    }

    /// Grouped scroll math: the wheel clamp stops when the tail fits, the hit-test maps
    /// cell offsets to exactly the rows the painter placed (2-cell controls, 1-cell
    /// chrome), and the keyboard clamp keeps the selected box fully visible.
    #[test]
    fn grouped_scroll_and_hit_agree() {
        let mut s = SettingsState::from_config(&cfg());
        s.set_category(prefs::Section::Cursor); // the trail rows make this one tall
        let rows = category_layout(&s.fields, s.category, usize::MAX);
        let band = 8usize;
        let max = max_group_scroll(&rows, band);
        assert!(max > 0, "the Cursor category overflows an 8-cell band");
        for _ in 0..100 {
            s.scroll_grouped(1, band, usize::MAX);
        }
        assert_eq!(s.scroll, max, "wheel scroll clamps at max");
        s.scroll_grouped(-100, band, usize::MAX);
        assert_eq!(s.scroll, 0);

        // Hit-test: the first visible row sits at cell 0; a control spans two cells.
        assert_eq!(group_row_at(&rows, 0, band, 0), Some(rows[0]));
        assert!(
            matches!(rows[0], GroupRow::Caption(_)),
            "a caption opens the layout"
        );
        assert_eq!(group_row_at(&rows, 0, band, 1), Some(rows[1]));
        assert_eq!(
            group_row_at(&rows, 0, band, 2),
            Some(rows[1]),
            "controls are 2 cells"
        );
        assert_eq!(
            group_row_at(&rows, 0, band, band),
            None,
            "outside the band misses"
        );

        // Keyboard: walking past the end clamps AND keeps the selection's box visible.
        let controls = category_controls(&s.fields, s.category);
        for _ in 0..controls.len() + 2 {
            s.move_selection_grouped(1, band, usize::MAX);
        }
        assert_eq!(
            s.selected,
            *controls.last().unwrap(),
            "clamped at the last control"
        );
        let sel_row = rows
            .iter()
            .position(|r| matches!(r, GroupRow::Control(i) if *i == s.selected))
            .unwrap();
        assert!(
            group_row_fully_visible(&rows, s.scroll, band, sel_row),
            "the clamp scrolled the selected box fully into the band"
        );
    }

    /// The sidebar's pure row map: rows 1-2 hit the search field, the eleven 2-cell
    /// category bands start at row 4, and nothing at/past the footer row hits.
    /// Every band below Cursor shifted down two rows when the Cursor Kitty pane was
    /// inserted at index 2 (2026-08-10) — the map is `4 + 2 * order_index`.
    #[test]
    fn sidebar_hit_maps_rows() {
        assert_eq!(sidebar_hit(1, 38), Some(SidebarHit::Search));
        assert_eq!(sidebar_hit(2, 38), Some(SidebarHit::Search));
        assert_eq!(sidebar_hit(3, 38), None, "the gap row above the categories");
        assert_eq!(
            sidebar_hit(4, 38),
            Some(SidebarHit::Category(prefs::Section::Appearance))
        );
        assert_eq!(
            sidebar_hit(5, 38),
            Some(SidebarHit::Category(prefs::Section::Appearance))
        );
        assert_eq!(
            sidebar_hit(6, 38),
            Some(SidebarHit::Category(prefs::Section::Cursor))
        );
        assert_eq!(
            sidebar_hit(8, 38),
            Some(SidebarHit::Category(prefs::Section::CursorKitty)),
            "the cat's own band owns rows 8-9"
        );
        assert_eq!(
            sidebar_hit(9, 38),
            Some(SidebarHit::Category(prefs::Section::CursorKitty))
        );
        assert_eq!(
            sidebar_hit(21, 38),
            Some(SidebarHit::Category(prefs::Section::Security))
        );
        assert_eq!(
            sidebar_hit(22, 38),
            Some(SidebarHit::Category(prefs::Section::Packages)),
            "the 10th category owns rows 22-23"
        );
        assert_eq!(
            sidebar_hit(24, 38),
            Some(SidebarHit::Category(prefs::Section::KittyLog)),
            "the 11th category owns rows 24-25"
        );
        assert_eq!(
            sidebar_hit(25, 38),
            Some(SidebarHit::Category(prefs::Section::KittyLog))
        );
        assert_eq!(sidebar_hit(26, 38), None, "past the eleven categories");
        assert_eq!(sidebar_hit(37, 38), None, "the footer row never hits");
        assert_eq!(sidebar_hit(5, 6), None, "a too-short card clips the row");
        // The painter clips a category whose FULL 2-cell row does not fit above the
        // footer (`row0 + 2 > footer_row`), so a clipped row's TOP cell must not hit
        // either — it used to switch categories on a click over blank sidebar.
        assert_eq!(sidebar_hit(4, 6), None, "top cell of a clipped first row");
        assert_eq!(
            sidebar_hit(16, 18),
            None,
            "Terminal's top cell clips at 18 rows"
        );
        assert_eq!(
            sidebar_hit(16, 19),
            Some(SidebarHit::Category(prefs::Section::Terminal)),
            "…and hits again once both its cells fit above the footer"
        );
    }

    /// Graft #2: while the theme MENU is open, the preview mock re-tints to the
    /// HIGHLIGHTED (uncommitted) builtin scheme — scrubbing the list previews live
    /// without a config write.
    #[test]
    fn theme_menu_highlight_retints_preview_mock() {
        let g = full_geom();
        let mut s = SettingsState::from_config(&cfg());
        s.selected = s
            .fields
            .iter()
            .position(|f| f.key == crate::prefs::EDIT_THEME)
            .unwrap();
        assert!(s.menu_open());
        // Highlight a scheme whose bg differs from the live theme's.
        let names = aterm_types::scheme::builtin_names();
        let target = names
            .iter()
            .position(|n| {
                aterm_types::scheme::builtin(n)
                    .is_some_and(|sch| sch.to_theme_parts().bg != Theme::default().bg)
            })
            .expect("a builtin scheme with a distinct bg");
        s.menu.as_mut().unwrap().highlighted = target;
        let bg = u32_rgb(
            aterm_types::scheme::builtin(names[target])
                .unwrap()
                .to_theme_parts()
                .bg,
        );
        assert!(
            tray(&s, &g).prims.iter().any(
                |p| matches!(p, DrawPrim::Panel { fill, .. } if [fill[0], fill[1], fill[2]] == bg)
            ),
            "the mock re-tints to the highlighted scheme's bg"
        );
    }

    /// `pane` and `category` are painted state, so both must perturb the fingerprint —
    /// otherwise Tab / a sidebar move would leave a stale card on glass.
    #[test]
    fn fingerprint_tracks_pane_and_category() {
        let mut s = SettingsState::from_config(&cfg());
        let base = s.fingerprint();
        s.pane = SettingsPane::Content;
        let content = s.fingerprint();
        assert_ne!(base, content, "a pane toggle repaints");
        s.set_category(prefs::Section::Terminal);
        assert_ne!(s.fingerprint(), content, "a category change repaints");
    }

    /// The Kitty Log page (§F4.6): the fingerprint folds the snapshot revision
    /// ONLY while the page is active (a sighting on another page moves no
    /// pixels — 0%-idle); the tray paints the collection book (header stats,
    /// seen rows, `???` silhouettes); `controls_lines` serializes the SAME
    /// model as `kittylog …` rows; and every row is non-activatable.
    #[test]
    fn kitty_log_page_paints_and_serializes_the_book() {
        let g = full_geom();
        let mut s = SettingsState::from_config(&cfg());
        let off_page = s.fingerprint();
        s.kitty_log.revision = 7;
        assert_eq!(
            s.fingerprint(),
            off_page,
            "a sighting is inert while another page is up"
        );
        s.set_category(prefs::Section::KittyLog);
        let on_page = s.fingerprint();
        s.kitty_log.revision = 8;
        assert_ne!(
            s.fingerprint(),
            on_page,
            "a sighting repaints the open book"
        );

        // Snapshot: a head (en), a sleeping full-cat (ja/zh), and the bow it wore.
        let mut log = crate::kitty_log::KittyLog {
            sightings: 2,
            trait_shy: 1,
            shown_cat: 2,
            accessory_bow: 1,
            ..Default::default()
        };
        log.entries.push(crate::kitty_log::KittyEntry {
            kitty_type: "head_peek".into(),
            magic: "none".into(),
            lang: "en".into(),
            count: 1,
            first_seen: "2026-07-01T00:00:00Z".into(),
            last_seen: "2026-07-02T00:00:00Z".into(),
            langs: vec!["en".into()],
        });
        log.entries.push(crate::kitty_log::KittyEntry {
            kitty_type: "head_tilt".into(),
            magic: "sakura".into(),
            lang: "ja".into(),
            count: 1,
            first_seen: "2026-07-03T00:00:00Z".into(),
            last_seen: "2026-07-03T00:00:00Z".into(),
            langs: vec!["ja".into(), "zh".into()],
        });
        log.collectibles = vec![
            crate::kitty_log::KittyCollectible {
                key: "s1_03".into(),
                variant: "s1_03".into(),
                age: "adult".into(),
                count: 1,
                first_seen: "2026-07-01T00:00:00Z".into(),
                last_seen: "2026-07-02T00:00:00Z".into(),
                langs: vec!["en".into()],
                ..Default::default()
            },
            crate::kitty_log::KittyCollectible {
                key: "spec_sleeping".into(),
                variant: "spec_sleeping".into(),
                age: "adult".into(),
                count: 1,
                first_seen: "2026-07-03T00:00:00Z".into(),
                last_seen: "2026-07-03T00:00:00Z".into(),
                langs: vec!["ja".into(), "zh".into()],
                ..Default::default()
            },
            crate::kitty_log::KittyCollectible {
                key: "acc_bow".into(),
                variant: "s1_03".into(),
                accessory: "acc_bow".into(),
                age: "adult".into(),
                count: 1,
                first_seen: "2026-07-03T00:00:00Z".into(),
                last_seen: "2026-07-03T00:00:00Z".into(),
                langs: vec!["ja".into()],
                ..Default::default()
            },
        ];
        s.kitty_log.log = log;

        // The painted card: header stats + a seen label + a ??? silhouette.
        let t = tray(&s, &g);
        let texts: Vec<&str> = t
            .prims
            .iter()
            .filter_map(|p| match p {
                DrawPrim::Text { s, .. } => Some(s.as_str()),
                _ => None,
            })
            .collect();
        let header = format!(
            "Sightings 2 · Collection 3/{}",
            aterm_effects::cat_glyphs_gen::GLYPH_IDS.len()
        );
        assert!(
            texts.iter().any(|s| s.starts_with(&header)),
            "the §F4.6 generated-roster header paints: {texts:?}"
        );
        assert!(
            texts.contains(&"Cinnamon Roll"),
            "the sighted special labels"
        );
        assert!(texts.contains(&"???"), "undiscovered cells paint as ???");
        assert!(
            texts.contains(&"SPECIAL CATS") && texts.contains(&"HEADS"),
            "the reachable art-group captions paint"
        );
        assert!(
            texts.iter().any(|text| text.contains("1/25 found")),
            "the compact head row paints distinct-design progress: {texts:?}"
        );
        assert!(
            texts.contains(&"ACCESSORIES") && texts.contains(&"Red Bow"),
            "the v3 accessory chips paint: {texts:?}"
        );

        // Introspection mirrors the same model (screen == introspection).
        let lines = s.controls_lines();
        assert!(
            lines.iter().any(|l| l.starts_with(&format!(
                "kittylog sightings=2 collected=3 denominator={}",
                aterm_effects::cat_glyphs_gen::GLYPH_IDS.len()
            ))),
            "{lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("tier=specials key=spec_sleeping") && l.contains("seen=true")),
            "the sleeping-special row serializes"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("tier=heads key=heads") && l.contains("langs=[en]")),
            "head language chips serialize"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("tier=accessories key=acc_bow")
                    && l.contains("seen=true")
                    && l.contains("count=1")),
            "the bow chip serializes"
        );
        assert_eq!(
            lines.iter().filter(|l| l.starts_with("field ")).count(),
            0,
            "no editable fields serialize on the read-only page"
        );

        // Read-only by construction: no control is ever targetable here.
        assert_eq!(
            s.action_target(),
            None,
            "rows on the Kitty Log page never activate"
        );
    }

    /// The legacy overlay serializer reflects both panes: the state line carries
    /// pane/category, the `preview` line reports the card's live subject (graft #3),
    /// and only the active category's fields serialize outside search mode.
    #[test]
    fn controls_lines_reflect_panes_and_preview() {
        let mut s = SettingsState::from_config(&cfg());
        let lines = s.controls_lines();
        assert!(lines[0].contains("pane=sidebar"), "{}", lines[0]);
        assert!(lines[0].contains("category=Appearance"), "{}", lines[0]);
        let preview = lines
            .iter()
            .find(|l| l.starts_with("preview "))
            .expect("a preview line");
        assert!(preview.contains("key=theme"), "{preview}");
        assert!(preview.contains("kind=theme"), "{preview}");
        let n = lines.iter().filter(|l| l.starts_with("field key=")).count();
        assert_eq!(
            n,
            category_controls(&s.fields, prefs::Section::Appearance).len(),
            "only the active category's fields serialize"
        );

        s.focus_content();
        s.set_category(prefs::Section::Cursor);
        let lines = s.controls_lines();
        assert!(lines[0].contains("pane=content"), "{}", lines[0]);
        assert!(lines[0].contains("category=Cursor"), "{}", lines[0]);
        let preview = lines.iter().find(|l| l.starts_with("preview ")).unwrap();
        assert!(preview.contains("kind=cursor"), "{preview}");

        // While filtering, the flat cross-category set serializes and the preview
        // rests on the default mock (key=none) — matching the painted card.
        s.searching = true;
        s.query = "font".to_string();
        s.snap_selection_visible();
        let lines = s.controls_lines();
        let preview = lines.iter().find(|l| l.starts_with("preview ")).unwrap();
        assert!(preview.contains("key=none"), "{preview}");
        assert!(preview.contains("kind=default"), "{preview}");
        let shown = lines.iter().filter(|l| l.starts_with("field key=")).count();
        assert_eq!(shown, s.visible_indices().len());
    }

    /// Design §4.6 (legacy-model audit): the overlay serializer records the SIDEBAR (active
    /// category + the full section list) and interleaves `group label="…"` lines
    /// before their fields in grouped mode — the painted two-pane/group-box
    /// structure, machine-readable (screen == introspection). The flat filtered
    /// list stays group-less (no boxes are painted there).
    #[test]
    fn controls_lines_expose_sidebar_and_groups() {
        let mut s = SettingsState::from_config(&cfg());
        let lines = s.controls_lines();
        let sidebar = lines
            .iter()
            .find(|l| l.starts_with("sidebar "))
            .expect("a sidebar line");
        assert_eq!(
            sidebar.as_str(),
            "sidebar selected=appearance \
             sections=[appearance,cursor,cursor kitty,typography,window & tabs,input,terminal,performance,security,packages,kitty log]",
        );
        // Group captions interleave BEFORE their fields, exactly as painted:
        // Appearance = Theme (theme, window_theme) then Colors (4 colour rows).
        let body: Vec<&str> = lines
            .iter()
            .filter(|l| l.starts_with("group ") || l.starts_with("field "))
            .map(String::as_str)
            .collect();
        assert!(body[0].starts_with("group label=\"Theme\""), "{}", body[0]);
        assert!(body[1].starts_with("field key=theme "), "{}", body[1]);
        assert!(
            body[2].starts_with("field key=window_theme "),
            "{}",
            body[2]
        );
        // window_colorspace closes the Theme box (full-coverage batch).
        assert!(
            body[3].starts_with("field key=window_colorspace "),
            "{}",
            body[3]
        );
        assert!(body[4].starts_with("group label=\"Colors\""), "{}", body[4]);
        assert!(body[5].starts_with("field key=foreground "), "{}", body[5]);
        assert_eq!(
            body.iter().filter(|l| l.starts_with("field ")).count(),
            category_controls(&s.fields, prefs::Section::Appearance).len(),
            "group lines change no field set/order"
        );
        // The sidebar line tracks the active category…
        s.set_category(prefs::Section::Security);
        let lines = s.controls_lines();
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("sidebar selected=security ")),
            "sidebar line follows the category"
        );
        // …and the filtered flat list serializes without group lines (none painted).
        s.searching = true;
        s.query = "cursor".to_string();
        s.snap_selection_visible();
        let lines = s.controls_lines();
        assert!(
            !lines.iter().any(|l| l.starts_with("group ")),
            "the flat search list paints no group boxes"
        );
        assert!(lines.iter().any(|l| l.starts_with("sidebar ")));
    }

    /// Design §3.3 (audit): the transient footer status ("saved: …"/"reset: …")
    /// clears on the NEXT navigation input — selection moves, category switches,
    /// pane toggles — instead of lingering painted for the rest of the session.
    /// (It still survives the very input that CREATED it: commits set it AFTER
    /// their internal navigation.)
    #[test]
    fn transient_status_clears_on_navigation() {
        let band = 20;
        let mut s = SettingsState::from_config(&cfg());
        s.pane = SettingsPane::Content;
        s.status = Some("saved: theme = Nord".into());
        s.move_selection_grouped(1, band, usize::MAX);
        assert_eq!(s.status, None, "content ↑/↓ clears the status");
        s.status = Some("reset: window_theme = (default)".into());
        s.set_category(prefs::Section::Terminal);
        assert_eq!(s.status, None, "a category switch clears the status");
        s.status = Some("saved: …".into());
        s.sidebar_move(1);
        assert_eq!(s.status, None, "sidebar ↑/↓ clears the status");
        s.status = Some("saved: …".into());
        s.focus_sidebar();
        assert_eq!(s.status, None, "a pane toggle clears the status");
        s.status = Some("saved: …".into());
        s.focus_content();
        assert_eq!(s.status, None, "…in both directions");
        // Flat (filtered) navigation clears too.
        s.search_begin();
        s.status = Some("saved: …".into());
        s.move_selection(1, band);
        assert_eq!(s.status, None, "filtered ↑/↓ clears the status");
    }

    /// Design §3.2 (audit): footnotes wrap to the BOX width. On the 64-col rung
    /// (full sidebar, no preview) the Permissions footnote is wider than its box:
    /// the shared layout splits it into TWO 1-cell Footnote rows losing no text,
    /// and every painted footnote Text FITS inside the box — the pre-fix single
    /// unwrapped row painted ~175 px past the box edge, under the scrollbar.
    #[test]
    fn footnotes_wrap_to_the_box_width() {
        let g = SettingsGeom {
            cw: 8.0,
            ch: 16.0,
            font_px: 13.0,
            cols: 64,
            panel_rows: 38,
        };
        let wrap = footnote_wrap_chars(g.cols);
        let mut s = SettingsState::from_config(&cfg());
        s.set_category(prefs::Section::Security);
        let notes: Vec<&str> = category_layout(&s.fields, s.category, wrap)
            .iter()
            .filter_map(|r| match r {
                GroupRow::Footnote(n) => Some(*n),
                _ => None,
            })
            .collect();
        assert_eq!(
            notes.len(),
            2,
            "the Permissions footnote wraps to two rows: {notes:?}"
        );
        let source = prefs::group_footnote("Permissions").expect("Permissions footnote");
        assert!(
            notes[0].starts_with("Off by default."),
            "the first wrapped row retains the current default-policy disclosure: {notes:?}"
        );
        assert_eq!(
            notes.join(" "),
            source,
            "the two-row layout retains the complete current capability disclosure"
        );
        assert!(
            notes[0].chars().count() <= wrap,
            "the first row respects the authored wrap: {:?}",
            notes[0]
        );
        assert!(
            notes[1].ends_with("macOS and Windows."),
            "the current platform disclosure remains in the retained remainder: {notes:?}"
        );

        // Painted fit: both rows are drawn and end inside the box's right edge
        // (the band at 38 rows holds the whole Permissions group). The retained
        // second-row source may be visually elided by the containment backstop,
        // so match either the exact row or its painted ellipsis prefix.
        let box_right = g.cols as f32 * g.cw - g.cw * 2.5;
        let painted: Vec<(f32, f32)> = tray(&s, &g)
            .prims
            .iter()
            .filter_map(|p| match p {
                DrawPrim::Text { x, s: txt, px, .. }
                    if notes.iter().any(|note| {
                        *note == txt
                            || txt
                                .strip_suffix('…')
                                .is_some_and(|prefix| note.starts_with(prefix))
                    }) =>
                {
                    // footnotes paint in the UI face — measure with its metric
                    Some((*x, *x + ui_text_width(txt, *px)))
                }
                _ => None,
            })
            .collect();
        assert_eq!(painted.len(), 2, "both wrapped rows painted");
        for (x0, x1) in painted {
            assert!(
                x1 <= box_right + 0.5,
                "footnote text fits the box: right {x1} vs box_right {box_right} (x {x0})"
            );
        }

        // The current disclosure fits on one row at the dedicated window's
        // 132 columns. Crossing to the wide rung may reflow it, but must retain
        // the complete source rather than dropping the footnote.
        let wide_notes = category_layout(&s.fields, s.category, footnote_wrap_chars(132))
            .iter()
            .filter_map(|row| match row {
                GroupRow::Footnote(note) => Some(*note),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            wide_notes,
            vec![source],
            "wide layout retains the disclosure"
        );
    }

    #[test]
    fn edit_pending_blank_buffer_removes_the_key() {
        let mut s = SettingsState::from_config(&cfg());
        s.selected = s
            .fields
            .iter()
            .position(|f| f.key == crate::prefs::EDIT_FONT_FAMILY)
            .unwrap();
        s.set_category(prefs::Section::Typography); // actions target the active category
        s.edit_begin();
        s.edit_push(' '); // whitespace-only → revert to default (remove key)
        assert_eq!(
            s.edit_pending(),
            Some((crate::prefs::EDIT_FONT_FAMILY, None))
        );
    }

    /// A settings state with the colour wheel open on `key`, seeded from `seed`
    /// (the wheel-test fixture; `[9, 9, 9]` is the theme-fallback stand-in).
    fn wheel_state(key: &str, seed: Option<&str>) -> SettingsState {
        let mut s = SettingsState::from_config(&cfg());
        s.pane = SettingsPane::Content;
        let idx = s.fields.iter().position(|f| f.key == key).unwrap();
        s.fields[idx].seed = seed.map(str::to_string);
        s.set_category(prefs::section_of(key));
        s.selected = idx;
        assert!(s.wheel_open([9, 9, 9]), "wheel opens on a Color row");
        s
    }

    /// ↵ on a Color row opens the WHEEL (never the text editor), seeded from the
    /// configured hex (fallback: the caller's theme colour for an unset key), and
    /// the wheel + popup menu are mutually exclusive. Esc discards — the working
    /// colour never persists on its own.
    #[test]
    fn color_wheel_opens_seeds_and_excludes_menu() {
        let mut s = SettingsState::from_config(&cfg());
        s.pane = SettingsPane::Content;
        let idx = s
            .fields
            .iter()
            .position(|f| f.key == crate::prefs::EDIT_FOREGROUND)
            .unwrap();
        s.fields[idx].seed = Some("#FF0000".to_string());
        s.selected = idx; // Appearance is the default category
        assert!(
            !s.edit_begin(),
            "Color rows no longer open the free-text editor"
        );
        let fp = s.fingerprint();
        assert!(s.wheel_open([9, 9, 9]));
        assert_ne!(s.fingerprint(), fp, "opening the wheel repaints");
        {
            let w = s.wheel.as_ref().unwrap();
            assert!(
                w.h.abs() < 1e-4 && (w.s - 1.0).abs() < 1e-4 && (w.v - 1.0).abs() < 1e-4,
                "#FF0000 seeds (h, s, v) = (0, 1, 1)"
            );
            assert_eq!(w.hex, "#FF0000");
            assert_eq!(w.focus, WheelFocus::Wheel);
        }
        assert!(!s.wheel_open([9, 9, 9]), "already open");
        s.wheel_cancel();
        assert!(s.wheel.is_none(), "Esc discards the working colour");

        // An UNSET colour key seeds from the caller-supplied theme fallback.
        let s2 = wheel_state(crate::prefs::EDIT_CURSOR_COLOR, None);
        assert_eq!(s2.wheel.as_ref().unwrap().hex, "#090909");

        // Mutual exclusion, both ways: opening one popover closes the other.
        let theme_idx = s
            .fields
            .iter()
            .position(|f| f.key == crate::prefs::EDIT_THEME)
            .unwrap();
        s.selected = theme_idx;
        assert!(s.menu_open());
        s.selected = idx;
        assert!(s.wheel_open([9, 9, 9]));
        assert!(s.menu.is_none(), "the wheel evicts the menu");
        s.selected = theme_idx;
        assert!(s.menu_open());
        assert!(s.wheel.is_none(), "the menu evicts the wheel");

        // A non-Color row never opens the wheel.
        assert!(!s.wheel_open([9, 9, 9]), "theme row is not a Color row");
    }

    /// Graft #4: the wheel's commit is ALWAYS a canonical uppercase `#RRGGBB` that
    /// prefs' save-time hex validation accepts by construction — for wheel scrubs,
    /// typed hex, and 3-digit shorthand alike; an EMPTIED hex commits `None`
    /// (reset to the theme default).
    #[test]
    fn wheel_commit_is_canonical_parseable_hex() {
        let mut s = wheel_state(crate::prefs::EDIT_FOREGROUND, Some("#FF0000"));
        // Scrub to an arbitrary colour: still canonical + parseable.
        s.wheel_set_hs(0.61, 0.37);
        s.wheel_set_v(0.83);
        let (key, val) = s.wheel_pending().unwrap();
        assert_eq!(key, crate::prefs::EDIT_FOREGROUND);
        let hex = val.unwrap();
        assert_eq!(hex.len(), 7, "#RRGGBB: {hex}");
        assert!(
            hex.starts_with('#')
                && hex[1..]
                    .chars()
                    .all(|c| c.is_ascii_digit() || c.is_ascii_uppercase()),
            "canonical uppercase: {hex}"
        );
        assert!(
            crate::app_config::parse_hex_color(&hex).is_some(),
            "typed_item's hex validation accepts the commit by construction: {hex}"
        );

        // Keyboard: Tab cycles Wheel → Value → Hex; typed shorthand live-syncs the
        // wheel and commits its 6-digit canonical form.
        s.wheel_focus_next();
        assert_eq!(s.wheel.as_ref().unwrap().focus, WheelFocus::Value);
        s.wheel_arrow(1.0, 0.0, false); // v += 0.02 on the value slider
        assert!((s.wheel.as_ref().unwrap().v - 0.85).abs() < 1e-4);
        s.wheel_focus_next();
        assert_eq!(s.wheel.as_ref().unwrap().focus, WheelFocus::Hex);
        for _ in 0..7 {
            s.wheel_hex_backspace(); // clear the synced "#RRGGBB"
        }
        assert!(s.wheel.as_ref().unwrap().hex.is_empty());
        // Empty hex + ↵ = remove the key (back to the theme default).
        assert_eq!(
            s.wheel_pending(),
            Some((crate::prefs::EDIT_FOREGROUND, None))
        );
        for c in "#f00".chars() {
            s.wheel_hex_push(c);
        }
        {
            let w = s.wheel.as_ref().unwrap();
            assert_eq!(w.hex, "#F00", "digits stored uppercase as typed");
            assert!(
                w.h.abs() < 1e-4 && (w.s - 1.0).abs() < 1e-4 && (w.v - 1.0).abs() < 1e-4,
                "a parseable buffer live-syncs the wheel"
            );
        }
        assert_eq!(
            s.wheel_pending().unwrap().1.as_deref(),
            Some("#FF0000"),
            "shorthand commits its canonical 6-digit form"
        );
        // Junk is rejected at the keystroke, never at commit.
        s.wheel_hex_push('z');
        assert_eq!(s.wheel.as_ref().unwrap().hex, "#F00");
        // Arrows on the hex field type, they do not scrub.
        let before = s.wheel.as_ref().unwrap().h;
        s.wheel_arrow(1.0, 0.0, false);
        assert_eq!(s.wheel.as_ref().unwrap().h, before);
    }

    /// ONE pure `wheel_geom` shared by painter + hit-test: the popover sits inside
    /// the card, each hit region resolves the control painted there (disk → polar
    /// h/s, slider → v, hex → focus, outside → cancel), and the painter emits the
    /// HsvDisk prim clipped like the menu.
    #[test]
    fn wheel_geom_hit_and_painter_agree() {
        let s = wheel_state(crate::prefs::EDIT_FOREGROUND, Some("#FF0000"));
        let g = full_geom();
        let wg = wheel_geom(&s, &g).expect("open wheel has a placement");
        let (card_w, card_h) = (g.cols as f32 * g.cw, g.panel_rows as f32 * g.ch);
        assert!(
            wg.x >= 0.0 && wg.x + wg.w <= card_w,
            "popover inside the card (x)"
        );
        assert!(
            wg.y >= 0.0 && wg.y + wg.h <= card_h,
            "popover inside the card (y)"
        );

        // Straight right of the disk centre at half radius → h = 0.25 turns
        // (3 o'clock, clockwise from 12), s = 0.5.
        match wheel_hit(&s, &g, wg.disk_cx + wg.disk_r * 0.5, wg.disk_cy) {
            Some(WheelHit::Disk { h, s }) => {
                assert!((h - 0.25).abs() < 1e-3, "h {h}");
                assert!((s - 0.5).abs() < 1e-3, "s {s}");
            }
            other => panic!("disk point resolves Disk, got {other:?}"),
        }
        // The slider midpoint reads v = 0.5.
        let (sx, sy, sw, sh) = wg.slider;
        match wheel_hit(&s, &g, sx + sw * 0.5, sy + sh * 0.5) {
            Some(WheelHit::Slider { v }) => assert!((v - 0.5).abs() < 1e-3, "v {v}"),
            other => panic!("slider point resolves Slider, got {other:?}"),
        }
        // The hex frame focuses; popover chrome swallows; outside cancels.
        let (hx, hy, hw, hh) = wg.hex;
        assert_eq!(
            wheel_hit(&s, &g, hx + hw * 0.5, hy + hh * 0.5),
            Some(WheelHit::Hex)
        );
        assert_eq!(
            wheel_hit(&s, &g, wg.x - 3.0, wg.y - 3.0),
            None,
            "click-away misses"
        );

        // Painter: the popover paints the HsvDisk prim (the one vocabulary
        // addition), clipped to the card like the menu.
        let t = tray(&s, &g);
        assert!(
            t.prims
                .iter()
                .any(|p| matches!(p, DrawPrim::HsvDisk { .. })),
            "the wheel paints its disk"
        );
        assert!(
            t.prims
                .iter()
                .any(|p| matches!(p, DrawPrim::ClipPush { .. }))
        );

        // Closing removes the popover; extreme geometry never panics with it open.
        let mut s2 = wheel_state(crate::prefs::EDIT_FOREGROUND, Some("#FF0000"));
        for cols in [132, 96, 80, 64, 24, 12] {
            let short = SettingsGeom {
                cw: 8.0,
                ch: 16.0,
                font_px: 13.0,
                cols,
                panel_rows: 6,
            };
            let _ = tray(&s2, &short);
        }
        s2.wheel_cancel();
        assert!(
            !tray(&s2, &g)
                .prims
                .iter()
                .any(|p| matches!(p, DrawPrim::HsvDisk { .. })),
            "closing the wheel removes the disk"
        );
    }

    /// Design §5.4: while the wheel is open the preview mock renders the CANDIDATE
    /// colour on the driven element — a background scrub re-tints the mock's bg
    /// panel with the UNCOMMITTED colour — and each quantized scrub step repaints.
    /// The legacy overlay serializer records the same candidate.
    #[test]
    fn wheel_scrub_tints_preview_and_serializes() {
        let mut s = wheel_state(crate::prefs::EDIT_BACKGROUND, None);
        // Scrub to pure red (h = 0, s = 1, v = 1).
        s.wheel_set_hs(0.0, 1.0);
        s.wheel_set_v(1.0);
        let t = tray(&s, &full_geom());
        assert!(
            t.prims.iter().any(
                |p| matches!(p, DrawPrim::Panel { fill, .. } if [fill[0], fill[1], fill[2]] == [255, 0, 0])
            ),
            "the mock bg panel paints the uncommitted candidate"
        );
        let fp = s.fingerprint();
        s.wheel_set_v(0.5);
        assert_ne!(s.fingerprint(), fp, "a scrub step repaints");

        // The colorwheel introspection line reports exactly the candidate on glass.
        let lines = s.controls_lines();
        let line = lines
            .iter()
            .find(|l| l.starts_with("colorwheel "))
            .expect("open wheel serialized");
        assert!(line.contains("key=background"), "{line}");
        assert!(line.contains("hex=\"#800000\""), "{line}");
        assert!(line.contains("focus=wheel"), "{line}");
        s.wheel_cancel();
        assert!(
            !s.controls_lines()
                .iter()
                .any(|l| l.starts_with("colorwheel ")),
            "closed wheel serializes no line"
        );
    }

    /// Confirming the search bar with an EMPTIED query (Enter/↓ after backspacing
    /// the query away) lands back in GROUPED mode, so `search_confirm` must
    /// re-anchor like `search_clear`: the category follows the selection (else
    /// activation/reset/highlight silently die on a selection stranded outside the
    /// active category) and `scroll` re-zeroes (a stale flat FIELD-index scroll
    /// out-ranges the GroupRow layout and paints an empty band).
    #[test]
    fn empty_query_confirm_reanchors_grouped_mode() {
        let band = pane_geom_cells(132, 38).group_band();
        let mut s = SettingsState::from_config(&cfg());
        assert_eq!(s.category, prefs::Section::Appearance);
        // `/`, type a Terminal-only query — the selection follows the filter.
        s.search_begin();
        for c in "scrollback".chars() {
            s.search_push(c);
        }
        assert_eq!(
            prefs::section_of(s.fields[s.selected].key),
            prefs::Section::Terminal
        );
        // Backspace the query away (everything visible again), wheel the flat
        // list (FIELD-index units), then confirm with Enter.
        for _ in 0.."scrollback".len() {
            s.search_backspace();
        }
        s.scroll_body(band as isize, band);
        assert!(s.scroll > 0, "the flat list scrolled (field-index units)");
        s.search_confirm();
        assert!(
            !s.filtering(),
            "an empty query confirms back into grouped mode"
        );
        assert_eq!(s.pane, SettingsPane::Content);
        // The category re-anchored on the selection, so the row stays actionable.
        assert_eq!(s.category, prefs::Section::Terminal);
        assert!(
            s.action_target().is_some(),
            "the confirmed selection stays live"
        );
        // The scroll is a valid GroupRow offset again; the caller's clamp (as
        // `settings_search_confirm` does) brings the selected box into view.
        let rows = category_layout(&s.fields, s.category, usize::MAX);
        assert!(
            s.scroll <= max_group_scroll(&rows, band),
            "scroll {}",
            s.scroll
        );
        s.clamp_group_scroll(band, usize::MAX);
        let sel = rows
            .iter()
            .position(|r| matches!(r, GroupRow::Control(i) if *i == s.selected))
            .expect("the selected control is laid out");
        assert!(group_row_fully_visible(&rows, s.scroll, band, sel));
    }

    /// Tab into the content pane while a search FILTER is active must keep the
    /// flat-list selection: `category` is deliberately stale while filtering, so
    /// the old unconditional snap yanked the selection onto the stale category's
    /// first control — off the filtered list, highlight-less and action-dead.
    #[test]
    fn pane_toggle_keeps_selection_while_filtering() {
        let mut s = SettingsState::from_config(&cfg());
        s.search_begin();
        for c in "scrollback".chars() {
            s.search_push(c);
        }
        s.search_confirm(); // non-empty query: the filter stays on
        assert!(s.filtering());
        let kept = s.selected;
        assert_ne!(
            prefs::section_of(s.fields[kept].key),
            s.category,
            "category is stale"
        );
        s.focus_sidebar();
        s.focus_content(); // Tab, Tab
        assert_eq!(
            s.selected, kept,
            "the filtered selection survives the pane round-trip"
        );
        assert!(s.action_target().is_some(), "…and stays actionable");
    }

    /// The scrollbar thumb must span the WHOLE track: at max scroll
    /// (`before == total − visible`) it lands flush at the track end — the old
    /// `before/total` mapping onto the already-shortened travel stranded it near
    /// the top (~2 % along) with the content fully scrolled.
    #[test]
    fn scrollbar_thumb_spans_the_full_track() {
        let r = Roles::from_theme(Theme::default());
        // The default 38-row window's Terminal numbers: 29 laid-out cells / 25-cell band.
        let (total, visible, track_h, ch) = (29.0_f32, 25.0_f32, 400.0_f32, 16.0_f32);
        let thumb = |before: f32| {
            let mut prims = Vec::new();
            paint_scrollbar(
                &mut prims, &r, 0.0, 0.0, track_h, 2.0, before, visible, total, ch,
            );
            match prims[1] {
                DrawPrim::Panel { y, h, .. } => (y, h),
                _ => panic!("the thumb is the second scrollbar prim"),
            }
        };
        let (y0, _) = thumb(0.0);
        assert_eq!(y0, 0.0, "unscrolled: the thumb sits at the track top");
        let (y1, h1) = thumb(total - visible);
        assert!(
            (y1 + h1 - track_h).abs() < 0.5,
            "fully scrolled: the thumb reaches the track end (y={y1} h={h1})"
        );
    }

    /// The flat search band's scrollbar offset is in LAID-OUT rows (headers
    /// included), converted from the FIELD-index unit `scroll_body` clamps in —
    /// the raw field index understated the thumb whenever headers interleave.
    #[test]
    fn flat_scrollbar_offset_counts_headers() {
        let s = SettingsState::from_config(&cfg());
        assert_eq!(
            flat_rows_before(&s.fields, None, 0),
            0,
            "unscrolled: nothing above"
        );
        let total = body_layout_masked(&s.fields, None, 0, usize::MAX).len();
        let last = s.fields.len() - 1;
        let before = flat_rows_before(&s.fields, None, last);
        assert!(
            before > last,
            "the offset counts the interleaved section headers"
        );
        assert!(
            before < total,
            "the window's own rows stay below the offset"
        );
    }

    /// The config-reload rebuild re-clamps `scroll` PER MODE: grouped `scroll` is
    /// a GroupRow index while `selected` is a field index, so the v1
    /// `scroll.min(selected)` clamp compared incommensurable units — it yanked the
    /// band toward the category top the frame an in-panel save's reload landed.
    #[test]
    fn reload_rebuild_clamps_scroll_mode_aware() {
        let band = 8; // a user-shrunk window: Appearance (18 cells) overflows
        let mut s = SettingsState::from_config(&cfg());
        // Minimum contrast: an early-BUILT field (small index) that lays out LATE
        // (the Text & Contrast box closes Appearance) — the widest units gap.
        let mc = s
            .fields
            .iter()
            .position(|f| f.key == crate::prefs::EDIT_MINIMUM_CONTRAST)
            .unwrap();
        s.selected = mc;
        // Wheel fully down, then the gesture rescue (the selected row stays aboard).
        s.scroll_grouped(100, band, usize::MAX);
        s.clamp_group_scroll(band, usize::MAX);
        let kept = s.scroll;
        assert!(
            kept > s.selected,
            "GroupRow scroll exceeds the field index — units differ"
        );
        s.rebuild_fields(crate::prefs::editable_fields(&cfg()), band, usize::MAX);
        assert_eq!(
            s.scroll, kept,
            "reload preserves the grouped scroll (no min-yank)"
        );
        // The symmetric transient: a stale scroll past the layout re-clamps down.
        s.scroll = 100;
        s.rebuild_fields(crate::prefs::editable_fields(&cfg()), band, usize::MAX);
        let rows = category_layout(&s.fields, s.category, usize::MAX);
        assert!(
            s.scroll <= max_group_scroll(&rows, band),
            "stale scroll re-clamped"
        );
        // Flat (search) mode keeps the existing semantics: the selected row is
        // brought back into the laid-out band.
        s.search_begin();
        for c in "cursor".chars() {
            s.search_push(c);
        }
        s.scroll = 99;
        s.rebuild_fields(crate::prefs::editable_fields(&cfg()), band, usize::MAX);
        let mask = s.visible_mask();
        assert!(
            body_layout_masked(&s.fields, mask.as_deref(), s.scroll, band)
                .iter()
                .any(|r| matches!(r, BodyRow::Control(i) if *i == s.selected)),
            "flat mode: the selected row is inside the band after the rebuild"
        );
    }

    /// The wheel popover's RIGHT COLUMN compresses into a short popover: when the
    /// disk shrink caps `h_pop`, the hex field must stay inside the panel and
    /// reachable — it used to paint below `wg.h`, where `wheel_hit` returned
    /// `None` and a click on the visible field cancelled the whole wheel.
    #[test]
    fn wheel_right_column_stays_inside_short_popover() {
        let s = wheel_state(crate::prefs::EDIT_FOREGROUND, Some("#FF0000"));
        for panel_rows in [5, 6, 7, 8, 12, 38] {
            let g = SettingsGeom {
                cw: 8.0,
                ch: 16.0,
                font_px: 13.0,
                cols: 132,
                panel_rows,
            };
            let wg = wheel_geom(&s, &g).expect("open wheel has a placement");
            let (hx, hy, hw, hh) = wg.hex;
            assert!(
                hy >= wg.y && hy + hh <= wg.y + wg.h + 1e-3,
                "hex inside the popover at panel_rows={panel_rows}"
            );
            if hh > 0.0 {
                assert_eq!(
                    wheel_hit(&s, &g, hx + hw * 0.5, hy + hh * 0.5),
                    Some(WheelHit::Hex),
                    "a click on the painted hex focuses it at panel_rows={panel_rows}"
                );
            }
        }
    }

    /// Containment (the two-pane layout): a popup chip labelled with a long
    /// verbatim custom value and a segmented control on the icon-strip rung must
    /// both stay inside the content pane — the stale `cw*6` x-floor let them
    /// paint across the sidebar seam, over the hairline and category labels.
    #[test]
    fn wide_widgets_stay_inside_the_content_pane() {
        let r = Roles::from_theme(Theme::default());
        // A preserved split-theme value labels the chip verbatim (no swatches) on
        // the full-sidebar, no-preview ladder rung (64..96 cols).
        let g = SettingsGeom {
            cw: 8.0,
            ch: 16.0,
            font_px: 13.0,
            cols: 64,
            panel_rows: 16,
        };
        let (v_left, v_right) = (content_v_left(&g), content_v_right(&g));
        assert!(
            v_left >= pane_geom(&g).content_x(g.cw),
            "the floor sits right of the seam"
        );
        let mut prims = Vec::new();
        let x = popup_chip(
            &mut prims,
            &[],
            "dark:Solarized Dark,light:Solarized Light",
            &r,
            g.cw,
            g.font_px,
            0.0,
            10.0,
            v_left,
            v_right,
        );
        assert!(
            x >= v_left,
            "the chip floors at the content pane, not the sidebar (x={x})"
        );
        let Some(DrawPrim::Panel { x: cx, w: cw_, .. }) = prims.first() else {
            panic!("the chip panel is the first prim");
        };
        assert!(
            *cx >= v_left && cx + cw_ <= v_right + 1e-3,
            "the chip spans v_left..v_right"
        );

        // A segmented control on the icon-strip rung (24 cols) floors there too —
        // the old `cw*6` landed it inside the 8-cell strip.
        let g2 = SettingsGeom {
            cw: 8.0,
            ch: 16.0,
            font_px: 13.0,
            cols: 24,
            panel_rows: 16,
        };
        let (v_left2, v_right2) = (content_v_left(&g2), content_v_right(&g2));
        let mut prims2 = Vec::new();
        let x0 = segmented(
            &mut prims2,
            &["auto", "light", "dark"],
            "auto",
            &r,
            g2.cw,
            g2.font_px,
            0.0,
            10.0,
            v_left2,
            v_right2,
        );
        assert!(
            x0 >= v_left2 && v_left2 > pane_geom(&g2).content_x(g2.cw) - 1e-3,
            "the segmented track clears the icon strip (x0={x0})"
        );
    }
}
