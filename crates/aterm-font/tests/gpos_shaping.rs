// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Synthetic end-to-end OpenType `GPOS`, `kern`, AAT `kerx`/`ankr`, and fallback shaping tests.

use aterm_font::shape::{Feature, GlyphPosition, ShapeFace, UnicodeBuffer, shape};
use aterm_font::{Face, Tag};

use crate::never_panic::sfnt;

fn be16(v: u16) -> [u8; 2] {
    v.to_be_bytes()
}

fn i16be(v: i16) -> [u8; 2] {
    v.to_be_bytes()
}

fn be32(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}

fn coverage_format1(glyphs: &[u16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + 2 * glyphs.len());
    out.extend(be16(1));
    out.extend(be16(glyphs.len() as u16));
    for &g in glyphs {
        out.extend(be16(g));
    }
    out
}

fn class_def_format1(start_glyph: u16, classes: &[u16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(6 + 2 * classes.len());
    out.extend(be16(1));
    out.extend(be16(start_glyph));
    out.extend(be16(classes.len() as u16));
    for &c in classes {
        out.extend(be16(c));
    }
    out
}

fn anchor_format1(x: i16, y: i16) -> [u8; 6] {
    let [x0, x1] = i16be(x);
    let [y0, y1] = i16be(y);
    [0, 1, x0, x1, y0, y1]
}

/// Minimal `cmap` format 4 mapping sorted `(codepoint <= 0xFFFE, glyph_id)` pairs.
fn cmap_format4(entries: &[(u16, u16)]) -> Vec<u8> {
    let seg_count = entries.len() + 1;
    let seg_count_x2 = (seg_count * 2) as u16;
    let search_range = 2 * (1u16 << seg_count.ilog2());
    let entry_selector = seg_count.ilog2() as u16;
    let range_shift = seg_count_x2 - search_range;

    let mut sub = Vec::new();
    sub.extend(be16(4));
    sub.extend(be16(0)); // filled below
    sub.extend(be16(0));
    sub.extend(be16(seg_count_x2));
    sub.extend(be16(search_range));
    sub.extend(be16(entry_selector));
    sub.extend(be16(range_shift));
    for &(cp, _) in entries {
        sub.extend(be16(cp));
    }
    sub.extend(be16(0xFFFF));
    sub.extend(be16(0));
    for &(cp, _) in entries {
        sub.extend(be16(cp));
    }
    sub.extend(be16(0xFFFF));
    for &(cp, gid) in entries {
        sub.extend(be16(gid.wrapping_sub(cp)));
    }
    sub.extend(be16(1));
    for _ in 0..seg_count {
        sub.extend(be16(0));
    }
    let sub_len = u16::try_from(sub.len()).unwrap();
    sub[2..4].copy_from_slice(&be16(sub_len));

    let mut cmap = Vec::new();
    cmap.extend(be16(0));
    cmap.extend(be16(1));
    cmap.extend(be16(0)); // Unicode platform
    cmap.extend(be16(3));
    cmap.extend(be32(12));
    cmap.extend(sub);
    cmap
}

fn hmtx_and_hhea(advances: &[u16]) -> (Vec<u8>, Vec<u8>) {
    let mut hmtx = Vec::with_capacity(4 * advances.len());
    for &adv in advances {
        hmtx.extend(be16(adv));
        hmtx.extend(be16(0));
    }
    let mut hhea = vec![0u8; 36];
    hhea[0..4].copy_from_slice(&be32(0x0001_0000));
    hhea[34..36].copy_from_slice(&be16(advances.len() as u16));
    (hmtx, hhea)
}

/// Build an SFNT with `cmap`, `hmtx` (`hhea` overridden to full `numberOfHMetrics`), and `extra`.
fn font_with_tables(
    cmap_entries: &[(u16, u16)],
    advances: &[u16],
    extra: &[(&[u8; 4], &[u8])],
) -> Vec<u8> {
    let cmap = cmap_format4(cmap_entries);
    let (hmtx, hhea) = hmtx_and_hhea(advances);
    let mut all: Vec<(&[u8; 4], &[u8])> = vec![(b"cmap", &cmap), (b"hmtx", &hmtx)];
    all.extend_from_slice(extra);
    let mut bytes = sfnt(advances.len(), &all);
    // Patch `hhea.numberOfHMetrics` so every glyph gets its own advance.
    let tables = usize::from(u16::from_be_bytes([bytes[4], bytes[5]]));
    let record = (0..tables)
        .map(|i| 12 + 16 * i)
        .find(|r| &bytes[*r..*r + 4] == b"hhea")
        .expect("hhea record");
    let offset = u32::from_be_bytes(bytes[record + 8..record + 12].try_into().unwrap()) as usize;
    bytes[offset..offset + 36].copy_from_slice(&hhea);
    bytes
}

/// Build a `GSUB` or `GPOS` v1.0 table with one script (`script_tag`), features `(tag, &[lookup_indices])`,
/// and raw `lookups` (`(lookup_type, lookup_flag, subtable_bytes)`).
fn layout_table(
    script_tag: &[u8; 4],
    features: &[(&[u8; 4], &[u16])],
    lookups: &[(u16, u16, Vec<u8>)],
) -> Vec<u8> {
    let mut out = vec![0, 1, 0, 0, 0, 0, 0, 0, 0, 0];

    let script_list = out.len();
    out.extend(be16(1));
    out.extend(script_tag);
    out.extend(be16(8));
    // Script table: defaultLangSys at offset 4, 0 langSysRecords.
    out.extend(be16(4));
    out.extend(be16(0));
    // LangSys: lookupOrderOffset=0, reqFeatureIndex=0xFFFF, all features enabled.
    out.extend(be16(0));
    out.extend(be16(0xFFFF));
    out.extend(be16(features.len() as u16));
    for i in 0..features.len() {
        out.extend(be16(i as u16));
    }

    let feature_list = out.len();
    out.extend(be16(features.len() as u16));
    let feat_records_end = 2 + 6 * features.len();
    let mut feat_bodies = Vec::new();
    for (tag, lookup_indices) in features {
        out.extend(*tag);
        out.extend(be16((feat_records_end + feat_bodies.len()) as u16));
        feat_bodies.extend(be16(0));
        feat_bodies.extend(be16(lookup_indices.len() as u16));
        for &idx in *lookup_indices {
            feat_bodies.extend(be16(idx));
        }
    }
    out.extend(feat_bodies);

    let lookup_list = out.len();
    out.extend(be16(lookups.len() as u16));
    let lookup_offsets_end = 2 + 2 * lookups.len();
    let mut lookup_bodies = Vec::new();
    for (kind, flag, subtable) in lookups {
        out.extend(be16((lookup_offsets_end + lookup_bodies.len()) as u16));
        lookup_bodies.extend(be16(*kind));
        lookup_bodies.extend(be16(*flag));
        lookup_bodies.extend(be16(1));
        lookup_bodies.extend(be16(8));
        lookup_bodies.extend(subtable);
    }
    out.extend(lookup_bodies);

    out[4..6].copy_from_slice(&be16(script_list as u16));
    out[6..8].copy_from_slice(&be16(feature_list as u16));
    out[8..10].copy_from_slice(&be16(lookup_list as u16));
    out
}

fn shape_positions(
    bytes: &[u8],
    coords: &[(&[u8; 4], f32)],
    text: &str,
) -> Vec<(u32, GlyphPosition)> {
    let mut face = Face::parse(bytes, 0).expect("synthetic face parses");
    for &(tag, val) in coords {
        face.set_variation(Tag::from_bytes(tag), val)
            .expect("variation axis exists");
    }
    let shape_face = ShapeFace::new(face);
    let mut buf = UnicodeBuffer::new();
    buf.push_str(text);
    let out = shape(&shape_face, &[], buf);
    out.glyph_infos()
        .iter()
        .map(|i| i.glyph_id)
        .zip(out.glyph_positions().iter().copied())
        .collect()
}

#[test]
fn single_and_pair_positioning_adjust_advances_and_chain_pairs() {
    // Glyphs: 0=.notdef, 1='A' (600), 2='V' (600), 3='W' (700), 4='o' (500).
    // Lookup 0 (`dist`, SinglePos Format 2): 'W' x_offset += 15, x_advance += 25.
    let mut single_f2 = Vec::new();
    single_f2.extend(be16(2));
    single_f2.extend(be16(12)); // coverage offset
    single_f2.extend(be16(0x0005)); // X_PLACEMENT | X_ADVANCE
    single_f2.extend(be16(1));
    single_f2.extend(i16be(15));
    single_f2.extend(i16be(25));
    single_f2.extend(coverage_format1(&[3]));

    // Lookup 1 (`kern`, PairPos Format 1):
    //   Pair (1='A', 2='V'): record1 x_advance = -80, record2 = 0 (empty -> idx advances to 'V' so ('V','A') also kerns!).
    //   Pair (2='V', 1='A'): record1 x_advance = -70, record2 = 0.
    //   Pair (1='A', 3='W'): record1 x_advance = -50, record2 x_advance = -30 (non-empty -> consumes both, skipping ('W', ...)).
    let mut pair_f1 = Vec::new();
    pair_f1.extend(be16(1));
    pair_f1.extend(be16(14)); // coverage at 14
    pair_f1.extend(be16(0x0004)); // valueFormat1 = X_ADVANCE
    pair_f1.extend(be16(0x0004)); // valueFormat2 = X_ADVANCE
    pair_f1.extend(be16(2)); // 2 PairSets (for glyph 1 and glyph 2)
    let cov = coverage_format1(&[1, 2]);
    let set0_off = 14 + cov.len();
    let mut set0 = Vec::new();
    set0.extend(be16(2)); // 2 pairs from 'A': ('A','V') and ('A','W')
    set0.extend(be16(2));
    set0.extend(i16be(-80));
    set0.extend(i16be(0));
    set0.extend(be16(3));
    set0.extend(i16be(-50));
    set0.extend(i16be(-30));
    let set1_off = set0_off + set0.len();
    let mut set1 = Vec::new();
    set1.extend(be16(1)); // 1 pair from 'V': ('V','A')
    set1.extend(be16(1));
    set1.extend(i16be(-70));
    set1.extend(i16be(0));
    pair_f1.extend(be16(set0_off as u16));
    pair_f1.extend(be16(set1_off as u16));
    pair_f1.extend(cov);
    pair_f1.extend(set0);
    pair_f1.extend(set1);

    // Lookup 2 (`kern`, PairPos Format 2): Class1(3='W')->1, Class2(4='o')->1 => x_advance1 = -90.
    let mut pair_f2 = Vec::new();
    let cov2 = coverage_format1(&[3]);
    let cdef1 = class_def_format1(3, &[1]);
    let cdef2 = class_def_format1(4, &[1]);
    // Header is 16 bytes + 4 class records * 2 bytes = 24 bytes.
    let cov2_off = 24;
    let cdef1_off = cov2_off + cov2.len();
    let cdef2_off = cdef1_off + cdef1.len();
    pair_f2.extend(be16(2));
    pair_f2.extend(be16(cov2_off as u16));
    pair_f2.extend(be16(0x0004)); // X_ADVANCE
    pair_f2.extend(be16(0x0000));
    pair_f2.extend(be16(cdef1_off as u16));
    pair_f2.extend(be16(cdef2_off as u16));
    pair_f2.extend(be16(2));
    pair_f2.extend(be16(2));
    // Matrix (2x2): (0,0)=0, (0,1)=0, (1,0)=0, (1,1)=-90.
    for v in [0i16, 0, 0, -90] {
        pair_f2.extend(i16be(v));
    }
    pair_f2.extend(cov2);
    pair_f2.extend(cdef1);
    pair_f2.extend(cdef2);

    let gpos = layout_table(
        b"latn",
        &[(b"dist", &[0]), (b"kern", &[1, 2])],
        &[(1, 0, single_f2), (2, 0, pair_f1), (2, 0, pair_f2)],
    );
    let bytes = font_with_tables(
        &[
            (b'A' as u16, 1),
            (b'V' as u16, 2),
            (b'W' as u16, 3),
            (b'o' as u16, 4),
        ],
        &[500, 600, 600, 700, 500],
        &[(b"GPOS", &gpos)],
    );

    // "AVA": ('A','V') has empty record2 so idx advances by 1 and ('V','A') also kerns!
    let ava = shape_positions(&bytes, &[], "AVA");
    assert_eq!(
        ava.iter().map(|(_, p)| p.x_advance).collect::<Vec<_>>(),
        [520, 530, 600]
    );

    // "AWo": ('A','W') has non-empty record2 (-30) so Lookup 1 consumes both 'A' and 'W',
    // then Lookup 2 (`PairPosFormat2`) kerns ('W','o') by -90 on top of SinglePos (+25, x_offset +15)!
    let awo = shape_positions(&bytes, &[], "AWo");
    assert_eq!(
        awo,
        [
            (
                1,
                GlyphPosition {
                    x_advance: 550,
                    y_advance: 0,
                    x_offset: 0,
                    y_offset: 0
                }
            ),
            (
                3,
                GlyphPosition {
                    x_advance: 700 + 25 - 30 - 90,
                    y_advance: 0,
                    x_offset: 15,
                    y_offset: 0
                }
            ),
            (
                4,
                GlyphPosition {
                    x_advance: 500,
                    y_advance: 0,
                    x_offset: 0,
                    y_offset: 0
                }
            ),
        ]
    );
}

#[test]
fn pair_set_variation_device_scales_with_coordinates() {
    // Variable `wght` 400..900 (default 400).
    let fixed = |v: i32| be32((v << 16) as u32);
    let mut fvar = be32(0x0001_0000).to_vec();
    for v in [16u16, 2, 1, 20, 0, 8] {
        fvar.extend(be16(v));
    }
    fvar.extend(b"wght");
    fvar.extend(fixed(400));
    fvar.extend(fixed(400));
    fvar.extend(fixed(900));
    fvar.extend([0, 0, 1, 0]);

    // GDEF v1.3 with an ItemVariationStore at (outer=0, inner=0) giving +100 at wght=900 (+1.0).
    let mut gdef = Vec::new();
    gdef.extend(be32(0x0001_0003));
    gdef.extend(be16(0)); // glyphClassDef
    gdef.extend(be16(0)); // attachList
    gdef.extend(be16(0)); // ligCaretList
    gdef.extend(be16(0)); // markAttachClassDef
    gdef.extend(be16(0)); // markGlyphSetsDef
    gdef.extend(be32(18)); // itemVarStoreOffset = 18
    // ItemVariationStore at offset 18:
    gdef.extend(be16(1)); // format = 1
    gdef.extend(be32(12)); // variationRegionListOffset = 12
    gdef.extend(be16(1)); // itemVariationDataCount = 1
    gdef.extend(be32(22)); // itemVariationDataOffsets[0] = 22
    // VariationRegionList (10 bytes): axisCount=1, regionCount=1, start=0.0, peak=1.0, end=1.0.
    gdef.extend(be16(1));
    gdef.extend(be16(1));
    gdef.extend([0, 0, 0x40, 0, 0x40, 0]);
    // ItemVariationData (10 bytes): itemCount=1, wordDeltaCount=1, regionIndexCount=1, regionIndices=[0], delta=+100.
    for v in [1u16, 1, 1, 0] {
        gdef.extend(be16(v));
    }
    gdef.extend(i16be(100));

    // PairPos Format 1 on ('A'=1, '!'=2) where `x_advance = 0` and `x_advance_device` sits AFTER
    // `pairValueRecords` inside `PairSet` (exercising `PairSet` offset resolution from `PairSet` start).
    let cov = coverage_format1(&[1]);
    let mut pair_set = Vec::new();
    pair_set.extend(be16(1)); // pairValueCount = 1
    pair_set.extend(be16(2)); // secondGlyph = 2
    pair_set.extend(i16be(0)); // x_advance = 0
    pair_set.extend(be16(8)); // x_advance_device offset = 8 (right after this PairValueRecord!)
    // VariationIndex table at offset 8 of PairSet: outer=0, inner=0, deltaFormat=0x8000.
    pair_set.extend(be16(0));
    pair_set.extend(be16(0));
    pair_set.extend(be16(0x8000));

    let mut sub = Vec::new();
    sub.extend(be16(1));
    sub.extend(be16(12)); // coverage at 12
    sub.extend(be16(0x0044)); // X_ADVANCE | X_ADVANCE_DEVICE
    sub.extend(be16(0x0000));
    sub.extend(be16(1));
    sub.extend(be16((12 + cov.len()) as u16));
    sub.extend(cov);
    sub.extend(pair_set);

    // Empty `HVAR` variation store so `hmtx` base advances stand unchanged at non-default
    // coordinates while `GPOS` `VariationDevice` supplies the entire delta.
    let mut hvar = Vec::new();
    hvar.extend(be32(0x0001_0000));
    hvar.extend(be32(20)); // variationStoreOffset
    hvar.extend([0u8; 12]); // advanceWidth / lsb / rsb mapping offsets = 0
    hvar.extend(be16(1)); // ItemVariationStore format = 1
    hvar.extend(be32(8)); // variationRegionListOffset = 8
    hvar.extend(be16(0)); // itemVariationDataCount = 0
    hvar.extend(be16(1)); // axisCount = 1
    hvar.extend(be16(0)); // regionCount = 0

    let gpos = layout_table(b"latn", &[(b"kern", &[0])], &[(2, 0, sub)]);
    let bytes = font_with_tables(
        &[(b'!' as u16, 2), (b'A' as u16, 1)],
        &[500, 600, 300],
        &[
            (b"fvar", &fvar),
            (b"HVAR", &hvar),
            (b"GDEF", &gdef),
            (b"GPOS", &gpos),
        ],
    );

    assert_eq!(shape_positions(&bytes, &[], "A!")[0].1.x_advance, 600);
    assert_eq!(
        shape_positions(&bytes, &[(b"wght", 650.0)], "A!")[0]
            .1
            .x_advance,
        650
    );
    assert_eq!(
        shape_positions(&bytes, &[(b"wght", 900.0)], "A!")[0]
            .1
            .x_advance,
        700
    );
}

#[test]
fn mark_to_base_mark_to_mark_and_mark_to_ligature_attach_and_zero_advances() {
    // Glyphs:
    //   0 = .notdef
    //   1 = 'a' (Base, adv 600)
    //   2 = 'f' (Base, adv 300)
    //   3 = 'i' (Base, adv 300)
    //   4 = U+0301 acute (Mark, hmtx adv 200 -> zeroed after GPOS!)
    //   5 = U+0302 circumflex (Mark, hmtx adv 200 -> zeroed after GPOS!)
    //   6 = 'fi' ligature (Ligature, adv 580)
    let mut gdef = Vec::new();
    gdef.extend(be32(0x0001_0000));
    gdef.extend(be16(12)); // glyphClassDef at 12
    gdef.extend(be16(0));
    gdef.extend(be16(0));
    gdef.extend(be16(0));
    // Classes for glyphs 1..=6: Base=1, Ligature=2, Mark=3.
    gdef.extend(class_def_format1(1, &[1, 1, 1, 3, 3, 2]));

    // GSUB `liga`: 'f' (2) + 'i' (3) -> 'fi' (6), ignoring marks (lookupFlag = 0x0008 IGNORE_MARKS).
    let mut lig_sub = Vec::new();
    lig_sub.extend(be16(1)); // format 1
    lig_sub.extend(be16(8)); // coverage at 8
    lig_sub.extend(be16(1)); // ligSetCount = 1
    lig_sub.extend(be16(14)); // ligSet[0] at 14 (coverage is 6 bytes: 8..14)
    lig_sub.extend(coverage_format1(&[2]));
    lig_sub.extend(be16(1)); // 1 ligature in set
    lig_sub.extend(be16(4)); // ligature table at +4
    lig_sub.extend(be16(6)); // ligGlyph = 6
    lig_sub.extend(be16(2)); // componentCount = 2
    lig_sub.extend(be16(3)); // component[1] = 3
    let gsub = layout_table(b"latn", &[(b"liga", &[0])], &[(4, 0x0008, lig_sub)]);

    // GPOS Lookup 0 (`mark`, MarkToBase Format 1):
    //   marks: [4, 5], both class 0, mark anchor (50, 100).
    //   bases: [1 ('a')], base anchor (300, 500) => mark offset rel to base = (250, 400), minus base adv 600 => x_offset = -350, y_offset = 400!
    let mark_cov = coverage_format1(&[4, 5]);
    let base_cov = coverage_format1(&[1]);
    let mut mark_array = Vec::new();
    mark_array.extend(be16(2));
    mark_array.extend(be16(0));
    mark_array.extend(be16(10)); // anchor at 10
    mark_array.extend(be16(0));
    mark_array.extend(be16(10));
    mark_array.extend(anchor_format1(50, 100));
    let mut base_array = Vec::new();
    base_array.extend(be16(1));
    base_array.extend(be16(4));
    base_array.extend(anchor_format1(300, 500));

    let mut m2b = Vec::new();
    let m_cov_off = 12;
    let b_cov_off = m_cov_off + mark_cov.len();
    let m_arr_off = b_cov_off + base_cov.len();
    let b_arr_off = m_arr_off + mark_array.len();
    m2b.extend(be16(1));
    m2b.extend(be16(m_cov_off as u16));
    m2b.extend(be16(b_cov_off as u16));
    m2b.extend(be16(1));
    m2b.extend(be16(m_arr_off as u16));
    m2b.extend(be16(b_arr_off as u16));
    m2b.extend(&mark_cov);
    m2b.extend(base_cov);
    m2b.extend(&mark_array);
    m2b.extend(base_array);

    // GPOS Lookup 1 (`mark`, MarkToLigature Format 1):
    //   marks: [4, 5], mark anchor (50, 100).
    //   ligature: [6 ('fi')], 2 components: comp 0 anchor (150, 520), comp 1 anchor (450, 540).
    //   With fi adv = 580:
    //     mark on 'f' (comp 0): (150 - 50) - 580 = -480, y = 520 - 100 = 420.
    //     mark on 'i' (comp 1): (450 - 50) - 580 = -180, y = 540 - 100 = 440.
    let lig_cov = coverage_format1(&[6]);
    let mut lig_attach = Vec::new();
    lig_attach.extend(be16(2)); // 2 components, 1 mark class -> 2 offsets (6 bytes header)
    lig_attach.extend(be16(6));
    lig_attach.extend(be16(12));
    lig_attach.extend(anchor_format1(150, 520));
    lig_attach.extend(anchor_format1(450, 540));
    let mut lig_array = Vec::new();
    lig_array.extend(be16(1));
    lig_array.extend(be16(4));
    lig_array.extend(lig_attach);

    let mut m2l = Vec::new();
    let l_cov_off = 12 + mark_cov.len();
    let lm_arr_off = l_cov_off + lig_cov.len();
    let l_arr_off = lm_arr_off + mark_array.len();
    m2l.extend(be16(1));
    m2l.extend(be16(12));
    m2l.extend(be16(l_cov_off as u16));
    m2l.extend(be16(1));
    m2l.extend(be16(lm_arr_off as u16));
    m2l.extend(be16(l_arr_off as u16));
    m2l.extend(&mark_cov);
    m2l.extend(lig_cov);
    m2l.extend(&mark_array);
    m2l.extend(lig_array);

    // GPOS Lookup 2 (`mkmk`, MarkToMark Format 1):
    //   mark1 = [5], anchor (10, 20); mark2 (base mark) = [4], anchor (30, 180).
    //   Relative to mark 4: dx = 30 - 10 = +20, dy = 180 - 20 = +160.
    //   Propagated on top of mark 4's (-350, 400) with mark 4's advance zeroed: (-330, 560)!
    let m1_cov = coverage_format1(&[5]);
    let m2_cov = coverage_format1(&[4]);
    let mut m1_arr = Vec::new();
    m1_arr.extend(be16(1));
    m1_arr.extend(be16(0));
    m1_arr.extend(be16(6));
    m1_arr.extend(anchor_format1(10, 20));
    let mut m2_arr = Vec::new();
    m2_arr.extend(be16(1));
    m2_arr.extend(be16(4));
    m2_arr.extend(anchor_format1(30, 180));

    let mut m2m = Vec::new();
    let m2_cov_off = 12 + m1_cov.len();
    let m1_arr_off = m2_cov_off + m2_cov.len();
    let m2_arr_off = m1_arr_off + m1_arr.len();
    m2m.extend(be16(1));
    m2m.extend(be16(12));
    m2m.extend(be16(m2_cov_off as u16));
    m2m.extend(be16(1));
    m2m.extend(be16(m1_arr_off as u16));
    m2m.extend(be16(m2_arr_off as u16));
    m2m.extend(m1_cov);
    m2m.extend(m2_cov);
    m2m.extend(m1_arr);
    m2m.extend(m2_arr);

    let gpos = layout_table(
        b"latn",
        &[(b"mark", &[0, 1]), (b"mkmk", &[2])],
        &[(4, 0, m2b), (5, 0, m2l), (6, 0, m2m)],
    );
    let bytes = font_with_tables(
        &[
            (b'a' as u16, 1),
            (b'f' as u16, 2),
            (b'i' as u16, 3),
            (0x0301, 4),
            (0x0302, 5),
        ],
        &[500, 600, 300, 300, 200, 200, 580],
        &[(b"GDEF", &gdef), (b"GSUB", &gsub), (b"GPOS", &gpos)],
    );

    // Base + mark + stacked mark:
    let stacked = shape_positions(&bytes, &[], "a\u{0301}\u{0302}");
    assert_eq!(
        stacked,
        [
            (
                1,
                GlyphPosition {
                    x_advance: 600,
                    y_advance: 0,
                    x_offset: 0,
                    y_offset: 0
                }
            ),
            (
                4,
                GlyphPosition {
                    x_advance: 0,
                    y_advance: 0,
                    x_offset: -350,
                    y_offset: 400
                }
            ),
            (
                5,
                GlyphPosition {
                    x_advance: 0,
                    y_advance: 0,
                    x_offset: -330,
                    y_offset: 560
                }
            ),
        ]
    );

    // Ligature `fi` with one mark on component 0 ('f') and one mark on component 1 ('i'):
    let lig_marks = shape_positions(&bytes, &[], "f\u{0301}i\u{0302}");
    assert_eq!(
        lig_marks,
        [
            (
                6,
                GlyphPosition {
                    x_advance: 580,
                    y_advance: 0,
                    x_offset: 0,
                    y_offset: 0
                }
            ),
            (
                4,
                GlyphPosition {
                    x_advance: 0,
                    y_advance: 0,
                    x_offset: -480,
                    y_offset: 420
                }
            ),
            (
                5,
                GlyphPosition {
                    x_advance: 0,
                    y_advance: 0,
                    x_offset: -180,
                    y_offset: 440
                }
            ),
        ]
    );
}

#[test]
fn cursive_attachment_propagates_offsets_and_hebrew_gates_on_script_tag() {
    // Glyphs:
    //   1='a' (adv 500, exit=(480, 120))
    //   2='b' (adv 500, entry=(30, 40), exit=(460, 90))
    //   3='c' (adv 500, entry=(20, 30))
    // Forward LTR cursive (`lookupFlag = 0` anchors the leading glyph on the baseline):
    //   pair (a, b): x_adv[0]=480, x_adv[1]=470, x_off[1]=-30, y_off[1] = 120 - 40 = +80 (parent=0).
    //   pair (b, c): x_adv[1]=460+(-30)=430, x_adv[2]=480, x_off[2]=-20,
    //                local y_off[2] = 90 - 30 = +60 (parent=1), propagated +80 => +140!
    let cov = coverage_format1(&[1, 2, 3]);
    let mut curs = Vec::new();
    let cov_off = 6 + 3 * 4;
    let exit0_off = cov_off + cov.len();
    let entry1_off = exit0_off + 6;
    let exit1_off = entry1_off + 6;
    let entry2_off = exit1_off + 6;
    curs.extend(be16(1));
    curs.extend(be16(cov_off as u16));
    curs.extend(be16(3));
    curs.extend(be16(0));
    curs.extend(be16(exit0_off as u16));
    curs.extend(be16(entry1_off as u16));
    curs.extend(be16(exit1_off as u16));
    curs.extend(be16(entry2_off as u16));
    curs.extend(be16(0));
    curs.extend(cov);
    curs.extend(anchor_format1(480, 120));
    curs.extend(anchor_format1(30, 40));
    curs.extend(anchor_format1(460, 90));
    curs.extend(anchor_format1(20, 30));

    let gpos_curs = layout_table(b"DFLT", &[(b"curs", &[0])], &[(3, 0, curs.clone())]);
    let bytes_curs = font_with_tables(
        &[(b'a' as u16, 1), (b'b' as u16, 2), (b'c' as u16, 3)],
        &[500, 500, 500, 500],
        &[(b"GPOS", &gpos_curs)],
    );
    assert_eq!(
        shape_positions(&bytes_curs, &[], "abc"),
        [
            (
                1,
                GlyphPosition {
                    x_advance: 480,
                    y_advance: 0,
                    x_offset: 0,
                    y_offset: 0
                }
            ),
            (
                2,
                GlyphPosition {
                    x_advance: 430,
                    y_advance: 0,
                    x_offset: -30,
                    y_offset: 80
                }
            ),
            (
                3,
                GlyphPosition {
                    x_advance: 480,
                    y_advance: 0,
                    x_offset: -20,
                    y_offset: 140
                }
            ),
        ]
    );

    // With `RIGHT_TO_LEFT` (`0x0001`), the trailing glyph stays on the baseline (`y_offset = 0`)
    // and earlier glyphs accumulate `entry_y - exit_y` (-60 on `b`, -80 + -60 = -140 on `a`).
    let gpos_curs_rtl_flag = layout_table(b"DFLT", &[(b"curs", &[0])], &[(3, 0x0001, curs)]);
    let bytes_curs_rtl_flag = font_with_tables(
        &[(b'a' as u16, 1), (b'b' as u16, 2), (b'c' as u16, 3)],
        &[500, 500, 500, 500],
        &[(b"GPOS", &gpos_curs_rtl_flag)],
    );
    assert_eq!(
        shape_positions(&bytes_curs_rtl_flag, &[], "abc")
            .into_iter()
            .map(|(_, p)| p.y_offset)
            .collect::<Vec<_>>(),
        [-140, -60, 0]
    );

    // Hebrew script gate: a `DFLT`-only `GPOS` is disabled for Hebrew runs, whereas `hebr` applies.
    let mut heb_single = Vec::new();
    heb_single.extend(be16(1));
    heb_single.extend(be16(8));
    heb_single.extend(be16(0x0004));
    heb_single.extend(i16be(-60));
    heb_single.extend(coverage_format1(&[1]));

    let dflt_only = layout_table(b"DFLT", &[(b"kern", &[0])], &[(1, 0, heb_single.clone())]);
    let hebr_tagged = layout_table(b"hebr", &[(b"kern", &[0])], &[(1, 0, heb_single)]);
    let dflt_bytes = font_with_tables(
        &[(0x05D0, 1), (0x05D1, 2)],
        &[500, 600, 600],
        &[(b"GPOS", &dflt_only)],
    );
    let hebr_bytes = font_with_tables(
        &[(0x05D0, 1), (0x05D1, 2)],
        &[500, 600, 600],
        &[(b"GPOS", &hebr_tagged)],
    );
    // RTL buffer reverses at end, so `\u{05D0}` (glyph 1) is the second glyph in visual order.
    assert_eq!(
        shape_positions(&dflt_bytes, &[], "\u{05D0}\u{05D1}")[1]
            .1
            .x_advance,
        600,
        "DFLT-only GPOS is disabled for Hebrew"
    );
    assert_eq!(
        shape_positions(&hebr_bytes, &[], "\u{05D0}\u{05D1}")[1]
            .1
            .x_advance,
        540,
        "hebr-script GPOS applies for Hebrew"
    );
}

fn shape_with_features(
    bytes: &[u8],
    features: &[Feature],
    text: &str,
) -> Vec<(u32, GlyphPosition)> {
    let face = Face::parse(bytes, 0).expect("synthetic face parses");
    let shape_face = ShapeFace::new(face);
    let mut buf = UnicodeBuffer::new();
    buf.push_str(text);
    let out = shape(&shape_face, features, buf);
    out.glyph_infos()
        .iter()
        .map(|i| i.glyph_id)
        .zip(out.glyph_positions().iter().copied())
        .collect()
}

fn ot_kern_f0_subtable(coverage: u8, pairs: &[(u16, u16, i16)]) -> Vec<u8> {
    let mut sub = Vec::new();
    let len = 6 + 8 + 6 * pairs.len();
    sub.extend(be16(0));
    sub.extend(be16(len as u16));
    sub.push(0); // format 0
    sub.push(coverage);
    sub.extend(be16(pairs.len() as u16));
    sub.extend([0u8; 6]);
    for &(l, r, v) in pairs {
        sub.extend(be16(l));
        sub.extend(be16(r));
        sub.extend(i16be(v));
    }
    sub
}

fn ot_kern_f2_single_pair(left_gid: u16, right_gid: u16, value: i16) -> Vec<u8> {
    // Subtable header is 6 bytes; Format 2 body is 28 bytes (total 34 bytes).
    let mut sub = Vec::new();
    sub.extend(be16(0));
    sub.extend(be16(34));
    sub.push(2); // format 2
    sub.push(0x01); // horizontal
    sub.extend(be16(4)); // row_width = 2 cols * 2 bytes
    sub.extend(be16(6 + 8)); // left class table at subtable offset 14
    sub.extend(be16(6 + 14)); // right class table at subtable offset 20
    sub.extend(be16(6 + 20)); // array at subtable offset 26
    // Left class table (6 bytes): row 1 starts at subtable byte offset 26 + 4 = 30.
    sub.extend(be16(left_gid));
    sub.extend(be16(1));
    sub.extend(be16(30));
    // Right class table (6 bytes): col 1 is byte offset +2.
    sub.extend(be16(right_gid));
    sub.extend(be16(1));
    sub.extend(be16(2));
    // 2x2 kerning array (8 bytes): (0,0)=0, (0,1)=0, (1,0)=0, (1,1)=value.
    for v in [0i16, 0, 0, value] {
        sub.extend(i16be(v));
    }
    sub
}

fn ot_kern_table(subtables: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend(be16(0));
    out.extend(be16(subtables.len() as u16));
    for sub in subtables {
        out.extend(sub);
    }
    out
}

#[test]
fn kern_formats_cross_stream_opt_out_and_gpos_fallback() {
    // Glyphs: 1='A'(600), 2='V'(600), 3='W'(700), 4=U+0301(Mark, 200), 5=U+05D0(600), 6=U+05D1(600).
    let mut gdef = Vec::new();
    gdef.extend(be32(0x0001_0000));
    gdef.extend(be16(12));
    gdef.extend([0u8; 6]);
    gdef.extend(class_def_format1(1, &[1, 1, 1, 3, 1, 1]));

    let f0 = ot_kern_f0_subtable(0x01, &[(1, 2, -80), (2, 1, -70), (6, 5, -60)]);
    let f2 = ot_kern_f2_single_pair(1, 3, -50);
    let kern = ot_kern_table(&[f0, f2]);
    let cmap = [
        (b'A' as u16, 1),
        (b'V' as u16, 2),
        (b'W' as u16, 3),
        (0x0301, 4),
        (0x05D0, 5),
        (0x05D1, 6),
    ];
    let advances = [500, 600, 600, 700, 200, 600, 600];
    let bytes = font_with_tables(&cmap, &advances, &[(b"GDEF", &gdef), (b"kern", &kern)]);

    // Format 0 splits (-80 -> -40/-40) and (-70 -> -35/-35) across adjacent pairs in "AVA".
    assert_eq!(
        shape_positions(&bytes, &[], "AVA"),
        [
            (
                1,
                GlyphPosition {
                    x_advance: 560,
                    y_advance: 0,
                    x_offset: 0,
                    y_offset: 0
                }
            ),
            (
                2,
                GlyphPosition {
                    x_advance: 525,
                    y_advance: 0,
                    x_offset: -40,
                    y_offset: 0
                }
            ),
            (
                1,
                GlyphPosition {
                    x_advance: 565,
                    y_advance: 0,
                    x_offset: -35,
                    y_offset: 0
                }
            ),
        ]
    );
    // Format 2 kerns ('A','W') by -50 (-25/-25).
    assert_eq!(
        shape_positions(&bytes, &[], "AW")
            .into_iter()
            .map(|(_, p)| p.x_advance)
            .collect::<Vec<_>>(),
        [575, 675]
    );
    // Marks are ignored between kerning bases and their advances are zeroed.
    assert_eq!(
        shape_positions(&bytes, &[], "A\u{0301}V")
            .into_iter()
            .map(|(g, p)| (g, p.x_advance))
            .collect::<Vec<_>>(),
        [(1, 560), (4, 0), (2, 560)]
    );

    // `kern=0` opts out in both LTR and RTL without corrupting RTL visual glyph order.
    let kern_off = [Feature::new(Tag::from_bytes(b"kern"), 0)];
    assert_eq!(
        shape_with_features(&bytes, &kern_off, "AVA")
            .into_iter()
            .map(|(_, p)| p.x_advance)
            .collect::<Vec<_>>(),
        [600, 600, 600]
    );
    assert_eq!(
        shape_positions(&bytes, &[], "\u{05D0}\u{05D1}")
            .into_iter()
            .map(|(g, p)| (g, p.x_advance))
            .collect::<Vec<_>>(),
        [(6, 570), (5, 570)]
    );
    assert_eq!(
        shape_with_features(&bytes, &kern_off, "\u{05D0}\u{05D1}")
            .into_iter()
            .map(|(g, p)| (g, p.x_advance))
            .collect::<Vec<_>>(),
        [(6, 600), (5, 600)],
        "RTL kern=0 must preserve visual glyph order [6, 5] while skipping kerning"
    );

    // Cross-stream `kern` (`0x05` = horizontal + cross-stream) shifts `y_offset`.
    let xstream = ot_kern_table(&[ot_kern_f0_subtable(0x05, &[(1, 2, -45)])]);
    let xstream_bytes = font_with_tables(&cmap, &advances, &[(b"kern", &xstream)]);
    assert_eq!(
        shape_positions(&xstream_bytes, &[], "AV")[1].1.y_offset,
        -45
    );

    // `GPOS` without `kern` feature still falls back to `kern`, whereas `GPOS` with `kern` suppresses it.
    let mut single_a = Vec::new();
    single_a.extend(be16(1));
    single_a.extend(be16(8));
    single_a.extend(be16(0x0004));
    single_a.extend(i16be(10));
    single_a.extend(coverage_format1(&[1]));
    let gpos_dist_only = layout_table(b"latn", &[(b"dist", &[0])], &[(1, 0, single_a.clone())]);
    let gpos_with_kern = layout_table(b"latn", &[(b"kern", &[0])], &[(1, 0, single_a)]);
    let fallback_bytes = font_with_tables(
        &cmap,
        &advances,
        &[(b"GPOS", &gpos_dist_only), (b"kern", &kern)],
    );
    let suppressed_bytes = font_with_tables(
        &cmap,
        &advances,
        &[(b"GPOS", &gpos_with_kern), (b"kern", &kern)],
    );
    assert_eq!(
        shape_positions(&fallback_bytes, &[], "AV")
            .into_iter()
            .map(|(_, p)| p.x_advance)
            .collect::<Vec<_>>(),
        [600 + 10 - 40, 600 - 40]
    );
    assert_eq!(
        shape_positions(&suppressed_bytes, &[], "AV")
            .into_iter()
            .map(|(_, p)| p.x_advance)
            .collect::<Vec<_>>(),
        [600 + 10, 600]
    );
}

#[test]
fn aat_kern_format1_state_machine_cycles_and_zero_classes_never_panic() {
    let build_aat_kern_f1 = |num_classes: u16, entry1_flags: u16| -> Vec<u8> {
        let mut data = Vec::new();
        data.extend(be16(num_classes));
        data.extend(be16(10)); // class_table_offset
        data.extend(be16(16)); // state_array_offset
        data.extend(be16(28)); // entry_table_offset
        data.extend(be16(40)); // values_offset
        // Class table (6 bytes at 10..16): glyph 1 ('A') -> 4, glyph 2 ('V') -> 5.
        data.extend(be16(1));
        data.extend(be16(2));
        data.extend([4u8, 5]);
        // State array (12 bytes at 16..28, 2 states * 6 classes):
        data.extend([0u8, 0, 0, 0, 1, 0]); // State 0
        data.extend([0u8, 0, 0, 0, 1, 2]); // State 1
        // Entry table (12 bytes at 28..40):
        let state0_off = 16u16;
        let state1_off = 16u16 + num_classes;
        data.extend(be16(state0_off));
        data.extend(be16(0));
        data.extend(be16(state1_off));
        data.extend(be16(entry1_flags));
        data.extend(be16(state0_off));
        data.extend(be16(40)); // value_offset = 40
        // Value list at 40..42: -96 with bit 0 set (`-95`) marking end of list.
        data.extend(i16be(-95));

        let mut kern = Vec::new();
        kern.extend(be32(0x0001_0000)); // AAT kern v1.0
        kern.extend(be32(1));
        kern.extend(be32((8 + data.len()) as u32));
        kern.push(0x00); // horizontal
        kern.push(1); // format 1
        kern.extend(be16(0));
        kern.extend(data);
        kern
    };

    let cmap = [(b'A' as u16, 1), (b'V' as u16, 2)];
    let advances = [500, 600, 600];

    // Normal PUSH (0x8000) kerns pushed 'A' by -96 (`x_advance = 504`, `x_offset = -96`).
    let normal = font_with_tables(
        &cmap,
        &advances,
        &[(b"kern", &build_aat_kern_f1(6, 0x8000))],
    );
    assert_eq!(
        shape_positions(&normal, &[], "AV"),
        [
            (
                1,
                GlyphPosition {
                    x_advance: 504,
                    y_advance: 0,
                    x_offset: -96,
                    y_offset: 0
                }
            ),
            (
                2,
                GlyphPosition {
                    x_advance: 600,
                    y_advance: 0,
                    x_offset: 0,
                    y_offset: 0
                }
            ),
        ]
    );

    // Cyclic `DONT_ADVANCE` (`0x4000`) terminates under `max_ops`.
    let cyclic = font_with_tables(
        &cmap,
        &advances,
        &[(b"kern", &build_aat_kern_f1(6, 0x4000))],
    );
    assert_eq!(shape_positions(&cyclic, &[], "AV").len(), 2);

    // Malformed `number_of_classes = 0` must not divide by zero in `StateTable::new_state`.
    let zero_cls = font_with_tables(
        &cmap,
        &advances,
        &[(b"kern", &build_aat_kern_f1(0, 0x8000))],
    );
    assert_eq!(shape_positions(&zero_cls, &[], "AV").len(), 2);
}

/// Synthetic SFNT carrying `kerx` Formats 0, 1, 2, 4, and 6 plus `ankr` and `GDEF`,
/// shared by `aat_kerx_formats_0_1_2_4_6_and_ankr_anchor_attachment` and `never_panic::seeds`.
pub(crate) fn kerx_ankr_seed_face() -> Vec<u8> {
    // Glyphs: 0=.notdef(500), 1='a'(500), 2='b'(500), 3='A'(600), 4='V'(600), 5='e'(500), 6=U+0301(Mark, 180).
    let mut gdef = Vec::new();
    gdef.extend(be32(0x0001_0000));
    gdef.extend(be16(12));
    gdef.extend([0u8; 6]);
    gdef.extend(class_def_format1(1, &[1, 1, 1, 1, 1, 3]));

    // `ankr` table: glyph 5 ('e') -> [(250, 480)], glyph 6 (U+0301) -> [(40, 90)], others -> [].
    let mut ankr = Vec::new();
    ankr.extend(be16(0)); // version
    ankr.extend(be16(0)); // reserved
    ankr.extend(be32(12)); // lookup_offset
    ankr.extend(be32(28)); // glyphs_offset (12 + 16)
    // AAT Lookup Format 0 (2 + 7 * 2 = 16 bytes):
    ankr.extend(be16(0));
    for off in [16u16, 16, 16, 16, 16, 0, 8] {
        ankr.extend(be16(off));
    }
    // Glyph anchor records at 28..48:
    ankr.extend(be32(1));
    ankr.extend(i16be(250));
    ankr.extend(i16be(480));
    ankr.extend(be32(1));
    ankr.extend(i16be(40));
    ankr.extend(i16be(90));
    ankr.extend(be32(0));

    let kerx_sub = |format: u8, data: &[u8]| -> Vec<u8> {
        let mut s = Vec::with_capacity(12 + data.len());
        s.extend(be32((12 + data.len()) as u32));
        s.push(0x00); // horizontal
        s.extend(be16(0));
        s.push(format);
        s.extend(be32(0)); // tuple_count
        s.extend_from_slice(data);
        s
    };

    // Subtable 0: Format 0 pair ('a'=1, 'b'=2) -> -50.
    let mut f0 = Vec::new();
    f0.extend(be32(1));
    f0.extend([0u8; 12]);
    f0.extend(be16(1));
    f0.extend(be16(2));
    f0.extend(i16be(-50));

    // Subtable 1: Format 1 state machine ('b'=2, 'a'=1) -> -40 (`-39` with end-of-list bit).
    let mut f1 = Vec::new();
    f1.extend(be32(6)); // number_of_classes
    f1.extend(be32(20)); // lookup_table_offset
    f1.extend(be32(36)); // state_array_offset
    f1.extend(be32(60)); // entry_table_offset
    f1.extend(be32(78)); // actions_offset
    f1.extend(be16(0)); // Lookup Format 0: glyph 1='a'->5, glyph 2='b'->4
    for c in [1u16, 5, 4, 1, 1, 1, 1] {
        f1.extend(be16(c));
    }
    for e in [0u16, 0, 0, 0, 1, 0, 0, 0, 0, 0, 1, 2] {
        f1.extend(be16(e));
    }
    for (st, fl, act) in [(0u16, 0u16, 0xFFFFu16), (1, 0x8000, 0xFFFF), (0, 0, 0)] {
        f1.extend(be16(st));
        f1.extend(be16(fl));
        f1.extend(be16(act));
    }
    f1.extend(i16be(-39));

    // Subtable 2: Format 2 matrix ('A'=3, 'V'=4) -> -86.
    let mut f2 = Vec::new();
    f2.extend(be32(4)); // row_width
    f2.extend(be32(12 + 16)); // left_hand_table_offset = 28
    f2.extend(be32(12 + 22)); // right_hand_table_offset = 34
    f2.extend(be32(12 + 28)); // array_offset = 40
    f2.extend(be16(3));
    f2.extend(be16(1));
    f2.extend(be16(44)); // row 1 at subtable byte offset 40 + 4 = 44
    f2.extend(be16(4));
    f2.extend(be16(1));
    f2.extend(be16(2)); // col 1 at +2
    for v in [0i16, 0, 0, -86] {
        f2.extend(i16be(v));
    }

    // Subtable 3: Format 4 `ankr` state machine ('e'=5 + U+0301=6 -> anchor points (0, 0)).
    let mut f4 = Vec::new();
    f4.extend(be32(6));
    f4.extend(be32(20));
    f4.extend(be32(36));
    f4.extend(be32(60));
    f4.extend(be32((1u32 << 30) | 78)); // action_type = 1 (anchor points), points_offset = 78
    f4.extend(be16(0)); // Lookup Format 0: glyph 5='e'->4, glyph 6=mark->5
    for c in [1u16, 1, 1, 1, 1, 4, 5] {
        f4.extend(be16(c));
    }
    for e in [0u16, 0, 0, 0, 1, 0, 0, 0, 0, 0, 1, 2] {
        f4.extend(be16(e));
    }
    for (st, fl, act) in [(0u16, 0u16, 0xFFFFu16), (1, 0x8000, 0xFFFF), (1, 0, 0)] {
        f4.extend(be16(st));
        f4.extend(be16(fl));
        f4.extend(be16(act));
    }
    f4.extend(be16(0));
    f4.extend(be16(0));

    // Subtable 4: Format 6 index array ('V'=4, 'A'=3) -> -74.
    let mut f6 = Vec::new();
    f6.extend(be32(0)); // 16-bit offsets
    f6.extend(be16(2));
    f6.extend(be16(2));
    f6.extend(be32(12 + 24)); // row_index_table_offset = 36
    f6.extend(be32(12 + 40)); // column_index_table_offset = 52
    f6.extend(be32(12 + 56)); // kerning_array_offset = 68
    f6.extend(be32(12 + 64)); // kerning_vector_offset = 76
    f6.extend(be16(0)); // row Lookup Format 0: glyph 4='V' -> pre-multiplied row index 2
    for r in [0u16, 0, 0, 0, 2, 0, 0] {
        f6.extend(be16(r));
    }
    f6.extend(be16(0)); // col Lookup Format 0: glyph 3='A' -> col index 1
    for c in [0u16, 0, 0, 1, 0, 0, 0] {
        f6.extend(be16(c));
    }
    for off in [0u16, 0, 0, 2] {
        f6.extend(be16(off));
    }
    f6.extend(i16be(0));
    f6.extend(i16be(-74));

    let subs = [
        kerx_sub(0, &f0),
        kerx_sub(1, &f1),
        kerx_sub(2, &f2),
        kerx_sub(4, &f4),
        kerx_sub(6, &f6),
    ];
    let mut kerx = Vec::new();
    kerx.extend(be16(2));
    kerx.extend(be16(0));
    kerx.extend(be32(subs.len() as u32));
    for s in &subs {
        kerx.extend(s);
    }

    font_with_tables(
        &[
            (b'A' as u16, 3),
            (b'V' as u16, 4),
            (b'a' as u16, 1),
            (b'b' as u16, 2),
            (b'e' as u16, 5),
            (0x0301, 6),
        ],
        &[500, 500, 500, 600, 600, 500, 180],
        &[(b"GDEF", &gdef), (b"ankr", &ankr), (b"kerx", &kerx)],
    )
}

#[test]
fn aat_kerx_formats_0_1_2_4_6_and_ankr_anchor_attachment() {
    let bytes = kerx_ankr_seed_face();

    // "aba": Format 0 kerns ('a','b') by -50 (-25/-25), Format 1 kerns pushed 'b' before 'a' by -40!
    assert_eq!(
        shape_positions(&bytes, &[], "aba"),
        [
            (
                1,
                GlyphPosition {
                    x_advance: 475,
                    y_advance: 0,
                    x_offset: 0,
                    y_offset: 0
                }
            ),
            (
                2,
                GlyphPosition {
                    x_advance: 435,
                    y_advance: 0,
                    x_offset: -65,
                    y_offset: 0
                }
            ),
            (
                1,
                GlyphPosition {
                    x_advance: 500,
                    y_advance: 0,
                    x_offset: 0,
                    y_offset: 0
                }
            ),
        ]
    );

    // "AVA": Format 2 kerns ('A','V') by -86 (-43/-43), Format 6 kerns ('V','A') by -74 (-37/-37)!
    assert_eq!(
        shape_positions(&bytes, &[], "AVA"),
        [
            (
                3,
                GlyphPosition {
                    x_advance: 557,
                    y_advance: 0,
                    x_offset: 0,
                    y_offset: 0
                }
            ),
            (
                4,
                GlyphPosition {
                    x_advance: 520,
                    y_advance: 0,
                    x_offset: -43,
                    y_offset: 0
                }
            ),
            (
                3,
                GlyphPosition {
                    x_advance: 563,
                    y_advance: 0,
                    x_offset: -37,
                    y_offset: 0
                }
            ),
        ]
    );

    // "e\u{0301}": Format 4 + `ankr` attaches mark anchor (40, 90) to base anchor (250, 480)
    // across base advance 500: x_offset = (250 - 40) - 500 = -290, y_offset = 480 - 90 = 390;
    // AAT `kerx` disables OpenType `GDEF` mark-width zeroing so the mark's hmtx advance (180) stands.
    assert_eq!(
        shape_positions(&bytes, &[], "e\u{0301}"),
        [
            (
                5,
                GlyphPosition {
                    x_advance: 500,
                    y_advance: 0,
                    x_offset: 0,
                    y_offset: 0
                }
            ),
            (
                6,
                GlyphPosition {
                    x_advance: 180,
                    y_advance: 0,
                    x_offset: -290,
                    y_offset: 390
                }
            ),
        ]
    );
}

#[test]
fn fallback_mark_positioning_stacks_marks_and_splits_ligatures() {
    let rect_glyph = |x_min: i16, y_min: i16, x_max: i16, y_max: i16| -> Vec<u8> {
        let mut g = Vec::new();
        g.extend(i16be(1)); // 1 contour
        g.extend(i16be(x_min));
        g.extend(i16be(y_min));
        g.extend(i16be(x_max));
        g.extend(i16be(y_max));
        g.extend(be16(3)); // 4 points (0..=3)
        g.extend(be16(0)); // 0 instructions
        g.extend([0x01u8; 4]); // 4 on-curve points, 2-byte signed deltas
        for dx in [x_min, x_max - x_min, 0, x_min - x_max] {
            g.extend(i16be(dx));
        }
        for dy in [y_min, 0, y_max - y_min, y_min - y_max] {
            g.extend(i16be(dy));
        }
        if g.len() % 2 != 0 {
            g.push(0);
        }
        g
    };

    // Glyphs:
    //   0 = .notdef (empty)
    //   1 = 'a' (adv 600, bbox [50, 0, 550, 500])
    //   2 = U+0301 Above CCC 230 (adv 200, bbox [20, 420, 180, 540])
    //   3 = U+0323 Below CCC 220 (adv 200, bbox [40, -160, 120, -60])
    //   4 = 'f' (adv 300, bbox [20, 0, 280, 700])
    //   5 = 'i' (adv 300, bbox [20, 0, 280, 700])
    //   6 = 'fi' ligature (adv 600, bbox [40, 0, 560, 700])
    //   7 = ' ' (adv 500, empty outline -> fallback centers on [0, 500])
    //   8 = U+05D0 Alef (adv 600, bbox [60, 0, 540, 620])
    //   9 = U+05B4 Hiriq CCC 14 -> recategorized to Below (adv 0, bbox [80, -40, 160, 20])
    let glyphs: [Vec<u8>; 10] = [
        Vec::new(),
        rect_glyph(50, 0, 550, 500),
        rect_glyph(20, 420, 180, 540),
        rect_glyph(40, -160, 120, -60),
        rect_glyph(20, 0, 280, 700),
        rect_glyph(20, 0, 280, 700),
        rect_glyph(40, 0, 560, 700),
        Vec::new(),
        rect_glyph(60, 0, 540, 620),
        rect_glyph(80, -40, 160, 20),
    ];
    let mut glyf = Vec::new();
    let mut loca = Vec::new();
    for g in &glyphs {
        loca.extend(be16((glyf.len() / 2) as u16));
        glyf.extend(g);
    }
    loca.extend(be16((glyf.len() / 2) as u16));

    // GSUB `liga`: 'f'(4) + 'i'(5) -> 'fi'(6), IGNORE_MARKS (0x0008).
    let mut lig_sub = Vec::new();
    lig_sub.extend(be16(1));
    lig_sub.extend(be16(8));
    lig_sub.extend(be16(1));
    lig_sub.extend(be16(14));
    lig_sub.extend(coverage_format1(&[4]));
    lig_sub.extend(be16(1));
    lig_sub.extend(be16(4));
    lig_sub.extend(be16(6));
    lig_sub.extend(be16(2));
    lig_sub.extend(be16(5));
    let gsub = layout_table(b"latn", &[(b"liga", &[0])], &[(4, 0x0008, lig_sub)]);

    let mut bytes = font_with_tables(
        &[
            (b' ' as u16, 7),
            (b'a' as u16, 1),
            (b'f' as u16, 4),
            (b'i' as u16, 5),
            (0x0301, 2),
            (0x0323, 3),
            (0x05B4, 9),
            (0x05D0, 8),
        ],
        &[500, 600, 200, 200, 300, 300, 600, 500, 600, 0],
        &[(b"loca", &loca), (b"glyf", &glyf), (b"GSUB", &gsub)],
    );
    // Patch `hhea` ascender=800, descender=-200 => `y_gap = (800 - (-200)) / 16 = 62`.
    let tables = usize::from(u16::from_be_bytes([bytes[4], bytes[5]]));
    let record = (0..tables)
        .map(|i| 12 + 16 * i)
        .find(|r| &bytes[*r..*r + 4] == b"hhea")
        .expect("hhea record");
    let hhea_off = u32::from_be_bytes(bytes[record + 8..record + 12].try_into().unwrap()) as usize;
    bytes[hhea_off + 4..hhea_off + 6].copy_from_slice(&i16be(800));
    bytes[hhea_off + 6..hhea_off + 8].copy_from_slice(&i16be(-200));

    // Below + Above + stacked Above on 'a' (base width=600 => mid_x=300, y=[0, 500], y_gap = 1000/16 = 62):
    //   U+0323 (Below, mid_x=80, y_max=-60 placed at 0 - 62 = -62 => y_offset = -62 - (-60) = -2):
    //     x_offset = 300 - 80 - 600 = -380.
    //   1st U+0301 (Above, mid_x=100, y=[420, 540]): x_offset = 300 - 100 - 600 = -400, y_offset = (500 + 62) - 420 = 142;
    //     expands base y_bearing to 500 + 62 + 120 = 682.
    //   2nd U+0301 (stacked Above): x_offset = -400, y_offset = (682 + 62) - 420 = 324!
    assert_eq!(
        shape_positions(&bytes, &[], "a\u{0323}\u{0301}\u{0301}"),
        [
            (
                1,
                GlyphPosition {
                    x_advance: 600,
                    y_advance: 0,
                    x_offset: 0,
                    y_offset: 0
                }
            ),
            (
                3,
                GlyphPosition {
                    x_advance: 0,
                    y_advance: 0,
                    x_offset: -380,
                    y_offset: -2
                }
            ),
            (
                2,
                GlyphPosition {
                    x_advance: 0,
                    y_advance: 0,
                    x_offset: -400,
                    y_offset: 142
                }
            ),
            (
                2,
                GlyphPosition {
                    x_advance: 0,
                    y_advance: 0,
                    x_offset: -400,
                    y_offset: 324
                }
            ),
        ]
    );

    // Ligature `fi` (advance 600 split into 2 components => [0, 300] mid 150 and [300, 600] mid 450):
    // each component gets its own centered mark without vertical stacking across components!
    assert_eq!(
        shape_positions(&bytes, &[], "f\u{0301}i\u{0301}"),
        [
            (
                6,
                GlyphPosition {
                    x_advance: 600,
                    y_advance: 0,
                    x_offset: 0,
                    y_offset: 0
                }
            ),
            (
                2,
                GlyphPosition {
                    x_advance: 0,
                    y_advance: 0,
                    x_offset: 150 - 100 - 600,
                    y_offset: (700 - 420) + 62
                }
            ),
            (
                2,
                GlyphPosition {
                    x_advance: 0,
                    y_advance: 0,
                    x_offset: 450 - 100 - 600,
                    y_offset: (700 - 420) + 62
                }
            ),
        ]
    );

    // Empty-contour base (' ', extents 0x0 widened to advance 500 => mid_x = 250) centers horizontally
    // (-350) and halves the negative `Above` overlap ((62 - 420) + 358 / 2 = -179).
    assert_eq!(
        shape_positions(&bytes, &[], " \u{0301}")[1].1,
        GlyphPosition {
            x_advance: 0,
            y_advance: 0,
            x_offset: -350,
            y_offset: -179,
        }
    );

    // Hebrew Hiriq (U+05B4, CCC 14) is recategorized to CCC 220 (Below) when `GPOS` lacks `hebr`:
    // base Alef y_min=0, Hiriq y_max=20 => offset_y = (0 - 20) - 62 = -82 (in visual RTL order, mark is index 0).
    let heb = shape_positions(&bytes, &[], "\u{05D0}\u{05B4}");
    assert_eq!(heb[0].0, 9);
    assert_eq!(heb[0].1.y_offset, -82);
}

#[test]
fn skippy_iter_skips_cgj_and_zwnj_across_gpos_pair_pos() {
    // PairPos Format 1 (`kern`) on ('A'=1, 'V'=2) -> -80, with CGJ (U+034F=3) and ZWNJ (U+200C=4).
    let cov = coverage_format1(&[1]);
    let mut sub = Vec::new();
    sub.extend(be16(1));
    sub.extend(be16(12));
    sub.extend(be16(0x0004));
    sub.extend(be16(0));
    sub.extend(be16(1));
    sub.extend(be16((12 + cov.len()) as u16));
    sub.extend(cov);
    sub.extend(be16(1));
    sub.extend(be16(2));
    sub.extend(i16be(-80));

    let gpos = layout_table(b"latn", &[(b"kern", &[0])], &[(2, 0, sub)]);
    let bytes = font_with_tables(
        &[(b'A' as u16, 1), (b'V' as u16, 2), (0x034F, 3), (0x200C, 4)],
        &[500, 600, 600, 0, 0],
        &[(b"GPOS", &gpos)],
    );

    assert_eq!(
        shape_positions(&bytes, &[], "A\u{034F}V")[0].1.x_advance,
        520
    );
    assert_eq!(
        shape_positions(&bytes, &[], "A\u{200C}V")[0].1.x_advance,
        520
    );
}
