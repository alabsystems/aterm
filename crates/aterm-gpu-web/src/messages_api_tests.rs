// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

// aterm-gpu-web's tests of the message band surface: `messages_api.rs`, which both web
// crates compile, includes this file into its tests, so these drive this
// crate's own frame path.

use super::*;
use crate::AtermGpuTerminal;

/// The bundled JetBrains Mono — the CPU twin's test face.
const FONT: &[u8] = include_bytes!("../../aterm-render/assets/bundled/JetBrainsMono-Regular.ttf");
const FG: u32 = 0x00d8_d8d8;
const BG: u32 = 0x0018_1818;
const CURSOR: u32 = 0x00a0_c0ff;
const SEL: u32 = 0x0040_4050;

fn stamp() -> WallStamp {
    WallStamp {
        unix_ms: 1_790_000_000_000,
    }
}

fn ms(n: u64) -> aterm_messages::Duration {
    aterm_messages::Duration::from_millis(n)
}

fn clocked(rows: u16, cols: u16) -> (AtermGpuTerminal, Instant) {
    let mut t =
        AtermGpuTerminal::new(rows, cols, FONT, 20.0, FG, BG, CURSOR, SEL).expect("bundled font");
    let t0 = Instant::now();
    t.band.reset_at(t0);
    (t, t0)
}

/// A warning reserves one row above the grid (the scratch grows, the grid
/// band moves below it, both faces hold the bleed), the tick reports the
/// row-count change, and when the row leaves the frame is band-less again.
/// With no band row the frame build changes nothing.
#[test]
fn the_band_composes_into_the_gpu_frame() {
    let (mut plain, _) = clocked(12, 60);
    let (mut t, t0) = clocked(12, 60);
    for x in [&mut plain, &mut t] {
        x.set_chrome(6, 0);
        x.process(b"$ ls");
    }
    let _ = t.notice_at("post ci Build finished", t0, stamp());
    let _ = t.render_headless();
    let _ = plain.render_headless();
    assert!(
        t.frame_scratch == plain.frame_scratch,
        "a record never reaches the frame"
    );
    assert_eq!(t.cpu.chrome_bleed(), None);
    let reply = t.notice_at(
        "post ci sev=warn hold=5 Disk nearly full",
        t0 + ms(1),
        stamp(),
    );
    assert!(reply.starts_with("OK message="), "{reply}");
    assert_eq!(t.band_rows(), 1);
    assert!(!t.render_headless(), "the band's frame is built");
    assert_eq!(t.frame_scratch.rows, 13, "one band row above twelve");
    assert_eq!(t.frame_scratch.grid_top_row, 1);
    assert_eq!(t.frame_scratch.grid_bot_row, 13);
    let words: String = t.frame_scratch.cells[0].iter().map(|c| c.ch).collect();
    assert!(words.contains("Disk nearly full"), "{words}");
    assert_eq!(
        t.frame_scratch.cells[1][0].ch, '$',
        "the grid, one row down"
    );
    assert!(
        t.cpu.chrome_bleed().is_some(),
        "the gutters in the band's tone"
    );
    assert!(t.render_headless(), "a settled band gates the next frame");
    assert_eq!(t.messages_next_wake_ms_at(t0 + ms(1)), 5000.0);
    // The hold ends, the shrink's quiet passes, the row count follows.
    let _ = t.messages_tick_at(t0 + ms(5001));
    let bits = t.messages_tick_at(t0 + ms(5001) + aterm_messages::SHRINK_QUIET);
    assert_eq!(bits & 2, 2, "the row count moved");
    assert_eq!(t.band_rows(), 0);
    assert!(!t.render_headless());
    let _ = plain.render_headless();
    assert!(
        t.frame_scratch == plain.frame_scratch,
        "the band-less frame again"
    );
    assert_eq!(t.cpu.chrome_bleed(), None);
    assert!(
        t.messages_next_wake_ms_at(t0 + ms(9000)) < 0.0,
        "nothing armed"
    );
}

/// THE BAND ON BOTH WEB RENDERERS (rulings 340 and 439), drawn through
/// this module itself: a held warning and a metered bar posted over
/// `notice`, committed by the tick and composed by the frame build
/// `render` runs, then drawn by the GPU renderer as `init` assembles it
/// (a native `GpuContext` in place of the canvas's, `init`'s own face,
/// `from_parts`) and by the CPU face that holds the band's bleed. The two land within the
/// grid's CPU/GPU tolerance; the frame is the band's two rows taller than
/// the band-less one, and those rows carry ink (a band that failed to
/// draw would compare equal to the grid alone there). No GPU: SKIP.
#[test]
fn the_band_frame_is_one_picture_on_both_web_renderers() {
    let ctx = match aterm_gpu::GpuContext::new() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("SKIP: no GPU: {e}");
            return;
        }
    };
    let (mut t, t0) = clocked(6, 24);
    t.set_chrome(6, 0);
    t.process(b"\x1b[31mRR\x1b[0m\r\n\x1b[44m  \x1b[0m\r\n\x1b[7mXX\x1b[0m\r\nab\r\n");
    let _ = t.render_headless();
    let plain = t.cpu.render_input(&t.frame_scratch);
    for line in [
        "post ci sev=warn hold=60 Disk nearly full -- 2 GB left",
        "progress p pct=50 Downloading assets",
    ] {
        let reply = t.notice_at(line, t0, stamp());
        assert!(reply.starts_with("OK"), "{reply}");
    }
    let _ = t.messages_tick_at(t0 + ms(2500));
    assert_eq!(t.band_rows(), 2, "the warning and the bar, past its grace");
    assert!(!t.render_headless(), "the band's frame is built");
    assert!(
        !t.frame_scratch.chrome_rasters.is_empty(),
        "the bar's meter is drawn at pixel resolution"
    );
    let face = t.gpu_face().expect("the bundled font");
    let mut gpu = match aterm_gpu::GpuRenderer::from_parts(ctx, face, None, t.theme) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("SKIP: gpu renderer unavailable: {e}");
            return;
        }
    };
    gpu.set_chrome_bleed(t.cpu.chrome_bleed());
    let cpu = t.cpu.render_input(&t.frame_scratch);
    let gpu = gpu.render_input(&mut aterm_gpu::WindowGpu::new(), &t.frame_scratch, None);
    let (_, ch) = t.cpu.cell_size();
    assert_eq!(
        (cpu.width, cpu.height),
        (plain.width, plain.height + 2 * ch),
        "the frame grows by the band's two rows"
    );
    assert_eq!((gpu.width, gpu.height), (cpu.width, cpu.height));
    // Per channel of the packed `0x00RRGGBB` the page's RGBA8 carries.
    let channel = |p: u32, s: u32| (p >> s) & 0xff;
    let delta = cpu
        .pixels
        .iter()
        .zip(&gpu.pixels)
        .flat_map(|(&a, &b)| [0, 8, 16].map(|s| channel(a, s).abs_diff(channel(b, s))))
        .max()
        .unwrap_or(0);
    assert!(
        delta <= 8,
        "the web CPU/GPU band frames diverge by {delta} > 8"
    );
    let pad = t.cpu.pad();
    let band = &gpu.pixels[pad * gpu.width..(pad + 2 * ch) * gpu.width];
    let inked = band.iter().filter(|&&p| p & 0x00FF_FFFF != BG).count();
    assert!(
        inked > band.len() / 20,
        "the GPU drew the band: {inked} inked pixels"
    );
}

/// The GPU module's copy carries the round's exports: a hidden page arms
/// nothing, the forced colours are the engine's forced inks, and the
/// log drains as codec lines — the CPU twin's tests, abridged, on this
/// module's own frame.
#[test]
fn the_gpu_band_has_the_twins_exports() {
    let (mut t, t0) = clocked(12, 60);
    let _ = t.notice_at("post ci sev=warn hold=5 Disk nearly full", t0, stamp());
    assert_eq!(t.messages_next_wake_ms_at(t0), 5000.0);
    t.set_band_visible_at(false, t0);
    assert_eq!(t.messages_next_wake_ms_at(t0), -1.0);
    t.set_band_visible_at(true, t0);
    // Five distinct colours: a swap between any two slots fails.
    let hc = ForcedPalette {
        window: [0x00, 0x00, 0x10],
        window_text: [0xff, 0xff, 0xff],
        highlight: [0x1a, 0xeb, 0xff],
        highlight_text: [0x10, 0x00, 0x00],
        btn_face: [0x00, 0x10, 0x00],
    };
    t.set_forced_colors_at(
        true,
        0x0000_0010,
        0x00ff_ffff,
        0x001a_ebff,
        0x0010_0000,
        0x0000_1000,
        t0,
    );
    assert_eq!(t.band.inks, BandInks::forced(hc));
    let _ = t.render_headless();
    assert!(t.frame_scratch.cells[0].iter().all(|c| c.bg == hc.btn_face));
    let log = t.messages_log_since_at(0.0);
    assert!(log.starts_with("OK 1 1\nm1\tkind=posted"), "{log}");
    t.set_details_view_at(true, t0);
    let _ = t.render_headless();
    let words: String = t.frame_scratch.cells[0].iter().map(|c| c.ch).collect();
    assert!(words.contains("Details"), "{words}");
}

/// The pet's drawn body in frame px `(x0, x1, y0, y1)`: the free sprite
/// whose atlas tile is the body's natural size (`ART_ROWS` cells tall at
/// `ART_ASPECT`), placed `pad` in and `grid_top` down (band rows are
/// already in its `y`).
fn drawn_pet_body(frame: &RenderInput, m: BandMetrics) -> Option<(f32, f32, f32, f32)> {
    use aterm_effects::kitty_pet::{ART_ASPECT, ART_ROWS};
    let nat_h = (ART_ROWS * m.cell_h as f32).round();
    let nat = ((nat_h * ART_ASPECT).round() as u16, nat_h as u16);
    let s = frame.free_sprites.iter().find(|s| (s.aw, s.ah) == nat)?;
    let x0 = m.pad as f32 + s.x as f32;
    let y0 = m.grid_top as f32 + s.y as f32;
    Some((x0, x0 + f32::from(s.w), y0, y0 + f32::from(s.h)))
}

/// With a band row committed, a press at the drawn cat's centre or feet
/// pets it, and a press half a row above its body reaches the terminal.
/// The same holds without the band.
#[test]
fn the_pet_is_pressed_where_the_band_draws_it() {
    let (mut t, t0) = clocked(12, 60);
    t.set_chrome(6, 0);
    // The caret three rows down, so a text row lies above the cat.
    t.process(b"\r\n\r\n\r\n$ ");
    t.set_cursor_glow(true, "rainbow kitty", None, None, 260, 24, 0.7, 0.6, true);
    t.set_cursor_pet(true, 7);
    let _ = t.render_headless();
    for frame in 0..240u32 {
        if frame % 6 == 0 {
            let ch = char::from(b'a' + u8::try_from(frame / 6 % 26).unwrap_or(0));
            t.note_typed_char(ch);
            t.process(&[ch as u8]);
        }
        t.advance_effects(33.0);
        let _ = t.render_headless();
        if t.cursor_pet_alpha() > 0 {
            break;
        }
    }
    assert!(t.cursor_pet_alpha() > 0, "the pet must reach the glass");
    let m = t.band_metrics();
    let half_row = m.cell_h as f32 / 2.0;
    for band_rows in [0, 1] {
        if band_rows == 1 {
            let reply = t.notice_at("post ci sev=warn Disk nearly full", t0, stamp());
            assert!(reply.starts_with("OK message="), "{reply}");
            assert!(!t.render_headless(), "the band's frame is built");
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

/// A held warning left open on the last visit restores as a `stale` record
/// (no band row, no swapchain growth, no timer); the next post takes the
/// next id and is on the band; a second restore is refused.
#[test]
fn the_gpu_band_restores_the_kept_log() {
    let (mut a, t0) = clocked(12, 60);
    let _ = a.notice_at("post ci sev=warn hold=100 Disk nearly full", t0, stamp());
    let _ = a.messages_tick_at(t0 + ms(10));
    assert_eq!(a.band_rows(), 1);
    let kept: Vec<String> = a
        .messages_log_since_at(0.0)
        .lines()
        .skip(1)
        .map(str::to_string)
        .collect();
    let (mut t, t1) = clocked(12, 60);
    let rows = t.surface_rows();
    assert_eq!(
        t.messages_restore_at(&kept.join("\n"), t1),
        "OK records=1 skipped=0 dup=0 over=0"
    );
    let _ = t.messages_tick_at(t1 + ms(10));
    assert_eq!(t.band_rows(), 0, "a restored row is a record");
    assert_eq!(t.surface_rows(), rows);
    assert_eq!(t.messages_next_wake_ms_at(t1 + ms(10)), -1.0);
    assert!(t.messages_read_at("8", 9).contains(" state=stale "));
    assert!(t
        .messages_restore_at(&kept.join("\n"), t1)
        .starts_with("ERR restore: "));
    let reply = t.notice_at("post ci sev=warn Fresh warning", t1 + ms(20), stamp());
    assert!(reply.starts_with("OK message=2"), "{reply}");
    assert_eq!(t.band_rows(), 1);
}

/// Post a record, a held warning and an error (each detail a sentence and
/// a technical line), tick a second later, and read the page's lines 90 s
/// after posting on a UTC clock. Both web modules pin the same lines.
fn post_page_rows(t: &mut AtermGpuTerminal, t0: Instant) -> Vec<String> {
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

/// The `page` line's chrome keys for a filter admitting `all` entries,
/// `problems` of them warnings or errors, under chip `chip` (`None`: every
/// tag), worded as the native page paints them.
fn page_chrome(all: usize, problems: usize, chip: Option<&str>) -> String {
    let on = u8::from(chip.is_none());
    let current = chip.unwrap_or("All");
    format!(
        "\theading=Messages\tsubtitle=What aterm reported, newest first.\tall_words=All \u{00b7} {all}\tproblems_words=Problems \u{00b7} {problems}\tall_tags=All tags\tall_tags_on={on}\ttag_menu=Tag: {current}\ttag_menu_name=Filter by tag: {current}\tcopy_all_button=Copy All\tcopy_all_name=Copy the shown messages\tempty=Nothing yet.\ttechnical_caption=Technical details\tcopy_button=Copy"
    )
}

/// Rows posted via `notice` read back from `messages_page` as the same
/// lines the CPU module pins, framed `OK <n>`; the Problems switch leaves
/// the record out, and a selected chip is named by the chrome.
#[test]
fn the_gpu_page_is_the_twins_page() {
    let (mut t, t0) = clocked(12, 60);
    let want = post_page_rows(&mut t, t0);
    let wall = stamp().unix_ms + 90_000;
    assert_eq!(
        t.messages_page_at("", false, 0, wall),
        format!("OK {}\n{}", want.len(), want.join("\n"))
    );
    let problems = t.messages_page_at("", true, 0, wall);
    assert!(problems.starts_with("OK 5\n"), "{problems}");
    assert!(problems.contains(&format!(
        "\tshown=2\tall=3\tproblems=2\tstatus=2 of 3{}",
        page_chrome(3, 2, None)
    )));
    assert!(problems.contains("\nchip\ttag=ci\twords=CI\tcount=1\ton=0\tlabel=CI \u{00b7} 1\n"));
    assert!(!problems.contains("\ttitle=Build finished\t"), "{problems}");
    // A chip down: the chrome names it, `All tags` is up.
    let deploy = t.messages_page_at("deploy", false, 0, wall);
    assert!(
        deploy.contains(&format!(
            "\tstatus=1 of 3{}\n",
            page_chrome(1, 1, Some("Deploy"))
        )),
        "{deploy}"
    );
}

/// A restored native crash record's `Open log` and `Manual` are listed but
/// not pressable (`button=` wording, `primary=0`) and no press route
/// reaches the record; an agent upgrade record offers no action.
#[test]
fn the_gpu_page_offers_what_the_twin_offers() {
    let (mut t, t0) = clocked(12, 60);
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
    // Nothing pressable, nothing leads: the engine's rule
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

/// Frame pixel `(x, y)` at the middle of band cell `(row, col)`.
fn band_px(t: &AtermGpuTerminal, row: usize, col: usize) -> (f64, f64) {
    let m = t.band_metrics();
    (
        (m.pad + col * m.cell_w + m.cell_w / 2) as f64,
        (m.grid_top + row * m.cell_h + m.cell_h / 2) as f64,
    )
}

/// A STILL POINTER'S LIT CHIP FOLLOWS THE LAYOUT (ruling 441 (f)), the
/// GPU module's copy: a progress row the pointer rests on ends `done …
/// ok`, and the frame its Complete echo builds is the frame a twin that
/// took the same lines with no pointer builds.
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
        let _ = b.render_headless();
    }
    assert!(
        t.frame_scratch != twin.frame_scratch,
        "control: the lit chip shows"
    );
    for b in [&mut t, &mut twin] {
        let reply = b.notice_at("done p ok Downloaded assets", t0 + ms(2102), stamp());
        assert!(reply.starts_with("OK"), "{reply}");
    }
    let at = t0 + ms(2200);
    for b in [&mut t, &mut twin] {
        let _ = b.messages_tick_at(at);
        let _ = b.render_headless();
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
    assert!(
        t.frame_scratch == twin.frame_scratch,
        "the echo draws a lit chip the still pointer left"
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

/// A RESTORE MOVES THE PAGE'S `rev=` (ruling 441 (e)), the GPU module's
/// copy: the site gates its re-read on the head's `rev=` (ruling 388 (5)),
/// so a restore that moves the log from no records to two moves it.
/// Control: a post after the restore moves it.
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
