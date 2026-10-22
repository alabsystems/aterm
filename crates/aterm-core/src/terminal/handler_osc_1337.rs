// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! iTerm2 OSC 1337 `File=` inline images
//! (`ESC ] 1337 ; File = key=value ; … : <base64 payload> BEL`), plus the
//! cell-stamping shared with sixel and Kitty placements.
//!
//! The engine does not decode the picture: it computes the cell footprint and
//! stamps an [`ImageRef`](aterm_grid::ImageRef) on every covered cell, and the
//! raw bytes ride behind an `Arc` for the renderer to decode once and tile.
//! Other OSC 1337 sub-commands are ignored here.
//!
//! Malformed input never panics and never writes a cell. A payload that is not
//! PNG is still stored so the cursor advances consistently; the renderer draws
//! nothing for it.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use aterm_grid::{ImageData, ImageFormat, ImageRef, ImageScaling, KittyPlacementTag};

use super::handler::TerminalHandler;

/// Where the cursor ends up after an inline image is placed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PlacementCursor {
    /// iTerm2 and sixel: a fresh line at column 0 below the image, so
    /// following output never overprints its last row.
    NextLine,
    /// Kitty's default: the cell just past the image's right edge on its last
    /// row (the next line's column 0 when that is past the right margin) —
    /// kitty moves the cursor `c` columns right and `r - 1` rows down.
    AfterImage,
    /// Kitty `C=1`: the cursor does not move and nothing scrolls; footprint rows
    /// below the screen's last row are clipped.
    Unmoved,
}

/// Maximum decoded image payload accepted (bytes). A single sequence must not be
/// able to pin an unbounded buffer; 16 MiB comfortably covers a full-screen PNG
/// while bounding the worst case.
const MAX_IMAGE_BYTES: usize = 16 * 1024 * 1024;

/// Maximum footprint in cells along either axis. Caps the per-image cell-stamp
/// work regardless of the requested dimensions; the footprint's height is held
/// tighter still ([`MAX_DECODED_IMAGE_PX`]).
const MAX_FOOTPRINT_CELLS: u16 = 4096;

/// The tallest raster the image paths make or read, in pixels: the renderers'
/// PNG decode limit (`aterm-render`'s `IMAGE_MAX_DIMENSION`) and the sixel
/// decoder's `SIXEL_MAX_DIMENSION`.
///
/// The renderers decode a placement into one RGBA buffer of its footprint's
/// pixel size, and an iTerm2 `height=` in cells is otherwise bounded only by
/// [`MAX_FOOTPRINT_CELLS`] (`height=4096` on a one-pixel PNG would ask each draw
/// for 262 MB on an 80-column grid of 10x20 px cells). So an iTerm2 footprint is
/// no taller than the screen or this, whichever is more.
const MAX_DECODED_IMAGE_PX: u32 = 4096;

/// Payload bytes of the inline images the live screen may show at once, past
/// which whole images are taken off it ([`TerminalHandler::admit_live_image`]).
///
/// A cell keeps its image while any of it shows, so without this a screen's
/// pictures are bounded only by cell count times the per-image cap: an 80x24
/// screen of sixels each declaring a 4096x20 raster (no pixel data) holds
/// 603 MB after 65 KB of output. Scrolled-off images are charged to the
/// scrollback budget. Generous next to any real screen: a full 6K window of
/// RGBA is 81 MB.
pub(super) const MAX_LIVE_IMAGE_BYTES: usize = 128 * 1024 * 1024;

/// What an over-budget screen is trimmed to: half the budget, so each count of
/// the screen's images is at least that many placed bytes after the last and
/// the counting stays linear in the bytes placed.
const LIVE_IMAGE_TRIM_BYTES: usize = MAX_LIVE_IMAGE_BYTES / 2;

/// A requested image dimension from the `width=`/`height=` argument.
///
/// iTerm2 accepts: a bare count = cells, an `N px` suffix = pixels, an `N%`
/// suffix = a percentage of the viewport, or `auto` = derive from the image's own
/// pixel size. Anything unparseable is treated as `Auto`.
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
    /// Parse a `width=`/`height=` value. Returns [`Dimension::Auto`] for `auto`,
    /// an empty value, or anything that does not parse — never errors.
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

    /// Resolve to a CELL count. `cell_px` is the per-axis cell pixel size,
    /// `viewport_cells` the grid extent on this axis, `auto_cells` the footprint
    /// derived from the image's own pixel size. Always returns at least 1 (a
    /// placed image occupies space) when the inputs are non-degenerate.
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
/// The clamp guarantees the value fits `u16`, so the narrowing never truncates.
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

/// Narrow a `usize` cell span to `u32`, saturating. Used by the sixel placement
/// path so a span (already bounded by the decoder's dimension cap) feeds
/// `clamp_cells` without a truncating cast.
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
    /// `inline=1` — display inline. `inline=0` (or absent) means "save to disk",
    /// which a terminal cannot do, so only `inline=1` paints anything.
    inline: bool,
    /// `preserveAspectRatio=0` disables aspect preservation (default on).
    preserve_aspect_ratio: bool,
}

impl TerminalHandler<'_> {
    /// Handle OSC 1337, the iTerm2 `File=` inline-image sub-command; any other
    /// body is ignored.
    ///
    /// The parser splits the body on every `;`; it divides into `(args,
    /// payload)` at the first `:`. Base64 has no `;`, so a well-formed payload is
    /// entirely in the last param and borrowed in place, and only the small args
    /// prefix is rejoined: no copy of a payload of up to `MAX_OSC_DATA` under the
    /// terminal lock. A `:` in an earlier param (malformed) rejoins the whole
    /// body so acceptance matches the unsplit form.
    pub(super) fn handle_osc_1337(&mut self, params: &[&[u8]]) {
        if params.len() < 2 {
            return;
        }

        // Locate the first ':' across the split params (param index, byte offset).
        let colon_at = params
            .iter()
            .enumerate()
            .skip(1)
            .find_map(|(i, p)| p.iter().position(|&b| b == b':').map(|j| (i, j)));
        // No ':' anywhere means no payload: nothing to draw.
        let Some((colon_param, colon_off)) = colon_at else {
            return;
        };

        // Fast path: the ':' is in the last param, so the payload is borrowed.
        if colon_param == params.len() - 1 {
            let payload = &params[colon_param][colon_off + 1..];
            // Single-param body: `File=args` sits before the ':' in the same
            // param — borrow it too; nothing is copied at all.
            if colon_param == 1 {
                let Some(args_bytes) = strip_prefix_ascii_ci(&params[1][..colon_off], b"File=")
                else {
                    return;
                };
                self.handle_osc_1337_file(args_bytes, payload);
                return;
            }
            // Rejoin `File=` + the arg list (everything before the ':').
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

        // Malformed shape (a ';' after the first ':'): rebuild the full body, so
        // the ';'-bearing payload fails base64 decode as the unsplit form would.
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
    /// and the base64 `payload`. Malformed input is ignored.
    fn handle_osc_1337_file(&mut self, args_bytes: &[u8], payload: &[u8]) {
        let args = parse_file_args(args_bytes);
        // inline=0 (or absent) means "download", which has no on-screen effect.
        if !args.inline {
            return;
        }

        let Ok(payload_str) = std::str::from_utf8(payload) else {
            return;
        };
        // Strip ASCII whitespace (line-wrapped payloads) for the strict decoder,
        // copying only when some is present.
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
        // Pixel dimensions from the (PNG) header, used for aspect + Auto sizing.
        let (px_w, px_h) = png_dimensions(&bytes).unwrap_or((0, 0));

        let (cell_w, cell_h) = self.iterm2.cell_px;
        let (cell_w, cell_h) = (cell_w.max(1), cell_h.max(1));
        let grid_cols = self.grid.cols();
        let grid_rows = self.grid.rows();

        // Auto footprint from the image's own pixels, rounded up so the whole
        // image fits. Clamp to a sane default when the header is unreadable.
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

        // The tallest footprint: the screen, or the tallest decodable raster
        // ([`MAX_DECODED_IMAGE_PX`]), whichever is more.
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

        // Preserve aspect ratio when one axis is Auto and the other explicit: scale
        // the Auto axis from the image's pixel aspect so the picture is not warped.
        if args.preserve_aspect_ratio && px_w > 0 && px_h > 0 {
            match (want_w, want_h) {
                (Dimension::Auto, _) if !matches!(want_h, Dimension::Auto) => {
                    // Height fixed: derive width from the aspect ratio. IHDR px_w is
                    // an attacker-controlled u32, so the product is computed in u64
                    // and the cell count saturated into clamp_cells' u32 domain.
                    let px =
                        u64::from(rows) * u64::from(cell_h) * u64::from(px_w) / u64::from(px_h);
                    let cells = u32::try_from(px.div_ceil(u64::from(cell_w))).unwrap_or(u32::MAX);
                    cols = clamp_cells(cells).max(1);
                }
                (_, Dimension::Auto) if !matches!(want_w, Dimension::Auto) => {
                    // width fixed → derive height from aspect.
                    let px =
                        u64::from(cols) * u64::from(cell_w) * u64::from(px_h) / u64::from(px_w);
                    let cells = u32::try_from(px.div_ceil(u64::from(cell_h))).unwrap_or(u32::MAX);
                    rows = clamp_cells(cells).min(tallest).max(1);
                }
                _ => {}
            }
        }

        // A footprint wider than the grid is clamped to the grid width (anchored at
        // the left margin, as iTerm2 does); a zero footprint draws nothing.
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
            // iTerm2 names its target in cells, so the image fits the requested
            // cells (unlike sixel below), keeping the aspect ratio unless
            // `preserveAspectRatio=0` asks to stretch.
            scaling: if args.preserve_aspect_ratio {
                ImageScaling::Fit
            } else {
                ImageScaling::Stretch
            },
            source_rect: None,
        });
        // iTerm2 inline images are left-anchored at the margin (column 0).
        self.place_image(&image, cols, rows, 0);
    }

    /// Stamp `image` onto a `rows`×`cols` block of cells starting at
    /// `start_col`, line-feeding once per footprint row so later output lands
    /// below the image and a tall image scrolls the grid as text would.
    ///
    /// `start_col` is the anchor: 0 for iTerm2 OSC 1337 (left margin, per spec),
    /// the cursor column for sixel (VT340/xterm). Cells past the right edge are
    /// clipped, not wrapped.
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

    /// [`place_image`](Self::place_image) with the Kitty placement tag every
    /// covered cell carries (`None` for iTerm2 and sixel) and the protocol's
    /// cursor policy.
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
            // Kitty `C=1`: stamp in place below the cursor row without any line
            // feed; cursor and scroll position stay.
            let top = self.grid.cursor_row();
            let visible = rows.min(self.grid.rows().saturating_sub(top));
            for cell_row in 0..visible {
                self.stamp_image_row(image, top + cell_row, cell_row, start_col, cols, kitty);
            }
            return;
        }
        // Anchor at `start_col`; the stamp loop restores this column after every
        // line_feed so footprint tiles map onto whole columns from the anchor.
        self.grid.set_cursor(self.grid.cursor_row(), start_col);

        // Middle-band footprint rows are never the final writer of a visible
        // cell: after `grid_rows` line feeds the cursor is pinned, so only the
        // first `grid_rows` rows and the last `2 * grid_rows` can remain. Skipping
        // the rest bounds a hostile `height=4096` to ~3*grid_rows*cols inserts
        // with an identical final state; the line feeds still run per row.
        let grid_rows = self.grid.rows();
        let skip_end = rows.saturating_sub(grid_rows.saturating_mul(2));

        for cell_row in 0..rows {
            let row = self.grid.cursor_row();
            if cell_row < grid_rows || cell_row >= skip_end {
                self.stamp_image_row(image, row, cell_row, start_col, cols, kitty);
            }
            // Line-feed to the next image row except after the last, leaving the
            // cursor on the final row's start as text flow would.
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
            // After the image move to a fresh line at column 0 so later text does
            // not overprint it (a Kitty image reaching the right margin wraps
            // there, as in kitty).
            self.grid.line_feed();
            self.grid.set_cursor(self.grid.cursor_row(), 0);
        }
    }

    /// Make room on the live screen for `image` (Kitty placement `kitty`, if
    /// one) so the screen's images stay within [`MAX_LIVE_IMAGE_BYTES`]; every
    /// stamp of every protocol comes through here.
    ///
    /// Each screen's `live_image_charge` is an upper bound on its image bytes:
    /// its last count plus everything stamped on it, or let go by the Kitty
    /// store, since. Only when the bound passes the budget are the images
    /// counted and trimmed ([`trim_live_images`](Self::trim_live_images)), so a
    /// count runs once per half-budget charged. An image the Kitty store holds
    /// is bounded by the store's budget and charged nothing until the store lets
    /// it go, then to both screens, as either grid may still show it.
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
    /// then leftmost, first: output placed it there earliest. `keep` (the image
    /// being placed) stays. Returns the bytes left.
    fn trim_live_images(&mut self, keep: &Arc<ImageData>) -> usize {
        let stored = self.kitty_store_images();
        let grid: &aterm_grid::Grid = self.grid;
        let (rows, cols) = (grid.rows(), grid.cols());
        // Each image once: the first layer showing it, row-major, and its bytes.
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
    /// stamped cell. A Kitty placement (`kitty` given) stacks over the images a
    /// cell already shows; any other image replaces them.
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
            // Use the grid-level setters, not `cell_extra_mut(..)`: they also arm
            // the extras' scroll-off image gate, without which the picture is not
            // carried into history when its row scrolls off.
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

    /// Place a decoded sixel raster through [`place_image`](Self::place_image)
    /// as an [`ImageFormat::RawRgba8`] [`ImageData`]. With DECSDM (mode 80)
    /// reset the cursor ends on the line below the image; with it set the cursor
    /// is restored.
    pub(super) fn place_sixel_image(&mut self, image: &crate::sixel::SixelImage) {
        let (cell_w, cell_h) = self.iterm2.cell_px;
        let (cell_w, cell_h) = (cell_w.max(1), cell_h.max(1));
        let grid_cols = self.grid.cols();

        // Footprint: the raster rounded up to whole cells, clamped to the grid
        // width and the per-image cell cap. Spans are bounded by
        // SIXEL_MAX_DIMENSION, so the `u32` conversion never truncates.
        let cols = clamp_cells(clamp_u32(image.cols_spanned(cell_w)))
            .max(1)
            .min(grid_cols);
        let rows = clamp_cells(clamp_u32(image.rows_spanned(cell_h))).max(1);
        if cols == 0 || rows == 0 {
            return;
        }

        // The decoder clamps both axes to SIXEL_MAX_DIMENSION (4096), so the
        // u16 narrowing here never truncates a real raster.
        let raw_w = u16::try_from(image.width()).unwrap_or(u16::MAX);
        let raw_h = u16::try_from(image.height()).unwrap_or(u16::MAX);

        // Bound the RGBA allocation by `MAX_IMAGE_BYTES`: a DECGRA-only sixel
        // defers `apply_raster` to `unhook`, past the DCS budget in
        // `dcs_put_inner`. Computed in u64 (4096*4096*4 overflows u32).
        let image_bytes = (image.width() as u64)
            .saturating_mul(image.height() as u64)
            .saturating_mul(4);
        if image_bytes > MAX_IMAGE_BYTES as u64 {
            return;
        }

        // Unpack packed 0xAARRGGBB u32s into RGBA8 bytes [r, g, b, a] — the
        // layout `resample_rgba`/`blit_image_cell` consume.
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
            // Sixel names pixels: drawn 1:1 from the footprint's top-left, not
            // scaled to fill it (filling would magnify and blur it), as on other
            // sixel terminals.
            scaling: ImageScaling::PixelExact,
            source_rect: None,
        });

        // Sixel anchors at the current cursor column (VT340/xterm), not the left
        // margin, so a mid-line sixel does not overprint cells to its left.
        let anchor_col = self.grid.cursor_col();

        let save_row = self.grid.cursor_row();
        self.place_image(&image_data, cols, rows, anchor_col);
        if self.modes.sixel_display_mode {
            // Display mode (DECSDM set): restore the cursor `place_image` moved.
            self.grid.set_cursor(save_row, anchor_col);
        }
    }
}

/// Parse the `File=` argument list (`key=value;key=value;…`). Unknown keys are
/// ignored; a missing/duplicate key takes the last value. Never errors.
fn parse_file_args(args: &[u8]) -> FileArgs {
    let mut out = FileArgs {
        // preserveAspectRatio defaults on per the iTerm2 spec.
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
        // name=, size= and any other key are accepted but unused for placement.
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
/// offset 16), without decoding. `None` for a non-PNG or truncated header.
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
