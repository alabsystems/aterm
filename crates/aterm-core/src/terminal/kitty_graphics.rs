// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Kitty graphics protocol (APC `G`) command parser.

/// The `a=` action of a Kitty graphics command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum KittyAction {
    /// `a=t` — transmit image data (store it), do not display yet.
    #[default]
    Transmit,
    /// `a=T` — transmit and immediately display at the cursor.
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
    #[inline]
    const fn from_char(c: char) -> Option<Self> {
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
    /// `t=d` — direct: the payload is the (base64) image data (the default).
    #[default]
    Direct,
    /// `t=f` — a regular file; the payload is its (base64) path.
    File,
    /// `t=t` — a temporary file (deleted after reading); payload is the path.
    TempFile,
    /// `t=s` — a POSIX shared-memory object; payload is its name.
    SharedMemory,
}

/// A parsed Kitty graphics command.
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
    /// `i=` — client-assigned image id.
    pub id: Option<u32>,
    /// `I=` — client-assigned image number (id alternative: the terminal picks the id and reports
    /// it).
    pub number: Option<u32>,
    /// `p=` — placement id.
    pub placement: Option<u32>,
    /// `x=` — for a display, the left edge of the source rectangle in pixels.
    pub x: Option<u32>,
    /// `y=` — for a display, the top edge of the source rectangle in pixels.
    pub y: Option<u32>,
    /// `w=` — width of the source rectangle in pixels (`0`/absent = to the right edge).
    pub crop_width: Option<u32>,
    /// `h=` — height of the source rectangle in pixels (`0`/absent = to the bottom edge).
    pub crop_height: Option<u32>,
    /// `C=1` — do not move the cursor after placing the image.
    pub cursor_stays: bool,
    /// `U=1` — a virtual placement: the image is shown only where the client prints Unicode
    /// placeholders, never stamped at the cursor.
    pub virtual_placement: bool,
    /// `s=` — source image width in pixels (for raw formats); for `a=a`, the animation state
    /// (1 stop, 2 run while loading, 3 run).
    pub width: Option<u32>,
    /// `v=` — source image height in pixels (for raw formats); for `a=a`, the loop count
    /// (1 forever, `n` n−1 times).
    pub height: Option<u32>,
    /// `c=` — display width in cells (columns); for `a=f`, `a=c` and `a=a`, a frame number.
    pub columns: Option<u32>,
    /// `r=` — display height in cells (rows); for `a=f`, `a=c`, `a=a` and a frame delete, a
    /// frame number.
    pub rows: Option<u32>,
    /// `z=` — z-index (may be negative: behind the text); for `a=f` and `a=a`, a frame's gap in
    /// milliseconds (negative: gapless).
    pub z_index: Option<i32>,
    /// `X=` — for a display, the pixel offset within the first cell at which the image starts.
    pub cell_x_offset: Option<u32>,
    /// `Y=` — for a display, the pixel offset within the first cell at which the image starts.
    pub cell_y_offset: Option<u32>,
    /// `P=` — the image id of a relative placement's parent.
    pub parent_id: Option<u32>,
    /// `Q=` — the placement id of a relative placement's parent.
    pub parent_placement: Option<u32>,
    /// `H=` — a relative placement's offset from its parent, in columns (negative is left).
    pub parent_dx: Option<i32>,
    /// `V=` — a relative placement's offset from its parent, in rows (negative is up).
    pub parent_dy: Option<i32>,
    /// `m=1` — more chunks of this image follow (chunked transmission).
    pub more: bool,
    /// `o=z` — the payload is zlib-compressed.
    pub compressed: bool,
    /// `d=` — for `a=d` delete, what to delete: `i`/`I` = by image id (`i=`), `a`/`A` = all, etc.
    pub delete_target: Option<char>,
    /// The base64-decoded payload (image bytes for `t=d`, else a path / shm name).
    pub payload: Vec<u8>,
}

/// The largest width or height, in pixels, of an image kitty loads (its `MAX_IMAGE_DIMENSION`).
pub(crate) const MAX_KITTY_DIMENSION: u32 = 10_000;

/// What a command arriving during a chunked load (`m=1`) does to that load.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DuringLoad {
    /// A chunk of the load: a transmit, query or frame load on the direct medium, whatever else it
    /// carries.
    Continues,
    /// A command of its own that ends the load first.
    Ends,
    /// A command of its own that leaves the load in flight.
    Beside,
}

impl KittyCommand {
    /// How this command is read while a chunked load is in flight.
    #[inline]
    pub(crate) const fn during_load(&self) -> DuringLoad {
        match self.action {
            KittyAction::Delete => DuringLoad::Ends,
            KittyAction::Display | KittyAction::Compose | KittyAction::Animate => {
                DuringLoad::Beside
            }
            KittyAction::Transmit
            | KittyAction::TransmitAndDisplay
            | KittyAction::Query
            | KittyAction::Frame => {
                if matches!(self.medium, KittyMedium::Direct) {
                    DuringLoad::Continues
                } else {
                    DuringLoad::Ends
                }
            }
        }
    }

    /// Whether kitty would begin loading this command's data.
    #[inline]
    pub(crate) const fn declares_loadable_size(&self) -> bool {
        let width_within = match self.width {
            Some(n) => n <= MAX_KITTY_DIMENSION,
            None => true,
        };
        let height_within = match self.height {
            Some(n) => n <= MAX_KITTY_DIMENSION,
            None => true,
        };
        let raw_has_area = matches!(
            (self.width, self.height),
            (Some(w), Some(h)) if w >= 1 && h >= 1
        );
        width_within && height_within && (matches!(self.format, KittyFormat::Png) || raw_has_area)
    }
}

/// Parse a Kitty graphics APC command from the raw APC payload (the bytes between `APC` and `ST`),
/// which for a graphics command begins with the `G` identifier.
#[must_use]
pub fn parse_kitty_command(apc: &[u8]) -> Option<KittyCommand> {
    let rest = apc.strip_prefix(b"G")?;
    let (control, payload_b64) = match rest.iter().position(|&b| b == b';') {
        Some(i) => (&rest[..i], &rest[i + 1..]),
        None => (rest, &b""[..]),
    };
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

    if !payload_b64.is_empty()
        && let Ok(s) = std::str::from_utf8(payload_b64)
        && let Ok(bytes) = aterm_codec::base64::decode(s)
    {
        cmd.payload = bytes;
    }

    Some(cmd)
}

#[inline]
fn nonzero(value: &str) -> Option<u32> {
    value.parse().ok().filter(|&v| v != 0)
}

/// Compose straight-alpha RGBA8 pixels the way kitty composes animation frames.
pub fn compose_rgba(
    under: &mut [u8],
    under_w: usize,
    (dx, dy): (usize, usize),
    over: &[u8],
    over_w: usize,
    (sx, sy, w, h): (usize, usize, usize, usize),
    blend: bool,
) {
    let height = |len: usize, width: usize| {
        width
            .checked_mul(4)
            .and_then(|stride| len.checked_div(stride))
            .unwrap_or(0)
    };
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

#[inline]
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
    let alpha = sa * 255 + dst_weight;
    for c in 0..3 {
        let mixed = u32::from(src[c]) * sa * 255 + u32::from(dst[c]) * dst_weight;
        dst[c] = u8::try_from((mixed + alpha / 2) / alpha).unwrap_or(u8::MAX);
    }
    dst[3] = u8::try_from((alpha + 127) / 255).unwrap_or(u8::MAX);
}

/// Extract `(width, height)` in pixels from a PNG's IHDR header, or `None` if the bytes are not a
/// PNG.
#[must_use]
#[inline]
pub fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    const SIG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    if bytes.len() < 24 || bytes[..8] != SIG {
        return None;
    }
    let w = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let h = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    Some((w, h))
}

/// Expand packed RGB (`f=24`, 3 bytes/pixel) to RGBA (4 bytes/pixel, opaque alpha) for the
/// renderer's `RawRgba8` path.
#[must_use]
pub fn rgb_to_rgba(rgb: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgb.len() / 3 * 4);
    for px in rgb.as_chunks::<3>().0 {
        out.extend_from_slice(&[px[0], px[1], px[2], 0xff]);
    }
    out
}

/// A zlib stream (RFC 1950) of `len` zero bytes, `len >= 1`.
#[cfg(test)]
pub(crate) fn zlib_zeros(len: usize) -> Vec<u8> {
    let mut bits: Vec<u8> = Vec::new();
    let mut field = |value: u32, n: u32| bits.extend((0..n).map(|i| ((value >> i) & 1) as u8));
    let code = |code: u32, n: u32| (0..n).fold(0, |r, i| r | ((code >> i) & 1) << (n - 1 - i));
    field(0b011, 3); // BFINAL, fixed Huffman
    field(code(0x30, 8), 8); // literal 0
    let mut left = len - 1;
    while left >= 258 {
        field(code(0b1100_0101, 8), 8); // length 258
        field(0, 5); // distance 1
        left -= 258;
    }
    for _ in 0..left {
        field(code(0x30, 8), 8);
    }
    field(0, 7); // end of block
    let mut out = vec![0x78, 0x01];
    out.extend(bits.chunks(8).map(|byte| {
        byte.iter()
            .enumerate()
            .fold(0u8, |acc, (i, &bit)| acc | (bit << i))
    }));
    let b = u32::try_from(len % 65521).expect("below 65521");
    out.extend_from_slice(&((b << 16) | 1).to_be_bytes());
    out
}

#[cfg(test)]
#[path = "kitty_graphics_tests.rs"]
mod tests;
