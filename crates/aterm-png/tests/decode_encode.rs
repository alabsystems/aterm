// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-png` decode and encode integration tests over the repository's PNG corpus, a
//! synthesized format matrix pinned to `MATRIX_DIGEST`, malformed and misordered files,
//! and encoder round trips.

use std::path::{Path, PathBuf};

/// What one decode produced.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Decoded {
    width: u32,
    height: u32,
    color_type: aterm_png::ColorType,
    bit_depth: aterm_png::BitDepth,
    line_size: usize,
    pixels: Vec<u8>,
}

/// Which transform set to decode under: all FOUR points of the public
/// transform surface, not just the pair the shipped call sites use.
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

/// FNV-1a over the decoded matrix, so the whole matrix is pinned by one value.
struct Fnv(u64);

impl Fnv {
    fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= u64::from(b);
            self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
        }
    }

    fn decoded(&mut self, result: &Result<Decoded, String>) {
        match result {
            Ok(d) => {
                self.feed(&d.width.to_le_bytes());
                self.feed(&d.height.to_le_bytes());
                self.feed(&[d.color_type as u8, d.bit_depth as u8]);
                self.feed(&(d.line_size as u64).to_le_bytes());
                self.feed(&d.pixels);
            }
            Err(_) => self.feed(b"ERR"),
        }
    }
}

/// Encode 8-bit `pixels` with this crate's encoder.
fn encode(color: aterm_png::ColorType, w: u32, h: u32, pixels: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = aterm_png::Encoder::new(&mut out, w, h);
    encoder.set_color(color);
    encoder.set_depth(aterm_png::BitDepth::Eight);
    let mut writer = encoder.write_header().expect("header");
    writer.write_image_data(pixels).expect("image data");
    writer.finish().expect("finish");
    out
}

/// Every `.png` in the repository, excluding build output.
fn corpus() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crate lives two levels under the repository root")
        .to_path_buf();
    corpus_under(&root)
}

/// Whether the corpus walk stays out of the directory `name` inside `parent`:
/// `target`, `.git` and every `*.noindex` build dir a tool chose
/// (`libc-oracle/target.noindex`) at any depth, and at the repository root every
/// `target-*` sibling build dir (`target-trust.noindex`, older lanes' leftovers)
/// and a leftover `.aterm-verify` state dir, which the checked-in .gitignore names
/// as `/target-*` and `/.aterm-verify/`.
fn corpus_skips(root: &Path, parent: &Path, name: &str) -> bool {
    name == "target"
        || name == ".git"
        || name.ends_with(".noindex")
        || (parent == root && (name.starts_with("target-") || name == ".aterm-verify"))
}

/// Every `.png` under `root`, sorted, minus the directories [`corpus_skips`]
/// names.
fn corpus_under(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if path.is_dir() {
                if !corpus_skips(root, &dir, &name) {
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

/// No file-count floor: the corpus is whatever the checkout holds (tracked
/// files plus any local capture dumps), and pinning its size would be a
/// registry of repository contents. The compression check below still needs
/// at least one image to have been round-tripped.
#[test]
fn the_repository_corpus_decodes_and_round_trips() {
    let files = corpus();
    let (mut images, mut raw_total, mut encoded_total) = (0usize, 0usize, 0usize);
    for path in &files {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let label = path.display().to_string();
        for (mode, _) in ALL_MODES {
            let _ = decode_mine(&bytes, mode);
        }
        let Ok(decoded) = decode_mine(&bytes, Mode::Identity) else {
            continue;
        };
        if decoded.bit_depth != aterm_png::BitDepth::Eight
            || decoded.color_type == aterm_png::ColorType::Indexed
        {
            continue;
        }
        let out = encode(
            decoded.color_type,
            decoded.width,
            decoded.height,
            &decoded.pixels,
        );
        let back = decode_mine(&out, Mode::Identity)
            .unwrap_or_else(|e| panic!("{label}: refused its own re-encoding: {e}"));
        assert_eq!(
            back, decoded,
            "{label}: the re-encoding decodes differently"
        );
        images += 1;
        raw_total += decoded.pixels.len();
        encoded_total += out.len();
    }
    assert!(
        encoded_total < raw_total,
        "the compressor must actually compress: {encoded_total} of {raw_total} \
         over {images} images"
    );
}

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
            out.push((row_counter % 5) as u8);
            row_counter += 1;
            for _ in 0..stride {
                out.push((next() >> 24) as u8);
            }
        }
    }
    out
}

/// Pinned FNV-1a digest of the decoded format matrix across all transforms.
const MATRIX_DIGEST: &str = "873b3aed06c78dbf";

/// Every legal (colour type x bit depth) pair, both interlace settings, with and
/// without `tRNS`, under every transform mode — the formats a colour font, a
/// wallpaper or a pasted image are allowed to arrive in.
#[test]
fn decodes_the_whole_format_matrix_to_its_pinned_digest() {
    let combos: [(u8, &[u8]); 5] = [
        (0, &[1, 2, 4, 8, 16]),
        (2, &[8, 16]),
        (3, &[1, 2, 4, 8]),
        (4, &[8, 16]),
        (6, &[8, 16]),
    ];
    let full_palette: Vec<u8> = (0..256u32)
        .flat_map(|i| {
            [
                (i * 7 % 256) as u8,
                (i * 13 % 256) as u8,
                (i * 29 % 256) as u8,
            ]
        })
        .collect();
    let short_palette: Vec<u8> = vec![255, 0, 0, 0, 255, 0, 0, 0, 255];

    let mut cases = 0usize;
    let mut digest = Fnv(0xcbf2_9ce4_8422_2325);
    for (color, depths) in combos {
        for &depth in depths {
            for palette in [full_palette.as_slice(), short_palette.as_slice()] {
                if color != 3 && !std::ptr::eq(palette, full_palette.as_slice()) {
                    continue;
                }
                for interlace in [0u8, 1u8] {
                    for with_trns in [false, true] {
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
                                _ => (0..200u32).map(|i| (i % 256) as u8).collect(),
                            })
                        } else {
                            None
                        };
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
                                digest.feed(label.as_bytes());
                                digest.feed(mode_name.as_bytes());
                                digest.decoded(&decode_mine(&bytes, mode));
                                cases += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(cases > 200, "the matrix should be broad, ran {cases} cases");
    assert_eq!(
        format!("{:016x}", digest.0),
        MATRIX_DIGEST,
        "the decoded format matrix changed ({cases} cases)"
    );
}

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

/// The Adler-32 lives at the END OF THE STREAM, not at the end of the buffer.
/// A decoder that takes the last four bytes of the IDAT payload to be the
/// checksum reports a perfectly good image as corrupt.
#[test]
fn idat_padded_after_the_zlib_stream_still_decodes() {
    let filtered = synth_filtered(9, 7, 8, 6, 0);
    let mut reference = None;
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

        let decoded = decode_mine(&bytes, Mode::ExpandStrip16)
            .unwrap_or_else(|e| panic!("IDAT with {pad} trailing bytes: refused: {e}"));
        let reference = reference.get_or_insert_with(|| decoded.clone());
        assert_eq!(&decoded, reference, "IDAT with {pad} trailing bytes");
    }
}

/// A stream that decompresses to far more than the header's geometry is a
/// decompression bomb, not padding: one maximal stored block of slack is
/// honoured, and the excess beyond it is refused with `ExcessImageData`.
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
    let reference = decode_mine(&build(0), Mode::ExpandStrip16).expect("exact size decodes");
    for extra in [1usize, 64, 65, 4096, 65_535] {
        let decoded = decode_mine(&build(extra), Mode::ExpandStrip16)
            .unwrap_or_else(|e| panic!("{extra} bytes of padding: refused inside the slack: {e}"));
        assert_eq!(decoded, reference, "{extra} bytes of padding");
    }
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
}

/// A limit of exactly the output buffer decodes every format: the image data
/// is streamed, so nothing else the decoder allocates scales with the image.
/// One byte less is refused when the header is read, before the caller sizes a
/// buffer by `output_buffer_size`.
#[test]
fn a_limit_of_exactly_the_output_buffer_decodes_every_format() {
    let palette: Vec<u8> = (0..=255u8).flat_map(|i| [i, i, i]).collect();
    let mut cases = Vec::new();
    for (color, depth) in [
        (6u8, 8u8),
        (6, 16),
        (2, 8),
        (2, 16),
        (4, 16),
        (0, 1),
        (0, 4),
        (0, 16),
        (3, 2),
        (3, 8),
    ] {
        for interlace in [0u8, 1] {
            let (w, h) = (17u32, 9u32);
            let bytes = build_png(
                w,
                h,
                depth,
                color,
                interlace,
                (color == 3).then_some(palette.as_slice()),
                None,
                &synth_filtered(w, h, depth, color, interlace),
            );
            for (mode, mode_name) in ALL_MODES {
                let unlimited = decode_mine(&bytes, mode)
                    .unwrap_or_else(|e| panic!("{color}/{depth}/{interlace} {mode_name}: {e}"));
                let label =
                    format!("colour {color} depth {depth} interlace {interlace} {mode_name}");
                cases.push((label, bytes.clone(), mode, unlimited.pixels));
            }
        }
    }
    fn decoder(bytes: &[u8], mode: Mode, limit: usize) -> aterm_png::Decoder<&[u8]> {
        let mut decoder = aterm_png::Decoder::new(bytes);
        decoder.set_limits(aterm_png::Limits { bytes: limit });
        decoder.set_transformations(mine_transforms(mode));
        decoder
    }
    for (label, bytes, mode, pixels) in &cases {
        let exact = pixels.len();
        let mut reader = decoder(bytes, *mode, exact)
            .read_info()
            .unwrap_or_else(|e| panic!("{label}: header at a {exact}-byte limit: {e}"));
        let mut buf = vec![0u8; reader.output_buffer_size()];
        reader
            .next_frame(&mut buf)
            .unwrap_or_else(|e| panic!("{label}: image at a {exact}-byte limit: {e}"));
        assert_eq!(&buf, pixels, "{label}");
    }
    for (label, bytes, mode, pixels) in &cases {
        assert!(
            matches!(
                decoder(bytes, *mode, pixels.len() - 1).read_info(),
                Err(aterm_png::DecodingError::LimitsExceeded)
            ),
            "{label}: one byte under the output must be refused with the header",
        );
    }
}

/// Reading a source is bounded even if it never reaches EOF. The guard turns
/// an unbounded read into an immediate regression failure instead of an OOM.
#[test]
fn encoded_input_cannot_bypass_the_pixel_budget() {
    use std::cell::Cell;
    use std::io::Read;

    struct GuardedRepeat<'a> {
        consumed: &'a Cell<usize>,
        maximum: usize,
    }

    impl Read for GuardedRepeat<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if buf.is_empty() {
                return Ok(0);
            }
            let left = self.maximum.saturating_sub(self.consumed.get());
            assert!(left > 0, "decoder read beyond its encoded-input ceiling");
            let n = left.min(buf.len());
            buf[..n].fill(0);
            self.consumed.set(self.consumed.get() + n);
            Ok(n)
        }
    }

    let pixel_budget = 4;
    let encoded_limit = pixel_budget * 4 + 64 * 1024;
    let consumed = Cell::new(0);
    let mut decoder = aterm_png::Decoder::new(GuardedRepeat {
        consumed: &consumed,
        maximum: encoded_limit + 1,
    });
    decoder.set_limits(aterm_png::Limits {
        bytes: pixel_budget,
    });
    assert!(matches!(
        decoder.read_info(),
        Err(aterm_png::DecodingError::LimitsExceeded)
    ));
    assert_eq!(consumed.get(), encoded_limit + 1);

    let mut bytes = encode(aterm_png::ColorType::Rgba, 1, 1, &[1, 2, 3, 4]);
    bytes.resize(encoded_limit, 0);
    let mut decoder = aterm_png::Decoder::new(bytes.as_slice());
    decoder.set_limits(aterm_png::Limits {
        bytes: pixel_budget,
    });
    let mut reader = decoder.read_info().expect("input exactly at the limit");
    let mut pixels = [0; 4];
    reader.next_frame(&mut pixels).expect("pixels");
    assert_eq!(pixels, [1, 2, 3, 4]);
    bytes.push(0);
    let mut decoder = aterm_png::Decoder::new(bytes.as_slice());
    decoder.set_limits(aterm_png::Limits {
        bytes: pixel_budget,
    });
    assert!(matches!(
        decoder.read_info(),
        Err(aterm_png::DecodingError::LimitsExceeded)
    ));

    let mut decoder = aterm_png::Decoder::new(bytes.as_slice());
    decoder.set_limits(aterm_png::Limits { bytes: usize::MAX });
    assert!(
        decoder.read_info().is_ok(),
        "a large budget must saturate without overflow"
    );
}

/// A zlib stream of `n` zero bytes — one literal, then distance-1
/// back-references in a fixed-Huffman block — written out here so it shares no
/// code with the decoder. Zero is filter type None and a black pixel, so this
/// is the filtered data of an all-black image of any geometry, at about 1/160
/// of its size.
fn zlib_zeros(n: usize) -> Vec<u8> {
    struct Bits {
        out: Vec<u8>,
        acc: u64,
        len: u32,
    }
    impl Bits {
        /// `count` bits of `value`, least-significant first.
        fn put(&mut self, value: u32, count: u32) {
            self.acc |= u64::from(value) << self.len;
            self.len += count;
            while self.len >= 8 {
                self.out.push(self.acc as u8);
                self.acc >>= 8;
                self.len -= 8;
            }
        }
        /// A Huffman code, most-significant bit first.
        fn code(&mut self, code: u32, count: u32) {
            for i in (0..count).rev() {
                self.put((code >> i) & 1, 1);
            }
        }
    }
    let mut bits = Bits {
        out: vec![0x78, 0x01],
        acc: 0,
        len: 0,
    };
    bits.put(1, 1); // BFINAL
    bits.put(1, 2); // BTYPE = fixed Huffman
    let literal_zero = |bits: &mut Bits| bits.code(0x30, 8);
    if n > 0 {
        literal_zero(&mut bits);
    }
    let rest = n.saturating_sub(1);
    for _ in 0..rest / 258 {
        bits.code(0xC5, 8); // length symbol 285: 258 bytes, no extra bits
        bits.code(0, 5); // distance code 0: distance 1
    }
    for _ in 0..rest % 258 {
        literal_zero(&mut bits);
    }
    bits.code(0, 7); // end of block
    if bits.len > 0 {
        bits.put(0, 8 - bits.len);
    }
    let mut out = bits.out;
    let b = (n % 65_521) as u32;
    out.extend_from_slice(&((b << 16) | 1).to_be_bytes());
    out
}

/// An all-black `w` x `h` PNG at `depth`/`color`, non-interlaced.
fn black_png(w: u32, h: u32, depth: u8, color: u8) -> Vec<u8> {
    let stride = (w as usize * samples_of(color) * depth as usize).div_ceil(8);
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[depth, color, 0, 0, 0]);
    bytes.extend_from_slice(&chunk(b"IHDR", &ihdr));
    bytes.extend_from_slice(&chunk(b"IDAT", &zlib_zeros((stride + 1) * h as usize)));
    bytes.extend_from_slice(&chunk(b"IEND", &[]));
    bytes
}

/// `decode_rgba8` decodes every image inside the dimension budget its caller
/// names: the renderer's inline-image cap is 4096 per side, the wallpaper's
/// 8192. A 4096x4096 RGBA8 image is exactly the inline budget, a 16-bit one
/// decompresses to twice its output, and a wallpaper past 64 MiB of RGBA (a 6K
/// display's own resolution is) is still inside 8192 per side.
#[test]
fn decode_rgba8_decodes_every_image_inside_the_callers_dimension_budget() {
    for (w, h, depth, color, max_dimension) in [
        (4096u32, 4096u32, 8u8, 6u8, 4096u32),
        (2900, 2900, 16, 6, 4096),
        (4200, 4100, 8, 6, 8192),
    ] {
        let png = black_png(w, h, depth, color);
        let (rgba, got_w, got_h) =
            aterm_png::decode_rgba8(&png, max_dimension).unwrap_or_else(|| {
                panic!("{w}x{h} depth {depth} inside a {max_dimension}-per-side budget: refused")
            });
        assert_eq!((got_w, got_h), (w as usize, h as usize));
        assert_eq!(rgba.len(), w as usize * h as usize * 4);
        assert!(
            rgba.iter().all(|&byte| byte == 0),
            "{w}x{h}: an all-black, fully transparent image",
        );
    }
    assert!(aterm_png::decode_rgba8(&black_png(4097, 1, 8, 6), 4096).is_none());
    assert!(aterm_png::decode_rgba8(&black_png(1, 4097, 8, 6), 4096).is_none());
}

/// A palette index past the end of PLTE renders as opaque black rather than
/// killing the whole image.
#[test]
fn an_index_past_the_palette_renders_black() {
    let palette = [255u8, 0, 0];
    let bytes = indexed_png(&palette, None, &[0, 5]);
    let mine = decode_mine(&bytes, Mode::ExpandStrip16).expect("must decode, not refuse");
    assert_eq!(mine.pixels, vec![255, 0, 0, 0, 0, 0], "red then black");

    let bytes = indexed_png(&palette, Some(&[9]), &[0, 5]);
    let mine = decode_mine(&bytes, Mode::ExpandStrip16).expect("must decode, not refuse");
    assert_eq!(mine.pixels, vec![255, 0, 0, 9, 0, 0, 0, 255]);
}

/// A tRNS with more entries than the palette is IGNORED, not honoured — but its
/// presence still decides the output shape.
#[test]
fn a_trns_longer_than_the_palette_is_ignored() {
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
    }
}

/// Below 16 bits only the low byte of a tRNS colour key is the sample; a key
/// with a stray high byte still matches (masked), rather than matching nothing.
#[test]
fn an_out_of_range_trns_key_still_matches_its_low_byte() {
    let bytes = build_png(2, 1, 8, 0, 0, None, Some(&[0x01, 0x2A]), &[0u8, 0x2A, 0x00]);
    let mine = decode_mine(&bytes, Mode::ExpandStrip16).expect("decodes");
    assert_eq!(mine.color_type, aterm_png::ColorType::GrayscaleAlpha);
    assert_eq!(mine.pixels[1], 0, "the masked key must match the sample");

    let bytes = build_png(2, 1, 4, 0, 0, None, Some(&[0x02, 0x0F]), &[0u8, 0xF0]);
    let mine = decode_mine(&bytes, Mode::ExpandStrip16).expect("decodes");
    assert_eq!(mine.pixels[1], 0, "depth 4: the masked key must match");

    let filtered = [0u8, 0x2A, 0x2A, 0x2A, 0, 0, 0];
    let trns = [0x01u8, 0x2A, 0x01, 0x2A, 0x01, 0x2A];
    let bytes = build_png(2, 1, 8, 2, 0, None, Some(&trns), &filtered);
    let mine = decode_mine(&bytes, Mode::ExpandStrip16).expect("decodes");
    assert_eq!(mine.pixels[3], 0, "RGB: the masked key must match");
}

/// A ZERO-LENGTH tRNS is absent: it must not promote an indexed image to Rgba
/// (only the `EXPAND`-alone mode can see the shape).
#[test]
fn a_zero_length_trns_is_absent() {
    let palette = [255u8, 0, 0, 0, 255, 0];
    let bytes = indexed_png(&palette, Some(&[]), &[0, 1]);
    let mine = decode_mine(&bytes, Mode::ExpandOnly).expect("decodes");
    assert_eq!(
        (mine.color_type, mine.line_size),
        (aterm_png::ColorType::Rgb, 6),
        "an empty tRNS must not promote the output to RGBA",
    );
}

/// Chunk ORDERING rules: a duplicate `PLTE`, a `tRNS` before `PLTE`, and `IDAT`
/// chunks interrupted by another chunk are refused; consecutive IDATs are not.
#[test]
fn illegal_chunk_orderings_are_refused() {
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
    split_idat.extend_from_slice(&chunk(b"tEXt", b"key\0value"));
    split_idat.extend_from_slice(&chunk(b"IDAT", &idat[4..]));
    split_idat.extend_from_slice(&chunk(b"IEND", &[]));

    for (label, bytes) in [
        ("duplicate PLTE", duplicate_plte),
        ("tRNS before PLTE", trns_before_plte),
        ("non-consecutive IDAT", split_idat),
    ] {
        for (mode, mode_name) in ALL_MODES {
            assert!(
                decode_mine(&bytes, mode).is_err(),
                "{label} [{mode_name}]: accepted"
            );
        }
    }

    let mut consecutive = sig.to_vec();
    consecutive.extend_from_slice(&chunk(b"IHDR", &ihdr));
    consecutive.extend_from_slice(&chunk(b"PLTE", &palette));
    consecutive.extend_from_slice(&chunk(b"IDAT", &idat[..4]));
    consecutive.extend_from_slice(&chunk(b"IDAT", &idat[4..]));
    consecutive.extend_from_slice(&chunk(b"IEND", &[]));
    let mine =
        decode_mine(&consecutive, Mode::ExpandStrip16).expect("a legal IDAT split must decode");
    assert_eq!(mine.pixels, vec![255, 0, 0, 0, 255, 0]);
}

/// Every chunk the encoder writes carries the CRC this file's own bitwise
/// `crc32` computes. The encoder and decoder share one `Crc32` (astream-wire's
/// fold), so a round trip alone could not see them agree on a wrong CRC; the
/// independent reference can. A stored CRC one bit off is then refused as
/// `BadCrc` naming its chunk, under every transform. Negative control: the same
/// bytes with the CRC left alone decode to the pixels written.
#[test]
fn encoded_chunk_crcs_match_an_independent_crc_and_a_flipped_one_is_refused() {
    let pixels: Vec<u8> = (0u8..18).map(|b| b.wrapping_mul(29)).collect();
    let png = encode(aterm_png::ColorType::Rgb, 3, 2, &pixels);
    let be32 = |at: usize| u32::from_be_bytes([png[at], png[at + 1], png[at + 2], png[at + 3]]);
    let mut at = 8;
    let mut kinds = Vec::new();
    while at < png.len() {
        let len = be32(at) as usize;
        let tagged = &png[at + 4..at + 8 + len];
        let kind = String::from_utf8_lossy(&tagged[..4]).into_owned();
        assert_eq!(be32(at + 8 + len), crc32(tagged), "chunk {kind}");
        kinds.push(kind);
        at += 12 + len;
    }
    assert_eq!(at, png.len(), "the chunks tile the file");
    assert_eq!(kinds.first().map(String::as_str), Some("IHDR"));
    assert!(kinds.iter().any(|k| k == "IDAT"), "{kinds:?}");
    assert_eq!(kinds.last().map(String::as_str), Some("IEND"));

    let mine = decode_mine(&png, Mode::Identity).expect("the encoder's own output decodes");
    assert_eq!(mine.pixels, pixels);

    let mut flipped = png.clone();
    flipped[8 + 4 + 4 + 13] ^= 0x01;
    for (mode, mode_name) in ALL_MODES {
        assert_eq!(
            decode_mine(&flipped, mode),
            Err("CRC mismatch in chunk IHDR".to_string()),
            "[{mode_name}]"
        );
    }
}

/// Test images for the encoder round trip: a gradient, a flat fill, noise, and
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

/// This decoder reads back exactly the pixels this encoder was given.
#[test]
fn this_decoder_reads_back_exactly_what_this_encoder_wrote() {
    for (label, color, w, h, data) in encoder_cases() {
        let out = encode(color, w, h, &data);
        let decoded = decode_mine(&out, Mode::Identity)
            .unwrap_or_else(|e| panic!("{label}: refused this encoder's PNG: {e}"));
        assert_eq!((decoded.width, decoded.height), (w, h), "{label}: geometry");
        assert_eq!(decoded.color_type, color, "{label}: colour type");
        assert_eq!(decoded.pixels, data, "{label}: pixels");
    }
}

/// The encoder's indexed path: what it writes reads back as exactly the colours
/// that went in, and an index past the palette is REFUSED at encode time.
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
    let identity = decode_mine(&out, Mode::Identity).expect("self decode");
    assert_eq!(identity.color_type, aterm_png::ColorType::Indexed);
    assert_eq!(identity.pixels, indices);

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

/// The `sRGB` chunk round-trips — the screenshot verbs assert on it.
#[test]
fn srgb_intent_round_trips() {
    let data = vec![9u8; 4 * 4 * 4];
    let mut out = Vec::new();
    {
        let mut encoder = aterm_png::Encoder::new(&mut out, 4, 4);
        encoder.set_color(aterm_png::ColorType::Rgba);
        encoder.set_depth(aterm_png::BitDepth::Eight);
        encoder.set_source_srgb(aterm_png::SrgbRenderingIntent::Perceptual);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&data).unwrap();
        writer.finish().unwrap();
    }
    let reader = aterm_png::Decoder::new(&out[..])
        .read_info()
        .expect("reads its own sRGB");
    assert_eq!(
        reader.info().srgb,
        Some(aterm_png::SrgbRenderingIntent::Perceptual),
    );
}

/// Seeded RGBA8 pixels, `w` x `h`.
fn seeded_rgba(w: u32, h: u32, seed: u32) -> Vec<u8> {
    let mut state = seed | 1;
    (0..w as usize * h as usize * 4)
        .map(|i| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            if (i / 4096) % 2 == 0 {
                (i % 251) as u8
            } else {
                (state >> 24) as u8
            }
        })
        .collect()
}

/// An RGBA8 sRGB encoder ready for its pixels.
fn rgba_writer(out: &mut Vec<u8>, w: u32, h: u32) -> aterm_png::Writer<&mut Vec<u8>> {
    let mut encoder = aterm_png::Encoder::new(out, w, h);
    encoder.set_color(aterm_png::ColorType::Rgba);
    encoder.set_depth(aterm_png::BitDepth::Eight);
    encoder.set_source_srgb(aterm_png::SrgbRenderingIntent::Perceptual);
    encoder.write_header().expect("header")
}

/// Streaming rows via `write_rows` produces the exact same bytes as
/// `write_image_data` across widths 1, 3, 17, and 256, and rejects rows one
/// byte short or long with `WrongDataSize`.
#[test]
fn write_rows_is_byte_identical_to_write_image_data() {
    for (w, h) in [(1u32, 5u32), (3, 7), (17, 33), (256, 300)] {
        let rgba = seeded_rgba(w, h, w * 7919 + h);
        let stride = w as usize * 4;

        let mut sliced = Vec::new();
        let mut writer = rgba_writer(&mut sliced, w, h);
        writer.write_image_data(&rgba).expect("slice");
        writer.finish().expect("finish");

        let mut rowed = Vec::new();
        let mut writer = rgba_writer(&mut rowed, w, h);
        writer
            .write_rows(|y, row| row.extend_from_slice(&rgba[y * stride..(y + 1) * stride]))
            .expect("rows");
        writer.finish().expect("finish");

        assert_eq!(rowed, sliced, "{w}x{h}: rows and slice write one file");
        let back = decode_mine(&rowed, Mode::Identity).expect("decodes");
        assert_eq!(back.pixels, rgba, "{w}x{h}: the pixels round-trip");
    }

    for delta in [-1isize, 1] {
        let (w, h) = (17u32, 4u32);
        let rgba = seeded_rgba(w, h, 3);
        let stride = w as usize * 4;
        let mut out = Vec::new();
        let mut writer = rgba_writer(&mut out, w, h);
        let error = writer
            .write_rows(|y, row| {
                let mut len = stride;
                if y == 2 {
                    len = len.saturating_add_signed(delta);
                }
                row.extend_from_slice(&rgba[y * stride..][..len]);
            })
            .expect_err("a row of the wrong length is refused");
        assert_eq!(
            error,
            aterm_png::EncodingError::WrongDataSize {
                expected: stride,
                got: stride.saturating_add_signed(delta),
            },
            "delta {delta}"
        );
        assert!(
            matches!(writer.finish(), Err(aterm_png::EncodingError::Parameter(_))),
            "delta {delta}: an incomplete file is not closed as if whole"
        );
    }
}

/// [`aterm_png::max_encoded_len`] bounds every file the encoder writes, and is
/// not vacuous: incompressible noise, the worst case for a fixed-Huffman
/// stream, lands within a tenth of it.
#[test]
fn max_encoded_len_bounds_every_encode() {
    let mut state = 0xDEAD_BEEFu32;
    for (w, h) in [(1u32, 1u32), (2, 3), (64, 64), (300, 200)] {
        let noise: Vec<u8> = (0..w as usize * h as usize * 4)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                (state >> 24) as u8
            })
            .collect();
        for pixels in [noise, vec![0u8; w as usize * h as usize * 4]] {
            let mut out = Vec::new();
            let mut writer = rgba_writer(&mut out, w, h);
            writer.write_image_data(&pixels).expect("data");
            writer.finish().expect("finish");
            let bound = aterm_png::max_encoded_len(w, h, aterm_png::ColorType::Rgba)
                .expect("the geometry fits");
            assert!(out.len() <= bound, "{w}x{h}: {} > {bound}", out.len());
        }
    }
    let (w, h) = (300u32, 200u32);
    let noise: Vec<u8> = (0..w as usize * h as usize * 4)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            (state >> 24) as u8
        })
        .collect();
    let mut out = Vec::new();
    let mut writer = rgba_writer(&mut out, w, h);
    writer.write_image_data(&noise).expect("data");
    writer.finish().expect("finish");
    let bound = aterm_png::max_encoded_len(w, h, aterm_png::ColorType::Rgba).unwrap();
    assert!(
        out.len() * 10 >= bound * 9,
        "noise wrote {} of a {bound}-byte bound",
        out.len()
    );
}
