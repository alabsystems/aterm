// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

// aterm-wasm's tests of the message band surface: `messages_api.rs`, which both web
// crates compile, includes this file into its tests, so these drive this
// crate's own frame path.

use super::*;
use crate::AtermTerminal;
use aterm_core::render::FrameRefill;
use aterm_messages::Duration;

/// The bundled JetBrains Mono — the face the web page ships, so every
/// test here draws with the page's own glyphs and needs no system font.
pub(super) const FONT: &[u8] =
    include_bytes!("../../aterm-render/assets/bundled/JetBrainsMono-Regular.ttf");
const FG: u32 = 0x00d8_d8d8;
const BG: u32 = 0x0018_1818;
const CURSOR: u32 = 0x00a0_c0ff;
const SEL: u32 = 0x0040_4050;

fn term(rows: u16, cols: u16) -> AtermTerminal {
    AtermTerminal::new(rows, cols, FONT, 20.0, FG, BG, CURSOR, SEL).expect("bundled font")
}

fn stamp() -> WallStamp {
    WallStamp {
        unix_ms: 1_790_000_000_000,
    }
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// A terminal whose band's clock starts at `t0`, and `t0`.
fn clocked(rows: u16, cols: u16) -> (AtermTerminal, Instant) {
    let mut t = term(rows, cols);
    let t0 = Instant::now();
    t.band.reset_at(t0);
    (t, t0)
}

/// Every reply the binding gives is the engine's own reply to the same
/// line at the same instant — usage, lane and title refusals, the live-row
/// cap, a record, a row, a restate, a finish, a dismissal, a press — so the
/// web's `notice` IS the control socket's grammar, not a second copy.
#[test]
fn wire_replies_and_caps_are_the_engines() {
    let (mut t, t0) = clocked(20, 80);
    let mut center = MessageCenter::new(MessageLog::default(), t0);
    let mut gate = WireGate::default();
    let lines = [
        "",
        "frob",
        "post",
        "post update Hello",
        "post ci sev=warn hold=0 Held too briefly",
        "post ci Build finished",
        "post ci sev=warn Disk nearly full -- 2 GB left",
        "post ci sev=error Deploy failed -- exit 1",
        "post ci sev=warn key=wire.w3 Third row",
        "post ci sev=warn Fourth row, over the cap",
        "progress p pct=40 Downloading assets",
        "progress p pct=41 Downloading assets",
        "done p ok Downloaded assets",
        "done nope ok",
        "dismiss 3",
        "dismiss 999",
        "act 4 details",
        "act 4 0",
        "act 999 details",
    ];
    for (i, line) in lines.iter().enumerate() {
        let now = t0 + ms(10 * i as u64);
        let want = match NoticeRequest::parse(line) {
            Err(refusal) => format!("ERR {refusal}"),
            Ok(NoticeRequest::Act { id, press }) => {
                match wire::press_target(&center, &mut gate, id, &press, &pct_encode, now) {
                    Ok((_, label)) => wire::acted_reply(label, false, &pct_encode),
                    Err(line) => line,
                }
            }
            Ok(req) => {
                let applied = wire::apply(&mut center, &mut gate, req, stamp(), now);
                if applied.paint == Paint::Now {
                    let _ = center.settle(now, true);
                    let _ = center.commit_rows(now, 22);
                    let _ = center.drain_shelved_for_persist();
                    let _ = gate.take_due(now);
                }
                applied.reply
            }
        };
        assert_eq!(t.notice_at(line, now, stamp()), want, "{line:?}");
    }
    // Non-vacuous: the refusals and the cap really answer, in the
    // engine's words.
    assert!(t
        .notice_at("frob", t0, stamp())
        .starts_with("ERR usage: notice"));
    let (mut t, t0) = clocked(20, 80);
    for (i, title) in ["One", "Two", "Three"].into_iter().enumerate() {
        let reply = t.notice_at(
            &format!("post ci sev=warn {title}"),
            t0 + ms(i as u64),
            stamp(),
        );
        assert!(reply.starts_with("OK message="), "{reply}");
    }
    assert_eq!(
        t.notice_at("post ci sev=warn Four", t0 + ms(5), stamp()),
        "ERR notice: 3 wire messages are live; end one with notice done",
        "three live wire rows at most"
    );
    assert_eq!(
        t.notice_at("act 1 details", t0 + ms(6), stamp()),
        format!("OK acted={} performed=0", pct_encode("Details \u{203a}")),
        "a press names the capsule and says the web performed nothing"
    );
}

/// AN IDLE BAND ARMS NO TIMER: nothing posted, and a record — a success or
/// info `post`, which goes to the log and never to the glass — leave
/// `messages_next_wake_ms` at -1 and the band empty.
#[test]
fn an_idle_band_arms_no_timer() {
    let (mut t, t0) = clocked(20, 80);
    assert!(t.messages_next_wake_ms_at(t0) < 0.0);
    assert_eq!(t.messages_tick_at(t0), 0);
    let reply = t.notice_at("post ci Build finished", t0, stamp());
    assert!(reply.starts_with("OK recorded message="), "{reply}");
    assert_eq!(t.messages_tick_at(t0 + ms(5)), 0);
    assert_eq!(t.band_rows(), 0, "a record never reaches the glass");
    assert!(t.messages_next_wake_ms_at(t0 + ms(5)) < 0.0);
    assert_eq!(
        t.messages_read_at("", 0).lines().count(),
        2,
        "it is in the log"
    );
}

/// A held warning arms exactly its hold's end, and nothing between.
#[test]
fn a_held_warn_arms_exactly_its_hold() {
    let (mut t, t0) = clocked(20, 80);
    let reply = t.notice_at("post ci sev=warn hold=5 Disk nearly full", t0, stamp());
    assert!(reply.starts_with("OK message="), "{reply}");
    assert_eq!(t.band_rows(), 1);
    assert_eq!(t.messages_next_wake_ms_at(t0), 5000.0);
    assert_eq!(t.messages_next_wake_ms_at(t0 + ms(1234)), 3766.0);
    assert_eq!(t.messages_tick_at(t0 + ms(5000)), 1, "the row folds");
    // The shrink's quiet, then nothing.
    assert_eq!(
        t.messages_tick_at(t0 + ms(5000) + aterm_messages::SHRINK_QUIET),
        3,
        "the band changed and its row left"
    );
    assert_eq!(t.band_rows(), 0);
    assert!(t.messages_next_wake_ms_at(t0 + ms(7000)) < 0.0);
}

/// A busy row's comet asks for the engine's 33 ms grid only while the band
/// may move; the still look asks for no frame between its words' ticks.
#[test]
fn a_comet_wakes_on_the_frame_grid_only_while_moving() {
    let (mut t, t0) = clocked(20, 80);
    let _ = t.notice_at("progress b busy Indexing the tree", t0, stamp());
    let at = t0 + aterm_messages::PROGRESS_GRACE + ms(100);
    let _ = t.messages_tick_at(at);
    assert_eq!(t.band_rows(), 1);
    let moving = t.messages_next_wake_ms_at(at);
    assert!((0.0..=33.0).contains(&moving), "{moving}");
    t.set_band_motion_at(false, at);
    let still = t.messages_next_wake_ms_at(at);
    assert!(still < 0.0 || still > 33.0, "{still}");
}

/// F4 (ruling 435): the wake is the native window's rule, so a resize
/// between a tick and a render is read at the NEW width — the same answer
/// the page gets once it renders. A busy row in the still look ticks its
/// elapsed words at 80 columns and has none at 20. NEGATIVE CONTROL: the
/// kept layout read from the last tick (the old web rule) slept 108 s past
/// the words the grown page shows.
#[test]
fn a_resize_before_the_render_wakes_for_the_new_width() {
    let (mut t, t0) = clocked(20, 20);
    let _ = t.notice_at("progress b busy Indexing the tree", t0, stamp());
    let at = t0 + aterm_messages::PROGRESS_GRACE + ms(12_100);
    let _ = t.messages_tick_at(t0 + aterm_messages::PROGRESS_GRACE + ms(100));
    let _ = t.messages_tick_at(at);
    t.set_band_motion_at(false, at);
    let narrow = t.messages_next_wake_ms_at(at);
    assert!(narrow > 60_000.0, "no words tick at 20 columns: {narrow}");
    t.resize(20, 80);
    let before_render = t.messages_next_wake_ms_at(at);
    assert!(
        (0.0..=1000.0).contains(&before_render),
        "the grown page's elapsed words tick: {before_render}"
    );
    t.render();
    assert_eq!(t.messages_next_wake_ms_at(at), before_render);
}

/// Ruling 436: a glide in flight is read on the page's measured frame
/// period — a 120 Hz page wakes every ~8 ms where the default reads 60 Hz
/// — and a period that is not a positive number changes nothing.
#[test]
fn a_glide_is_read_on_the_pages_frame_period() {
    let glide_wake = |period: Option<f64>| {
        let (mut t, t0) = clocked(20, 80);
        if let Some(p) = period {
            t.set_frame_period_ms(p);
        }
        let _ = t.notice_at("progress b pct=10 Building aterm", t0, stamp());
        let at = t0 + aterm_messages::PROGRESS_GRACE + ms(100);
        let _ = t.messages_tick_at(at);
        let _ = t.notice_at("progress b pct=90 Building aterm", at, stamp());
        let _ = t.messages_tick_at(at + ms(1));
        t.messages_next_wake_ms_at(at + ms(1))
    };
    let default = glide_wake(None);
    let fast = glide_wake(Some(1000.0 / 120.0));
    assert!((0.0..=9.0).contains(&fast), "{fast}");
    assert!(default > 9.0 && default <= 17.0, "{default}");
    for junk in [f64::NAN, -5.0, 0.0, f64::INFINITY] {
        assert_eq!(glide_wake(Some(junk)), default, "{junk}");
    }
}

/// Ruling 436: the page's pointer is a hand exactly where a press acts —
/// `band_press_acts` agrees with what `band_press` does on every cell of
/// a row, with and without a Details view — and never off the band.
#[test]
fn the_pointer_is_a_hand_exactly_where_a_press_acts() {
    let fresh = |details: bool| {
        let (mut t, t0) = clocked(12, 60);
        let _ = t.notice_at(
            "post ci sev=warn Disk nearly full -- 2 GB left",
            t0,
            stamp(),
        );
        t.set_details_view_at(details, t0);
        (t, t0)
    };
    for details in [false, true] {
        let (t, _) = fresh(details);
        let mut hands = 0;
        for col in 0..60 {
            let (x, y) = band_px(&t, 0, col);
            let acts = t.band_press_acts(x, y);
            let (mut twin, t0) = fresh(details);
            let pressed = twin.band_press_at(x, y, t0 + ms(1));
            assert_eq!(acts, pressed != "OK none", "col {col}: {pressed}");
            hands += usize::from(acts);
        }
        assert_eq!(hands > 0, details, "{hands} cells act");
        let (x, _) = band_px(&t, 0, 3);
        assert!(!t.band_press_acts(x, -1.0), "off the frame");
        assert!(!t.band_press_acts(x, 400.0), "on the grid");
    }
}

/// WF-1: once the band's frame is rendered, nothing asks for another.
#[test]
fn a_settled_band_needs_no_frame() {
    let (mut t, t0) = clocked(20, 80);
    let _ = t.notice_at("post ci sev=warn Disk nearly full", t0, stamp());
    assert!(t.needs_frame());
    t.render();
    assert!(!t.needs_frame(), "the band's frame is on the glass");
    t.render();
    assert!(t.last_render_skipped());
    assert_eq!(
        t.messages_tick_at(t0 + ms(40)),
        0,
        "a tick that moved nothing"
    );
    assert!(!t.needs_frame());
}

/// DMG-1 UNDER A BAND: an echo frame refills only its damaged rows, because
/// the render takes last frame's band rows off the kept scratch first.
#[test]
fn an_echo_frame_under_a_band_takes_the_scoped_refill() {
    let (mut t, t0) = clocked(20, 80);
    let _ = t.notice_at("post ci sev=warn Disk nearly full", t0, stamp());
    t.process(b"$ ");
    t.render();
    // The first frames are full refills of their own (a fresh grid is
    // full damage, and a prepend onto a full-damage fill is not blessed),
    // exactly as on the native window; the steady state is what counts.
    t.process(b"x");
    t.render();
    t.process(b"y");
    t.render();
    assert!(
        matches!(t.last_refill, Some(FrameRefill::Scoped { .. })),
        "{:?}",
        t.last_refill
    );
    assert_eq!(t.frame_scratch.rows, 21, "the band row is back on top");
    assert_eq!(t.frame_scratch.cells[1][3].ch, 'y');
    assert_eq!(
        t.frame_scratch.cells[0][0].ch, ' ',
        "the band's glyph margin"
    );
}

/// WITH NO BAND ROW NOTHING CHANGES: a terminal that only recorded a
/// message renders byte for byte what one that never heard of the band
/// does.
#[test]
fn no_band_row_leaves_the_frame_byte_identical() {
    let mut plain = term(12, 60);
    let (mut quiet, t0) = clocked(12, 60);
    let _ = quiet.notice_at("post ci Build finished", t0, stamp());
    let _ = quiet.messages_tick_at(t0 + ms(40));
    for t in [&mut plain, &mut quiet] {
        t.set_chrome(6, 0);
        t.process(b"$ ls\r\nCargo.toml  src\r\n$ ");
        t.render();
    }
    assert_eq!(
        (plain.width(), plain.height()),
        (quiet.width(), quiet.height())
    );
    assert!(plain.rgba() == quiet.rgba(), "the same pixels");
    assert_eq!(quiet.grid_top_px(), 6);
    assert_eq!(quiet.band_height_px(), 0);
}

/// The rows RESERVE space: the frame grows by exactly the band's rows, the
/// grid starts below them, and the terminal's own rows are all still there.
#[test]
fn the_frame_grows_by_the_band_rows() {
    let mut plain = term(12, 60);
    let (mut banded, t0) = clocked(12, 60);
    let _ = banded.notice_at("post ci sev=warn Disk nearly full", t0, stamp());
    let _ = banded.notice_at("post ci sev=error Deploy failed", t0 + ms(40), stamp());
    for t in [&mut plain, &mut banded] {
        t.set_chrome(6, 0);
        t.process(b"$ ");
        t.render();
    }
    let ch = banded.cell_height();
    assert_eq!(banded.band_rows(), 2);
    assert_eq!(banded.band_height_px() as usize, 2 * ch);
    assert_eq!(banded.width(), plain.width());
    assert_eq!(banded.height(), plain.height() + 2 * ch);
    assert_eq!(banded.grid_top_px() as usize, 6 + 2 * ch);
    assert_eq!(banded.frame_scratch.rows, 14);
    assert_eq!(banded.frame_scratch.grid_top_row, 2);
    assert_eq!(banded.frame_scratch.grid_bot_row, 14);
    // The grid's pixels are the plain frame's, one band lower.
    let w = plain.width() * 4;
    let dy = 2 * ch * w;
    let top = 6 * w;
    assert!(
        plain.rgba()[top..] == banded.rgba()[top + dy..],
        "the grid is the plain grid, moved down by the band"
    );
}

/// The pet's drawn body in FRAME px `(x0, x1, y0, y1)`, read off the frame
/// the renderer drew: the free sprite whose atlas tile is the body's
/// natural size (`ART_ROWS` cells tall at `ART_ASPECT` — motes and
/// word-cats bake at other sizes), placed where the rasterizer stamps it,
/// `pad` in and `grid_top` down (any band rows are already in its `y`).
fn drawn_pet_body(frame: &RenderInput, m: BandMetrics) -> Option<(f32, f32, f32, f32)> {
    use aterm_effects::kitty_pet::{ART_ASPECT, ART_ROWS};
    let nat_h = (ART_ROWS * m.cell_h as f32).round();
    let nat = ((nat_h * ART_ASPECT).round() as u16, nat_h as u16);
    let s = frame.free_sprites.iter().find(|s| (s.aw, s.ah) == nat)?;
    let x0 = m.pad as f32 + s.x as f32;
    let y0 = m.grid_top as f32 + s.y as f32;
    Some((x0, x0 + f32::from(s.w), y0, y0 + f32::from(s.h)))
}

/// Drive the pet onto the glass through the binding alone — the glow
/// style that names it, the seed door, then witnessed keystrokes at 33 ms
/// — until `cursor_pet_alpha` goes positive.
fn wake_the_pet(t: &mut AtermTerminal) -> bool {
    t.set_cursor_glow(true, "rainbow kitty", None, None, 260, 24, 0.7, 0.6, true);
    t.set_cursor_pet(true, 7);
    t.render();
    for frame in 0..240u32 {
        if frame % 6 == 0 {
            let ch = char::from(b'a' + u8::try_from(frame / 6 % 26).unwrap_or(0));
            t.note_typed_char(ch);
            t.process(&[ch as u8]);
        }
        t.advance_effects(33.0);
        t.render();
        if t.cursor_pet_alpha() > 0 {
            return true;
        }
    }
    false
}

/// THE PET UNDER A BAND IS PRESSED WHERE IT IS DRAWN. The band's rows
/// land above the grid after the effects ran, and the compose moves the
/// pet's sprite down with the grid; the page presses in canvas pixels,
/// band included (ruling 347). So with a row committed, a press at the
/// drawn body's centre or its feet pets the cat, and a press on the text
/// half a row above the drawn body passes to the terminal — where a hit
/// rect left a band row too high swallowed that press and let a press on
/// the cat's feet start a selection. Without the band the same holds (the
/// control).
#[test]
fn the_pet_is_pressed_where_the_band_draws_it() {
    let (mut t, t0) = clocked(12, 60);
    t.set_chrome(6, 0);
    // The caret three rows down, so a text row lies above the cat.
    t.process(b"\r\n\r\n\r\n$ ");
    assert!(wake_the_pet(&mut t), "the pet must reach the glass");
    let m = t.band_metrics();
    let half_row = m.cell_h as f32 / 2.0;
    for band_rows in [0, 1] {
        if band_rows == 1 {
            let reply = t.notice_at("post ci sev=warn Disk nearly full", t0, stamp());
            assert!(reply.starts_with("OK message="), "{reply}");
            t.render();
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

/// When the band leaves, the frame is the band-less frame again — the
/// cached renderer keeps no band pixel, no bleed and no stale gutter.
#[test]
fn the_band_leaving_restores_the_bandless_frame() {
    let mut plain = term(12, 60);
    let (mut banded, t0) = clocked(12, 60);
    for t in [&mut plain, &mut banded] {
        t.set_chrome(6, 4);
        t.process(b"$ ls\r\nCargo.toml  src\r\n$ ");
    }
    let _ = banded.notice_at("progress p pct=50 Downloading assets", t0, stamp());
    let _ = banded.messages_tick_at(t0 + ms(2500));
    banded.render();
    assert_eq!(banded.band_rows(), 1);
    let _ = banded.notice_at("done p withdraw", t0 + ms(3000), stamp());
    let mut at = t0 + ms(3000);
    for _ in 0..200 {
        let wake = banded.messages_next_wake_ms_at(at);
        if wake < 0.0 {
            break;
        }
        at += ms(wake as u64).max(ms(1));
        let _ = banded.messages_tick_at(at);
        banded.render();
    }
    assert_eq!(banded.band_rows(), 0, "the row left and the count followed");
    assert!(
        banded.messages_next_wake_ms_at(at) < 0.0,
        "and nothing is armed"
    );
    banded.render();
    plain.render();
    assert_eq!(
        (plain.width(), plain.height()),
        (banded.width(), banded.height())
    );
    assert!(
        plain.rgba() == banded.rgba(),
        "the band-less frame, byte for byte"
    );
}

/// The `messages` read is the engine's rows over the ring, paged as the
/// control socket pages them.
#[test]
fn messages_read_pages_the_log() {
    let (mut t, t0) = clocked(20, 80);
    for i in 0..5u64 {
        let _ = t.notice_at(&format!("post ci Build {i} finished"), t0 + ms(i), stamp());
    }
    assert_eq!(
        t.messages_read_at("junk", 0),
        "ERR usage: messages [<n>] [since=<id>] [tag=<tag>] [sev=<sev>] [live]"
    );
    let page = t.messages_read_at("2", 7);
    let want = wire::message_rows(
        &t.band.center,
        &wire::parse_read_args("2").expect("args"),
        7,
        &pct_encode,
    );
    assert_eq!(page, format!("OK 2\n{}", want.join("\n")));
    assert!(page.contains("Build%204%20finished"), "{page}");
    let since = t.messages_read_at("since=3", 7);
    assert!(since.starts_with("OK 2\n"), "{since}");
    assert_eq!(
        t.messages_read_at("live", 7),
        "OK 0",
        "records are never live"
    );
}

/// F2, A HIDDEN PAGE ARMS NOTHING (ruling 337): a live row — a busy one
/// past its grace, and a held warning — arms `-1` while the page is
/// hidden, where the same rows on a visible page each arm a wake (a
/// still look included, the negative control); hidden, the band draws
/// the still look. Visible again, the page is told what came due while
/// it slept (`0`: tick now), and the tick settles it.
#[test]
fn a_hidden_page_arms_no_timer_and_resumes() {
    let (mut t, t0) = clocked(20, 80);
    let _ = t.notice_at("progress b busy Indexing the tree", t0, stamp());
    let _ = t.notice_at("post ci sev=warn hold=5 Disk nearly full", t0, stamp());
    let at = t0 + aterm_messages::PROGRESS_GRACE + ms(100);
    let _ = t.messages_tick_at(at);
    assert_eq!(t.band_rows(), 2);
    let moving = t.messages_next_wake_ms_at(at);
    assert!((0.0..=33.0).contains(&moving), "{moving}");
    // The negative control: a VISIBLE still page still arms its state.
    t.set_band_motion_at(false, at);
    let still = t.messages_next_wake_ms_at(at);
    assert!(still >= 0.0, "a visible still page keeps its hold: {still}");
    let still_rows = t.band.rows.clone();
    t.set_band_motion_at(true, at);
    t.set_band_visible_at(false, at);
    assert_eq!(t.messages_next_wake_ms_at(at), -1.0, "hidden: no timer");
    assert_eq!(t.band.look().pace, aterm_messages::Pace::Still);
    assert!(t.band.rows == still_rows, "hidden paints the still look");
    // Long past the hold, still hidden: nothing is armed, nothing moved.
    let later = at + ms(9000);
    assert_eq!(t.messages_next_wake_ms_at(later), -1.0);
    assert_eq!(t.band_rows(), 2);
    // Visible again: what came due is due now, and the tick settles it.
    t.set_band_visible_at(true, later);
    assert_eq!(
        t.messages_next_wake_ms_at(later),
        0.0,
        "the hold is overdue"
    );
    assert_eq!(t.messages_tick_at(later) & 1, 1, "the warning folds");
    assert_eq!(t.band.look().pace, aterm_messages::Pace::Moving);
    let resumed = t.messages_next_wake_ms_at(later);
    assert!(resumed >= 0.0, "the comet asks again: {resumed}");
}

/// Frame pixel `(x, y)` at the middle of band cell `(row, col)`.
fn band_px(t: &AtermTerminal, row: usize, col: usize) -> (f64, f64) {
    let m = t.band_metrics();
    (
        (m.pad + col * m.cell_w + m.cell_w / 2) as f64,
        (m.grid_top + row * m.cell_h + m.cell_h / 2) as f64,
    )
}

/// The `Details ›` capsule of band row `row`: its first column and width.
fn details_chip(t: &AtermTerminal, row: usize) -> (usize, usize) {
    let p = t.band.view.layout().expect("a laid-out band");
    let c = p.rows[row]
        .capsules
        .iter()
        .find(|c| c.action.is_details())
        .expect("the row ends in its Details link");
    (c.col, c.width)
}

/// THE POINTER ON THE WEB (c). Without a Details view the links are
/// withheld: the row body lights nothing and a press there does nothing.
/// Once the page has one, every row ends in `Details ›`; the pointer on
/// the row body lights that chip and ONLY that chip (every pixel that
/// moves is inside its cells), moving along the body changes nothing,
/// leaving restores the resting frame byte for byte; and a press is
/// native's `notice act <id> details`: the same reply words, the same
/// record afterwards (the row seen and folded), with the row's id for
/// the page's Details view.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one walk through the pointer's states"
)]
fn the_pointer_lights_only_the_chip_and_a_press_is_notice_act() {
    let (mut t, t0) = clocked(12, 60);
    t.set_chrome(6, 0);
    let _ = t.notice_at(
        "post ci sev=warn Disk nearly full -- 2 GB left",
        t0,
        stamp(),
    );
    t.render();
    let (x, y) = band_px(&t, 0, 20);
    assert!(
        !t.band_hover_at(x, y, t0),
        "withheld links: nothing to light"
    );
    assert_eq!(t.band_press_at(x, y, t0), "OK none");
    assert_eq!(t.band_rows(), 1, "and nothing happened");
    // The page says it has a Details view: the rows gain their links.
    t.set_details_view_at(true, t0);
    assert!(t.needs_frame(), "the links are a new frame");
    t.render();
    let words: String = t.frame_scratch.cells[0].iter().map(|c| c.ch).collect();
    assert!(words.contains("Details"), "{words}");
    let rest = t.rgba();
    let (chip, width) = details_chip(&t, 0);
    // The body lights the chip, and only the chip.
    assert!(t.band_hover_at(x, y, t0 + ms(1)));
    assert!(
        !t.band_hover_at(x + 3.0, y, t0 + ms(2)),
        "same chip: no change"
    );
    t.render();
    let lit = t.rgba();
    let m = t.band_metrics();
    let w = t.width();
    let (y0, y1) = (m.grid_top, m.grid_top + m.cell_h);
    let (x0, x1) = (m.pad + chip * m.cell_w, m.pad + (chip + width) * m.cell_w);
    let mut moved = 0usize;
    for (i, (a, b)) in rest.chunks(4).zip(lit.chunks(4)).enumerate() {
        if a != b {
            let (px, py) = (i % w, i / w);
            assert!(
                (y0..y1).contains(&py) && (x0..x1).contains(&px),
                "pixel ({px}, {py}) moved outside the chip {x0}..{x1} x {y0}..{y1}"
            );
            moved += 1;
        }
    }
    assert!(moved > 0, "the chip lit");
    // Leaving restores the resting frame.
    assert!(t.band_hover_at(-1.0, -1.0, t0 + ms(3)));
    t.render();
    assert!(t.rgba() == rest, "the resting frame, byte for byte");
    // The press is native's `notice act 1 details`, on a twin.
    let (mut twin, _) = clocked(12, 60);
    twin.band.reset_at(t0);
    let _ = twin.notice_at(
        "post ci sev=warn Disk nearly full -- 2 GB left",
        t0,
        stamp(),
    );
    twin.set_details_view_at(true, t0);
    let native = twin.notice_at("act 1 details", t0 + ms(4), stamp());
    assert_eq!(
        native,
        format!("OK acted={} performed=1", pct_encode("Details \u{203a}"))
    );
    let pressed = t.band_press_at(x, y, t0 + ms(4));
    assert_eq!(pressed, format!("{native} id=1"));
    assert_eq!(
        t.messages_read_at("", 9),
        twin.messages_read_at("", 9),
        "the same record afterwards"
    );
    assert_eq!(t.band_rows(), 1, "the row folds after the shrink's quiet");
    assert!(
        t.messages_read_at("", 9).contains("state=folded"),
        "{}",
        t.messages_read_at("", 9)
    );
    // Off the band — above it, and on the grid — a press is nobody's.
    assert_eq!(t.band_press_at(x, 0.0, t0 + ms(5)), "OK none");
    assert_eq!(t.band_press_at(x, 400.0, t0 + ms(5)), "OK none");
}

/// A STILL POINTER'S LIT CHIP FOLLOWS THE LAYOUT (ruling 441 (f)), under
/// ruling 341's "a pointer lights only where a press acts": an echo is
/// `Hit::Nothing` and a press there is `OK none`, so the chip the pointer
/// lit never stays lit on the echo a row leaves (the web twin of rulings
/// 417 (k) and 418 (a)). A progress row the pointer rests on ends `done …
/// ok` (a Complete echo): the band draws as a twin's that took the same
/// lines with no pointer at all. (A Details press on a plain warning
/// leaves no echo — only an animated row records one — so the press itself
/// does not reach this.)
#[test]
fn a_still_pointers_chip_never_lights_the_echo() {
    let (mut t, t0) = clocked(12, 60);
    let (mut twin, _) = clocked(12, 60);
    twin.band.reset_at(t0);
    for x in [&mut t, &mut twin] {
        x.set_chrome(6, 0);
        let _ = x.notice_at("progress p pct=40 Downloading assets", t0, stamp());
        x.set_details_view_at(true, t0);
        let _ = x.messages_tick_at(t0 + ms(2100));
    }
    assert_eq!(t.band_rows(), 1, "premise: revealed");
    let (x, y) = band_px(&t, 0, 20);
    assert!(t.band_hover_at(x, y, t0 + ms(2101)), "the body lights");
    for b in [&mut t, &mut twin] {
        let _ = b.messages_tick_at(t0 + ms(2101));
        b.render();
    }
    assert!(t.rgba() != twin.rgba(), "control: the lit chip shows");
    for b in [&mut t, &mut twin] {
        let reply = b.notice_at("done p ok Downloaded assets", t0 + ms(2102), stamp());
        assert!(reply.starts_with("OK"), "{reply}");
    }
    let at = t0 + ms(2200);
    for b in [&mut t, &mut twin] {
        let _ = b.messages_tick_at(at);
        b.render();
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
    let differ = t.rgba() != twin.rgba();
    let flips = t.band_hover_at(x, y, at);
    t.render();
    let resolved = t.rgba() == twin.rgba();
    assert!(
        !differ,
        "the echo draws a lit chip the still pointer left (a fresh hover at the same \
         pixel changes the lit state: {flips}; after it the frames agree: {resolved})"
    );
}

/// Without a Details view, `notice act <id> details` performs nothing AND
/// changes nothing — nothing was shown, so nothing is marked seen — while
/// with one it marks the row seen, as native's press does.
#[test]
fn a_details_press_needs_the_pages_details_view() {
    let (mut t, t0) = clocked(12, 60);
    let _ = t.notice_at("post ci sev=warn Disk nearly full", t0, stamp());
    let reply = t.notice_at("act 1 details", t0 + ms(1), stamp());
    assert!(reply.ends_with("performed=0"), "{reply}");
    assert!(t.messages_read_at("", 9).contains("state=held"));
    t.set_details_view_at(true, t0 + ms(2));
    let reply = t.notice_at("act 1 details", t0 + ms(3), stamp());
    assert!(reply.ends_with("performed=1"), "{reply}");
    assert!(t.messages_read_at("", 9).contains("state=folded"));
}

/// The overflow row: with a Details view a press anywhere on it opens the
/// view at the top of the log; without one it is words alone.
#[test]
fn the_overflow_row_opens_the_details_view_only_with_one() {
    // A fixed three-row canvas: the page re-grids on every row the band
    // takes (bit 1), so the third warning finds room for two rows only.
    let (mut t, t0) = clocked(3, 60);
    for (i, title) in ["One", "Two", "Three"].into_iter().enumerate() {
        let _ = t.notice_at(
            &format!("post ci sev=warn {title}"),
            t0 + ms(i as u64),
            stamp(),
        );
        let fit = 3 - t.band_rows() as u16;
        t.resize(fit, 60);
    }
    let _ = t.messages_tick_at(t0 + ms(10));
    assert_eq!(t.band_rows(), 2);
    let last = 1;
    let p = t.band.lay(t.cols).presentation(&t.band.center);
    let overflow = matches!(p.rows[last].kind, aterm_messages::RowKind::Overflow { .. });
    assert!(
        overflow,
        "three rows on a two-row band: the last is the overflow"
    );
    let (x, y) = band_px(&t, last, 3);
    assert_eq!(t.band_press_at(x, y, t0 + ms(11)), "OK none");
    t.set_details_view_at(true, t0 + ms(12));
    assert_eq!(t.band_press_at(x, y, t0 + ms(13)), "OK messages");
}

/// D, CSS `forced-colors`: the five system colours become the engine's
/// forced inks (`BandInks::forced`, the native High Contrast mapping),
/// the look turns ungraded, the band is re-inked in them (its ground is
/// `ButtonFace`), a theme change under them moves nothing, and switching
/// them off returns the theme's frame byte for byte. The same palette
/// twice is no change (the negative control).
#[test]
fn forced_colors_own_the_band_ungraded() {
    let (mut t, t0) = clocked(12, 60);
    t.set_chrome(6, 0);
    let _ = t.notice_at("post ci sev=warn Disk nearly full", t0, stamp());
    t.render();
    let themed = t.rgba();
    let graded = t.band.inks;
    // Five distinct colours, so a swap between any two of the five
    // slots changes the inks or the ground (a palette with three
    // blacks let `canvas`, `highlight_text` and `button_face` trade
    // places unseen).
    let hc = ForcedPalette {
        window: [0x00, 0x00, 0x10],
        window_text: [0xff, 0xff, 0xff],
        highlight: [0x1a, 0xeb, 0xff],
        highlight_text: [0x10, 0x00, 0x00],
        btn_face: [0x00, 0x10, 0x00],
    };
    let pack = |c: [u8; 3]| (u32::from(c[0]) << 16) | (u32::from(c[1]) << 8) | u32::from(c[2]);
    let set = |t: &mut AtermTerminal, on: bool, at: Instant| {
        t.set_forced_colors_at(
            on,
            pack(hc.window),
            pack(hc.window_text),
            pack(hc.highlight),
            pack(hc.highlight_text),
            pack(hc.btn_face),
            at,
        );
    };
    set(&mut t, true, t0 + ms(1));
    assert_eq!(
        t.band.inks,
        BandInks::forced(hc),
        "the engine's forced derivation"
    );
    assert!(!t.band.look().graded, "ungraded under forced colours");
    assert!(t.needs_frame(), "re-inked");
    t.render();
    assert!(t.frame_scratch.cells[0].iter().all(|c| c.bg == hc.btn_face));
    assert!(t.rgba() != themed);
    let forced_frame = t.rgba();
    set(&mut t, true, t0 + ms(2));
    assert!(!t.needs_frame(), "the same palette: no change");
    t.set_theme(0x0010_2030, 0x00f0_f0f0, 0x0000_00ff, SEL);
    assert_eq!(t.band.inks, BandInks::forced(hc), "the OS owns the colour");
    set(&mut t, false, t0 + ms(3));
    t.set_theme(FG, BG, CURSOR, SEL);
    assert_eq!(t.band.inks, graded, "the theme's inks again");
    assert!(t.band.look().graded);
    t.render();
    assert!(t.rgba() == themed, "the themed frame, byte for byte");
    assert!(forced_frame != themed);
}

/// E, THE LOG ACROSS RELOADS: the lines the page keeps are the engine's
/// `m1` codec lines, each decoding to the record it was drained as —
/// a record posted since the last tick included — paged by a cursor; a
/// page that fell behind the journal reads a `dropped` line first.
#[test]
fn the_log_drains_as_codec_lines_since_a_cursor() {
    let (mut t, t0) = clocked(20, 80);
    assert_eq!(t.messages_log_since_at(0.0), "OK 0 0");
    let _ = t.notice_at("post ci Build finished", t0, stamp());
    let _ = t.notice_at("post ci sev=warn Disk nearly full", t0 + ms(1), stamp());
    let first = t.messages_log_since_at(0.0);
    let mut lines = first.lines();
    let head: Vec<&str> = lines.next().expect("head").split(' ').collect();
    assert_eq!(head[0], "OK");
    let next: u64 = head[1].parse().expect("next");
    let n: usize = head[2].parse().expect("n");
    // The record's post and its retirement (a record is on the log, never
    // the glass), then the warning's post.
    assert_eq!(n, 3, "{first}");
    assert_eq!(next, 3);
    let ids: Vec<Option<MessageId>> = lines
        .map(|l| LogLine::decode(l).expect("an m1 line").id())
        .collect();
    let (one, two) = (MessageId::from_raw(1), MessageId::from_raw(2));
    assert_eq!(ids, vec![one, one, two]);
    assert_eq!(t.messages_log_since_at(3.0), "OK 3 0", "nothing new");
    assert_eq!(
        t.messages_log_since_at(99.0),
        "OK 3 0",
        "a cursor past the end"
    );
    // Past the cap, the oldest go, and a reader behind is told how many.
    let filler: Vec<(LogLine, Shelf)> = (0..JOURNAL_CAP)
        .map(|_| (LogLine::Dropped { count: 7 }, Shelf::Wire))
        .collect();
    t.band.journal_push(filler);
    let behind = t.messages_log_since_at(0.0);
    let mut lines = behind.lines();
    assert_eq!(
        lines.next(),
        Some(format!("OK {} {}", JOURNAL_CAP + 3, JOURNAL_CAP + 1).as_str())
    );
    assert_eq!(
        LogLine::decode(lines.next().expect("dropped")).expect("m1"),
        LogLine::Dropped { count: 3 },
        "the three lines evicted before the reader took them"
    );
    assert_eq!(
        t.messages_log_since_at(3.0).lines().count(),
        JOURNAL_CAP + 1,
        "a reader in step misses nothing"
    );
}

/// E, THE QUIT RECORD: a closing page's rows are recorded as cut off by
/// the quit (ruling 267) — a determinate row with its percent — and the
/// lines come back from the page's cursor; the band empties.
#[test]
fn a_closing_page_writes_the_quit_record() {
    let (mut t, t0) = clocked(20, 80);
    let _ = t.notice_at("post ci sev=warn Disk nearly full", t0, stamp());
    let _ = t.notice_at("progress p pct=28 Downloading assets", t0, stamp());
    let _ = t.messages_tick_at(t0 + ms(2500));
    let cursor = t
        .messages_log_since_at(0.0)
        .lines()
        .next()
        .expect("head")
        .split(' ')
        .nth(1)
        .expect("next")
        .parse::<f64>()
        .expect("cursor");
    let out = t.messages_quit_at(cursor, t0 + ms(3000));
    let lines: Vec<LogLine> = out
        .lines()
        .skip(1)
        .map(|l| LogLine::decode(l).expect("m1"))
        .collect();
    assert_eq!(lines.len(), 2, "{out}");
    for line in &lines {
        assert!(
            matches!(
                line,
                LogLine::Retired {
                    how: aterm_messages::Retired::Quit,
                    ..
                }
            ),
            "{line:?}"
        );
    }
    assert!(
        out.contains("28%25%20when%20aterm%20quit") || out.contains("28% when aterm quit"),
        "{out}"
    );
    let read = t.messages_read_at("", 9);
    assert_eq!(read.matches("state=quit").count(), 2, "{read}");
    assert_eq!(
        t.messages_quit_at(0.0, t0 + ms(3001)).lines().next(),
        Some("OK 4 4"),
        "idempotent"
    );
}

/// The lines a page instance has not handed its store yet, as the page
/// keeps them: `messages_log_since`'s lines after its head, from `cursor`.
fn kept_since(t: &mut AtermTerminal, cursor: f64) -> (f64, String) {
    let out = t.messages_log_since_at(cursor);
    let mut lines = out.lines();
    let next = lines
        .next()
        .and_then(|head| head.split(' ').nth(1))
        .and_then(|n| n.parse::<f64>().ok())
        .expect("the head's next cursor");
    (next, lines.collect::<Vec<_>>().join("\n"))
}

/// The `state=` word of each row `messages_read` answers, by id.
fn states(t: &AtermTerminal) -> Vec<(u64, String)> {
    t.messages_read_at("512", 9)
        .lines()
        .skip(1)
        .map(|row| {
            let id = row.split(' ').nth(1).and_then(|n| n.parse().ok());
            let state = row
                .split(' ')
                .find_map(|w| w.strip_prefix("state="))
                .map(str::to_string);
            (id.expect("an id"), state.expect("a state"))
        })
        .collect()
}

/// F, THE LOG ACROSS RELOADS (ruling 345): the lines one visit kept come
/// back on the next as the LOG — a record keeps its outcome, a held
/// warning and a progress row the page never closed come back `stale`,
/// never on the band, and arm no timer — and the next post's id is above
/// every kept one. The new visit's own drain hands back only its own lines.
#[test]
fn a_restored_log_is_records_and_never_the_glass() {
    let (mut a, t0) = clocked(20, 80);
    let _ = a.notice_at("post ci Build finished", t0, stamp());
    let _ = a.notice_at("post ci sev=warn hold=100 Disk nearly full", t0, stamp());
    let _ = a.notice_at("progress p pct=28 Downloading assets", t0, stamp());
    let _ = a.messages_tick_at(t0 + ms(2500));
    // The negative control: on the visit that posted them, the warning
    // and the bar ARE on the band, and arm a timer.
    assert_eq!(a.band_rows(), 2);
    assert!(a.messages_next_wake_ms_at(t0 + ms(2500)) >= 0.0);
    // The page is gone without its pagehide (a crash): no quit record.
    let (_, kept) = kept_since(&mut a, 0.0);
    let a_states = states(&a);

    let (mut b, t1) = clocked(20, 80);
    assert_eq!(
        b.messages_restore_at(&kept, t1),
        "OK records=3 skipped=0 dup=0 over=0"
    );
    assert!(b.messages_next_wake_ms_at(t1) < 0.0, "records arm nothing");
    assert_eq!(b.messages_tick_at(t1 + ms(10)) & 1, 0, "nothing to draw");
    assert_eq!(
        b.band_rows(),
        0,
        "a restored row never comes back on the glass"
    );
    assert!(b.messages_next_wake_ms_at(t1 + ms(10)) < 0.0);
    assert_eq!(b.messages_read_at("live", 9), "OK 0");
    let b_states = states(&b);
    assert_eq!(
        b_states.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        vec![1, 2, 3],
        "the records in order"
    );
    assert_eq!(
        b_states[0], a_states[0],
        "a retired record keeps its outcome"
    );
    assert_eq!(a_states[1].1, "held");
    assert_eq!(b_states[1].1, "stale");
    assert_eq!(b_states[2].1, "stale");
    assert_eq!(
        b.messages_entry_at(2.0, 9).lines().count(),
        2,
        "the Details view reads it"
    );
    // Nothing restored is handed back as new.
    assert_eq!(b.messages_log_since_at(0.0), "OK 0 0");
    // The next post is above every kept id, and it is on the band.
    let reply = b.notice_at("post ci sev=warn Fresh warning", t1 + ms(20), stamp());
    assert!(reply.starts_with("OK message=4"), "{reply}");
    assert_eq!(b.band_rows(), 1);
    let (_, own) = kept_since(&mut b, 0.0);
    let ids: Vec<Option<MessageId>> = own
        .lines()
        .map(|l| LogLine::decode(l).expect("m1").id())
        .collect();
    assert_eq!(ids, vec![MessageId::from_raw(4)], "only this visit's line");
}

/// A restore loads the kept log ONCE and before the first notice:
/// restoring twice, or restoring lines this visit drained itself, is
/// refused and changes nothing (the engine's replay alone WOULD load a
/// retired record twice — the negative control); a line the page kept
/// twice is read once; and three visits sharing one store (drain →
/// restore → drain → restore) load every record once, under distinct ids.
#[test]
fn restoring_twice_or_after_a_post_never_duplicates() {
    let (mut a, t0) = clocked(20, 80);
    let _ = a.notice_at("post ci Build finished", t0, stamp());
    let _ = a.notice_at("post ci sev=warn Disk nearly full", t0, stamp());
    let (_, kept_a) = kept_since(&mut a, 0.0);
    // The negative control: the same lines replayed twice into one log
    // load the retired record twice.
    let decoded = || kept_a.lines().map(|l| LogLine::decode(l).expect("m1"));
    let mut twice = MessageLog::empty();
    twice.replay_all(decoded().chain(decoded()));
    assert_eq!(twice.len(), 3, "the replay alone duplicates");

    let (mut b, t1) = clocked(20, 80);
    assert!(b
        .messages_restore_at(&kept_a, t1)
        .starts_with("OK records=2 "));
    let before = b.messages_read_at("512", 9);
    assert_eq!(
        b.messages_restore_at(&kept_a, t1 + ms(1)),
        "ERR restore: only once, before the first notice"
    );
    assert_eq!(
        b.messages_read_at("512", 9),
        before,
        "twice changes nothing"
    );

    // Lines this visit drained itself: refused, nothing doubled.
    let (mut c, t2) = clocked(20, 80);
    let _ = c.notice_at("post ci Deployed site", t2, stamp());
    let (cursor, own) = kept_since(&mut c, 0.0);
    assert!(c
        .messages_restore_at(&own, t2 + ms(1))
        .starts_with("ERR restore: "));
    assert_eq!(c.messages_read_at("512", 9).lines().count(), 2);
    assert_eq!(
        kept_since(&mut c, cursor).1,
        "",
        "the drain returns nothing again"
    );

    // A line kept twice is read once.
    let (mut d, t3) = clocked(20, 80);
    let doubled = format!("{kept_a}\n{kept_a}\n");
    let lines = kept_a.lines().count();
    assert_eq!(
        d.messages_restore_at(&doubled, t3),
        format!("OK records=2 skipped=0 dup={lines} over=0")
    );

    // Three visits, one store: B's new post lands above A's ids, and the
    // third visit loads A's and B's records once each.
    let _ = b.notice_at("post ci Tests passed", t1 + ms(5), stamp());
    let (_, kept_b) = kept_since(&mut b, 0.0);
    assert!(!kept_b.contains("Build%20finished") && !kept_b.contains("Build finished"));
    let store = format!("{kept_a}\n{kept_b}");
    let (mut e, t4) = clocked(20, 80);
    assert!(e
        .messages_restore_at(&store, t4)
        .starts_with("OK records=3 "));
    let ids: Vec<u64> = states(&e).into_iter().map(|(id, _)| id).collect();
    assert_eq!(ids, vec![1, 2, 3]);
    let reply = e.notice_at("post ci Shipped", t4 + ms(1), stamp());
    assert!(reply.contains("message=4"), "{reply}");
}

/// Two tabs of one site share the store and mint ids from their own
/// counters: both records load, the second under a fresh id above both —
/// the native loader's rule for two writers of one file (ruling 270).
#[test]
fn two_tabs_sharing_a_store_both_load() {
    let (mut a, t0) = clocked(20, 80);
    let (mut b, _) = clocked(20, 80);
    let _ = a.notice_at("post ci From the first tab", t0, stamp());
    let _ = b.notice_at("post ci From the second tab", t0 + ms(3), stamp());
    let store = format!(
        "{}\n{}",
        kept_since(&mut a, 0.0).1,
        kept_since(&mut b, 0.0).1
    );
    let (mut c, t1) = clocked(20, 80);
    assert!(c
        .messages_restore_at(&store, t1)
        .starts_with("OK records=2 "));
    let read = c.messages_read_at("512", 9);
    assert!(
        read.contains("first%20tab") && read.contains("second%20tab"),
        "{read}"
    );
    assert_eq!(
        states(&c).into_iter().map(|(id, _)| id).collect::<Vec<_>>(),
        vec![1, 2]
    );
    let reply = c.notice_at("post ci Third", t1 + ms(1), stamp());
    assert!(reply.contains("message=3"), "{reply}");
}

/// A restore's input is bounded and junk-proof: torn, foreign and
/// over-long lines are skipped and counted, and only the newest
/// [`RESTORE_CAP`] lines are read — the older ones counted, their records
/// not loaded — while the next id is still above every line READ.
#[test]
fn a_restore_skips_junk_and_reads_only_the_newest_lines() {
    let (mut a, t0) = clocked(20, 80);
    let (mut cursor, mut store) = (0.0, Vec::new());
    let records = RESTORE_CAP / 2 + 3;
    for i in 0..records {
        // Past the wire's 60 mints a minute: one post every 1.1 s.
        let at = t0 + ms(1100 * i as u64);
        let reply = a.notice_at(&format!("post ci Build {i} finished"), at, stamp());
        assert!(reply.starts_with("OK recorded message="), "{reply}");
        let (next, lines) = kept_since(&mut a, cursor);
        cursor = next;
        store.extend(lines.lines().map(str::to_string));
    }
    assert_eq!(store.len(), 2 * records, "a record's post and retirement");
    let good = store[store.len() - 1].clone();
    let torn = good[..good.len() - 8].to_string();
    let long = format!(
        "m1\tkind=posted\ttitle={}",
        "x".repeat(aterm_messages::log::MAX_LINE_BYTES)
    );
    let junk = ["frob".to_string(), torn, long];
    let (mut b, t1) = clocked(20, 80);
    assert_eq!(
        b.messages_restore_at(&junk.join("\n"), t1),
        "OK records=0 skipped=3 dup=0 over=0"
    );
    let (mut c, t2) = clocked(20, 80);
    let reply = c.messages_restore_at(&store.join("\n"), t2);
    assert_eq!(
        reply,
        format!("OK records={LOG_CAP} skipped=0 dup=0 over=6")
    );
    assert_eq!(c.messages_entry_at(3.0, 9), "OK 0", "an unread record");
    assert!(
        c.messages_entry_at(4.0, 9).starts_with("OK 1\n"),
        "the oldest read"
    );
    let next = c.notice_at("post ci After the restore", t2 + ms(1), stamp());
    assert!(next.contains(&format!("message={}", records + 1)), "{next}");
}

/// The Details view's data: one record's `messages` row, the same bytes
/// `messages_read` answers for it — the first id and a later one — and
/// `OK 0` for an id the ring does not hold or a page's junk number.
#[test]
fn messages_entry_is_the_records_row() {
    let (mut t, t0) = clocked(20, 80);
    for i in 0..4u64 {
        let _ = t.notice_at(
            &format!("post ci Build {i} finished -- line {i}"),
            t0 + ms(i),
            stamp(),
        );
    }
    let all = t.messages_read_at("", 7);
    let rows: Vec<&str> = all.lines().skip(1).collect();
    assert_eq!(t.messages_entry_at(1.0, 7), format!("OK 1\n{}", rows[0]));
    assert_eq!(t.messages_entry_at(3.0, 7), format!("OK 1\n{}", rows[2]));
    for junk in [0.0, -1.0, 0.5, 99.0, f64::NAN, f64::INFINITY] {
        assert_eq!(t.messages_entry_at(junk, 7), "OK 0", "{junk}");
    }
}

/// The web's percent-encoder is the control socket's, byte for byte: every
/// byte value a `str` can carry (each ASCII byte, and every two-byte scalar,
/// which walks every lead byte `C2`–`DF` over every continuation byte
/// `80`–`BF`; the three- and four-byte leads by a stride through the rest),
/// and a UTF-8 corpus.
#[test]
fn pct_encode_is_the_control_wires() {
    for b in 0u8..=0x7f {
        let s = char::from(b).to_string();
        assert_eq!(
            pct_encode(&s),
            aterm_control::wire::pct_encode(&s),
            "{b:#x}"
        );
    }
    let two_byte = (0x80u32..0x800).filter_map(char::from_u32);
    let wider = (0x800u32..=0x10_ffff)
        .step_by(61)
        .filter_map(char::from_u32);
    for c in two_byte.chain(wider) {
        let s = c.to_string();
        assert_eq!(pct_encode(&s), aterm_control::wire::pct_encode(&s), "{c:?}");
    }
    for s in [
        "",
        "%",
        "a b",
        "Details ›",
        "日本語 ✓ ⚠ …",
        "\t\n\r\u{7f}",
        "~/src/aterm",
    ] {
        assert_eq!(pct_encode(s), aterm_control::wire::pct_encode(s), "{s:?}");
    }
}

/// The band row under frame pixel row `y` is `y.checked_sub(grid_top)? /
/// cell_h.max(1)` (the expression `band_row` replaced in `cell_under`,
/// guard first so the verifier proves the subtraction): `None` above the
/// band, a zero cell height read as one, the same quotient out to
/// `usize::MAX`.
#[test]
fn band_row_is_the_pixel_rows_band_row() {
    assert_eq!(band_row(9, 10, 16), None, "above the band");
    assert_eq!(band_row(10, 10, 16), Some(0), "the band's first pixel row");
    assert_eq!(band_row(25, 10, 16), Some(0), "row 0's last pixel row");
    assert_eq!(band_row(26, 10, 16), Some(1), "row 1's first pixel row");
    assert_eq!(band_row(13, 10, 0), Some(3), "a zero height reads as one");
    assert_eq!(band_row(usize::MAX, 0, 1), Some(usize::MAX));
    let edges = [
        0,
        1,
        2,
        9,
        10,
        15,
        16,
        17,
        31,
        32,
        1000,
        usize::MAX - 1,
        usize::MAX,
    ];
    for y in edges {
        for top in edges {
            for h in [0, 1, 2, 3, 16, 17, usize::MAX] {
                assert_eq!(
                    band_row(y, top, h),
                    y.checked_sub(top).map(|d| d / h.max(1)),
                    "y={y} top={top} h={h}"
                );
            }
        }
    }
}

/// A pixel on a committed band row is that row and the geometry's column;
/// a pixel above the band, below its last row, negative or not a number
/// is `None`. At every pixel row of a two-row band and the grid under it,
/// with the real metrics and a zero cell height, `cell_under` answers what
/// its body answered before `band_row` (written out here).
#[test]
fn cell_under_is_the_band_cell_under_a_pixel() {
    let (mut t, t0) = clocked(12, 60);
    t.set_chrome(6, 4);
    let _ = t.notice_at("post ci sev=warn Disk nearly full", t0, stamp());
    let _ = t.notice_at("post ci sev=error Deploy failed", t0 + ms(40), stamp());
    assert_eq!(t.band.committed(), 2);
    let m = t.band_metrics();
    assert_eq!((m.pad, m.grid_top), (6, 10), "pad 6, head 4");
    let cell = |x: f64, y: f64| t.band.cell_under(x, y, 60, m);
    let (top, ch) = (m.grid_top as f64, m.cell_h as f64);
    let x3 = (m.pad + 3 * m.cell_w + m.cell_w / 2) as f64;
    assert_eq!(cell(x3, top - 1.0), None, "the chrome above the band");
    assert_eq!(cell(x3, top), Some((0, 3)), "the band's first pixel row");
    assert_eq!(cell(x3, top + ch - 0.5), Some((0, 3)));
    assert_eq!(cell(x3, top + ch), Some((1, 3)));
    assert_eq!(cell(x3, top + 2.0 * ch - 0.5), Some((1, 3)));
    assert_eq!(cell(x3, top + 2.0 * ch), None, "the grid under the band");
    assert_eq!(cell(0.0, top), Some((0, 0)), "the gutter is column 0");
    for (x, y) in [(-1.0, top), (x3, -1.0), (f64::NAN, top), (x3, f64::NAN)] {
        assert_eq!(cell(x, y), None, "({x}, {y})");
    }
    let before = |x: f64, y: f64, m: BandMetrics| -> Option<(usize, usize)> {
        if !(x >= 0.0 && y >= 0.0) {
            return None;
        }
        let row = (y as usize).checked_sub(m.grid_top)? / m.cell_h.max(1);
        (row < t.band.committed()).then(|| {
            (
                row,
                WebBand::geometry(60, m.cell_w, m.pad).cell_at(x as usize),
            )
        })
    };
    for metrics in [m, BandMetrics { cell_h: 0, ..m }] {
        for py in 0..m.grid_top + 3 * m.cell_h {
            for x in [0.0, x3, x3 + 0.75, 10_000.0, -0.5] {
                let y = py as f64 + 0.5;
                assert_eq!(
                    t.band.cell_under(x, y, 60, metrics),
                    before(x, y, metrics),
                    "({x}, {y}) {metrics:?}"
                );
            }
        }
    }
}

/// The tick's reply: bit 0, the band changed (a row-count change is a
/// change); bit 1, its row count did.
#[test]
fn the_tick_reply_is_two_bits() {
    assert_eq!(tick_bits(false, false), 0);
    assert_eq!(tick_bits(true, false), 1);
    assert_eq!(tick_bits(false, true), 3);
    assert_eq!(tick_bits(true, true), 3);
}

/// `getTimezoneOffset()` minutes WEST of UTC are the engine's seconds
/// EAST, rounded half away from zero; a day or more either way, or not a
/// number, is UTC. Over a sweep of the open day `utc_offset_s` is the
/// negated rounded product it replaced, value for value.
#[test]
fn a_page_offset_is_seconds_east_of_utc() {
    for (west_min, east_s) in [
        (0.0, 0),
        (-0.0, 0),
        (420.0, -25_200),
        (-120.0, 7_200),
        (-330.0, 19_800),
        (-345.0, 20_700),
        (0.125, -8),
        (-0.125, 8),
        (1439.999, -86_400),
        (-1439.999, 86_400),
    ] {
        assert_eq!(utc_offset_s(west_min), east_s, "{west_min}");
    }
    for junk in [
        1440.0,
        -1440.0,
        1e300,
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
    ] {
        assert_eq!(utc_offset_s(junk), 0, "{junk}");
    }
    let replaced = |m: f64| -((m * 60.0).round() as i64);
    for i in -(1440 * 8 - 1)..1440 * 8 {
        let m = f64::from(i) / 8.0;
        assert_eq!(utc_offset_s(m), replaced(m), "{m}");
        let odd = m + 0.013_7;
        if odd.abs() < 1440.0 {
            assert_eq!(utc_offset_s(odd), replaced(odd), "{odd}");
        }
    }
}

/// The band-less frame `a` moved down by `n` band rows, as the banded
/// frame `b` must be: every grid stream `n` rows lower, every pixel `n·ch`
/// lower, every window-space quad re-tagged onto the row its moved pixel
/// lies in (an in-grid quad on its old row + `n`). Returns the streams that
/// carried anything, so the caller can hold the check to being non-vacuous.
#[allow(
    clippy::too_many_lines,
    reason = "one assertion per RenderInput stream"
)]
fn assert_translated(
    a: &RenderInput,
    b: &RenderInput,
    n: usize,
    grid_top: usize,
    ch: usize,
) -> Vec<&'static str> {
    use aterm_core::render::{FirePatch, GlowQuad, RainHalo};
    let dy = u16::try_from(n * ch).expect("small");
    let rows = u16::try_from(n).expect("small");
    let tag = |y: u16| u16::try_from((usize::from(y)).saturating_sub(grid_top) / ch).expect("row");
    let in_grid = |y: u16| usize::from(y) >= grid_top;
    let mut lit = Vec::new();
    let mut note = |name: &'static str, any: bool| {
        if any {
            lit.push(name);
        }
    };
    assert_eq!(b.cursor_row, a.cursor_row + n);
    assert_eq!(b.cursor_col, a.cursor_col);
    assert_eq!(b.rows, a.rows + n);
    assert_eq!(b.cursor_trail.len(), a.cursor_trail.len(), "trail");
    for (x, y) in a.cursor_trail.iter().zip(&b.cursor_trail) {
        assert_eq!(
            (y.row, y.col, y.alpha),
            (x.row + n, x.col, x.alpha),
            "trail"
        );
    }
    note("trail", !a.cursor_trail.is_empty());
    let quad = |x: &GlowQuad, y: &GlowQuad, what: &str| {
        let want = GlowQuad {
            y: x.y + dy,
            row: tag(x.y + dy),
            ..*x
        };
        assert_eq!(*y, want, "{what}");
        if in_grid(x.y) {
            assert_eq!(y.row, x.row + rows, "{what}: an in-grid quad keeps its row");
        }
    };
    assert_eq!(a.cursor_glow_add.len(), b.cursor_glow_add.len(), "glow");
    for (x, y) in a.cursor_glow_add.iter().zip(&b.cursor_glow_add) {
        quad(x, y, "glow");
    }
    note("glow", !a.cursor_glow_add.is_empty());
    assert_eq!(a.glow_under.len(), b.glow_under.len(), "under");
    for (x, y) in a.glow_under.iter().zip(&b.glow_under) {
        quad(x, y, "under");
    }
    note("under", !a.glow_under.is_empty());
    assert_eq!(a.fire_patch.len(), b.fire_patch.len(), "fire");
    for (x, y) in a.fire_patch.iter().zip(&b.fire_patch) {
        let want = FirePatch {
            y: x.y + dy,
            base_y: x.base_y + dy,
            row: tag(x.y + dy),
            ..*x
        };
        assert_eq!(*y, want, "fire");
    }
    note("fire", !a.fire_patch.is_empty());
    assert_eq!(a.glow_halo.len(), b.glow_halo.len(), "halo");
    for (x, y) in a.glow_halo.iter().zip(&b.glow_halo) {
        let want = RainHalo {
            y: x.y + dy,
            cy: x.cy + dy,
            row: tag(x.y + dy),
            ..*x
        };
        assert_eq!(*y, want, "halo");
    }
    note("halo", !a.glow_halo.is_empty());
    assert_eq!(a.char_fg.len(), b.char_fg.len(), "char_fg");
    for (x, y) in a.char_fg.iter().zip(&b.char_fg) {
        let mut z = *x;
        z.row = x.row + rows;
        assert_eq!(z, *y, "char_fg");
    }
    note("char_fg", !a.char_fg.is_empty());
    assert_eq!(a.fire_halo.len(), b.fire_halo.len(), "fire_halo");
    for (x, y) in a.fire_halo.iter().zip(&b.fire_halo) {
        let mut z = *x;
        z.row = x.row + rows;
        assert_eq!(z, *y, "fire_halo");
    }
    note("fire_halo", !a.fire_halo.is_empty());
    assert_eq!(
        a.word_decorations.len(),
        b.word_decorations.len(),
        "sparkle"
    );
    for (x, y) in a.word_decorations.iter().zip(&b.word_decorations) {
        let mut z = *x;
        z.row = x.row + rows;
        assert_eq!(z, *y, "sparkle");
    }
    note("sparkle", !a.word_decorations.is_empty());
    assert_eq!(a.ink.len(), b.ink.len(), "ink");
    for (x, y) in a.ink.iter().zip(&b.ink) {
        let mut z = *x;
        z.row = x.row + rows;
        assert_eq!(z, *y, "ink");
    }
    note("ink", !a.ink.is_empty());
    for (what, xs, ys) in [
        ("cat", &a.cat_quads, &b.cat_quads),
        ("rain", &a.rain_quads, &b.rain_quads),
    ] {
        assert_eq!(xs.len(), ys.len(), "{what}");
        for (x, y) in xs.iter().zip(ys) {
            let mut z = *x;
            z.row = x.row + rows;
            z.y = x.y + dy;
            assert_eq!(z, *y, "{what}");
        }
        note(what, !xs.is_empty());
    }
    assert_eq!(a.nova_add.len(), b.nova_add.len(), "nova");
    for (x, y) in a.nova_add.iter().zip(&b.nova_add) {
        let want = GlowQuad {
            row: x.row + rows,
            y: x.y + dy,
            ..*x
        };
        assert_eq!(*y, want, "nova");
    }
    note("nova", !a.nova_add.is_empty());
    assert_eq!(a.rain_add.len(), b.rain_add.len(), "rain_add");
    for (x, y) in a.rain_add.iter().zip(&b.rain_add) {
        let want = RainHalo {
            row: x.row + rows,
            y: x.y + dy,
            cy: x.cy + dy,
            ..*x
        };
        assert_eq!(*y, want, "rain_add");
    }
    note("rain_add", !a.rain_add.is_empty());
    assert_eq!(a.free_sprites.len(), b.free_sprites.len(), "pet");
    for (x, y) in a.free_sprites.iter().zip(&b.free_sprites) {
        let mut z = *x;
        z.y = x.y + i32::from(dy);
        assert_eq!(z, *y, "pet");
    }
    note("pet", !a.free_sprites.is_empty());
    assert_eq!(a.fx_clip, b.fx_clip, "the web sets no effect clip");
    lit
}

/// EFFECTS UNDER A BAND ARE THE BAND-LESS FRAME, TRANSLATED (design ruling
/// 331): the web effects pipeline places its window-space streams from ONE
/// chrome origin that never counted the band, so the prepend moves them
/// (`HostRowPixels::Translate`). Two terminals, one with a two-row band,
/// fed the same bytes, keys and clock through ember fire, the lumen aurora
/// with the trail, sparkle words and rain, and the pet: every frame's every
/// stream on the banded one is the band-less one moved down by the band —
/// and each of glow, trail, fire, halo, sparkle, rain and the pet lit at
/// least once, so no stream passed by being empty.
#[test]
fn effects_under_a_band_are_the_bandless_frame_translated() {
    let mut plain = term(16, 60);
    let (mut banded, t0) = clocked(16, 60);
    let _ = banded.notice_at("post ci sev=warn Disk nearly full", t0, stamp());
    let _ = banded.notice_at("post ci sev=error Deploy failed", t0 + ms(40), stamp());
    assert_eq!(banded.band_rows(), 2);
    let ch = banded.cell_height();
    let mut lit = std::collections::BTreeSet::new();
    let mut frames = 0;
    let step = |plain: &mut AtermTerminal,
                banded: &mut AtermTerminal,
                lit: &mut std::collections::BTreeSet<&'static str>| {
        for t in [&mut *plain, &mut *banded] {
            t.advance_effects(33.0);
            t.render();
        }
        let grid_top = banded.renderer.grid_top();
        lit.extend(assert_translated(
            &plain.frame_scratch,
            &banded.frame_scratch,
            2,
            grid_top,
            ch,
        ));
    };
    for t in [&mut plain, &mut banded] {
        t.set_chrome(6, 4);
        t.set_effects_focused(true);
        t.set_sparkle_words_enabled(true);
        t.process(b"a curious cat watched the build fail: fuck\r\n$ ");
    }
    // Ember: fire, its halo, the flame bed under the ink, charred ink.
    for t in [&mut plain, &mut banded] {
        t.set_cursor_glow(true, "ember", None, None, 400, 64, 1.0, 2.0, true);
    }
    for ch_ in "burning".chars() {
        for t in [&mut plain, &mut banded] {
            let _ = t.note_typed_char(ch_);
            t.process(ch_.to_string().as_bytes());
        }
        step(&mut plain, &mut banded, &mut lit);
        frames += 1;
    }
    // The lumen aurora with the trail, and rain.
    for t in [&mut plain, &mut banded] {
        t.set_cursor_glow(true, "lumen", None, None, 260, 24, 0.7, 0.6, true);
        t.set_cursor_trail(true, 260, 24, None);
        // The decorative rain (no output material), which falls into the
        // empty cells while keys keep it calm.
        t.set_matrix_rain(
            30, 6, 4, 6, None, None, "matrix", None, 320, 12, false, true, true, false, 7,
        );
        t.set_matrix_rain_enabled(true);
    }
    for ch_ in " glowing words fuck".chars() {
        for t in [&mut plain, &mut banded] {
            let _ = t.note_typed_char(ch_);
            t.process(ch_.to_string().as_bytes());
        }
        step(&mut plain, &mut banded, &mut lit);
        frames += 1;
    }
    for _ in 0..120 {
        for t in [&mut plain, &mut banded] {
            t.note_keystroke();
        }
        step(&mut plain, &mut banded, &mut lit);
        frames += 1;
        if lit.contains("rain") {
            break;
        }
    }
    // The pet.
    for t in [&mut plain, &mut banded] {
        t.set_cursor_trail(false, 260, 24, None);
        t.set_cursor_glow(true, "rainbow kitty", None, None, 260, 24, 0.7, 0.6, true);
        t.set_cursor_pet(true, 7);
        let _ = t.note_typed_char('p');
        t.process(b"p");
    }
    for _ in 0..240 {
        step(&mut plain, &mut banded, &mut lit);
        frames += 1;
        if lit.contains("pet") && frames > 120 {
            break;
        }
    }
    for want in ["glow", "trail", "fire", "halo", "sparkle", "rain", "pet"] {
        assert!(
            lit.contains(want),
            "{want} never lit in {frames} frames: lit {lit:?}"
        );
    }
}

/// One golden scene: its name and its events in time order — a `notice`
/// line, or a shot (`None`), at ms from the center's birth.
type Scene = (String, Vec<(u64, Option<String>)>);

/// FNV-1a-64 — the golden's digest (the native writer's `web_fnv`).
fn fnv(bytes: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// One golden frame line from this module's composed frame — the native
/// writer's `web_golden_line`, over the page's own RGBA.
fn golden_line(key: &str, t: &AtermTerminal) -> String {
    use std::fmt::Write as _;
    let n = t.band.committed();
    let (w, grid_top, cell_h) = (t.width(), t.renderer.grid_top(), t.cell_height());
    let rgba = &t.rgba;
    let px = |y0: usize, y1: usize| fnv(&rgba[y0 * w * 4..y1 * w * 4]);
    let bleed = t.band.bleed(n);
    let mut line = format!(
        "frame\t{key}\tn={n}\tw={w}\ttop={:016x}\tbleed={:016x}",
        px(0, grid_top),
        fnv(format!("{:?}", bleed.as_ref()).as_bytes())
    );
    let input = &t.frame_scratch;
    let mut words = Vec::new();
    for r in 0..n {
        let cells = fnv(format!("{:?}", input.cells[r]).as_bytes());
        let rasters: Vec<_> = input
            .chrome_rasters
            .iter()
            .filter(|m| usize::from(m.row) == r)
            .collect();
        let raster = fnv(format!("{rasters:?}").as_bytes());
        let y0 = grid_top + r * cell_h;
        let _ = write!(
            line,
            "\tr{r}={cells:016x}:{raster:016x}:{:016x}",
            px(y0, y0 + cell_h)
        );
        words.push(
            input.cells[r]
                .iter()
                .map(|c| c.ch)
                .collect::<String>()
                .trim_end()
                .to_string(),
        );
    }
    let _ = write!(line, "\t|{}|", words.join("|"));
    line
}

/// THE WEB BAND IS THE NATIVE GOLDEN (design ruling 333): every scene of
/// `tests/fixtures/band_native.tsv` — which aterm-gui writes from the
/// native window's band — replayed through THIS module's `notice`, tick
/// and `render`, lands on every digest: each band row's cells, raster and
/// pixels, the bleed, and the pixels above the grid, in every theme, width
/// and look the golden holds.
#[test]
#[allow(clippy::too_many_lines, reason = "one replay of the golden's matrix")]
fn the_web_band_is_the_native_golden() {
    let golden = include_str!("../tests/fixtures/band_native.tsv");
    let hex = |s: &str| u32::from_str_radix(s, 16).expect("hex");
    let mut themes: Vec<(String, [u32; 8])> = Vec::new();
    let mut scenes: Vec<Scene> = Vec::new();
    let mut frames: Vec<&str> = Vec::new();
    let scene = |scenes: &mut Vec<Scene>, name: &str| {
        if scenes.last().is_none_or(|(n, _)| n != name) {
            scenes.push((name.to_string(), Vec::new()));
        }
        scenes.len() - 1
    };
    for line in golden.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        match f[0] {
            "theme" => themes.push((
                f[1].to_string(),
                [
                    hex(f[2]),
                    hex(f[3]),
                    hex(f[4]),
                    hex(f[5]),
                    hex(f[6]),
                    hex(f[7]),
                    hex(f[8]),
                    hex(f[9]),
                ],
            )),
            "line" => {
                let i = scene(&mut scenes, f[1]);
                scenes[i]
                    .1
                    .push((f[2].parse().expect("ms"), Some(f[3..].join("\t"))));
            }
            "shot" => {
                let i = scene(&mut scenes, f[1]);
                scenes[i].1.push((f[2].parse().expect("ms"), None));
            }
            "frame" => frames.push(line),
            _ => {}
        }
    }
    for (_, events) in &mut scenes {
        events.sort_by_key(|(at, l)| (*at, l.is_none()));
    }
    assert_eq!(themes.len(), 4);
    assert_eq!(scenes.len(), 16);
    let mut got = Vec::new();
    let mut px = 0usize;
    for (tname, [fg, bg, cursor, sel, blue, cyan, red, yellow]) in &themes {
        for cols in [60u16, 80, 120] {
            let mut t = AtermTerminal::new(20, cols, FONT, 20.0, *fg, *bg, *cursor, *sel)
                .expect("bundled font");
            // On wasm32 the portable rasterizer is the ONLY one; this
            // native test run would otherwise draw with the host's
            // (CoreText on macOS). The golden pins the same path.
            t.renderer.debug_force_fontdue();
            t.set_chrome(6, 0);
            for (i, c) in [(4u8, blue), (6, cyan), (1, red), (3, yellow)] {
                t.set_palette_color(i, (*c >> 16) as u8, (*c >> 8) as u8, *c as u8);
            }
            for (lname, moving) in [("moving", true), ("still", false)] {
                for (name, events) in &scenes {
                    let t0 = Instant::now();
                    t.band.reset_at(t0);
                    t.set_band_motion_at(moving, t0);
                    for (at, line) in events {
                        let now = t0 + ms(*at);
                        if let Some(line) = line {
                            let _ = t.notice_at(line, now, stamp());
                            continue;
                        }
                        let _ = t.messages_tick_at(now);
                        t.render();
                        let key = format!("{name}\t{tname}\t{cols}\t{lname}\t{at}");
                        px += t.width() * (t.renderer.grid_top() + t.band_height_px() as usize);
                        got.push(golden_line(&key, &t));
                    }
                }
            }
        }
    }
    assert_eq!(got.len(), frames.len(), "one frame per golden frame");
    let (mut cells, mut rasters, mut pixels, mut other) = (0usize, 0usize, 0usize, 0usize);
    let mut first = None;
    for (g, w) in got.iter().zip(&frames) {
        if g == w {
            continue;
        }
        first.get_or_insert_with(|| format!("web:    {g}\nnative: {w}"));
        let (gf, wf): (Vec<&str>, Vec<&str>) = (g.split('\t').collect(), w.split('\t').collect());
        for (a, b) in gf.iter().zip(&wf) {
            if a == b {
                continue;
            }
            match a.split_once('=') {
                Some((k, _)) if k.starts_with('r') => {
                    let (x, y): (Vec<&str>, Vec<&str>) =
                        (a.split(':').collect(), b.split(':').collect());
                    cells += usize::from(x.first() != y.first());
                    rasters += usize::from(x.get(1) != y.get(1));
                    pixels += usize::from(x.get(2) != y.get(2));
                }
                _ => other += 1,
            }
        }
    }
    let rows: usize = frames
        .iter()
        .map(|f| {
            f.split('\t')
                .filter(|x| x.starts_with('r') && x.contains('='))
                .count()
        })
        .sum();
    eprintln!(
        "web band parity: {} frames, {rows} band rows compared (cells, raster, pixels each), \
         {px} RGBA pixels digested, {cells} cell / {rasters} raster / {pixels} pixel row \
         differences, {other} other",
        frames.len()
    );
    assert!(
        first.is_none(),
        "the web band left the native golden: {cells} cell, {rasters} raster, {pixels} pixel \
         row differences, {other} other\n{}",
        first.unwrap_or_default()
    );
}

/// The three rows both web modules' page tests post — a record, a held
/// warning and an error, each detail a sentence and a technical line —
/// ticked a second later; and the page's lines for them read 90 s after
/// they were stamped, on a UTC clock (the CPU and the GPU module pin the
/// SAME lines: one grammar, the engine's).
fn post_page_rows(t: &mut AtermTerminal, t0: Instant) -> Vec<String> {
    for (i, line) in [
        "post ci Build finished -- 12 tests\\nall green",
        "post ci sev=warn hold=5 Disk nearly full -- 2 GB left",
        "post deploy sev=error Deploy failed -- exit 1\\nsee the job log",
    ]
    .into_iter()
    .enumerate()
    {
        let reply = t.notice_at(line, t0 + ms(i as u64), stamp());
        assert!(reply.starts_with("OK "), "{reply}");
    }
    let _ = t.messages_tick_at(t0 + ms(1000));
    let at = stamp().unix_ms;
    let wall = at + 90_000;
    let rev = t.band.center.revision();
    vec![
        format!(
            "page\trev={rev}\tnow={wall}\ttotal=3\tshown=3\tall=3\tproblems=2\tstatus=3 messages{}",
            page_chrome(3, 2, None)
        ),
        "chip\ttag=deploy\twords=Deploy\tcount=1\ton=0\tlabel=Deploy \u{00b7} 1".to_string(),
        "chip\ttag=ci\twords=CI\tcount=2\ton=0\tlabel=CI \u{00b7} 2".to_string(),
        format!(
            "entry\tid=3\tat={at}\twhen=1 min ago\tlocal=Today 2:13:20 PM\ttag=deploy\ttag_words=Deploy\tsev=error\tsev_words=Error\tmark=\u{2715}\ttitle=Deploy failed\tstate=held\tstate_words=showing now\trep=1\tdescription=Error, Deploy, 1 minute ago\tsentence=exit 1\ttechnical=see the job log\tmeta=Today 2:13:20 PM \u{00b7} showing now\tcopy=Deploy failed\\n2026-09-21 14:13:20 UTC \u{00b7} deploy \u{00b7} error\\nexit 1\\nsee the job log"
        ),
        format!(
            "entry\tid=2\tat={at}\twhen=1 min ago\tlocal=Today 2:13:20 PM\ttag=ci\ttag_words=CI\tsev=warn\tsev_words=Warning\tmark=\u{26a0}\ttitle=Disk nearly full\tstate=held\tstate_words=showing now\trep=1\tdescription=Warning, CI, 1 minute ago\tsentence=2 GB left\ttechnical=\tmeta=Today 2:13:20 PM \u{00b7} showing now\tcopy=Disk nearly full\\n2026-09-21 14:13:20 UTC \u{00b7} ci \u{00b7} warn\\n2 GB left"
        ),
        format!(
            "entry\tid=1\tat={at}\twhen=1 min ago\tlocal=Today 2:13:20 PM\ttag=ci\ttag_words=CI\tsev=info\tsev_words=Info\tmark=\u{2139}\ttitle=Build finished\tstate=recorded\tstate_words=recorded\trep=1\tdescription=Info, CI, 1 minute ago\tsentence=12 tests\ttechnical=all green\tmeta=Today 2:13:20 PM \u{00b7} recorded\tcopy=Build finished\\n2026-09-21 14:13:20 UTC \u{00b7} ci \u{00b7} info\\n12 tests\\nall green"
        ),
    ]
}

/// The `page` line's chrome keys (ruling 396), after every older key: a
/// page whose tag filter admits `all` entries, `problems` of them
/// warnings or errors, under the chip worded `chip` (`None`: every tag).
/// The words are the engine's, byte for byte, as the native page paints
/// them.
fn page_chrome(all: usize, problems: usize, chip: Option<&str>) -> String {
    let on = u8::from(chip.is_none());
    let current = chip.unwrap_or("All");
    format!(
        "\theading=Messages\tsubtitle=What aterm reported, newest first.\tall_words=All \u{00b7} {all}\tproblems_words=Problems \u{00b7} {problems}\tall_tags=All tags\tall_tags_on={on}\ttag_menu=Tag: {current}\ttag_menu_name=Filter by tag: {current}\tcopy_all_button=Copy All\tcopy_all_name=Copy the shown messages\tempty=Nothing yet.\ttechnical_caption=Technical details\tcopy_button=Copy"
    )
}

/// THE SETTINGS ▸ MESSAGES PAGE ON THE WEB (ruling 387): rows posted
/// through `notice` come back from `messages_page` as the engine's page
/// grammar with every word said — newest first, a script's tags as their
/// chips (`Deploy`, `CI`), the rows on the band `showing now` and the
/// record `recorded`, each detail's sentence and technical lines, each
/// entry's meta line and Copy text, each chip's label and the page's
/// chrome words (ruling 396) — framed `OK <n>`. The tag chip and the
/// Problems switch filter it, and the count line, the segments' words
/// and the tag pop-up say so; the page's `getTimezoneOffset()` moves the
/// local times, and junk reads as UTC.
#[test]
fn the_web_page_is_the_engines_page() {
    let (mut t, t0) = clocked(20, 80);
    let want = post_page_rows(&mut t, t0);
    let wall = stamp().unix_ms + 90_000;
    assert_eq!(
        t.messages_page_at("", false, 0, wall),
        format!("OK {}\n{}", want.len(), want.join("\n"))
    );
    let rev = t.band.center.revision();
    // A chip: `deploy` alone, its chip lit, the count line says so.
    let deploy = t.messages_page_at("deploy", false, 0, wall);
    let lines: Vec<&str> = deploy.lines().collect();
    assert_eq!(lines.len(), 5, "{deploy}");
    assert_eq!(lines[0], "OK 4");
    assert_eq!(
        lines[1],
        format!(
            "page\trev={rev}\tnow={wall}\ttotal=3\tshown=1\tall=1\tproblems=1\tstatus=1 of 3{}",
            page_chrome(1, 1, Some("Deploy"))
        )
    );
    assert_eq!(
        lines[2],
        "chip\ttag=deploy\twords=Deploy\tcount=1\ton=1\tlabel=Deploy \u{00b7} 1"
    );
    assert_eq!(
        lines[3],
        "chip\ttag=ci\twords=CI\tcount=2\ton=0\tlabel=CI \u{00b7} 2"
    );
    assert_eq!(lines[4], want[3]);
    // Problems: the warning and the error; each chip counts its own.
    let problems = t.messages_page_at("", true, 0, wall);
    let lines: Vec<&str> = problems.lines().collect();
    assert_eq!(
        lines[1],
        format!(
            "page\trev={rev}\tnow={wall}\ttotal=3\tshown=2\tall=3\tproblems=2\tstatus=2 of 3{}",
            page_chrome(3, 2, None)
        )
    );
    assert_eq!(
        lines[3],
        "chip\ttag=ci\twords=CI\tcount=1\ton=0\tlabel=CI \u{00b7} 1"
    );
    assert_eq!(lines[4..], [want[3].as_str(), want[4].as_str()]);
    // The page's clock: `getTimezoneOffset()` is minutes WEST of UTC.
    assert_eq!(utc_offset_s(-120.0), 7200);
    assert_eq!(utc_offset_s(420.0), -25_200);
    for junk in [f64::NAN, f64::INFINITY, 1440.0, -2000.0] {
        assert_eq!(utc_offset_s(junk), 0, "{junk}");
    }
    let east = t.messages_page_at("", false, utc_offset_s(-120.0), wall);
    assert!(east.contains("\tlocal=Today 4:13:20 PM\t"), "{east}");
    assert!(!east.contains("2:13:20 PM"), "{east}");
}

/// THE WEB'S HOST OFFERS NOTHING IT CANNOT DO (rulings 387, 389): a
/// restored native log's crash record offers its `Open log` NOT
/// pressable — a page has no filesystem — and its `Manual` (a footer
/// button worded `Open Manual`, ruling 396) NOT pressable either: the
/// web performs no navigation, and no press route reaches a record
/// (`notice act` on it answers `no live message`, pinned here). So
/// neither leads: both lines end `primary=0` (ruling 407). An agent's
/// upgrade record offers no word at all: no harness runs
/// under a page, so no tab's upgrade stands (ruling 315). The log itself
/// still names the word (the negative control: the page dropped it, the
/// record did not).
#[test]
fn the_web_page_offers_no_file_and_no_upgrade() {
    let (mut t, t0) = clocked(20, 80);
    let mut native = MessageCenter::new(MessageLog::default(), t0);
    for message in [
        aterm_messages::Message::new(
            aterm_messages::tags::CRASH,
            aterm_messages::Severity::Error,
            "aterm closed unexpectedly",
        )
        .line("crash log at /logs/crash-1.log")
        .action(Intent::OpenPath {
            path: "/logs/crash-1.log".into(),
        })
        .action(Intent::OpenSettings {
            route: "/manual".into(),
        })
        .hold(aterm_messages::Hold::LogOnly),
        aterm_messages::Message::new(
            aterm_messages::tags::HARNESS,
            aterm_messages::Severity::Warn,
            "Claude upgrade waits in tab 1",
        )
        .action(Intent::AgentUpgrade {
            tab: "s-1".into(),
            to: "2.1.282".into(),
            word: UpgradeWord::Now,
        })
        .hold(aterm_messages::Hold::LogOnly),
    ] {
        let _ = native.post(message, stamp(), t0);
    }
    let kept: Vec<String> = native
        .drain_shelved_for_persist()
        .into_iter()
        .map(|(line, _)| line.encode())
        .collect();
    assert_eq!(
        t.messages_restore_at(&kept.join("\n"), t0),
        "OK records=2 skipped=0 dup=0 over=0"
    );
    let page = t.messages_page_at("", false, 0, stamp().unix_ms + 60_000);
    let actions: Vec<&str> = page.lines().filter(|l| l.starts_with("action\t")).collect();
    assert_eq!(
        actions,
        [
            "action\tid=1\tindex=0\tlabel=Open log\tactionable=0\tbutton=Open log\tprimary=0",
            "action\tid=1\tindex=1\tlabel=Manual\tactionable=0\tbutton=Open Manual\tprimary=0",
        ],
        "{page}"
    );
    // Nothing pressable, nothing leads (ruling 407): the engine's rule
    // gives no Primary, and the key is the last of its line (appended).
    assert!(!page.contains("\tprimary=1"), "{page}");
    assert_eq!(
        t.notice_at("act 1 1", t0, stamp()),
        wire::no_live_reply(MessageId::from_raw(1).expect("id 1")),
        "a pressable Manual would answer this"
    );
    assert!(page.contains("\nentry\tid=2\t"), "{page}");
    assert!(
        t.messages_entry_at(2.0, stamp().unix_ms)
            .contains(" actions=Upgrade%20now"),
        "the record keeps its word"
    );
}

/// The `rev=` and `total=` fields of the page's head line.
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

/// A RESTORE MOVES THE PAGE'S `rev=` (ruling 441 (e)). Ruling 388 (5):
/// the site reads the page again after a notice or a tick, "the head's
/// `rev=` says whether the log moved", and `MessageCenter::revision` bumps
/// on every observable change. A restore that moves the log from no
/// records to two moves `rev=` too. Control: a post after the restore
/// moves it.
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
