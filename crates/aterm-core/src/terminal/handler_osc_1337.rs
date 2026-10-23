// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! iTerm2 OSC 1337 `File=` inline image handler and shared cell-stamping helpers.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use aterm_grid::{ImageData, ImageFormat, ImageRef, ImageScaling, KittyPlacementTag};

use super::handler::TerminalHandler;

/// Cursor placement policy after stamping an inline image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PlacementCursor {
    /// Move to column 0 on the line below the image (iTerm2 and sixel).
    NextLine,
    /// Advance past the image's right edge on its last row (Kitty default).
    AfterImage,
    /// Leave the cursor unmoved without scrolling (`C=1`).
    Unmoved,
}

/// Maximum decoded image payload accepted (bytes).
const MAX_IMAGE_BYTES: usize = 16 * 1024 * 1024;

/// Maximum footprint in cells along either axis.
const MAX_FOOTPRINT_CELLS: u16 = 4096;

/// The tallest raster the image paths make or read, in pixels: the renderers'
/// PNG decode limit (`aterm-render`'s `IMAGE_MAX_DIMENSION`) and the sixel
/// decoder's `SIXEL_MAX_DIMENSION`.
const MAX_DECODED_IMAGE_PX: u32 = 4096;

/// Maximum live-screen inline image payload bytes before older images are trimmed.
pub(super) const MAX_LIVE_IMAGE_BYTES: usize = 128 * 1024 * 1024;

/// What an over-budget screen is trimmed to: half the budget, so each count of
/// the screen's images is at least that many placed bytes after the last and
/// the counting stays linear in the bytes placed.
const LIVE_IMAGE_TRIM_BYTES: usize = MAX_LIVE_IMAGE_BYTES / 2;

/// A requested image dimension from the `width=`/`height=` argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dimension {
    /// Explicit cell count.
    Cells(u32),
    /// Explicit pixel count (converted to cells via the cell metric).
    Pixels(u32),
    /// Percentage of the viewport axis (1..=100).
    Percent(u32),
    /// Derive from the decoded image's pixel size.
    Auto,
}

impl Dimension {
    /// Parse a `width=`/`height=` value.
    fn parse(value: &str) -> Self {
        let v = value.trim();
        if v.is_empty() || v.eq_ignore_ascii_case("auto") {
            return Dimension::Auto;
        }
        if let Some(px) = v.strip_suffix("px").or_else(|| v.strip_suffix("PX")) {
            return px
                .trim()
                .parse::<u32>()
                .map_or(Dimension::Auto, Dimension::Pixels);
        }
        if let Some(pct) = v.strip_suffix('%') {
            return pct
                .trim()
                .parse::<u32>()
                .map_or(Dimension::Auto, |p| Dimension::Percent(p.clamp(1, 100)));
        }
        v.parse::<u32>().map_or(Dimension::Auto, Dimension::Cells)
    }

    /// Resolve to a CELL count.
    #[inline]
    const fn to_cells(self, cell_px: u16, viewport_cells: u16, auto_cells: u16) -> u16 {
        let cells = match self {
            Dimension::Cells(c) => c,
            Dimension::Pixels(px) => {
                let denom = if cell_px == 0 { 1 } else { cell_px as u32 };
                px.div_ceil(denom)
            }
            Dimension::Percent(p) => {
                let scaled = (viewport_cells as u32).saturating_mul(p) / 100;
                if scaled == 0 { 1 } else { scaled }
            }
            Dimension::Auto => auto_cells as u32,
        };
        clamp_cells(cells)
    }
}

/// Clamp a `u32` cell count into `0..=MAX_FOOTPRINT_CELLS` and narrow to `u16`.
#[allow(
    clippy::cast_possible_truncation,
    reason = "min(MAX_FOOTPRINT_CELLS=4096) guarantees the value fits u16"
)]
#[inline]
const fn clamp_cells(cells: u32) -> u16 {
    if cells > MAX_FOOTPRINT_CELLS as u32 {
        MAX_FOOTPRINT_CELLS
    } else {
        cells as u16
    }
}

/// Narrow a `usize` cell span to `u32`, saturating.
#[allow(
    clippy::cast_possible_truncation,
    reason = "v <= u32::MAX is checked before narrowing"
)]
#[inline]
const fn clamp_u32(v: usize) -> u32 {
    if v > u32::MAX as usize {
        u32::MAX
    } else {
        v as u32
    }
}

/// Parsed `File=` arguments (everything before the `:` payload separator).
#[derive(Debug, Default)]
struct FileArgs {
    /// `width=` request (default: derive from the image).
    width: Option<Dimension>,
    /// `height=` request (default: derive from the image).
    height: Option<Dimension>,
    /// `inline=1` — display inline.
    inline: bool,
    /// `preserveAspectRatio=0` disables aspect preservation (default on).
    preserve_aspect_ratio: bool,
}

impl TerminalHandler<'_> {
    /// Handle OSC 1337 (`File=` inline image; other sub-commands ignored).
    pub(super) fn handle_osc_1337(&mut self, params: &[&[u8]]) {
        if params.len() < 2 {
            return;
        }

        let colon_at = params
            .iter()
            .enumerate()
            .skip(1)
            .find_map(|(i, p)| p.iter().position(|&b| b == b':').map(|j| (i, j)));
        let Some((colon_param, colon_off)) = colon_at else {
            return;
        };

        if colon_param == params.len() - 1 {
            let payload = &params[colon_param][colon_off + 1..];
            if colon_param == 1 {
                let Some(args_bytes) = strip_prefix_ascii_ci(&params[1][..colon_off], b"File=")
                else {
                    return;
                };
                self.handle_osc_1337_file(args_bytes, payload);
                return;
            }
            let mut head: Vec<u8> = params[1].to_vec();
            for extra in &params[2..colon_param] {
                head.push(b';');
                head.extend_from_slice(extra);
            }
            head.push(b';');
            head.extend_from_slice(&params[colon_param][..colon_off]);
            let Some(args_bytes) = strip_prefix_ascii_ci(&head, b"File=") else {
                return;
            };
            self.handle_osc_1337_file(args_bytes, payload);
            return;
        }

        let mut body: Vec<u8> = params[1].to_vec();
        for extra in &params[2..] {
            body.push(b';');
            body.extend_from_slice(extra);
        }
        let Some(rest) = strip_prefix_ascii_ci(&body, b"File=") else {
            return;
        };
        let Some(colon) = rest.iter().position(|&b| b == b':') else {
            return;
        };
        self.handle_osc_1337_file(&rest[..colon], &rest[colon + 1..]);
    }

    /// Parse and place one `File=` body, pre-split at the first `:` into `args`
    /// and the base64 `payload`.
    fn handle_osc_1337_file(&mut self, args_bytes: &[u8], payload: &[u8]) {
        let args = parse_file_args(args_bytes);
        if !args.inline {
            return;
        }

        let Ok(payload_str) = std::str::from_utf8(payload) else {
            return;
        };
        let cleaned: std::borrow::Cow<'_, str> = if payload.iter().any(u8::is_ascii_whitespace) {
            let mut s = String::with_capacity(payload_str.len());
            s.extend(payload_str.chars().filter(|c| !c.is_ascii_whitespace()));
            std::borrow::Cow::Owned(s)
        } else {
            std::borrow::Cow::Borrowed(payload_str)
        };
        let Ok(bytes) = aterm_codec::base64::decode(&cleaned) else {
            return;
        };
        if bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES {
            return;
        }

        let format = detect_format(&bytes);
        let (px_w, px_h) = png_dimensions(&bytes).unwrap_or((0, 0));

        let (cell_w, cell_h) = self.iterm2.cell_px;
        let (cell_w, cell_h) = (cell_w.max(1), cell_h.max(1));
        let grid_cols = self.grid.cols();
        let grid_rows = self.grid.rows();

        let auto_cols = if px_w == 0 {
            1
        } else {
            clamp_cells(px_w.div_ceil(u32::from(cell_w))).max(1)
        };
        let auto_rows = if px_h == 0 {
            1
        } else {
            clamp_cells(px_h.div_ceil(u32::from(cell_h))).max(1)
        };

        let tallest = grid_rows.max(clamp_cells(
            MAX_DECODED_IMAGE_PX.div_ceil(u32::from(cell_h)),
        ));

        let want_w = args.width.unwrap_or(Dimension::Auto);
        let want_h = args.height.unwrap_or(Dimension::Auto);
        let mut cols = want_w.to_cells(cell_w, grid_cols, auto_cols).max(1);
        let mut rows = want_h
            .to_cells(cell_h, grid_rows, auto_rows)
            .min(tallest)
            .max(1);

        if args.preserve_aspect_ratio && px_w > 0 && px_h > 0 {
            match (want_w, want_h) {
                (Dimension::Auto, _) if !matches!(want_h, Dimension::Auto) => {
                    let px =
                        u64::from(rows) * u64::from(cell_h) * u64::from(px_w) / u64::from(px_h);
                    let cells = u32::try_from(px.div_ceil(u64::from(cell_w))).unwrap_or(u32::MAX);
                    cols = clamp_cells(cells).max(1);
                }
                (_, Dimension::Auto) if !matches!(want_w, Dimension::Auto) => {
                    let px =
                        u64::from(cols) * u64::from(cell_w) * u64::from(px_h) / u64::from(px_w);
                    let cells = u32::try_from(px.div_ceil(u64::from(cell_h))).unwrap_or(u32::MAX);
                    rows = clamp_cells(cells).min(tallest).max(1);
                }
                _ => {}
            }
        }

        cols = cols.min(grid_cols);
        if cols == 0 || rows == 0 {
            return;
        }

        let image = Arc::new(ImageData {
            bytes,
            format,
            cols,
            rows,
            z_index: 0,
            band_lift_px: 0,
            cell_offset_px: (0, 0),
            scaling: if args.preserve_aspect_ratio {
                ImageScaling::Fit
            } else {
                ImageScaling::Stretch
            },
            source_rect: None,
        });
        self.place_image(&image, cols, rows, 0);
    }

    /// Stamp `image` onto a `rows`×`cols` block starting at `start_col` and advance to the next line.
    #[inline]
    pub(super) fn place_image(
        &mut self,
        image: &Arc<ImageData>,
        cols: u16,
        rows: u16,
        start_col: u16,
    ) {
        self.place_image_as(
            image,
            cols,
            rows,
            start_col,
            None,
            PlacementCursor::NextLine,
        );
    }

    /// Stamp `image` with an optional Kitty placement tag and cursor policy.
    pub(super) fn place_image_as(
        &mut self,
        image: &Arc<ImageData>,
        cols: u16,
        rows: u16,
        start_col: u16,
        kitty: Option<KittyPlacementTag>,
        cursor: PlacementCursor,
    ) {
        self.admit_live_image(image, kitty);
        let start_col = start_col.min(self.grid.cols().saturating_sub(1));
        if cursor == PlacementCursor::Unmoved {
            let top = self.grid.cursor_row();
            let visible = rows.min(self.grid.rows().saturating_sub(top));
            for cell_row in 0..visible {
                self.stamp_image_row(image, top + cell_row, cell_row, start_col, cols, kitty);
            }
            return;
        }
        self.grid.set_cursor(self.grid.cursor_row(), start_col);

        let grid_rows = self.grid.rows();
        let skip_end = rows.saturating_sub(grid_rows.saturating_mul(2));

        for cell_row in 0..rows {
            let row = self.grid.cursor_row();
            if cell_row < grid_rows || cell_row >= skip_end {
                self.stamp_image_row(image, row, cell_row, start_col, cols, kitty);
            }
            if cell_row + 1 < rows {
                self.grid.line_feed();
                self.grid.set_cursor(self.grid.cursor_row(), start_col);
            }
        }
        let end_col = start_col.saturating_add(cols);
        if cursor == PlacementCursor::AfterImage && end_col < self.grid.cols() {
            // Kitty: continue right after the image, on its last row.
            self.grid.set_cursor(self.grid.cursor_row(), end_col);
        } else {
            self.grid.line_feed();
            self.grid.set_cursor(self.grid.cursor_row(), 0);
        }
    }

    /// Trim older live-screen images if needed and return whether `image` fits [`MAX_LIVE_IMAGE_BYTES`].
    pub(super) fn admit_live_image(
        &mut self,
        image: &Arc<ImageData>,
        kitty: Option<KittyPlacementTag>,
    ) {
        let add = if kitty.is_some_and(|tag| self.kitty_store_holds(tag.image_id, image)) {
            0
        } else {
            image.payload_bytes()
        };
        let screen = usize::from(self.modes.alternate_screen);
        let charge = self.transient.live_image_charge[screen].saturating_add(add);
        self.transient.live_image_charge[screen] = if charge > MAX_LIVE_IMAGE_BYTES {
            self.trim_live_images(image).saturating_add(add)
        } else {
            charge
        };
    }

    /// Count the payload bytes of the images the live screen shows (each
    /// once, the Kitty store's excepted) and, past [`LIVE_IMAGE_TRIM_BYTES`],
    /// take whole images off the screen — the one that first shows topmost,
    /// then leftmost, first: output placed it there earliest.
    fn trim_live_images(&mut self, keep: &Arc<ImageData>) -> usize {
        let stored = self.kitty_store_images();
        let grid: &aterm_grid::Grid = self.grid;
        let (rows, cols) = (grid.rows(), grid.cols());
        let mut index: HashMap<*const ImageData, usize> = HashMap::new();
        let mut shown: Vec<((u16, u16, usize), *const ImageData, usize)> = Vec::new();
        for (at, extra) in grid.extras().iter() {
            if at.row >= rows || at.col >= cols {
                continue;
            }
            for (depth, layer) in extra.images().iter().enumerate() {
                let image = Arc::as_ptr(&layer.image);
                if stored.contains(&image) {
                    continue;
                }
                let first = (at.row, at.col, depth);
                match index.get(&image) {
                    Some(&i) => shown[i].0 = shown[i].0.min(first),
                    None => {
                        index.insert(image, shown.len());
                        shown.push((first, image, layer.image.payload_bytes()));
                    }
                }
            }
        }
        let mut total = shown
            .iter()
            .fold(0usize, |sum, &(_, _, bytes)| sum.saturating_add(bytes));
        if total <= LIVE_IMAGE_TRIM_BYTES {
            return total;
        }
        shown.sort_unstable_by_key(|&(first, ..)| first);
        let keep = Arc::as_ptr(keep);
        let mut gone: HashSet<*const ImageData> = HashSet::new();
        for &(_, image, bytes) in &shown {
            if total <= LIVE_IMAGE_TRIM_BYTES {
                break;
            }
            if image != keep {
                gone.insert(image);
                total = total.saturating_sub(bytes);
            }
        }
        let cells: Vec<(u16, u16)> = grid
            .extras()
            .iter()
            .filter(|(at, extra)| {
                at.row < rows
                    && at.col < cols
                    && extra
                        .images()
                        .iter()
                        .any(|layer| gone.contains(&Arc::as_ptr(&layer.image)))
            })
            .map(|(at, _)| (at.row, at.col))
            .collect();
        for (row, col) in cells {
            self.grid
                .cell_extra_mut(row, col)
                .retain_images(|layer| !gone.contains(&Arc::as_ptr(&layer.image)));
            self.grid.damage_mut().mark_cell(row, col);
        }
        total
    }

    /// Stamp footprint row `cell_row` of `image` onto grid row `row`, columns
    /// `start_col..start_col + cols` (clipped at the right edge), damaging each
    /// stamped cell.
    fn stamp_image_row(
        &mut self,
        image: &Arc<ImageData>,
        row: u16,
        cell_row: u16,
        start_col: u16,
        cols: u16,
        kitty: Option<KittyPlacementTag>,
    ) {
        for cell_col in 0..cols {
            let col = start_col.saturating_add(cell_col);
            if col >= self.grid.cols() {
                break;
            }
            let layer = ImageRef {
                image: Arc::clone(image),
                cell_row,
                cell_col,
                kitty,
            };
            if kitty.is_some() {
                self.grid.stack_cell_image(row, col, layer);
            } else {
                self.grid.set_cell_image(row, col, layer);
            }
            self.grid.damage_mut().mark_cell(row, col);
        }
    }

    /// Place a decoded sixel raster as an [`ImageFormat::RawRgba8`] [`ImageData`].
    pub(super) fn place_sixel_image(&mut self, image: &crate::sixel::SixelImage) {
        let (cell_w, cell_h) = self.iterm2.cell_px;
        let (cell_w, cell_h) = (cell_w.max(1), cell_h.max(1));
        let grid_cols = self.grid.cols();

        let cols = clamp_cells(clamp_u32(image.cols_spanned(cell_w)))
            .max(1)
            .min(grid_cols);
        let rows = clamp_cells(clamp_u32(image.rows_spanned(cell_h))).max(1);
        if cols == 0 || rows == 0 {
            return;
        }

        let raw_w = u16::try_from(image.width()).unwrap_or(u16::MAX);
        let raw_h = u16::try_from(image.height()).unwrap_or(u16::MAX);

        let image_bytes = (image.width() as u64)
            .saturating_mul(image.height() as u64)
            .saturating_mul(4);
        if image_bytes > MAX_IMAGE_BYTES as u64 {
            return;
        }

        let mut bytes: Vec<u8> = Vec::with_capacity(image.pixels().len() * 4);
        for &px in image.pixels() {
            let [a, r, g, b] = px.to_be_bytes();
            bytes.extend_from_slice(&[r, g, b, a]);
        }

        let image_data = Arc::new(ImageData {
            bytes,
            format: ImageFormat::RawRgba8 {
                width: raw_w,
                height: raw_h,
            },
            cols,
            rows,
            z_index: 0,
            band_lift_px: 0,
            cell_offset_px: (0, 0),
            scaling: ImageScaling::PixelExact,
            source_rect: None,
        });

        let anchor_col = self.grid.cursor_col();

        let save_row = self.grid.cursor_row();
        self.place_image(&image_data, cols, rows, anchor_col);
        if self.modes.sixel_display_mode {
            // Display mode (DECSDM set): restore the cursor `place_image` moved.
            self.grid.set_cursor(save_row, anchor_col);
        }
    }
}

/// Parse the `File=` argument list (`key=value;key=value;…`).
fn parse_file_args(args: &[u8]) -> FileArgs {
    let mut out = FileArgs {
        preserve_aspect_ratio: true,
        ..FileArgs::default()
    };
    let s = String::from_utf8_lossy(args);
    for pair in s.split(';') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        if key.eq_ignore_ascii_case("width") {
            out.width = Some(Dimension::parse(value));
        } else if key.eq_ignore_ascii_case("height") {
            out.height = Some(Dimension::parse(value));
        } else if key.eq_ignore_ascii_case("inline") {
            out.inline = value == "1";
        } else if key.eq_ignore_ascii_case("preserveAspectRatio") {
            // Default on; only an explicit "0" disables it.
            out.preserve_aspect_ratio = value != "0";
        }
    }
    out
}

/// Case-insensitively strip an ASCII prefix, returning the remainder.
#[inline]
fn strip_prefix_ascii_ci<'a>(haystack: &'a [u8], prefix: &[u8]) -> Option<&'a [u8]> {
    if haystack.len() >= prefix.len() && haystack[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&haystack[prefix.len()..])
    } else {
        None
    }
}

/// Classify the raw payload by magic bytes. Only PNG is renderable.
#[inline]
const fn detect_format(bytes: &[u8]) -> ImageFormat {
    if matches!(bytes, [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, ..]) {
        ImageFormat::Png
    } else {
        ImageFormat::Unknown
    }
}

/// A PNG's pixel dimensions from the IHDR chunk (the big-endian u32s at byte
/// offset 16), without decoding.
#[inline]
const fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    // 8-byte signature + 4-byte length + "IHDR" (4) → width@16, height@20.
    if bytes.len() < 24 || !matches!(bytes, [0x89, b'P', b'N', b'G', ..]) {
        return None;
    }
    let w = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let h = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    if w == 0 || h == 0 {
        return None;
    }
    Some((w, h))
}

#[cfg(test)]
#[path = "handler_osc_1337_tests.rs"]
mod tests;
