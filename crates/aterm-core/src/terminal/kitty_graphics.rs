// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Kitty graphics protocol (APC `G`) command PARSER.
//!
//! Parses one Kitty graphics command — `APC G <control> ; <base64 payload> ST` —
//! into a structured [`KittyCommand`]. The control data is a comma-separated list
//! of `key=value` pairs; the optional payload after the `;` is the base64-encoded
//! image data (or, for non-direct mediums, a base64 path / shared-memory name).
//!
//! Protocol: <https://sw.kovidgoyal.net/kitty/graphics-protocol/>.
//!
//! The parser is pure, allocation-bounded, and never panics, so it is fully
//! unit-testable without a `Terminal`. The image store, placements and deletes
//! built on it live in `handler_actions.rs` (`handle_complete_kitty_command`,
//! which lists what the engine does not implement); `kitty_graphics` is
//! advertised TRUE on the strength of that core (`aterm-types`
//! `terminal_core.rs`).

/// The `a=` action of a Kitty graphics command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum KittyAction {
    /// `a=t` — transmit image data (store it), do not display yet.
    #[default]
    Transmit,
    /// `a=T` — transmit AND immediately display at the cursor.
    TransmitAndDisplay,
    /// `a=q` — query: does the terminal support this / is the id known.
    Query,
    /// `a=p` — put: display an already-transmitted image.
    Display,
    /// `a=d` — delete images / placements.
    Delete,
    /// `a=f` — transmit an animation frame.
    Frame,
    /// `a=a` — control animation.
    Animate,
    /// `a=c` — compose animation frames.
    Compose,
}

impl KittyAction {
    fn from_char(c: char) -> Option<Self> {
        Some(match c {
            't' => Self::Transmit,
            'T' => Self::TransmitAndDisplay,
            'q' => Self::Query,
            'p' => Self::Display,
            'd' => Self::Delete,
            'f' => Self::Frame,
            'a' => Self::Animate,
            'c' => Self::Compose,
            _ => return None,
        })
    }
}

/// The `f=` pixel format of transmitted image data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum KittyFormat {
    /// `f=24` — packed RGB, 3 bytes/pixel.
    Rgb,
    /// `f=32` — packed RGBA, 4 bytes/pixel (the protocol default).
    #[default]
    Rgba,
    /// `f=100` — a PNG file.
    Png,
}

/// The `t=` transmission medium.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum KittyMedium {
    /// `t=d` — direct: the payload IS the (base64) image data (the default).
    #[default]
    Direct,
    /// `t=f` — a regular file; the payload is its (base64) path.
    File,
    /// `t=t` — a temporary file (deleted after reading); payload is the path.
    TempFile,
    /// `t=s` — a POSIX shared-memory object; payload is its name.
    SharedMemory,
}

/// A parsed Kitty graphics command. Every numeric field is `Option` so an absent
/// key is distinguishable from an explicit `0`; the payload is base64-DECODED
/// (empty when absent or undecodable).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is one independent protocol key (m=, o=, C=, U=); a state machine would invent states the protocol does not have"
)]
pub struct KittyCommand {
    /// `a=` — what to do (default [`KittyAction::Transmit`]).
    pub action: KittyAction,
    /// `q=` — suppress responses: 0 verbose, 1 no-success, 2 no responses.
    pub quiet: u8,
    /// `f=` — pixel format (default [`KittyFormat::Rgba`]).
    pub format: KittyFormat,
    /// `t=` — transmission medium (default [`KittyMedium::Direct`]).
    pub medium: KittyMedium,
    /// `i=` — client-assigned image id. `i=0` means "none", as in kitty.
    pub id: Option<u32>,
    /// `I=` — client-assigned image number (id alternative: the terminal picks
    /// the id and reports it). `I=0` means "none".
    pub number: Option<u32>,
    /// `p=` — placement id. `p=0` means "none".
    pub placement: Option<u32>,
    /// `x=` — for a display, the LEFT edge of the source rectangle in pixels;
    /// for a delete (`d=p`/`q`/`x`), the 1-based cell COLUMN; for `d=r`, the
    /// lowest image id of the range.
    pub x: Option<u32>,
    /// `y=` — for a display, the TOP edge of the source rectangle in pixels;
    /// for a delete (`d=p`/`q`/`y`), the 1-based cell ROW; for `d=r`, the
    /// highest image id of the range.
    pub y: Option<u32>,
    /// `w=` — width of the source rectangle in pixels (`0`/absent = to the
    /// right edge).
    pub crop_width: Option<u32>,
    /// `h=` — height of the source rectangle in pixels (`0`/absent = to the
    /// bottom edge).
    pub crop_height: Option<u32>,
    /// `C=1` — do not move the cursor after placing the image.
    pub cursor_stays: bool,
    /// `U=1` — a VIRTUAL placement: the image is shown only where the client
    /// prints Unicode placeholders, never stamped at the cursor.
    pub virtual_placement: bool,
    /// `s=` — source image width in pixels (for raw formats).
    pub width: Option<u32>,
    /// `v=` — source image height in pixels (for raw formats).
    pub height: Option<u32>,
    /// `c=` — display width in CELLS (columns).
    pub columns: Option<u32>,
    /// `r=` — display height in CELLS (rows).
    pub rows: Option<u32>,
    /// `z=` — z-index (may be negative: behind the text).
    pub z_index: Option<i32>,
    /// `X=` — for a display, the pixel offset within the first cell at which
    /// the image starts; for `a=f`, the frame's composition mode; for `a=c`,
    /// the source rectangle's left edge.
    pub cell_x_offset: Option<u32>,
    /// `Y=` — for a display, the pixel offset within the first cell at which
    /// the image starts; for `a=f`, the frame's background colour; for `a=c`,
    /// the source rectangle's top edge.
    pub cell_y_offset: Option<u32>,
    /// `P=` — the image id of a relative placement's PARENT. `P=0` means none.
    pub parent_id: Option<u32>,
    /// `Q=` — the placement id of a relative placement's parent. `Q=0` means
    /// none.
    pub parent_placement: Option<u32>,
    /// `H=` — a relative placement's offset from its parent, in columns
    /// (negative is left).
    pub parent_dx: Option<i32>,
    /// `V=` — a relative placement's offset from its parent, in rows (negative
    /// is up).
    pub parent_dy: Option<i32>,
    /// `m=1` — more chunks of this image follow (chunked transmission).
    pub more: bool,
    /// `o=z` — the payload is zlib-compressed.
    pub compressed: bool,
    /// `d=` — for `a=d` delete, WHAT to delete: `i`/`I` = by image id (`i=`),
    /// `a`/`A` = all, etc. `None` (no `d=`) means delete all visible placements.
    pub delete_target: Option<char>,
    /// The base64-DECODED payload (image bytes for `t=d`, else a path / shm name).
    /// Empty when there was no payload or it failed to decode.
    pub payload: Vec<u8>,
}

/// Parse a Kitty graphics APC command from the raw APC payload (the bytes between
/// `APC` and `ST`), which for a graphics command begins with the `G` identifier.
///
/// Returns `None` when the payload is not a `G` command or the control data is not
/// valid UTF-8. Unknown control keys are ignored and a duplicate key takes the
/// last value (matching kitty). A malformed or absent base64 payload yields an
/// empty [`KittyCommand::payload`] rather than a parse failure, so the control
/// half is still usable (e.g. a query or delete with no data). Never panics.
#[must_use]
pub fn parse_kitty_command(apc: &[u8]) -> Option<KittyCommand> {
    // The graphics identifier is the leading `G`.
    let rest = apc.strip_prefix(b"G")?;
    // Split control data from the optional base64 payload on the first ';'.
    let (control, payload_b64) = match rest.iter().position(|&b| b == b';') {
        Some(i) => (&rest[..i], &rest[i + 1..]),
        None => (rest, &b""[..]),
    };
    // Control data is ASCII key=value pairs; reject non-UTF-8 outright.
    let control = std::str::from_utf8(control).ok()?;

    let mut cmd = KittyCommand::default();
    for pair in control.split(',') {
        if pair.is_empty() {
            continue;
        }
        let Some((key, value)) = pair.split_once('=') else {
            continue; // malformed pair (no '='): ignore, per kitty leniency
        };
        match key {
            "a" => {
                if let Some(a) = value.chars().next().and_then(KittyAction::from_char) {
                    cmd.action = a;
                }
            }
            "q" => cmd.quiet = value.parse().unwrap_or(0),
            "f" => {
                cmd.format = match value {
                    "24" => KittyFormat::Rgb,
                    "100" => KittyFormat::Png,
                    _ => KittyFormat::Rgba, // 32 and anything else default to RGBA
                };
            }
            "t" => {
                cmd.medium = match value.chars().next() {
                    Some('f') => KittyMedium::File,
                    Some('t') => KittyMedium::TempFile,
                    Some('s') => KittyMedium::SharedMemory,
                    _ => KittyMedium::Direct,
                };
            }
            "i" => cmd.id = nonzero(value),
            "I" => cmd.number = nonzero(value),
            "p" => cmd.placement = nonzero(value),
            "x" => cmd.x = value.parse().ok(),
            "y" => cmd.y = value.parse().ok(),
            "w" => cmd.crop_width = value.parse().ok(),
            "h" => cmd.crop_height = value.parse().ok(),
            "C" => cmd.cursor_stays = value == "1",
            "U" => cmd.virtual_placement = value == "1",
            "s" => cmd.width = value.parse().ok(),
            "v" => cmd.height = value.parse().ok(),
            "c" => cmd.columns = value.parse().ok(),
            "r" => cmd.rows = value.parse().ok(),
            "z" => cmd.z_index = value.parse().ok(),
            "X" => cmd.cell_x_offset = value.parse().ok(),
            "Y" => cmd.cell_y_offset = value.parse().ok(),
            "P" => cmd.parent_id = nonzero(value),
            "Q" => cmd.parent_placement = nonzero(value),
            "H" => cmd.parent_dx = value.parse().ok(),
            "V" => cmd.parent_dy = value.parse().ok(),
            "m" => cmd.more = value == "1",
            "o" => cmd.compressed = value == "z",
            "d" => cmd.delete_target = value.chars().next(),
            _ => {} // unknown key: ignore (forward-compatible)
        }
    }

    // Kitty base64 uses the standard alphabet; tolerate a bad payload by leaving it
    // empty (the control half — query/delete/metadata — is still valid). The APC
    // payload of a single chunk is continuous base64 (no line-wrapping).
    if !payload_b64.is_empty()
        && let Ok(s) = std::str::from_utf8(payload_b64)
        && let Ok(bytes) = aterm_codec::base64::decode(s)
    {
        cmd.payload = bytes;
    }

    Some(cmd)
}

/// Parse an id-like key (`i=`, `I=`, `p=`), where kitty reserves `0` for "not
/// specified".
fn nonzero(value: &str) -> Option<u32> {
    value.parse().ok().filter(|&v| v != 0)
}

/// Compose straight-alpha RGBA8 pixels the way kitty composes animation
/// frames: the `w × h` rectangle at `(sx, sy)` of `over` (a raster `over_w`
/// pixels wide) lands at `(dx, dy)` of `under` (a raster `under_w` pixels
/// wide) — alpha-blended ("source over") when `blend`, copied when not
/// (kitty's `X=1` / `C=1`). Whatever falls outside either raster is skipped,
/// so no geometry a client sends can index out of bounds.
pub fn compose_rgba(
    under: &mut [u8],
    under_w: usize,
    (dx, dy): (usize, usize),
    over: &[u8],
    over_w: usize,
    (sx, sy, w, h): (usize, usize, usize, usize),
    blend: bool,
) {
    let height = |len: usize, width: usize| len.checked_div(width.saturating_mul(4)).unwrap_or(0);
    let (under_h, over_h) = (height(under.len(), under_w), height(over.len(), over_w));
    for row in 0..h {
        let (src_row, dst_row) = (sy.saturating_add(row), dy.saturating_add(row));
        if src_row >= over_h || dst_row >= under_h {
            break;
        }
        for col in 0..w {
            let (src_col, dst_col) = (sx.saturating_add(col), dx.saturating_add(col));
            if src_col >= over_w || dst_col >= under_w {
                break;
            }
            let s = (src_row * over_w + src_col) * 4;
            let d = (dst_row * under_w + dst_col) * 4;
            let (Some(src), Some(dst)) = (over.get(s..s + 4), under.get_mut(d..d + 4)) else {
                return;
            };
            if blend {
                blend_over(dst, src);
            } else {
                dst.copy_from_slice(src);
            }
        }
    }
}

/// Straight-alpha "source over" of one RGBA8 pixel onto another.
fn blend_over(dst: &mut [u8], src: &[u8]) {
    let sa = u32::from(src[3]);
    if sa == 255 {
        dst.copy_from_slice(src);
        return;
    }
    if sa == 0 {
        return;
    }
    let dst_weight = u32::from(dst[3]) * (255 - sa);
    // The result's alpha, scaled by 255: never 0, since `sa > 0`.
    let alpha = sa * 255 + dst_weight;
    for c in 0..3 {
        let mixed = u32::from(src[c]) * sa * 255 + u32::from(dst[c]) * dst_weight;
        dst[c] = u8::try_from((mixed + alpha / 2) / alpha).unwrap_or(u8::MAX);
    }
    dst[3] = u8::try_from((alpha + 127) / 255).unwrap_or(u8::MAX);
}

/// Extract `(width, height)` in pixels from a PNG's IHDR header, or `None` if the
/// bytes are not a PNG. Lets the Kitty handler compute a PNG image's CELL
/// footprint without a full decode (the renderer decodes the pixels at draw time).
#[must_use]
pub fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    const SIG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    // PNG: 8-byte signature, then the IHDR chunk
    // `[len:4][type:4 "IHDR"][width:4 BE][height:4 BE]…`. Width is at byte 16,
    // height at byte 20.
    if bytes.len() < 24 || bytes[..8] != SIG {
        return None;
    }
    let w = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let h = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    Some((w, h))
}

/// Expand packed RGB (`f=24`, 3 bytes/pixel) to RGBA (4 bytes/pixel, opaque
/// alpha) for the renderer's `RawRgba8` path. A trailing partial pixel is dropped.
#[must_use]
pub fn rgb_to_rgba(rgb: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgb.len() / 3 * 4);
    for px in rgb.as_chunks::<3>().0 {
        out.extend_from_slice(&[px[0], px[1], px[2], 0xff]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an APC `G` payload from a control string + raw payload bytes (base64-
    /// encoded here so the parser decodes them back).
    fn apc(control: &str, payload: &[u8]) -> Vec<u8> {
        let mut v = b"G".to_vec();
        v.extend_from_slice(control.as_bytes());
        if !payload.is_empty() {
            v.push(b';');
            v.extend_from_slice(
                aterm_codec::base64::encode(payload)
                    .expect("encode")
                    .as_bytes(),
            );
        }
        v
    }

    #[test]
    fn transmit_and_display_png_with_id() {
        let c = parse_kitty_command(&apc("a=T,f=100,i=7", b"hello")).expect("parses");
        assert_eq!(c.action, KittyAction::TransmitAndDisplay);
        assert_eq!(c.format, KittyFormat::Png);
        assert_eq!(c.id, Some(7));
        assert_eq!(c.payload, b"hello");
    }

    #[test]
    fn defaults_when_keys_absent() {
        let c = parse_kitty_command(&apc("", b"raw")).expect("parses");
        assert_eq!(c.action, KittyAction::Transmit);
        assert_eq!(c.format, KittyFormat::Rgba);
        assert_eq!(c.medium, KittyMedium::Direct);
        assert_eq!(c.quiet, 0);
        assert!(!c.more && !c.compressed);
        assert_eq!(c.payload, b"raw");
    }

    #[test]
    fn raw_rgb_dimensions_and_medium() {
        let c = parse_kitty_command(&apc("f=24,s=10,v=20,t=f", b"")).expect("parses");
        assert_eq!(c.format, KittyFormat::Rgb);
        assert_eq!((c.width, c.height), (Some(10), Some(20)));
        assert_eq!(c.medium, KittyMedium::File);
        assert!(c.payload.is_empty());
    }

    #[test]
    fn query_and_delete_actions() {
        assert_eq!(
            parse_kitty_command(&apc("a=q,i=2", b"")).unwrap().action,
            KittyAction::Query
        );
        assert_eq!(
            parse_kitty_command(&apc("a=d", b"")).unwrap().action,
            KittyAction::Delete
        );
    }

    #[test]
    fn placement_z_index_and_chunking() {
        let c = parse_kitty_command(&apc("a=p,p=3,c=5,r=2,z=-1,m=1,o=z", b"")).expect("parses");
        assert_eq!(c.action, KittyAction::Display);
        assert_eq!(c.placement, Some(3));
        assert_eq!((c.columns, c.rows), (Some(5), Some(2)));
        assert_eq!(c.z_index, Some(-1));
        assert!(c.more);
        assert!(c.compressed);
    }

    #[test]
    fn source_rect_cursor_and_virtual_keys() {
        let c = parse_kitty_command(&apc("a=p,i=4,x=3,y=5,w=7,h=9,C=1,U=1", b"")).expect("parses");
        assert_eq!((c.x, c.y), (Some(3), Some(5)));
        assert_eq!((c.crop_width, c.crop_height), (Some(7), Some(9)));
        assert!(c.cursor_stays && c.virtual_placement);
        let c = parse_kitty_command(&apc("a=p,i=4", b"")).expect("parses");
        assert_eq!(
            (c.x, c.y, c.crop_width, c.crop_height),
            (None, None, None, None)
        );
        assert!(!c.cursor_stays && !c.virtual_placement);
    }

    /// The cell-offset and relative-placement keys. The negative control is
    /// the parse before these keys were read: every one of them fell to the
    /// unknown-key arm, so a relative put parsed as a plain put at the cursor.
    #[test]
    fn cell_offset_and_relative_placement_keys() {
        let c = parse_kitty_command(&apc("a=p,i=4,X=3,Y=5,P=7,Q=9,H=-2,V=4", b"")).expect("parses");
        assert_eq!((c.cell_x_offset, c.cell_y_offset), (Some(3), Some(5)));
        assert_eq!((c.parent_id, c.parent_placement), (Some(7), Some(9)));
        assert_eq!((c.parent_dx, c.parent_dy), (Some(-2), Some(4)));
        let c = parse_kitty_command(&apc("a=p,i=4,P=0,Q=0", b"")).expect("parses");
        assert_eq!(
            (
                c.parent_id,
                c.parent_placement,
                c.parent_dx,
                c.cell_x_offset
            ),
            (None, None, None, None),
            "P=0/Q=0 name no parent, as i=0 names no image"
        );
    }

    #[test]
    fn zero_ids_mean_unspecified() {
        let c = parse_kitty_command(&apc("a=p,i=0,I=0,p=0", b"")).expect("parses");
        assert_eq!((c.id, c.number, c.placement), (None, None, None));
    }

    #[test]
    fn unknown_keys_ignored_last_value_wins() {
        let c = parse_kitty_command(&apc("zz=99,i=1,i=2,bogus", b"")).expect("parses");
        assert_eq!(c.id, Some(2), "duplicate key takes the last value");
    }

    #[test]
    fn non_g_payload_is_none() {
        assert!(parse_kitty_command(b"X a=t").is_none());
        assert!(parse_kitty_command(b"").is_none());
    }

    #[test]
    fn bad_base64_keeps_command_with_empty_payload() {
        // '!' is not in the base64 alphabet -> payload undecodable, control intact.
        let c = parse_kitty_command(b"Ga=T,i=9;!!!not-base64!!!").expect("parses control");
        assert_eq!(c.action, KittyAction::TransmitAndDisplay);
        assert_eq!(c.id, Some(9));
        assert!(
            c.payload.is_empty(),
            "bad payload -> empty, not a parse failure"
        );
    }

    #[test]
    fn non_utf8_control_is_none() {
        // A 0xFF byte in the control half is not valid UTF-8.
        assert!(parse_kitty_command(b"Ga=\xfft").is_none());
    }

    /// Composition copies or blends inside both rasters and skips what falls
    /// outside them — a rectangle past either edge is clipped, never a panic.
    #[test]
    fn compose_rgba_copies_blends_and_clips() {
        // A 2x2 opaque blue canvas.
        let mut under = [0, 0, 255, 255].repeat(4);
        // A 1x1 half-transparent red pixel, blended at (1, 1).
        compose_rgba(
            &mut under,
            2,
            (1, 1),
            &[255, 0, 0, 128],
            1,
            (0, 0, 1, 1),
            true,
        );
        assert_eq!(
            &under[12..16],
            &[128, 0, 127, 255],
            "source over an opaque pixel"
        );
        assert_eq!(&under[..4], &[0, 0, 255, 255], "the rest untouched");
        // The same pixel COPIED keeps its own alpha.
        compose_rgba(
            &mut under,
            2,
            (0, 0),
            &[255, 0, 0, 128],
            1,
            (0, 0, 1, 1),
            false,
        );
        assert_eq!(&under[..4], &[255, 0, 0, 128]);
        // Over a fully transparent pixel, blending is the source itself.
        let mut clear = [0u8; 4];
        compose_rgba(
            &mut clear,
            1,
            (0, 0),
            &[10, 20, 30, 40],
            1,
            (0, 0, 1, 1),
            true,
        );
        assert_eq!(clear, [10, 20, 30, 40]);
        // Out of bounds on every side: clipped.
        let before = under.clone();
        compose_rgba(&mut under, 2, (2, 0), &[1; 16], 2, (0, 0, 9, 9), false);
        compose_rgba(&mut under, 2, (0, 0), &[1; 16], 2, (2, 2, 9, 9), false);
        assert_eq!(under, before, "nothing lands outside either raster");
        compose_rgba(&mut under, 2, (1, 0), &[9; 16], 2, (0, 0, 9, 9), false);
        assert_eq!(
            &under[4..8],
            &[9; 4],
            "the in-bounds part of a large rectangle lands"
        );
    }

    #[test]
    fn png_dimensions_reads_ihdr() {
        // Minimal PNG signature + IHDR with width=3, height=5.
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        png.extend_from_slice(&[0, 0, 0, 13]); // IHDR length
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&3u32.to_be_bytes()); // width
        png.extend_from_slice(&5u32.to_be_bytes()); // height
        assert_eq!(png_dimensions(&png), Some((3, 5)));
        assert_eq!(png_dimensions(b"not a png"), None);
        assert_eq!(png_dimensions(b""), None);
    }

    #[test]
    fn rgb_to_rgba_inserts_opaque_alpha() {
        let rgba = rgb_to_rgba(&[1, 2, 3, 4, 5, 6]);
        assert_eq!(rgba, vec![1, 2, 3, 0xff, 4, 5, 6, 0xff]);
        // Trailing partial pixel is dropped.
        assert_eq!(rgb_to_rgba(&[9, 9]), Vec::<u8>::new());
    }
}
