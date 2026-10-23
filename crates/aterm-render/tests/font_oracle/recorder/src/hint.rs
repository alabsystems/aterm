// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Skrifa implementation of the hinting oracle's [`Hinter`].

use skrifa::{
    FontRef, MetadataProvider, Tag,
    instance::{Location, Size},
    outline::{
        DrawSettings, Engine, HintingInstance, HintingOptions, OutlinePen, SmoothMode, Target,
    },
};

use crate::hint_raster::Hinter;
pub(crate) use crate::hint_raster::{Mode, PathPen, answer};

pub fn location_of(font: &FontRef, coords: &[(u32, f32)]) -> Location {
    font.axes()
        .location(coords.iter().map(|&(tag, v)| (Tag::from_u32(tag), v)))
}

pub fn options(mode: Mode) -> HintingOptions {
    let target = |mode| Target::Smooth {
        mode,
        symmetric_rendering: false,
        preserve_linear_metrics: false,
    };
    match mode {
        Mode::Full => HintingOptions {
            engine: Engine::Auto(None),
            target: target(SmoothMode::Normal),
        },
        Mode::Light => HintingOptions {
            engine: Engine::Auto(None),
            target: target(SmoothMode::Light),
        },
        Mode::Lcd => HintingOptions {
            engine: Engine::Auto(None),
            target: Target::Smooth {
                mode: SmoothMode::Lcd,
                symmetric_rendering: false,
                preserve_linear_metrics: false,
            },
        },
    }
}

/// skrifa's autohinter, ttf-parser's reader.
pub(crate) struct Skrifa;

struct Pen<'a>(&'a mut PathPen);

impl OutlinePen for Pen<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.move_to(x, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.0.line_to(x, y);
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        self.0.quad_to(cx, cy, x, y);
    }
    fn curve_to(&mut self, c0x: f32, c0y: f32, c1x: f32, c1y: f32, x: f32, y: f32) {
        self.0.curve_to(c0x, c0y, c1x, c1y, x, y);
    }
    fn close(&mut self) {
        self.0.close();
    }
}

struct Scaled<'a> {
    pen: &'a mut PathPen,
    scale: f32,
}

impl ttf_parser::OutlineBuilder for Scaled<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        let s = self.scale;
        self.pen.move_to(x * s, y * s);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let s = self.scale;
        self.pen.line_to(x * s, y * s);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let s = self.scale;
        self.pen.quad_to(x1 * s, y1 * s, x * s, y * s);
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let s = self.scale;
        self.pen
            .curve_to(x1 * s, y1 * s, x2 * s, y2 * s, x * s, y * s);
    }
    fn close(&mut self) {
        self.pen.close();
    }
}

impl Hinter for Skrifa {
    type Instance = HintingInstance;

    fn instance(
        &self,
        bytes: &[u8],
        index: u32,
        px: f32,
        mode: Mode,
        coords: &[(u32, f32)],
    ) -> Option<HintingInstance> {
        if !px.is_finite() || px <= 0.0 {
            return None;
        }
        let font = FontRef::from_index(bytes, index).ok()?;
        let location = location_of(&font, coords);
        HintingInstance::new(
            &font.outline_glyphs(),
            Size::new(px),
            &location,
            options(mode),
        )
        .ok()
    }

    fn draw(
        &self,
        hint: &HintingInstance,
        bytes: &[u8],
        index: u32,
        gid: u16,
        pen: &mut PathPen,
    ) -> Option<()> {
        let font = FontRef::from_index(bytes, index).ok()?;
        let outline = font.outline_glyphs().get(skrifa::GlyphId::from(gid))?;
        outline
            .draw(DrawSettings::hinted(hint, false), &mut Pen(pen))
            .ok()?;
        Some(())
    }

    fn unhinted(
        &self,
        bytes: &[u8],
        index: u32,
        gid: u16,
        px: f32,
        pen: &mut PathPen,
    ) -> Option<()> {
        let face = ttf_parser::Face::parse(bytes, index).ok()?;
        let scale = px / f32::from(face.units_per_em());
        face.outline_glyph(ttf_parser::GlyphId(gid), &mut Scaled { pen, scale })?;
        Some(())
    }

    fn linear_advance(
        &self,
        bytes: &[u8],
        index: u32,
        gid: u16,
        px: f32,
        coords: &[(u32, f32)],
    ) -> Option<f32> {
        let mut face = ttf_parser::Face::parse(bytes, index).ok()?;
        for &(tag, value) in coords {
            let _ = face.set_variation(ttf_parser::Tag(tag), value);
        }
        let upem = f32::from(face.units_per_em());
        if upem <= 0.0 || !px.is_finite() || px <= 0.0 {
            return None;
        }
        let scale = px / upem;
        Some(
            f32::from(
                face.glyph_hor_advance(ttf_parser::GlyphId(gid))
                    .unwrap_or(0),
            ) * scale,
        )
    }
}
