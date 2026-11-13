// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::PointerAction;
use crate::cursor_glow::Geom;
use crate::input::{InputEvent, InputOutcome, PixelOffset, Source};
use crate::{App, WindowId, term_lock};
use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_session::sink::SinkWriter;
use aterm_types::keyboard::{Key, KeyEventType, Modifiers};
use std::io::{ErrorKind, Read};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::Instant;

fn fixture(mode: u16) -> (App, UnixStream, Geom) {
    let (reader, writer) = UnixStream::pair().expect("private input observer");
    reader.set_nonblocking(true).unwrap();
    let mut app = App::headless_for_test_with_sink(Arc::new(SinkWriter::new_owned(writer.into())));
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("rainbow".to_owned());
    app.recompute_sparkle();
    let wid = WindowId(0);
    let term = app.front_terminal(wid).unwrap().term.clone();
    if mode != 0 {
        term_lock(&term).process(format!("\x1b[?{mode}h\x1b[?1006h").as_bytes());
    }
    app.pointer_cmd(PointerAction::Move { row: 2, col: 3 })
        .unwrap();
    let geom = Geom {
        cw: 8,
        ch: 16,
        rows: 24,
        cols: 80,
        origin_x: 0,
        origin_y: 0,
        win_w: 640,
        win_h: 384,
        head: 0,
    };
    let cfg = app.glow_config();
    app.windows
        .get_mut(&wid)
        .unwrap()
        .fx
        .cursor_mut()
        .glow
        .tick(Some((0, 0)), Instant::now(), &cfg, geom, &mut Vec::new());
    (app, reader, geom)
}

fn read_input(reader: &mut UnixStream) -> Vec<u8> {
    let mut bytes = [0; 128];
    match reader.read(&mut bytes) {
        Ok(n) => bytes[..n].to_vec(),
        Err(e) if e.kind() == ErrorKind::WouldBlock => Vec::new(),
        Err(e) => panic!("reading input: {e}"),
    }
}

fn press(app: &mut App) {
    assert_eq!(
        app.input(
            WindowId(0),
            InputEvent::Key {
                key: Key::Character('a'),
                mods: Modifiers::empty(),
                base_layout: None,
                layout_text: None,
                event_type: KeyEventType::Press,
            },
            Source::Human,
        ),
        InputOutcome::Ok,
    );
}

fn echo_at(app: &mut App, geom: Geom, now: Instant) -> (u64, &'static str, bool) {
    let wid = WindowId(0);
    let term = app.front_terminal(wid).unwrap().term.clone();
    term_lock(&term).process(b"a");
    let cursor = term_lock(&term).cursor();
    let cfg = app.glow_config();
    let ws = app.windows.get_mut(&wid).unwrap();
    let mut out = Vec::new();
    ws.fx
        .cursor_mut()
        .glow
        .tick(Some((cursor.row, cursor.col)), now, &cfg, geom, &mut out);
    (
        ws.fx.cursor_mut().glow.spawns(),
        ws.fx
            .cursor_mut()
            .glow
            .admission_log()
            .last()
            .unwrap()
            .licence,
        {
            let glow = &ws.fx.cursor().glow;
            !glow.out_laid(&out).is_empty() || !glow.under_laid().is_empty()
        },
    )
}

#[test]
fn byte_silent_pointer_preserves_the_keys_delayed_echo() {
    let wid = WindowId(0);
    let model = aterm_models_effects::cursor_hint_license_model();
    let (mut control, _reader, geom) = fixture(0);
    press(&mut control);
    let pressed = Instant::now();
    control.clear_move_license(wid);
    let cleared_echo = echo_at(&mut control, geom, pressed);
    assert_eq!(cleared_echo, (1, "inflight", true));
    for controller in [false, true] {
        for mode in [0, 1000, 1002, 1003] {
            for same_pixel in [false, true] {
                let (mut app, mut reader, geom) = fixture(mode);
                let _ = read_input(&mut reader);
                press(&mut app);
                let pressed = Instant::now();
                assert_eq!(read_input(&mut reader), b"a");
                let mut before = model.init_state();
                assert!(model.fire("PressArmsLicense", &mut before));
                assert!(app.windows[&wid].fx.cursor().glow.move_licensed(pressed));
                let col = if same_pixel { 3 } else { 4 };
                if controller {
                    assert_eq!(
                        app.input(
                            wid,
                            InputEvent::MouseMove {
                                buttons: 3,
                                row: 2,
                                col,
                                mods: 0,
                                side: SelectionSide::Left,
                                px_off: PixelOffset::default(),
                            },
                            Source::Controller,
                        ),
                        InputOutcome::Ok,
                    );
                } else {
                    app.pointer_cmd(PointerAction::Move { row: 2, col })
                        .unwrap();
                }
                let reported = mode == 1003 && !same_pixel;
                let bytes = read_input(&mut reader);
                assert_eq!(
                    bytes.is_empty(),
                    !reported,
                    "{mode}/{controller}/{same_pixel}"
                );
                if reported {
                    assert!(bytes.starts_with(b"\x1b[<35;"), "{bytes:?}");
                }
                let ws = &app.windows[&wid];
                let live = ws.fx.cursor().glow.move_licensed(pressed);
                assert_eq!(live, !reported, "{mode}/{controller}/{same_pixel}");
                assert_eq!(ws.fx.cursor().trail.move_licensed(pressed), !reported);
                assert_eq!(
                    echo_at(&mut app, geom, pressed),
                    if reported {
                        cleared_echo
                    } else {
                        (1, "key", true)
                    },
                    "echo admission {mode}/{controller}/{same_pixel}",
                );
                if mode == 0 && !controller && !same_pixel {
                    let mut after = before.clone();
                    after.insert("hint", i64::from(live));
                    let (ok, why) = aterm_spec::verify::validate_transition_tiered(
                        &model,
                        &[],
                        &before,
                        &after,
                        Some("ByteSilentPointerPreservesLicense"),
                        "plain pointer hover",
                    );
                    assert!(ok, "{why}");
                    let mut erased = after;
                    erased.insert("hint", 0);
                    let (ok, _) = aterm_spec::verify::validate_transition_tiered(
                        &model,
                        &[],
                        &before,
                        &erased,
                        Some("ByteSilentPointerPreservesLicense"),
                        "hover-clear negative control",
                    );
                    assert!(!ok, "the model must reject a hover that loses the key");
                }
            }
        }
    }
}

#[test]
fn pointer_gestures_still_retire_the_pending_typed_license() {
    for local_selection in [false, true] {
        let (mut app, mut reader, _) = fixture(0);
        let wid = WindowId(0);
        press(&mut app);
        assert_eq!(read_input(&mut reader), b"a");
        if local_selection {
            app.begin_selection(wid, SelectionType::Simple);
            app.pointer_cmd(PointerAction::Move { row: 2, col: 5 })
                .unwrap();
        } else {
            assert_eq!(
                app.input(
                    wid,
                    InputEvent::MouseMove {
                        buttons: 0,
                        row: 2,
                        col: 5,
                        mods: 0,
                        side: SelectionSide::Left,
                        px_off: PixelOffset::default(),
                    },
                    Source::Controller
                ),
                InputOutcome::Ok
            );
        }
        assert_eq!(app.windows[&wid].selecting, local_selection);
        assert!(
            read_input(&mut reader).is_empty(),
            "tracking-OFF drag stays local"
        );
        assert!(
            !app.windows[&wid]
                .fx
                .cursor()
                .glow
                .move_licensed(Instant::now())
        );
        assert!(
            !app.windows[&wid]
                .fx
                .cursor()
                .trail
                .move_licensed(Instant::now())
        );
    }
}
