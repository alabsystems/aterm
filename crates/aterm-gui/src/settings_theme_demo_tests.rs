// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::settings_preview::{DemoFrame, DemoStripLayout, PreviewPause, SettingsPreviewSpec};
use crate::widget::{DrawPrim, TextFace};
use aterm_core::config::TerminalConfig;
use aterm_core::grid::cell_flags::CellFlags;
use aterm_core::terminal::{ColorBaseline, Terminal};
use aterm_types::Rgb;

fn colors(scheme: &str) -> DemoColors {
    floored(scheme, 1.0)
}

/// `scheme`'s demo colours under the theme-pair floor `floor`.
fn floored(scheme: &str, floor: f32) -> DemoColors {
    let scheme = aterm_types::scheme::builtin(scheme).expect("built-in scheme");
    DemoColors::new(
        PreviewTerminalTheme::from_scheme(&scheme),
        None,
        true,
        floor,
    )
}

/// Both sessions, each with the marker of the one event that alone inks
/// slot 14 (bright cyan).
const SESSIONS: [(DemoSession, &[u8]); 2] = [
    (DemoSession::PowerShell, b"help"),
    (DemoSession::Zsh, b"link@"),
];

/// The visible rows a whole `tape` leaves on a grid wide and tall enough that
/// nothing wraps or scrolls away.
fn tape_text(tape: &[DemoEvent]) -> Vec<String> {
    let mut term = Terminal::new(64, 200);
    for event in tape {
        term.process(event.bytes);
    }
    term.cell_frame(64, 200)
        .cells
        .iter()
        .map(|row| {
            row.iter()
                .map(|cell| cell.ch)
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .filter(|row| !row.is_empty())
        .collect()
}

/// Forget this thread's tape, so the next frame is built cold.
fn cold() {
    TAPE.with_borrow_mut(|slot| *slot = None);
}

fn palette_with_slot4(color: Rgb) -> TerminalConfig {
    let mut palette = aterm_types::ColorPalette::new();
    palette.set(4, color);
    TerminalConfig {
        custom_palette: Some(palette),
        ..TerminalConfig::default()
    }
}

/// The spike the incremental tape rests on: a cell parsed under one palette
/// draws the new palette's colour once the baseline is rebased, so a theme
/// change never has to replay the tape.
#[test]
fn rebase_recolours_parsed_palette_cells() {
    let before = Rgb::new(0x11, 0x22, 0xcc);
    let after = Rgb::new(0xee, 0x55, 0x00);
    let run = |rebase: bool| {
        let mut term = Terminal::new(2, 8);
        let _ = term.apply_config(&palette_with_slot4(before));
        term.process(b"\x1b[34mX");
        if rebase {
            let _ = term.rebase_colors(&ColorBaseline::of_config(&palette_with_slot4(after)));
        }
        term.cell_frame(2, 8).cells[0][0].fg
    };
    assert_eq!(
        run(true),
        [after.r, after.g, after.b],
        "rebased cells recolour"
    );
    assert_eq!(
        run(false),
        [before.r, before.g, before.b],
        "without the rebase the cell keeps the colour it was parsed under"
    );
}

/// What a tape inks across a whole loop, on a grid tall enough that nothing
/// scrolls away before it is read.
#[derive(Debug, Default)]
struct Inked {
    slots: [bool; 16],
    bold: bool,
    faint: bool,
    italic: bool,
    underline: bool,
    link: bool,
}

impl Inked {
    fn of(tape: &[DemoEvent]) -> Self {
        let (rows, cols) = (64_u16, 120_u16);
        let mut term = Terminal::new(rows, cols);
        let _ = term.apply_config(&colors("Dracula").config());
        let mut inked = Self::default();
        for event in tape {
            term.process(event.bytes);
            for row in 0..rows {
                for col in 0..cols {
                    let Some(cell) = term.grid().cell(row, col) else {
                        continue;
                    };
                    let colors = cell.colors();
                    if colors.fg_is_indexed() && colors.fg_index() < 16 {
                        inked.slots[usize::from(colors.fg_index())] = true;
                    }
                    if colors.bg_is_indexed() && colors.bg_index() < 16 {
                        inked.slots[usize::from(colors.bg_index())] = true;
                    }
                    let flags = cell.flags();
                    inked.bold |= flags.contains(CellFlags::BOLD);
                    inked.faint |= flags.contains(CellFlags::DIM);
                    inked.italic |= flags.contains(CellFlags::ITALIC);
                    inked.underline |= flags.contains(CellFlags::UNDERLINE);
                    inked.link |= term.hyperlink_at(row, col).is_some();
                }
            }
        }
        inked
    }

    /// What the tape never inks, by name.
    fn missing(&self) -> Vec<String> {
        let mut missing = self
            .slots
            .iter()
            .enumerate()
            .filter(|(_, inked)| !**inked)
            .map(|(slot, _)| format!("slot {slot}"))
            .collect::<Vec<_>>();
        for (name, inked) in [
            ("bold", self.bold),
            ("faint", self.faint),
            ("italic", self.italic),
            ("underline", self.underline),
            ("link", self.link),
        ] {
            if !inked {
                missing.push(name.to_string());
            }
        }
        missing
    }
}

#[test]
fn the_tape_inks_every_slot_and_attribute() {
    for (session, slot14) in SESSIONS {
        let tape = session.tape();
        assert_eq!(
            Inked::of(tape).missing(),
            Vec::<String>::new(),
            "{session:?}"
        );
        // The check notices a slot that only one event inks.
        let without = tape_without(session, slot14);
        assert_eq!(without.len() + 1, tape.len(), "{session:?}");
        assert_eq!(
            Inked::of(&without).missing(),
            vec!["slot 14".to_string()],
            "{session:?}"
        );
    }
    // The closing sweep selects cells.
    cold();
    let (input, _) = demo_frame(&colors("Dracula"), DEMO_LOOP_MS - 60, true, 24, 80);
    assert!(
        !input.selection.is_empty(),
        "the sweep selects at the loop's end"
    );
    let (input, _) = demo_frame(&colors("Dracula"), DEMO_LOOP_MS / 2, true, 24, 80);
    assert!(input.selection.is_empty(), "and nothing before it");
}

#[test]
fn demo_frame_is_a_pure_function_of_phase() {
    let theme = colors("Dracula");
    let phase = u64::from(DEMO_TAPE[DEMO_SETTLED_EVENT + 20].at_ms) + 10;
    cold();
    let (cold_frame, cold_key) = demo_frame(&theme, phase, true, 16, 60);
    cold();
    let mut warm = None;
    for step in (0..=phase).step_by(97) {
        warm = Some(demo_frame(&theme, step, true, 16, 60));
    }
    let (warm_frame, warm_key) = demo_frame(&theme, phase, true, 16, 60);
    assert!(warm.is_some());
    assert_eq!(warm_key, cold_key);
    assert_eq!(
        *warm_frame, *cold_frame,
        "fed incrementally or cold, one frame"
    );

    // Wrapping back to the loop's start equals a fresh tape there.
    let _ = demo_frame(&theme, DEMO_LOOP_MS - 1, true, 16, 60);
    let (wrapped, _) = demo_frame(&theme, 0, true, 16, 60);
    cold();
    let (fresh, _) = demo_frame(&theme, 0, true, 16, 60);
    assert_eq!(*wrapped, *fresh);

    // Negative control: the next event is a different frame.
    let index = event_index(phase);
    let next = u64::from(DEMO_TAPE[index + 1].at_ms);
    let (next_frame, next_key) = demo_frame(&theme, next, true, 16, 60);
    assert_eq!(event_index(next), index + 1);
    assert_ne!(next_key, cold_key);
    assert_ne!(*next_frame, *cold_frame);
}

#[test]
fn a_theme_switch_keeps_the_phase() {
    let phase = u64::from(DEMO_TAPE[DEMO_SETTLED_EVENT].at_ms) + 5;
    cold();
    let (night, _) = demo_frame(&colors("Dracula"), phase, true, 20, 60);
    let (day, _) = demo_frame(&colors("Solarized Light"), phase, true, 20, 60);
    assert_eq!(frame_key(phase, true), frame_key(phase, true));
    assert_ne!(*night, *day, "the new theme draws");
    let text = |input: &RenderInput| {
        input
            .cells
            .iter()
            .map(|row| row.iter().map(|cell| cell.ch).collect::<String>())
            .collect::<Vec<_>>()
    };
    assert_eq!(text(&night), text(&day), "the session goes on where it was");
    // The rebased tape equals one built in the new theme from the start.
    cold();
    let (built, _) = demo_frame(&colors("Solarized Light"), phase, true, 20, 60);
    assert_eq!(*built, *day);
}

#[test]
fn the_cadence_is_never_continuous() {
    for phase in (0..DEMO_LOOP_MS).step_by(5) {
        let PreviewAnimation::BlinkEdge { after_ms } = demo_animation(phase, true) else {
            panic!("phase {phase}: {:?}", demo_animation(phase, true));
        };
        let next_event = DEMO_TAPE
            .get(event_index(phase) + 1)
            .map_or(DEMO_LOOP_MS, |event| u64::from(event.at_ms));
        let next_blink = (phase / BLINK_HALF_MS + 1) * BLINK_HALF_MS;
        assert!(after_ms >= 1, "phase {phase}");
        assert!(after_ms <= next_event - phase, "phase {phase}");
        assert!(after_ms <= next_blink - phase, "phase {phase}");
        // The frame does not change before the wake.
        if after_ms > 1 {
            assert_eq!(
                frame_key(phase, true),
                frame_key(phase + after_ms - 1, true),
                "phase {phase}"
            );
        }
    }
}

#[test]
fn the_demo_rests() {
    let demo = |pause: Option<PreviewPause>| {
        let spec = SettingsPreviewSpec::theme_demo(13.0, 4_000);
        match pause {
            Some(pause) => spec.with_reduced_motion(true).with_pause(pause),
            None => spec,
        }
        .animation()
    };
    for pause in [
        PreviewPause::MotionSetting,
        PreviewPause::SystemReduceMotion,
        PreviewPause::SeriousMode,
        PreviewPause::Load,
        PreviewPause::Unfocused,
        PreviewPause::Resting,
    ] {
        assert_eq!(demo(Some(pause)), PreviewAnimation::None, "{pause:?}");
    }
    assert!(demo_rests(3 * DEMO_LOOP_MS));
    // Negative control: unpaused, inside three loops, it plays.
    assert!(!demo_rests(3 * DEMO_LOOP_MS - 1));
    assert!(matches!(demo(None), PreviewAnimation::BlinkEdge { .. }));
}

#[test]
fn the_rest_frame_is_the_settled_event() {
    for phase in [0, DEMO_LOOP_MS / 3, DEMO_LOOP_MS - 1] {
        assert_eq!(frame_key(phase, false), (DEMO_SETTLED_EVENT, true, 0));
    }
    // The settled event is the prompt after the test run: the run's badges
    // are the write before it, in either session.
    for (session, _) in SESSIONS {
        let (tape, settled) = (session.tape(), session.settled_event());
        assert!(
            tape[settled - 1].bytes.windows(6).any(|w| w == b" PASS "),
            "{session:?}"
        );
        assert_eq!(
            tape[settled].bytes,
            session.prompt(),
            "{session:?}: the caret rests on a fresh prompt"
        );
    }
    assert_eq!(DEMO_TAPE[DEMO_SETTLED_EVENT].bytes, PROMPT);
    cold();
    let (resting, _) = demo_frame(&colors("Dracula"), 0, false, 24, 80);
    let at = u64::from(DEMO_TAPE[DEMO_SETTLED_EVENT].at_ms);
    let (playing, _) = demo_frame(&colors("Dracula"), at, true, 24, 80);
    assert_eq!(resting.cells, playing.cells);
    assert!(resting.cursor_visible, "a resting caret is steady");
}

#[test]
fn no_event_exceeds_256_bytes() {
    for (session, _) in SESSIONS {
        let tape = session.tape();
        for (index, event) in tape.iter().enumerate() {
            assert!(event.bytes.len() <= 256, "{session:?} event {index}");
        }
        assert!(
            tape.windows(2).all(|w| w[0].at_ms <= w[1].at_ms),
            "{session:?}: in time order"
        );
        // Lines measured off the parsed grid, wide enough that nothing wraps.
        let mut wrapping = 0;
        for text in tape_text(tape) {
            let width = text.chars().count();
            if text.contains(WRAPPING_TEXT) {
                wrapping += 1;
                assert!(
                    width > 44,
                    "{session:?}: the comment is the line that wraps"
                );
            } else {
                assert!(width <= 44, "{session:?}: {width} columns: {text}");
            }
        }
        assert_eq!(wrapping, 1, "{session:?}");
        let loop_ms = session.loop_ms();
        assert!(
            (14_000..=19_000).contains(&loop_ms),
            "{session:?}: about 16 s: {loop_ms}"
        );
    }
    assert_eq!(DEMO_LOOP_MS, DemoSession::PLATFORM.loop_ms());
}

/// The demo plays the shell a terminal on this platform opens: on Windows a
/// PowerShell session under its PowerShell tab, `PS C:\src\aterm>` prompts and
/// PSReadLine's colours on what is typed; elsewhere the zsh session. On
/// Windows it was the zsh session, a starship prompt and `targo` under a
/// PowerShell tab (the 2026-10-18 Fluent judge's
/// `J4F-7-settings-samples-unix-and-light-badges`). NEGATIVE CONTROL: the zsh
/// session is the Unix one, and macOS and Linux still play it.
#[test]
fn the_demo_plays_this_platforms_shell() {
    assert_eq!(
        DemoSession::PLATFORM == DemoSession::PowerShell,
        cfg!(windows)
    );
    assert_eq!(DEMO_TAPE, DemoSession::PLATFORM.tape());
    let pwsh = tape_text(DemoSession::PowerShell.tape());
    for line in [
        "PS C:\\src\\aterm> git status -sb",
        "PS C:\\src\\aterm> cargo test -q theme",
        "  --> src\\theme.rs:4:5",
        "PS C:\\src\\aterm> bat -p src\\theme.rs",
    ] {
        assert!(pwsh.iter().any(|row| row == line), "{line}: {pwsh:#?}");
    }
    let unix = ["~/", "\u{276f}", "targo", "\u{271a}", "$ "];
    for row in &pwsh {
        for mark in unix {
            assert!(
                !row.contains(mark),
                "{mark} in the PowerShell session: {row}"
            );
        }
    }
    // Negative control: the zsh session is the Unix one.
    let zsh = tape_text(DemoSession::Zsh.tape());
    assert!(
        zsh.iter().any(|row| row == "\u{276f} targo test -q theme"),
        "{zsh:#?}"
    );
    assert!(zsh.iter().all(|row| !row.starts_with("PS ")));

    // PSReadLine's colours: the command bright yellow, a parameter bright
    // black, the rest the text colour; the switch rides with the character
    // after it, so typing keeps its 55 ms beat.
    let mut term = Terminal::new(8, 80);
    let tape = DemoSession::PowerShell.tape();
    // The clear, the prompt, then one event a typed character.
    let first_command = &tape[..2 + "git status -sb".len()];
    for event in first_command {
        term.process(event.bytes);
    }
    let slots: Vec<Option<u8>> = (17..31)
        .map(|col| {
            let colors = term.grid().cell(0, col).expect("a typed cell").colors();
            colors.fg_is_indexed().then(|| colors.fg_index())
        })
        .collect();
    let (yellow, black) = (Some(11), Some(8));
    assert_eq!(
        slots,
        [
            yellow, yellow, yellow, None, None, None, None, None, None, None, None, black, black,
            black
        ],
        "git status -sb"
    );
    let typed: Vec<&DemoEvent> = tape
        .iter()
        .skip(2)
        .take_while(|event| event.bytes != b"\x1b[0m\r\n")
        .collect();
    assert_eq!(typed.len(), "git status -sb".len(), "one event a character");
    assert!(
        typed.windows(2).all(|w| w[1].at_ms - w[0].at_ms == CHAR_MS),
        "the colour switches cost no time"
    );
}

/// The contrast of the drawn cells of the first `word` in `input`.
fn word_contrast(input: &RenderInput, word: &str) -> f32 {
    let (row, col) = input
        .cells
        .iter()
        .enumerate()
        .find_map(|(row, cells)| {
            let text: String = cells.iter().map(|cell| cell.ch).collect();
            text.find(word)
                .map(|byte| (row, text[..byte].chars().count()))
        })
        .unwrap_or_else(|| panic!("{word} on screen"));
    let cell = &input.cells[row][col];
    crate::native_appearance::contrast_ratio(cell.fg, cell.bg)
}

/// The demo holds its colours to the floor a terminal tab holds the same
/// colours to: Default Light's FAIL badge (bright white on red) and PASS
/// badge (black on green) clear 4.5:1 under Windows' default floor, where the
/// demo drew them at 1.49:1 and 2.71:1 (the 2026-10-18 type judge's
/// `j4t-themes-demo-light-badges-unfloored`), and the demo the Themes page
/// paints uses the floor a tab resolves when `minimum_contrast` is unset.
/// NEGATIVE CONTROL: with the floor off the badges are as low as before.
#[test]
fn the_demo_holds_theme_pairs_to_the_terminals_floor() {
    let settled = u64::from(DEMO_TAPE[DEMO_SETTLED_EVENT].at_ms);
    cold();
    let (off, off_key) = demo_frame(&floored("Default Light", 1.0), settled, true, 24, 60);
    assert!(word_contrast(&off, "FAIL ") < 2.0, "the floor off");
    assert!(word_contrast(&off, "PASS ") < 3.0, "the floor off");
    let (on, on_key) = demo_frame(&floored("Default Light", 4.5), settled, true, 24, 60);
    assert_ne!(on_key, off_key, "the floor is part of the frame's key");
    for badge in ["FAIL ", "PASS "] {
        let ratio = word_contrast(&on, badge);
        assert!(ratio >= 4.5, "{badge}: {ratio:.2}:1");
    }
    // The text on the page keeps its own colours (the floor asks 3:1 there).
    assert_eq!(
        word_contrast(&on, "notes.txt"),
        word_contrast(&off, "notes.txt")
    );

    // The demo the page paints draws under the terminal's default floor.
    let painted = |appearance: crate::settings_preview::AppearancePreviewSpec| {
        let spec = demo_spec(
            "Default Light",
            aterm_messages::tokens::AuthoredChrome::NONE,
        )
        .with_appearance(appearance);
        let mut prims = Vec::new();
        spec.paint_demo_frame(
            &mut prims,
            DEMO_RECT,
            aterm_render::Theme::default(),
            demo_roles(),
            DemoFrame::Windows,
        );
        prims
            .into_iter()
            .find_map(|prim| match prim {
                DrawPrim::TerminalSpecimen { spec, .. } => Some(Arc::clone(&spec.input)),
                _ => None,
            })
            .expect("the demo draws its grid")
    };
    let default = painted(crate::settings_preview::AppearancePreviewSpec::default());
    let floor = crate::app_config::DEFAULT_INDEXED_MINIMUM_CONTRAST;
    let fail = word_contrast(&default, "FAIL ");
    if cfg!(windows) {
        assert_eq!(floor, 4.5);
        assert!(fail >= 4.5, "the page's demo, FAIL {fail:.2}:1");
    } else {
        assert_eq!(floor, 1.0);
        assert!(fail < 2.0, "macOS and Linux keep the floor off");
    }
    // A set `minimum_contrast` turns the theme-pair floor off, as in a tab.
    let off = painted(crate::settings_preview::AppearancePreviewSpec {
        indexed_minimum_contrast: 1.0,
        ..crate::settings_preview::AppearancePreviewSpec::default()
    });
    assert!(word_contrast(&off, "FAIL ") < 2.0);
}

#[test]
fn the_memo_hits_and_misses() {
    cold();
    let phase = 2_000;
    let (first, key) = demo_frame(&colors("Dracula"), phase, true, 12, 50);
    let (again, again_key) = demo_frame(&colors("Dracula"), phase + 1, true, 12, 50);
    assert_eq!(frame_key(phase, true), frame_key(phase + 1, true));
    assert_eq!(key, again_key);
    assert!(Arc::ptr_eq(&first, &again), "an equal key reuses the frame");
    let (other, other_key) = demo_frame(&colors("Nord"), phase, true, 12, 50);
    assert_ne!(other_key, key);
    assert!(!Arc::ptr_eq(&first, &other), "a new theme is a new frame");
}

/// The demo window the tests paint: 520 × 300 at the origin, wide enough that
/// no tab title is cut.
const DEMO_RECT: crate::native_ui::LogicalRect =
    crate::native_ui::LogicalRect::new(0.0, 0.0, 520.0, 300.0);

fn demo_spec(scheme: &str, chrome: aterm_messages::tokens::AuthoredChrome) -> SettingsPreviewSpec {
    SettingsPreviewSpec::theme_demo(13.0, 0)
        .with_terminal_theme(colors(scheme).theme)
        .with_reduced_motion(true)
        .with_demo(chrome, None, 0)
}

fn demo_roles() -> crate::settings::Roles {
    crate::native_appearance::NativeRoles::from_theme(aterm_render::Theme::default())
}

/// The prims of `scheme`'s demo with `chrome`, resting, in `frame`, `None`
/// for the frame this platform draws.
fn demo_prims(
    scheme: &str,
    chrome: aterm_messages::tokens::AuthoredChrome,
    frame: Option<DemoFrame>,
) -> Vec<DrawPrim> {
    let spec = demo_spec(scheme, chrome);
    let mut prims = Vec::new();
    let theme = aterm_render::Theme::default();
    match frame {
        Some(frame) => spec.paint_demo_frame(&mut prims, DEMO_RECT, theme, demo_roles(), frame),
        None => spec.paint(&mut prims, DEMO_RECT, theme, demo_roles()),
    }
    prims
}

/// The pixel at logical `(x, y)` of `prims` rasterized at `scale`.
fn demo_pixels(prims: &[DrawPrim], scale: f32) -> impl Fn(f32, f32) -> [u8; 3] + use<> {
    let (pixels, width, _) =
        crate::tray_raster::rasterize_tray(prims, 520, 300, scale, [0, 0, 0, 255]);
    move |x: f32, y: f32| {
        let (x, y) = ((x * scale) as u32, (y * scale) as u32);
        let index = ((y * width + x) * 4) as usize;
        [pixels[index], pixels[index + 1], pixels[index + 2]]
    }
}

fn texts(prims: &[DrawPrim]) -> Vec<(&str, f32, TextFace)> {
    prims
        .iter()
        .filter_map(|prim| match prim {
            DrawPrim::Text { s, x, face, .. } => Some((s.as_str(), *x, *face)),
            _ => None,
        })
        .collect()
}

fn dots(prims: &[DrawPrim]) -> usize {
    prims
        .iter()
        .filter(|prim| matches!(prim, DrawPrim::Dot { .. }))
        .count()
}

fn strip_sample_of(
    scheme: &str,
    chrome: aterm_messages::tokens::AuthoredChrome,
) -> crate::tab_bar::StripSample {
    let night = colors(scheme).theme;
    crate::tab_bar::strip_sample(crate::chrome_band::ChromePalette {
        theme: night.into_theme(),
        ansi: Some(crate::settings_preview::demo_ansi_hues(&night)),
        authored: chrome,
    })
}

const AUTHORED: aterm_messages::tokens::AuthoredChrome = aterm_messages::tokens::AuthoredChrome {
    active_tab: Some([0x12, 0x34, 0x56]),
    accent: Some([0xff, 0x40, 0xb0]),
    border: Some([0x9a, 0x9a, 0x9a]),
    search_match: None,
};

/// The demo is framed as aterm's own window on this platform: on Windows a
/// Windows 11 window (no window dots, Windows shells on the tabs, the
/// caption's glyphs at the right), elsewhere the dots window. It was the dots
/// window with `zsh — aterm` on Windows too (the 2026-10-18 Fluent judge's
/// `F4-themes-demo-macos-window-mock`).
#[test]
fn the_demo_window_is_this_platforms() {
    assert_eq!(DemoFrame::PLATFORM == DemoFrame::Windows, cfg!(windows));
    let none = aterm_messages::tokens::AuthoredChrome::NONE;
    let prims = demo_prims("Dracula", none, None);
    let labels: Vec<&str> = texts(&prims).into_iter().map(|(s, _, _)| s).collect();
    let maximize = prims
        .iter()
        .any(|prim| matches!(prim, DrawPrim::Stroke { w, h, .. } if *w == 9.0 && *h == 9.0));
    if cfg!(windows) {
        assert_eq!(dots(&prims), 0, "no window dots on a Windows window");
        assert!(labels.contains(&"PowerShell"), "{labels:?}");
        assert!(labels.contains(&"Command Prompt"), "{labels:?}");
        assert!(
            !labels.iter().any(|label| label.contains("zsh")),
            "{labels:?}"
        );
        assert!(maximize, "the caption's maximize box");
    } else {
        assert_eq!(dots(&prims), 3);
        assert!(labels.contains(&"zsh — aterm"), "{labels:?}");
        assert!(!maximize);
    }
    // The platform's frame is the one painted.
    let framed = demo_prims("Dracula", none, Some(DemoFrame::PLATFORM));
    assert_eq!(format!("{prims:?}"), format!("{framed:?}"));
}

/// A Windows demo is a Windows 11 window on every platform it is painted on:
/// minimize, maximize and close at the right edge, 40 pt apart on the strip's
/// middle, in white on a dark band and black on a light one; no window dots;
/// the selected tab's title semibold, the quiet one regular, both Windows
/// shells. Negative control: the dots frame has its dots and no caption.
#[test]
fn a_windows_demo_is_a_windows_11_window() {
    let none = aterm_messages::tokens::AuthoredChrome::NONE;
    let layout = DemoStripLayout::of(DEMO_RECT, DemoFrame::Windows);
    let ([minimize, maximize, close], cy) =
        layout.caption.expect("a 520-pt window has its buttons");
    assert_eq!(
        close,
        DEMO_RECT.right() - 20.0,
        "close stands at the right edge"
    );
    assert_eq!((maximize - minimize, close - maximize), (40.0, 40.0));
    assert_eq!(cy, (DEMO_RECT.y + layout.strip_h * 0.5).round());
    let plus = layout.new_tab.expect("the + follows the tabs");
    assert!(layout.tabs_x + layout.tab_w + layout.quiet_w < plus && plus + 10.0 < minimize - 5.0);
    for (scheme, ink) in [
        ("Dracula", [0xff, 0xff, 0xff]),
        ("Default Light", [0, 0, 0]),
    ] {
        let prims = demo_prims(scheme, none, Some(DemoFrame::Windows));
        assert_eq!(dots(&prims), 0, "{scheme}");
        let ink = [ink[0], ink[1], ink[2], 255];
        let bar = prims.iter().any(|prim| {
            matches!(prim, DrawPrim::Panel { x, y, w, h, fill, .. }
                if *x == minimize - 5.0 && *y == cy && *w == 10.0 && *h == 1.0 && *fill == ink)
        });
        let square = prims.iter().any(|prim| {
            matches!(prim, DrawPrim::Stroke { x, y, w, h, color, .. }
                if *x + 4.5 == maximize && *y + 4.5 == cy && *w == 9.0 && *h == 9.0 && *color == ink)
        });
        let cross = prims
            .iter()
            .filter(|prim| {
                matches!(prim, DrawPrim::Line { x1, y1, x2, y2, color, .. }
                    if (x1 + x2) * 0.5 == close && (y1 + y2) * 0.5 == cy
                        && (x2 - x1).abs() == 9.0 && (y2 - y1).abs() == 9.0 && *color == ink)
            })
            .count();
        assert!(
            bar && square && cross == 2,
            "{scheme}: minimize {bar}, maximize {square}, close {cross}"
        );
        let labels = texts(&prims);
        assert!(
            labels.contains(&("PowerShell", layout.tabs_x + 10.0, TextFace::UiBold)),
            "{scheme}: {labels:?}"
        );
        assert!(
            labels.contains(&(
                "Command Prompt",
                layout.tabs_x + layout.tab_w + 10.0,
                TextFace::Ui
            )),
            "{scheme}: {labels:?}"
        );
    }
    // Negative control: the dots window.
    let prims = demo_prims("Dracula", none, Some(DemoFrame::Dots));
    assert_eq!(dots(&prims), 3);
    assert!(
        DemoStripLayout::of(DEMO_RECT, DemoFrame::Dots)
            .caption
            .is_none()
    );
    assert!(
        !prims
            .iter()
            .any(|prim| matches!(prim, DrawPrim::Stroke { w, h, .. } if *w == 9.0 && *h == 9.0))
    );
}

/// The Windows caption stands at the right edge whenever its buttons fit, the
/// tabs and the `+` take what it leaves, and nothing crosses the window's
/// corners or the buttons, from a sliver to the widest demo.
#[test]
fn a_windows_demo_strip_fits_every_width() {
    for width in [
        40.0, 100.0, 135.0, 136.0, 170.0, 240.0, 320.0, 460.0, 560.0, 900.0,
    ] {
        let rect = crate::native_ui::LogicalRect::new(13.5, 7.0, width, 200.0);
        let layout = DemoStripLayout::of(rect, DemoFrame::Windows);
        assert_eq!(layout.caption.is_some(), width >= 136.0, "{width}");
        let first_glyph = layout
            .caption
            .map_or(rect.right(), |([minimize, ..], _)| minimize - 5.0);
        if let Some(([.., close], _)) = layout.caption {
            assert!(
                close + 5.0 <= rect.right() - 8.0,
                "{width}: close clears the corner"
            );
        }
        if layout.tab_w >= DemoStripLayout::MIN_TAB_W {
            let plus = layout.new_tab.expect("tabs bring their +");
            assert!(layout.tabs_x >= rect.x + 8.0, "{width}");
            assert!(
                plus + 5.0 < first_glyph,
                "{width}: the + before the caption"
            );
            assert!(
                layout.tabs_x + layout.tab_w + layout.quiet_w <= plus - 10.0,
                "{width}"
            );
            assert!(layout.tab_w <= 168.0 && layout.quiet_w <= 168.0, "{width}");
        } else {
            assert!(layout.new_tab.is_none(), "{width}");
        }
    }
}

/// A Windows demo's selected tab is the real strip's: the terminal's ground,
/// its top corners rounded on the band, running on through the seam into the
/// grid where its fill IS the ground, at 1x and 2x. Negative control: an
/// `active_tab_color` apart from the ground stands closed on the seam.
#[test]
fn a_windows_demo_tab_opens_into_its_grid() {
    let ground = crate::settings::u32_rgb(colors("Dracula").theme.bg);
    let open = aterm_messages::tokens::AuthoredChrome {
        active_tab: Some(ground),
        ..aterm_messages::tokens::AuthoredChrome::NONE
    };
    let layout = DemoStripLayout::of(DEMO_RECT, DemoFrame::Windows);
    let foot = DEMO_RECT.y + layout.strip_h;
    let inside = layout.tabs_x + 4.0;
    let quiet = layout.tabs_x + layout.tab_w + 4.0;
    for scale in [1.0, 2.0] {
        let strip = strip_sample_of("Dracula", open);
        let at = demo_pixels(
            &demo_prims("Dracula", open, Some(DemoFrame::Windows)),
            scale,
        );
        let mut y = layout.tab_top + 8.0;
        while y < foot + 6.0 {
            assert_eq!(at(inside, y), ground, "{scale}x: the tab at {y}");
            y += 0.5;
        }
        assert_eq!(
            at(quiet, foot - 1.0),
            strip.seam,
            "{scale}x: the seam beside it"
        );
        assert_eq!(
            at(layout.tabs_x, layout.tab_top),
            strip.band,
            "{scale}x: a rounded corner"
        );
        // Negative control: a tab of another colour stands on the seam.
        let at = demo_pixels(
            &demo_prims("Dracula", AUTHORED, Some(DemoFrame::Windows)),
            scale,
        );
        assert_eq!(at(inside, foot - 2.0), [0x12, 0x34, 0x56], "{scale}x");
        assert_eq!(
            at(inside, foot - 1.0),
            strip_sample_of("Dracula", AUTHORED).seam,
            "{scale}x"
        );
        assert_eq!(at(inside, foot + 1.0), ground, "{scale}x");
    }
}

/// The tab strip paints the candidate's chrome colours in either frame: the
/// selected tab's fill and the seam; the dots frame's selected card carries
/// the accent rule, which the Windows tab row does not draw (its selected
/// tab is marked by its shape, as Windows Terminal's is).
#[test]
fn the_demo_tab_strip_paints_chrome_tokens() {
    let paint = |chrome, frame: DemoFrame| {
        let at = demo_pixels(&demo_prims("Dracula", chrome, Some(frame)), 1.0);
        let layout = DemoStripLayout::of(DEMO_RECT, frame);
        let foot = DEMO_RECT.y + layout.strip_h;
        match frame {
            // Inside the active tab clear of its label, on its rule, and on
            // the seam after it (the tab spans 52..220).
            DemoFrame::Dots => (at(212.0, 12.0), at(120.0, 27.0), at(220.0, 14.0)),
            // Inside the tab clear of its label and `✕`, where the dots frame
            // has its rule, and on the seam under the quiet tab.
            DemoFrame::Windows => (
                at(layout.tabs_x + layout.tab_w - 4.0, layout.tab_top + 4.0),
                at(layout.tabs_x + 20.0, foot - 2.0),
                at(layout.tabs_x + layout.tab_w * 1.5, foot - 1.0),
            ),
        }
    };
    let none = aterm_messages::tokens::AuthoredChrome::NONE;
    let (authored, derived) = (
        strip_sample_of("Dracula", AUTHORED),
        strip_sample_of("Dracula", none),
    );
    let (tab, rule, seam) = paint(AUTHORED, DemoFrame::Dots);
    assert_eq!(tab, [0x12, 0x34, 0x56], "active_tab_color fills the tab");
    assert_eq!(rule, authored.rule, "accent_color is its rule");
    assert_eq!(seam, authored.seam, "border_color is the seam");
    // Negative control: a theme with no chrome colours shows the derived
    // tones, not the last theme's.
    let (tab, rule, seam) = paint(none, DemoFrame::Dots);
    assert_eq!(
        (tab, rule, seam),
        (derived.active, derived.rule, derived.seam)
    );
    assert_ne!(tab, [0x12, 0x34, 0x56]);
    assert_ne!(rule, authored.rule);
    // The Windows tab row: the fill and the seam, and no rule.
    let (tab, foot, seam) = paint(AUTHORED, DemoFrame::Windows);
    assert_eq!(tab, [0x12, 0x34, 0x56], "active_tab_color fills the tab");
    assert_eq!(foot, [0x12, 0x34, 0x56], "no accent rule under it");
    assert_eq!(seam, authored.seam, "border_color is the seam");
    let (tab, _, seam) = paint(none, DemoFrame::Windows);
    assert_eq!((tab, seam), (derived.active, derived.seam));
}

/// A DEMO TAB SAYS A WHOLE TITLE OR NONE (audit r14 PV5): both tabs used to
/// get one equal width, so at 2x text (or a narrow window) the selected tab
/// painted "zsh —…" beside a whole "targo test". In either frame, at 1x and
/// 2x text, from a sliver of a window to the widest demo, every title a tab
/// paints is one of its authored titles, whole, never an ellipsis, inside
/// its own tab; the quiet tab gives way first, so wherever the selected tab
/// shows a title the longest one it can hold. NEGATIVE CONTROL: at 30
/// columns and at 102 columns with 2x text, half the strip is too narrow for
/// "zsh — aterm", so an equal split would cut it.
#[test]
fn a_demo_tab_says_a_whole_title_or_none() {
    use crate::native_appearance::{AppearancePreferences, install_preferences};
    let none = aterm_messages::tokens::AuthoredChrome::NONE;
    let spec = demo_spec("Dracula", none);
    let mut long_titles = 0;
    for scale in [1.0_f32, 2.0] {
        let _ = install_preferences(AppearancePreferences {
            text_scale: scale,
            ..AppearancePreferences::default()
        });
        for frame in [DemoFrame::Dots, DemoFrame::Windows] {
            let [selected, quiet] = frame.titles();
            for width in (40..=900).step_by(13) {
                let rect = crate::native_ui::LogicalRect::new(10.0, 6.0, width as f32, 220.0);
                let layout = DemoStripLayout::of(rect, frame);
                let mut prims = Vec::new();
                spec.paint_demo_frame(
                    &mut prims,
                    rect,
                    aterm_render::Theme::default(),
                    demo_roles(),
                    frame,
                );
                let strip_bottom = rect.y + layout.strip_h;
                for prim in &prims {
                    let DrawPrim::Text {
                        s,
                        x,
                        px,
                        face,
                        baseline,
                        ..
                    } = prim
                    else {
                        continue;
                    };
                    if *baseline > strip_bottom {
                        continue;
                    }
                    assert!(!s.contains('…'), "{frame:?} {width} at {scale}x: {s:?}");
                    let selected_tab = *x < layout.tabs_x + layout.tab_w;
                    let (titles, right) = if selected_tab {
                        (selected, layout.tabs_x + layout.tab_w)
                    } else {
                        (quiet, layout.tabs_x + layout.tab_w + layout.quiet_w)
                    };
                    assert!(
                        titles.contains(&s.as_str()),
                        "{frame:?} {width} at {scale}x: {s:?} is not one of {titles:?}"
                    );
                    let ends = x + crate::tray_raster::ui_text_width_for(*face, s, *px);
                    assert!(
                        ends <= right - 8.0,
                        "{frame:?} {width} at {scale}x: {s:?} ends at {ends:.1}, its tab at {right:.1}"
                    );
                    if selected_tab && s == selected[0] {
                        long_titles += 1;
                    }
                }
            }
        }
    }
    let _ = install_preferences(AppearancePreferences::default());
    assert!(
        long_titles > 40,
        "the long title is kept wherever it fits: {long_titles}"
    );

    // NEGATIVE CONTROL: where the cut was seen, an equal split cannot hold
    // the selected title, and the selected tab still says a whole one.
    // The demo widths the audit measured: 30x24 at 1x (a 69-pt tab each) and
    // 102x29 at 2x (a 119-pt one).
    for (scale, demo_width) in [(1.0_f32, 202.0_f32), (2.0, 302.0)] {
        let _ = install_preferences(AppearancePreferences {
            text_scale: scale,
            ..AppearancePreferences::default()
        });
        let px = crate::type_scale::TypeStep::Caption
            .px(13.75)
            .scaled(scale)
            .get();
        let title =
            crate::tray_raster::ui_text_width_for(TextFace::Ui, DemoFrame::Dots.titles()[0][0], px);
        let fits_or_cut = |width: f32| {
            let rect = crate::native_ui::LogicalRect::new(0.0, 0.0, width, 200.0);
            let room = rect.right() - 12.0 - (rect.x + 52.0);
            let layout = DemoStripLayout::of(rect, DemoFrame::Dots);
            (room / 2.0 - 20.0 < title, layout.titles[0])
        };
        let (equal_cuts, kept) = fits_or_cut(demo_width);
        assert!(equal_cuts, "{scale}x: an equal split cuts the title");
        assert!(
            kept.is_some_and(|title| DemoFrame::Dots.titles()[0].contains(&title)),
            "{scale}x: the selected tab still says a whole title: {kept:?}"
        );
    }
    let _ = install_preferences(AppearancePreferences::default());
}
