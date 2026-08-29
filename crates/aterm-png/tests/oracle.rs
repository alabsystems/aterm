// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The differential test: `png` as the ORACLE for `aterm-png`.
//!
//! Five shapes of check, because they pin different things:
//!
//! 1. **The repository's own corpus** — every `.png` in the tree (969 of them at
//!    the time of writing: golden frames, assets, docs images, capture dumps,
//!    vendored fixtures) is decoded by both implementations under the exact
//!    transform aterm's shipped call sites use, and the pixel buffers must be
//!    BYTE-IDENTICAL. This is the check that says "the images aterm really
//!    handles come out the same".
//! 2. **A synthesized format matrix** — the corpus is 8-bit, non-interlaced,
//!    three colour types. The formats aterm can be HANDED are wider than that: a
//!    colour-font strike, a user's wallpaper, a pasted inline image. So this
//!    builds PNGs covering every legal (colour type x bit depth x interlace x
//!    tRNS x palette length) combination and compares the same way. The fixtures
//!    are assembled from raw chunks with STORED (uncompressed) DEFLATE blocks,
//!    so the builder shares no code with either decoder — it is a third party to
//!    the comparison. Both a FULL 256-entry palette and a three-entry one are
//!    used: with the short one the random samples index past the palette's end
//!    and the 200-byte tRNS is longer than the palette, which are precisely the
//!    two cases a full palette was engineering away.
//! 3. **Malformed but decodable files** — the matrix builds well-formed
//!    fixtures on purpose, so it cannot reach the inputs that actually break a
//!    decoder: IDAT padded past the end of its zlib stream, an index past PLTE,
//!    an over-long tRNS, a colour key with a stray high byte, a zero-length
//!    chunk, and the three chunk orderings PNG forbids. Each has its own named
//!    test, and each was verified to FAIL against the implementation before the
//!    fix rather than merely to pass after it.
//! 4. **Cross reads** — the oracle must read what this encoder writes, and this
//!    decoder must read what the oracle writes, pixel-for-pixel. That is what
//!    proves the two are interchangeable rather than merely self-consistent.
//!    Indexed output is included, even though nothing in aterm writes one today,
//!    because an encoder that can emit a file its own decoder refuses is a hole
//!    in the round trip whether or not anything is standing in it.
//! 5. **The ratio the compressor gives up** — measured on the corpus rather than
//!    asserted from theory, with a bound loose enough to be about the trade and
//!    tight enough to catch a compressor that has stopped compressing.

use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Bridging the two crates' enums
// ---------------------------------------------------------------------------

fn oracle_color(c: png::ColorType) -> aterm_png::ColorType {
    match c {
        png::ColorType::Grayscale => aterm_png::ColorType::Grayscale,
        png::ColorType::Rgb => aterm_png::ColorType::Rgb,
        png::ColorType::Indexed => aterm_png::ColorType::Indexed,
        png::ColorType::GrayscaleAlpha => aterm_png::ColorType::GrayscaleAlpha,
        png::ColorType::Rgba => aterm_png::ColorType::Rgba,
    }
}

fn oracle_depth(d: png::BitDepth) -> aterm_png::BitDepth {
    match d {
        png::BitDepth::One => aterm_png::BitDepth::One,
        png::BitDepth::Two => aterm_png::BitDepth::Two,
        png::BitDepth::Four => aterm_png::BitDepth::Four,
        png::BitDepth::Eight => aterm_png::BitDepth::Eight,
        png::BitDepth::Sixteen => aterm_png::BitDepth::Sixteen,
    }
}

fn mine_color(c: aterm_png::ColorType) -> png::ColorType {
    match c {
        aterm_png::ColorType::Grayscale => png::ColorType::Grayscale,
        aterm_png::ColorType::Rgb => png::ColorType::Rgb,
        aterm_png::ColorType::Indexed => png::ColorType::Indexed,
        aterm_png::ColorType::GrayscaleAlpha => png::ColorType::GrayscaleAlpha,
        aterm_png::ColorType::Rgba => png::ColorType::Rgba,
    }
}

/// What one decode produced, reduced so the two implementations compare as data.
#[derive(Debug, PartialEq, Eq)]
struct Decoded {
    width: u32,
    height: u32,
    color_type: aterm_png::ColorType,
    bit_depth: aterm_png::BitDepth,
    line_size: usize,
    pixels: Vec<u8>,
}

/// Which transform pair to run a comparison under.
///
/// All FOUR points of the public transform surface are here, not just the pair
/// the shipped call sites use. `EXPAND` alone is what catches an output whose
/// SHAPE diverges — a chunk that promotes Rgb to Rgba in one implementation and
/// not the other changes `line_size`, not merely pixels, and both shipped
/// consumers happen to be immune to that because they fill alpha themselves.
#[derive(Clone, Copy)]
enum Mode {
    /// What aterm's shipped decode sites ask for.
    ExpandStrip16,
    /// No transform at all: the file's own samples, at the file's own packing.
    Identity,
    /// `EXPAND` on its own.
    ExpandOnly,
    /// `STRIP_16` on its own.
    Strip16Only,
}

/// Every mode, with the label a failure will be reported under.
const ALL_MODES: [(Mode, &str); 4] = [
    (Mode::ExpandStrip16, "EXPAND|STRIP_16"),
    (Mode::Identity, "identity"),
    (Mode::ExpandOnly, "EXPAND"),
    (Mode::Strip16Only, "STRIP_16"),
];

fn mine_transforms(mode: Mode) -> aterm_png::Transformations {
    use aterm_png::Transformations as T;
    match mode {
        Mode::ExpandStrip16 => T::EXPAND | T::STRIP_16,
        Mode::Identity => T::IDENTITY,
        Mode::ExpandOnly => T::EXPAND,
        Mode::Strip16Only => T::STRIP_16,
    }
}

fn oracle_transforms(mode: Mode) -> png::Transformations {
    use png::Transformations as T;
    match mode {
        Mode::ExpandStrip16 => T::EXPAND | T::STRIP_16,
        Mode::Identity => T::IDENTITY,
        Mode::ExpandOnly => T::EXPAND,
        Mode::Strip16Only => T::STRIP_16,
    }
}

/// Decode one image with both implementations and require them to agree —
/// including on REFUSAL, which is agreement too.
fn assert_agrees(label: &str, bytes: &[u8], mode: Mode, mode_name: &str) {
    let mine = decode_mine(bytes, mode);
    let oracle = decode_oracle(bytes, mode);
    match (mine, oracle) {
        (Ok(mine), Ok(oracle)) => assert_same(label, mode_name, &mine, &oracle),
        (Err(_), Err(_)) => {}
        (mine, oracle) => panic!(
            "{label} [{mode_name}]: one implementation decoded and the other did not — \
             mine {mine:?}, oracle {oracle:?}"
        ),
    }
}

/// The same, under every transform mode.
fn assert_agrees_everywhere(label: &str, bytes: &[u8]) {
    for (mode, mode_name) in ALL_MODES {
        assert_agrees(label, bytes, mode, mode_name);
    }
}

fn decode_mine(bytes: &[u8], mode: Mode) -> Result<Decoded, String> {
    let mut decoder = aterm_png::Decoder::new(bytes);
    decoder.set_limits(aterm_png::Limits {
        bytes: 512 * 1024 * 1024,
    });
    decoder.set_transformations(mine_transforms(mode));
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    buf.truncate(info.buffer_size());
    Ok(Decoded {
        width: info.width,
        height: info.height,
        color_type: info.color_type,
        bit_depth: info.bit_depth,
        line_size: info.line_size,
        pixels: buf,
    })
}

fn decode_oracle(bytes: &[u8], mode: Mode) -> Result<Decoded, String> {
    let mut decoder = png::Decoder::new(bytes);
    decoder.set_limits(png::Limits {
        bytes: 512 * 1024 * 1024,
    });
    decoder.set_transformations(oracle_transforms(mode));
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    buf.truncate(info.buffer_size());
    Ok(Decoded {
        width: info.width,
        height: info.height,
        color_type: oracle_color(info.color_type),
        bit_depth: oracle_depth(info.bit_depth),
        line_size: info.line_size,
        pixels: buf,
    })
}

/// Compare the two decodes of one image, reporting the FIRST differing byte
/// rather than dumping megabytes of pixels.
fn assert_same(label: &str, mode_name: &str, mine: &Decoded, oracle: &Decoded) {
    assert_eq!(
        (
            mine.width,
            mine.height,
            mine.color_type,
            mine.bit_depth,
            mine.line_size
        ),
        (
            oracle.width,
            oracle.height,
            oracle.color_type,
            oracle.bit_depth,
            oracle.line_size
        ),
        "{label} [{mode_name}]: output geometry differs",
    );
    if mine.pixels != oracle.pixels {
        let at = mine
            .pixels
            .iter()
            .zip(&oracle.pixels)
            .position(|(a, b)| a != b);
        panic!(
            "{label} [{mode_name}]: pixels differ (len {} vs {}), first difference at {at:?}: \
             mine {:?} oracle {:?}",
            mine.pixels.len(),
            oracle.pixels.len(),
            at.map(|i| mine.pixels[i]),
            at.map(|i| oracle.pixels[i]),
        );
    }
}

/// Every `.png` in the repository, excluding build output.
fn corpus() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crate lives two levels under the repository root")
        .to_path_buf();
    let mut found = Vec::new();
    let mut stack = vec![root];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if path.is_dir() {
                if name != "target" && name != ".git" {
                    stack.push(path);
                }
            } else if name.to_ascii_lowercase().ends_with(".png") {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// Every `.png` this checkout holds, under BOTH transform settings.
///
/// The corpus is the strongest available evidence about real inputs: golden
/// frames, app icons, docs assets and vendored winit fixtures, produced by many
/// different encoders over the project's life.
///
/// THE FLOOR IS THE TRACKED COUNT, and that is the whole point of the number.
/// It used to be 200, which no clone of this repository has ever satisfied —
/// `git ls-files '*.png'` returns 24. It passed only on a working copy that had
/// accumulated ~990 untracked capture dumps and rendered goldens beside them, so
/// the guard was asserting on one developer's filesystem and failed for everyone
/// else, CI included. The walk still picks those local files up when they are
/// there, and they are genuinely worth comparing; what it must not do is require
/// them.
#[test]
fn decodes_the_repository_corpus_identically_to_the_oracle() {
    let files = corpus();
    assert!(
        files.len() >= 24,
        "expected at least the 24 `.png` files tracked in this repository, found \
         {} — has the walk broken?",
        files.len()
    );
    let mut compared = 0usize;
    for path in &files {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let label = path.display().to_string();
        for (mode, mode_name) in ALL_MODES {
            // Agreement on REFUSAL is agreement; `assert_agrees` treats it so.
            assert_agrees(&label, &bytes, mode, mode_name);
            compared += 1;
        }
    }
    assert_eq!(
        compared,
        files.len() * ALL_MODES.len(),
        "every file, under every transform mode",
    );
}

// ---------------------------------------------------------------------------
// A PNG builder that shares nothing with either decoder
// ---------------------------------------------------------------------------

/// CRC-32 (ISO-HDLC), written out here so the fixture builder does not borrow
/// the implementation under test.
fn crc32(data: &[u8]) -> u32 {
    let mut c: u32 = 0xFFFF_FFFF;
    for &b in data {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
    }
    c ^ 0xFFFF_FFFF
}

/// A zlib stream that uses only STORED (uncompressed) DEFLATE blocks — three
/// lines of format, no compressor, so nothing about the fixtures depends on
/// code being tested.
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78u8, 0x01];
    let mut chunks: Vec<&[u8]> = data.chunks(65535).collect();
    if chunks.is_empty() {
        chunks.push(&[]);
    }
    let last = chunks.len() - 1;
    for (i, chunk) in chunks.iter().enumerate() {
        out.push(u8::from(i == last)); // BFINAL, BTYPE = 00 (stored)
        let len = chunk.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(chunk);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in data {
        a = (a + u32::from(byte)) % 65521;
        b = (b + a) % 65521;
    }
    out.extend_from_slice(&((b << 16) | a).to_be_bytes());
    out
}

fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc_input = kind.to_vec();
    crc_input.extend_from_slice(data);
    out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
    out
}

/// Assemble a PNG from already-filtered scanline data.
#[allow(clippy::too_many_arguments)]
fn build_png(
    width: u32,
    height: u32,
    depth: u8,
    color: u8,
    interlace: u8,
    palette: Option<&[u8]>,
    trns: Option<&[u8]>,
    filtered: &[u8],
) -> Vec<u8> {
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[depth, color, 0, 0, interlace]);
    out.extend_from_slice(&chunk(b"IHDR", &ihdr));
    if let Some(p) = palette {
        out.extend_from_slice(&chunk(b"PLTE", p));
    }
    if let Some(t) = trns {
        out.extend_from_slice(&chunk(b"tRNS", t));
    }
    out.extend_from_slice(&chunk(b"IDAT", &zlib_stored(filtered)));
    out.extend_from_slice(&chunk(b"IEND", &[]));
    out
}

/// Samples per pixel for a PNG colour-type code.
fn samples_of(color: u8) -> usize {
    match color {
        0 | 3 => 1,
        2 => 3,
        4 => 2,
        _ => 4,
    }
}

/// Adam7 pass geometry, written out independently of the decoder's copy.
const ADAM7: [(u32, u32, u32, u32); 7] = [
    (0, 0, 8, 8),
    (4, 0, 8, 8),
    (0, 4, 4, 8),
    (2, 0, 4, 4),
    (0, 2, 2, 4),
    (1, 0, 2, 2),
    (0, 1, 1, 2),
];

/// Deterministic pseudo-random filtered scanline data for one image geometry,
/// with every row carrying a DIFFERENT filter type so all five are exercised.
fn synth_filtered(width: u32, height: u32, depth: u8, color: u8, interlace: u8) -> Vec<u8> {
    let samples = samples_of(color);
    let mut state = 0x2545_F491u32;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        state
    };
    let mut out = Vec::new();
    let passes: Vec<(u32, u32)> = if interlace == 1 {
        (0..7)
            .filter_map(|p| {
                let (x0, y0, dx, dy) = ADAM7[p];
                let pw = if width > x0 {
                    (width - x0).div_ceil(dx)
                } else {
                    0
                };
                let ph = if height > y0 {
                    (height - y0).div_ceil(dy)
                } else {
                    0
                };
                (pw > 0 && ph > 0).then_some((pw, ph))
            })
            .collect()
    } else {
        vec![(width, height)]
    };
    let mut row_counter = 0u32;
    for (pw, ph) in passes {
        let stride = ((pw as usize) * samples * depth as usize).div_ceil(8);
        for _ in 0..ph {
            // Cycle the filter type so None/Sub/Up/Average/Paeth all appear.
            out.push((row_counter % 5) as u8);
            row_counter += 1;
            for _ in 0..stride {
                out.push((next() >> 24) as u8);
            }
        }
    }
    out
}

/// Every legal (colour type x bit depth) pair, both interlace settings, with and
/// without `tRNS`, under both transform modes.
///
/// The corpus in the first test is real but narrow (8-bit, non-interlaced, three
/// colour types). This is the check that the WHOLE base format agrees — the
/// formats a colour font, a wallpaper or a pasted image are allowed to arrive in.
#[test]
fn decodes_the_whole_format_matrix_identically_to_the_oracle() {
    // (colour type, legal bit depths for it)
    let combos: [(u8, &[u8]); 5] = [
        (0, &[1, 2, 4, 8, 16]),
        (2, &[8, 16]),
        (3, &[1, 2, 4, 8]),
        (4, &[8, 16]),
        (6, &[8, 16]),
    ];
    // A 256-entry palette, so no index in the random data is out of range...
    let full_palette: Vec<u8> = (0..256u32)
        .flat_map(|i| {
            [
                (i * 7 % 256) as u8,
                (i * 13 % 256) as u8,
                (i * 29 % 256) as u8,
            ]
        })
        .collect();
    // ...and a THREE-entry one, so most of them are. A palette shorter than
    // 2^depth is legal PNG and the random samples then index past its end,
    // which is the case the full-palette matrix was engineered to avoid. It
    // also makes the 200-byte tRNS below longer than the palette, which is the
    // other divergence this dimension exists to pin.
    let short_palette: Vec<u8> = vec![255, 0, 0, 0, 255, 0, 0, 0, 255];

    let mut cases = 0usize;
    for (color, depths) in combos {
        for &depth in depths {
            for palette in [full_palette.as_slice(), short_palette.as_slice()] {
                // The palette dimension only means anything for an indexed image.
                if color != 3 && !std::ptr::eq(palette, full_palette.as_slice()) {
                    continue;
                }
                for interlace in [0u8, 1u8] {
                    for with_trns in [false, true] {
                        // tRNS is illegal where an alpha channel already exists.
                        if with_trns && matches!(color, 4 | 6) {
                            continue;
                        }
                        let trns: Option<Vec<u8>> = if with_trns {
                            Some(match color {
                                0 => {
                                    // A grey sample value at the source depth. The
                                    // shift is done in u32 because depth reaches 16.
                                    let max = ((1u32 << depth) - 1) as u16;
                                    let key = 1u16.min(max);
                                    key.to_be_bytes().to_vec()
                                }
                                2 => {
                                    let key = 1u16;
                                    let mut v = Vec::new();
                                    v.extend_from_slice(&key.to_be_bytes());
                                    v.extend_from_slice(&key.to_be_bytes());
                                    v.extend_from_slice(&key.to_be_bytes());
                                    v
                                }
                                // Shorter than the palette on purpose: the tail
                                // entries must read as fully opaque.
                                _ => (0..200u32).map(|i| (i % 256) as u8).collect(),
                            })
                        } else {
                            None
                        };
                        // Sizes chosen so several Adam7 passes are empty for the
                        // small one and all seven are populated for the larger.
                        for (w, h) in [(1u32, 1u32), (3, 5), (17, 9), (32, 32)] {
                            let filtered = synth_filtered(w, h, depth, color, interlace);
                            let bytes = build_png(
                                w,
                                h,
                                depth,
                                color,
                                interlace,
                                (color == 3).then_some(palette),
                                trns.as_deref(),
                                &filtered,
                            );
                            let label = format!(
                                "synthetic color={color} depth={depth} interlace={interlace} \
                             trns={with_trns} palette={} {w}x{h}",
                                palette.len() / 3,
                            );
                            for (mode, mode_name) in ALL_MODES {
                                assert_agrees(&label, &bytes, mode, mode_name);
                                cases += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(cases > 200, "the matrix should be broad, ran {cases} cases");
}

// ---------------------------------------------------------------------------
// The malformed-but-decodable cases
//
// Every one of these is a file some encoder can produce and the retired crate
// rendered. They are here because the matrix above cannot reach them: it builds
// well-formed fixtures on purpose, and a decoder that refuses a file the oracle
// renders looks exactly like a decoder that agrees with it until someone hands
// it the file. That regression has happened once already in this codebase — a
// colour-emoji strike that stopped rendering at all rather than rendering with
// one wrong pixel.
// ---------------------------------------------------------------------------

/// A one-row indexed PNG, assembled from raw chunks.
fn indexed_png(palette: &[u8], trns: Option<&[u8]>, indices: &[u8]) -> Vec<u8> {
    let mut filtered = vec![0u8]; // filter type None
    filtered.extend_from_slice(indices);
    build_png(
        indices.len() as u32,
        1,
        8,
        3,
        0,
        Some(palette),
        trns,
        &filtered,
    )
}

/// Bytes after the end of the zlib stream, inside the IDAT payload.
///
/// The Adler-32 lives at the END OF THE STREAM, not at the end of the buffer.
/// A decoder that takes the last four bytes of the concatenated IDAT payload to
/// be the checksum compares it against the padding and reports a perfectly good
/// image as corrupt — total failure (blank emoji, blank wallpaper, a refused
/// inline image) on a file the retired crate decoded, with an error that blames
/// corruption.
#[test]
fn idat_padded_after_the_zlib_stream_still_decodes() {
    let filtered = synth_filtered(9, 7, 8, 6, 0);
    for pad in [0usize, 1, 4, 64, 65, 1024] {
        let mut payload = zlib_stored(&filtered);
        payload.extend(std::iter::repeat_n(0x5Au8, pad));
        let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&9u32.to_be_bytes());
        ihdr.extend_from_slice(&7u32.to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
        bytes.extend_from_slice(&chunk(b"IHDR", &ihdr));
        bytes.extend_from_slice(&chunk(b"IDAT", &payload));
        bytes.extend_from_slice(&chunk(b"IEND", &[]));

        let label = format!("IDAT with {pad} trailing bytes");
        // Non-vacuous: this must DECODE, not merely agree about failing.
        decode_mine(&bytes, Mode::ExpandStrip16)
            .unwrap_or_else(|e| panic!("{label}: aterm-png refused it: {e}"));
        assert_agrees_everywhere(&label, &bytes);
    }
}

/// The one refusal kept DELIBERATELY, recorded as a divergence rather than
/// discovered as one.
///
/// A stream that decompresses to substantially more than the header's geometry
/// calls for is a decompression bomb, not padding: the excess is unbounded and
/// nothing will ever read it. The retired crate decodes such a file (it stops
/// reading once it has enough rows); this one refuses past one maximal DEFLATE
/// stored block of slack, and says so with its own error rather than calling the
/// file corrupt. Both halves are pinned here: the slack is honoured, and the
/// excess beyond it is refused with `ExcessImageData`.
#[test]
fn image_data_far_past_the_declared_size_is_refused_as_its_own_error() {
    let (w, h) = (9u32, 7u32);
    let filtered = synth_filtered(w, h, 8, 6, 0);
    let build = |extra: usize| {
        let mut padded = filtered.clone();
        padded.extend(std::iter::repeat_n(0u8, extra));
        let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&w.to_be_bytes());
        ihdr.extend_from_slice(&h.to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
        bytes.extend_from_slice(&chunk(b"IHDR", &ihdr));
        bytes.extend_from_slice(&chunk(b"IDAT", &zlib_stored(&padded)));
        bytes.extend_from_slice(&chunk(b"IEND", &[]));
        bytes
    };
    // Within the slack: decodes, and agrees with the oracle pixel for pixel.
    for extra in [1usize, 64, 65, 4096, 65_535] {
        let bytes = build(extra);
        let label = format!("{extra} bytes of decompressed padding");
        decode_mine(&bytes, Mode::ExpandStrip16)
            .unwrap_or_else(|e| panic!("{label}: refused inside the slack: {e}"));
        assert_agrees_everywhere(&label, &bytes);
    }
    // Past it: refused, with the error that says what actually happened. The
    // oracle still decodes this one — a DECLARED divergence, not an accident.
    let bytes = build(65_536 * 4);
    let err = aterm_png::Decoder::new(&bytes[..])
        .read_info()
        .expect("header parses")
        .next_frame(&mut vec![0u8; (w * h * 4) as usize])
        .expect_err("a stream this far past the image must be refused");
    assert!(
        matches!(err, aterm_png::DecodingError::ExcessImageData),
        "expected ExcessImageData, got {err:?}",
    );
    assert!(
        decode_oracle(&bytes, Mode::ExpandStrip16).is_ok(),
        "the divergence is only worth recording if the oracle really does accept it",
    );
}

/// A palette index past the end of PLTE renders as black rather than killing the
/// whole image.
///
/// This is the regression class the crate header cites by name. `png` fills its
/// lookup table with opaque black for every index it never populated, so an
/// index past the palette produces (0, 0, 0, 255); refusing the file instead
/// turns one stray byte into a blank glyph.
#[test]
fn an_index_past_the_palette_renders_black_in_both() {
    let palette = [255u8, 0, 0];
    let bytes = indexed_png(&palette, None, &[0, 5]);
    let mine = decode_mine(&bytes, Mode::ExpandStrip16).expect("must decode, not refuse");
    assert_eq!(mine.pixels, vec![255, 0, 0, 0, 0, 0], "red then black");
    assert_agrees_everywhere("index past the palette", &bytes);

    // ...and with tRNS in play the undefined index is OPAQUE black, because the
    // alpha table does not reach it either.
    let bytes = indexed_png(&palette, Some(&[9]), &[0, 5]);
    let mine = decode_mine(&bytes, Mode::ExpandStrip16).expect("must decode, not refuse");
    assert_eq!(mine.pixels, vec![255, 0, 0, 9, 0, 0, 0, 255]);
    assert_agrees_everywhere("index past the palette, with tRNS", &bytes);
}

/// "The tRNS chunk shall not contain more alpha values than there are palette
/// entries" — one that does is IGNORED, not honoured.
///
/// The chunk still promotes the output to RGBA (its presence is what decides the
/// shape), but every entry reads back opaque. Honouring it instead makes a
/// malformed strike translucent where it used to be solid.
#[test]
fn a_trns_longer_than_the_palette_is_ignored_by_both() {
    let palette = [255u8, 0, 0, 0, 255, 0];
    for len in [1usize, 2, 3, 4, 8, 256] {
        let trns: Vec<u8> = (0..len).map(|i| (77 + i) as u8).collect();
        let bytes = indexed_png(&palette, Some(&trns), &[0, 1]);
        let label = format!("tRNS of {len} against a 2-entry palette");
        let mine =
            decode_mine(&bytes, Mode::ExpandStrip16).unwrap_or_else(|e| panic!("{label}: {e}"));
        assert_eq!(
            mine.color_type,
            aterm_png::ColorType::Rgba,
            "{label}: the chunk still decides the output shape",
        );
        if len <= 2 {
            assert_eq!(mine.pixels[3], 77, "{label}: a legal tRNS is honoured");
        } else {
            assert_eq!(mine.pixels[3], 255, "{label}: an over-long tRNS is ignored");
            assert_eq!(mine.pixels[7], 255, "{label}: ...for every entry");
        }
        assert_agrees_everywhere(&label, &bytes);
    }
}

/// A `tRNS` colour key is stored in two bytes at EVERY depth, but below 16 bits
/// only the low byte is the sample. A key whose high byte is not zero is
/// malformed, and the question is only what a decoder does with it: `png` masks,
/// so the key still matches: comparing the full 16-bit word instead makes it
/// match nothing and silently changes a pixel from transparent to opaque.
#[test]
fn an_out_of_range_trns_key_still_matches_its_low_byte_in_both() {
    // Grayscale, depth 8: sample 0x2A against the key 0x012A.
    let filtered = vec![0u8, 0x2A, 0x00];
    let bytes = build_png(2, 1, 8, 0, 0, None, Some(&[0x01, 0x2A]), &filtered);
    let mine = decode_mine(&bytes, Mode::ExpandStrip16).expect("decodes");
    assert_eq!(mine.color_type, aterm_png::ColorType::GrayscaleAlpha);
    assert_eq!(mine.pixels[1], 0, "the masked key must match the sample");
    assert_agrees_everywhere("grayscale tRNS key with a stray high byte", &bytes);

    // Grayscale, depth 4: two samples per byte, key 0x020F against sample 0xF.
    let filtered = vec![0u8, 0xF0];
    let bytes = build_png(2, 1, 4, 0, 0, None, Some(&[0x02, 0x0F]), &filtered);
    let mine = decode_mine(&bytes, Mode::ExpandStrip16).expect("decodes");
    assert_eq!(mine.pixels[1], 0, "depth 4: the masked key must match");
    assert_agrees_everywhere("4-bit tRNS key with a stray high byte", &bytes);

    // RGB, depth 8.
    let filtered = vec![0u8, 0x2A, 0x2A, 0x2A, 0, 0, 0];
    let trns = [0x01u8, 0x2A, 0x01, 0x2A, 0x01, 0x2A];
    let bytes = build_png(2, 1, 8, 2, 0, None, Some(&trns), &filtered);
    let mine = decode_mine(&bytes, Mode::ExpandStrip16).expect("decodes");
    assert_eq!(mine.pixels[3], 0, "RGB: the masked key must match");
    assert_agrees_everywhere("RGB tRNS key with a stray high byte", &bytes);
}

/// A ZERO-LENGTH chunk carries nothing, and the retired crate never parses one —
/// its reader jumps from the length straight to the CRC. So an empty `tRNS` is
/// ABSENT, which changes the output's SHAPE and not merely its pixels: parsing
/// it would promote an indexed image to Rgba with a `w * 4` stride where the
/// oracle produces Rgb and `w * 3`. Only the `EXPAND`-alone mode can see that,
/// which is why it is in the matrix now.
#[test]
fn a_zero_length_trns_is_absent_in_both() {
    let palette = [255u8, 0, 0, 0, 255, 0];
    let bytes = indexed_png(&palette, Some(&[]), &[0, 1]);
    let mine = decode_mine(&bytes, Mode::ExpandOnly).expect("decodes");
    assert_eq!(
        (mine.color_type, mine.line_size),
        (aterm_png::ColorType::Rgb, 6),
        "an empty tRNS must not promote the output to RGBA",
    );
    assert_agrees_everywhere("zero-length tRNS", &bytes);
}

/// Chunk ORDERING rules, each of which the retired crate enforces and this one
/// silently accepted: a duplicate `PLTE`, a `tRNS` before `PLTE`, and `IDAT`
/// chunks interrupted by another chunk. None of these is data loss — but each
/// meant a corrupt or hostile file that the previous decoder refused now
/// produced an image, and the acceptance was untested in either direction.
#[test]
fn illegal_chunk_orderings_are_refused_by_both() {
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&2u32.to_be_bytes());
    ihdr.extend_from_slice(&1u32.to_be_bytes());
    ihdr.extend_from_slice(&[8, 3, 0, 0, 0]);
    let sig: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let palette = [255u8, 0, 0, 0, 255, 0];
    let idat = zlib_stored(&[0u8, 0, 1]);

    let mut duplicate_plte = sig.to_vec();
    duplicate_plte.extend_from_slice(&chunk(b"IHDR", &ihdr));
    duplicate_plte.extend_from_slice(&chunk(b"PLTE", &palette));
    duplicate_plte.extend_from_slice(&chunk(b"PLTE", &[0u8, 0, 255, 255, 255, 0]));
    duplicate_plte.extend_from_slice(&chunk(b"IDAT", &idat));
    duplicate_plte.extend_from_slice(&chunk(b"IEND", &[]));

    let mut trns_before_plte = sig.to_vec();
    trns_before_plte.extend_from_slice(&chunk(b"IHDR", &ihdr));
    trns_before_plte.extend_from_slice(&chunk(b"tRNS", &[128u8]));
    trns_before_plte.extend_from_slice(&chunk(b"PLTE", &palette));
    trns_before_plte.extend_from_slice(&chunk(b"IDAT", &idat));
    trns_before_plte.extend_from_slice(&chunk(b"IEND", &[]));

    let mut split_idat = sig.to_vec();
    split_idat.extend_from_slice(&chunk(b"IHDR", &ihdr));
    split_idat.extend_from_slice(&chunk(b"PLTE", &palette));
    split_idat.extend_from_slice(&chunk(b"IDAT", &idat[..4]));
    // An ancillary chunk in the middle of the IDAT run.
    split_idat.extend_from_slice(&chunk(b"tEXt", b"key\0value"));
    split_idat.extend_from_slice(&chunk(b"IDAT", &idat[4..]));
    split_idat.extend_from_slice(&chunk(b"IEND", &[]));

    for (label, bytes) in [
        ("duplicate PLTE", duplicate_plte),
        ("tRNS before PLTE", trns_before_plte),
        ("non-consecutive IDAT", split_idat),
    ] {
        assert!(
            decode_oracle(&bytes, Mode::ExpandStrip16).is_err(),
            "{label}: the oracle is expected to refuse this — the fixture is wrong if it does not",
        );
        assert!(
            decode_mine(&bytes, Mode::ExpandStrip16).is_err(),
            "{label}: accepted a file the oracle refuses",
        );
        assert_agrees_everywhere(label, &bytes);
    }

    // ...and the legal split — consecutive IDATs — must still work in both.
    let mut consecutive = sig.to_vec();
    consecutive.extend_from_slice(&chunk(b"IHDR", &ihdr));
    consecutive.extend_from_slice(&chunk(b"PLTE", &palette));
    consecutive.extend_from_slice(&chunk(b"IDAT", &idat[..4]));
    consecutive.extend_from_slice(&chunk(b"IDAT", &idat[4..]));
    consecutive.extend_from_slice(&chunk(b"IEND", &[]));
    decode_mine(&consecutive, Mode::ExpandStrip16).expect("a legal IDAT split must still decode");
    assert_agrees_everywhere("consecutive IDATs", &consecutive);
}

// ---------------------------------------------------------------------------
// Cross reads
// ---------------------------------------------------------------------------

/// Test images for the encoder cross-checks: a gradient, a flat fill, noise, and
/// a single pixel, at each of the colour types aterm writes.
fn encoder_cases() -> Vec<(&'static str, aterm_png::ColorType, u32, u32, Vec<u8>)> {
    let mut cases = Vec::new();
    for (name, color, samples) in [
        ("grayscale", aterm_png::ColorType::Grayscale, 1usize),
        ("rgb", aterm_png::ColorType::Rgb, 3),
        ("ga", aterm_png::ColorType::GrayscaleAlpha, 2),
        ("rgba", aterm_png::ColorType::Rgba, 4),
    ] {
        for (shape, w, h) in [
            ("1x1", 1u32, 1u32),
            ("gradient", 64, 48),
            ("wide", 257, 3),
            ("tall", 3, 257),
        ] {
            let mut data = Vec::with_capacity((w * h) as usize * samples);
            let mut state = 0x9E37_79B9u32;
            for y in 0..h {
                for x in 0..w {
                    state ^= state << 13;
                    state ^= state >> 17;
                    state ^= state << 5;
                    for s in 0..samples {
                        let value = match shape {
                            "gradient" => ((x * 4 + y * 2 + s as u32 * 17) % 256) as u8,
                            "wide" => (state >> 24) as u8,
                            _ => ((x + y + s as u32) % 256) as u8,
                        };
                        data.push(value);
                    }
                }
            }
            cases.push((
                Box::leak(format!("{name}/{shape}").into_boxed_str()) as &'static str,
                color,
                w,
                h,
                data,
            ));
        }
    }
    cases
}

/// The oracle must decode this encoder's output to exactly the pixels that went
/// in. A compressor is allowed to produce different BYTES from the oracle's; it
/// is not allowed to produce different PIXELS.
#[test]
fn the_oracle_reads_back_exactly_what_this_encoder_wrote() {
    for (label, color, w, h, data) in encoder_cases() {
        let mut out = Vec::new();
        {
            let mut encoder = aterm_png::Encoder::new(&mut out, w, h);
            encoder.set_color(color);
            encoder.set_depth(aterm_png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("header");
            writer.write_image_data(&data).expect("image data");
            writer.finish().expect("finish");
        }
        let decoded = decode_oracle(&out, Mode::Identity)
            .unwrap_or_else(|e| panic!("{label}: the oracle refused this encoder's PNG: {e}"));
        assert_eq!((decoded.width, decoded.height), (w, h), "{label}: geometry");
        assert_eq!(decoded.color_type, color, "{label}: colour type");
        assert_eq!(decoded.pixels, data, "{label}: pixels");

        // And this crate's own decoder agrees with the oracle on the same file.
        let mine = decode_mine(&out, Mode::Identity).expect("self decode");
        assert_same(label, "self", &mine, &decoded);
    }
}

/// The mirror: this decoder must read the ORACLE's encodings identically. The
/// oracle picks its own filters and its own DEFLATE blocks, so this exercises
/// filter/stream shapes this encoder never produces.
#[test]
fn this_decoder_reads_back_exactly_what_the_oracle_wrote() {
    for (label, color, w, h, data) in encoder_cases() {
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, w, h);
            encoder.set_color(mine_color(color));
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("header");
            writer.write_image_data(&data).expect("image data");
        }
        let mine = decode_mine(&out, Mode::Identity)
            .unwrap_or_else(|e| panic!("{label}: this decoder refused the oracle's PNG: {e}"));
        assert_eq!(mine.pixels, data, "{label}: pixels");
        let oracle = decode_oracle(&out, Mode::Identity).expect("oracle self decode");
        assert_same(label, "oracle-written", &mine, &oracle);
    }
}

/// The indexed + `tRNS` path both implementations must agree on — the exact
/// shape of Noto Color Emoji's CBDT strikes, which is the case that broke colour
/// emoji rendering once already (see `aterm-render`'s
/// `decode_png_expands_indexed_palette_to_rgba`).
#[test]
fn indexed_with_trns_expands_the_same_in_both() {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, 2, 1);
        encoder.set_color(png::ColorType::Indexed);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_palette(vec![255, 0, 0, 0, 255, 0]);
        encoder.set_trns(vec![255, 0]);
        let mut writer = encoder.write_header().expect("header");
        writer.write_image_data(&[0u8, 1u8]).expect("image data");
    }
    let mine = decode_mine(&out, Mode::ExpandStrip16).expect("mine");
    let oracle = decode_oracle(&out, Mode::ExpandStrip16).expect("oracle");
    assert_same("indexed+tRNS", "EXPAND|STRIP_16", &mine, &oracle);
    assert_eq!(mine.color_type, aterm_png::ColorType::Rgba);
    assert_eq!(&mine.pixels[0..4], &[255, 0, 0, 255]);
    assert_eq!(mine.pixels[7], 0, "the tRNS palette entry is transparent");
}

/// The encoder's indexed path, which nothing in aterm writes today and which
/// therefore had no round-trip at all.
///
/// Two halves. The first: what this encoder writes for an indexed image is read
/// back — by the oracle and by this crate's own decoder — as exactly the colours
/// that went in. The second: an index past the palette is REFUSED at encode
/// time, because writing one produces a file that is not a legal indexed PNG,
/// and a codec whose whole warrant is that it round-trips must not be able to
/// emit something its own decoder has to paper over.
#[test]
fn indexed_encoding_round_trips_and_refuses_an_index_past_the_palette() {
    let palette = vec![255u8, 0, 0, 0, 255, 0, 0, 0, 255];
    let indices = [0u8, 1, 2, 2, 1, 0];
    let mut out = Vec::new();
    {
        let mut encoder = aterm_png::Encoder::new(&mut out, indices.len() as u32, 1);
        encoder.set_color(aterm_png::ColorType::Indexed);
        encoder.set_depth(aterm_png::BitDepth::Eight);
        encoder.set_palette(palette.clone());
        encoder.set_trns(vec![255u8, 128, 0]);
        let mut writer = encoder.write_header().expect("header");
        writer.write_image_data(&indices).expect("image data");
        writer.finish().expect("finish");
    }
    let mine = decode_mine(&out, Mode::ExpandStrip16).expect("self decode");
    assert_eq!(
        mine.pixels,
        vec![
            255, 0, 0, 255, // index 0: red, opaque
            0, 255, 0, 128, // index 1: green, half
            0, 0, 255, 0, // index 2: blue, transparent
            0, 0, 255, 0, //
            0, 255, 0, 128, //
            255, 0, 0, 255, //
        ],
    );
    assert_agrees_everywhere("encoder indexed round trip", &out);

    // An index the palette does not have is refused, rather than written.
    let mut bad = Vec::new();
    let mut encoder = aterm_png::Encoder::new(&mut bad, 2, 1);
    encoder.set_color(aterm_png::ColorType::Indexed);
    encoder.set_depth(aterm_png::BitDepth::Eight);
    encoder.set_palette(vec![255u8, 0, 0]);
    let mut writer = encoder.write_header().expect("header");
    let err = writer
        .write_image_data(&[0u8, 9])
        .expect_err("an index past a 1-entry palette must be refused");
    assert!(
        matches!(err, aterm_png::EncodingError::Parameter(_)),
        "expected a parameter error, got {err:?}",
    );
}

/// The `sRGB` chunk round-trips through both directions — the screenshot verbs
/// assert on it, so it is part of the contract, not decoration.
#[test]
fn srgb_intent_survives_both_ways() {
    let data = vec![9u8; 4 * 4 * 4];
    let mut mine_out = Vec::new();
    {
        let mut encoder = aterm_png::Encoder::new(&mut mine_out, 4, 4);
        encoder.set_color(aterm_png::ColorType::Rgba);
        encoder.set_depth(aterm_png::BitDepth::Eight);
        encoder.set_source_srgb(aterm_png::SrgbRenderingIntent::Perceptual);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&data).unwrap();
        writer.finish().unwrap();
    }
    let oracle_reader = png::Decoder::new(std::io::Cursor::new(&mine_out))
        .read_info()
        .expect("oracle reads this encoder's sRGB");
    assert_eq!(
        oracle_reader.info().srgb,
        Some(png::SrgbRenderingIntent::Perceptual),
    );

    let mut oracle_out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut oracle_out, 4, 4);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&data).unwrap();
    }
    let mine_reader = aterm_png::Decoder::new(&oracle_out[..])
        .read_info()
        .expect("this decoder reads the oracle's sRGB");
    assert_eq!(
        mine_reader.info().srgb,
        Some(aterm_png::SrgbRenderingIntent::Perceptual),
    );
}

/// What the fixed-Huffman compressor actually costs, measured on the corpus
/// rather than assumed.
///
/// The bound is deliberately loose. It fails only if the compressor has stopped
/// compressing — the regression worth catching, because a silent fallback to
/// "no matches found" would still produce perfectly valid PNGs, just enormous
/// ones. The printed line is the point: it records what the trade actually is
/// instead of leaving it to be assumed. (It is currently a small WIN, not a
/// loss — see `src/deflate.rs` for why.)
#[test]
fn compression_ratio_against_the_oracle_is_recorded() {
    let mut mine_total = 0usize;
    let mut oracle_total = 0usize;
    let mut raw_total = 0usize;
    let mut images = 0usize;
    for path in corpus().into_iter().take(60) {
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let Ok(decoded) = decode_mine(&bytes, Mode::Identity) else {
            continue;
        };
        // Only the straightforward 8-bit cases; the point is the compressor.
        if decoded.bit_depth != aterm_png::BitDepth::Eight
            || decoded.color_type == aterm_png::ColorType::Indexed
        {
            continue;
        }
        let mut mine_out = Vec::new();
        {
            let mut encoder = aterm_png::Encoder::new(&mut mine_out, decoded.width, decoded.height);
            encoder.set_color(decoded.color_type);
            encoder.set_depth(aterm_png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&decoded.pixels).unwrap();
            writer.finish().unwrap();
        }
        let mut oracle_out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut oracle_out, decoded.width, decoded.height);
            encoder.set_color(mine_color(decoded.color_type));
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&decoded.pixels).unwrap();
        }
        mine_total += mine_out.len();
        oracle_total += oracle_out.len();
        raw_total += decoded.pixels.len();
        images += 1;
    }
    assert!(images >= 10, "expected a real sample, got {images} images");
    eprintln!(
        "PNG encode over {images} corpus images: raw {raw_total} B, aterm-png {mine_total} B \
         ({:.2}x), oracle {oracle_total} B ({:.2}x); aterm-png is {:.2}x the oracle's size",
        raw_total as f64 / mine_total as f64,
        raw_total as f64 / oracle_total as f64,
        mine_total as f64 / oracle_total as f64,
    );
    assert!(
        mine_total < raw_total,
        "the compressor must actually compress"
    );
    assert!(
        mine_total < oracle_total * 3,
        "a fixed-Huffman give-up of more than 3x against zlib is a regression, not a trade: \
         {mine_total} vs {oracle_total}"
    );
}
