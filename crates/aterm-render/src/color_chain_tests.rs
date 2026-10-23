// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the colour-emoji fallback chain ([`Renderer::color_pick`]).

use crate::{GlyphClass, GlyphImage, GlyphKey, Renderer, Theme, color_face_draws};

const COLR: &[u8] = include_bytes!("../tests/fixtures/colr_1.ttf");
const MONO: &[u8] = include_bytes!("../assets/bundled/JetBrainsMono-Regular.ttf");

/// 🫨 SHAKING FACE: `Emoji_Presentation=Yes`, mapped by the next face only.
const SHAKING: char = '\u{1FAE8}';
/// A code point both faces map (the second cmap group, U+F0200..U+F0247), to the same glyph id.
const SHARED: char = '\u{F0200}';

/// The byte range of table `tag` in `font`.
fn table(font: &[u8], tag: &[u8; 4]) -> std::ops::Range<usize> {
    let be16 = |at: usize| usize::from(u16::from_be_bytes([font[at], font[at + 1]]));
    let be32 = |at: usize| {
        u32::from_be_bytes([font[at], font[at + 1], font[at + 2], font[at + 3]]) as usize
    };
    (0..be16(4))
        .map(|i| 12 + 16 * i)
        .find(|&record| &font[record..record + 4] == tag)
        .map(|record| be32(record + 8)..be32(record + 8) + be32(record + 12))
        .unwrap_or_else(|| panic!("colr_1.ttf has a {tag:?} table"))
}

/// The next face: [`COLR`] with its first colour group starting at [`SHAKING`], an
/// explicit invisible ZWJ in the same face, and every CPAL colour record's RGB inverted.
fn next_face() -> Vec<u8> {
    let mut font = COLR.to_vec();
    let cmap = table(&font, b"cmap").start;
    let be16 = |f: &[u8], at: usize| usize::from(u16::from_be_bytes([f[at], f[at + 1]]));
    let be32 = |f: &[u8], at: usize| u32::from_be_bytes([f[at], f[at + 1], f[at + 2], f[at + 3]]);
    let mut moved = 0;
    for record in 0..be16(&font, cmap + 2) {
        let subtable = cmap + be32(&font, cmap + 4 + 8 * record + 4) as usize;
        if be16(&font, subtable) != 12 {
            continue;
        }
        let group = subtable + 16;
        let (start, end) = (be32(&font, group), be32(&font, group + 4));
        assert_eq!((start, end), (0xF0100, 0xF0103), "colr_1.ttf's first group");
        let to = SHAKING as u32;
        font[group..group + 4].copy_from_slice(&to.to_be_bytes());
        font[group + 4..group + 8].copy_from_slice(&(to + (end - start)).to_be_bytes());
        // Map ZWJ to glyph 1 (empty, zero-advance) so same-face shaping succeeds without cascade.
        let groups = group..group + 12 * be32(&font, subtable + 12) as usize;
        let last = groups.end - 12;
        assert_eq!(
            (
                be32(&font, last),
                be32(&font, last + 4),
                be32(&font, last + 8)
            ),
            (0xFE001, 0xFE007, 1),
            "colr_1.ttf's empty-glyph group"
        );
        font[last..last + 4].copy_from_slice(&0x200Du32.to_be_bytes());
        font[last + 4..last + 8].copy_from_slice(&0x200Du32.to_be_bytes());
        font[groups].rotate_right(12);
        moved += 1;
    }
    assert_eq!(moved, 1, "colr_1.ttf has one format-12 subtable");
    let face = aterm_font::Face::parse(&font, 0).expect("the next face parses");
    assert_eq!(
        face.glyph_index('\u{200D}'),
        Some(aterm_font::GlyphId(1)),
        "the joiner belongs to the next face"
    );
    assert_eq!(
        face.glyph_hor_advance(aterm_font::GlyphId(1)),
        Some(0),
        "the joiner glyph advances nothing"
    );
    let cpal = table(&font, b"CPAL").start;
    let records = be16(&font, cpal + 6);
    let first = cpal + be32(&font, cpal + 8) as usize;
    for record in 0..records {
        let bgra = first + 4 * record;
        for channel in &mut font[bgra..bgra + 3] {
            *channel = 255 - *channel;
        }
    }
    repair_checksums(&mut font);
    font
}

/// Keep the altered fixture valid SFNT data, including the special zeroed-head
/// calculation: https://learn.microsoft.com/typography/opentype/spec/otff.
fn repair_checksums(font: &mut [u8]) {
    fn sum(bytes: &[u8]) -> u32 {
        bytes.chunks(4).fold(0u32, |sum, part| {
            let mut word = [0; 4];
            word[..part.len()].copy_from_slice(part);
            sum.wrapping_add(u32::from_be_bytes(word))
        })
    }
    let head = table(font, b"head").start;
    font[head + 8..head + 12].fill(0);
    let tables = usize::from(u16::from_be_bytes([font[4], font[5]]));
    for record in (0..tables).map(|i| 12 + 16 * i) {
        let offset = u32::from_be_bytes(font[record + 8..record + 12].try_into().unwrap()) as usize;
        let len = u32::from_be_bytes(font[record + 12..record + 16].try_into().unwrap()) as usize;
        let checksum = sum(&font[offset..offset + len]);
        font[record + 4..record + 8].copy_from_slice(&checksum.to_be_bytes());
    }
    let adjustment = 0xB1B0_AFBAu32.wrapping_sub(sum(font));
    font[head + 8..head + 12].copy_from_slice(&adjustment.to_be_bytes());
}

/// The head and next faces written to a scratch directory, as discovery would find them.
struct Faces {
    dir: std::path::PathBuf,
    head: String,
    next: String,
    next_bytes: Vec<u8>,
}

impl Faces {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "aterm-color-chain-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch font directory");
        let next_bytes = next_face();
        let head = dir.join("head-emoji.ttf");
        let next = dir.join("next-emoji.ttf");
        std::fs::write(&head, COLR).expect("head face written");
        std::fs::write(&next, &next_bytes).expect("next face written");
        Faces {
            dir,
            head: head.to_string_lossy().into_owned(),
            next: next.to_string_lossy().into_owned(),
            next_bytes,
        }
    }

    /// A renderer whose colour chain is these two faces, neither loaded yet, and no
    /// other face that could answer an emoji (no fallback chain, no runtime discovery).
    fn renderer(&self) -> Renderer {
        let mut r = renderer();
        r.color_font_paths = vec![self.head.clone(), self.next.clone()];
        r
    }
}

impl Drop for Faces {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn renderer() -> Renderer {
    let mut r = Renderer::from_bytes(MONO, 24.0, Theme::default()).expect("bundled face");
    r.set_runtime_font_discovery(false);
    r
}

/// A renderer whose colour chain is `face` alone: what that face draws for `key`.
fn solo(face: &[u8], key: GlyphKey) -> Vec<u8> {
    let mut r = renderer();
    r.set_color_font_bytes(face.to_vec()).expect("face parses");
    r.glyph_image(key).bytes().to_vec()
}

fn inked(image: &GlyphImage) -> bool {
    matches!(image, GlyphImage::Rgba { .. })
        && image
            .bytes()
            .as_chunks::<4>()
            .0
            .iter()
            .any(|pixel| pixel[3] > 0)
}

/// A code point the head lacks draws from the next face that has it, in colour, and the
/// next face loads only then: a code point the head draws reads no other face.
#[test]
fn a_point_the_head_lacks_draws_from_the_next_face_that_has_it() {
    assert!(
        !color_face_draws(COLR, SHAKING) && color_face_draws(&next_face(), SHAKING),
        "precondition: the head lacks 🫨 and the next face draws it"
    );
    let faces = Faces::new("fallthrough");
    let mut r = faces.renderer();

    let shared = r.glyph_key_emoji(SHARED);
    assert_eq!(shared, GlyphKey::rgba_char_in(0, SHARED, r.px_q));
    assert_eq!(
        (r.color_chain.len(), r.color_font_paths.as_slice()),
        (1, std::slice::from_ref(&faces.next)),
        "a point the head draws loads the head alone"
    );

    let key = r.glyph_key(SHAKING);
    assert_eq!(
        key,
        GlyphKey::rgba_char_in(1, SHAKING, r.px_q),
        "🫨 draws from the next face"
    );
    assert_eq!(key.chr(), Some(SHAKING));
    assert_eq!(key.colour_face(), 1);
    assert!(r.color_font_paths.is_empty(), "the next face loaded");
    let image = r.glyph_image(key).clone();
    assert!(inked(&image), "🫨 draws a colour glyph");
    assert_eq!(
        image.bytes(),
        solo(
            &faces.next_bytes,
            GlyphKey::rgba_char(crate::FaceId::ColorEmoji, SHAKING, r.px_q)
        ),
        "🫨 is the next face's glyph"
    );
    assert_eq!(r.glyph_key_emoji(SHAKING), key, "the VS16 dispatch agrees");

    let head_image = r.glyph_image(shared).bytes().to_vec();
    assert_eq!(head_image, solo(COLR, shared));
    assert_ne!(
        head_image,
        solo(&faces.next_bytes, shared),
        "precondition: the faces' palettes differ"
    );

    let mut sealed = faces.renderer();
    sealed.seal_admitted_font_sources();
    assert_eq!(sealed.admitted_font_sources().emoji.len(), 2);
    assert_eq!(
        sealed.glyph_key(SHAKING),
        GlyphKey::rgba_char_in(1, SHAKING, sealed.px_q)
    );
    let mut rebuilt = sealed
        .rebuild_from_admitted(30.0, Theme::default())
        .expect("a sealed generation rebuilds");
    let key = rebuilt.glyph_key(SHAKING);
    assert_eq!(key.colour_face(), 1);
    assert!(inked(rebuilt.glyph_image(key)));
}

/// Both faces carry glyph `gid`, drawn in different colours: each face's glyph is its own
/// key and its own image, so neither can serve the other from the glyph cache or an atlas.
/// A cluster the head cannot draw is shaped on the next face and drawn from it.
#[test]
fn two_faces_glyph_ids_are_different_keys_and_images() {
    let faces = Faces::new("gid");
    let mut r = faces.renderer();
    r.ensure_color_chain();
    assert_eq!(r.color_chain.len(), 2);
    let gid = aterm_font::Face::parse(COLR, 0)
        .expect("fixture parses")
        .glyph_index(SHARED)
        .expect("the head maps the shared point")
        .0;
    let (head, next) = (
        GlyphKey::rgba_gid_in(0, gid, r.px_q),
        GlyphKey::rgba_gid_in(1, gid, r.px_q),
    );
    assert_ne!(head, next, "one glyph id in two faces is two keys");
    assert_eq!(
        head,
        GlyphKey::rgba_gid(crate::FaceId::ColorEmoji, gid, r.px_q)
    );
    let head_image = r.glyph_image(head).bytes().to_vec();
    let next_image = r.glyph_image(next).bytes().to_vec();
    assert_eq!(head_image, solo(COLR, head));
    assert_eq!(next_image, solo(&faces.next_bytes, head));
    assert_ne!(
        head_image, next_image,
        "each key draws its own face's glyph"
    );

    let cluster = "\u{1FAE8}\u{200D}\u{1FAE8}";
    let key = r
        .glyph_key_cluster(cluster)
        .expect("the next face draws the cluster");
    assert_eq!(key.glyph_class, GlyphClass::RgbaCluster);
    let mut alone = renderer();
    alone
        .set_color_font_bytes(faces.next_bytes.clone())
        .expect("face parses");
    let alone_key = alone.glyph_key_cluster(cluster).expect("the face draws it");
    let drawn = r.glyph_image(key).clone();
    assert!(inked(&drawn));
    assert_eq!(drawn.bytes(), alone.glyph_image(alone_key).bytes());
}

/// An injected face (the configured `emoji_font` the GUI's font worker admits) heads the
/// chain, the discovered faces stay behind it, and a later injection replaces it rather
/// than stacking.
#[test]
fn an_injected_face_heads_the_chain_and_discovery_stays_behind_it() {
    let faces = Faces::new("injected");
    let mut r = renderer();
    r.color_font_paths = vec![faces.next.clone()];
    r.set_color_font_bytes(COLR.to_vec()).expect("face parses");
    assert_eq!(
        r.color_font_paths,
        std::slice::from_ref(&faces.next),
        "discovery stays pending"
    );
    assert_eq!(
        r.glyph_key_emoji(SHARED).colour_face(),
        0,
        "the injected face leads"
    );
    assert_eq!(
        r.glyph_key_emoji(SHAKING).colour_face(),
        1,
        "discovery fills its gaps"
    );

    r.set_color_font_bytes(faces.next_bytes.clone())
        .expect("face parses");
    assert_eq!(
        r.color_chain.len(),
        1,
        "no stacked injection, no repeated face"
    );
    assert!(r.color_chain[0].injected);
    assert_eq!(r.glyph_key_emoji(SHAKING).colour_face(), 0);
    assert_eq!(r.glyph_key_emoji(SHARED).colour_face(), 0);
}
