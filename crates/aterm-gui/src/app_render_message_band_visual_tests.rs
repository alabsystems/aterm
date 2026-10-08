// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::time::{Duration, Instant};

use aterm_messages::{
    ActionIndex, COMET_PERIOD, LOAD_AFTER, Look, MessageId, Outcome, PROGRESS_GRACE, Pace,
    SHRINK_QUIET,
};

use crate::message_band::{BandHover, HoverTarget};
use crate::toolchain_words;
use crate::update_words::{self, ApplyPosture};
use crate::{App, WindowId, term_lock};

/// High Contrast's look: palette inks, hard edges, moving.
const FLAT: Look = Look {
    pace: Pace::Moving,
    graded: false,
};

/// A rendered frame: its pixels (`0xRRGGBB`) and size.
struct Frame {
    width: usize,
    height: usize,
    pixels: Vec<u32>,
    /// The first pixel row of the band and one cell's height.
    band_y: usize,
    cell_h: usize,
}

/// The capture font's size in px, used by the CPU renderer and by a
/// [`glass_window`]'s cell layout so the band's pixel mapping agrees.
const CAPTURE_PX: f32 = 20.0;

/// A capture window that is not a whole number of cells wide: `pad` px of
/// gutter on every side and `extra` px (under one cell) of remainder band
/// split around it. Band cells sit at `pad + extra/2` in a window
/// `cols·cw + 2·pad + extra` px wide, so 0 % is an empty track, 50 % the
/// window's middle and 100 % edge to edge.
fn glass_window(app: &mut App, session: u64, cols: u16, pad: usize, extra: usize) -> WindowId {
    let wid = app.insert_logical_window(crate::stub_session(session), 20, cols);
    let ws = app.windows.get_mut(&wid).unwrap();
    ws.metrics.font_px = CAPTURE_PX;
    ws.metrics.pad = pad;
    ws.metrics.pad_top = pad;
    let (cw, ch) = app.win_cell_size(wid);
    // Less than one cell, or the presenters would count another column.
    let extra = extra.min(cw.saturating_sub(1));
    let width = usize::from(cols) * cw + 2 * pad + extra;
    let height = 21 * ch + 2 * pad;
    app.windows.get_mut(&wid).unwrap().win_px = Some(winit::dpi::PhysicalSize::new(
        u32::try_from(width).unwrap_or(u32::MAX),
        u32::try_from(height).unwrap_or(u32::MAX),
    ));
    wid
}

/// Fill the scratch from the engine, prepare the band's motion at `at` in
/// `look`, splice the band as a redraw does, and render through the
/// window's gutters and remainder bands ([`glass_window`]): the chrome bleed
/// (`band_row_edges`) and the presenters' band placement
/// (`place_frame_bands`). `None` when there is no system font.
fn render(
    app: &mut App,
    wid: WindowId,
    at: Instant,
    look: Look,
    theme: aterm_render::Theme,
) -> Option<Frame> {
    let (rows, cols, pad, win_w) = {
        let ws = &app.windows[&wid];
        (
            ws.rows as usize,
            ws.cols as usize,
            ws.metrics.pad,
            ws.win_px.map(|s| s.width as usize),
        )
    };
    // The meter borrows the scheme's blue or cyan where its cursor is
    // near-grey: shoot a builtin ground with its own palette.
    if let Some(s) = aterm_types::scheme::builtin_names()
        .into_iter()
        .filter_map(aterm_types::scheme::builtin)
        .find(|s| {
            let t = s.to_theme_parts();
            (t.fg, t.bg, t.cursor) == (theme.fg, theme.bg, theme.cursor)
        })
    {
        let mut tc = app
            .session_factory
            .terminal_config
            .clone()
            .unwrap_or_default();
        tc.custom_palette = Some(s.to_color_palette());
        app.session_factory.terminal_config = Some(tc);
    }
    let terminal = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    {
        let ws = app.windows.get_mut(&wid).unwrap();
        let mut term = term_lock(&terminal);
        term.cell_frame_into(&mut ws.input_scratch, rows, cols);
        // A capture repaints every frame it shoots.
        ws.band.invalidate();
    }
    app.settle_messages(at);
    app.prepare_band_motion_with(wid, at, look);
    app.splice_message_band(wid, theme);
    let mut cpu = aterm_render::Renderer::from_system(CAPTURE_PX, theme)?;
    let Some(win_w) = win_w else {
        // A window with no glass lays out at the App's font, so repaint the
        // band's pixel raster for this renderer.
        reraster_for(app, wid, &cpu, theme);
        let frame = cpu.render_input(&app.windows[&wid].input_scratch);
        return Some(Frame {
            width: frame.width,
            height: frame.height,
            pixels: frame.pixels.clone(),
            band_y: cpu.grid_top(),
            cell_h: cpu.cell_size().1,
        });
    };
    assert_eq!(
        app.win_cell_size(wid),
        cpu.cell_size(),
        "a glass window lays its cells out at the capture font's size"
    );
    cpu.set_pad(pad);
    let pack = |c: [u8; 3]| (u32::from(c[0]) << 16) | (u32::from(c[1]) << 8) | u32::from(c[2]);
    let band = crate::chrome_band::band_colors(theme);
    let committed = usize::from(app.message_band_rows);
    cpu.set_chrome_bleed((committed > 0).then(|| aterm_render::ChromeBleed {
        rows: committed,
        first: 0,
        color: pack(band.bar_bg),
        seam: Some(pack(band.label)),
        top_extends_cells: false,
        row_edges: super::band_row_edges(0, committed, &app.windows[&wid].cached_band_edges),
    }));
    let frame = cpu.render_input(&app.windows[&wid].input_scratch);
    let mut pixels = vec![0u32; win_w * frame.height];
    aterm_render::place_frame_bands(
        &mut pixels,
        win_w,
        frame.height,
        &frame.pixels,
        frame.width,
        frame.height,
        false,
        theme.bg,
        cpu.chrome_extent_px(frame.height),
    );
    Some(Frame {
        width: win_w,
        height: frame.height,
        pixels,
        band_y: cpu.grid_top(),
        cell_h: cpu.cell_size().1,
    })
}

/// Repaint window `wid`'s band rasters on the frame `cpu` draws (its
/// cell width, its pad, no remainder band), keeping each one's row.
fn reraster_for(
    app: &mut App,
    wid: WindowId,
    cpu: &aterm_render::Renderer,
    theme: aterm_render::Theme,
) {
    let (cw, ch) = cpu.cell_size();
    let pad = cpu.pad();
    let Some(ws) = app.windows.get(&wid) else {
        return;
    };
    let (Some(motion), Some(p)) = (ws.band.motion(), ws.band.layout()) else {
        return;
    };
    let cols = p.cols;
    let geom = crate::message_band::BandGeometry {
        win_w: cols * cw + 2 * pad,
        cells_x: pad,
        cell_w: cw,
    };
    let (_, _, rasters) =
        crate::message_band::paint_rows_on(p, app.band_palette(theme), ws.band_hover, geom, motion);
    let rows: Vec<u16> = ws
        .input_scratch
        .chrome_rasters
        .iter()
        .map(|m| m.row)
        .collect();
    let mut fresh: Vec<aterm_render::ChromeRaster> = rasters
        .iter()
        .flatten()
        .zip(rows)
        .map(|(r, row)| crate::message_band::OnFrame::on_frame(r, row, 0, geom.win_w, ch))
        .collect();
    // The seam each ring took over from the composed stack
    // (`message_band::floor_rings`) rides along.
    for (new, old) in fresh.iter_mut().zip(&ws.input_scratch.chrome_rasters) {
        for (a, b) in new.rings.iter_mut().zip(&old.rings) {
            a.seam = b.seam;
        }
    }
    if let Some(ws) = app.windows.get_mut(&wid) {
        ws.input_scratch.chrome_rasters = fresh;
    }
}

/// Write `pixels` (`width` wide) as `path`.
fn write_png(path: &std::path::Path, width: usize, height: usize, pixels: &[u32]) {
    let mut rgb = Vec::with_capacity(pixels.len() * 3);
    for &p in pixels {
        rgb.push((p >> 16) as u8);
        rgb.push((p >> 8) as u8);
        rgb.push(p as u8);
    }
    let file = std::fs::File::create(path).expect("create png");
    let mut encoder =
        aterm_png::Encoder::new(std::io::BufWriter::new(file), width as u32, height as u32);
    encoder.set_color(aterm_png::ColorType::Rgb);
    encoder.set_depth(aterm_png::BitDepth::Eight);
    encoder
        .write_header()
        .expect("png header")
        .write_image_data(&rgb)
        .expect("png data");
}

/// Capture window `wid` at `at` in `look` and `theme` as `name.png`, and
/// print the chrome rows' text for byte comparison. `false` when there is
/// no system font.
#[allow(
    clippy::too_many_arguments,
    reason = "a capture is a pure function of exactly these inputs, spelled at the call"
)]
fn capture_in(
    app: &mut App,
    wid: WindowId,
    dir: &std::path::Path,
    name: &str,
    at: Instant,
    look: Look,
    theme: aterm_render::Theme,
) -> bool {
    let Some(frame) = render(app, wid, at, look, theme) else {
        return false;
    };
    let path = dir.join(format!("{name}.png"));
    write_png(&path, frame.width, frame.height, &frame.pixels);
    let ws = &app.windows[&wid];
    for r in 0..usize::from(app.message_band_rows) {
        let text: String = ws.input_scratch.cells[r].iter().map(|c| c.ch).collect();
        crate::logging::stderr_line!("{name} row{r} |{text}|");
    }
    crate::logging::stderr_line!(
        "wrote {} ({}x{})",
        path.display(),
        frame.width,
        frame.height
    );
    true
}

/// [`capture_in`] on the default theme.
fn capture(
    app: &mut App,
    wid: WindowId,
    dir: &std::path::Path,
    name: &str,
    at: Instant,
    look: Look,
) -> bool {
    capture_in(
        app,
        wid,
        dir,
        name,
        at,
        look,
        aterm_render::Theme::default(),
    )
}

/// Extra grounds every row is shot on beside the default dark one: a dark
/// theme with a cool accent and two light themes.
const ROW_GROUNDS: [&str; 3] = ["Tokyo Night", "GitHub Light", "Solarized Light"];

/// The widths a row is shot at on the other grounds: 60, 80 and 120 whole
/// columns, and a window that is not a whole number of cells wide.
const GROUND_WIDTHS: [&str; 4] = ["60", "80", "120", "g80"];

/// [`capture_in`] window `wid` (labelled `cols`) on each of `grounds` as
/// `<name>-<ground>-<cols>`, when `cols` is one of [`GROUND_WIDTHS`].
#[allow(
    clippy::too_many_arguments,
    reason = "a capture is a pure function of exactly these inputs, spelled at the call"
)]
fn capture_grounds(
    app: &mut App,
    wid: WindowId,
    cols: &str,
    dir: &std::path::Path,
    name: &str,
    at: Instant,
    look: Look,
    grounds: &[&str],
) {
    if !GROUND_WIDTHS.contains(&cols) {
        return;
    }
    for ground in grounds {
        let slug = ground.to_lowercase().replace(' ', "-");
        capture_in(
            app,
            wid,
            dir,
            &format!("{name}-{slug}-{cols}"),
            at,
            look,
            scheme(ground),
        );
    }
}

/// The instant a scene is shot: past the progress grace (so a graced row is
/// on the glass) and 2 s after the newest on-glass row's motion epoch.
fn shot(app: &mut App) -> Instant {
    let revealed = Instant::now() + PROGRESS_GRACE;
    app.settle_messages(revealed);
    app.messages
        .on_glass()
        .filter_map(|l| l.motion_since)
        .max()
        .map_or(revealed, |epoch| {
            (epoch + Duration::from_secs(2)).max(revealed)
        })
}

/// A builtin scheme as the renderer's theme.
fn scheme(name: &str) -> aterm_render::Theme {
    let parts = aterm_types::scheme::builtin(name)
        .expect("a builtin scheme")
        .to_theme_parts();
    aterm_render::Theme {
        fg: parts.fg,
        bg: parts.bg,
        cursor: parts.cursor,
        selection: parts.selection,
    }
}

fn content(app: &App, wid: WindowId) {
    let lines = [
        "$ claude --version",
        "2.1.267 (Claude Code)",
        "$ codex --version",
        "codex-cli 0.154.0",
        "$ ",
    ];
    let term = app.front_terminal(wid).expect("front").term.clone();
    let mut bytes = Vec::new();
    for line in lines {
        bytes.extend_from_slice(line.as_bytes());
        bytes.extend_from_slice(b"\r\n");
    }
    term_lock(&term).process(&bytes);
}

const INSTALLED: &str =
    "\u{2713} ALab toolchain installed: claude, codex \u{2014} ready in every aterm tab";

/// The updater's health sentence.
const HEALTH_BODY: &str = "20 failed checks in a row since 2026-08-27T22:04:36Z: release \
     manifests exist but cannot be downloaded.";

/// The first run's announcement, as the lane's tagged wake opens it.
const FIRST_RUN: &str =
    "installing 10 ALab program(s) over the network (about 3 GB on disk when finished)";

/// The first run's mid-pass read, as the tailer posts it: `trust`
/// extracting (the disk is the load).
fn first_run_read() -> crate::PkgProgressSnapshot {
    first_run_read_at(120_000_000)
}

/// [`first_run_read`] with `trust` extracted `extracted` bytes into its
/// 900 MB.
fn first_run_read_at(extracted: u64) -> crate::PkgProgressSnapshot {
    let mut programs = std::collections::BTreeMap::new();
    programs.insert(
        "trust".to_string(),
        atpkg::progress::ProgramProgress {
            phase: atpkg::progress::Phase::Extract,
            bytes_done: extracted,
            bytes_total: 900_000_000,
            build: Some(5520),
            bumped: false,
            error: None,
        },
    );
    crate::PkgProgressSnapshot {
        file: atpkg::progress::ProgressFile {
            v: atpkg::progress::PROGRESS_VERSION,
            pid: Some(7),
            pass: "net".to_string(),
            started_unix: 1_700_000_000,
            heartbeat_unix: 1_700_000_000,
            overall: atpkg::progress::Overall {
                programs_done: 3,
                programs_total: 10,
                bytes_done: 512_000_000,
                bytes_total: 1_200_000_000,
            },
            queue: vec!["ty".to_string(), "ay".to_string()],
            programs,
            ended_unix: None,
        },
        running: true,
    }
}

/// Record the pass's answers (the roster, the R6 markers); they are records
/// and must leave the glass unchanged.
fn record_the_answers(app: &mut App) {
    app.record_toolchain_outcome(toolchain_words::installed(INSTALLED));
    app.post_machine_settings("spotlight-noindex 73 dir(s) migrated; universal-control disabled");
    app.post_managed_current("claude 2.1.280 (Anthropic latest); codex 0.156.0 (OpenAI latest)");
}

/// The update lane's download as the poller reports it, fed into the center
/// at a controlled arrival instant: the first read posts, later ones restate
/// in place, and the ETA estimator sees the true time base.
struct Download {
    total: u64,
    id: Option<MessageId>,
}

impl Download {
    fn read(&mut self, app: &mut App, done: u64, t: Instant) {
        let msg = update_words::progress(
            &aterm_update::Progress::Downloading {
                version: "0.91.0".into(),
                bytes_done: done,
                bytes_total: self.total,
            },
            None,
            "",
        );
        match self.id {
            Some(id) => {
                app.messages
                    .restate(id, crate::messages_host::restatement_of(&msg), t);
            }
            None => {
                self.id = Some(
                    app.messages
                        .post(msg, crate::messages_host::wall_stamp_now(), t)
                        .id,
                );
            }
        }
    }

    /// Reads every 100 ms at `rate` bytes/s from `from` (bytes `done0`)
    /// until `permille` of the total: the instant of the last read.
    fn run_to(
        &mut self,
        app: &mut App,
        from: Instant,
        done0: u64,
        rate: u64,
        permille: u64,
    ) -> (Instant, u64) {
        let target = self.total * permille / 1000;
        let (mut t, mut done) = (from, done0);
        loop {
            self.read(app, done.min(self.total), t);
            if done >= target {
                return (t, done);
            }
            t += Duration::from_millis(100);
            done += rate / 10;
        }
    }
}

#[test]
#[ignore = "visual capture: needs a system font; run with --ignored"]
#[allow(
    clippy::too_many_lines,
    reason = "one gate, one list of scenes, read top to bottom"
)]
fn message_band_visual_capture() {
    let dir = std::env::var("STATUS_BARS_PNG_DIR").map_or_else(
        |_| std::env::temp_dir().join("status-bars"),
        std::path::PathBuf::from,
    );
    std::fs::create_dir_all(&dir).expect("output dir");
    let mut app = App::headless_for_test();
    let windows = capture_windows(&mut app);
    let w80 = windows[1].1;
    // "Staged automatic" is a record: the scene commits no row.
    app.clear_messages_for_test();
    app.note_update_progress(&aterm_update::Progress::Staged {
        version: "0.91.0".into(),
        build: 1234,
    });
    app.settle_messages(Instant::now() + PROGRESS_GRACE);
    assert_eq!(
        app.message_band_rows, 0,
        "a staged automatic build is a record"
    );
    // …and so is a staged build whose manual latch lapses: it tries again
    // by itself (`TRIES_AGAIN`), so nothing is the person's to do.
    app.post_message(update_words::staged(
        "0.92.0",
        1234,
        Some(ApplyPosture::ManualOnlyLatched { lapses: true }),
    ));
    app.settle_messages(Instant::now() + PROGRESS_GRACE);
    assert_eq!(app.message_band_rows, 0, "a self-retry is a record");
    // The rows, one scene each, at the four widths — every row one a
    // reporter really posts, shot 2 s after its motion epoch.
    type Scene = (&'static str, fn(&mut App));
    let scenes: [Scene; 27] = [
        ("01-first-run-announced", |app| {
            app.announce_toolchain_pass(FIRST_RUN, true);
        }),
        // The update lane: one row per lane, a title per phase, each shot on
        // the engine's motion grid (`shot`) so the spinner and comet are
        // mid-move.
        ("05-health", |app| {
            app.post_message(update_words::health_warning(
                update_words::HealthKind::Download,
                aterm_update::health_failing_title("pipeline"),
                HEALTH_BODY,
            ));
        }),
        // A download a person started (Check for Updates): the animated
        // row with its ETA (design ruling 220).
        ("06-downloading", |app| {
            app.note_update_check_asked(true);
            app.note_update_progress(&aterm_update::Progress::Downloading {
                version: "0.92.0".into(),
                bytes_done: 45_000_000,
                bytes_total: 74_000_000,
            });
        }),
        ("07-checking", |app| {
            app.note_update_check_asked(true);
            app.note_update_progress(&aterm_update::Progress::Verifying {
                version: "0.92.0".into(),
            });
        }),
        // The same download run by the background loop: nobody is waiting,
        // so the band takes no row; Settings ▸ Software Update shows it.
        ("06b-downloading-unattended", |app| {
            app.note_update_progress(&aterm_update::Progress::Downloading {
                version: "0.92.0".into(),
                bytes_done: 45_000_000,
                bytes_total: 74_000_000,
            });
            assert_eq!(app.messages.live_rows().count(), 0, "silent");
        }),
        // The one row the automatic lane raises before the switch: unsaved
        // editor work holding the install, which only the person can clear.
        ("08c-editor-waits", |app| {
            app.note_update_blockers(&[App::UNSAVED_NATIVE_WORK_BLOCKS_APPLY.to_string()], true);
        }),
        ("09-staged-manual", |app| {
            app.post_message(update_words::staged(
                "0.92.0",
                1234,
                Some(ApplyPosture::ManualByConfig),
            ));
        }),
        // An installed build nothing will switch to by itself: the one
        // decision left to the person, with the Version menu's press.
        ("09-activate", |app| {
            app.note_update_outcome(update_words::needs_install(
                &crate::app_update_screen::update_installed_title(Some("0.92.0")),
                crate::app_update_screen::UPDATE_INSTALLED_DETAIL,
                aterm_messages::Severity::Info,
                1234,
            ));
        }),
        // The first-run pass: the live row, with its answers recorded
        // behind it; the glass shows only the row.
        ("10-first-run-pass", |app| {
            app.announce_toolchain_pass(FIRST_RUN, true);
            app.apply_toolchain_snapshot(Some(&first_run_read()), true);
            record_the_answers(app);
        }),
        ("11-landed", |app| {
            app.post_update_landed("0.92.0", 1234, 0);
        }),
        // A lane that stopped: a row the person acts on, main's words and
        // the `Install now` press (ruling 143; was U17's press-failed row).
        ("11b-update-did-not-apply", |app| {
            // A stopped lane whose attempt failed for a reason the person
            // can act on: its few words beside the title (ruling 246).
            app.note_update_outcome(update_words::needs_install_because(
                "Couldn't install aterm v0.92.0",
                update_words::short_cause("rename: No space left on device (os error 28)"),
                update_words::INSTALL_FROM_MENU,
                aterm_messages::Severity::Warn,
                1234,
            ));
        }),
        // The crash row: the title whole, the home-abbreviated path in the
        // detail behind `Details ›`, and the `Open log` capsule.
        ("12-crash", |app| {
            app.post_message(crate::message_reporters::crash_message(
                &crate::logging::CrashEvidence {
                    path: std::path::PathBuf::from(
                        "/Users//ana/Library/Logs/aterm/crash-signal-1-1.log.seen",
                    ),
                    head: vec!["fatal signal 11 (SIGSEGV)".into()],
                },
            ));
        }),
        // The overflow row: an aterm.toml that tripped five families; the
        // restart family is a record, so four want rows of a three-row band
        // and the third row is the link.
        ("13-overflow", |app| {
            use crate::message_reporters::{ConfigFamily, ConfigWarnings};
            let mut warns = ConfigWarnings::default();
            warns.push(
                ConfigFamily::Keybindings,
                "config keybindings: skipping \"ctrl+x\": unknown action \"foo\"".into(),
            );
            warns.push(
                ConfigFamily::IgnoredKeys,
                "config line 3: windw_padding \u{2014} did you mean \"window_padding\"? \
                 (no effect in this build)"
                    .into(),
            );
            warns.push(
                ConfigFamily::UnacceptedValues,
                "config cursor_style: \"blob\" is not one of block, bar, underline; ignored".into(),
            );
            warns.push(
                ConfigFamily::Fonts,
                "config font_family_bold: \"Nope\" is not an admissible font (not found); \
                 ignored"
                    .into(),
            );
            warns.push(
                ConfigFamily::Restart,
                "gpu applies on next launch (the renderer backend is chosen at startup)".into(),
            );
            app.replace_config_messages(warns.into_messages());
        }),
        // The Full Disk Access question with both capsules, its route words
        // after Open Settings, and a failed gesture's short row.
        ("17-file-access", |app| {
            app.post_message(crate::message_reporters::file_access_question(
                &crate::consent_card::NeedWords::sample(),
            ));
        }),
        ("18-route-words", |app| {
            let id = app.post_message(crate::message_reporters::file_access_question(
                &crate::consent_card::NeedWords::sample(),
            ));
            let intent = app
                .messages
                .act(id, ActionIndex(0), Instant::now())
                .expect("Open Settings");
            app.perform_intent(WindowId(0), id, intent);
        }),
        ("21-gesture-failure", |app| {
            app.post_message(crate::message_reporters::new_tab_failed(
                "fork: Resource temporarily unavailable (os error 35)",
            ));
            app.post_message(crate::message_reporters::split_refused(
                "Split refused: this pane is 20x5 cells; a vertical split needs at least 20x7",
            ));
        }),
        // Attention triage rows.
        ("22-installing-explicit", |app| {
            app.begin_update_installing(true);
            app.level_up = None;
        }),
        ("23-routine-heavy-pass", |app| {
            app.apply_toolchain_snapshot(Some(&first_run_read()), false);
        }),
        // The dead publisher stands on the glass only where a screen
        // reader had attached (with none it is a record).
        ("24-gpu-and-a11y", |app| {
            app.post_message(crate::message_reporters::gpu_lost());
            app.post_message(crate::message_reporters::a11y_publisher_dead(
                "the accessibility bridge thread exited",
                true,
            ));
        }),
        ("25-one-unknown-key", |app| {
            use crate::message_reporters::{ConfigFamily, ConfigWarnings};
            let mut warns = ConfigWarnings::default();
            warns.push(
                ConfigFamily::IgnoredKeys,
                "config line 3: windw_padding \u{2014} did you mean \"window_padding\"? \
                 (no effect in this build)"
                    .into(),
            );
            app.replace_config_messages(warns.into_messages());
        }),
        ("26-move-to-applications", |app| {
            app.post_message(
                crate::message_reporters::install_posture(
                    aterm_update::which_copy::InstallPosture::MountedImage,
                )
                .expect("a posture to fix"),
            );
        }),
        // Heavy system use, named from the first frame: the row is on the
        // glass because it is heavy.
        ("28-first-run-disk-busy", |app| {
            app.announce_toolchain_pass(FIRST_RUN, true);
            app.apply_toolchain_snapshot(Some(&first_run_read()), true);
        }),
        // A first run that failed: an install, in the lane's one noun.
        ("32-alab-install-failed", |app| {
            app.post_message(toolchain_words::first_run_short(
                toolchain_words::FirstRunShort::Failed,
                "could not reach the index",
            ));
        }),
        // The lane's one retry promise, on the stopped-lane row.
        ("11c-install-tries-again", |app| {
            app.note_update_outcome(update_words::needs_install(
                "Couldn't install aterm v0.92.0",
                update_words::TRIES_AGAIN,
                aterm_messages::Severity::Warn,
                1234,
            ));
        }),
        // The config families' short titles: one value, three trail
        // settings.
        ("34-config-values", |app| {
            use crate::message_reporters::{ConfigFamily, ConfigWarnings};
            let mut warns = ConfigWarnings::default();
            warns.push(
                ConfigFamily::UnacceptedValues,
                "config cursor_style: \"blob\" is not one of block, bar, underline; ignored".into(),
            );
            for key in ["trail_length", "trail_fade", "trail_theme"] {
                warns.push(
                    ConfigFamily::CursorTrail,
                    format!("config {key}: out of range; the default is used"),
                );
            }
            app.replace_config_messages(warns.into_messages());
        }),
        // The session rows whose repeated detail line went.
        ("35-shell-lost", |app| {
            app.post_message(crate::message_reporters::shell_lost_in_update(
                "the pty handed over was closed (os error 9)",
            ));
            app.post_message(crate::message_reporters::restored_tab_failed(
                "fork: Resource temporarily unavailable (os error 35)",
            ));
        }),
        // An a11y bridge that died with a screen reader attached: the
        // retry line alone behind Details.
        ("36-a11y-dead", |app| {
            app.post_message(crate::message_reporters::a11y_publisher_dead(
                "the accessibility bridge thread exited",
                true,
            ));
        }),
    ];
    for (name, scene) in scenes {
        app.clear_messages_for_test();
        scene(&mut app);
        let at = shot(&mut app);
        for (cols, wid) in windows {
            if !capture(
                &mut app,
                wid,
                &dir,
                &format!("{name}-{cols}"),
                at,
                Look::MOVING,
            ) {
                crate::logging::stderr_line!("no system font \u{2014} visual capture skipped");
                return;
            }
            capture_grounds(
                &mut app,
                wid,
                cols,
                &dir,
                name,
                at,
                Look::MOVING,
                &ROW_GROUNDS,
            );
        }
    }
    // Hover: the staged manual row with the pointer on `Install now` (a lit
    // Primary), then on the row body (`band_hover_for`), which lights only
    // the row's `Details ›`, the press a body click performs.
    app.clear_messages_for_test();
    app.post_message(update_words::staged(
        "0.91.0",
        1234,
        Some(ApplyPosture::ManualByConfig),
    ));
    let at = shot(&mut app);
    for (cols, wid) in windows {
        for (tag, target) in [
            ("capsule", HoverTarget::Capsule(ActionIndex(0))),
            ("body", HoverTarget::Body),
        ] {
            app.windows.get_mut(&wid).unwrap().band_hover = Some(BandHover { row: 0, target });
            capture(
                &mut app,
                wid,
                &dir,
                &format!("14-hover-{tag}-{cols}"),
                at,
                Look::MOVING,
            );
            capture_grounds(
                &mut app,
                wid,
                cols,
                &dir,
                &format!("14-hover-{tag}"),
                at,
                Look::MOVING,
                &ROW_GROUNDS,
            );
            app.windows.get_mut(&wid).unwrap().band_hover = None;
        }
    }
    // The first-run sequence (the silent lane): the live row, the pass's
    // answers recorded behind it (no row), then the child's exit folds the
    // row: its Complete echo, and one D1 quiet later the band gives it back.
    app.clear_messages_for_test();
    app.announce_toolchain_pass(FIRST_RUN, true);
    app.apply_toolchain_snapshot(Some(&first_run_read()), true);
    record_the_answers(&mut app);
    assert_eq!(app.message_band_rows, 1, "the one row");
    let at = shot(&mut app);
    capture(&mut app, w80, &dir, "15-first-run-live", at, Look::MOVING);
    app.toolchain_pass_ended(true);
    let quiet = SHRINK_QUIET + Duration::from_secs(2);
    let shrunk = app.settle_messages(Instant::now() + quiet);
    crate::logging::stderr_line!(
        "16-first-run-folded: shrunk={shrunk} rows={}",
        app.message_band_rows
    );
    assert_eq!(
        app.message_band_rows, 0,
        "the row folded at the child's exit; the band gave its row back"
    );
    capture(
        &mut app,
        w80,
        &dir,
        "16-first-run-folded",
        Instant::now() + quiet,
        Look::MOVING,
    );
    motion_sequences(&mut app, &dir, &windows);
    merged_design_scenes(&mut app, &dir, &windows);
    strain_scenes(&mut app, &dir, &windows);
    composition_scenes(&mut app, &dir, &windows);
    person_pass_scenes(&mut app, &dir, &windows);
    session_wait_scenes(&mut app, &dir, &windows);
    wire_and_glint_scenes(&mut app, &dir, &windows);
}

/// A script's `notice` lines through the engine's own `wire::apply`, the
/// host's path, on the capture's clock.
fn notice(app: &mut App, line: &str, at: Instant) {
    let req = aterm_messages::wire::NoticeRequest::parse(line).expect("a notice line");
    let applied = aterm_messages::wire::apply(
        &mut app.messages,
        &mut app.wire_gate,
        req,
        crate::messages_host::wall_stamp_now(),
        at,
    );
    assert!(applied.reply.starts_with("OK"), "{line}: {}", applied.reply);
}

/// A script's `notice progress` rows (busy, a percentage fill, an amount
/// with its ETA, its Complete echo) and failure row, then the glint
/// mid-travel over a settled bar, at every width on the default ground and
/// [`ROW_GROUNDS`].
#[allow(
    clippy::too_many_lines,
    reason = "one list of scenes, read top to bottom"
)]
fn wire_and_glint_scenes(app: &mut App, dir: &std::path::Path, windows: &[(&str, WindowId)]) {
    let shoot = |app: &mut App, name: &str, at: Instant| {
        app.settle_messages(at);
        for &(cols, wid) in windows {
            capture(app, wid, dir, &format!("{name}-{cols}"), at, Look::MOVING);
            capture_grounds(app, wid, cols, dir, name, at, Look::MOVING, &ROW_GROUNDS);
        }
    };
    // 70: `notice progress build busy`, 3 s in — the comet.
    app.clear_messages_for_test();
    let start = Instant::now();
    notice(
        app,
        "progress build busy load=cpu Building aterm -- release profile",
        start,
    );
    let epoch = start + PROGRESS_GRACE;
    app.settle_messages(epoch);
    let epoch = app
        .messages
        .on_glass()
        .find_map(|l| l.motion_since)
        .unwrap_or(epoch);
    shoot(app, "70-notice-busy", epoch + Duration::from_secs(2));
    // 71: `pct=`, restated every 200 ms from 10 % to 64 %.
    app.clear_messages_for_test();
    let start = Instant::now();
    let mut t = start;
    for k in 0..=27u64 {
        t = start + Duration::from_millis(200 * k);
        let pct = 10 + 2 * k;
        notice(
            app,
            &format!("progress index pct={pct} Indexing the photo library"),
            t,
        );
    }
    shoot(
        app,
        "71-notice-pct",
        t + aterm_messages::FILL_GLIDE_MAX + aterm_messages::ANIM_FRAME,
    );
    // 72: `done=<n>/<total> unit=bytes` at 3 MB/s for 8 s — the ETA.
    app.clear_messages_for_test();
    let start = Instant::now();
    let total = 180_000_000u64;
    let mut t = start;
    for k in 0..=80u64 {
        t = start + Duration::from_millis(100 * k);
        let done = 12_000_000 + 300_000 * k;
        notice(
            app,
            &format!(
                "progress backup done={done}/{total} unit=bytes load=network \
                 Uploading the backup"
            ),
            t,
        );
    }
    shoot(
        app,
        "72-notice-amount-eta",
        t + aterm_messages::FILL_GLIDE_MAX + aterm_messages::ANIM_FRAME,
    );
    // 73: `notice done backup ok`, a quarter of a second into its echo.
    let end = t + Duration::from_millis(100);
    notice(app, "done backup ok", end);
    shoot(app, "73-notice-done-echo", end + Duration::from_millis(250));
    // 74: `notice done backup warn` on a fresh row: the Fault echo.
    app.clear_messages_for_test();
    let start = Instant::now();
    let mut t = start;
    for k in 0..=40u64 {
        t = start + Duration::from_millis(100 * k);
        let done = 300_000 * k;
        notice(
            app,
            &format!("progress backup done={done}/{total} unit=bytes Uploading the backup"),
            t,
        );
    }
    // On the glass first: a row resolved before it was ever shown
    // retires unseen, with no echo.
    app.settle_messages(t);
    let end = t + Duration::from_millis(100);
    notice(app, "done backup warn", end);
    shoot(app, "74-notice-fault-echo", end + Duration::from_millis(60));
    // 75: `notice post deploy sev=warn`: a script's failure row.
    app.clear_messages_for_test();
    notice(
        app,
        "post deploy sev=warn Deploy to staging failed -- exit status 2\\nsee deploy.log",
        Instant::now(),
    );
    let at = shot(app);
    shoot(app, "75-notice-failed", at);
    // 76: the GLINT mid-travel over a settled bar at 57 %: a quarter, half
    // and three quarters of the way across.
    app.clear_messages_for_test();
    let mut download = Download {
        total: 200_000_000,
        id: None,
    };
    let (t, _) = download.run_to(app, Instant::now(), 0, 4_000_000, 570);
    app.settle_messages(t);
    let epoch = app
        .messages
        .on_glass()
        .find_map(|l| l.motion_since)
        .expect("on glass");
    let from = aterm_messages::animate::next_glint_start(
        t + aterm_messages::FILL_GLIDE_MAX + aterm_messages::ANIM_FRAME,
        epoch,
    );
    for k in 1..=3u32 {
        shoot(
            app,
            &format!("76-glint-{k}"),
            from + aterm_messages::GLINT_TRAVEL * k / 4,
        );
    }
    app.clear_messages_for_test();
}

/// Waits inside a session: a large paste to a slow program and a scrollback
/// rewrap, each mid-way with its ETA and at its Complete echo, at every
/// width and on GitHub Light.
fn session_wait_scenes(app: &mut App, dir: &std::path::Path, windows: &[(&str, WindowId)]) {
    use aterm_messages::waits::{paste_row, rewrap_row};
    let shoot = |app: &mut App, name: &str, at: Instant| {
        app.settle_messages(at);
        let at = app
            .messages
            .on_glass()
            .filter_map(|l| l.motion_since)
            .max()
            .map_or(at, |epoch| (epoch + Duration::from_secs(2)).max(at));
        for &(cols, wid) in windows {
            capture(app, wid, dir, &format!("{name}-{cols}"), at, Look::MOVING);
            capture_in(
                app,
                wid,
                dir,
                &format!("{name}-github-light-{cols}"),
                at,
                Look::MOVING,
                scheme("GitHub Light"),
            );
            capture_grounds(
                app,
                wid,
                cols,
                dir,
                name,
                at,
                Look::MOVING,
                &["Tokyo Night", "Solarized Light"],
            );
        }
    };
    // A 4.2 MB paste into a line editor at 110 KB/s, read every 100 ms for
    // 10 s: `Pasting 4.2 MB`, the fill, `1.1 MB / 4.2 MB`, the ETA and
    // `Stop paste`.
    app.clear_messages_for_test();
    let start = Instant::now();
    let total = 4_200_000;
    let series = aterm_messages::Amount::series_of("session.paste.0#1");
    let id = app
        .messages
        .post(
            paste_row(0, series, 0, total, PROGRESS_GRACE),
            crate::messages_host::wall_stamp_now(),
            start,
        )
        .id;
    let mut t = start;
    for k in 1..=100u64 {
        t = start + Duration::from_millis(100 * k);
        let msg = paste_row(0, series, 11_000 * k, total, Duration::ZERO);
        app.messages
            .restate(id, crate::messages_host::restatement_of(&msg), t);
    }
    shoot(app, "90-paste-eta", t);
    // 91: its Complete echo, a quarter of a second in.
    let end = t + Duration::from_millis(100);
    app.messages
        .withdraw_with(id, aterm_messages::EchoKind::Complete, end);
    shoot(
        app,
        "91-paste-complete-echo",
        end + Duration::from_millis(250),
    );
    // 92: a 3.4M-line history rewrapping at 200K lines/s, sampled every
    // 250 ms for 6 s after the person scrolled up into it.
    app.clear_messages_for_test();
    let start = Instant::now();
    let lines = 3_400_000;
    let id = app
        .messages
        .post(
            rewrap_row(0, 0, lines, PROGRESS_GRACE),
            crate::messages_host::wall_stamp_now(),
            start,
        )
        .id;
    let mut t = start;
    for k in 1..=24u64 {
        t = start + Duration::from_millis(250 * k);
        let msg = rewrap_row(0, 50_000 * k, lines, Duration::ZERO);
        app.messages
            .restate(id, crate::messages_host::restatement_of(&msg), t);
    }
    shoot(app, "92-rewrap-eta", t);
    // 93: the re-attach's Complete echo.
    let end = t + Duration::from_millis(100);
    app.messages
        .withdraw_with(id, aterm_messages::EchoKind::Complete, end);
    shoot(
        app,
        "93-rewrap-complete-echo",
        end + Duration::from_millis(250),
    );
    app.clear_messages_for_test();
}

/// A light update pass's read (design ruling 224): `claude` done, `codex`
/// downloading `done` of its 60 MB — `1 of 2 programs`, network busy.
fn check_update_read(done: u64) -> crate::PkgProgressSnapshot {
    let mut programs = std::collections::BTreeMap::new();
    let program = |phase, bytes_done, bytes_total| atpkg::progress::ProgramProgress {
        phase,
        bytes_done,
        bytes_total,
        build: Some(156),
        bumped: false,
        error: None,
    };
    programs.insert(
        "claude".to_string(),
        program(atpkg::progress::Phase::Done, 58_000_000, 58_000_000),
    );
    programs.insert(
        "codex".to_string(),
        program(atpkg::progress::Phase::Download, done, 60_000_000),
    );
    crate::PkgProgressSnapshot {
        file: atpkg::progress::ProgressFile {
            v: atpkg::progress::PROGRESS_VERSION,
            pid: Some(9),
            pass: "net".to_string(),
            started_unix: 1_700_000_500,
            heartbeat_unix: 1_700_000_500,
            overall: atpkg::progress::Overall {
                programs_done: 1,
                programs_total: 2,
                bytes_done: 58_000_000 + done,
                bytes_total: 118_000_000,
            },
            queue: Vec::new(),
            programs,
            ended_unix: None,
        },
        running: true,
    }
}

/// Restate the person's live row with `read`'s words at `t` (the host's
/// `apply_toolchain_snapshot` on the capture's synthetic clock), so the
/// fill's own rate latches its ETA.
fn restate_person_read(
    app: &mut App,
    verb: crate::message_reporters::PackagesVerb,
    read: &crate::PkgProgressSnapshot,
    t: Instant,
) {
    let routine = verb != crate::message_reporters::PackagesVerb::Install;
    let id = app
        .messages
        .live_by_key(toolchain_words::KEY_PASS)
        .map(|l| l.id)
        .expect("the person's row");
    if let toolchain_words::SnapshotWords::Live { message, .. } =
        toolchain_words::snapshot_words(Some(read), None, routine)
    {
        let message = message.finished_as(verb.finished_work());
        app.messages
            .restate(id, crate::messages_host::restatement_of(&message), t);
    }
}

/// Settings ▸ Packages verbs and the sized update download, at every width:
/// Check busy with its elapsed clock, queued at the store lock, filled with
/// its ETA once the plan lands, and its Complete echo; Install sized by its
/// announcement, then filled with its ETA; Remove busy on the disk; a
/// person's download with its total.
fn person_pass_scenes(app: &mut App, dir: &std::path::Path, windows: &[(&str, WindowId)]) {
    use crate::message_reporters::PackagesVerb;
    use crate::messages_host::PersonPass;
    // Settled at `at`, shot on the engine's motion grid 2 s past the
    // newest row's motion epoch (as `shot`), so the comet is mid-row.
    let shoot = |app: &mut App, name: &str, at: Instant| {
        app.settle_messages(at);
        let at = app
            .messages
            .on_glass()
            .filter_map(|l| l.motion_since)
            .max()
            .map_or(at, |epoch| (epoch + Duration::from_secs(2)).max(at));
        for &(cols, wid) in windows {
            capture(app, wid, dir, &format!("{name}-{cols}"), at, Look::MOVING);
            capture_grounds(app, wid, cols, dir, name, at, Look::MOVING, &ROW_GROUNDS);
        }
    };
    // 80: the Check, busy, 12 s in (the elapsed clock shows past 10 s).
    app.clear_messages_for_test();
    let start = Instant::now();
    app.person_pass(PersonPass::Began(PackagesVerb::Check));
    shoot(app, "80-check-busy", start + Duration::from_secs(12));
    // 81: queued behind the six-hourly pass, 40 s in.
    app.clear_messages_for_test();
    let start = Instant::now();
    app.person_pass(PersonPass::Began(PackagesVerb::Check));
    app.person_pass(PersonPass::Waiting);
    shoot(app, "81-check-waiting", start + Duration::from_secs(40));
    // The plan landed: `codex` downloading at 6 MB/s, read every 100 ms for
    // 6 s: `Updating ALab tools`, the fill, `1 of 2 programs`, the ETA,
    // network busy.
    app.clear_messages_for_test();
    let start = Instant::now();
    app.person_pass(PersonPass::Began(PackagesVerb::Check));
    let mut t = start;
    for k in 0..=60u64 {
        t = start + PROGRESS_GRACE + Duration::from_millis(100 * k);
        restate_person_read(
            app,
            PackagesVerb::Check,
            &check_update_read(4_000_000 + 600_000 * k),
            t,
        );
    }
    shoot(app, "82-check-updating-eta", t);
    // 83: its Complete echo, a quarter of a second into it — the echo the
    // child's exit names (`App::person_pass`), on the scene's clock.
    let id = app
        .messages
        .live_by_key(toolchain_words::KEY_PASS)
        .map(|l| l.id)
        .expect("the Check's row");
    let end = t + Duration::from_millis(100);
    app.messages
        .withdraw_with(id, aterm_messages::EchoKind::Complete, end);
    app.person_pass(PersonPass::Ended(aterm_messages::EchoKind::Complete));
    shoot(
        app,
        "83-check-complete-echo",
        end + Duration::from_millis(250),
    );
    // 84: the Install, sized by atpkg's announcement before its plan.
    app.clear_messages_for_test();
    let start = Instant::now();
    app.person_pass(PersonPass::Began(PackagesVerb::Install));
    app.announce_toolchain_pass(FIRST_RUN, false);
    shoot(app, "84-install-announced", start + Duration::from_secs(4));
    // 85: the Install's plan, `trust` extracting at 25 MB/s for 8 s.
    let mut t = start;
    for k in 0..=80u64 {
        t = start + PROGRESS_GRACE + Duration::from_millis(100 * k);
        restate_person_read(
            app,
            PackagesVerb::Install,
            &first_run_read_at(120_000_000 + 2_500_000 * k),
            t,
        );
    }
    shoot(app, "85-install-eta", t);
    // 86: the Remove, busy on the disk, 6 s in.
    app.clear_messages_for_test();
    let start = Instant::now();
    app.person_pass(PersonPass::Began(PackagesVerb::Remove));
    shoot(app, "86-remove-busy", start + Duration::from_secs(6));
    // 87: a person's update download with the size the HEAD gave it, at
    // 4 MB/s for 10 s: the fill, `45 MB / 74 MB` and its ETA.
    app.clear_messages_for_test();
    let start = Instant::now();
    let report = |done: u64| aterm_update::Progress::Downloading {
        version: "0.95.0".into(),
        bytes_done: done,
        bytes_total: 74_000_000,
    };
    let id = app
        .messages
        .post(
            update_words::progress(&report(5_000_000), None, "0.95.0"),
            crate::messages_host::wall_stamp_now(),
            start,
        )
        .id;
    let mut t = start;
    for k in 1..=100u64 {
        t = start + Duration::from_millis(100 * k);
        let msg = update_words::progress(&report(5_000_000 + 400_000 * k), None, "0.95.0");
        app.messages
            .restate(id, crate::messages_host::restatement_of(&msg), t);
    }
    shoot(app, "87-download-sized-eta", t);
    app.clear_messages_for_test();
}

/// The strain row as the engine builds it (`StrainTracker::row`): its title,
/// a level meter at `permille` with stats, load words only when a culprit
/// is named, Warn under critical memory.
fn strain_row(
    kind: aterm_messages::strain::StrainKind,
    culprit: &aterm_messages::strain::Culprit,
    permille: u16,
    stats: &str,
    severity: aterm_messages::Severity,
) -> aterm_messages::Message {
    use aterm_messages::strain::{self, Culprit, StrainConfig};
    use aterm_messages::{Hold, Message, Meter, STRAIN_KEY, tags};
    let cfg = StrainConfig {
        services_noun: strain::MACOS_SERVICES,
        glass: true,
        self_pid: 1,
    };
    let named = !matches!(culprit, Culprit::Resource);
    let msg = Message::new(tags::SYSTEM, severity, strain::title(kind, culprit, &cfg))
        .key(STRAIN_KEY)
        .hold(Hold::Live {
            stale_after: aterm_messages::STALE_STRAIN,
        })
        // The engine's declaration: the one load the kind explains
        // (design ruling 221).
        .loads([kind.load()])
        .meter(Meter {
            load: named.then(|| kind.load()),
            ..Meter::level(permille, stats)
        })
        .no_excerpt();
    // The navigation to the tab the title names (ruling 243).
    match strain::culprit_tab(culprit) {
        Some(show) => msg.action(show),
        None => msg,
    }
}

/// The strain row's level gauge for each kind (CPU with a named tab, memory
/// under critical pressure as Warn, disk with no culprit) on the default
/// ground, Tokyo Night and two light themes; then beside a download's
/// progress bar, so the two meters read as distinct.
fn strain_scenes(app: &mut App, dir: &std::path::Path, windows: &[(&str, WindowId)]) {
    use aterm_messages::Severity;
    use aterm_messages::strain::{Culprit, StrainKind};
    let tab2 = Culprit::Session {
        program: "yes".into(),
        tab: 2,
        elsewhere: false,
        receiving_keys: false,
        window: windows.first().map_or(0, |(_, wid)| wid.0),
    };
    let rows = [
        (
            "cpu",
            strain_row(StrainKind::Cpu, &tab2, 740, "6 of 8 cores", Severity::Info),
        ),
        (
            "memory",
            strain_row(
                StrainKind::Memory,
                &Culprit::Resource,
                960,
                "memory full \u{b7} swapping",
                Severity::Warn,
            ),
        ),
        (
            "disk",
            strain_row(
                StrainKind::Disk,
                &Culprit::Resource,
                420,
                "waiting on disk",
                Severity::Info,
            ),
        ),
    ];
    for (slug, msg) in rows {
        app.clear_messages_for_test();
        app.post_message(msg);
        let at = shot(app);
        for &(cols, wid) in windows {
            capture(
                app,
                wid,
                dir,
                &format!("60-strain-{slug}-{cols}"),
                at,
                Look::MOVING,
            );
            for scheme_name in THEMED_SCHEMES.into_iter().take(3) {
                let theme = scheme(scheme_name);
                let ground = scheme_name.to_lowercase().replace(' ', "-");
                capture_in(
                    app,
                    wid,
                    dir,
                    &format!("61-strain-{slug}-{ground}-{cols}"),
                    at,
                    Look::MOVING,
                    theme,
                );
            }
        }
    }
    // The gauge beside a download at 50 %: a level and a fill, two rows.
    app.clear_messages_for_test();
    let mut download = Download {
        total: 200_000_000,
        id: None,
    };
    let (t, _) = download.run_to(app, Instant::now(), 0, 4_000_000, 500);
    // Posted at the harness's instant (the download's reads run 25 s of
    // injected time): at the wall's, the row would be past its staleness.
    let strain_at = t - Duration::from_secs(4);
    app.messages.post(
        strain_row(StrainKind::Cpu, &tab2, 740, "6 of 8 cores", Severity::Info),
        crate::messages_host::wall_stamp_now(),
        strain_at,
    );
    let at = t + aterm_messages::FILL_GLIDE_MAX + aterm_messages::ANIM_FRAME;
    app.settle_messages(at);
    for &(cols, wid) in windows {
        capture(
            app,
            wid,
            dir,
            &format!("62-strain-beside-download-{cols}"),
            at,
            Look::MOVING,
        );
        capture_grounds(
            app,
            wid,
            cols,
            dir,
            "62-strain-beside-download",
            at,
            Look::MOVING,
            &["Tokyo Night", "Solarized Light"],
        );
        capture_in(
            app,
            wid,
            dir,
            &format!("62-strain-beside-download-github-light-{cols}"),
            at,
            Look::MOVING,
            scheme("GitHub Light"),
        );
    }
}

/// The capture windows every band scene is shot in: 60, 80, 120 and 160
/// whole columns, plus `g80` and `g120`, which are not a whole number of
/// cells wide (with gutters). Each shows the same shell lines and stands in
/// for the focused window.
fn capture_windows(app: &mut App) -> [(&'static str, WindowId); 6] {
    let w80 = WindowId(0);
    let w60 = app.insert_logical_window(crate::stub_session(1), 20, 60);
    let w120 = app.insert_logical_window(crate::stub_session(2), 20, 120);
    let w160 = app.insert_logical_window(crate::stub_session(3), 20, 160);
    let g80 = glass_window(app, 4, 80, 10, 7);
    let g120 = glass_window(app, 5, 120, 8, 11);
    app.frontmost_window = Some(w80);
    let windows = [
        ("60", w60),
        ("80", w80),
        ("120", w120),
        ("160", w160),
        ("g80", g80),
        ("g120", g120),
    ];
    for (_, wid) in windows {
        content(app, wid);
        // Each window stands in for the focused one; every scene is shot in
        // an explicit look, so the comet follows the engine's grid. The
        // glyph cell's `⚠ ✕ ✓ ℹ ↻` are drawn icons, independent of the font.
        app.windows.get_mut(&wid).unwrap().focused = true;
    }
    windows
}

/// Every capsule the band laid out in window `wid`, top row first, as
/// `(row, slug, hover target)`: authored capsules by action, the row's
/// `Details ›` (and the overflow row's `Messages ›`) by the row body
/// (`band_hover_for`). Read after a render.
fn laid_capsules(app: &App, wid: WindowId) -> Vec<(u8, String, HoverTarget)> {
    let Some(presentation) = app.windows[&wid].band.layout() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (r, row) in presentation.rows.iter().enumerate() {
        for capsule in &row.capsules {
            let slug: String = capsule
                .full_label
                .chars()
                .filter_map(|c| {
                    if c.is_ascii_alphanumeric() {
                        Some(c.to_ascii_lowercase())
                    } else if c == ' ' {
                        Some('-')
                    } else {
                        None
                    }
                })
                .collect::<String>()
                .trim_matches('-')
                .to_string();
            let target = match capsule.role {
                aterm_messages::CapsuleRole::Details => HoverTarget::Body,
                _ => HoverTarget::Capsule(capsule.action),
            };
            out.push((u8::try_from(r).unwrap_or(u8::MAX), slug, target));
        }
    }
    out
}

/// Compositions of several row kinds at once: a person's download with its
/// ETA under a config warning beside the strain row; five kinds (the
/// overflow row); a paste with `Stop paste` while a script's failure row is
/// up; the pointer on each capsule of a mixed stack; and Catppuccin Latte's
/// outlined Primary over a fill beside GitHub Light's.
#[allow(
    clippy::too_many_lines,
    reason = "one list of compositions, read top to bottom"
)]
fn composition_scenes(app: &mut App, dir: &std::path::Path, windows: &[(&str, WindowId)]) {
    use aterm_messages::strain::{Culprit, StrainKind};
    use aterm_messages::{Hold, Intent, Message, Meter, Severity, tags};
    let tab2 = Culprit::Session {
        program: "yes".into(),
        tab: 2,
        elsewhere: false,
        receiving_keys: false,
        window: windows.first().map_or(0, |(_, wid)| wid.0),
    };
    let config_warning = || {
        use crate::message_reporters::{ConfigFamily, ConfigWarnings};
        let mut warns = ConfigWarnings::default();
        warns.push(
            ConfigFamily::IgnoredKeys,
            "config line 3: windw_padding \u{2014} did you mean \"window_padding\"? \
             (no effect in this build)"
                .into(),
        );
        warns.into_messages()
    };
    let shoot = |app: &mut App, name: &str, at: Instant| {
        crate::logging::stderr_line!("{name}: band rows {}", app.message_band_rows);
        for &(cols, wid) in windows {
            capture(app, wid, dir, &format!("{name}-{cols}"), at, Look::MOVING);
            capture_grounds(app, wid, cols, dir, name, at, Look::MOVING, &ROW_GROUNDS);
        }
    };
    // The pointer on each capsule of the stack, at 80, 120 and g120, on
    // the default ground and GitHub Light.
    let hover_each = |app: &mut App, name: &str, at: Instant| {
        for &(cols, wid) in windows {
            if !["80", "120", "g120"].contains(&cols) {
                continue;
            }
            // Lay the band out in this window first.
            render(app, wid, at, Look::MOVING, aterm_render::Theme::default());
            for (row, slug, target) in laid_capsules(app, wid) {
                app.windows.get_mut(&wid).unwrap().band_hover = Some(BandHover { row, target });
                let shot_name = format!("{name}-hover-r{row}-{slug}");
                capture(
                    app,
                    wid,
                    dir,
                    &format!("{shot_name}-{cols}"),
                    at,
                    Look::MOVING,
                );
                capture_in(
                    app,
                    wid,
                    dir,
                    &format!("{shot_name}-github-light-{cols}"),
                    at,
                    Look::MOVING,
                    scheme("GitHub Light"),
                );
                app.windows.get_mut(&wid).unwrap().band_hover = None;
            }
        }
    };
    // Three kinds at once: a person's download (Check for Updates) at 45 %
    // with its ETA latched, a config warning, and the strain gauge, each
    // posted on the capture's own clock.
    let three = |app: &mut App| -> Instant {
        app.clear_messages_for_test();
        app.note_update_check_asked(true);
        let mut download = Download {
            total: 74_000_000,
            id: None,
        };
        let (t, _) = download.run_to(app, Instant::now(), 0, 2_000_000, 450);
        let near = t - Duration::from_secs(4);
        for msg in config_warning() {
            app.messages
                .post(msg, crate::messages_host::wall_stamp_now(), near);
        }
        app.messages.post(
            strain_row(StrainKind::Cpu, &tab2, 740, "6 of 8 cores", Severity::Info),
            crate::messages_host::wall_stamp_now(),
            near,
        );
        let at = t + aterm_messages::FILL_GLIDE_MAX + aterm_messages::ANIM_FRAME;
        app.settle_messages(at);
        at
    };
    let at = three(app);
    shoot(app, "94-three-kinds", at);
    hover_each(app, "94-three-kinds", at);
    // Five kinds at once (the three above, a crash and a dead screen-reader
    // bridge): the band keeps three rows and the last is the overflow link.
    let at = three(app);
    let near = at - Duration::from_secs(3);
    app.messages.post(
        crate::message_reporters::crash_message(&crate::logging::CrashEvidence {
            path: std::path::PathBuf::from(
                "/Users//ana/Library/Logs/aterm/crash-signal-1-1.log.seen",
            ),
            head: vec!["fatal signal 11 (SIGSEGV)".into()],
        }),
        crate::messages_host::wall_stamp_now(),
        near,
    );
    app.messages.post(
        crate::message_reporters::a11y_publisher_dead(
            "the accessibility bridge thread exited",
            true,
        ),
        crate::messages_host::wall_stamp_now(),
        near,
    );
    app.settle_messages(at);
    shoot(app, "95-five-kinds-overflow", at);
    hover_each(app, "95-five-kinds-overflow", at);
    // A 4.2 MB paste into a slow program, 10 s in, with its `Stop
    // paste`, while a script's failure row is up.
    {
        use aterm_messages::waits::paste_row;
        app.clear_messages_for_test();
        let start = Instant::now();
        notice(
            app,
            "post deploy sev=warn Deploy to staging failed -- exit status 2\\nsee deploy.log",
            start,
        );
        let total = 4_200_000;
        let series = aterm_messages::Amount::series_of("session.paste.0#1");
        let id = app
            .messages
            .post(
                paste_row(0, series, 0, total, PROGRESS_GRACE),
                crate::messages_host::wall_stamp_now(),
                start,
            )
            .id;
        let mut t = start;
        for k in 1..=100u64 {
            t = start + Duration::from_millis(100 * k);
            let msg = paste_row(0, series, 11_000 * k, total, Duration::ZERO);
            app.messages
                .restate(id, crate::messages_host::restatement_of(&msg), t);
        }
        let at = t + aterm_messages::FILL_GLIDE_MAX + aterm_messages::ANIM_FRAME;
        app.settle_messages(at);
        shoot(app, "96-paste-beside-warning", at);
        hover_each(app, "96-paste-beside-warning", at);
    }
    // Catppuccin Latte's outlined Primary over a fill: a metered row with a
    // Primary and a Secondary at 50 %, 80 %, 95 % and 100 %, still and lit,
    // on Latte and, for comparison, GitHub Light and the default ground.
    // Each fill matches its count.
    for (permille, count) in [
        (500u16, "2 of 4 tabs"),
        (800, "4 of 5 tabs"),
        (950, "19 of 20 tabs"),
        (1000, "4 of 4 tabs"),
    ] {
        app.clear_messages_for_test();
        app.messages.post(
            Message::new(tags::SESSION, Severity::Info, "Saving the session")
                .no_excerpt()
                .meter(Meter {
                    fill_permille: Some(permille),
                    stats: count.into(),
                    ..Meter::default()
                })
                .hold(Hold::Live {
                    stale_after: Duration::from_secs(120),
                })
                .action(Intent::NewWindow)
                .action(Intent::OpenPath {
                    path: "/Users//ana/Library/Logs/aterm/aterm.log".into(),
                }),
            crate::messages_host::wall_stamp_now(),
            Instant::now(),
        );
        let at = shot(app);
        let pct = permille / 10;
        for &(cols, wid) in windows {
            if !["80", "120", "g120"].contains(&cols) {
                continue;
            }
            for (ground, theme) in [
                ("catppuccin-latte", scheme("Catppuccin Latte")),
                ("github-light", scheme("GitHub Light")),
                ("default", aterm_render::Theme::default()),
            ] {
                for (lit, hover) in [
                    ("still", None),
                    (
                        "lit",
                        Some(BandHover {
                            row: 0,
                            target: HoverTarget::Capsule(ActionIndex(0)),
                        }),
                    ),
                ] {
                    app.windows.get_mut(&wid).unwrap().band_hover = hover;
                    capture_in(
                        app,
                        wid,
                        dir,
                        &format!("97-outlined-over-fill-{ground}-{pct:03}-{lit}-{cols}"),
                        at,
                        Look::MOVING,
                        theme,
                    );
                }
                app.windows.get_mut(&wid).unwrap().band_hover = None;
            }
        }
    }
    app.clear_messages_for_test();
}

/// The compositions alone ([`composition_scenes`]), into
/// `STATUS_BARS_PNG_DIR`, without the full scene list's other frames.
///
/// ```text
/// STATUS_BARS_PNG_DIR=<dir> targo --unverified test -p aterm-gui --lib \
///     message_band_composition_capture -- --ignored --nocapture
/// ```
#[test]
#[ignore = "visual capture: needs a system font; run with --ignored"]
fn message_band_composition_capture() {
    let dir = std::env::var("STATUS_BARS_PNG_DIR").map_or_else(
        |_| std::env::temp_dir().join("status-bars"),
        std::path::PathBuf::from,
    );
    std::fs::create_dir_all(&dir).expect("output dir");
    let mut app = App::headless_for_test();
    let windows = capture_windows(&mut app);
    if aterm_render::Renderer::from_system(CAPTURE_PX, aterm_render::Theme::default()).is_none() {
        crate::logging::stderr_line!("no system font \u{2014} visual capture skipped");
        return;
    }
    composition_scenes(&mut app, &dir, &windows);
}

/// The motion frame sequences at 80 and 120 columns.
#[allow(
    clippy::too_many_lines,
    reason = "one sequence per motion form, read top to bottom"
)]
fn motion_sequences(app: &mut App, dir: &std::path::Path, windows: &[(&str, WindowId)]) {
    // The comet: eight frames across one crossing in steady state (the last
    // head's tail draining out the right while the next ignites at the
    // left), on the first run's announcement (work with no fraction).
    app.clear_messages_for_test();
    app.announce_toolchain_pass(FIRST_RUN, true);
    let epoch = app
        .messages
        .on_glass()
        .find_map(|l| l.motion_since)
        .expect("the row is on the glass");
    for k in 0..8u32 {
        let at = epoch + COMET_PERIOD + COMET_PERIOD * k / 8;
        for &(cols, wid) in windows {
            capture(
                app,
                wid,
                dir,
                &format!("40-comet-{k}-{cols}"),
                at,
                Look::MOVING,
            );
        }
    }
    // The same row past `LOAD_AFTER`, laid out as above: the load words are
    // up from the first frame.
    for k in 0..8u32 {
        let at = epoch + LOAD_AFTER + COMET_PERIOD * k / 8;
        for &(cols, wid) in windows {
            capture(
                app,
                wid,
                dir,
                &format!("46-comet-steady-{k}-{cols}"),
                at,
                Look::MOVING,
            );
        }
    }
    // The first run's ETA: reads every 100 ms for 8 s with `trust`
    // extracting at 25 MB/s, so the fill's own rate latches the ETA.
    app.clear_messages_for_test();
    let start = Instant::now();
    let id = app
        .messages
        .post(
            toolchain_words::announced(FIRST_RUN),
            crate::messages_host::wall_stamp_now(),
            start,
        )
        .id;
    let mut t = start;
    for k in 0..=80u64 {
        t = start + Duration::from_millis(100 * k);
        let read = first_run_read_at(120_000_000 + 2_500_000 * k);
        if let toolchain_words::SnapshotWords::Live { message, .. } =
            toolchain_words::snapshot_words(Some(&read), None, false)
        {
            app.messages
                .restate(id, crate::messages_host::restatement_of(&message), t);
        }
    }
    app.settle_messages(t);
    for &(cols, wid) in windows {
        capture(
            app,
            wid,
            dir,
            &format!("47-first-run-eta-{cols}"),
            t,
            Look::MOVING,
        );
        capture_grounds(
            app,
            wid,
            cols,
            dir,
            "47-first-run-eta",
            t,
            Look::MOVING,
            &ROW_GROUNDS,
        );
    }
    // A determinate download at a steady 4 MB/s: 0 % is an empty track, 5 %
    // a sliver from the window's left edge, 50 % the window's middle, 95 %
    // short of its right edge and 100 % edge to edge, gutters included, each
    // with its latched ETA; then its Complete echo (wipe, glow, fade start).
    // The 0 % frame is shot once the row has waited out its grace unread.
    app.clear_messages_for_test();
    let mut download = Download {
        total: 200_000_000,
        id: None,
    };
    let start = Instant::now();
    let rate = 4_000_000;
    download.read(app, 0, start);
    let mut t = start + PROGRESS_GRACE + Duration::from_millis(100);
    download.read(app, 0, t);
    app.settle_messages(t);
    for &(cols, wid) in windows {
        capture(
            app,
            wid,
            dir,
            &format!("41-download-000-{cols}"),
            t,
            Look::MOVING,
        );
        capture_grounds(
            app,
            wid,
            cols,
            dir,
            "41-download-000",
            t,
            Look::MOVING,
            &ROW_GROUNDS,
        );
    }
    let mut done = 0;
    for permille in [50u64, 500, 950, 1000] {
        t += Duration::from_millis(100);
        (t, done) = download.run_to(app, t, done, rate, permille);
        app.settle_messages(t);
        // Past the glide by a frame of the grid the motion is read on,
        // so the frame shows the data, not the last step of the ease.
        let at = t + aterm_messages::FILL_GLIDE_MAX + aterm_messages::ANIM_FRAME;
        for &(cols, wid) in windows {
            capture(
                app,
                wid,
                dir,
                &format!("41-download-{:03}-{cols}", permille / 10),
                at,
                Look::MOVING,
            );
            capture_grounds(
                app,
                wid,
                cols,
                dir,
                &format!("41-download-{:03}", permille / 10),
                at,
                Look::MOVING,
                &ROW_GROUNDS,
            );
        }
        done += rate / 10;
    }
    let id = download.id.expect("posted");
    assert!(app.messages.resolve(id, Outcome::Ok, t));
    for (k, ms) in [100u64, 350, 550].into_iter().enumerate() {
        let at = t + Duration::from_millis(ms);
        for &(cols, wid) in windows {
            capture(
                app,
                wid,
                dir,
                &format!("42-download-complete-{k}-{cols}"),
                at,
                Look::MOVING,
            );
            if k < 2 {
                capture_grounds(
                    app,
                    wid,
                    cols,
                    dir,
                    &format!("42-download-complete-{k}"),
                    at,
                    Look::MOVING,
                    &ROW_GROUNDS,
                );
            }
        }
    }
    // The fault echo: a download failed at 40 % (the flash in the fault hue
    // over the fill, `failed` in the time slot, then the fade).
    app.clear_messages_for_test();
    let mut failing = Download {
        total: 200_000_000,
        id: None,
    };
    let (t, _) = failing.run_to(app, Instant::now(), 0, rate, 400);
    app.settle_messages(t);
    let id = failing.id.expect("posted");
    assert!(app.messages.resolve(id, Outcome::Warn, t));
    for (k, ms) in [60u64, 200, 380, 520].into_iter().enumerate() {
        let at = t + Duration::from_millis(ms);
        for &(cols, wid) in windows {
            capture(
                app,
                wid,
                dir,
                &format!("42b-download-fault-{k}-{cols}"),
                at,
                Look::MOVING,
            );
            if k < 2 {
                capture_grounds(
                    app,
                    wid,
                    cols,
                    dir,
                    &format!("42b-download-fault-{k}"),
                    at,
                    Look::MOVING,
                    &ROW_GROUNDS,
                );
            }
        }
    }
    // A stalled download: 8 s of bytes, then none for 12 s.
    app.clear_messages_for_test();
    let mut stalled = Download {
        total: 200_000_000,
        id: None,
    };
    let start = Instant::now();
    let (t, done) = stalled.run_to(app, start, 0, rate, 160);
    let at = t + Duration::from_secs(12);
    stalled.read(app, done, at);
    for &(cols, wid) in windows {
        capture(
            app,
            wid,
            dir,
            &format!("43-download-stalled-{cols}"),
            at,
            Look::MOVING,
        );
        capture_grounds(
            app,
            wid,
            cols,
            dir,
            "43-download-stalled",
            at,
            Look::MOVING,
            &ROW_GROUNDS,
        );
    }
    // The DOWNLOAD'S ETA (scene 27): 6 s of steady samples, latched, and
    // no load words.
    app.clear_messages_for_test();
    let mut eta = Download {
        total: 74_000_000,
        id: None,
    };
    let (t, _) = eta.run_to(app, Instant::now(), 0, 2_000_000, 170);
    app.settle_messages(t);
    for &(cols, wid) in windows {
        capture(
            app,
            wid,
            dir,
            &format!("27-download-eta-{cols}"),
            t,
            Look::MOVING,
        );
        capture_grounds(
            app,
            wid,
            cols,
            dir,
            "27-download-eta",
            t,
            Look::MOVING,
            &ROW_GROUNDS,
        );
    }
    // A heavy-load row: a heavy routine pass extracting, its load words up
    // from its first frame.
    app.clear_messages_for_test();
    app.apply_toolchain_snapshot(Some(&first_run_read()), false);
    let at = shot(app) + Duration::from_secs(4);
    for &(cols, wid) in windows {
        capture(
            app,
            wid,
            dir,
            &format!("44-heavy-load-{cols}"),
            at,
            Look::MOVING,
        );
        capture_grounds(
            app,
            wid,
            cols,
            dir,
            "44-heavy-load",
            at,
            Look::MOVING,
            &ROW_GROUNDS,
        );
    }
    // Reduced motion: work with no fraction is its unlit track (no comet,
    // no spinner), the bar stands at its data, and the band a second later
    // is the same frame.
    app.clear_messages_for_test();
    app.announce_toolchain_pass(FIRST_RUN, true);
    app.post_message(crate::message_reporters::gpu_lost());
    let mut still = Download {
        total: 200_000_000,
        id: None,
    };
    let (t, _) = still.run_to(app, Instant::now(), 0, rate, 570);
    app.settle_messages(t);
    for &(cols, wid) in windows {
        capture(
            app,
            wid,
            dir,
            &format!("29-still-forms-{cols}"),
            t,
            Look::STILL,
        );
        capture_grounds(
            app,
            wid,
            cols,
            dir,
            "29-still-forms",
            t,
            Look::STILL,
            &ROW_GROUNDS,
        );
        capture(
            app,
            wid,
            dir,
            &format!("45-reduced-motion-{cols}"),
            t + Duration::from_secs(1),
            Look::STILL,
        );
    }
    // High contrast: palette inks only, flat tones with hard cell edges
    // (`Look::graded` false), for the moving comet and the bar.
    for (name, palette) in crate::chrome_band::hc_fixtures::STOCK.into_iter().take(2) {
        let slug = name.to_lowercase().replace(' ', "-");
        crate::chrome_band::hc_fixtures::with_forced(palette, || {
            for &(cols, wid) in windows {
                capture(
                    app,
                    wid,
                    dir,
                    &format!("30-high-contrast-{slug}-{cols}"),
                    t,
                    FLAT,
                );
            }
        });
    }
    // Light themes: GitHub Light and Solarized Light.
    for scheme_name in ["GitHub Light", "Solarized Light"] {
        let theme = scheme(scheme_name);
        let slug = scheme_name.to_lowercase().replace(' ', "-");
        for &(cols, wid) in windows {
            capture_in(
                app,
                wid,
                dir,
                &format!("31-light-{slug}-{cols}"),
                t,
                Look::MOVING,
                theme,
            );
        }
    }
}

/// Full-width meter scenes at every capture width: a Warn row whose fill
/// wears `warn`, and a Primary, a Secondary and `Details ›` over the fill
/// (95 %) and the track (50 %), resting and lit. Paint fixtures are posted
/// straight into the center, since the painter is under test.
fn merged_design_scenes(app: &mut App, dir: &std::path::Path, windows: &[(&str, WindowId)]) {
    use aterm_messages::{Hold, Intent, Message, Meter, Severity, tags};
    let live = Hold::Live {
        stale_after: Duration::from_secs(120),
    };
    let post = |app: &mut App, msg: Message| {
        app.messages
            .post(msg, crate::messages_host::wall_stamp_now(), Instant::now())
            .id
    };
    // A Warn row with a fill: the fill is `warn`, and every word on it is
    // floored against the tone under it.
    app.clear_messages_for_test();
    post(
        app,
        Message::new(tags::UPDATE, Severity::Warn, "Retrying aterm v0.92.0")
            .no_excerpt()
            .meter(Meter {
                fill_permille: Some(620),
                stats: "46 MB / 74 MB".into(),
                ..Meter::default()
            })
            .hold(live)
            .action(Intent::OpenSettings {
                route: "/updates".into(),
            }),
    );
    let at = shot(app);
    for &(cols, wid) in windows {
        capture(
            app,
            wid,
            dir,
            &format!("50-warn-fill-{cols}"),
            at,
            Look::MOVING,
        );
    }
    // Chips over the meter: a Primary (`New window`), a Secondary (`Open
    // log`) and `Details ›`, over the fill at 95 %, over the track at 50 %,
    // then each lit under the pointer at 95 %.
    for (permille, count) in [(950u16, "19 of 20 tabs"), (500, "2 of 4 tabs")] {
        app.clear_messages_for_test();
        post(
            app,
            Message::new(tags::SESSION, Severity::Info, "Saving the session")
                .no_excerpt()
                .meter(Meter {
                    fill_permille: Some(permille),
                    stats: count.into(),
                    ..Meter::default()
                })
                .hold(live)
                .action(Intent::NewWindow)
                .action(Intent::OpenPath {
                    path: "/Users//ana/Library/Logs/aterm/aterm.log".into(),
                }),
        );
        let at = shot(app);
        for &(cols, wid) in windows {
            capture(
                app,
                wid,
                dir,
                &format!("51-chips-{:02}-{cols}", permille / 10),
                at,
                Look::MOVING,
            );
            if permille == 950 {
                for (tag, k) in [("primary", 0), ("secondary", 1)] {
                    app.windows.get_mut(&wid).unwrap().band_hover = Some(BandHover {
                        row: 0,
                        target: HoverTarget::Capsule(ActionIndex(k)),
                    });
                    capture(
                        app,
                        wid,
                        dir,
                        &format!("51-chips-95-lit-{tag}-{cols}"),
                        at,
                        Look::MOVING,
                    );
                    app.windows.get_mut(&wid).unwrap().band_hover = None;
                }
            }
            // …and on the light themes (ruling 155): the chips over the
            // fill and over the track, the Primary lit over the fill.
            for scheme_name in LIGHT_SCHEMES {
                let theme = scheme(scheme_name);
                let slug = scheme_name.to_lowercase().replace(' ', "-");
                let name = format!("52-chips-{slug}-{:02}", permille / 10);
                capture_in(
                    app,
                    wid,
                    dir,
                    &format!("{name}-{cols}"),
                    at,
                    Look::MOVING,
                    theme,
                );
                if permille == 950 {
                    app.windows.get_mut(&wid).unwrap().band_hover = Some(BandHover {
                        row: 0,
                        target: HoverTarget::Capsule(ActionIndex(0)),
                    });
                    capture_in(
                        app,
                        wid,
                        dir,
                        &format!("{name}-lit-{cols}"),
                        at,
                        Look::MOVING,
                        theme,
                    );
                    app.windows.get_mut(&wid).unwrap().band_hover = None;
                }
            }
        }
    }
    // The comet on other grounds: the busy row in steady state on a dark
    // theme with a cool accent and on the light themes; the tail must fall
    // off smoothly with no striping.
    app.clear_messages_for_test();
    app.announce_toolchain_pass(FIRST_RUN, true);
    let epoch = app
        .messages
        .on_glass()
        .find_map(|l| l.motion_since)
        .expect("the row is on the glass");
    for scheme_name in THEMED_SCHEMES {
        let theme = scheme(scheme_name);
        let slug = scheme_name.to_lowercase().replace(' ', "-");
        for k in [0u32, 3, 6] {
            let at = epoch + LOAD_AFTER + COMET_PERIOD * k / 8;
            for &(cols, wid) in windows {
                capture_in(
                    app,
                    wid,
                    dir,
                    &format!("53-comet-{slug}-{k}-{cols}"),
                    at,
                    Look::MOVING,
                    theme,
                );
            }
        }
    }
    // Chips over the comet: a busy row with a Primary (accent ring; the comet
    // is the only solid accent) and a Secondary, at four phases of the sweep
    // (k = 0: head under the ring), on the default ground, Tokyo Night and
    // the light themes.
    app.clear_messages_for_test();
    post(
        app,
        Message::new(tags::SESSION, Severity::Info, "Saving the session")
            .no_excerpt()
            .meter(Meter::busy("3 of 4 tabs"))
            .hold(live)
            .action(Intent::NewWindow)
            .action(Intent::OpenPath {
                path: "/Users//ana/Library/Logs/aterm/aterm.log".into(),
            }),
    );
    shot(app);
    let epoch = app
        .messages
        .on_glass()
        .find_map(|l| l.motion_since)
        .expect("the busy row is on the glass");
    for k in [0u32, 1, 5, 7] {
        let at = epoch + LOAD_AFTER + COMET_PERIOD * k / 8;
        for &(cols, wid) in windows {
            capture(
                app,
                wid,
                dir,
                &format!("54-chips-busy-{k}-{cols}"),
                at,
                Look::MOVING,
            );
            for scheme_name in THEMED_SCHEMES {
                let theme = scheme(scheme_name);
                let slug = scheme_name.to_lowercase().replace(' ', "-");
                capture_in(
                    app,
                    wid,
                    dir,
                    &format!("54-chips-busy-{slug}-{k}-{cols}"),
                    at,
                    Look::MOVING,
                    theme,
                );
            }
        }
    }
}

/// The light builtin schemes the chips and the comet are shot on.
const LIGHT_SCHEMES: [&str; 3] = ["GitHub Light", "Solarized Light", "Catppuccin Latte"];

/// Every non-default ground the motion is shot on: a dark theme with a
/// cool accent, then the light ones.
const THEMED_SCHEMES: [&str; 4] = [
    "Tokyo Night",
    "GitHub Light",
    "Solarized Light",
    "Catppuccin Latte",
];

/// THE MOTION FILMSTRIPS (design §10.12): for each motion form, the band
/// row at 32 injected instants stacked into one PNG — the comet's sweep,
/// the glint's travel, the Complete echo, the Fault echo, a busy row's
/// Fault echo and a glide — at 80 columns in a window with gutters that
/// is not a whole number of cells wide ([`glass_window`]), into
/// `<dir>/filmstrips`; then the comet, the glint and both echoes on Tokyo
/// Night and the light themes (rulings 155–158). Ignored by default, like the
/// frames.
#[test]
#[ignore = "visual capture: needs a system font; run with --ignored"]
fn message_band_motion_filmstrips() {
    let dir = std::env::var("STATUS_BARS_PNG_DIR")
        .map_or_else(
            |_| std::env::temp_dir().join("status-bars"),
            std::path::PathBuf::from,
        )
        .join("filmstrips");
    std::fs::create_dir_all(&dir).expect("output dir");
    let mut app = App::headless_for_test();
    // A window that is not a whole number of cells wide, with gutters:
    // the comet enters through the left gutter and leaves through the
    // right, and the echo's wipe reaches the window's edge.
    let wid = glass_window(&mut app, 1, 80, 10, 7);
    content(&app, wid);
    let strip_in =
        |app: &mut App, name: &str, instants: &[Instant], theme: aterm_render::Theme| -> bool {
            let mut stacked: Vec<u32> = Vec::new();
            let mut width = 0;
            let mut row_h = 0;
            for &at in instants {
                let Some(frame) = render(app, wid, at, Look::MOVING, theme) else {
                    return false;
                };
                width = frame.width;
                row_h = frame.cell_h;
                let y0 = frame.band_y;
                stacked.extend_from_slice(&frame.pixels[y0 * width..(y0 + row_h) * width]);
            }
            let path = dir.join(format!("{name}.png"));
            write_png(&path, width, row_h * instants.len(), &stacked);
            crate::logging::stderr_line!("wrote {}", path.display());
            true
        };
    let strip = |app: &mut App, name: &str, instants: &[Instant]| -> bool {
        strip_in(app, name, instants, aterm_render::Theme::default())
    };
    let across = |from: Instant, span: Duration| -> Vec<Instant> {
        (0..32u32).map(|k| from + span * k / 32).collect()
    };
    // The comet's sweep, one period.
    app.clear_messages_for_test();
    app.announce_toolchain_pass(FIRST_RUN, true);
    let epoch = app
        .messages
        .on_glass()
        .find_map(|l| l.motion_since)
        .expect("on glass");
    if !strip(&mut app, "comet", &across(epoch, COMET_PERIOD)) {
        crate::logging::stderr_line!("no system font \u{2014} filmstrips skipped");
        return;
    }
    // …and in steady state: two crossings after the first, the drain of
    // each head overlapping the ignition of the next.
    strip(
        &mut app,
        "comet-steady",
        &across(epoch + COMET_PERIOD, COMET_PERIOD * 2),
    );
    for scheme_name in THEMED_SCHEMES {
        let slug = scheme_name.to_lowercase().replace(' ', "-");
        strip_in(
            &mut app,
            &format!("comet-steady-{slug}"),
            &across(epoch + COMET_PERIOD, COMET_PERIOD * 2),
            scheme(scheme_name),
        );
    }
    // A BUSY row's Fault echo: the flash over the comet's track.
    let failed_at = epoch + COMET_PERIOD + COMET_PERIOD / 3;
    let busy = app
        .messages
        .on_glass()
        .next()
        .map(|l| l.id)
        .expect("on glass");
    assert!(app.messages.resolve(busy, Outcome::Warn, failed_at));
    strip(
        &mut app,
        "echo-fault-busy",
        &across(
            failed_at - Duration::from_millis(100),
            Duration::from_millis(800),
        ),
    );
    // The glint's travel over a bar at 57 %, then a glide to 80 %.
    app.clear_messages_for_test();
    let mut download = Download {
        total: 200_000_000,
        id: None,
    };
    let (t, _) = download.run_to(&mut app, Instant::now(), 0, 4_000_000, 570);
    app.settle_messages(t);
    let epoch = app
        .messages
        .on_glass()
        .find_map(|l| l.motion_since)
        .expect("on glass");
    let glint_from = t.max(epoch + aterm_messages::GLINT_DELAY);
    strip(
        &mut app,
        "glint",
        &across(glint_from, aterm_messages::GLINT_TRAVEL),
    );
    let jump = glint_from + aterm_messages::GLINT_PERIOD;
    download.read(&mut app, 160_000_000, jump);
    strip(
        &mut app,
        "glide",
        &across(
            jump,
            aterm_messages::animate::glide_span(570, 800) + aterm_messages::ANIM_FRAME,
        ),
    );
    // The Complete echo, and the Fault echo of the same row.
    let resolved = jump + Duration::from_secs(1);
    let id = download.id.expect("posted");
    assert!(app.messages.resolve(id, Outcome::Ok, resolved));
    // The wipe (its glide from 80 %), the one sweep, the fade (ruling 244).
    strip(
        &mut app,
        "echo-complete",
        &across(resolved, Duration::from_millis(1400)),
    );
    app.clear_messages_for_test();
    let mut failing = Download {
        total: 200_000_000,
        id: None,
    };
    let (t, _) = failing.run_to(&mut app, Instant::now(), 0, 4_000_000, 400);
    app.settle_messages(t);
    let id = failing.id.expect("posted");
    assert!(app.messages.resolve(id, Outcome::Warn, t));
    strip(
        &mut app,
        "echo-fault",
        &across(t, Duration::from_millis(700)),
    );
    // Both echoes on the other grounds. A strip settles the center as it
    // goes and an echo retires at its end, so each ground gets its own
    // row: a download to 80 % that completes, and one to 40 % that fails.
    for scheme_name in THEMED_SCHEMES {
        let slug = scheme_name.to_lowercase().replace(' ', "-");
        // The glint's travel over a bar at 57 %: the words it passes keep
        // their side and their contrast.
        app.clear_messages_for_test();
        let mut glinting = Download {
            total: 200_000_000,
            id: None,
        };
        let (t, _) = glinting.run_to(&mut app, Instant::now(), 0, 4_000_000, 570);
        app.settle_messages(t);
        let epoch = app
            .messages
            .on_glass()
            .find_map(|l| l.motion_since)
            .expect("on glass");
        strip_in(
            &mut app,
            &format!("glint-{slug}"),
            &across(
                t.max(epoch + aterm_messages::GLINT_DELAY),
                aterm_messages::GLINT_TRAVEL,
            ),
            scheme(scheme_name),
        );
        for (outcome, permille, name, span) in [
            (Outcome::Ok, 800, "echo-complete", 1400),
            (Outcome::Warn, 400, "echo-fault", 700),
        ] {
            app.clear_messages_for_test();
            let mut row = Download {
                total: 200_000_000,
                id: None,
            };
            let (t, _) = row.run_to(&mut app, Instant::now(), 0, 4_000_000, permille);
            let at = t + aterm_messages::FILL_GLIDE_MAX + aterm_messages::ANIM_FRAME;
            app.settle_messages(at);
            assert!(app.messages.resolve(row.id.expect("posted"), outcome, at));
            strip_in(
                &mut app,
                &format!("{name}-{slug}"),
                &across(at, Duration::from_millis(span)),
                scheme(scheme_name),
            );
        }
    }
    // THE STRAIN GAUGE (ruling 208): a level that rises from 40 % to 90 %
    // between two readings — the glide, and nothing moving once it lands
    // — on the default ground and a light one.
    use aterm_messages::Severity;
    use aterm_messages::strain::{Culprit, StrainKind};
    let tab2 = Culprit::Session {
        program: "yes".into(),
        tab: 2,
        elsewhere: false,
        receiving_keys: false,
        window: wid.0,
    };
    for (slug, theme) in [
        ("default", aterm_render::Theme::default()),
        ("github-light", scheme("GitHub Light")),
    ] {
        app.clear_messages_for_test();
        let t0 = Instant::now();
        let id = app
            .messages
            .post(
                strain_row(
                    StrainKind::Cpu,
                    &tab2,
                    400,
                    "3.2 of 8 cores",
                    Severity::Info,
                ),
                crate::messages_host::wall_stamp_now(),
                t0,
            )
            .id;
        let rise = t0 + Duration::from_secs(4);
        app.settle_messages(rise);
        app.messages.restate(
            id,
            crate::messages_host::restatement_of(&strain_row(
                StrainKind::Cpu,
                &tab2,
                900,
                "7 of 8 cores",
                Severity::Info,
            )),
            rise,
        );
        strip_in(
            &mut app,
            &format!("strain-gauge-rise-{slug}"),
            &across(
                rise - aterm_messages::ANIM_FRAME * 2,
                aterm_messages::FILL_GLIDE * 2,
            ),
            theme,
        );
    }
}

// -----------------------------------------------------------------------
// The web band's golden: this host's band, frame by frame, for the web
// module to be held to.
// -----------------------------------------------------------------------

/// The golden the web module's `the_web_band_is_the_native_golden` reads.
const WEB_GOLDEN: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../aterm-wasm/tests/fixtures/band_native.tsv"
);

/// The face both hosts draw the golden with: the bundled JetBrains Mono the
/// web page ships, at the capture size.
const WEB_FONT: &[u8] =
    include_bytes!("../../aterm-render/assets/bundled/JetBrainsMono-Regular.ttf");

/// The frame's side and top padding, px (the web page's `set_chrome(6, 0)`).
const WEB_PAD: usize = 6;

/// One golden scene: its name, its `notice` lines (ms, line), and its shots.
type WebScene = (&'static str, &'static [(u64, &'static str)], &'static [u64]);

/// Each golden scene: its name, the `notice` lines at their offsets (ms from
/// the center's birth), and the instants it is shot at. Lines go through
/// the engine's wire path on both hosts, and every shot is also a settle
/// tick (a progress row reaches the glass at its grace's end only if a tick
/// runs then, so finishes are shot at 2.5 s first).
const WEB_SCENES: &[WebScene] = &[
    ("record", &[(0, "post ci Build finished")], &[100]),
    (
        "warn",
        &[(0, "post ci sev=warn Disk nearly full -- 2 GB left on /")],
        &[100],
    ),
    (
        "error",
        &[(0, "post ci sev=error Deploy failed -- exit 1")],
        &[100],
    ),
    (
        "bar0",
        &[(0, "progress p pct=0 Downloading assets")],
        &[2500],
    ),
    (
        "bar25",
        &[(0, "progress p pct=25 Downloading assets")],
        &[2500],
    ),
    (
        "bar50",
        &[(0, "progress p pct=50 Downloading assets")],
        &[2500],
    ),
    (
        "bar999",
        &[(0, "progress p pct=99.9 Downloading assets")],
        &[2500],
    ),
    (
        "bar100",
        &[(0, "progress p pct=100 Downloading assets")],
        &[2500],
    ),
    (
        "amount",
        &[
            (0, "progress f done=3/10 Copying files"),
            (1000, "progress f done=5/10 Copying files"),
            (2000, "progress f done=7/10 Copying files"),
        ],
        &[2600, 3000],
    ),
    (
        "busy",
        &[(0, "progress b busy Indexing the tree")],
        &[2600, 3400, 4700],
    ),
    (
        "stall",
        &[(0, "progress s pct=40 Uploading logs")],
        &[2500, 13_000],
    ),
    (
        "complete",
        &[(0, "progress p pct=50 Building"), (3000, "done p ok Built")],
        &[2500, 3100, 3400],
    ),
    (
        "fault",
        &[
            (0, "progress p pct=50 Uploading"),
            (3000, "done p warn Upload failed"),
        ],
        &[2500, 3100, 3300],
    ),
    (
        "vanish",
        &[(0, "progress p pct=50 Syncing"), (3000, "done p withdraw")],
        &[2500, 3050, 3200],
    ),
    (
        "stack",
        &[
            (0, "post ci sev=warn Disk nearly full"),
            (10, "post ci sev=error Deploy failed"),
            (20, "progress p pct=60 Downloading assets"),
        ],
        &[2600],
    ),
    (
        "shrink",
        &[
            (0, "post ci sev=warn Disk nearly full"),
            (10, "post ci sev=error Deploy failed"),
            (1000, "dismiss 2"),
        ],
        &[900, 1200, 2700],
    ),
];

/// FNV-1a-64's offset basis.
const WEB_FNV_BASIS: u64 = 0xcbf2_9ce4_8422_2325;

/// FNV-1a-64 — the golden's digest (the web reader computes the same).
fn web_fnv(bytes: &[u8]) -> u64 {
    web_fnv_more(WEB_FNV_BASIS, bytes)
}

/// [`web_fnv`]'s state `h` carried on over `bytes`.
fn web_fnv_more(mut h: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// [`web_fnv`] of CPU-frame pixels (`0xAARRGGBB`, `AA` the inverted alpha)
/// read as the RGBA bytes the golden digests: `R, G, B, 0xff - AA` each.
///
/// STRAIGHT FROM THE FRAME (2026-10-05, gate speed). Until then every frame
/// was first copied whole into an RGBA `Vec<u8>` through a per-pixel
/// `flat_map`, though a line reads only the rows above the grid and the
/// band's: that copy was 89% of the test's samples (7,729 of 8,692), and
/// the test took 14.9 s of CPU run alone, 2.3 s without it. The bytes
/// hashed, and their order, are the copy's, so every digest is the
/// committed golden's.
fn web_fnv_pixels(pixels: &[u32]) -> u64 {
    pixels.iter().fold(WEB_FNV_BASIS, |h, &p| {
        web_fnv_more(
            h,
            &[
                (p >> 16) as u8,
                (p >> 8) as u8,
                p as u8,
                0xff - (p >> 24) as u8,
            ],
        )
    })
}

/// One golden frame line: the band's rows (cells, raster, pixels each), the
/// bleed, and the pixels above the grid, from the composed frame `input`,
/// its CPU-frame `pixels` (`w` px wide), `grid_top` and `cell_h`.
#[allow(
    clippy::too_many_arguments,
    reason = "a golden line is a pure function of exactly these inputs"
)]
fn web_golden_line(
    key: &str,
    n: usize,
    input: &aterm_core::render::RenderInput,
    bleed: Option<&aterm_render::ChromeBleed>,
    pixels: &[u32],
    w: usize,
    grid_top: usize,
    cell_h: usize,
) -> String {
    use std::fmt::Write as _;
    let px = |y0: usize, y1: usize| web_fnv_pixels(&pixels[y0 * w..y1 * w]);
    let mut line = format!(
        "frame\t{key}\tn={n}\tw={w}\ttop={:016x}\tbleed={:016x}",
        px(0, grid_top),
        web_fnv(format!("{bleed:?}").as_bytes())
    );
    let mut words = Vec::new();
    for r in 0..n {
        let cells = web_fnv(format!("{:?}", input.cells[r]).as_bytes());
        let rasters: Vec<_> = input
            .chrome_rasters
            .iter()
            .filter(|m| usize::from(m.row) == r)
            .collect();
        let raster = web_fnv(format!("{rasters:?}").as_bytes());
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

/// The renderer both hosts draw the golden with: the bundled face on the
/// portable rasterizer.
fn web_portable(theme: aterm_render::Theme) -> aterm_render::Renderer {
    let mut r =
        aterm_render::Renderer::from_bytes(WEB_FONT, CAPTURE_PX, theme).expect("bundled font");
    // The web module has one rasterizer, the portable one; this host's
    // default (CoreText on macOS, hinted on Linux and Windows) is
    // pinned to it, and neither side discovers system faces.
    r.debug_force_fontdue();
    r.set_runtime_font_discovery(false);
    r
}

/// This host's golden, as text: the themes, the scenes and every frame.
///
/// ONE THREAD PER THEME (2026-09-30, gate speed). Each theme's frames are
/// drawn on an App of their own ([`web_golden_theme_frames`]) and the
/// blocks are joined in the golden's theme order, so the text is the one
/// the single pass wrote, line for line — the golden it is compared with
/// says so on every run. The single pass drew the four themes one after
/// another on one thread: 15.7 s of CPU, run alone, the fifth-longest test
/// of the gate's `-p aterm-gui --lib` binary; on four, 5.1 s alone and 3.9 s
/// in that binary at `RUST_TEST_THREADS=7` (measured 2026-09-30).
fn web_golden_text() -> String {
    use std::fmt::Write as _;
    let scheme = |name: &str| aterm_types::scheme::builtin(name).expect("builtin scheme");
    let default_palette = aterm_types::ColorPalette::new();
    let rgb = |c: aterm_types::Rgb| (u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b);
    let mut themes: Vec<(&str, aterm_render::Theme, aterm_types::ColorPalette)> = vec![(
        "dark",
        aterm_render::Theme::default(),
        default_palette.clone(),
    )];
    for name in ["GitHub Light", "Catppuccin Latte", "Solarized Light"] {
        let s = scheme(name);
        let t = s.to_theme_parts();
        themes.push((
            name,
            aterm_render::Theme {
                fg: t.fg,
                bg: t.bg,
                cursor: t.cursor,
                selection: t.selection,
            },
            s.to_color_palette(),
        ));
    }
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# band_native.tsv — THIS HOST's message band, frame by frame (design ruling 333 of\n\
         # docs/DESIGN-unified-messages-2026-09-21.md). Written by aterm-gui's\n\
         # `message_band_visual_tests::the_web_band_golden_is_this_hosts_band` (which also\n\
         # asserts it; rewrite with ATERM_BAND_GOLDEN=write) and read by aterm-wasm's\n\
         # `messages_api::tests::the_web_band_is_the_native_golden`, which replays the scenes\n\
         # through the web module and must land on every digest. Digests are FNV-1a-64 of each\n\
         # band row's RenderCells (Debug), its ChromeRasters (Debug) and its RGBA pixel rows,\n\
         # the ChromeBleed (Debug) and the pixels above the grid. Both hosts draw with the\n\
         # bundled JetBrains Mono at 20 px on the portable rasterizer, pad 6, links withheld.\n\
         # The colour arithmetic runs on THIS host's libm (macOS libSystem here): on another\n\
         # libm both tests would flag the same last-place difference together (ruling 326 (g)),\n\
         # which is a platform fact, not a web defect."
    );
    let _ = writeln!(
        out,
        "font\tJetBrainsMono-Regular.ttf\t{CAPTURE_PX}\tpad\t{WEB_PAD}"
    );
    for (name, t, pal) in &themes {
        let _ = writeln!(
            out,
            "theme\t{name}\t{:06x}\t{:06x}\t{:06x}\t{:06x}\t{:06x}\t{:06x}",
            t.fg,
            t.bg,
            t.cursor,
            t.selection,
            rgb(pal.get(4)),
            rgb(pal.get(6))
        );
    }
    for (name, lines, shots) in WEB_SCENES {
        for (at, line) in *lines {
            let _ = writeln!(out, "line\t{name}\t{at}\t{line}");
        }
        for at in *shots {
            let _ = writeln!(out, "shot\t{name}\t{at}");
        }
    }
    let blocks: Vec<String> = std::thread::scope(|scope| {
        let jobs: Vec<_> = themes
            .iter()
            .map(|(tname, theme, palette)| {
                scope.spawn(move || web_golden_theme_frames(tname, *theme, palette))
            })
            .collect();
        jobs.into_iter()
            .map(|job| {
                job.join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
            .collect()
    });
    for block in blocks {
        out.push_str(&block);
    }
    out
}

/// One theme's frames of the golden, in the golden's order — every width,
/// look, scene and shot — drawn on a headless App of their own that lays
/// the band as the web does (links withheld).
#[allow(clippy::too_many_lines, reason = "one pass over the golden's matrix")]
fn web_golden_theme_frames(
    tname: &str,
    theme: aterm_render::Theme,
    palette: &aterm_types::ColorPalette,
) -> String {
    use aterm_messages::wire::{self, NoticeRequest, Paint, WireGate};
    use aterm_messages::{MessageCenter, MessageLog, WallStamp};
    use std::fmt::Write as _;
    let mut app = App::headless_for_test();
    app.backend = crate::BackendSlot::Ready(crate::Backend::Cpu(web_portable(
        aterm_render::Theme::default(),
    )));
    app.band_lay = |cols| aterm_messages::drive::Lay::web(cols, false);
    let mut out = String::new();
    let stamp = WallStamp {
        unix_ms: 1_790_000_000_000,
    };
    let looks = [
        ("moving", Look::MOVING),
        (
            "still",
            Look {
                pace: Pace::Still,
                graded: true,
            },
        ),
    ];
    let windows: Vec<(u16, WindowId)> = [60u16, 80, 120]
        .into_iter()
        .zip(1u64..)
        .map(|(cols, session)| (cols, glass_window(&mut app, session, cols, WEB_PAD, 0)))
        .collect();
    let mut tc = app
        .session_factory
        .terminal_config
        .clone()
        .unwrap_or_default();
    tc.custom_palette = (tname != "dark").then(|| palette.clone());
    app.session_factory.terminal_config = Some(tc);
    for &(cols, wid) in &windows {
        app.frontmost_window = Some(wid);
        let mut cpu = web_portable(theme);
        cpu.set_pad(WEB_PAD);
        assert_eq!(
            app.win_cell_size(wid),
            cpu.cell_size(),
            "both hosts lay the band out in the same cells"
        );
        let terminal = app.front_terminal(wid).expect("front").term.clone();
        {
            let mut term = term_lock(&terminal);
            let rgb = |c: u32| aterm_core::terminal::Rgb {
                r: (c >> 16) as u8,
                g: (c >> 8) as u8,
                b: c as u8,
            };
            term.set_default_foreground(rgb(theme.fg));
            term.set_default_background(rgb(theme.bg));
            term.set_default_cursor_color(Some(rgb(theme.cursor)));
            term.set_default_selection_background(Some(rgb(theme.selection)));
        }
        for (lname, look) in looks {
            for (scene, lines, shots) in WEB_SCENES {
                let t0 = Instant::now();
                app.messages = MessageCenter::new(MessageLog::default(), t0);
                app.wire_gate = WireGate::default();
                let _ = app.settle_messages(t0);
                let mut events: Vec<(u64, Option<&str>)> = lines
                    .iter()
                    .map(|&(at, l)| (at, Some(l)))
                    .chain(shots.iter().map(|&at| (at, None)))
                    .collect();
                events.sort_by_key(|&(at, l)| (at, l.is_none()));
                for (at, line) in events {
                    let now = t0 + Duration::from_millis(at);
                    if let Some(line) = line {
                        // `App::take_notice` at an injected instant.
                        let req = NoticeRequest::parse(line).expect("a golden line parses");
                        let applied =
                            wire::apply(&mut app.messages, &mut app.wire_gate, req, stamp, now);
                        if applied.paint == Paint::Now {
                            let _ = app.settle_messages(now);
                        }
                        continue;
                    }
                    let (rows, cols_n) = {
                        let ws = &app.windows[&wid];
                        (ws.rows as usize, ws.cols as usize)
                    };
                    {
                        let ws = app.windows.get_mut(&wid).expect("window");
                        term_lock(&terminal).cell_frame_into(&mut ws.input_scratch, rows, cols_n);
                        ws.band.invalidate();
                    }
                    let _ = app.settle_messages(now);
                    app.prepare_band_motion_with(wid, now, look);
                    app.splice_message_band(wid, theme);
                    let n = usize::from(app.message_band_rows);
                    let inks = app.band_palette(theme).colors();
                    let ws = &app.windows[&wid];
                    let bleed = (n > 0).then(|| {
                        aterm_render::band::band_bleed(
                            &inks,
                            0,
                            n,
                            aterm_render::band::band_row_edges(0, n, &ws.cached_band_edges),
                        )
                    });
                    cpu.set_chrome_bleed(bleed);
                    let frame = cpu.render_input(&ws.input_scratch);
                    let key = format!("{scene}\t{tname}\t{cols}\t{lname}\t{at}");
                    let _ = writeln!(
                        out,
                        "{}",
                        web_golden_line(
                            &key,
                            n,
                            &ws.input_scratch,
                            bleed.as_ref(),
                            &frame.pixels,
                            frame.width,
                            cpu.grid_top(),
                            cpu.cell_size().1,
                        )
                    );
                }
            }
        }
    }
    out
}

/// The web band's golden digests are this host's band: every scene, theme,
/// width and look, which the web module's parity test must match. Rewrite
/// with `ATERM_BAND_GOLDEN=write` after a deliberate band change.
#[test]
fn the_web_band_golden_is_this_hosts_band() {
    let text = web_golden_text();
    let path = std::path::Path::new(WEB_GOLDEN);
    if std::env::var("ATERM_BAND_GOLDEN").is_ok_and(|v| v == "write") {
        std::fs::create_dir_all(path.parent().expect("fixtures dir")).expect("fixtures dir");
        std::fs::write(path, &text).expect("write the golden");
        return;
    }
    let want = std::fs::read_to_string(path).expect("the golden (ATERM_BAND_GOLDEN=write)");
    let frames = text.lines().filter(|l| l.starts_with("frame\t")).count();
    assert!(
        frames > 500,
        "the golden covers the matrix: {frames} frames"
    );
    for (i, (got, want)) in text.lines().zip(want.lines()).enumerate() {
        assert_eq!(got, want, "golden line {}", i + 1);
    }
    assert_eq!(text.lines().count(), want.lines().count(), "golden length");
}
