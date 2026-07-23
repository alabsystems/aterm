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
//! This is the FOUNDATION slice of KITTY-CORE (docs/EXCEED_GHOSTTY_PLAN.md): it is
//! pure, allocation-bounded, and never panics, so it is fully unit-testable
//! without a `Terminal`. The per-screen image store + renderer integration (and
//! only then re-advertising `kitty_graphics` in the capability set) build on it.

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
    /// `I=` — client-assigned image number (id alternative).
    pub number: Option<u32>,
    /// `p=` — placement id.
    pub placement: Option<u32>,
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
            "i" => cmd.id = value.parse().ok(),
            "I" => cmd.number = value.parse().ok(),
            "p" => cmd.placement = value.parse().ok(),
            "s" => cmd.width = value.parse().ok(),
            "v" => cmd.height = value.parse().ok(),
            "c" => cmd.columns = value.parse().ok(),
            "r" => cmd.rows = value.parse().ok(),
            "z" => cmd.z_index = value.parse().ok(),
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
