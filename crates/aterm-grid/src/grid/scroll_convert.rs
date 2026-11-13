// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Row-to-line conversion for scrollback.

use std::sync::Arc;

use aterm_alloc::SmallVec;
use aterm_rle::Rle;
use aterm_scrollback::{CellAttrs, HyperlinkSpan, ImageSpan, Line, UnderlineColorSpan};

use super::Grid;
use super::scroll_materialize::walk_folds_across;
use crate::Cell;
use crate::CellExtra;
use crate::CellFlags;
use crate::PackedColor;
use crate::Row;
use crate::{CellCoord, CellExtras};

use std::sync::atomic::{AtomicBool, Ordering};

static SCROLLBACK_TEXT_ONLY: AtomicBool = AtomicBool::new(false);

/// Enable or disable the headless scrollback-text-only fast path.
#[inline]
pub fn set_scrollback_text_only(enabled: bool) {
    SCROLLBACK_TEXT_ONLY.store(enabled, Ordering::Relaxed);
}

/// Whether the headless scrollback-text-only fast path is active.
#[must_use]
#[inline]
pub fn scrollback_text_only() -> bool {
    SCROLLBACK_TEXT_ONLY.load(Ordering::Relaxed)
}

/// Whether `cells[idx]` is a genuine wide-character continuation spacer rather than a DECSCA-protected cell: `PROTECTED` and `WIDE_CONTINUATION` share bit 10, and skipping protected cells would drop them from scrollback.
#[inline]
pub(in crate::grid) const fn is_spacer(cells: &[Cell], idx: usize) -> bool {
    cells[idx].is_wide_continuation()
        && !cells[idx].is_wide()
        && idx > 0
        && cells[idx - 1].is_wide()
}

const RUN_DISQUALIFY: CellFlags =
    CellFlags(CellFlags::COMPLEX.0 | CellFlags::WIDE.0 | CellFlags::WIDE_CONTINUATION.0);

/// A cell's plain printable-ASCII byte, or `None` when it needs the per-cell
/// path.
#[inline]
const fn plain_ascii_byte(cell: &Cell) -> Option<u8> {
    if cell.flags().intersects(RUN_DISQUALIFY) {
        return None;
    }
    // The flags above exclude character-overflow indices. Testing the raw BMP value
    // also rejects surrogates without decoding a Unicode scalar per cell.
    let c = cell.char_data();
    if c >= 0x20 && c < 0x80 {
        Some(c as u8)
    } else {
        None
    }
}

/// The attributes of `cell` exactly as the per-cell loop computed them.
#[inline]
const fn cell_attrs_of(cell: &Cell) -> CellAttrs {
    let fg_raw = match cell.fg_color() {
        Some(c) => c.0,
        None => PackedColor::DEFAULT_FG.0,
    };
    let bg_raw = match cell.bg_color() {
        Some(c) => c.0,
        None => PackedColor::DEFAULT_BG.0,
    };
    CellAttrs::from_raw(fg_raw, bg_raw, cell.flags().bits())
}

/// Copy eight matching plain ASCII cells, leaving rejected output unused.
/// `flags` and `colors` come from a first cell accepted by `plain_ascii_byte`;
/// matching that qualified style also excludes complex and wide cells.
#[inline]
fn copy_plain_ascii_block(
    cells: &[Cell; 8],
    flags: u16,
    colors: crate::PackedColors,
    out: &mut [u8; 8],
) -> bool {
    #[cfg(all(
        target_arch = "aarch64",
        target_endian = "little",
        target_feature = "neon"
    ))]
    if cells.as_ptr().cast::<u16>().is_aligned() {
        // SAFETY: The cfg enables NEON; the exact arrays and checked alignment
        // satisfy the helper's read and write bounds.
        return unsafe { copy_plain_ascii_block_neon(cells, flags, colors, out) };
    }

    let mut mismatch = 0u32;
    for (offset, cell) in cells.iter().enumerate() {
        let raw = cell.char_data();
        mismatch |= u32::from(cell.flags().bits() ^ flags);
        mismatch |= cell.colors().0 ^ colors.0;
        mismatch |= u32::from(raw.wrapping_sub(0x20) >= 0x60);
        out[offset] = (raw & 0xff) as u8;
    }
    mismatch == 0
}

#[cfg(all(
    target_arch = "aarch64",
    target_endian = "little",
    target_feature = "neon"
))]
#[inline]
#[target_feature(enable = "neon")]
unsafe fn copy_plain_ascii_block_neon(
    cells: &[Cell; 8],
    flags: u16,
    colors: crate::PackedColors,
    out: &mut [u8; 8],
) -> bool {
    use std::arch::aarch64::{
        vcgtq_u16, vdupq_n_u16, veorq_u16, vld4q_u16, vmaxvq_u16, vmovn_u16, vorrq_u16, vst1_u8,
        vsubq_u16,
    };

    // SAFETY: The cfg guarantees NEON and little endian. Cell's repr(C, packed)
    // layout is asserted in cell.rs: eight initialized bytes, with u16 char,
    // u32 colors and u16 flags at offsets 0, 2 and 6. This exact eight-cell
    // array therefore supplies all 64 bytes read, without padding. The caller
    // checked u16 alignment; no reference with increased alignment is formed.
    let fields = unsafe { vld4q_u16(cells.as_ptr().cast::<u16>()) };
    let colors_low = vdupq_n_u16((colors.0 & 0xffff) as u16);
    let colors_high = vdupq_n_u16((colors.0 >> 16) as u16);
    let styles = vorrq_u16(
        veorq_u16(fields.3, vdupq_n_u16(flags)),
        vorrq_u16(
            veorq_u16(fields.1, colors_low),
            veorq_u16(fields.2, colors_high),
        ),
    );
    let outside_ascii = vcgtq_u16(vsubq_u16(fields.0, vdupq_n_u16(0x20)), vdupq_n_u16(0x5f));
    if vmaxvq_u16(vorrq_u16(styles, outside_ascii)) != 0 {
        return false;
    }
    // SAFETY: out supplies exactly eight writable bytes. Each accepted u16 is
    // in 0x20..=0x7f, so narrowing preserves its initialized ASCII byte.
    unsafe { vst1_u8(out.as_mut_ptr(), vmovn_u16(fields.0)) };
    true
}

/// Append the maximal run of printable ASCII under one attribute at `from` in
/// fixed-size chunks and one [`AttrRunBuilder::extend`], returning where it ended.
#[inline]
fn append_plain_run<const CHECK_UTF8: bool>(
    cells: &[Cell],
    from: usize,
    text: &mut String,
    attrs_rle: &mut AttrRunBuilder,
) -> usize {
    let Some(first) = plain_ascii_byte(&cells[from]) else {
        return from;
    };
    let attrs = cell_attrs_of(&cells[from]);
    let flags = cells[from].flags().bits();
    let colors = cells[from].colors();
    if cells
        .get(from + 1)
        .is_some_and(|next| next.flags().bits() != flags || next.colors() != colors)
    {
        text.push(char::from(first));
        attrs_rle.extend(attrs, 1);
        return from + 1;
    }

    let mut chunk = [0u8; 64];
    let mut end = from;
    while end < cells.len() {
        let mut n = 0usize;
        while n + 8 <= chunk.len() && end + 8 <= cells.len() {
            let block = cells[end..end + 8].try_into().expect("eight cells");
            let out = (&mut chunk[n..n + 8]).try_into().expect("eight bytes");
            if !copy_plain_ascii_block(block, flags, colors, out) {
                break;
            }
            end += 8;
            n += 8;
        }

        while n < chunk.len() && end < cells.len() {
            let cell = &cells[end];
            if cell.flags().bits() != flags || cell.colors() != colors {
                break;
            }
            let Some(byte) = plain_ascii_byte(cell) else {
                break;
            };
            chunk[n] = byte;
            n += 1;
            end += 1;
        }
        if n == 0 {
            break;
        }
        let ascii = if CHECK_UTF8 {
            std::str::from_utf8(&chunk[..n]).unwrap_or_default()
        } else {
            // SAFETY: Portable block lookahead may write before validation, but n advances
            // only when every raw character passes 0x20..0x80. On failure the
            // scalar tail overwrites its accepted prefix with plain_ascii_byte
            // results before advancing n; all other lookahead stays outside
            // this slice. Thus every selected byte is initialized ASCII.
            unsafe { std::str::from_utf8_unchecked(&chunk[..n]) }
        };
        text.push_str(ascii);
        if n < chunk.len() {
            break;
        }
    }
    attrs_rle.extend(attrs, u32::try_from(end - from).unwrap_or(u32::MAX));
    end
}

/// Whether any cell carries something [`Grid::extract_row_extras_into`] can
/// extract without the extras map: a complex codepoint or an RGB overflow
/// colour.
#[inline]
fn row_has_extractable_cells(cells: &[Cell]) -> bool {
    let mut found = false;
    for cell in cells {
        found |= cell.is_complex() | cell.fg_needs_overflow() | cell.bg_needs_overflow();
    }
    found
}

const _: () = assert!(std::mem::size_of::<Cell>() == 8);

/// `cells` (a row's first cells, every occupied one) as a scrollback [`Line`], with `extras`
/// extracted when the row left the screen.
pub(super) fn cells_to_line(cells: &[Cell], extras: &ScrolledRowExtras, wrapped: bool) -> Line {
    cells_to_line_with_spans(
        cells,
        extras,
        wrapped,
        extras.hyperlinks.clone(),
        extras.images.clone(),
    )
}

/// [`cells_to_line`] for extras the caller gives up: their link and image spans move into the
/// line instead of being cloned.
pub(super) fn cells_to_line_owned(
    cells: &[Cell],
    mut extras: ScrolledRowExtras,
    wrapped: bool,
) -> Line {
    let hyperlinks = std::mem::take(&mut extras.hyperlinks);
    let images = std::mem::take(&mut extras.images);
    cells_to_line_with_spans(cells, &extras, wrapped, hyperlinks, images)
}

fn cells_to_line_with_spans(
    cells: &[Cell],
    extras: &ScrolledRowExtras,
    wrapped: bool,
    hyperlinks: Vec<HyperlinkSpan>,
    images: Vec<ImageSpan>,
) -> Line {
    #[cfg(any(test, debug_assertions))]
    super::count_row_to_line_op();

    let mut line = if cells.is_empty() {
        Line::new()
    } else if extras.is_empty() && hyperlinks.is_empty() {
        plain_cells_to_line(cells)
    } else {
        styled_cells_to_line(cells, extras, hyperlinks)
    };
    if !images.is_empty() {
        line.set_images(images);
    }
    if wrapped {
        line.set_wrapped(true);
    }
    line
}

fn plain_cells_to_line(cells: &[Cell]) -> Line {
    plain_cells_to_line_with::<false>(cells)
}

#[inline]
fn plain_cells_to_line_with<const CHECK_UTF8: bool>(cells: &[Cell]) -> Line {
    let mut text = String::with_capacity(cells.len());
    let mut attrs_rle = AttrRunBuilder::empty();

    let mut idx = 0;
    while idx < cells.len() {
        let run_end = append_plain_run::<CHECK_UTF8>(cells, idx, &mut text, &mut attrs_rle);
        if run_end > idx {
            idx = run_end;
            continue;
        }
        let cell = &cells[idx];
        if !is_spacer(cells, idx) {
            text.push(cell.char());
            attrs_rle.push(cell_attrs_of(cell));
        }
        idx += 1;
    }

    Line::with_hyperlinks_owned(text, attrs_rle.finish(), Vec::new())
}

fn styled_cells_to_line(
    cells: &[Cell],
    extras: &ScrolledRowExtras,
    hyperlinks: Vec<HyperlinkSpan>,
) -> Line {
    let mut text = String::with_capacity(cells.len());
    let mut attrs_rle = AttrRunBuilder::empty();
    let mut cursors = RowToLineCursorState::default();
    let mut prev: Option<usize> = None;

    for (physical_col, cell) in cells.iter().enumerate() {
        if is_spacer(cells, physical_col) {
            continue;
        }

        let col_u16 = u16::try_from(physical_col).unwrap_or(u16::MAX);
        let at = text.len();
        let char_count = push_cell_text(&mut text, *cell, extras, &mut cursors, col_u16);
        let opens = prev.is_some_and(|prev| walk_folds_across(&text, prev, at));
        prev = Some(at);
        let fg_raw = resolve_cell_color(
            cell.fg_needs_overflow(),
            cell.fg_color().map_or(PackedColor::DEFAULT_FG.0, |c| c.0),
            &extras.rgb_fg,
            &mut cursors.rgb_fg_idx,
            col_u16,
            PackedColor::DEFAULT_FG.0,
        );
        let bg_raw = resolve_cell_color(
            cell.bg_needs_overflow(),
            cell.bg_color().map_or(PackedColor::DEFAULT_BG.0, |c| c.0),
            &extras.rgb_bg,
            &mut cursors.rgb_bg_idx,
            col_u16,
            PackedColor::DEFAULT_BG.0,
        );

        let attrs = CellAttrs::from_raw(fg_raw, bg_raw, cell.flags().bits());
        if opens {
            attrs_rle.push(attrs.with_opens_cell(true));
            push_repeated_attrs(&mut attrs_rle, attrs, char_count.saturating_sub(1));
        } else {
            push_repeated_attrs(&mut attrs_rle, attrs, char_count);
        }
        push_combining_marks(
            &mut text,
            &mut attrs_rle,
            attrs,
            extras,
            &mut cursors,
            col_u16,
        );
    }

    let mut line = Line::with_hyperlinks_owned(text, attrs_rle.finish(), hyperlinks);
    if !extras.underline_colors.is_empty() {
        line.set_underline_colors(coalesce_underline_spans(&extras.underline_colors));
    }
    line
}

/// Extend a compatible run at or after `minimum_index`, or begin a new run.
/// Passing zero for a column's first layer needs no ordering comparison.
#[inline(always)]
fn capture_image_layer(
    open: &mut aterm_alloc::ArrayVec<(usize, u16), { 2 * crate::extra::MAX_IMAGE_LAYERS }>,
    spans: &mut Vec<ImageSpan>,
    col: u16,
    layer: &crate::ImageRef,
    minimum_index: usize,
) -> usize {
    if let Some(run) = open.iter_mut().find(|run| {
        let span = &spans[run.0];
        run.0 >= minimum_index
            && Arc::ptr_eq(&span.image, &layer.image)
            && span.kitty == layer.kitty
            && span.image_row == layer.cell_row
            && run.1 == layer.cell_col
    }) {
        run.1 = run.1.saturating_add(1);
        spans[run.0].end_col = col.saturating_add(1);
        run.0
    } else {
        let index = spans.len();
        open.push((index, layer.cell_col.saturating_add(1)));
        spans.push(ImageSpan {
            kitty: layer.kitty,
            ..ImageSpan::new(
                col,
                col.saturating_add(1),
                layer.cell_row,
                layer.cell_col,
                Arc::clone(&layer.image),
            )
        });
        index
    }
}

/// Cell extras extracted from a scrolled-off row.
#[derive(Debug, Clone, Default)]
pub struct ScrolledRowExtras {
    /// Coalesced hyperlink spans.
    pub hyperlinks: Vec<HyperlinkSpan>,
    /// Complex character strings keyed by physical column.
    pub complex_chars: Vec<(u16, Arc<str>)>,
    /// Combining characters keyed by physical column.
    pub combining: Vec<(u16, SmallVec<char, 2>)>,
    /// Resolved foreground RGB colors keyed by physical column.
    pub rgb_fg: Vec<(u16, [u8; 3])>,
    /// Resolved background RGB colors keyed by physical column.
    pub rgb_bg: Vec<(u16, [u8; 3])>,
    /// Packed SGR 58 underline colors keyed by physical column.
    pub underline_colors: Vec<(u16, u32)>,
    /// Coalesced inline-image spans.
    pub images: Vec<ImageSpan>,
}

impl ScrolledRowExtras {
    /// Estimated bytes retained by this boxed extras value: vector capacities, out-of-line strings (shared ones charged per holder) and each image's per-row share.
    pub(crate) fn retained_memory_used(&self) -> usize {
        const fn capacity_bytes<T>(values: &Vec<T>) -> usize {
            values.capacity().saturating_mul(std::mem::size_of::<T>())
        }
        let mut total = std::mem::size_of::<Self>()
            .saturating_add(self.link_bytes())
            .saturating_add(capacity_bytes(&self.complex_chars))
            .saturating_add(capacity_bytes(&self.combining))
            .saturating_add(capacity_bytes(&self.rgb_fg))
            .saturating_add(capacity_bytes(&self.rgb_bg))
            .saturating_add(capacity_bytes(&self.underline_colors))
            .saturating_add(capacity_bytes(&self.images));
        for (_, text) in &self.complex_chars {
            total = total.saturating_add(text.len());
        }
        for (_, marks) in &self.combining {
            if marks.spilled() {
                total = total
                    .saturating_add(marks.capacity().saturating_mul(std::mem::size_of::<char>()));
            }
        }
        for span in &self.images {
            total = total.saturating_add(span.image.per_row_bytes());
        }
        total
    }

    /// True when all fields are empty (common case: plain ASCII text).
    #[inline]
    pub(crate) const fn is_empty(&self) -> bool {
        self.hyperlinks.is_empty()
            && self.complex_chars.is_empty()
            && self.combining.is_empty()
            && self.rgb_fg.is_empty()
            && self.rgb_bg.is_empty()
            && self.underline_colors.is_empty()
            && self.images.is_empty()
    }

    /// The OSC 8 share of [`retained_memory_used`](Self::retained_memory_used):
    /// span capacity and per-reference payload/string charges. This is the
    /// budget credit released by [`strip_hyperlinks`](Self::strip_hyperlinks).
    #[must_use]
    pub(crate) fn link_bytes(&self) -> usize {
        let mut total = self
            .hyperlinks
            .capacity()
            .saturating_mul(std::mem::size_of::<HyperlinkSpan>());
        for span in &self.hyperlinks {
            total = total.saturating_add(span.retained_payload_bytes());
        }
        total
    }

    /// Drop this row's links, spans and vector both, keeping its text, colours and images: the history link budget's eviction (`Grid::history_link_budget`).
    #[inline]
    pub(crate) fn strip_hyperlinks(&mut self) {
        self.hyperlinks = Vec::new();
    }

    /// Clear all fields, retaining `Vec` capacities so the struct can be recycled as extraction scratch (see `extract_row_extras_into`).
    #[inline]
    pub(crate) fn clear(&mut self) {
        self.hyperlinks.clear();
        self.complex_chars.clear();
        self.combining.clear();
        self.rgb_fg.clear();
        self.rgb_bg.clear();
        self.underline_colors.clear();
        self.images.clear();
    }
}

/// Monotone read cursors over the four column-sorted `ScrolledRowExtras` vectors.
#[derive(Default)]
pub(in crate::grid) struct RowToLineCursorState {
    complex_idx: usize,
    combining_idx: usize,
    pub(in crate::grid) rgb_fg_idx: usize,
    pub(in crate::grid) rgb_bg_idx: usize,
}

impl Grid {
    /// Convert a row to a [`Line`] without extras (test helper).
    #[cfg(test)]
    pub(crate) fn row_to_line_static(row: &Row) -> Line {
        Self::row_to_line_with_stored_extras(row, &ScrolledRowExtras::default())
    }

    /// Convert a row to a [`Line`] using extras extracted at scroll time (RGB overflow colours already resolved).
    pub(crate) fn row_to_line_with_stored_extras(row: &Row, extras: &ScrolledRowExtras) -> Line {
        Self::row_to_line_with_stored_extras_at_len(row, extras, row.written_len())
    }

    /// [`Self::row_to_line_with_stored_extras`] with an explicit cell count.
    pub(crate) fn row_to_line_with_stored_extras_at_len(
        row: &Row,
        extras: &ScrolledRowExtras,
        len: u16,
    ) -> Line {
        let len = usize::from(len.min(row.cols()));
        cells_to_line(&row.as_slice()[..len], extras, row.is_wrapped())
    }

    /// Convert a row while transferring its already-owned link and image spans.
    pub(crate) fn row_to_line_with_owned_extras_at_len(
        row: &Row,
        extras: ScrolledRowExtras,
        len: u16,
    ) -> Line {
        let len = usize::from(len.min(row.cols()));
        cells_to_line_owned(&row.as_slice()[..len], extras, row.is_wrapped())
    }

    /// Extract a row's extras (hyperlinks, complex chars, combining marks and RGB colours) before `shift_rows_up_by` discards them.
    pub(crate) fn extract_row_extras(
        row: &Row,
        extras: &CellExtras,
        row_idx: u16,
    ) -> ScrolledRowExtras {
        let mut result = ScrolledRowExtras::default();
        Self::extract_row_extras_into(&mut result, row, extras, row_idx);
        result
    }

    /// [`Grid::extract_row_extras`] into a caller's struct (cleared first), so the scroll path can recycle a popped `ring_extras` allocation instead of reallocating its `Vec`s per styled row.
    pub(crate) fn extract_row_extras_into(
        result: &mut ScrolledRowExtras,
        row: &Row,
        extras: &CellExtras,
        row_idx: u16,
    ) {
        result.clear();
        if scrollback_text_only() {
            Self::extract_hyperlinks_only_into(result, row, extras, row_idx);
            return;
        }
        Self::extract_image_spans_into(result, row, extras, row_idx);
        let len = row.len() as usize;
        if len == 0 {
            return;
        }

        if !extras.has_any_data() {
            return;
        }

        let probe_map = extras.map_may_have_row(row_idx, len);

        if !probe_map && !row_has_extractable_cells(&row.as_slice()[..len]) {
            return;
        }

        if result.rgb_fg.capacity() == 0 || result.rgb_bg.capacity() == 0 {
            let cap_cells = &row.as_slice()[..len];
            let (mut n_fg, mut n_bg) = (0usize, 0usize);
            for (i, c) in cap_cells.iter().enumerate() {
                // Mirror the main loop's spacer skip so the reserve matches what
                // is actually pushed.
                if is_spacer(cap_cells, i) {
                    continue;
                }
                if c.fg_needs_overflow() {
                    n_fg += 1;
                }
                if c.bg_needs_overflow() {
                    n_bg += 1;
                }
            }
            if n_fg > 0 {
                result.rgb_fg.reserve(n_fg);
            }
            if n_bg > 0 {
                result.rgb_bg.reserve(n_bg);
            }
        }

        let mut current_span: Option<(u16, &Arc<crate::HyperlinkData>)> = None;

        let cells = &row.as_slice()[..len];
        for (physical_col, cell) in cells.iter().enumerate() {
            if is_spacer(cells, physical_col) {
                continue;
            }

            let col_u16 = u16::try_from(physical_col).unwrap_or(u16::MAX);
            let coord = CellCoord::new(row_idx, col_u16);

            if cell.is_complex() {
                if let Some(arc) = extras.complex_char_arc_for(row_idx, col_u16) {
                    result.complex_chars.push((col_u16, Arc::clone(arc)));
                } else if let Some(c) = extras.complex_codepoint_for(row_idx, col_u16) {
                    let mut buf = [0u8; 4];
                    let s = c.encode_utf8(&mut buf);
                    result.complex_chars.push((col_u16, Arc::from(s)));
                }
            }

            if cell.fg_needs_overflow()
                && let Some(rgb) = extras.fg_rgb_for(row_idx, col_u16)
            {
                result.rgb_fg.push((col_u16, rgb));
            }

            if cell.bg_needs_overflow()
                && let Some(rgb) = extras.bg_rgb_for(row_idx, col_u16)
            {
                result.rgb_bg.push((col_u16, rgb));
            }

            if let Some(extra) = probe_map.then(|| extras.get(coord)).flatten() {
                if !extra.combining().is_empty() {
                    result
                        .combining
                        .push((col_u16, SmallVec::from_slice(extra.combining())));
                }

                if let Some(packed) = extra.underline_color_u32() {
                    result.underline_colors.push((col_u16, packed));
                }

                let payload = extra.hyperlink_data();
                match (current_span, payload) {
                    (None, Some(payload)) => {
                        current_span = Some((col_u16, payload));
                    }
                    (Some((_, previous)), Some(payload))
                        if crate::HyperlinkData::same_scope(previous, payload) => {}
                    (Some((start, previous)), next) => {
                        result.hyperlinks.push(HyperlinkSpan::from_shared(
                            start,
                            col_u16,
                            Arc::clone(previous),
                        ));
                        current_span = next.map(|payload| (col_u16, payload));
                    }
                    (None, None) => {}
                }
            } else {
                if let Some((start, previous)) = current_span.take() {
                    result.hyperlinks.push(HyperlinkSpan::from_shared(
                        start,
                        col_u16,
                        Arc::clone(previous),
                    ));
                }
            }
        }

        if let Some((start, payload)) = current_span {
            let end_col = u16::try_from(len).unwrap_or(u16::MAX);
            result.hyperlinks.push(HyperlinkSpan::from_shared(
                start,
                end_col,
                Arc::clone(payload),
            ));
        }
    }

    pub(super) fn extract_image_spans_into(
        result: &mut ScrolledRowExtras,
        row: &Row,
        extras: &CellExtras,
        row_idx: u16,
    ) {
        if !extras.any_image() {
            return;
        }
        let cells = row.as_slice();
        // Append each span once, at birth. Only active output indices need
        // staging; stable sorting below preserves birth order within each key.
        let first = result.images.len();
        // A column can close every previous layer and open every current one
        // before compaction, so two per-cell layer limits bound this array.
        let mut open: aterm_alloc::ArrayVec<(usize, u16), { 2 * crate::extra::MAX_IMAGE_LAYERS }> =
            aterm_alloc::ArrayVec::new();
        for (physical_col, cell) in cells.iter().enumerate() {
            let Ok(col_u16) = u16::try_from(physical_col) else {
                break;
            };
            let layers = if cell.has_extras() {
                extras
                    .get(CellCoord::new(row_idx, col_u16))
                    .map_or(&[][..], CellExtra::images)
            } else {
                &[]
            };
            // Equal-key layers use emission order as their paint precedence.
            // Reusing an older upper run after opening a lower one would put
            // that upper run below it when the spans are restored.
            if let Some((first_layer, rest)) = layers.split_first() {
                let mut previous_layer = first_layer;
                let mut previous_index =
                    capture_image_layer(&mut open, &mut result.images, col_u16, first_layer, 0);
                for layer in rest {
                    previous_index = if previous_layer.image.z_index == layer.image.z_index
                        && previous_layer.draw_key() == layer.draw_key()
                    {
                        capture_image_layer(
                            &mut open,
                            &mut result.images,
                            col_u16,
                            layer,
                            previous_index + 1,
                        )
                    } else {
                        capture_image_layer(&mut open, &mut result.images, col_u16, layer, 0)
                    };
                    previous_layer = layer;
                }
            }
            // These indices are Copy. Compact them directly rather than relying
            // on inlining a generic retain's unwind bookkeeping at every column.
            let mut kept = 0;
            for read in 0..open.len() {
                let run = open[read];
                if result.images[run.0].end_col > col_u16 {
                    if kept != read {
                        open[kept] = run;
                    }
                    kept += 1;
                }
            }
            open.truncate(kept);
        }
        result.images[first..]
            .sort_by_key(|span| aterm_types::layer_order(span.image.z_index, span.kitty));
    }

    fn extract_hyperlinks_only_into(
        result: &mut ScrolledRowExtras,
        row: &Row,
        extras: &CellExtras,
        row_idx: u16,
    ) {
        let len = row.len() as usize;
        if len == 0 || !extras.has_any_data() {
            return;
        }
        let mut current_span: Option<(u16, &Arc<crate::HyperlinkData>)> = None;
        let cells = &row.as_slice()[..len];
        for physical_col in 0..len {
            if is_spacer(cells, physical_col) {
                continue;
            }
            let col_u16 = u16::try_from(physical_col).unwrap_or(u16::MAX);
            let coord = CellCoord::new(row_idx, col_u16);
            let payload = extras.get(coord).and_then(|e| e.hyperlink_data());
            match (current_span, payload) {
                (None, Some(payload)) => {
                    current_span = Some((col_u16, payload));
                }
                (Some((_, previous)), Some(payload))
                    if crate::HyperlinkData::same_scope(previous, payload) => {}
                (Some((start, previous)), next) => {
                    result.hyperlinks.push(HyperlinkSpan::from_shared(
                        start,
                        col_u16,
                        Arc::clone(previous),
                    ));
                    current_span = next.map(|payload| (col_u16, payload));
                }
                (None, None) => {}
            }
        }
        if let Some((start, payload)) = current_span {
            let end_col = u16::try_from(len).unwrap_or(u16::MAX);
            result.hyperlinks.push(HyperlinkSpan::from_shared(
                start,
                end_col,
                Arc::clone(payload),
            ));
        }
    }
}

fn push_cell_text(
    text: &mut String,
    cell: Cell,
    extras: &ScrolledRowExtras,
    cursors: &mut RowToLineCursorState,
    col_u16: u16,
) -> usize {
    if !cell.is_complex() {
        let ch = cell.char();
        text.push(if ch == '\0' { ' ' } else { ch });
        return 1;
    }

    if let Some(value) = next_complex_char(extras, cursors, col_u16) {
        let char_count = value.chars().count();
        text.push_str(value);
        char_count
    } else {
        text.push('\u{FFFD}');
        1
    }
}

/// Consume this column's stored complex-char string, if the cursor is on it.
#[inline]
pub(in crate::grid) fn next_complex_char<'a>(
    extras: &'a ScrolledRowExtras,
    cursors: &mut RowToLineCursorState,
    col_u16: u16,
) -> Option<&'a Arc<str>> {
    next_keyed(&extras.complex_chars, &mut cursors.complex_idx, col_u16)
}

/// The value a history row's column-keyed extras list (ascending by column) holds for `col_u16`, if the cursor `idx` is on it, advancing past it.
#[inline]
pub(in crate::grid) const fn next_keyed<'a, T>(
    entries: &'a [(u16, T)],
    idx: &mut usize,
    col_u16: u16,
) -> Option<&'a T> {
    if *idx < entries.len() && entries[*idx].0 == col_u16 {
        let value = &entries[*idx].1;
        *idx += 1;
        Some(value)
    } else {
        None
    }
}

/// Consume this column's stored combining marks, if the cursor is on them.
#[inline]
pub(in crate::grid) fn next_combining<'a>(
    extras: &'a ScrolledRowExtras,
    cursors: &mut RowToLineCursorState,
    col_u16: u16,
) -> Option<&'a [char]> {
    next_keyed(&extras.combining, &mut cursors.combining_idx, col_u16).map(|marks| marks.as_slice())
}

/// Resolve one channel of a cell's colour the way the Line serializer does: inline when the cell carries it, else the column's pre-resolved stored RGB (extraction resolved RGB overflow at scroll time), else the default.
#[inline]
pub(in crate::grid) const fn resolve_cell_color(
    needs_stored: bool,
    inline_raw: u32,
    stored_colors: &[(u16, [u8; 3])],
    color_idx: &mut usize,
    col_u16: u16,
    default_raw: u32,
) -> u32 {
    if !needs_stored {
        return inline_raw;
    }

    match next_keyed(stored_colors, color_idx, col_u16) {
        Some(&[r, g, b]) => PackedColor::rgb(r, g, b).0,
        None => default_raw,
    }
}

struct AttrRunBuilder {
    /// Runs already closed out. Empty (and unallocated) until the first spill.
    rle: Rle<CellAttrs>,
    /// Value of the open run, seeded to `DEFAULT` so a plain line's first cell
    /// merges into it instead of taking the cold path.
    value: CellAttrs,
    /// How many cells the open run covers; `0` means nothing opened yet.
    len: u32,
    /// Whether any run was closed into `rle`: tells "one run, and it is
    /// default" (drop it) from "several runs, the last one default" (keep them).
    spilled: bool,
}

impl AttrRunBuilder {
    const fn empty() -> Self {
        Self {
            rle: Rle::new(),
            value: CellAttrs::DEFAULT,
            len: 0,
            spilled: false,
        }
    }

    #[inline]
    fn push(&mut self, attrs: CellAttrs) {
        self.extend(attrs, 1);
    }

    /// Per-cell hot path: one comparison and one add. A fresh builder's open run
    /// is `(DEFAULT, 0)`, so `len == 0` needs no test.
    #[inline]
    fn extend(&mut self, attrs: CellAttrs, count: u32) {
        if count == 0 {
            return;
        }
        if self.value == attrs {
            self.len = self.len.saturating_add(count);
            return;
        }
        self.open_new_run(attrs, count);
    }

    /// Cold path: close the open run (if any) into the `Rle` and start a new one.
    #[inline(never)]
    fn open_new_run(&mut self, attrs: CellAttrs, count: u32) {
        if self.len != 0 {
            self.rle.extend_with(self.value, self.len);
            self.spilled = true;
        }
        self.value = attrs;
        self.len = count;
    }

    fn finish(mut self) -> Rle<CellAttrs> {
        if self.len != 0 && (self.spilled || !self.value.is_default()) {
            self.rle.extend_with(self.value, self.len);
        }
        self.rle
    }
}

fn push_repeated_attrs(attrs_rle: &mut AttrRunBuilder, attrs: CellAttrs, char_count: usize) {
    attrs_rle.extend(attrs, u32::try_from(char_count).unwrap_or(u32::MAX));
}

fn push_combining_marks(
    text: &mut String,
    attrs_rle: &mut AttrRunBuilder,
    attrs: CellAttrs,
    extras: &ScrolledRowExtras,
    cursors: &mut RowToLineCursorState,
    col_u16: u16,
) {
    let Some(combining) = next_combining(extras, cursors, col_u16) else {
        return;
    };

    for &c in combining {
        text.push(c);
        attrs_rle.push(attrs);
    }
}

/// Coalesce per-column packed underline colours (ascending by column) into `start, end)` `UnderlineColorSpan`s; a colour change or a column gap starts a new span, so filling the spans reproduces each column's colour.
pub(in crate::grid) fn coalesce_underline_spans(per_col: &[(u16, u32)]) -> Vec<UnderlineColorSpan> {
    let mut spans: Vec<UnderlineColorSpan> = Vec::new();
    for &(col, color) in per_col {
        if let Some(last) = spans.last_mut()
            && last.color == color
            && last.end_col == col
        {
            last.end_col = col.saturating_add(1);
            continue;
        }
        spans.push(UnderlineColorSpan::new(col, col.saturating_add(1), color));
    }
    spans
}

#[cfg(test)]
#[path = "scroll_convert_plain_run_equivalence_tests.rs"]
mod plain_run_equivalence_tests;

#[cfg(test)]
#[path = "scroll_convert_extract_gate_tests.rs"]
mod extract_gate_tests;

#[cfg(test)]
#[path = "scroll_convert_scrolled_row_extras_tests.rs"]
mod scrolled_row_extras_tests;
