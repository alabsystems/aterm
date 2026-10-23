// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

struct FontTables {
    head: [u8; 54],
    hhea: [u8; 36],
    hmtx: [u8; 8],
    loca: [u8; 12],
    glyf: Vec<u8>,
}

impl FontTables {
    fn new(glyf: Vec<u8>) -> Self {
        let mut head = [0; 54];
        head[18..20].copy_from_slice(&1000u16.to_be_bytes());
        head[50..52].copy_from_slice(&1u16.to_be_bytes());
        let mut hhea = [0; 36];
        hhea[34..36].copy_from_slice(&2u16.to_be_bytes());
        let mut hmtx = [0; 8];
        hmtx[0..2].copy_from_slice(&500u16.to_be_bytes());
        hmtx[4..6].copy_from_slice(&500u16.to_be_bytes());
        let mut loca = [0; 12];
        loca[8..12].copy_from_slice(&u32::try_from(glyf.len()).unwrap().to_be_bytes());
        Self {
            head,
            hhea,
            hmtx,
            loca,
            glyf,
        }
    }

    fn outlines(&self) -> Outlines<'_> {
        Outlines::new(&Tables {
            head: &self.head,
            loca: Some(&self.loca),
            glyf: Some(&self.glyf),
            gvar: None,
            hhea: Some(&self.hhea),
            hmtx: Some(&self.hmtx),
            os2: None,
        })
        .unwrap()
    }
}

fn simple(endpoints: &[u16]) -> FontTables {
    let mut data = Vec::new();
    data.extend_from_slice(&i16::try_from(endpoints.len()).unwrap().to_be_bytes());
    data.extend_from_slice(&[0; 8]);
    for endpoint in endpoints {
        data.extend_from_slice(&endpoint.to_be_bytes());
    }
    data.extend_from_slice(&0u16.to_be_bytes()); // no instructions
    let points = endpoints.last().map_or(0, |end| usize::from(*end) + 1);
    data.extend(std::iter::repeat_n(ON_CURVE_POINT | 0x10 | 0x20, points));
    FontTables::new(data)
}

fn empty_components(count: usize) -> FontTables {
    let mut data = Vec::new();
    data.extend_from_slice(&(-1i16).to_be_bytes());
    data.extend_from_slice(&[0; 8]);
    for i in 0..count {
        let flags = ARGS_ARE_XY_VALUES | if i + 1 < count { MORE_COMPONENTS } else { 0 };
        data.extend_from_slice(&flags.to_be_bytes());
        data.extend_from_slice(&0u16.to_be_bytes()); // empty glyph zero
        data.extend_from_slice(&[0, 0]); // byte-sized x/y offset
    }
    FontTables::new(data)
}

#[test]
fn valid_contours_pass_preflight_and_load() {
    let font = simple(&[0, 1]);
    let outlines = font.outlines();
    let outline = outlines.outline(1).unwrap();
    assert_eq!((outline.points, outline.contours), (6, 2));
    let loaded = UnscaledScaler::new(&outlines, &outline, &[])
        .unwrap()
        .load_outline(&outline.glyph, outline.glyph_id)
        .unwrap();
    assert_eq!(loaded.points.len(), 2);
    assert_eq!(loaded.contours, [0, 1]);
}

#[test]
fn invalid_contour_endpoints_are_rejected_during_preflight() {
    for endpoints in [[0, 0], [1, 0]] {
        let font = simple(&endpoints);
        assert_eq!(
            font.outlines()
                .outline(1)
                .map(|outline| (outline.points, outline.contours)),
            Err(DrawError::Read),
            "invalid endpoints {endpoints:?} must be rejected before loader allocation"
        );
    }
}

#[test]
fn empty_component_edges_obey_the_same_preflight_limit() {
    for count in [1, MAX_GRAPH_EDGES] {
        let font = empty_components(count);
        let outlines = font.outlines();
        let outline = outlines.outline(1).unwrap();
        let loaded = UnscaledScaler::new(&outlines, &outline, &[])
            .unwrap()
            .load_outline(&outline.glyph, outline.glyph_id)
            .unwrap();
        assert!(loaded.points.is_empty() && loaded.contours.is_empty());
    }
    let font = empty_components(MAX_GRAPH_EDGES + 1);
    assert_eq!(
        font.outlines()
            .outline(1)
            .map(|outline| (outline.points, outline.contours)),
        Err(DrawError::RecursionLimitExceeded),
        "an empty component is still an edge in the traversed graph"
    );
}
