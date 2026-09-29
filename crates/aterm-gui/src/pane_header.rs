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
    meta_attention: bool,
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
                    images.sort_unstable_by_key(|(col, _)| *col);
                }
            }
        }
    }
}

impl App {
    /// Nonblocking reads, with the previous label retained during PTY contention.
    /// The same fingerprint gates repaint and the cached raster; title-only and
    /// focus-only changes therefore repaint even with the native parent strip.
    pub(crate) fn refresh_pane_headers(&mut self, wid: WindowId, plan: &VisibleLeafPlan) -> u64 {
        let Some(ws) = self.windows.get_mut(&wid) else {
            return 0;
        };
        if plan.leaves.iter().all(|leaf| leaf.header.is_none()) {
            ws.pane_headers.entries.clear();
            return 0;
        }
        let mut cache = std::mem::take(&mut ws.pane_headers);
        cache.entries.retain(|entry| {
            plan.leaves
                .iter()
                .any(|leaf| leaf.header.is_some() && leaf.view == entry.view)
        });
        let theme = self.chrome_palette_theme();
        let colors = chrome_band::band_colors(theme);
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
                    meta_attention: false,
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
                    if let Some(session) = self.pool.get(view.session) {
                        let meta = session.ctx.meta.try_lock().ok().map(|meta| {
                            (
                                meta.presentation_value("title"),
                                meta.presentation_value("icon"),
                                meta.attention.is_some(),
                            )
                        });
                        if let Some((user_title, icon, attention)) = meta {
                            entry.icon = icon.unwrap_or_default();
                            entry.meta_attention = attention;
                            if let Some(title) = user_title.filter(|s| !s.is_empty()) {
                                entry.title = title;
                            } else if let Some(term) = term_try_lock(&session.term) {
                                use crate::cwd_native::ReportedCwd as _;
                                let cwd = term.native_working_directory();
                                entry.title = crate::app_tabs::resolved_terminal_title_rung(
                                    None,
                                    term.title(),
                                    cwd.as_deref(),
                                )
                                .unwrap_or_else(|| "Terminal".into());
                            }
                        }
                    }
                    indicators.attention |= entry.meta_attention;
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
    let colors = chrome_band::band_colors(theme);
    let mut cell = chrome_band::blank_cell(theme);
    cell.fg = if focused { colors.value } else { colors.label };
    let mut row = SemanticRow {
        cells: vec![cell; cols],
        ..SemanticRow::default()
    };
    if let Some(marker) = row.cells.first_mut() {
        marker.ch = if focused { '▸' } else { '└' };
        marker.fg = if focused { colors.accent } else { colors.label };
    }
    let status = status_label(indicators);
    let suffix = if focused && cols >= 32 {
        if status.is_empty() {
            "FOCUSED".into()
        } else {
            format!("{status} FOCUSED")
        }
    } else if cols >= 6 {
        status.to_string()
    } else {
        String::new()
    };
    let suffix_cols = suffix.chars().count();
    let label_cols = cols.saturating_sub(1 + suffix_cols + usize::from(suffix_cols > 0));
    let label = fit_cell_label(label, label_cols);
    let mut col = 1;
    for cluster in aterm_grapheme::split_graphemes(&label) {
        let folded = cluster.is_emoji || cluster.text.contains('\u{20e3}');
        let width = if folded {
            cluster.width
        } else {
            aterm_grapheme::grapheme_grid_columns(cluster.text)
        };
        if width == 0 {
            continue;
        }
        if col + width > cols {
            break;
        }
        if folded {
            row.cells[col].ch = cluster.text.chars().next().unwrap_or(' ');
            if cluster.codepoint_count > 1 {
                row.clusters.push((col, cluster.text.into()));
            }
            if width == 2 {
                row.cells[col + 1].wide = true;
            }
            col += width;
        } else {
            let mut base = None;
            for ch in cluster.text.chars() {
                let width = aterm_grapheme::char_width(ch);
                if width == 0 {
                    if let Some(base) = base {
                        if let Some((_, marks)) =
                            row.combining.last_mut().filter(|(at, _)| *at == base)
                        {
                            let mut extended = marks.to_vec();
                            extended.push(ch);
                            *marks = extended.into_boxed_slice();
                        } else {
                            row.combining.push((base, vec![ch].into_boxed_slice()));
                        }
                    }
                    continue;
                }
                row.cells[col].ch = ch;
                base = Some(col);
                if width == 2 {
                    row.cells[col + 1].wide = true;
                }
                col += width;
            }
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

fn fit_cell_label(label: &str, cols: usize) -> String {
    if aterm_grapheme::str_grid_columns(label) <= cols {
        return label.into();
    }
    if cols == 0 {
        return String::new();
    }
    let mut result = String::new();
    let mut available = cols - 1;
    for cluster in label.graphemes() {
        let width = aterm_grapheme::grapheme_grid_columns(cluster);
        if width > available {
            break;
        }
        result.push_str(cluster);
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

fn raster(
    label: &str,
    cols: usize,
    cell: (usize, usize),
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
    let size = TypeStep::Secondary.px(h * 0.80);
    let face = if focused {
        TextFace::UiBold
    } else {
        TextFace::Ui
    };
    let fg = if focused { colors.value } else { colors.label };
    let accent = if focused { colors.accent } else { colors.label };
    let y = tray_raster::row_baseline(0.0, h - scale, size.get());
    let mut prims = vec![
        // Shared straight band, with a quiet seam against the content. Only
        // the focused child gets a short accent rail; no top-level tab shape.
        DrawPrim::Line {
            x1: 0.0,
            y1: h - 0.5,
            x2: w,
            y2: h - 0.5,
            width: 1.0,
            color: rgba(colors.label, 45),
        },
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
    ];
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
    let status = status_label(indicators);
    let suffix = if focused && w >= 220.0 * scale {
        if status.is_empty() {
            "FOCUSED".to_string()
        } else {
            format!("{status}  FOCUSED")
        }
    } else {
        status.to_string()
    };
    let suffix_size = TypeStep::Caption.px(h * 0.70);
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

    #[test]
    fn semantic_rows_preserve_clusters_and_mark_only_wide_continuations() {
        let row = semantic_row(
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
        let clipped = semantic_row(
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
