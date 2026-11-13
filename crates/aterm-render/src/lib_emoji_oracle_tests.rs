// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::oracle_corpus as corpus;
use super::*;

fn recorded_glyphs(answer: &str) -> Vec<(u32, i32, i32, i32)> {
    let (_, rest) = answer.split_once('|').expect("an answer header");
    rest.split_whitespace()
        .map(|t| {
            let (glyph, offsets) = t.split_once('@').unwrap_or((t, "0,0"));
            let mut g = glyph.split(':');
            let id = g.next().unwrap().parse().unwrap();
            let _cluster = g.next();
            let advance = g
                .next()
                .unwrap()
                .split(',')
                .next()
                .unwrap()
                .parse()
                .unwrap();
            let (x, y) = offsets.split_once(',').unwrap();
            (id, advance, x.parse().unwrap(), y.parse().unwrap())
        })
        .collect()
}

fn each_emoji_line(
    mut check: impl FnMut(&crate::font::FaceBytes, &aterm_font::Face<'_>, &corpus::Line),
) {
    let text = std::fs::read_to_string(
        aterm_tempfile::manifest_dir().join("tests/fixtures/font-oracle/shaping.txt"),
    )
    .expect("the shaping corpus");
    let lines: Vec<corpus::Line> = text
        .lines()
        .filter(|t| !t.is_empty() && !t.starts_with('#'))
        .filter_map(corpus::Line::parse)
        .filter(|l| l.case.starts_with("emoji."))
        .collect();
    let mut checked = 0usize;
    for face in corpus::FACES
        .iter()
        .filter(|f| f.roles & corpus::EMOJI != 0)
    {
        let corpus::Origin::System(path) = face.origin else {
            continue;
        };
        let Ok(file) = font_file::admit_font_file(std::path::Path::new(path)) else {
            continue;
        };
        let key = corpus::file_key(&file);
        let tt = aterm_font::Face::parse(&file, 0).expect("the colour face parses");
        for l in lines.iter().filter(|l| l.key == key) {
            checked += 1;
            check(&file, &tt, l);
        }
    }
    if checked == 0 {
        aterm_spec::host::could_not_run(
            "no colour face the shaping corpus recorded is installed on this host",
        );
    }
}

#[cfg(target_os = "macos")]
fn ids(glyphs: &[(u32, i32, i32, i32)]) -> Vec<u32> {
    glyphs.iter().map(|g| g.0).collect()
}

#[cfg(target_os = "macos")]
#[test]
fn coretext_answers_the_emoji_lines() {
    let mut same_glyphs = 0usize;
    each_emoji_line(|file, tt, l| {
        let cluster = corpus::unescape(&l.input).unwrap();
        let recorded = recorded_glyphs(&l.answer);
        let now = shape_colour_cluster(file, &cluster);
        match l.case.as_str() {
            "emoji.ascii" => {
                assert_eq!(recorded, [(0, 800, 0, 0)]);
                assert_eq!(now, None);
            }
            "emoji.smile-vs15" => {
                assert_eq!(recorded, [(214, 800, 0, 0), (3, 0, 0, 0)]);
                assert_eq!(now.as_deref(), Some(&[(214, 800, 0, 0)][..]));
            }
            _ => {
                let now = now.unwrap_or_else(|| panic!("{}: CoreText declined", l.case));
                assert_eq!(ids(&now), ids(&recorded), "{}: glyph ids", l.case);
                same_glyphs += 1;
                assert_eq!(
                    colour_cluster_decision(tt, &cluster, &now),
                    colour_cluster_decision(tt, &cluster, &recorded),
                    "{}: what the renderer draws",
                    l.case
                );
            }
        }
    });
    assert!(same_glyphs >= 15, "only {same_glyphs} emoji lines compared");
}

#[cfg(not(target_os = "macos"))]
#[test]
fn the_gsub_shaper_answers_the_emoji_lines() {
    let mut same_glyphs = 0usize;
    each_emoji_line(|file, tt, l| {
        let cluster = corpus::unescape(&l.input).unwrap();
        let recorded = recorded_glyphs(&l.answer);
        let now = shape_colour_cluster(file, &cluster)
            .unwrap_or_else(|| panic!("{}: the shaper declined", l.case));
        assert_eq!(now, recorded, "{}: shaped positions", l.case);
        same_glyphs += 1;
        assert_eq!(
            colour_cluster_decision(tt, &cluster, &now),
            colour_cluster_decision(tt, &cluster, &recorded),
            "{}: what the renderer draws",
            l.case
        );
    });
    assert!(same_glyphs >= 15, "only {same_glyphs} emoji lines compared");
}

#[test]
fn opentype_gpos_stacks_layered_colr_emoji_clusters() {
    let be16 = |v: u16| v.to_be_bytes();
    let i16be = |v: i16| v.to_be_bytes();
    let be32 = |v: u32| v.to_be_bytes();
    let cov1 = |glyphs: &[u16]| {
        let mut v = Vec::with_capacity(4 + 2 * glyphs.len());
        v.extend(be16(1));
        v.extend(be16(glyphs.len() as u16));
        for &g in glyphs {
            v.extend(be16(g));
        }
        v
    };
    let rect_glyf = |x0: i16, y0: i16, x1: i16, y1: i16| {
        let mut g = Vec::with_capacity(36);
        for v in [1i16, x0, y0, x1, y1] {
            g.extend(i16be(v));
        }
        g.extend(be16(3)); // endPtsOfContours = [3]
        g.extend(be16(0)); // instructionLength = 0
        g.extend([0x01u8; 4]); // 4 on-curve 16-bit deltas
        for dx in [x0, x1 - x0, 0, x0 - x1] {
            g.extend(i16be(dx));
        }
        for dy in [y0, 0, y1 - y0, y0 - y1] {
            g.extend(i16be(dy));
        }
        g.resize(36, 0);
        g
    };
    let layout_table = |features: &[(&[u8; 4], &[u16])], lookups: &[(u16, Vec<u8>)]| {
        let mut out = vec![0, 1, 0, 0, 0, 0, 0, 0, 0, 0];
        let script_list = out.len();
        out.extend(be16(1));
        out.extend(b"DFLT");
        out.extend(be16(8));
        out.extend(be16(4));
        out.extend(be16(0));
        out.extend(be16(0));
        out.extend(be16(0xFFFF));
        out.extend(be16(features.len() as u16));
        for i in 0..features.len() {
            out.extend(be16(i as u16));
        }
        let feature_list = out.len();
        out.extend(be16(features.len() as u16));
        let feat_end = 2 + 6 * features.len();
        let mut feat_bodies = Vec::new();
        for (tag, idxs) in features {
            out.extend(*tag);
            out.extend(be16((feat_end + feat_bodies.len()) as u16));
            feat_bodies.extend(be16(0));
            feat_bodies.extend(be16(idxs.len() as u16));
            for &idx in *idxs {
                feat_bodies.extend(be16(idx));
            }
        }
        out.extend(feat_bodies);
        let lookup_list = out.len();
        out.extend(be16(lookups.len() as u16));
        let lkp_end = 2 + 2 * lookups.len();
        let mut lkp_bodies = Vec::new();
        for (kind, sub) in lookups {
            out.extend(be16((lkp_end + lkp_bodies.len()) as u16));
            lkp_bodies.extend(be16(*kind));
            lkp_bodies.extend(be16(0));
            lkp_bodies.extend(be16(1));
            lkp_bodies.extend(be16(8));
            lkp_bodies.extend(sub);
        }
        out.extend(lkp_bodies);
        out[4..6].copy_from_slice(&be16(script_list as u16));
        out[6..8].copy_from_slice(&be16(feature_list as u16));
        out[8..10].copy_from_slice(&be16(lookup_list as u16));
        out
    };

    // 9 glyphs modelling Segoe UI Emoji's `holding-hands` (2 stacked layers) and `family`
    // (4 positioned members: 2 bases + 2 marks).
    let boxes: [Option<(i16, i16, i16, i16)>; 9] = [
        None,                          // 0 .notdef
        None,                          // 1 U+200D ZWJ
        Some((250, 200, 1250, 1800)),  // 2 U+1F9D1 left person (Red)
        None,                          // 3 U+1F91D handshake
        Some((1550, 200, 2550, 1800)), // 4 holding-hands right layer (Blue)
        Some((50, 1050, 650, 1800)),   // 5 U+1F468 man (Red)
        Some((50, 1050, 650, 1800)),   // 6 U+1F469 woman (Blue)
        Some((50, 200, 650, 950)),     // 7 U+1F467 girl mark (Green)
        Some((50, 200, 650, 950)),     // 8 U+1F466 boy mark (Gold)
    ];
    let advances: [u16; 9] = [2048, 0, 2812, 2812, 2812, 841, 1081, 0, 0];

    let mut glyf = Vec::new();
    let mut loca = Vec::new();
    for b in boxes {
        loca.extend(be16((glyf.len() / 2) as u16));
        if let Some((x0, y0, x1, y1)) = b {
            glyf.extend(rect_glyf(x0, y0, x1, y1));
        }
    }
    loca.extend(be16((glyf.len() / 2) as u16));

    let mut hmtx = Vec::new();
    for adv in advances {
        hmtx.extend(be16(adv));
        hmtx.extend(be16(0));
    }

    // cmap format 12 (sorted code points).
    let mappings: [(u32, u32); 7] = [
        (0x200D, 1),
        (0x1F466, 8),
        (0x1F467, 7),
        (0x1F468, 5),
        (0x1F469, 6),
        (0x1F91D, 3),
        (0x1F9D1, 2),
    ];
    let mut cmap = Vec::new();
    cmap.extend(be16(0));
    cmap.extend(be16(1));
    cmap.extend(be16(3));
    cmap.extend(be16(10));
    cmap.extend(be32(12));
    cmap.extend(be16(12));
    cmap.extend(be16(0));
    cmap.extend(be32((16 + 12 * mappings.len()) as u32));
    cmap.extend(be32(0));
    cmap.extend(be32(mappings.len() as u32));
    for (cp, gid) in mappings {
        cmap.extend(be32(cp));
        cmap.extend(be32(cp));
        cmap.extend(be32(gid));
    }

    // CPAL v0 (4 BGRA colours) + COLR v0 (6 base glyphs -> 1 solid layer each).
    let mut cpal = Vec::new();
    for v in [0u16, 4, 1, 4] {
        cpal.extend(be16(v));
    }
    cpal.extend(be32(14));
    cpal.extend(be16(0));
    cpal.extend([40, 40, 220, 255]); // 0 Red
    cpal.extend([230, 90, 40, 255]); // 1 Blue
    cpal.extend([80, 190, 40, 255]); // 2 Green
    cpal.extend([30, 180, 230, 255]); // 3 Gold

    let colr_layers: [(u16, u16); 6] = [(2, 0), (4, 1), (5, 0), (6, 1), (7, 2), (8, 3)];
    let mut colr = Vec::new();
    colr.extend(be16(0));
    colr.extend(be16(colr_layers.len() as u16));
    colr.extend(be32(14));
    colr.extend(be32((14 + 6 * colr_layers.len()) as u32));
    colr.extend(be16(colr_layers.len() as u16));
    for (idx, &(gid, _)) in colr_layers.iter().enumerate() {
        colr.extend(be16(gid));
        colr.extend(be16(idx as u16));
        colr.extend(be16(1));
    }
    for &(gid, pal) in &colr_layers {
        colr.extend(be16(gid));
        colr.extend(be16(pal));
    }

    // GDEF v1.0: glyphs 2..=6 Base (1), glyphs 7..=8 Mark (3).
    let mut gdef = Vec::new();
    gdef.extend(be32(0x0001_0000));
    gdef.extend(be16(12));
    for _ in 0..3 {
        gdef.extend(be16(0));
    }
    gdef.extend(be16(1));
    gdef.extend(be16(2));
    gdef.extend(be16(7));
    for c in [1u16, 1, 1, 1, 1, 3, 3] {
        gdef.extend(be16(c));
    }

    // GSUB LookupType 4 (LigatureSubst on first glyph 1 = ZWJ):
    //   [1, 3, 1, 2] -> 4 (`holding-hands` right layer),
    //   [1, 6] -> 6, [1, 7] -> 7, [1, 8] -> 8 (`family` ZWJ absorption).
    let lig_rules: [(&[u16], u16); 4] = [(&[3, 1, 2], 4), (&[6], 6), (&[7], 7), (&[8], 8)];
    let cov_zwj = cov1(&[1]);
    let mut lig_sub = Vec::new();
    lig_sub.extend(be16(1));
    lig_sub.extend(be16(8));
    lig_sub.extend(be16(1));
    let set_off = 8 + cov_zwj.len();
    lig_sub.extend(be16(set_off as u16));
    lig_sub.extend(cov_zwj);
    lig_sub.extend(be16(lig_rules.len() as u16));
    let lig_offs_end = 2 + 2 * lig_rules.len();
    let mut lig_bodies = Vec::new();
    for (comps, target) in lig_rules {
        lig_sub.extend(be16((lig_offs_end + lig_bodies.len()) as u16));
        lig_bodies.extend(be16(target));
        lig_bodies.extend(be16((comps.len() + 1) as u16));
        for &c in comps {
            lig_bodies.extend(be16(c));
        }
    }
    lig_sub.extend(lig_bodies);
    let gsub = layout_table(&[(b"ccmp", &[0])], &[(4, lig_sub)]);

    // GPOS:
    //   Lookup 0 (`PairAdjustment` on (2, 4)): value1.x_advance = -2812 -> 0!
    //   Lookup 1 (`SingleAdjustment` Format 2 on [5, 6]):
    //     5: x_off +425, x_adv +564 (841 -> 1405); 6: x_off 0, x_adv +80 (1081 -> 1161).
    //   Lookup 2 (`MarkToBaseAdjustment` on marks [7, 8] -> base [6]):
    //     7: anchor (1009, 0) -> -1009 - 1161 = -2170; 8: anchor (0, 0) -> -1161.
    let cov_p = cov1(&[2]);
    let mut pair_sub = Vec::new();
    pair_sub.extend(be16(1));
    pair_sub.extend(be16(12));
    pair_sub.extend(be16(0x0004));
    pair_sub.extend(be16(0));
    pair_sub.extend(be16(1));
    pair_sub.extend(be16((12 + cov_p.len()) as u16));
    pair_sub.extend(cov_p);
    pair_sub.extend(be16(1));
    pair_sub.extend(be16(4));
    pair_sub.extend(i16be(-2812));

    let mut single_sub = Vec::new();
    single_sub.extend(be16(2));
    single_sub.extend(be16(16));
    single_sub.extend(be16(0x0005));
    single_sub.extend(be16(2));
    single_sub.extend(i16be(425));
    single_sub.extend(i16be(564));
    single_sub.extend(i16be(0));
    single_sub.extend(i16be(80));
    single_sub.extend(cov1(&[5, 6]));

    let m_cov = cov1(&[7, 8]);
    let b_cov = cov1(&[6]);
    let mut m2b = Vec::new();
    let m_cov_off = 12usize;
    let b_cov_off = m_cov_off + m_cov.len();
    let marks_off = b_cov_off + b_cov.len();
    let base_off = marks_off + (2 + 2 * 4 + 2 * 6);
    m2b.extend(be16(1));
    m2b.extend(be16(m_cov_off as u16));
    m2b.extend(be16(b_cov_off as u16));
    m2b.extend(be16(1));
    m2b.extend(be16(marks_off as u16));
    m2b.extend(be16(base_off as u16));
    m2b.extend(m_cov);
    m2b.extend(b_cov);
    m2b.extend(be16(2));
    m2b.extend(be16(0));
    m2b.extend(be16(10));
    m2b.extend(be16(0));
    m2b.extend(be16(16));
    m2b.extend([0, 1]);
    m2b.extend(i16be(1009));
    m2b.extend(i16be(0));
    m2b.extend([0, 1, 0, 0, 0, 0]);
    m2b.extend(be16(1));
    m2b.extend(be16(4));
    m2b.extend([0, 1, 0, 0, 0, 0]);

    let gpos = layout_table(
        &[(b"dist", &[0, 1]), (b"mark", &[2])],
        &[(2, pair_sub), (1, single_sub), (4, m2b)],
    );

    let mut head = vec![0u8; 54];
    head[0..4].copy_from_slice(&be32(0x0001_0000));
    head[12..16].copy_from_slice(&be32(0x5F0F_3CF5));
    head[18..20].copy_from_slice(&be16(2048));
    head[36..38].copy_from_slice(&i16be(0));
    head[38..40].copy_from_slice(&i16be(-512));
    head[40..42].copy_from_slice(&i16be(2812));
    head[42..44].copy_from_slice(&i16be(2048));

    let mut hhea = vec![0u8; 36];
    hhea[0..4].copy_from_slice(&be32(0x0001_0000));
    hhea[4..6].copy_from_slice(&i16be(2048));
    hhea[6..8].copy_from_slice(&i16be(-512));
    hhea[34..36].copy_from_slice(&be16(advances.len() as u16));

    let mut maxp = vec![0u8; 6];
    maxp[0..4].copy_from_slice(&be32(0x0001_0000));
    maxp[4..6].copy_from_slice(&be16(advances.len() as u16));

    let mut tables: Vec<([u8; 4], &[u8])> = vec![
        (*b"COLR", &colr),
        (*b"CPAL", &cpal),
        (*b"GDEF", &gdef),
        (*b"GPOS", &gpos),
        (*b"GSUB", &gsub),
        (*b"cmap", &cmap),
        (*b"glyf", &glyf),
        (*b"head", &head),
        (*b"hhea", &hhea),
        (*b"hmtx", &hmtx),
        (*b"loca", &loca),
        (*b"maxp", &maxp),
    ];
    tables.sort_by_key(|(t, _)| *t);
    let n = tables.len() as u16;
    let p2 = 1u16 << n.ilog2();
    let mut sfnt = Vec::new();
    sfnt.extend(be32(0x0001_0000));
    sfnt.extend(be16(n));
    sfnt.extend(be16(p2 * 16));
    sfnt.extend(be16(n.ilog2() as u16));
    sfnt.extend(be16((n - p2) * 16));
    let mut cursor = 12 + 16 * tables.len();
    for (tag, body) in &tables {
        sfnt.extend(tag);
        sfnt.extend(be32(0));
        sfnt.extend(be32(cursor as u32));
        sfnt.extend(be32(body.len() as u32));
        cursor += (body.len() + 3) & !3;
    }
    for (_, body) in &tables {
        sfnt.extend_from_slice(body);
        sfnt.resize((sfnt.len() + 3) & !3, 0);
    }

    let face = aterm_font::Face::parse(&sfnt, 0).expect("synthetic COLR+GPOS face parses");

    // 1. Two-layer stacked cluster (`emoji.holding-hands` pattern): `hmtx` 2812, 2812 -> `GPOS` 0, 2812.
    let hands = "\u{1F9D1}\u{200D}\u{1F91D}\u{200D}\u{1F9D1}";
    let shaped_hands = shape_colour_cluster_opentype(&sfnt, hands).expect("hands shape");
    assert_eq!(shaped_hands, [(2, 0, 0, 0), (4, 2812, 0, 0)]);
    let Some(ColourCluster::Run(hands_run)) = colour_cluster_decision(&face, hands, &shaped_hands)
    else {
        panic!("expected layered Run for holding-hands");
    };
    assert_eq!((hands_run.width, hands_run.em), (2812, 2812));
    assert_eq!(
        hands_run
            .glyphs
            .iter()
            .map(|g| (g.gid, g.x))
            .collect::<Vec<_>>(),
        [(2, 0), (4, 0)],
        "both layers stack at x = 0 instead of 2 em side by side"
    );
    let hands_colr: Vec<_> = hands_run
        .glyphs
        .iter()
        .map(|g| (aterm_font::GlyphId(g.gid), g.x as f32, g.y as f32))
        .collect();
    let hands_rgba =
        crate::colr::rasterize_colr_run(&face, &hands_colr, 24, 24).expect("COLR run paints");
    let has_red = hands_rgba
        .as_chunks::<4>()
        .0
        .iter()
        .any(|p| p[3] >= 128 && p[0] > 180 && p[2] < 80);
    let has_blue = hands_rgba
        .as_chunks::<4>()
        .0
        .iter()
        .any(|p| p[3] >= 128 && p[2] > 180 && p[0] < 80);
    assert!(
        has_red && has_blue,
        "both stacked layers ink inside the cell box"
    );

    // 2. Four-layer family cluster (`emoji.family` pattern): `SinglePos` + `MarkToBase` + propagation.
    let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}";
    let shaped_family = shape_colour_cluster_opentype(&sfnt, family).expect("family shapes");
    assert_eq!(
        shaped_family,
        [
            (5, 1405, 425, 0),
            (6, 1161, 0, 0),
            (7, 0, -2170, 0),
            (8, 0, -1161, 0),
        ]
    );
    let Some(ColourCluster::Run(family_run)) =
        colour_cluster_decision(&face, family, &shaped_family)
    else {
        panic!("expected layered Run for family");
    };
    assert_eq!((family_run.width, family_run.em), (2566, 1405));
    assert_eq!(
        family_run
            .glyphs
            .iter()
            .map(|g| (g.gid, g.x))
            .collect::<Vec<_>>(),
        [(5, 425), (6, 1405), (7, 396), (8, 1405)],
    );
    let family_colr: Vec<_> = family_run
        .glyphs
        .iter()
        .map(|g| (aterm_font::GlyphId(g.gid), g.x as f32, g.y as f32))
        .collect();
    let family_rgba =
        crate::colr::rasterize_colr_run(&face, &family_colr, 24, 24).expect("family paints");
    let px = family_rgba.as_chunks::<4>().0;
    let has_green = px.iter().any(|p| p[3] >= 128 && p[1] > 150 && p[0] < 80);
    let has_gold = px
        .iter()
        .any(|p| p[3] >= 128 && p[0] > 180 && p[1] > 140 && p[2] < 80);
    assert!(
        px.iter().any(|p| p[3] >= 128 && p[0] > 180 && p[2] < 80)
            && px.iter().any(|p| p[3] >= 128 && p[2] > 180 && p[0] < 80)
            && has_green
            && has_gold,
        "all four family members ink inside the shared cell box"
    );
}
