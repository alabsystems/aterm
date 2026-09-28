// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Settings control model and the chrome primitives the in-grid overlays share.
//! Native Settings views reuse [`SettingsState`] preference metadata/projections, while
//! production pixels, input, accessibility, and machine inspection compile from
//! [`crate::native_ui`]. The retired card's painter, input model and hit-testing are
//! gone; what remains here is the control catalogue, [`demo_style`] and the pane
//! ladder that `lib.rs`'s tick lane still names, the overlay geometry and colour
//! roles ([`SettingsGeom`], [`Roles`]), the row writers ([`blank_row`],
//! [`write_str`]), and the aurora / rainbow-banner painters native Settings draws.

use aterm_core::terminal::RenderCell;
use aterm_render::Theme;

use crate::app_config::Config;
use crate::chrome_band;
use crate::prefs::{self, EditField, EditKind};
use crate::widget::{DrawPrim, TextWeight, rgba};

/// Shared semantic state embedded in each native Settings tab: the editable control
/// catalogue plus the search/selection fields the native view reads through
/// `legacy`. The same value is the `cfg(test)` retired-card overlay, which is why the
/// landing/demo fields that `lib.rs`'s tick lane still names live here.
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
    /// The fuzzy-search query filtering the visible controls (empty ⇒ every control shows).
    /// Matched against each field's label, key, section name, and [`prefs::keywords_of`].
    pub(crate) query: String,
    /// Whether the search bar is FOCUSED.
    pub(crate) searching: bool,
    /// Retired preview-card demo phase, still advanced by `lib.rs`'s demo tick.
    pub(crate) demo_phase: u32,
    /// The ACTIVE category: [`demo_style`] only answers for a selection inside it.
    pub(crate) category: prefs::Section,
    /// Retired card pane focus. Nothing reads it; `lib.rs`'s demo-tick test still
    /// writes it, so it stays until that test stops opening the retired card.
    #[cfg(test)]
    #[allow(dead_code, reason = "written by lib.rs's settings_demo_tick test only")]
    pub(crate) pane: SettingsPane,
    /// Whether the retired LANDING page is up. Nothing sets it any more; `lib.rs`'s
    /// animation-tick lane still reads it.
    pub(crate) landing: bool,
    /// Animation phase of the retired landing page and kitty cameo, advanced by
    /// [`Self::tick_landing`].
    pub(crate) landing_phase: u32,
    /// The retired kitty cameo, expired by [`Self::tick_landing`] after
    /// [`KITTY_POP_TICKS`]. Nothing summons one any more; `lib.rs` still clears it.
    pub(crate) kitty_pop: Option<KittyPop>,
    /// The ids of the loaded Trail Packs (`cursor_trail_packs`), so the
    /// `cursor_trail_style` picker lists a `pack:<id>` option per loaded pack
    /// (the dynamic twin of the static [`prefs::CURSOR_TRAIL_STYLES`]). Sorted; empty
    /// when none are configured.
    pub(crate) trail_pack_ids: Vec<String>,
}

/// The retired kitty cameo (§L.4): only its start phase is left, for
/// [`SettingsState::tick_landing`] to expire it.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct KittyPop {
    /// `landing_phase` at summon time — the cameo's local clock zero.
    pub(crate) start: u32,
}

/// Cameo lifetime in demo ticks (~30fps ⇒ ≈2.6 s on screen).
pub(crate) const KITTY_POP_TICKS: u32 = 78;

/// The retired card's two keyboard-focus zones (see [`SettingsState::pane`]).
#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SettingsPane {
    Sidebar,
    Content,
}

/// Host facts an overlay painter cannot know on its own, threaded through
/// [`crate::overlay::OverlayModel::tray`]. Native Settings receives equivalent host
/// facts through its renderer-native view context.
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

impl SettingsState {
    /// Build the snapshot from the live config with no Trail Packs loaded.
    #[cfg(test)]
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
            query: String::new(),
            searching: false,
            demo_phase: 0,
            category: prefs::Section::ORDER[0],
            #[cfg(test)]
            pane: SettingsPane::Sidebar,
            landing: false,
            landing_phase: 0,
            kitty_pop: None,
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

    /// One animation tick of the retired landing page / kitty cameo lane: advance the
    /// phase and expire a finished cameo. `lib.rs`'s `next_demo_tick` fire calls it.
    pub(crate) fn tick_landing(&mut self) {
        self.landing_phase = self.landing_phase.wrapping_add(1);
        if let Some(k) = &self.kitty_pop
            && self.landing_phase.wrapping_sub(k.start) > KITTY_POP_TICKS
        {
            self.kitty_pop = None;
        }
    }

    /// Whether the search filter owns the pane: the search bar is focused or a query is
    /// active.
    pub(crate) fn filtering(&self) -> bool {
        self.searching || !self.query.trim().is_empty()
    }

    /// Activate a sidebar category (keyboard move or mouse click): per-category scroll
    /// resets on a CHANGE only (re-clicking the active category keeps your place), and
    /// the selection snaps onto the category's first laid-out control so the content
    /// pane always has a live target.
    #[cfg(test)]
    pub(crate) fn set_category(&mut self, sec: prefs::Section) {
        if self.category != sec {
            self.category = sec;
            self.scroll = 0;
        }
        self.snap_selection_category();
    }

    /// Pull `selected` onto the active category's FIRST control (group-laid-out order)
    /// when it points outside the category.
    #[cfg(test)]
    pub(crate) fn snap_selection_category(&mut self) {
        let controls = category_controls(&self.fields, self.category);
        if !controls.contains(&self.selected) {
            self.selected = controls.first().copied().unwrap_or(0);
        }
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
            // An off-roster but live value normalizes to its canonical name too
            // — `rainbow kitty v2` reads back as `music box`, the same way an
            // offered alias reads back as its picker row.
            enum_recognized(f)
                .or_else(|| enum_offered_or_runtime(f))
                .unwrap_or(raw)
        } else {
            raw
        }
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

/// The voice/style a row's configured value RESOLVES to at RUNTIME even when the
/// picker does not offer it — the third state the Enum layer was missing.
///
/// `enum_recognized` answers one question: is this value one of the options?
/// For every row but one that is the same as "does the engine accept it", and
/// the two were treated as interchangeable. The typing-sound row pulled them
/// apart: `SoundVoice::RainbowKittyV2` ("music box") is live and selectable by
/// config while `SoundVoice::ALL` — the picker AND v1's voice-agnostic sweep
/// list — deliberately withholds it until v2's migration completes. Read
/// through the options alone, that config reported as `auto`: Settings
/// contradicting the synth actually playing.
///
/// So resolve it the way the runtime does (`Config::trail_sound_voice` =
/// `SoundVoice::parse(..).unwrap_or_default()`) and let a recognized voice
/// answer with its own canonical name, whichever spelling was authored. `None`
/// for a genuinely unknown value, whose truthful answer is the row's default —
/// which for this row IS `auto`, `SoundVoice::default()`.
fn enum_offered_or_runtime(f: &EditField) -> Option<&'static str> {
    (f.key == prefs::EDIT_TRAIL_SOUND_STYLE)
        .then(|| prefs::trail_sound_style_canonical(enum_candidate(f)))
        .flatten()
}

/// The canonical current option spelling of an Enum row, for
/// [`SettingsState::display_value`] and [`demo_style`]. It resolves annotated defaults
/// and documented aliases before falling back for a genuinely unrecognized spelling.
///
/// THE FALLBACK MUST BE WHAT THE RUNTIME DRAWS, not `options[0]`: both consumers claim to
/// describe the live effect. An unrecognized `cursor_trail_style` resolves to
/// [`prefs::DEFAULT_CURSOR_TRAIL_STYLE`] and RENDERS (`app_config::resolve_trail_style`),
/// so `options[0]` — "phaser" — would name an effect the engine is not playing. Every
/// other Enum row keeps the defensive first-option fallback; that is what `cursor_style`
/// pins.
pub(crate) fn enum_current(f: &EditField) -> &'static str {
    let EditKind::Enum { options } = f.kind else {
        return "";
    };
    enum_recognized(f).unwrap_or_else(|| {
        if f.key == prefs::EDIT_CURSOR_TRAIL_STYLE {
            prefs::DEFAULT_CURSOR_TRAIL_STYLE
        } else if let Some(live) = enum_offered_or_runtime(f) {
            live
        } else {
            options.first().copied().unwrap_or("")
        }
    })
}

/// The canonical option an Enum row's configured value RESOLVES to (directly or via the
/// alias map), or `None` for a genuinely unrecognized spelling.
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

/// Theme-derived colour roles shared by the overlay painters and native Settings
/// chrome, rebuilt from the live [`Theme`] so they re-tint without a hardcoded palette.
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

/// The width of `s` at size `px` in the MONO (terminal) face — REAL advances from
/// the chrome font stack (the user's terminal face, DejaVu strictly as coverage
/// fallback), replacing the old hardcoded `0.6 em × chars` estimate that drifted
/// from every non-DejaVu face. Regular weight; measure bold runs via
/// [`crate::tray_raster::measure_text`] directly. UI-face chrome measures with
/// [`ui_text_width`] instead.
pub(crate) fn text_w(s: &str, px: f32) -> f32 {
    crate::tray_raster::measure_text(s, px, TextWeight::Regular)
}

/// `f32::clamp(lo, hi)` panics when `lo > hi`. Overlay geometry can invert bounds in
/// extreme windows — a tiny font drops the max widget below its floor; an ultra-narrow
/// grid drops the available width below the desired minimum. Pin to the achievable
/// (upper) bound in that case instead of panicking.
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

/// Total height the retired Settings card requested; `lib.rs` and the `cfg(test)`
/// overlay still size the card slot with it.
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

/// Device-pixel geometry of the in-grid overlay card slot, handed to every
/// [`crate::overlay::OverlayModel::tray`]. Native Settings does not use this
/// terminal-grid geometry.
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
// the preview card's interior pixels (the feedback-#2 regression fix).

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

/// The retired two-pane card's fixed region map, all cell units: a PURE function of
/// the window geometry (see [`pane_geom_cells`]).
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
    /// Whether the preview band is reserved at this geometry.
    pub(crate) fn preview_shown(&self) -> bool {
        self.preview.1 > self.preview.0
    }
}

/// The retired card's region map on bare cell counts. `lib.rs` still asks it whether the
/// preview band would show (`settings_demo_active`).
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

// ---- Landing page (design §L) -------------------------------------------------------

const BLOB_CORAL: [u8; 3] = [0xFF, 0x51, 0x48];
const BLOB_MARIGOLD: [u8; 3] = [0xFF, 0xC2, 0x2E];
const BLOB_LEAF: [u8; 3] = [0x2F, 0xAE, 0x5B];
const BLOB_COBALT: [u8; 3] = [0x2E, 0x5B, 0xFF];
const BLOB_VIOLET: [u8; 3] = [0xB0, 0x5C, 0xFF];

// ---- §L.5 The rainbow welcome ---------------------------------------------------
// The landing's welcome flourish: a pastel rainbow arch sweeping in behind the
// hero and a small twinkling constellation. Purely decorative.
/// The rainbow arch's sixth stripe (the blob palette covers the other five).
const RAINBOW_TEAL: [u8; 3] = [0x21, 0xC2, 0xB7];

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

// ---- Group-box layout (design §3.2) ------------------------------------------------

/// One laid-out row of a category's grouped layout: a group caption, a control, a
/// footnote under a box, or the gap between groups.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum GroupRow {
    Caption(&'static str),
    Control(usize),
    Footnote(&'static str),
    Gap,
}

/// The grouped layout of one category: its fields ordered by (`prefs::group_of` order,
/// build order), with a caption opening each group and its footnote + a gap closing it.
pub(crate) fn category_layout(fields: &[EditField], category: prefs::Section) -> Vec<GroupRow> {
    let mut idxs: Vec<usize> = (0..fields.len())
        .filter(|&i| prefs::section_of(fields[i].key) == category)
        .collect();
    idxs.sort_by_key(|&i| (prefs::group_of(fields[i].key).1, i));
    let mut out = Vec::with_capacity(idxs.len() + 8);
    let mut cur: Option<&'static str> = None;
    for &i in &idxs {
        let (caption, _) = prefs::group_of(fields[i].key);
        if cur != Some(caption) {
            if let Some(prev) = cur {
                if let Some(note) = prefs::group_footnote(prev) {
                    out.push(GroupRow::Footnote(note));
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
        out.push(GroupRow::Footnote(note));
    }
    out
}

/// The category's control field indices in LAID-OUT order.
pub(crate) fn category_controls(fields: &[EditField], category: prefs::Section) -> Vec<usize> {
    category_layout(fields, category)
        .into_iter()
        .filter_map(|r| match r {
            GroupRow::Control(i) => Some(i),
            _ => None,
        })
        .collect()
}

/// The style the retired preview card's DEMO lane played, or `None` when idle: the
/// EFFECTIVE trail style while the trail toggle / trail-effect row is the selection
/// inside the active category, and never while the search filter owns the pane.
/// `lib.rs` (`settings_demo_active`) still arms its `next_demo_tick` from it.
pub(crate) fn demo_style(state: &SettingsState) -> Option<&str> {
    if state.filtering() {
        return None; // the resting default mock has no focused subject
    }
    let f = state
        .fields
        .get(state.selected)
        .filter(|f| prefs::section_of(f.key) == state.category)?;
    match f.key {
        prefs::EDIT_CURSOR_TRAIL | prefs::EDIT_CURSOR_TRAIL_STYLE => state
            .fields
            .iter()
            .find(|g| g.key == prefs::EDIT_CURSOR_TRAIL_STYLE)
            .map(enum_current),
        _ => None,
    }
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

    /// A configured ENUM ALIAS (e.g. cursor_style "beam" == bar) must resolve to its
    /// canonical option, never silently substitute `options[0]` (which would contradict
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
        // "beam" is the documented alias for "bar" — resolve to bar, not block.
        assert_eq!(enum_current(&field("beam")), "bar");
        // The loader still accepts retired `underline` and renders it as a bar.
        assert_eq!(enum_current(&field("underline")), "bar");
        // A genuinely unknown spelling falls back to the first option.
        assert_eq!(enum_current(&field("zzz")), "block");

        // Trail-style aliases resolve through the shared prefs table: a
        // configured "rainbow" (or legacy "nyan"/"nyan rainbow") renders the
        // banded ribbon LIVE, so the row (and the demo lane riding on it) must
        // show its canonical name — previously every alias
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
        // default style and renders, so the displayed value and the demo lane
        // name the trail that is actually on screen.
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
            assert!(
                matches!(f.kind, EditKind::Enum { .. }),
                "alias key {key} must remain an enum"
            );
        }

        for &(alias, canonical) in crate::prefs::CURSOR_TRAIL_STYLE_ALIASES {
            let f = field(crate::prefs::EDIT_CURSOR_TRAIL_STYLE, alias);
            assert_eq!(SettingsState::display_value(&f), canonical, "{alias}");
            assert_eq!(enum_current(&f), canonical, "{alias}");
        }

        // The typing-sound row: every synth alias (water → droplet, mech →
        // mechanical, bell / glass bell → music box, …) reads back the voice the
        // synth actually plays — including an alias of a voice the roster
        // (`SoundVoice::ALL`) deliberately withholds, which must never collapse to
        // the roster's first entry (`auto`).
        for &(alias, voice) in aterm_effects::trail_sound::SoundVoice::ALIASES {
            let canonical = voice.name();
            let f = field(crate::prefs::EDIT_TRAIL_SOUND_STYLE, alias);
            assert_eq!(SettingsState::display_value(&f), canonical, "{alias}");
            assert_eq!(enum_current(&f), canonical, "{alias}");
        }
        let f = field(crate::prefs::EDIT_TRAIL_SOUND_STYLE, "water");
        assert_eq!(enum_current(&f), "droplet");
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
        assert_eq!(
            enum_current(&trail),
            "rainbow kitty",
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
    }

    /// Graft #1: `cursor_trail_style` is ONE "Cursor trail" row (Enum over
    /// [`CURSOR_TRAIL_STYLES`]), and selecting it or the trail master toggle inside
    /// the active category is what arms [`demo_style`] with the EFFECTIVE style.
    #[test]
    fn the_one_trail_style_row_demos_the_effective_style_when_selected() {
        let mut s = SettingsState::from_config(&cfg());
        let rows: Vec<usize> = (0..s.fields.len())
            .filter(|&i| s.fields[i].key == crate::prefs::EDIT_CURSOR_TRAIL_STYLE)
            .collect();
        assert_eq!(rows.len(), 1, "exactly one trail-style row");
        let idx = rows[0];
        assert_eq!(s.fields[idx].label, "Cursor trail");
        assert!(
            matches!(s.fields[idx].kind, EditKind::Enum { options } if options == CURSOR_TRAIL_STYLES),
            "the row offers the whole style list"
        );

        // Idle: no demo subject. Focusing the row demos the EFFECTIVE style.
        // The style row lives on the CAT's category since 2026-08-10, and only a
        // selection inside the live category counts.
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

        // While filtering there is no focused subject: no demo.
        s.searching = true;
        s.query = "trail".to_string();
        assert_eq!(demo_style(&s), None);
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

    /// A TYPO'D STYLE RESOLVES TO THE TRAIL THAT IS ACTUALLY DRAWN. The displayed
    /// value and [`demo_style`] have to agree with `app_config::resolve_trail_style`,
    /// which substitutes [`prefs::DEFAULT_CURSOR_TRAIL_STYLE`] and RENDERS.
    /// `options[0]` is "phaser", a wholly different look, and naming it would have
    /// made Settings contradict the glass.
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
        // The demo lane reads the same seam.
        s.set_category(prefs::Section::CursorKitty);
        s.selected = idx;
        assert_eq!(
            demo_style(&s),
            Some(crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE),
            "the demo lane must play the fallback, not options[0]"
        );
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
            category_layout(&s.fields, sec)
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
        // "Rainbow wake" went with its dial (retired 2026-09-16); of the two
        // left, "Companion" is the showcase card and "Kitty art" is Manual-only,
        // so this page paints no ordinary group box
        // (`native_settings::the_cursor_kitty_page_is_its_showcase_card`).
        assert_eq!(
            caps(prefs::Section::CursorKitty),
            ["Companion", "Kitty art"]
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
                "Presence",
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
        // "This Mac" (2026-09-14): the `[machine]` table's two editable keys —
        // `machine.universal_control` and `machine.spotlight_noindex` — group under
        // their own caption between Permissions and the network drive, which is
        // where `prefs::category_of` puts them. The caption list moved when those
        // keys became editable and this pin did not, so it was red on main.
        assert_eq!(
            caps(prefs::Section::Security),
            ["Permissions", "This Mac", "Network drive"]
        );
        // The Kitty Log page is READ-ONLY (§F4.6): no editable key ever maps
        // to it, so its grouped layout is empty.
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
            category_layout(&s.fields, prefs::Section::Appearance)
                .iter()
                .any(|r| matches!(r, GroupRow::Footnote(n) if n.contains("theme's color")))
        );
    }
}
