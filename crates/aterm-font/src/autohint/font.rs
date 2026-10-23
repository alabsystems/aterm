// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0 AND (MIT OR Apache-2.0)
//
// The CFF adapters below are derived from read-fonts 0.43.2 src/ps/cs.rs
// (`TransformSink`'s unscaled path, `NopFilterSink`) and skrifa 0.46.2
// src/outline/mod.rs (`OutlineGlyphCollection`, `draw_unscaled`), Copyright
// (c) 2019 Fontations Developers, MIT OR Apache-2.0 (see
// ../../LICENSE-FONTATIONS), modified by the aterm project in 2026. The rest
// of this file is aterm's.

//! The face as the hinter reads it: skrifa's outline collection and font attributes.

use super::charmap::Charmap;
use super::fixed::{F2Dot14, Fixed};
use super::glyf::{self, Outlines};
use super::unscaled::{UnscaledOutlineSink, UnscaledPenAdapter, UnscaledPoint};
use super::{DrawError, GlyphId};
use crate::opentype_layout::LayoutTable;
use crate::{Face, NormalizedCoordinate, OutlineSink, Tag};

fn read_u16(data: &[u8], offset: usize) -> Option<u16> {
    let b = data.get(offset..offset.checked_add(2)?)?;
    Some(u16::from_be_bytes([b[0], b[1]]))
}

/// One face, read for hinting.
pub(crate) struct HintFont<'a> {
    face: Face<'a>,
    glyf: Option<Outlines<'a>>,
    has_cff: bool,
    units_per_em: i32,
    glyph_count: u32,
    is_italic: bool,
    charmap: Charmap<'a>,
    fvar: Option<&'a [u8]>,
    avar: Option<&'a [u8]>,
}

/// A glyph's outline source (skrifa's `OutlineGlyph`).
pub(crate) enum OutlineGlyph<'a> {
    Glyf(glyf::Outline<'a>),
    Cff(GlyphId),
}

impl<'a> HintFont<'a> {
    /// The face, if aterm-font reads it and it has `glyf` or CFF/CFF2 outlines.
    pub(crate) fn new(data: &'a [u8], index: u32) -> Option<Self> {
        let face = Face::parse(data, index).ok()?;
        let raw = face.raw_face();
        let table = |tag: &[u8; 4]| raw.table(Tag::from_bytes(tag));
        let head = table(b"head")?;
        let glyf = Outlines::new(&glyf::Tables {
            head,
            loca: table(b"loca"),
            glyf: table(b"glyf"),
            gvar: table(b"gvar"),
            hhea: table(b"hhea"),
            hmtx: table(b"hmtx"),
            os2: table(b"OS/2"),
        });
        let has_cff = face.tables().cff.is_some() || face.tables().cff2.is_some();
        if glyf.is_none() && !has_cff {
            return None;
        }
        if head.len() < 54 {
            return None;
        }
        let units_per_em = i32::from(read_u16(head, 18)?);
        let maxp_glyphs = table(b"maxp")
            .filter(|maxp| maxp.len() >= 6)
            .and_then(|maxp| read_u16(maxp, 4));
        let glyph_count = u32::from(maxp_glyphs.unwrap_or_default());
        let charmap = Charmap::new(table(b"cmap"), u32::from(maxp_glyphs.unwrap_or(u16::MAX)));
        let is_italic = match table(b"OS/2").filter(|os2| os2.len() >= 78) {
            Some(os2) => read_u16(os2, 62)? & (0x0001 | 0x0200) != 0,
            None => read_u16(head, 44)? & 0x0002 != 0,
        };
        Some(Self {
            glyf,
            has_cff,
            units_per_em,
            glyph_count,
            is_italic,
            charmap,
            fvar: table(b"fvar"),
            avar: table(b"avar"),
            face,
        })
    }

    pub(crate) fn units_per_em(&self) -> i32 {
        self.units_per_em
    }

    /// `maxp`'s glyph count, 0 without one.
    pub(crate) fn glyph_count(&self) -> u32 {
        self.glyph_count
    }

    /// True for an italic or oblique face (no horizontal fitting).
    pub(crate) fn is_italic(&self) -> bool {
        self.is_italic
    }

    pub(crate) fn charmap(&self) -> &Charmap<'a> {
        &self.charmap
    }

    pub(crate) fn gsub(&self) -> Option<LayoutTable<'a>> {
        self.face.tables().gsub
    }

    /// The `GSUB` table's length in bytes (0 without one).
    pub(crate) fn gsub_len(&self) -> usize {
        self.face
            .raw_face()
            .table(Tag::from_bytes(b"GSUB"))
            .map_or(0, <[u8]>::len)
    }

    /// The normalized location of user-space settings (empty: default).
    pub(crate) fn location(&self, settings: &[(u32, f32)]) -> Vec<F2Dot14> {
        super::location::location(self.fvar, self.avar, settings)
    }

    /// The glyph's outline source: `None` for a `glyf` glyph `loca` cannot place.
    pub(crate) fn outline(&self, glyph_id: GlyphId) -> Option<OutlineGlyph<'a>> {
        if let Some(glyf) = &self.glyf {
            let gid = u16::try_from(glyph_id.to_u32()).ok()?;
            return glyf.outline(gid).ok().map(OutlineGlyph::Glyf);
        }
        self.has_cff.then_some(OutlineGlyph::Cff(glyph_id))
    }

    /// Draws the glyph's unscaled outline at `coords` into `sink`.
    pub(crate) fn draw_unscaled(
        &self,
        glyph: &OutlineGlyph,
        coords: &[F2Dot14],
        sink: &mut impl UnscaledOutlineSink,
    ) -> Result<(), DrawError> {
        match glyph {
            OutlineGlyph::Glyf(outline) => {
                let outlines = self.glyf.as_ref().ok_or(DrawError::Read)?;
                let loaded = glyf::UnscaledScaler::new(outlines, outline, coords)?
                    .load_outline(&outline.glyph, outline.glyph_id)?;
                sink.reserve(loaded.points.len());
                let mut contour_start = 0;
                for contour_end in loaded.contours.iter().map(|c| *c as usize) {
                    if contour_end >= contour_start {
                        if let (Some(points), Some(flags)) = (
                            loaded.points.get(contour_start..=contour_end),
                            loaded.flags.get(contour_start..=contour_end),
                        ) {
                            sink.extend(points.iter().zip(flags).enumerate().map(
                                |(ix, (point, flags))| {
                                    UnscaledPoint::from_glyf_point(*point, *flags, ix == 0)
                                },
                            ))?;
                        }
                    }
                    contour_start = contour_end + 1;
                }
                Ok(())
            }
            OutlineGlyph::Cff(glyph_id) => {
                let gid = u16::try_from(glyph_id.to_u32()).map_err(|_| DrawError::Read)?;
                let mut face = self.face.clone();
                face.set_normalized_coordinates(
                    coords
                        .iter()
                        .map(|c| NormalizedCoordinate::from(c.to_bits())),
                );
                let mut adapter = UnscaledPenAdapter::new(sink);
                let mut filter = CffUnscaled::new(&mut adapter);
                let tables = face.tables();
                let drawn = if let Some(cff) = tables.cff.as_ref() {
                    cff.outline(crate::GlyphId(gid), &mut filter)
                } else if let Some(cff2) = tables.cff2.as_ref() {
                    cff2.outline(
                        face.variation_coordinates(),
                        crate::GlyphId(gid),
                        &mut filter,
                    )
                } else {
                    Err(crate::CFFError::NoGlyph)
                };
                match drawn {
                    Ok(_) | Err(crate::CFFError::ZeroBBox) => {}
                    Err(_) => return Err(DrawError::Read),
                }
                filter.finish();
                adapter.finish()
            }
        }
    }
}

const ONE_OVER_64: Fixed = Fixed::from_bits(0x400);

#[derive(Copy, Clone)]
enum PendingElement {
    Move([i32; 2]),
    Line([i32; 2]),
    Curve([i32; 6]),
}

impl PendingElement {
    fn target_point(&self) -> [i32; 2] {
        match self {
            Self::Move(xy) | Self::Line(xy) => *xy,
            Self::Curve([.., x, y]) => [*x, *y],
        }
    }
}

struct CffUnscaled<'a, 'b, T> {
    is_open: bool,
    start: Option<[i32; 2]>,
    pending_element: Option<PendingElement>,
    inner: &'a mut UnscaledPenAdapter<'b, T>,
}

impl<'a, 'b, T: UnscaledOutlineSink> CffUnscaled<'a, 'b, T> {
    fn new(inner: &'a mut UnscaledPenAdapter<'b, T>) -> Self {
        Self {
            is_open: false,
            start: None,
            pending_element: None,
            inner,
        }
    }

    fn unit(v: f32) -> i32 {
        let fixed = Fixed::from_f64(f64::from(v));
        (fixed * ONE_OVER_64).to_bits() >> 10
    }

    fn flush_pending(&mut self, for_close: bool) {
        if let Some(pending) = self.pending_element.take() {
            match pending {
                PendingElement::Move([x, y]) => {
                    if !for_close {
                        self.is_open = true;
                        self.inner.move_to(x as f32, y as f32);
                        self.start = Some([x, y]);
                    }
                }
                PendingElement::Line([x, y]) => {
                    if !for_close || self.start != Some([x, y]) {
                        self.inner.line_to(x as f32, y as f32);
                    }
                }
                PendingElement::Curve([cx0, cy0, cx1, cy1, x, y]) => {
                    self.inner.curve_to(
                        cx0 as f32, cy0 as f32, cx1 as f32, cy1 as f32, x as f32, y as f32,
                    );
                }
            }
        }
    }

    fn finish(&mut self) {
        self.close_impl();
    }

    fn close_impl(&mut self) {
        self.flush_pending(true);
        if self.is_open {
            self.inner.close();
            self.is_open = false;
        }
    }
}

impl<T: UnscaledOutlineSink> OutlineSink for CffUnscaled<'_, '_, T> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.pending_element = Some(PendingElement::Move([Self::unit(x), Self::unit(y)]));
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let (x, y) = (Self::unit(x), Self::unit(y));
        if self
            .pending_element
            .map(|element| element.target_point() == [x, y])
            .unwrap_or_default()
        {
            return;
        }
        self.flush_pending(false);
        self.pending_element = Some(PendingElement::Line([x, y]));
    }

    fn quad_to(&mut self, _: f32, _: f32, x: f32, y: f32) {
        self.line_to(x, y);
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.flush_pending(false);
        self.pending_element = Some(PendingElement::Curve([
            Self::unit(x1),
            Self::unit(y1),
            Self::unit(x2),
            Self::unit(y2),
            Self::unit(x),
            Self::unit(y),
        ]));
    }

    fn close(&mut self) {
        self.close_impl();
    }
}
