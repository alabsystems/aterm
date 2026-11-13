// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Subordinate, per-leaf chrome for a split tab.

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
                    images.sort_by_key(|(col, _)| *col);
                }
            }
        }
    }
}

impl App {
    /// Refreshes each split leaf's header and returns their fingerprint (0 for none).
    pub(crate) fn refresh_pane_headers(&mut self, wid: WindowId, plan: &VisibleLeafPlan) -> u64 {
        let Some(ws) = self.windows.get_mut(&wid) else {
            return 0;
        };
        if plan.leaves.iter().all(|leaf| leaf.header.is_none()) {
            ws.pane_headers.entries.clear();
            return 0;
        }
        let mut cache = std::mem::take(&mut ws.pane_headers);
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
                    if let Some(session) = self.pool.get(view.session)
                        && let Ok(meta) = session.ctx.meta.try_lock()
                    {
                        if !meta.presentation_value_into("icon", &mut entry.icon) {
                            entry.icon.clear();
                        }
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

/// The header row's fill for band tones `colors`: under the Fluent split chrome (Windows) the pane's own ground, the terminal background the band calls its well ([`chrome_band::BandColors`]'s `field_bg`); the band's tone under the Classic chrome.
pub(crate) fn header_ground(colors: &chrome_band::BandColors) -> [u8; 3] {
    if crate::pane_chrome::split_chrome() == crate::pane_chrome::SplitChrome::Fluent {
        colors.field_bg
    } else {
        colors.bar_bg
    }
}

/// `colors` with its header inks held to their floors on `ground`
/// ([`header_ground`]): the words (`value`, `label`, `warn`) to AA, the dot and
/// the focus rule (`accent`) to the 3:1 of a graphical mark.
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

/// The header's tones for palette `p`: the band's, on [`header_ground`] as their `bar_bg`, inks floored there ([`inks_on`]).
pub(crate) fn header_colors(p: impl Into<chrome_band::ChromePalette>) -> chrome_band::BandColors {
    let band = chrome_band::band_colors(p);
    inks_on(band, header_ground(&band))
}

fn label(ordinal: usize, icon: &str, title: &str) -> String {
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
    let mut cell = chrome_band::cell(' ', colors.label, colors.bar_bg, false, true);
    if fluent {
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
    let suffix = if cols >= 6 {
        status.to_string()
    } else {
        String::new()
    };
    let suffix_cols = suffix.chars().count();
    let label_cols = cols.saturating_sub(1 + suffix_cols + usize::from(suffix_cols > 0));
    let label = fit_cell_label(label, label_cols);
    let mut col = 1;
    let mut grid = aterm_grapheme::GridColumns::new();
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

const fn status_label(indicators: TabIndicators) -> &'static str {
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
fn header_label_px(row_h: f32, scale: f32, fluent: bool) -> crate::type_scale::StepPx {
    if fluent {
        TypeStep::Secondary.px(crate::tab_bar::chrome_label_base_px(row_h, scale))
    } else {
        TypeStep::Secondary.px(row_h * 0.80)
    }
}

/// The focused Fluent header's accent rule in a header row `h` device pixels tall: its first pixel row and its thickness, whole pixels at the row's foot (two at 100%, three at 150%, four at 200%: twice the row's 24 px unit, rounded).
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
/// the grid below share ([`header_ground`]).
const HEADER_FOOT_FLOOR: f64 = 1.5;

/// The foot's colour ([`HEADER_FOOT_ALPHA`]); under the Fluent chrome, where
/// the header wears its pane's ground and the foot is all that closes it
/// against the grid, held at [`HEADER_FOOT_FLOOR`] there: the label ink laid
/// at the least opacity that clears it, as an opaque tone.
fn header_foot(colors: &chrome_band::BandColors, fluent: bool) -> crate::widget::Rgba {
    let quiet = rgba(colors.label, HEADER_FOOT_ALPHA);
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
    let mut prims = vec![DrawPrim::Line {
        x1: 0.0,
        y1: h - 0.5,
        x2: w,
        y2: h - 0.5,
        width: 1.0,
        color: header_foot(&colors, fluent),
    }];
    if fluent {
        prims.push(DrawPrim::Dot {
            cx: 12.0 * scale,
            cy: h * 0.5,
            r: if focused { 3.0 } else { 2.5 } * scale,
            color: rgba(accent, if focused { 255 } else { 120 }),
        });
        if focused {
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
#[path = "pane_header_tests.rs"]
mod tests;
