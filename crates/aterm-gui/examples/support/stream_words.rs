// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The words of each `RenderInput` stream element `trail_dump`'s `streams` line hashes
//! (`docs/design/PRISM-GLASS.md` §15.0.2(b)): every field named, a colour as its `u32`, a signed
//! field by its two's-complement bits and a fieldless enum by its discriminant. Each struct is
//! taken apart by an exhaustive pattern, so a field a product type gains fails the dump's build
//! until a harness commit names it here; `FxContent` and `FxArt` keep their fields private and
//! are read through every accessor they have. Nothing goes through `Debug`.

use aterm_core::render::{
    CharFg, FireHaloCell, FirePatch, FxArt, FxContent, FxInk, FxItem, FxLayer, FxRamps, FxRect,
    FxSprite, GlowQuad, GlowRamp, GlowRamps, InkCell, PxRect, RainHalo, TrailCell, WordDecoration,
};
use aterm_effects::state_words::Words;

/// An `i32` by its bits.
fn signed(v: i32) -> u64 {
    u64::from(v as u32)
}

/// A `u32` a stream carries whole: a colour, an override.
pub(crate) fn whole(out: &mut impl Words, v: u32) {
    out.put("v", u64::from(v));
}

/// A glow quad: `glow_under`, `cursor_glow_add` and `nova_add`.
pub(crate) fn glow_quad(out: &mut impl Words, q: &GlowQuad) {
    let &GlowQuad {
        row,
        x,
        y,
        w,
        h,
        color,
        alpha,
        color2,
        alpha2,
    } = q;
    out.put("row", u64::from(row));
    out.put("x", u64::from(x));
    out.put("y", u64::from(y));
    out.put("w", u64::from(w));
    out.put("h", u64::from(h));
    out.put("color", u64::from(color));
    out.put("alpha", u64::from(alpha));
    out.put("color2", u64::from(color2));
    out.put("alpha2", u64::from(alpha2));
}

/// A ramp table: `glow_under_ramps` and `cursor_glow_add_ramps`, every item, every end and
/// how many are lit.
pub(crate) fn glow_ramps(out: &mut impl Words, r: &GlowRamps) {
    let GlowRamps { items, ends, lit } = r;
    out.list("items", items, glow_ramp);
    out.list("ends", ends, |f, &end| f.put("end", u64::from(end)));
    out.put("lit", *lit as u64);
}

fn glow_ramp(out: &mut impl Words, r: &GlowRamp) {
    let &GlowRamp {
        at,
        row,
        x,
        y,
        w,
        h,
        join,
        joined,
        ink,
    } = r;
    out.put("at", u64::from(at));
    out.put("row", u64::from(row));
    out.put("x", u64::from(x));
    out.put("y", u64::from(y));
    out.put("w", u64::from(w));
    out.put("h", u64::from(h));
    out.put("join", u64::from(join));
    out.put("joined", u64::from(joined));
    out.put("ink", u64::from(ink));
}

/// A halo: `glow_halo` and `rain_add`.
pub(crate) fn rain_halo(out: &mut impl Words, halo: &RainHalo) {
    let &RainHalo {
        row,
        x,
        y,
        w,
        h,
        color,
        cx,
        cy,
        rx,
        ry,
        mode,
    } = halo;
    out.put("row", u64::from(row));
    out.put("x", u64::from(x));
    out.put("y", u64::from(y));
    out.put("w", u64::from(w));
    out.put("h", u64::from(h));
    out.put("color", u64::from(color));
    out.put("cx", u64::from(cx));
    out.put("cy", u64::from(cy));
    out.put("rx", u64::from(rx));
    out.put("ry", u64::from(ry));
    out.put("mode", mode as u64);
}

/// A fire patch: `fire_patch`.
pub(crate) fn fire_patch(out: &mut impl Words, p: &FirePatch) {
    let &FirePatch {
        row,
        x,
        y,
        w,
        h,
        base_y,
        peak_h,
        phase,
        temp,
        strength,
        lean,
        cov_cap,
        cell_h,
        mode,
    } = p;
    out.put("row", u64::from(row));
    out.put("x", u64::from(x));
    out.put("y", u64::from(y));
    out.put("w", u64::from(w));
    out.put("h", u64::from(h));
    out.put("base_y", u64::from(base_y));
    out.put("peak_h", u64::from(peak_h));
    out.put("phase", u64::from(phase));
    out.put("temp", u64::from(temp));
    out.put("strength", u64::from(strength));
    out.put("lean", u64::from(lean as u8));
    out.put("cov_cap", u64::from(cov_cap));
    out.put("cell_h", u64::from(cell_h));
    out.put("mode", mode as u64);
}

/// A faded trail cell: `cursor_trail`.
pub(crate) fn trail_cell(out: &mut impl Words, c: &TrailCell) {
    let &TrailCell { row, col, alpha } = c;
    out.put("row", row as u64);
    out.put("col", col as u64);
    out.put("alpha", u64::from(alpha));
}

/// A word decoration sprite: `word_decorations`.
pub(crate) fn word_decoration(out: &mut impl Words, d: &WordDecoration) {
    let &WordDecoration {
        row,
        col,
        dx,
        dy,
        glyph,
        blend,
        color,
        alpha,
    } = d;
    out.put("row", u64::from(row));
    out.put("col", u64::from(col));
    out.put("dx", u64::from(dx as u8));
    out.put("dy", u64::from(dy as u8));
    out.put("glyph", glyph as u64);
    out.put("blend", blend as u64);
    out.put("color", u64::from(color));
    out.put("alpha", u64::from(alpha));
}

/// A cell's word ink: `ink`.
pub(crate) fn ink_cell(out: &mut impl Words, c: &InkCell) {
    let &InkCell {
        row,
        col,
        color: [r, g, b],
    } = c;
    out.put("row", u64::from(row));
    out.put("col", u64::from(col));
    out.put(
        "color",
        u64::from(r) << 16 | u64::from(g) << 8 | u64::from(b),
    );
}

/// A charred glyph's ink: `char_fg`.
pub(crate) fn char_fg(out: &mut impl Words, c: &CharFg) {
    let &CharFg { row, col, fg } = c;
    out.put("row", u64::from(row));
    out.put("col", u64::from(col));
    out.put("fg", u64::from(fg));
}

/// A fire contrast-halo cell: `fire_halo`.
pub(crate) fn fire_halo_cell(out: &mut impl Words, c: &FireHaloCell) {
    let &FireHaloCell { row, col, strength } = c;
    out.put("row", u64::from(row));
    out.put("col", u64::from(col));
    out.put("strength", u64::from(strength));
}

/// An fx layer: `fx_layers`, its content's every item, ink and bound.
pub(crate) fn fx_layer(out: &mut impl Words, l: &FxLayer) {
    let FxLayer {
        id,
        content,
        slot,
        space,
        blend,
        bloom,
        knock,
        offset: (ox, oy),
        clip,
    } = l;
    out.put("id", *id);
    out.group("content", |f| fx_content(f, content));
    out.put("slot", *slot as u64);
    out.put("space", *space as u64);
    out.put("blend", *blend as u64);
    out.flag("bloom", *bloom);
    out.flag("knock", *knock);
    out.put("offset.x", signed(*ox));
    out.put("offset.y", signed(*oy));
    out.group("clip", |f| px_rect(f, *clip));
}

fn fx_content(out: &mut impl Words, c: &FxContent) {
    out.list("items", c.items(), fx_item);
    out.list("palette", c.palette(), fx_ink);
    out.group("extent", |f| px_rect(f, c.extent()));
    out.flag("opaque", c.draws_opaque());
}

fn fx_item(out: &mut impl Words, item: &FxItem) {
    match item {
        FxItem::Rect(r) => {
            let &FxRect { x, y, w, h, ink } = r;
            out.put("kind", 0);
            out.put("x", signed(x));
            out.put("y", signed(y));
            out.put("w", u64::from(w));
            out.put("h", u64::from(h));
            out.put("ink", u64::from(ink));
        }
        FxItem::Ramps(r) => {
            let &FxRamps { x, y, w, h, ink } = r;
            out.put("kind", 1);
            out.put("x", signed(x));
            out.put("y", signed(y));
            out.put("w", u64::from(w));
            out.put("h", u64::from(h));
            out.put("ink", u64::from(ink));
        }
        FxItem::Sprite(s) => {
            let FxSprite {
                x,
                y,
                dw,
                dh,
                flip_x,
                art,
                ink,
                clip,
            } = s;
            out.put("kind", 2);
            out.put("x", signed(*x));
            out.put("y", signed(*y));
            out.put("dw", u64::from(*dw));
            out.put("dh", u64::from(*dh));
            out.flag("flip_x", *flip_x);
            out.group("art", |f| fx_art(f, art));
            out.put("ink", u64::from(*ink));
            out.group("clip", |f| px_rect(f, *clip));
        }
    }
}

/// An fx art by its content id, its size and texel format, and its texels themselves.
fn fx_art(out: &mut impl Words, a: &FxArt) {
    let id = a.id();
    out.put("id.lo", id as u64);
    out.put("id.hi", (id >> 64) as u64);
    out.put("w", u64::from(a.w()));
    out.put("h", u64::from(a.h()));
    out.put("texels", a.texels() as u64);
    out.put("max_index", u64::from(a.max_index()));
    out.put(
        "data",
        aterm_hash::fnv1a_64_extend(aterm_hash::FNV64_OFFSET, a.data()),
    );
}

fn fx_ink(out: &mut impl Words, ink: &FxInk) {
    let &FxInk { color, alpha } = ink;
    out.put("color", u64::from(color));
    out.put("alpha", u64::from(alpha));
}

fn px_rect(out: &mut impl Words, r: PxRect) {
    let PxRect { x0, y0, x1, y1 } = r;
    out.put("x0", signed(x0));
    out.put("y0", signed(y0));
    out.put("x1", signed(x1));
    out.put("y1", signed(y1));
}
