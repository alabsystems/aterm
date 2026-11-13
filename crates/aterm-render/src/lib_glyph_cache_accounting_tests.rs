// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn renderer() -> Renderer {
    Renderer::from_bytes(embedded_font(), 16.0, Theme::default()).expect("embedded font")
}

fn accounting_model() -> aterm_spec::derive::Model {
    aterm_spec::ty_model! {
        GlyphImageClearAccounting {
            const Buggy = 0;
            var resident = 0;
            var charged = 0;
            action Warm when (resident == 0) {
                resident = 1;
                charged = 1;
            }
            action Hit when (resident == 1) {
                charged = charged;
            }
            action Clear {
                resident = 0;
                charged = if Buggy == 1 { charged } else { 0 };
            }
            invariant AccountingFollowsResidency: charged == resident;
        }
    }
}

fn assert_accounted(r: &Renderer) {
    assert_eq!(
        r.glyph_bytes,
        r.glyphs.values().map(|g| g.img.byte_len()).sum::<usize>(),
        "only currently resident bitmap bytes may be charged"
    );
}

#[test]
fn glyph_byte_accounting_follows_real_settings_seal_and_size_clears() {
    let model = accounting_model();
    aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
    let mut state = model.init_state();
    let mut r = renderer();
    for step in [
        "Warm", "Hit", "Settings", "Warm", "Seal", "Warm", "Size", "Warm",
    ] {
        let action = match step {
            "Warm" | "Hit" => {
                let before = r.glyph_bytes;
                let key = r.glyph_key('A');
                let image = r.glyph_image(key);
                assert!(image.bytes().iter().any(|&byte| byte > 0));
                if step == "Hit" {
                    assert_eq!(r.glyph_bytes, before, "hits cannot charge twice");
                }
                step
            }
            "Settings" => {
                r.set_font_thicken(!r.font_thicken());
                "Clear"
            }
            "Seal" => {
                r.seal_admitted_font_sources();
                "Clear"
            }
            "Size" => {
                r.set_px(24.0);
                "Clear"
            }
            _ => unreachable!(),
        };
        assert!(model.fire(action, &mut state));
        assert_accounted(&r);
        assert_eq!(state["resident"], i64::from(!r.glyphs.is_empty()), "{step}");
        assert_eq!(state["charged"], i64::from(r.glyph_bytes > 0), "{step}");
        assert!(model.check_invariant("AccountingFollowsResidency", &state));
    }
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    assert!(buggy.fire("Clear", &mut state));
    assert!(!buggy.check_invariant("AccountingFollowsResidency", &state));
}

fn inflate(r: &mut Renderer, key: GlyphKey, bytes: usize) {
    let entry = r.glyphs.get_mut(&key).expect("the glyph is cached");
    let used = entry.used.get();
    r.glyph_bytes -= entry.img.byte_len();
    *entry = CachedGlyph::new(GlyphImage::Mono {
        width: bytes,
        height: 1,
        xmin: 0,
        ymin: 0,
        advance: 1.0,
        bytes: vec![0xFF; bytes],
    });
    entry.used.set(used);
    r.glyph_bytes += bytes;
}

#[test]
fn glyph_eviction_keeps_the_working_set_of_the_frame_just_drawn() {
    const BYTE_CAP: usize = 12 * 1024 * 1024;
    let mut r = renderer();
    let (hot, cold) = (r.glyph_key('A'), r.glyph_key('B'));
    r.evict_caches_if_large();
    let _ = r.glyph_image(cold);
    r.evict_caches_if_large();
    let _ = r.glyph_image(hot);
    let big = GlyphImage::Mono {
        width: BYTE_CAP + 1,
        height: 1,
        xmin: 0,
        ymin: 0,
        advance: 1.0,
        bytes: vec![0xFF; BYTE_CAP + 1],
    };
    r.glyph_bytes += big.byte_len() - r.glyphs[&hot].img.byte_len();
    r.glyphs.get_mut(&hot).expect("hot is cached").img = big;
    assert_accounted(&r);
    assert!(r.glyph_bytes > BYTE_CAP);

    r.evict_caches_if_large();
    assert!(
        r.glyphs.contains_key(&hot),
        "the working set of the frame just drawn survives the boundary"
    );
    assert!(
        !r.glyphs.contains_key(&cold),
        "what that frame did not read is evicted"
    );
    assert_accounted(&r);

    r.evict_caches_if_large();
    assert!(
        r.glyphs.contains_key(&hot),
        "a frame that reads less does not trim the kept set"
    );
    let _ = r.glyph_image(cold);
    inflate(&mut r, cold, BYTE_CAP + 2);
    assert_accounted(&r);
    r.evict_caches_if_large();
    assert!(
        !r.glyphs.contains_key(&hot),
        "a working set no frame reads is evicted once the cache doubles"
    );
    assert!(r.glyphs.contains_key(&cold));
    assert_eq!(r.glyph_bytes, BYTE_CAP + 2);
    assert_accounted(&r);
}

#[test]
fn a_screen_of_glyphs_past_the_budget_survives_a_cursor_only_frame() {
    const BYTE_CAP: usize = 12 * 1024 * 1024;
    let text = "ABCDEFGH";
    let mut r = renderer();
    let mut term = aterm_core::terminal::Terminal::new(3, 8);
    term.process(format!("{text}\r\n\r\n").as_bytes());
    let mut wc = WindowCpu::new();
    let _ = r.render_input_cached(&mut wc, &term.cell_frame(3, 8));
    let keys: Vec<GlyphKey> = text
        .chars()
        .map(|ch| {
            let cell = RenderCell {
                ch,
                ..RenderCell::default()
            };
            r.resolve_cell_key(None, &cell)
        })
        .collect();
    let each = BYTE_CAP / keys.len() + 1;
    for &key in &keys {
        inflate(&mut r, key, each);
    }
    assert_accounted(&r);
    assert!(r.glyph_bytes > BYTE_CAP, "the screen passes the budget");

    term.process(b"\x1b[3;5H");
    let input = term.cell_frame(3, 8);
    let _ = r.render_input_cached(&mut wc, &input);
    wc.invalidate();
    let _ = r.render_input_cached(&mut wc, &input);
    for (ch, key) in text.chars().zip(&keys) {
        assert_eq!(
            r.glyphs.get(key).map(|g| g.img.byte_len()),
            Some(each),
            "{ch:?}, read by the full repaint before a cursor-only frame, \
             was evicted and re-rasterized"
        );
    }
    assert_accounted(&r);
}
