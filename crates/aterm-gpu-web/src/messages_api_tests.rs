// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

use super::*;
use crate::AtermGpuTerminal;

const FONT: &[u8] = include_bytes!("../../aterm-render/assets/bundled/JetBrainsMono-Regular.ttf");
const FG: u32 = 0x00d8_d8d8;
const BG: u32 = 0x0018_1818;
const CURSOR: u32 = 0x00a0_c0ff;
const SEL: u32 = 0x0040_4050;

type Host = AtermGpuTerminal;

fn draw(t: &mut Host) {
    let _ = t.render_headless().expect("renderable frame");
}

fn same_picture(a: &Host, b: &Host) -> bool {
    a.frame_scratch == b.frame_scratch
}

fn clocked(rows: u16, cols: u16) -> (AtermGpuTerminal, Instant) {
    let mut t =
        AtermGpuTerminal::new(rows, cols, FONT, 20.0, FG, BG, CURSOR, SEL).expect("bundled font");
    let t0 = Instant::now();
    t.band.reset_at(t0);
    (t, t0)
}

#[test]
fn the_band_composes_into_the_gpu_frame() {
    let (mut plain, _) = clocked(12, 60);
    let (mut t, t0) = clocked(12, 60);
    for x in [&mut plain, &mut t] {
        x.set_chrome(6, 0).expect("admitted geometry");
        x.process(b"$ ls");
    }
    let _ = t.notice_at("post ci Build finished", t0, stamp());
    let _ = t.render_headless().expect("renderable frame");
    let _ = plain.render_headless().expect("renderable frame");
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
    assert!(
        !t.render_headless().expect("renderable frame"),
        "the band's frame is built"
    );
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
    assert!(
        t.render_headless().expect("renderable frame"),
        "a settled band gates the next frame"
    );
    assert_eq!(t.messages_next_wake_ms_at(t0 + ms(1)), 5000.0);
    let _ = t.messages_tick_at(t0 + ms(5001));
    let bits = t.messages_tick_at(t0 + ms(5001) + aterm_messages::SHRINK_QUIET);
    assert_eq!(bits & 2, 2, "the row count moved");
    assert_eq!(t.band_rows(), 0);
    assert!(!t.render_headless().expect("renderable frame"));
    let _ = plain.render_headless().expect("renderable frame");
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

#[test]
fn the_band_frame_is_one_picture_on_both_web_renderers() {
    let ctx = match aterm_gpu::GpuContext::new() {
        Ok(c) => c,
        Err(e) => aterm_spec::host::could_not_run(format!("no GPU: {e}")),
    };
    let (mut t, t0) = clocked(6, 24);
    t.set_chrome(6, 0).expect("admitted geometry");
    t.process(b"\x1b[31mRR\x1b[0m\r\n\x1b[44m  \x1b[0m\r\n\x1b[7mXX\x1b[0m\r\nab\r\n");
    let _ = t.render_headless().expect("renderable frame");
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
    assert!(
        !t.render_headless().expect("renderable frame"),
        "the band's frame is built"
    );
    assert!(
        !t.frame_scratch.chrome_rasters.is_empty(),
        "the bar's meter is drawn at pixel resolution"
    );
    let face = t.gpu_face().expect("the bundled font");
    let mut gpu = match aterm_gpu::GpuRenderer::from_parts(ctx, face, None, t.theme) {
        Ok(g) => g,
        Err(e) => aterm_spec::host::could_not_run(format!("gpu renderer unavailable: {e}")),
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

#[test]
fn the_gpu_band_has_the_twins_exports() {
    let (mut t, t0) = clocked(12, 60);
    let _ = t.notice_at("post ci sev=warn hold=5 Disk nearly full", t0, stamp());
    assert_eq!(t.messages_next_wake_ms_at(t0), 5000.0);
    t.set_band_visible_at(false, t0);
    assert_eq!(t.messages_next_wake_ms_at(t0), -1.0);
    t.set_band_visible_at(true, t0);
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
    let _ = t.render_headless().expect("renderable frame");
    assert!(t.frame_scratch.cells[0].iter().all(|c| c.bg == hc.btn_face));
    let log = t.messages_log_since_at(0.0);
    assert!(log.starts_with("OK 1 1\nm1\tkind=posted"), "{log}");
    t.set_details_view_at(true, t0);
    let _ = t.render_headless().expect("renderable frame");
    let words: String = t.frame_scratch.cells[0].iter().map(|c| c.ch).collect();
    assert!(words.contains("Details"), "{words}");
}

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
    let size = t.checked_surface_size().expect("the prior physical frame");
    assert_eq!(
        t.messages_restore_at(&kept.join("\n"), t1),
        "OK records=1 skipped=0 dup=0 over=0"
    );
    let _ = t.messages_tick_at(t1 + ms(10));
    assert_eq!(t.band_rows(), 0, "a restored row is a record");
    assert_eq!(
        t.checked_surface_size()
            .expect("the restored physical frame"),
        size
    );
    assert_eq!(t.messages_next_wake_ms_at(t1 + ms(10)), -1.0);
    assert!(t.messages_read_at("8", 9).contains(" state=stale "));
    assert!(t
        .messages_restore_at(&kept.join("\n"), t1)
        .starts_with("ERR restore: "));
    let reply = t.notice_at("post ci sev=warn Fresh warning", t1 + ms(20), stamp());
    assert!(reply.starts_with("OK message=2"), "{reply}");
    assert_eq!(t.band_rows(), 1);
}

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

fn page_chrome(all: usize, problems: usize, chip: Option<&str>) -> String {
    let on = u8::from(chip.is_none());
    let current = chip.unwrap_or("All");
    format!(
        "\theading=Messages\tsubtitle=What aterm reported, newest first.\tall_words=All \u{00b7} {all}\tproblems_words=Problems \u{00b7} {problems}\tall_tags=All tags\tall_tags_on={on}\ttag_menu=Tag: {current}\ttag_menu_name=Filter by tag: {current}\tcopy_all_button=Copy All\tcopy_all_name=Copy All shown messages\tempty=Nothing yet.\ttechnical_caption=Technical details\tcopy_button=Copy"
    )
}

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
    let deploy = t.messages_page_at("deploy", false, 0, wall);
    assert!(
        deploy.contains(&format!(
            "\tstatus=1 of 3{}\n",
            page_chrome(1, 1, Some("Deploy"))
        )),
        "{deploy}"
    );
}

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
