// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/messages_api_tests.rs"
));

fn stamp() -> WallStamp {
    WallStamp {
        unix_ms: 1_790_000_000_000,
    }
}

fn ms(n: u64) -> aterm_messages::Duration {
    aterm_messages::Duration::from_millis(n)
}

fn band_px(t: &Host, row: usize, col: usize) -> (f64, f64) {
    let m = t.band_metrics();
    (
        (m.pad + col * m.cell_w + m.cell_w / 2) as f64,
        (m.grid_top + row * m.cell_h + m.cell_h / 2) as f64,
    )
}

fn page_head(page: &str) -> (String, String) {
    let head = page.lines().nth(1).expect("the page's head line");
    let field = |name: &str| {
        head.split('\t')
            .find_map(|f| f.strip_prefix(name))
            .expect("a head field")
            .to_string()
    };
    (field("rev="), field("total="))
}

/// The pet's art at cell height `cell_h`, `(w, h)` texels: what tells its
/// sprite from a word cat's.
fn pet_art_size(cell_h: usize) -> (u16, u16) {
    use aterm_effects::kitty_pet::{ART_ASPECT, ART_ROWS};
    let h = (ART_ROWS * cell_h as f32).round();
    ((h * ART_ASPECT).round() as u16, h as u16)
}

fn drawn_pet_body(frame: &RenderInput, m: BandMetrics) -> Option<(f32, f32, f32, f32)> {
    let nat = pet_art_size(m.cell_h);
    let s = aterm_render::sprites::published(&frame.fx_layers)
        .into_iter()
        .find(|s| (s.art.w(), s.art.h()) == nat)?;
    let x0 = m.pad as f32 + s.x as f32;
    let y0 = m.grid_top as f32 + s.y as f32;
    Some((x0, x0 + f32::from(s.w), y0, y0 + f32::from(s.h)))
}

fn wake_the_pet(t: &mut Host) -> bool {
    t.set_cursor_glow(true, "rainbow", None, None, 260, 24, 0.7, 0.6, true);
    t.set_cursor_pet(true, 7);
    draw(t);
    for frame in 0..240u32 {
        if frame % 6 == 0 {
            let ch = char::from(b'a' + u8::try_from(frame / 6 % 26).unwrap_or(0));
            t.note_typed_char(ch);
            t.process(&[ch as u8]);
        }
        t.advance_effects(33.0);
        draw(t);
        if t.cursor_pet_alpha() > 0 {
            return true;
        }
    }
    false
}

#[test]
fn the_pet_is_pressed_where_the_band_draws_it() {
    let (mut t, t0) = clocked(12, 60);
    t.set_chrome(6, 0).expect("admitted geometry");
    t.process(b"\r\n\r\n\r\n$ ");
    assert!(wake_the_pet(&mut t), "the pet must reach the glass");
    let m = t.band_metrics();
    let half_row = m.cell_h as f32 / 2.0;
    for band_rows in [0, 1] {
        if band_rows == 1 {
            let reply = t.notice_at("post ci sev=warn Disk nearly full", t0, stamp());
            assert!(reply.starts_with("OK message="), "{reply}");
            draw(&mut t);
        }
        assert_eq!(t.band_rows(), band_rows);
        assert!(t.cursor_pet_alpha() > 0, "the pet is on the glass");
        let (x0, x1, y0, y1) =
            drawn_pet_body(&t.frame_scratch, m).expect("the pet's body is drawn");
        assert!(
            y0 - half_row >= t.grid_top_px() as f32,
            "fixture: the row above the cat is the terminal's, not the band's"
        );
        let cx = (x0 + x1) / 2.0;
        assert_eq!(
            t.pet_press_px(cx, y0 - half_row),
            0,
            "band rows {band_rows}: a press on the text above the drawn cat is the terminal's"
        );
        assert_eq!(
            t.pet_press_px(cx, (y0 + y1) / 2.0),
            1,
            "band rows {band_rows}: a press on the drawn cat's centre pets it"
        );
        assert_eq!(
            t.pet_press_px(cx, y1 - half_row / 2.0),
            1,
            "band rows {band_rows}: a press on the drawn cat's feet pets it"
        );
    }
}

#[test]
fn a_still_pointers_chip_never_lights_the_echo() {
    let (mut t, t0) = clocked(12, 60);
    let (mut twin, _) = clocked(12, 60);
    twin.band.reset_at(t0);
    for x in [&mut t, &mut twin] {
        x.set_chrome(6, 0).expect("admitted geometry");
        let _ = x.notice_at("progress p pct=40 Downloading assets", t0, stamp());
        x.set_details_view_at(true, t0);
        let _ = x.messages_tick_at(t0 + ms(2100));
    }
    assert_eq!(t.band_rows(), 1, "premise: revealed");
    let (x, y) = band_px(&t, 0, 20);
    assert!(t.band_hover_at(x, y, t0 + ms(2101)), "the body lights");
    for b in [&mut t, &mut twin] {
        let _ = b.messages_tick_at(t0 + ms(2101));
        draw(b);
    }
    assert!(!same_picture(&t, &twin), "control: the lit chip shows");
    for b in [&mut t, &mut twin] {
        let reply = b.notice_at("done p ok Downloaded assets", t0 + ms(2102), stamp());
        assert!(reply.starts_with("OK"), "{reply}");
    }
    let at = t0 + ms(2200);
    for b in [&mut t, &mut twin] {
        let _ = b.messages_tick_at(at);
        draw(b);
    }
    let echo = t
        .band
        .view
        .layout()
        .and_then(|p| p.rows.first().map(|r| r.kind))
        .expect("a laid-out row");
    assert!(
        matches!(echo, aterm_messages::RowKind::Echo(_)),
        "premise: the echo is on the band: {echo:?}"
    );
    let differ = !same_picture(&t, &twin);
    let flips = t.band_hover_at(x, y, at);
    draw(&mut t);
    let resolved = same_picture(&t, &twin);
    assert!(
        !differ,
        "the echo draws a lit chip the still pointer left (a fresh hover at the same \
         pixel changes the lit state: {flips}; after it the frames agree: {resolved})"
    );
}

#[test]
fn the_restore_moves_the_pages_rev() {
    let (mut prev, p0) = clocked(20, 80);
    for line in [
        "post ci Build finished",
        "post ci sev=warn hold=100 Disk nearly full",
    ] {
        let reply = prev.notice_at(line, p0, stamp());
        assert!(reply.starts_with("OK"), "{line}: {reply}");
    }
    let _ = prev.messages_tick_at(p0 + ms(10));
    let kept: Vec<String> = prev
        .messages_log_since_at(0.0)
        .lines()
        .skip(1)
        .map(str::to_string)
        .collect();
    let (mut t, t0) = clocked(20, 80);
    let wall = stamp().unix_ms + 90_000;
    let (rev0, total0) = page_head(&t.messages_page_at("", false, 0, wall));
    assert_eq!(total0, "0", "premise: nothing yet");
    let reply = t.messages_restore_at(&kept.join("\n"), t0 + ms(1));
    assert!(reply.starts_with("OK records=2 "), "{reply}");
    let _ = t.messages_tick_at(t0 + ms(2));
    let (rev1, total1) = page_head(&t.messages_page_at("", false, 0, wall));
    assert_eq!(total1, "2", "premise: the log moved");
    assert_ne!(
        rev1, rev0,
        "the restore moved the log from 0 to 2 records and `rev=` did not move"
    );
    let _ = t.notice_at("post ci sev=warn Fresh warning", t0 + ms(3), stamp());
    let (rev2, _) = page_head(&t.messages_page_at("", false, 0, wall));
    assert_ne!(rev2, rev1, "control: a post moves it");
}
