// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Synthetic `CBLC`/`CBDT`, `MVAR`, and `head.macStyle` font table tests.

use aterm_font::{Face, GlyphId, RasterImageFormat, Tag};

use crate::never_panic::sfnt;

fn be16(v: u16) -> [u8; 2] {
    v.to_be_bytes()
}

fn be32(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}

struct Bitmap {
    height: u8,
    width: u8,
    bearing_x: i8,
    bearing_y: i8,
    png: &'static [u8],
}

fn cbdt_tables(strikes: &[(u8, [Bitmap; 2])]) -> (Vec<u8>, Vec<u8>) {
    let mut cbdt: Vec<u8> = be32(0x0003_0000).to_vec();
    let sizes_end = 8 + 48 * strikes.len() as u32;
    let mut sizes: Vec<u8> = Vec::new();
    let mut arrays: Vec<u8> = Vec::new();
    for (ppem, glyphs) in strikes {
        let image_data_offset = cbdt.len() as u32;
        let mut offsets = vec![0u32];
        for g in glyphs {
            cbdt.extend([g.height, g.width, g.bearing_x as u8, g.bearing_y as u8, 0]);
            cbdt.extend(be32(g.png.len() as u32));
            cbdt.extend(g.png);
            offsets.push(cbdt.len() as u32 - image_data_offset);
        }
        let array_offset = sizes_end + arrays.len() as u32;
        arrays.extend(be16(1));
        arrays.extend(be16(2));
        arrays.extend(be32(8));
        arrays.extend(be16(1));
        arrays.extend(be16(17));
        arrays.extend(be32(image_data_offset));
        for o in offsets {
            arrays.extend(be32(o));
        }
        sizes.extend(be32(array_offset));
        sizes.extend(be32(0));
        sizes.extend(be32(1));
        sizes.extend(be32(0));
        sizes.extend([0u8; 24]);
        sizes.extend(be16(1));
        sizes.extend(be16(2));
        sizes.extend([*ppem, *ppem, 32, 0]);
    }
    let mut cblc: Vec<u8> = be32(0x0003_0000).to_vec();
    cblc.extend(be32(strikes.len() as u32));
    cblc.extend(sizes);
    cblc.extend(arrays);
    (cblc, cbdt)
}

/// Synthetic 4-glyph face with `CBDT` bitmaps in 20 and 109 ppem strikes.
pub(crate) fn cbdt_face() -> Vec<u8> {
    let (cblc, cbdt) = cbdt_tables(&[
        (
            20,
            [
                Bitmap {
                    height: 18,
                    width: 20,
                    bearing_x: 1,
                    bearing_y: 16,
                    png: b"png 20/1",
                },
                Bitmap {
                    height: 10,
                    width: 12,
                    bearing_x: -2,
                    bearing_y: 8,
                    png: b"png 20/2!",
                },
            ],
        ),
        (
            109,
            [
                Bitmap {
                    height: 100,
                    width: 109,
                    bearing_x: 0,
                    bearing_y: 90,
                    png: b"png 109/1",
                },
                Bitmap {
                    height: 60,
                    width: 64,
                    bearing_x: 3,
                    bearing_y: 50,
                    png: b"png 109/2",
                },
            ],
        ),
    ]);
    sfnt(4, &[(b"CBLC", &cblc), (b"CBDT", &cbdt)])
}

#[test]
fn cbdt_glyphs_are_read_from_the_right_strike() {
    let bytes = cbdt_face();
    let face = Face::parse(&bytes, 0).expect("the synthetic face parses");
    assert!(face.tables().cbdt.is_some(), "CBLC + CBDT parse");

    let image = |gid: u16, ppem: u16| face.glyph_raster_image(GlyphId(gid), ppem);
    let small = image(1, 16).expect("glyph 1 at 16 px");
    assert_eq!(
        (small.x, small.y, small.width, small.height),
        (1, 16 - 18, 20, 18)
    );
    assert_eq!(small.pixels_per_em, 20);
    assert_eq!(small.format, RasterImageFormat::PNG);
    assert_eq!(small.data, b"png 20/1");

    let second = image(2, 20).expect("glyph 2 at 20 px");
    assert_eq!(
        (second.x, second.y, second.width, second.height, second.data),
        (-2, 8 - 10, 12, 10, &b"png 20/2!"[..])
    );

    for ppem in [21, 64, 109, 200, u16::MAX] {
        let large = image(1, ppem).expect("glyph 1");
        assert_eq!(
            (large.pixels_per_em, large.y, large.data),
            (109, 90 - 100, &b"png 109/1"[..]),
            "ppem {ppem}"
        );
    }
    assert_eq!(image(2, 64).map(|i| i.data), Some(&b"png 109/2"[..]));

    assert!(image(0, 20).is_none());
    assert!(image(3, 20).is_none());
    let plain = sfnt(4, &[]);
    let plain = Face::parse(&plain, 0).expect("the plain face parses");
    assert!(plain.glyph_raster_image(GlyphId(1), 20).is_none());
}

fn mvar_face(mvar: bool) -> Vec<u8> {
    let fixed = |v: i32| be32((v << 16) as u32);
    let mut fvar: Vec<u8> = be32(0x0001_0000).to_vec();
    for v in [16u16, 2, 1, 20, 0, 8] {
        fvar.extend(be16(v));
    }
    fvar.extend(b"wght");
    fvar.extend(fixed(100));
    fvar.extend(fixed(400));
    fvar.extend(fixed(900));
    fvar.extend([0, 0, 1, 0]);
    let mut os2 = vec![0u8; 96];
    os2[0..2].copy_from_slice(&be16(4));
    os2[62..64].copy_from_slice(&be16(0x80));
    os2[68..70].copy_from_slice(&be16(800));
    let mut post = vec![0u8; 32];
    post[0..4].copy_from_slice(&be32(0x0003_0000));
    post[8..10].copy_from_slice(&(-100i16).to_be_bytes());
    post[10..12].copy_from_slice(&be16(50));
    let mut table: Vec<u8> = be32(0x0001_0000).to_vec();
    for v in [0u16, 8, 2, 28] {
        table.extend(be16(v));
    }
    for tag in [b"hasc", b"undo"] {
        table.extend(tag);
        table.extend([0, 0, 0, 0]);
    }
    table.extend(be16(1));
    table.extend(be32(12));
    table.extend(be16(1));
    table.extend(be32(22));
    table.extend(be16(1));
    table.extend(be16(1));
    table.extend([0, 0, 0x40, 0, 0x40, 0]);
    for v in [1u16, 1, 1, 0] {
        table.extend(be16(v));
    }
    table.extend(be16(100));
    let mut tables: Vec<(&[u8; 4], &[u8])> =
        vec![(b"fvar", &fvar), (b"OS/2", &os2), (b"post", &post)];
    if mvar {
        tables.push((b"MVAR", &table));
    }
    sfnt(1, &tables)
}

#[test]
fn mvar_moves_the_line_metrics_with_the_instance() {
    let wght = Tag::from_bytes(b"wght");
    let metrics = |mvar: bool, at: Option<f32>| {
        let bytes = mvar_face(mvar);
        let mut face = Face::parse(&bytes, 0).expect("the synthetic face parses");
        assert!(face.is_variable());
        if let Some(v) = at {
            face.set_variation(wght, v).expect("wght is an axis");
        }
        let ul = face.underline_metrics().expect("post underline");
        (face.ascender(), ul.position, ul.thickness)
    };
    assert_eq!(metrics(true, None), (800, -100, 50));
    assert_eq!(metrics(true, Some(400.0)), (800, -100, 50));
    assert_eq!(metrics(true, Some(650.0)), (850, -50, 50));
    assert_eq!(metrics(true, Some(900.0)), (900, 0, 50));
    assert_eq!(metrics(true, Some(100.0)), (800, -100, 50));
    assert_eq!(metrics(false, Some(900.0)), (800, -100, 50));
}

#[test]
fn a_face_without_os2_takes_its_style_from_mac_style() {
    let face_with = |mac_style: u16| {
        let mut bytes = sfnt(1, &[]);
        let tables = usize::from(u16::from_be_bytes([bytes[4], bytes[5]]));
        let record = (0..tables)
            .map(|index| 12 + 16 * index)
            .find(|record| &bytes[*record..*record + 4] == b"head")
            .expect("the synthetic face has a head table");
        let offset =
            u32::from_be_bytes(bytes[record + 8..record + 12].try_into().unwrap()) as usize;
        bytes[offset + 44..offset + 46].copy_from_slice(&be16(mac_style));
        bytes
    };
    let style = |mac_style: u16| {
        let bytes = face_with(mac_style);
        let face = Face::parse(&bytes, 0).expect("the synthetic face parses");
        assert!(face.tables().os2.is_none());
        (face.is_bold(), face.is_italic(), face.weight().to_number())
    };
    assert_eq!(style(0b00), (false, false, 400));
    assert_eq!(style(0b01), (true, false, 700));
    assert_eq!(style(0b10), (false, true, 400));
    assert_eq!(style(0b11), (true, true, 700));
}
