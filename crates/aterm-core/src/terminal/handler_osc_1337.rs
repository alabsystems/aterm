// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! iTerm2 OSC 1337 `File=` inline-image handler.
//!
//! `imgcat` and friends emit
//!
//! ```text
//! ESC ] 1337 ; File = key=value ; key=value ; … : <base64 payload> BEL
//! ```
//!
//! to draw an image inline over the grid. This module parses that one sub-command
//! (`File=`), base64-decodes the payload, computes the image's CELL footprint, and
//! stamps an [`ImageRef`](aterm_grid::ImageRef) onto every covered cell. The
//! engine does NOT decode pixels — it has no image codec; the raw bytes ride along
//! behind an `Arc` and the renderer decodes once (keyed by `Arc` identity) and
//! blits each cell's tile. See `aterm-render`'s image pass.
//!
//! Every other OSC 1337 sub-command (`SetUserVar`, `SetMark`, …) is handled via
//! the shell-API / KVP callback layer, not here. A param that is not `File=` is
//! left for those paths (silently ignored in the default lib build).
//!
//! ## Robustness
//!
//! Malformed input never panics and never writes a cell: a missing payload, an
//! undecodable base64 body, a zero footprint, or a footprint that does not fit the
//! grid all return early. An unknown image format (not PNG) is still stored so the
//! cursor advances consistently, but the renderer draws nothing for it.

use std::sync::Arc;

use aterm_grid::{ImageData, ImageFormat, ImageRef};

use super::handler::TerminalHandler;

/// Maximum decoded image payload accepted (bytes). A single sequence must not be
/// able to pin an unbounded buffer; 16 MiB comfortably covers a full-screen PNG
/// while bounding the worst case.
const MAX_IMAGE_BYTES: usize = 16 * 1024 * 1024;

/// Maximum footprint in cells along either axis. Caps the per-image cell-stamp
/// work and the renderer's blit area regardless of the requested dimensions.
const MAX_FOOTPRINT_CELLS: u16 = 4096;

/// A requested image dimension from the `width=`/`height=` argument.
///
/// iTerm2 accepts: a bare count = CELLS, an `N px` suffix = PIXELS, an `N%`
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
    fn to_cells(self, cell_px: u16, viewport_cells: u16, auto_cells: u16) -> u16 {
        let cells = match self {
            Dimension::Cells(c) => c,
            Dimension::Pixels(px) => px.div_ceil(u32::from(cell_px.max(1))),
            Dimension::Percent(p) => (u32::from(viewport_cells).saturating_mul(p) / 100).max(1),
            Dimension::Auto => u32::from(auto_cells),
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
fn clamp_cells(cells: u32) -> u16 {
    cells.min(u32::from(MAX_FOOTPRINT_CELLS)) as u16
}

/// Narrow a `usize` cell span to `u32`, saturating. Used by the sixel placement
/// path so a span (already bounded by the decoder's dimension cap) feeds
/// `clamp_cells` without a truncating cast.
#[cfg(feature = "sixel")]
fn clamp_u32(v: usize) -> u32 {
    u32::try_from(v).unwrap_or(u32::MAX)
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
    /// Handle OSC 1337 — the iTerm2 `File=` inline-image sub-command.
    ///
    /// `params` is the VTE-split OSC: `params[0] == "1337"`, and the rest is the
    /// `File=…:base64` body, which the parser splits on every `;` (the `File=`
    /// arg list is semicolon-separated) and on no other delimiter.
    ///
    /// The body is dispatched as `(args, payload)` split at the FIRST `:`. The
    /// base64 alphabet contains no `;`, so for well-formed input the whole
    /// payload sits inside the LAST split param and is borrowed in place — only
    /// the small args prefix is ever rejoined. A `:` in an earlier param means a
    /// `;` follows the payload start (malformed base64); that case falls back to
    /// a full rejoin so accept/reject behavior matches the split-free path
    /// byte for byte. This avoids copying a near-8-MiB payload (the parser's
    /// `MAX_OSC_DATA` cap) inside one dispatch under the terminal lock.
    ///
    /// Any body that is not `File=` is left to the shell-API / KVP path (this is a
    /// no-op for it). All malformed `File=` input is ignored without panicking.
    pub(super) fn handle_osc_1337(&mut self, params: &[&[u8]]) {
        if params.len() < 2 {
            return;
        }

        // Locate the FIRST ':' across the split params (param index, byte offset).
        let colon_at = params
            .iter()
            .enumerate()
            .skip(1)
            .find_map(|(i, p)| p.iter().position(|&b| b == b':').map(|j| (i, j)));
        // No ':' anywhere means no payload — nothing to draw regardless of the
        // sub-command (the rejoined-body path returned without effect too).
        let Some((colon_param, colon_off)) = colon_at else {
            return;
        };

        // Fast path: the ':' lives in the LAST param, so the payload contains no
        // ';' and can be borrowed in place. Rejoin only the args prefix.
        if colon_param == params.len() - 1 {
            let payload = &params[colon_param][colon_off + 1..];
            // Single-param body: `File=args` sits before the ':' in the same
            // param — borrow it too; nothing is copied at all.
            if colon_param == 1 {
                // Only the File= sub-command is handled here. Everything else
                // (SetUserVar, SetMark, …) flows through the shell-API/KVP layer.
                let Some(args_bytes) = strip_prefix_ascii_ci(&params[1][..colon_off], b"File=")
                else {
                    return;
                };
                self.handle_osc_1337_file(args_bytes, payload);
                return;
            }
            // Rejoin everything before the ':' — `File=` + the arg list. This is
            // the small (non-payload) prefix of the body.
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

        // Malformed shape (a ';' after the first ':'): reconstruct the full body
        // exactly as before, so downstream accept/reject behavior is unchanged
        // (the ';'-bearing payload then fails base64 decode as it always did).
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

    /// Parse + place one `File=` body, pre-split at the first `:` into the
    /// `args` list and the base64 `payload`. Ignored without effect on any
    /// malformed input.
    fn handle_osc_1337_file(&mut self, args_bytes: &[u8], payload: &[u8]) {
        let args = parse_file_args(args_bytes);
        // inline=0 (or absent) means "download", which has no on-screen effect.
        if !args.inline {
            return;
        }

        // Base64 → raw image bytes. A decode failure degrades to nothing.
        let payload_str = match std::str::from_utf8(payload) {
            Ok(s) => s,
            Err(_) => return,
        };
        // The payload may carry incidental whitespace/newlines from line-wrapping;
        // the decoder is strict, so strip ASCII whitespace first. `imgcat` and
        // friends emit none, so probe first and only copy when a strip is
        // actually needed (the byte probe equals the char filter: ASCII
        // whitespace is single-byte and non-ASCII UTF-8 bytes are >= 0x80).
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

        // Auto footprint from the image's own pixels, rounded UP so the whole
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

        let want_w = args.width.unwrap_or(Dimension::Auto);
        let want_h = args.height.unwrap_or(Dimension::Auto);
        let mut cols = want_w.to_cells(cell_w, grid_cols, auto_cols).max(1);
        let mut rows = want_h.to_cells(cell_h, grid_rows, auto_rows).max(1);

        // Preserve aspect ratio when one axis is Auto and the other explicit: scale
        // the Auto axis from the image's pixel aspect so the picture is not warped.
        if args.preserve_aspect_ratio && px_w > 0 && px_h > 0 {
            match (want_w, want_h) {
                (Dimension::Auto, _) if !matches!(want_h, Dimension::Auto) => {
                    // height fixed → derive width from aspect (in pixels, then cells).
                    // IHDR px_w is an attacker-controlled u32 (up to u32::MAX), so the
                    // rows*cell_h*px_w product overflows u32 — accumulate in u64 and
                    // saturate the cell count back into clamp_cells' u32 domain.
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
                    rows = clamp_cells(cells).max(1);
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
        });
        // iTerm2 inline images are LEFT-anchored at the margin (column 0).
        self.place_image(&image, cols, rows, 0);
    }

    /// Stamp `image` onto a `rows`×`cols` block of cells starting at `start_col`,
    /// advancing the cursor down one line per footprint row so following output
    /// lands BELOW the image. Each footprint row scrolls via `line_feed`, so an
    /// image taller than the remaining screen scrolls the grid exactly as text
    /// would.
    ///
    /// `start_col` is the anchoring column: the iTerm2 OSC 1337 path passes `0`
    /// (left-anchored at the margin, per spec); the sixel path passes the CURRENT
    /// cursor column so a mid-line sixel paints at the cursor instead of snapping
    /// to column 0 (VT340/xterm). Cells whose column would fall past the right
    /// edge are clipped (not wrapped), so a wide image anchored near the margin is
    /// truncated rather than overprinting the next row.
    pub(super) fn place_image(
        &mut self,
        image: &Arc<ImageData>,
        cols: u16,
        rows: u16,
        start_col: u16,
    ) {
        // Anchor at `start_col`; the per-row stamp loop below restores this column
        // after every line_feed so the footprint tiles map cleanly onto whole
        // columns from the anchor.
        let start_col = start_col.min(self.grid.cols().saturating_sub(1));
        self.grid.set_cursor(self.grid.cursor_row(), start_col);

        // Footprint rows in the middle band can never be the FINAL writer of any
        // visible cell, so their per-cell stamps are skipped: after at most
        // `grid_rows` line feeds the cursor is pinned (at the scroll-region
        // bottom, scrolling, or at the last screen row, stationary). A row
        // stamped at a pinned-scrolling cursor leaves the region after at most
        // `grid_rows` further scrolls; at a pinned-stationary cursor it is
        // overwritten identically-shaped by every later footprint row. Only the
        // first `grid_rows` rows (stamped while the cursor may still be
        // descending — possibly OUTSIDE the scroll region, where nothing ever
        // scrolls or overwrites them) and the last `2 * grid_rows` rows can
        // remain visible. This bounds a hostile `height=4096` placement to
        // ~3*grid_rows*cols extras-map inserts instead of rows*cols (~1M on a
        // wide window) while keeping the final grid, cursor, scroll count, and
        // scrollback text identical; the line feeds still run per footprint row
        // (the ordinary optimized text-scroll cost).
        let grid_rows = self.grid.rows();
        let skip_end = rows.saturating_sub(grid_rows.saturating_mul(2));

        for cell_row in 0..rows {
            let row = self.grid.cursor_row();
            if cell_row < grid_rows || cell_row >= skip_end {
                for cell_col in 0..cols {
                    let col = start_col + cell_col;
                    if col >= self.grid.cols() {
                        break;
                    }
                    let extra = self.grid.cell_extra_mut(row, col);
                    extra.set_image(Some(ImageRef {
                        image: Arc::clone(image),
                        cell_row,
                        cell_col,
                    }));
                    self.grid.damage_mut().mark_cell(row, col);
                }
            }
            // Advance to the next image row (scrolling at the bottom), except after
            // the last row — leave the cursor on the final image row's start so the
            // next write continues right after, matching text flow.
            if cell_row + 1 < rows {
                self.grid.line_feed();
                self.grid.set_cursor(self.grid.cursor_row(), start_col);
            }
        }
        // After the image, move to a fresh line at column 0 so subsequent text does
        // not overprint the last image row.
        self.grid.line_feed();
        self.grid.set_cursor(self.grid.cursor_row(), 0);
    }

    /// Place a decoded sixel raster as an inline image, reusing the OSC 1337
    /// placement/blit path.
    ///
    /// The sixel decoder (`aterm-sixel`) has already materialized RGBA pixels —
    /// the engine carries no codec — so we wrap them in an [`ImageData`] tagged
    /// [`ImageFormat::RawRgba8`] and hand it to the same [`place_image`] the
    /// iTerm2 path uses. The footprint is computed from the iTerm2 cell metric
    /// exactly as `handle_osc_1337_file` does.
    ///
    /// Cursor behavior follows the sixel mode (DEC private mode 80, DECSDM):
    /// - **scrolling mode** (DECSDM reset, the default): `place_image` line-feeds
    ///   per footprint row and leaves the cursor on the line BELOW the image.
    /// - **display mode** (DECSDM set): the image is painted at the cursor and
    ///   the cursor is RESTORED to where it was, so it does not move.
    #[cfg(feature = "sixel")]
    pub(super) fn place_sixel_image(&mut self, image: &crate::sixel::SixelImage) {
        let (cell_w, cell_h) = self.iterm2.cell_px;
        let (cell_w, cell_h) = (cell_w.max(1), cell_h.max(1));
        let grid_cols = self.grid.cols();

        // Footprint in cells: the image rounds its pixel raster UP to whole
        // cells (`cols_spanned`/`rows_spanned`); clamp to the grid width and the
        // per-image cell cap. The spans are bounded by SIXEL_MAX_DIMENSION / 1,
        // so the `u32` conversion never truncates.
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

        // Bound the realized RGBA allocation by the same per-image cap the OSC
        // 1337 path enforces at decode (`MAX_IMAGE_BYTES`). A DECGRA-only sixel
        // (`"Pan;Pad;Ph;Pv` with no trailing `$`/`-`/data byte) defers
        // `apply_raster` until `unhook`, so the width*height buffer is composed
        // AFTER the DCS stream/pixel-budget accounting in `dcs_put_inner` —
        // bypassing both the 10MB DCS budget and the 16MB image cap. Recompute
        // the byte count in u64 (4096*4096*4 overflows the u32 arithmetic
        // assumed elsewhere) and reject the image rather than allocating it.
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
            let bytes_le = px.to_be_bytes(); // [a, r, g, b]
            let (a, r, g, b) = (bytes_le[0], bytes_le[1], bytes_le[2], bytes_le[3]);
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
        });

        // Sixel anchors at the CURRENT cursor column (VT340/xterm), NOT at the
        // left margin like iTerm2's OSC 1337 path: a sixel emitted mid-line paints
        // starting at the cursor and does not overprint the cells to its left.
        let anchor_col = self.grid.cursor_col();

        if self.modes.sixel_display_mode {
            // Display mode (DECSDM set): paint at the cursor, then restore it so
            // the cursor does not move. `place_image` mutates the cursor as it
            // stamps + line-feeds, so save and restore around it.
            let save_row = self.grid.cursor_row();
            let save_col = self.grid.cursor_col();
            self.place_image(&image_data, cols, rows, anchor_col);
            self.grid.set_cursor(save_row, save_col);
        } else {
            // Scrolling mode (DECSDM reset, default): cursor advances below the
            // image, matching text flow.
            self.place_image(&image_data, cols, rows, anchor_col);
        }
    }
}

/// Parse the `File=` argument list (`key=value;key=value;…`). Unknown keys are
/// ignored; a missing/duplicate key takes the last value. Never errors.
fn parse_file_args(args: &[u8]) -> FileArgs {
    let mut out = FileArgs {
        // preserveAspectRatio defaults ON per the iTerm2 spec.
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
            // Default ON; only an explicit "0" disables it.
            out.preserve_aspect_ratio = value != "0";
        }
        // name=, size= and any other key are accepted but unused for placement.
    }
    out
}

/// Case-insensitively strip an ASCII prefix, returning the remainder.
fn strip_prefix_ascii_ci<'a>(haystack: &'a [u8], prefix: &[u8]) -> Option<&'a [u8]> {
    if haystack.len() >= prefix.len() && haystack[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&haystack[prefix.len()..])
    } else {
        None
    }
}

/// Classify the raw payload by magic bytes. Only PNG is renderable today.
fn detect_format(bytes: &[u8]) -> ImageFormat {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        ImageFormat::Png
    } else {
        ImageFormat::Unknown
    }
}

/// Read a PNG's pixel dimensions straight from the IHDR chunk without decoding
/// pixels (the engine has no codec). Returns `None` for a non-PNG or a truncated
/// header. IHDR width/height are the two big-endian u32s at byte offset 16.
fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    // 8-byte signature + 4-byte length + "IHDR" (4) → width@16, height@20.
    if bytes.len() < 24 || !bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
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
