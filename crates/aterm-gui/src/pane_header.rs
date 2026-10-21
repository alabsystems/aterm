// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Subordinate, per-leaf chrome for a split tab. The canonical plan owns the
//! reserved row; this module only names and paints it. A flat rail and a small
//! branch/ordinal distinguish siblings from the parent tab's selectable cards.

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use aterm_core::grid::extra::{ImageData, ImageFormat, ImageRef, ImageScaling};
use aterm_core::terminal::RenderCell;
use aterm_grapheme::GraphemeClusters as _;
use aterm_render::RenderInput;

use crate::tab_model::{TabIconKind, TabIndicators, View, ViewId, VisibleLeafPlan};
use crate::type_scale::TypeStep;
use crate::widget::{DrawPrim, TextFace, TextWeight, rgba, text_prim};
use crate::{App, WindowId, chrome_band, term_try_lock, tray_raster};

#[derive(Default)]
pub(crate) struct HeaderCache {
    entries: Vec<Header>,
}

struct Header {
    view: ViewId,
    title: String,
    icon: String,
    native_icon: Option<TabIconKind>,
    need_mark: bool,
    row: usize,
    col: usize,
    key: u64,
    semantic: SemanticRow,
    image: Option<Arc<ImageData>>,
}

impl HeaderCache {
    pub(crate) fn paint(&self, input: &mut RenderInput) {
        if !self.entries.is_empty() {
            // This is host chrome written after extraction, so it cannot keep
            // an engine-only snapshot identity or a composed retention blessing.
            input.snapshot_seq = input.snapshot_seq.wrapping_add(1);
        }
        for header in &self.entries {
            let Some(row) = input.cells.get_mut(header.row) else {
                continue;
            };
            let end = (header.col + header.semantic.cells.len()).min(row.len());
            if end <= header.col {
                continue;
            }
            row[header.col..end].copy_from_slice(&header.semantic.cells[..end - header.col]);
            if let Some(clusters) = input.clusters.get_mut(header.row) {
                clusters.retain(|(col, _)| !(header.col..end).contains(col));
                clusters.extend(
                    header
                        .semantic
                        .clusters
                        .iter()
                        .filter(|(col, _)| header.col + col < end)
                        .map(|(col, cluster)| (header.col + col, cluster.clone())),
                );
                clusters.sort_unstable_by_key(|(col, _)| *col);
            }
            if let Some(combining) = input.combining.get_mut(header.row) {
                combining.retain(|(col, _)| !(header.col..end).contains(col));
                combining.extend(
                    header
                        .semantic
                        .combining
                        .iter()
                        .filter(|(col, _)| header.col + col < end)
                        .map(|(col, marks)| (header.col + col, marks.clone())),
                );
                combining.sort_unstable_by_key(|(col, _)| *col);
            }
            if let Some(images) = input.images.get_mut(header.row) {
                images.retain(|(col, _)| !(header.col..end).contains(col));
                if let Some(image) = &header.image {
                    images.extend((header.col..end).map(|col| {
                        (
                            col,
                            ImageRef {
                                image: Arc::clone(image),
                                cell_row: 0,
                                cell_col: (col - header.col) as u16,
                                kitty: None,
                            },
                        )
                    }));
                    // Stable: the pane's other cells keep their layer order.
                    images.sort_by_key(|(col, _)| *col);
                }
            }
        }
    }
}

impl App {
    /// Refreshes each split leaf's header and returns their fingerprint (0 for none). Reads are
    /// nonblocking: a contended PTY keeps the previous label. The fingerprint gates both repaint
    /// and the cached raster, so a title- or focus-only change repaints even under the native
    /// strip.
    pub(crate) fn refresh_pane_headers(&mut self, wid: WindowId, plan: &VisibleLeafPlan) -> u64 {
        let Some(ws) = self.windows.get_mut(&wid) else {
            return 0;
        };
        if plan.leaves.iter().all(|leaf| leaf.header.is_none()) {
            ws.pane_headers.entries.clear();
            return 0;
        }
        let mut cache = std::mem::take(&mut ws.pane_headers);
        // The window's own scale: the Fluent label is sized off the strip's
        // chrome base ([`header_label_px`]), not off the cell.
        let scale = ws.scale as f32;
        cache.entries.retain(|entry| {
            plan.leaves
                .iter()
                .any(|leaf| leaf.header.is_some() && leaf.view == entry.view)
        });
        let palette = self.chrome_palette(crate::app_render::ChromeSurface::Header);
        let theme = palette.theme;
        let colors = header_colors(palette);
        let (cw, ch) = self.win_cell_size(wid);
        let font_epoch = tray_raster::strip_band_font_epoch();
        let mut fingerprint = std::collections::hash_map::DefaultHasher::new();
        for (ordinal, leaf) in plan.leaves.iter().enumerate() {
            let Some(rect) = leaf.header else { continue };
            let entry = if let Some(index) = cache.entries.iter().position(|e| e.view == leaf.view)
            {
                &mut cache.entries[index]
            } else {
                cache.entries.push(Header {
                    view: leaf.view,
                    title: "Terminal".into(),
                    icon: String::new(),
                    native_icon: None,
                    need_mark: false,
                    row: 0,
                    col: 0,
                    key: 0,
                    semantic: SemanticRow::default(),
                    image: None,
                });
                cache.entries.last_mut().expect("just inserted header")
            };
            let mut indicators = TabIndicators::default();
            match self.view_store.get(leaf.view).copied() {
                Some(View::Terminal(view)) => {
                    indicators = self.session_status_indicators(view.session);
                    // Written into the resident strings, as `refill_strip_titles`
                    // does: the rungs of `resolved_terminal_title_rung`, then
                    // "Terminal", without a per-frame allocation.
                    if let Some(session) = self.pool.get(view.session)
                        && let Ok(meta) = session.ctx.meta.try_lock()
                    {
                        if !meta.presentation_value_into("icon", &mut entry.icon) {
                            entry.icon.clear();
                        }
                        // The header's mark is the session's published need
                        // (the presence slot, no lock): a wait the harness
                        // settles is no mark, as on the tab (owner surfaces §5).
                        entry.need_mark = self
                            .presence
                            .slot(view.session)
                            .is_some_and(|slot| slot.need.is_some());
                        let user_titled = meta.presentation_value_into("title", &mut entry.title);
                        drop(meta);
                        if !user_titled && let Some(term) = term_try_lock(&session.term) {
                            use crate::cwd_native::ReportedCwd as _;
                            let live = crate::tab_label::informative(term.title());
                            entry.title.clear();
                            if !live.is_empty() {
                                live.push_to(&mut entry.title);
                            } else if !crate::app_window::push_home_abbreviated_cwd(
                                &mut entry.title,
                                term.native_working_directory().as_deref(),
                            ) {
                                entry.title.push_str("Terminal");
                            }
                        }
                    }
                    indicators.attention |= entry.need_mark;
                }
                Some(View::Native(_)) => {
                    if let Some(presentation) = self.live_view_presentation(leaf.view) {
                        entry.title = presentation.title;
                        entry.native_icon = presentation.icon;
                        indicators = presentation.indicators;
                    }
                }
                None => continue,
            }
            entry.row = rect.origin.y.round().max(0.0) as usize;
            entry.col = rect.origin.x.round().max(0.0) as usize;
            let cols = rect.size.width.round().max(0.0) as usize;
            let mut key = std::collections::hash_map::DefaultHasher::new();
            (
                leaf.view,
                ordinal,
                leaf.focused,
                entry.row,
                entry.col,
                cols,
                cw,
                ch,
                scale.to_bits(),
                font_epoch,
            )
                .hash(&mut key);
            (
                &entry.title,
                &entry.icon,
                entry.native_icon,
                colors,
                indicators.dirty,
                indicators.busy,
                indicators.wants_attention(),
            )
                .hash(&mut key);
            let key = key.finish();
            if entry.key != key {
                entry.key = key;
                let label = label(ordinal + 1, &entry.icon, &entry.title);
                entry.semantic = semantic_row(&label, cols, leaf.focused, theme, indicators);
                entry.image = raster(
                    &label,
                    cols,
                    (cw, ch),
                    scale,
                    leaf.focused,
                    indicators,
                    colors,
                    entry.native_icon,
                );
            }
            key.hash(&mut fingerprint);
        }
        let fp = if cache.entries.is_empty() {
            0
        } else {
            fingerprint.finish()
        };
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.pane_headers = cache;
        }
        fp
    }

    pub(crate) fn paint_pane_headers(&mut self, wid: WindowId) {
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.pane_headers.paint(&mut ws.input_scratch);
        }
    }
}

/// The header row's fill for band tones `colors`: under the Fluent split
/// chrome (Windows) the pane's own ground, the terminal background the band
/// calls its well ([`chrome_band::BandColors`]'s `field_bg`); the band's tone
/// under the Classic chrome.
///
/// On the ground, a split tab is one surface from the strip's selected tab
/// down: the tab opens through the seam into the header row
/// ([`crate::tab_bar::StripFloor`]), the header's label, dot and accent rule
/// sit on the pane's ground over the quiet hairline that closes it against the
/// grid, and the divider's gap is that ground too, as Windows Terminal's panes
/// (which have no headers) meet their tab. On the band's tone the selected tab
/// opened into a grey band as an island of the ground, and in a real window
/// the band's foot was a dark stripe between the strip and the headers (the
/// 2026-10-18 judges' `selected-tab-opens-onto-grey-band`,
/// `selected-tab-opens-into-header-band`).
pub(crate) fn header_ground(colors: &chrome_band::BandColors) -> [u8; 3] {
    if crate::pane_chrome::split_chrome() == crate::pane_chrome::SplitChrome::Fluent {
        colors.field_bg
    } else {
        colors.bar_bg
    }
}

/// `colors` with its header inks held to their floors on `ground`
/// ([`header_ground`]): the words (`value`, `label`, `warn`) to AA, the dot and
/// the focus rule (`accent`) to the 3:1 of a graphical mark. The band floored
/// them on its own tone; on the ground most already clear (the band is the
/// ground stepped toward the ink), and this keeps every theme legible there.
fn inks_on(mut colors: chrome_band::BandColors, ground: [u8; 3]) -> chrome_band::BandColors {
    if ground == colors.bar_bg {
        return colors;
    }
    let floor = chrome_band::ensure_contrast;
    colors.value = floor(colors.value, ground, 4.5);
    colors.label = floor(colors.label, ground, 4.5);
    colors.warn = floor(colors.warn, ground, 4.5);
    colors.accent = floor(colors.accent, ground, 3.0);
    colors.bar_bg = ground;
    colors
}

/// The header's tones for palette `p`: the band's, on [`header_ground`] as
/// their `bar_bg`, inks floored there ([`inks_on`]).
pub(crate) fn header_colors(p: impl Into<chrome_band::ChromePalette>) -> chrome_band::BandColors {
    let band = chrome_band::band_colors(p);
    inks_on(band, header_ground(&band))
}

fn label(ordinal: usize, icon: &str, title: &str) -> String {
    // OSC/meta text is data: never allow a newline, tab or bidi override to
    // impersonate another pane. Limit before font shaping, even for huge OSCs.
    let clean = |s: &str, max: usize| -> String {
        let mut result = String::new();
        let mut remaining_chars = max * 8;
        for cluster in s.graphemes().take(max) {
            let count = cluster.chars().count();
            if count > remaining_chars {
                break;
            }
            remaining_chars -= count;
            result.extend(cluster.chars().filter(|c| {
                !c.is_control()
                    && !matches!(*c,
                '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
            }));
        }
        result
    };
    let icon = clean(icon, 8);
    let title = clean(title, 512);
    if icon.is_empty() {
        format!("{ordinal}  {title}")
    } else {
        format!("{ordinal}  {icon}  {title}")
    }
}

#[derive(Default)]
struct SemanticRow {
    cells: Vec<RenderCell>,
    clusters: Vec<(usize, Box<str>)>,
    combining: Vec<(usize, Box<[char]>)>,
}

fn semantic_row(
    label: &str,
    cols: usize,
    focused: bool,
    theme: aterm_render::Theme,
    indicators: TabIndicators,
) -> SemanticRow {
    let colors = header_colors(theme);
    let fluent = crate::pane_chrome::split_chrome() == crate::pane_chrome::SplitChrome::Fluent;
    // The band's blank cell on the header's own fill ([`header_ground`]).
    let mut cell = chrome_band::cell(' ', colors.label, colors.bar_bg, false, true);
    if fluent {
        // The strip's seam (or a divider's hairline) is the one rule above a
        // Fluent header; a second, top seam here would stack three rules in a
        // row and a half.
        cell.overline = false;
        cell.overline_color = None;
    }
    cell.fg = if focused { colors.value } else { colors.label };
    let mut row = SemanticRow {
        cells: vec![cell; cols],
        ..SemanticRow::default()
    };
    if let Some(marker) = row.cells.first_mut() {
        marker.ch = match (fluent, focused) {
            (true, true) => '•',
            (true, false) => ' ',
            (false, true) => '▸',
            (false, false) => '└',
        };
        marker.fg = if focused { colors.accent } else { colors.label };
    }
    let status = status_label(indicators);
    // Focus is the `▸` marker and the label's colour; the right side is status alone.
    let suffix = if cols >= 6 {
        status.to_string()
    } else {
        String::new()
    };
    let suffix_cols = suffix.chars().count();
    let label_cols = cols.saturating_sub(1 + suffix_cols + usize::from(suffix_cols > 0));
    let label = fit_cell_label(label, label_cols);
    let mut col = 1;
    // The grid's own walk over the label, so a cluster is painted in exactly
    // the cells `fit_cell_label` measured it in.
    let mut grid = aterm_grapheme::GridColumns::new();
    // The cell a joining character (a mark, a selector, a letter the grid
    // folds after a ZWJ) attaches to.
    let mut base: Option<usize> = None;
    for cluster in aterm_grapheme::split_graphemes(&label) {
        let mut after = grid.clone();
        let width = header_cells(&cluster, &mut after);
        if col + width > cols {
            break;
        }
        if is_glyph(&cluster) {
            grid = after;
            base = None;
            let Some(cell) = row.cells.get_mut(col).filter(|_| width > 0) else {
                continue;
            };
            cell.ch = cluster.text.chars().next().unwrap_or(' ');
            if cluster.codepoint_count > 1 {
                row.clusters.push((col, cluster.text.into()));
            }
            if width == 2 {
                mark_continuation(&mut row.cells, col);
            }
            col += width;
            continue;
        }
        for ch in cluster.text.chars() {
            let step = grid.step(ch);
            if let aterm_grapheme::GridStep::Opens { width, .. } = step {
                if let Some(cell) = row.cells.get_mut(col) {
                    cell.ch = ch;
                    base = Some(col);
                }
                if width == 2 {
                    mark_continuation(&mut row.cells, col);
                }
            } else if let Some(at) = base {
                if let Some((_, marks)) = row.combining.last_mut().filter(|(cell, _)| *cell == at) {
                    let mut extended = marks.to_vec();
                    extended.push(ch);
                    *marks = extended.into_boxed_slice();
                } else {
                    row.combining.push((at, vec![ch].into_boxed_slice()));
                }
                // VS16 widened the cell it joined.
                if step == (aterm_grapheme::GridStep::Joins { widened: true }) {
                    mark_continuation(&mut row.cells, at);
                }
            }
            col += step.advance();
        }
    }
    for (col, ch) in (cols.saturating_sub(suffix_cols)..cols).zip(suffix.chars()) {
        row.cells[col].ch = ch;
        row.cells[col].fg = if indicators.wants_attention() {
            colors.warn
        } else {
            colors.label
        };
    }
    row
}

/// Mark the column after the wide cell at `col` as its continuation.
fn mark_continuation(cells: &mut [RenderCell], col: usize) {
    if let Some(spacer) = cells.get_mut(col + 1) {
        spacer.wide = true;
    }
}

/// An emoji or keycap cluster, which the header paints as one glyph.
fn is_glyph(cluster: &aterm_grapheme::Grapheme<'_>) -> bool {
    cluster.is_emoji || cluster.text.contains('\u{20e3}')
}

/// The cells the header paints `cluster` in, feeding its characters to `grid`
/// (the walk over the label so far): a glyph takes its display width, and any
/// other cluster the cells the grid lays it out in, in context.
fn header_cells(
    cluster: &aterm_grapheme::Grapheme<'_>,
    grid: &mut aterm_grapheme::GridColumns,
) -> usize {
    let cells = grid.advance_str(cluster.text);
    if is_glyph(cluster) {
        cluster.width
    } else {
        cells
    }
}

fn fit_cell_label(label: &str, cols: usize) -> String {
    let mut grid = aterm_grapheme::GridColumns::new();
    let total: usize = aterm_grapheme::split_graphemes(label)
        .map(|cluster| header_cells(&cluster, &mut grid))
        .sum();
    if total <= cols {
        return label.into();
    }
    if cols == 0 {
        return String::new();
    }
    let mut grid = aterm_grapheme::GridColumns::new();
    let mut result = String::new();
    let mut available = cols - 1;
    for cluster in aterm_grapheme::split_graphemes(label) {
        let width = header_cells(&cluster, &mut grid);
        if width > available {
            break;
        }
        result.push_str(cluster.text);
        available -= width;
    }
    result.push('…');
    result
}

fn status_label(indicators: TabIndicators) -> &'static str {
    if indicators.wants_attention() {
        "!"
    } else if indicators.busy {
        "•"
    } else if indicators.dirty {
        "●"
    } else {
        ""
    }
}

fn fit_label(label: &str, width: f32, px: f32, face: TextFace) -> String {
    if tray_raster::ui_text_width_for(face, label, px) <= width {
        return label.into();
    }
    let ellipsis = "…";
    if tray_raster::ui_text_width_for(face, ellipsis, px) > width {
        return String::new();
    }
    let mut ends: Vec<_> = label.grapheme_indices().map(|(at, _)| at).collect();
    ends.push(label.len());
    let mut lo = 0;
    let mut hi = ends.len() - 1;
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let candidate = format!("{}…", &label[..ends[mid]]);
        if tray_raster::ui_text_width_for(face, &candidate, px) <= width {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    format!("{}…", &label[..ends[lo]])
}

/// The native app glyph uses the same 16-unit icon geometry as its parent tab.
fn draw_native_icon(
    prims: &mut Vec<DrawPrim>,
    kind: TabIconKind,
    x: f32,
    y: f32,
    side: f32,
    color: [u8; 3],
) {
    use crate::tab_bar::{TAB_ICON_DESIGN_SIZE, TabIconPrimitive, tab_icon_primitives};
    let k = side / TAB_ICON_DESIGN_SIZE;
    let color = rgba(color, 255);
    for primitive in tab_icon_primitives(kind) {
        match *primitive {
            TabIconPrimitive::Line { from, to, width } => prims.push(DrawPrim::Line {
                x1: x + from[0] * k,
                y1: y + from[1] * k,
                x2: x + to[0] * k,
                y2: y + to[1] * k,
                width: (width * k).max(1.0),
                color,
            }),
            TabIconPrimitive::RoundedRect {
                rect,
                radius,
                width,
            } => prims.push(DrawPrim::Stroke {
                x: x + rect[0] * k,
                y: y + rect[1] * k,
                w: rect[2] * k,
                h: rect[3] * k,
                radius: radius * k,
                width: (width * k).max(1.0),
                color,
            }),
            TabIconPrimitive::Dot { center, radius } => prims.push(DrawPrim::Dot {
                cx: x + center[0] * k,
                cy: y + center[1] * k,
                r: (radius * k).max(1.0),
                color,
            }),
            TabIconPrimitive::Triangle { points } => {
                for (a, b) in [(0, 1), (1, 2), (2, 0)] {
                    prims.push(DrawPrim::Line {
                        x1: x + points[a][0] * k,
                        y1: y + points[a][1] * k,
                        x2: x + points[b][0] * k,
                        y2: y + points[b][1] * k,
                        width: k.max(1.0),
                        color,
                    });
                }
            }
        }
    }
}

/// The header label's size in a `row_h`-px header row at the window's `scale`.
///
/// Fluent (Windows): the Secondary step on the tab strip's own chrome base
/// ([`crate::tab_bar::chrome_label_base_px`], 13 logical px), so the pane's
/// header reads one step below the tab title above it in the same face. It
/// was sized off the cell (`Secondary` of 0.8 × the row, 16.6 px in the 23 px
/// Windows row against the tabs' 13 px: the 2026-10-18 judges'
/// `pane-header-type-larger-than-tab`). Classic keeps the cell-sized label.
fn header_label_px(row_h: f32, scale: f32, fluent: bool) -> crate::type_scale::StepPx {
    if fluent {
        TypeStep::Secondary.px(crate::tab_bar::chrome_label_base_px(row_h, scale))
    } else {
        TypeStep::Secondary.px(row_h * 0.80)
    }
}

/// The focused Fluent header's accent rule in a header row `h` device pixels
/// tall: its first pixel row and its thickness, whole pixels at the row's foot
/// (two at 100%, three at 150%, four at 200%: twice the row's 24 px unit,
/// rounded). The focus ring's side lines start on its top row and turn into it
/// across the divider gap (`crate::pane_chrome::focus_ring_rects`), so the
/// rule and the ring meet in an L; the rule was an antialiased line the ring
/// passed three pixels short of (the 2026-10-18 judge's
/// `focus-ring-header-corner-notch`).
#[must_use]
pub(crate) fn focus_rule_rows(h: u32) -> (u32, u32) {
    let unit = (h as f32 / 24.0).max(0.5);
    let thick = ((2.0 * unit).round() as u32).clamp(1, h.max(1));
    (h.saturating_sub(thick), thick)
}

/// The quiet hairline under a header, against its pane's grid: the label ink
/// at this opacity over the header's fill.
const HEADER_FOOT_ALPHA: u8 = 45;

/// The least contrast a Fluent header's foot holds against the ground it and
/// the grid below share ([`header_ground`]). The label at
/// [`HEADER_FOOT_ALPHA`] is 2.04:1 on Default's ground but 1.17:1 on Default
/// Light's (`#e5e6e8` on `#f6f7f9`), where an unfocused header nearly merged
/// into its grid (the 2026-10-18 judges' `j3d-light-header-foot-faint`).
const HEADER_FOOT_FLOOR: f64 = 1.5;

/// The foot's colour ([`HEADER_FOOT_ALPHA`]); under the Fluent chrome, where
/// the header wears its pane's ground and the foot is all that closes it
/// against the grid, held at [`HEADER_FOOT_FLOOR`] there: the label ink laid
/// at the least opacity that clears it, as an opaque tone. A foot that clears
/// the floor (every dark theme) is drawn as before, and the Classic header,
/// whose band fill already parts it from the grid, too. The focused header's
/// accent underline covers the foot either way.
fn header_foot(colors: &chrome_band::BandColors, fluent: bool) -> crate::widget::Rgba {
    let quiet = rgba(colors.label, HEADER_FOOT_ALPHA);
    // The label ink at `alpha` over the fill as the pen lays a hairline:
    // linear-light source-over.
    let laid = |alpha: u8| -> [u8; 3] {
        let a = f32::from(alpha) / 255.0;
        std::array::from_fn(|k| {
            let ink = aterm_render::srgb_to_linear(f32::from(colors.label[k]) / 255.0);
            let ground = aterm_render::srgb_to_linear(f32::from(colors.bar_bg[k]) / 255.0);
            (aterm_render::linear_to_srgb(ink.mul_add(a, ground * (1.0 - a))) * 255.0)
                .round()
                .clamp(0.0, 255.0) as u8
        })
    };
    let clears = |c: [u8; 3]| chrome_band::contrast(c, colors.bar_bg) >= HEADER_FOOT_FLOOR;
    if !fluent || clears(laid(HEADER_FOOT_ALPHA)) {
        return quiet;
    }
    let floored = (HEADER_FOOT_ALPHA..=u8::MAX)
        .map(laid)
        .find(|&c| clears(c))
        .unwrap_or_else(|| {
            chrome_band::ensure_contrast(laid(u8::MAX), colors.bar_bg, HEADER_FOOT_FLOOR)
        });
    rgba(floored, 255)
}

#[allow(
    clippy::too_many_arguments,
    reason = "one header's whole description: label, geometry, scale and state"
)]
fn raster(
    label: &str,
    cols: usize,
    cell: (usize, usize),
    window_scale: f32,
    focused: bool,
    indicators: TabIndicators,
    colors: chrome_band::BandColors,
    native_icon: Option<TabIconKind>,
) -> Option<Arc<ImageData>> {
    // The proportional pen cannot paint every glyph the terminal cascade can.
    // Keep its semantic cells visible for CJK/emoji rather than covering real
    // content with a raster containing missing-glyph holes.
    if !tray_raster::strip_band_run_coverable(label) {
        return None;
    }
    let (cw, ch) = cell;
    let width = u16::try_from(cols.checked_mul(cw)?).ok()?;
    let height = u16::try_from(ch).ok()?;
    if width == 0 || height == 0 {
        return None;
    }
    let (w, h) = (f32::from(width), f32::from(height));
    let scale = (h / 24.0).max(0.5);
    let fluent = crate::pane_chrome::split_chrome() == crate::pane_chrome::SplitChrome::Fluent;
    let size = header_label_px(h, window_scale, fluent);
    let face = if focused {
        TextFace::UiBold
    } else {
        TextFace::Ui
    };
    let fg = if focused { colors.value } else { colors.label };
    let accent = if focused { colors.accent } else { colors.label };
    let y = tray_raster::row_baseline(0.0, h - scale, size.get());
    // Shared straight band, with a quiet seam against the content.
    let mut prims = vec![DrawPrim::Line {
        x1: 0.0,
        y1: h - 0.5,
        x2: w,
        y2: h - 0.5,
        width: 1.0,
        color: header_foot(&colors, fluent),
    }];
    if fluent {
        // A dot, not a tree branch: the accent on the focused pane (whose
        // side of each divider carries the accent ring), a quiet one beside it.
        prims.push(DrawPrim::Dot {
            cx: 12.0 * scale,
            cy: h * 0.5,
            r: if focused { 3.0 } else { 2.5 } * scale,
            color: rgba(accent, if focused { 255 } else { 120 }),
        });
        if focused {
            // The selected header's accent underline (a Fluent selected tab's):
            // the header says at a glance which pane has the keyboard. On whole
            // pixel rows from the pane's first column, where the focus ring's
            // side line turns into it ([`focus_rule_rows`]).
            let (top, thick) = focus_rule_rows(u32::from(height));
            let mid = top as f32 + thick as f32 / 2.0;
            prims.push(DrawPrim::Line {
                x1: 0.0,
                y1: mid,
                x2: w,
                y2: mid,
                width: thick as f32,
                color: rgba(accent, 255),
            });
        }
    } else {
        // A small branch; only the focused child gets a short accent rail; no
        // top-level tab shape.
        prims.extend([
            DrawPrim::Line {
                x1: 10.0 * scale,
                y1: h * 0.30,
                x2: 10.0 * scale,
                y2: h * 0.57,
                width: scale,
                color: rgba(accent, 255),
            },
            DrawPrim::Line {
                x1: 10.0 * scale,
                y1: h * 0.57,
                x2: 16.0 * scale,
                y2: h * 0.57,
                width: scale,
                color: rgba(accent, 255),
            },
        ]);
        if focused {
            prims.push(DrawPrim::Panel {
                x: 0.0,
                y: 3.0 * scale,
                w: 2.0 * scale,
                h: (h - 6.0 * scale).max(1.0),
                radius: scale,
                fill: rgba(accent, 255),
            });
        }
    }
    let status = status_label(indicators);
    // Focus is the accent rail and the bold label; the right side is status alone.
    let suffix = status.to_string();
    let suffix_size = if fluent {
        size
    } else {
        TypeStep::Caption.px(h * 0.70)
    };
    let suffix_width = tray_raster::ui_text_width_for(TextFace::Ui, &suffix, suffix_size.get());
    let inset = 8.0 * scale;
    let mut start = 22.0 * scale;
    if let Some(icon) = native_icon {
        draw_native_icon(
            &mut prims,
            icon,
            start,
            (h - 14.0 * scale) * 0.5,
            14.0 * scale,
            fg,
        );
        start += 20.0 * scale;
    }
    let end =
        (w - inset - suffix_width - if suffix.is_empty() { 0.0 } else { 12.0 * scale }).max(start);
    prims.push(text_prim(
        start,
        y,
        fit_label(label, end - start, size.get(), face),
        size,
        TextWeight::Regular,
        face,
        rgba(fg, 255),
    ));
    if !suffix.is_empty() && w - suffix_width > start {
        prims.push(text_prim(
            w - inset - suffix_width,
            y,
            suffix,
            suffix_size,
            TextWeight::Regular,
            TextFace::Ui,
            rgba(
                if indicators.wants_attention() {
                    colors.warn
                } else {
                    accent
                },
                255,
            ),
        ));
    }
    let bytes = tray_raster::rasterize_tray_pixels(
        &prims,
        u32::from(width),
        u32::from(height),
        1.0,
        rgba(colors.bar_bg, 255),
    );
    Some(Arc::new(ImageData {
        bytes,
        format: ImageFormat::RawRgba8 { width, height },
        cols: cols as u16,
        rows: 1,
        z_index: 0,
        band_lift_px: 0,
        cell_offset_px: (0, 0),
        scaling: ImageScaling::Fit,
        source_rect: None,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_remove_control_sequences_without_splitting_graphemes() {
        assert_eq!(label(2, "🧑🏽‍💻\n", "a\tb\u{202e}c\u{2069}"), "2  🧑🏽‍💻  abc");
        let family = "👨‍👩‍👧‍👦";
        let icon = family.repeat(9);
        assert_eq!(
            label(1, &icon, "shell"),
            format!("1  {}  shell", family.repeat(8))
        );
        let title = format!("{}e\u{301}", "a".repeat(511));
        assert!(label(1, "", &title).ends_with("e\u{301}"));
        assert_eq!(label(1, "", &format!("e{}", "\u{301}".repeat(5000))), "1  ");
    }

    #[test]
    fn fitted_labels_only_cut_at_grapheme_boundaries() {
        let text = "Cafe\u{301} 👩🏽‍💻 界 long title";
        let mut boundaries: Vec<_> = text.grapheme_indices().map(|(at, _)| at).collect();
        boundaries.push(text.len());
        let px = 13.0;
        for width in [0.0, 8.0, 16.0, 30.0, 45.0, 60.0, 80.0, 120.0, 500.0] {
            let fitted = fit_label(text, width, px, TextFace::Ui);
            assert!(tray_raster::ui_text_width_for(TextFace::Ui, &fitted, px) <= width);
            let prefix = fitted.strip_suffix('…').unwrap_or(&fitted);
            assert!(text.starts_with(prefix));
            assert!(
                boundaries.contains(&prefix.len()),
                "split grapheme: {fitted:?}"
            );
        }
    }

    /// [`semantic_row`] under the Classic split chrome, whose markers are the
    /// `▸`/`└` branch glyphs.
    fn classic_row(
        label: &str,
        cols: usize,
        focused: bool,
        theme: aterm_render::Theme,
        indicators: TabIndicators,
    ) -> SemanticRow {
        crate::pane_chrome::with_split_chrome(crate::pane_chrome::SplitChrome::Classic, || {
            semantic_row(label, cols, focused, theme, indicators)
        })
    }

    /// The Fluent header (Windows) has no top seam of its own (the strip's rule
    /// or a divider's hairline is above it) and marks the focused pane with a
    /// dot, the other with nothing. NEGATIVE CONTROL: the Classic header keeps
    /// its seam and its branch glyphs.
    #[test]
    fn a_fluent_header_has_no_top_seam_and_dots_the_focused_pane() {
        let theme = aterm_render::Theme::default();
        let indicators = TabIndicators::default();
        let fluent = |focused| {
            crate::pane_chrome::with_split_chrome(crate::pane_chrome::SplitChrome::Fluent, || {
                semantic_row("1  shell", 12, focused, theme, indicators)
            })
        };
        let (focused, other) = (fluent(true), fluent(false));
        assert!(
            focused
                .cells
                .iter()
                .chain(&other.cells)
                .all(|c| !c.overline)
        );
        assert_eq!(focused.cells[0].ch, '•');
        assert_eq!(other.cells[0].ch, ' ');
        let classic = classic_row("1  shell", 12, true, theme, indicators);
        assert!(classic.cells.iter().all(|c| c.overline), "CONTROL");
        assert_eq!(classic.cells[0].ch, '▸', "CONTROL");
    }

    /// THE FLUENT HEADER WEARS ITS PANE'S GROUND ([`header_ground`]): its cells and
    /// its raster are the terminal's background, so the strip's selected tab opens
    /// into it, and on every bundled scheme its words keep AA and its dot and rule
    /// 3:1 there. NEGATIVE CONTROL: the Classic header keeps the band's tone, which
    /// the Fluent header wore until 2026-10-18 under a ground-coloured selected
    /// tab (the judges' `selected-tab-opens-onto-grey-band`).
    #[test]
    fn a_fluent_header_wears_its_panes_ground() {
        use crate::pane_chrome::{SplitChrome, with_split_chrome};
        for name in aterm_types::scheme::builtin_names() {
            let theme =
                aterm_render::Theme::from(&aterm_types::scheme::builtin(name).expect("builtin"));
            let ground = crate::settings::u32_rgb(theme.bg);
            let band = chrome_band::band_colors(theme);
            let fluent = with_split_chrome(SplitChrome::Fluent, || header_colors(theme));
            assert_eq!(fluent.bar_bg, ground, "{name}: the Fluent header's fill");
            let row = with_split_chrome(SplitChrome::Fluent, || {
                semantic_row("1  shell", 12, true, theme, TabIndicators::default())
            });
            assert!(
                row.cells.iter().all(|c| c.bg == ground),
                "{name}: its cells"
            );
            for (what, ink, floor) in [
                ("value", fluent.value, 4.5),
                ("label", fluent.label, 4.5),
                ("warn", fluent.warn, 4.5),
                ("accent", fluent.accent, 3.0),
            ] {
                let ratio = chrome_band::contrast(ink, ground);
                assert!(
                    ratio >= floor - 0.01,
                    "{name}: the {what} ink {ink:?} is {ratio:.2}:1 on the ground"
                );
            }
            let classic = with_split_chrome(SplitChrome::Classic, || header_colors(theme));
            assert_eq!(classic.bar_bg, band.bar_bg, "{name}: CONTROL");
        }
        if !tray_raster::strip_band_ui_ready() {
            tray_raster::prepare_ui_fonts_for_direct_view_test();
        }
        if tray_raster::strip_band_ui_ready() {
            let theme = aterm_render::Theme::default();
            let colors = with_split_chrome(SplitChrome::Fluent, || header_colors(theme));
            let image = with_split_chrome(SplitChrome::Fluent, || {
                raster(
                    "1  shell",
                    20,
                    (9, 23),
                    1.0,
                    false,
                    TabIndicators::default(),
                    colors,
                    None,
                )
            })
            .expect("the header rasters");
            // The far end of the row, clear of the label and over its hairline.
            let width = 20 * 9;
            let at = (2 * width + width - 2) * 4;
            assert_eq!(
                &image.bytes[at..at + 3],
                crate::settings::u32_rgb(theme.bg).as_slice(),
                "the raster's fill is the ground"
            );
            tray_raster::clear_ui_fonts_for_test();
        }
    }

    /// AN UNFOCUSED FLUENT HEADER IS CLOSED AGAINST ITS GRID: on every bundled
    /// scheme its foot, the hairline under it, holds [`HEADER_FOOT_FLOOR`]
    /// against the ground the header and the grid share, in the raster too; a
    /// foot that already cleared it (Default's 2.04:1) is the same quiet line
    /// as before, and the Classic header keeps its line. NEGATIVE CONTROL: the
    /// label at [`HEADER_FOOT_ALPHA`] alone is under the floor on Default Light
    /// (1.17:1, the judges' `j3d-light-header-foot-faint`).
    #[test]
    fn an_unfocused_fluent_header_keeps_its_foot_off_the_grid() {
        use crate::pane_chrome::{SplitChrome, with_split_chrome};
        let opaque = |c: crate::widget::Rgba| [c[0], c[1], c[2]];
        for name in aterm_types::scheme::builtin_names() {
            let theme =
                aterm_render::Theme::from(&aterm_types::scheme::builtin(name).expect("builtin"));
            let colors = with_split_chrome(SplitChrome::Fluent, || header_colors(theme));
            let foot = header_foot(&colors, true);
            let quiet = rgba(colors.label, HEADER_FOOT_ALPHA);
            if foot != quiet {
                assert_eq!(foot[3], 255, "{name}: a floored foot is opaque");
                let ratio = chrome_band::contrast(opaque(foot), colors.bar_bg);
                assert!(
                    (HEADER_FOOT_FLOOR - 0.01..HEADER_FOOT_FLOOR + 0.1).contains(&ratio),
                    "{name}: the foot is {ratio:.2}:1 on the ground, not just over the floor"
                );
            }
            assert_eq!(header_foot(&colors, false), quiet, "{name}: Classic");
        }
        let dark = with_split_chrome(SplitChrome::Fluent, || {
            header_colors(aterm_render::Theme::default())
        });
        assert_eq!(
            header_foot(&dark, true),
            rgba(dark.label, HEADER_FOOT_ALPHA),
            "Default's foot clears the floor and is drawn as before"
        );
        let light_theme = aterm_render::Theme::from(
            &aterm_types::scheme::builtin("Default Light").expect("builtin"),
        );
        let light = with_split_chrome(SplitChrome::Fluent, || header_colors(light_theme));
        assert_ne!(
            header_foot(&light, true),
            rgba(light.label, HEADER_FOOT_ALPHA),
            "CONTROL: Default Light's quiet foot is under the floor"
        );
        if !tray_raster::strip_band_ui_ready() {
            tray_raster::prepare_ui_fonts_for_direct_view_test();
        }
        if tray_raster::strip_band_ui_ready() {
            let (cols, cw, ch) = (20usize, 9usize, 23usize);
            let image = with_split_chrome(SplitChrome::Fluent, || {
                raster(
                    "2  shell",
                    cols,
                    (cw, ch),
                    1.0,
                    false,
                    TabIndicators::default(),
                    light,
                    None,
                )
            })
            .expect("the header rasters");
            let width = cols * cw;
            let at = ((ch - 1) * width + width - 2) * 4;
            let foot: [u8; 3] = [image.bytes[at], image.bytes[at + 1], image.bytes[at + 2]];
            let ratio = chrome_band::contrast(foot, light.bar_bg);
            assert!(
                ratio >= HEADER_FOOT_FLOOR - 0.02,
                "the raster's foot {foot:?} is {ratio:.2}:1 on {:?}",
                light.bar_bg
            );
            tray_raster::clear_ui_fonts_for_test();
        }
    }

    /// THE FOCUSED FLUENT HEADER'S RULE LIES ON WHOLE PIXEL ROWS from the
    /// pane's first pixel column to its last ([`focus_rule_rows`]: two rows at
    /// 100%, three at 150%, four at 200%, at the row's foot), the rows the focus
    /// ring's side line turns into (`crate::pane_chrome::focus_ring_rects`); the
    /// row above it is not the accent. NEGATIVE CONTROL: the rule it replaced,
    /// `2 × (h/24)` thick centred `h/24` above the foot, began 0.08 px into its
    /// top row at 100%, so that row was antialiased, not the ring's.
    #[test]
    fn the_focused_fluent_rule_lies_on_whole_rows_from_the_panes_edge() {
        use crate::pane_chrome::{SplitChrome, with_split_chrome};
        assert_eq!(focus_rule_rows(23), (21, 2));
        assert_eq!(focus_rule_rows(35), (32, 3));
        assert_eq!(focus_rule_rows(46), (42, 4));
        let old_top = 23.0f32 - 2.0 * (23.0 / 24.0);
        assert!(
            old_top.fract() > 0.0,
            "CONTROL: the old rule's top row was partial"
        );
        if !tray_raster::strip_band_ui_ready() {
            tray_raster::prepare_ui_fonts_for_direct_view_test();
        }
        if !tray_raster::strip_band_ui_ready() {
            aterm_spec::host::could_not_run("no system UI face here for the strip band");
        }
        let colors = with_split_chrome(SplitChrome::Fluent, || {
            header_colors(aterm_render::Theme::default())
        });
        for (cw, ch, scale) in [(9usize, 23usize, 1.0f32), (18, 46, 2.0)] {
            let cols = 20;
            let image = with_split_chrome(SplitChrome::Fluent, || {
                raster(
                    "2  shell",
                    cols,
                    (cw, ch),
                    scale,
                    true,
                    TabIndicators::default(),
                    colors,
                    None,
                )
            })
            .expect("the header rasters");
            let width = cols * cw;
            let px = |x: usize, y: usize| {
                let at = (y * width + x) * 4;
                [image.bytes[at], image.bytes[at + 1], image.bytes[at + 2]]
            };
            let near = |a: [u8; 3], b: [u8; 3]| a.iter().zip(b).all(|(p, q)| p.abs_diff(q) <= 1);
            let (top, thick) = focus_rule_rows(ch as u32);
            let (top, thick) = (top as usize, thick as usize);
            for x in [0, 1, width / 2, width - 1] {
                for y in top..top + thick {
                    assert!(
                        near(px(x, y), colors.accent),
                        "{scale}x: ({x}, {y}) is {:?}, not the accent {:?}",
                        px(x, y),
                        colors.accent
                    );
                }
                assert!(
                    !near(px(x, top - 1), colors.accent),
                    "{scale}x: the row above the rule at x {x}"
                );
            }
        }
        tray_raster::clear_ui_fonts_for_test();
    }

    /// THE HEADER READS BELOW THE TAB: under the Fluent chrome the pane
    /// header's label is the Secondary step of the strip's own chrome base, at
    /// every scale smaller than the tab title above it, in the same face.
    /// NEGATIVE CONTROL: the cell-sized Classic label, which the Fluent header
    /// used until 2026-10-18, is 16.6 px in the 23 px Windows row against the
    /// strip's 13 px.
    #[test]
    fn the_fluent_header_label_sits_a_step_below_the_tab_title() {
        for (row_h, scale) in [(23.0f32, 1.0f32), (29.0, 1.25), (35.0, 1.5), (46.0, 2.0)] {
            let tab = crate::tab_bar::chrome_label_base_px(row_h + 9.0 * scale, scale);
            let header = header_label_px(row_h, scale, true).get();
            assert!(
                header < tab && header >= tab * 0.85,
                "{scale}x: header {header} px against the tab's {tab} px"
            );
        }
        assert!((header_label_px(23.0, 1.0, true).get() - 11.7).abs() < 0.01);
        let classic = header_label_px(23.0, 1.0, false).get();
        assert!(
            classic > crate::tab_bar::chrome_label_base_px(32.0, 1.0),
            "CONTROL: the cell-sized label outweighs the tab: {classic}"
        );
    }

    #[test]
    fn semantic_rows_preserve_clusters_and_mark_only_wide_continuations() {
        let row = classic_row(
            "e\u{301}👩‍💻界Z",
            7,
            true,
            aterm_render::Theme::default(),
            TabIndicators::default(),
        );
        assert_eq!(
            row.cells.iter().map(|c| c.ch).collect::<String>(),
            "▸e👩 界 Z"
        );
        assert_eq!(row.combining, vec![(1, vec!['\u{301}'].into_boxed_slice())]);
        assert_eq!(row.clusters, vec![(2, Box::<str>::from("👩‍💻"))]);
        for (col, cell) in row.cells.iter().enumerate() {
            assert_eq!(cell.wide, matches!(col, 3 | 5));
        }
        let clipped = classic_row(
            "e\u{301}👩‍💻",
            3,
            false,
            aterm_render::Theme::default(),
            TabIndicators::default(),
        );
        assert_eq!(
            clipped.cells.iter().map(|c| c.ch).collect::<String>(),
            "└e…"
        );
        assert!(
            clipped.clusters.is_empty(),
            "a wide cluster never straddles the pane edge"
        );
    }

    /// A title paints in exactly the cells it was measured in: Devanagari half forms the
    /// grid folds after a ZWJ stay in the row, and Sinhala's second cluster joins the first's cell.
    #[test]
    fn header_rows_paint_titles_in_the_cells_they_measure() {
        let theme = aterm_render::Theme::default();
        let indicators = TabIndicators::default();
        for title in [
            "\u{915}\u{94d}\u{200d}\u{937}",
            "\u{d9a}\u{dca}\u{200d}\u{dba}",
            "\u{1f44d}\u{200d}\u{200d}\u{1f44d}",
            "\u{a9}\u{fe0f}",
            "\u{928}\u{94d}\u{926}\u{940}",
        ] {
            for n in [1, 3, 8, 40] {
                let text = label(1, "", &title.repeat(n));
                for cols in 2..60 {
                    let row = semantic_row(&text, cols, cols % 2 == 0, theme, indicators);
                    assert_eq!(row.cells.len(), cols);
                    assert!(
                        row.clusters.iter().all(|(at, _)| *at < cols)
                            && row.combining.iter().all(|(at, _)| *at < cols),
                        "{title:?} x{n} in {cols} columns"
                    );
                }
            }
        }
        let row = classic_row("\u{d9a}\u{dca}\u{200d}\u{dba}Z", 4, true, theme, indicators);
        assert_eq!(
            row.cells.iter().map(|c| c.ch).collect::<String>(),
            "\u{25b8}\u{d9a}Z "
        );
        assert_eq!(
            row.combining,
            vec![(1, vec!['\u{dca}', '\u{200d}', '\u{dba}'].into_boxed_slice())]
        );
    }

    #[test]
    fn header_cache_reuses_pixels_and_refreshes_title_focus_and_native_icon() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.split_active_stub_tab(wid);
        let mut plan = app.active_visible_leaf_plan(wid).unwrap();
        let first = app.refresh_pane_headers(wid, &plan);
        let pixels = app.windows[&wid].pane_headers.entries[0].image.clone();
        let cells = app.windows[&wid].pane_headers.entries[0]
            .semantic
            .cells
            .as_ptr();
        assert_eq!(app.refresh_pane_headers(wid, &plan), first);
        assert_eq!(
            app.windows[&wid].pane_headers.entries[0]
                .semantic
                .cells
                .as_ptr(),
            cells
        );
        assert_eq!(
            pixels.as_ref().map(Arc::as_ptr),
            app.windows[&wid].pane_headers.entries[0]
                .image
                .as_ref()
                .map(Arc::as_ptr)
        );
        let session = app
            .view_store
            .get(plan.leaves[0].view)
            .copied()
            .unwrap()
            .terminal_session()
            .unwrap();
        crate::term_lock(&app.pool.get(session).unwrap().term).process(b"\x1b]2;renamed pane\x07");
        let renamed = app.refresh_pane_headers(wid, &plan);
        assert_ne!(renamed, first);
        assert_eq!(
            app.windows[&wid].pane_headers.entries[0].title,
            "renamed pane"
        );
        plan.focused = plan.leaves[0].view;
        for leaf in &mut plan.leaves {
            leaf.focused = leaf.view == plan.focused;
        }
        let focused = app.refresh_pane_headers(wid, &plan);
        assert_ne!(focused, renamed);
        assert_eq!(
            app.pool
                .get(session)
                .unwrap()
                .ctx
                .meta
                .lock()
                .unwrap()
                .set("attention", Some("Needs review".into())),
            Some(true)
        );
        // The `meta set` wake publishes the need the header marks.
        app.refresh_presence_session(session);
        assert_ne!(app.refresh_pane_headers(wid, &plan), focused);
        assert!(
            app.windows[&wid].pane_headers.entries[0]
                .semantic
                .cells
                .iter()
                .any(|cell| cell.ch == '!')
        );

        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
        let plan = app.active_visible_leaf_plan(wid).unwrap();
        app.refresh_pane_headers(wid, &plan);
        assert_eq!(
            app.windows[&wid].pane_headers.entries[0].native_icon,
            Some(TabIconKind::Settings)
        );
        app.windows
            .get_mut(&wid)
            .unwrap()
            .tab_set
            .active_mut()
            .unwrap()
            .zoomed = true;
        let plan = app.active_visible_leaf_plan(wid).unwrap();
        assert_eq!(app.refresh_pane_headers(wid, &plan), 0);
        assert!(app.windows[&wid].pane_headers.entries.is_empty());
    }
}
