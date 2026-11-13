// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for `trail-gallery` argument parsing, per-instance configuration, and checkout output path guards.

use super::*;

/// [`line_fixture_waiting`] no key reaches while a command runs: each of its waits ends at once.
fn line_fixture(
    input: &mut impl std::io::Read,
    output: &mut impl std::io::Write,
    mode: LineMode,
    size: impl FnMut() -> Option<(u16, u16)>,
) -> std::io::Result<()> {
    line_fixture_waiting(input, output, mode, size, &mut |_| Ok(false))
}

/// The `typing` scenario's fixture.
fn typing_fixture(
    input: &mut impl std::io::Read,
    output: &mut impl std::io::Write,
    size: impl FnMut() -> Option<(u16, u16)>,
) -> std::io::Result<()> {
    line_fixture(input, output, LineMode::Typing, size)
}

fn typed_terminal(keys: &[u8]) -> aterm_core::terminal::Terminal {
    typed_terminal_at(keys, 72)
}

fn typed_terminal_at(keys: &[u8], cols: u16) -> aterm_core::terminal::Terminal {
    let mut output = Vec::new();
    typing_fixture(&mut std::io::Cursor::new(keys), &mut output, || {
        Some((14, cols))
    })
    .unwrap();
    let mut term = aterm_core::terminal::Terminal::new(14, cols);
    term.process(&output);
    term
}

#[test]
fn typed_gallery_input_is_literal_and_return_only_starts_another_prompt() {
    let ordinary = typed_terminal(TYPING_TEXT.as_bytes());
    assert_eq!(
        ordinary.row_text(4).as_deref(),
        Some("$ echo the quick brown fox")
    );

    let literal = "$(touch sentinel); <>&| `echo not-a-command`";
    let mut keys = TYPING_TEXT.as_bytes().to_vec();
    keys.push(b'\r');
    keys.extend_from_slice(literal.as_bytes());
    keys.extend_from_slice(b"\r\x04must not appear");
    let mut term = typed_terminal(&keys);
    assert_eq!(term.row_text(4), ordinary.row_text(4));
    assert_eq!(term.row_text(5), Some(format!("$ {literal}")));
    assert_eq!(term.row_text(6).as_deref(), Some("$ "));
    assert_eq!(term.row_text(7).as_deref(), Some(""));
    let frame = term.cell_frame(14, 72);
    assert_eq!((frame.cursor_row, frame.cursor_col), (6, 2));
}

#[test]
fn typed_gallery_edits_and_clear_repaint_the_current_prompt_without_stale_text() {
    let ordinary = typed_terminal(b"abc\x08\x7fz");
    assert_eq!(ordinary.row_text(4).unwrap().trim_end(), "$ az");

    let mut keys = TYPING_TEXT.as_bytes().to_vec();
    keys.extend_from_slice(b"\r\x0cabc\x08\x7fz\x0c\x7f\x15\x08\x08q\x03must not appear");
    let mut term = typed_terminal(&keys);
    assert_eq!(term.row_text(0).unwrap().trim_end(), "$ q");
    for row in 1..14 {
        assert!(term.row_text(row).unwrap().trim_end().is_empty());
    }
    let frame = term.cell_frame(14, 72);
    assert_eq!((frame.cursor_row, frame.cursor_col), (0, 3));

    let ignored = typed_terminal(b"q\x00\x1b\xff\x0c");
    assert_eq!(ignored.row_text(0).unwrap().trim_end(), "$ q");
}

#[test]
fn typed_gallery_deletion_repaints_pending_wrap_and_wrapped_rows() {
    for cols in [12_u16, 40, 72] {
        for len in [usize::from(cols) - 2, usize::from(cols) - 1, 80] {
            let mut keys = vec![b'x'; len];
            if len == 80 {
                keys.push(b'y');
            }
            keys.push(127);
            let mut term = typed_terminal_at(&keys, cols);
            let mut expected = aterm_core::terminal::Terminal::new(14, cols);
            expected.process(b"\x1b[2J\x1b[5;1H$ ");
            expected.process(&vec![b'x'; len - 1]);
            for row in 0..14 {
                assert_eq!(
                    term.row_text(row).unwrap().trim_end(),
                    expected.row_text(row).unwrap().trim_end(),
                    "{cols} columns, {len} input bytes, row {row}"
                );
            }
            let frame = term.cell_frame(14, usize::from(cols));
            let expected_frame = expected.cell_frame(14, usize::from(cols));
            assert_eq!(
                (frame.cursor_row, frame.cursor_col),
                (expected_frame.cursor_row, expected_frame.cursor_col)
            );

            keys.push(21);
            let cleared = typed_terminal_at(&keys, cols);
            assert_eq!(cleared.row_text(4).unwrap().trim_end(), "$");
            for row in 5..14 {
                assert!(cleared.row_text(row).unwrap().trim_end().is_empty());
            }
        }
    }
}

#[test]
fn typed_gallery_edits_follow_a_prompt_scrolled_from_the_bottom_row() {
    let mut keys = vec![b'\r'; 9];
    let mut bottom = typed_terminal(&keys);
    assert_eq!(bottom.cell_frame(14, 72).cursor_row, 13);
    keys.extend_from_slice(&[b'x'; 71]);
    let scrolled = typed_terminal(&keys);
    let full = format!("$ {}", "x".repeat(70));
    assert_eq!(scrolled.row_text(12).as_deref(), Some(full.as_str()));
    assert_eq!(scrolled.row_text(13).as_deref(), Some("x"));

    keys.push(127);
    let mut deleted = typed_terminal(&keys);
    assert_eq!(deleted.row_text(12).unwrap().trim_end(), full);
    assert!(deleted.row_text(13).unwrap().trim_end().is_empty());
    let frame = deleted.cell_frame(14, 72);
    assert_eq!((frame.cursor_row, frame.cursor_col), (12, 71));
    keys.extend_from_slice(b"xxxxxxxxxx\x15");
    let mut cleared = typed_terminal(&keys);
    assert_eq!(cleared.row_text(12).unwrap().trim_end(), "$");
    assert!(cleared.row_text(13).unwrap().trim_end().is_empty());
    let frame = cleared.cell_frame(14, 72);
    assert_eq!((frame.cursor_row, frame.cursor_col), (12, 2));

    for (cols, row) in [(1_u16, 0), (2, 0), (8, 3)] {
        let mut keys = vec![b'x'; 80];
        let full = typed_terminal_at(&keys, cols);
        assert!(full.visible_content().contains('x'));
        if cols == 8 {
            assert!(full.row_text(row).unwrap().starts_with("$ "));
        } else {
            assert!(!full.visible_content().contains('$'));
        }
        keys.push(21);
        let mut cleared = typed_terminal_at(&keys, cols);
        let mut expected = aterm_core::terminal::Terminal::new(14, cols);
        expected.process(format!("\x1b[2J\x1b[{};1H$ ", row + 1).as_bytes());
        for row in 0..14 {
            assert_eq!(
                cleared.row_text(row).unwrap().trim_end(),
                expected.row_text(row).unwrap().trim_end()
            );
        }
        let frame = cleared.cell_frame(14, usize::from(cols));
        let expected_frame = expected.cell_frame(14, usize::from(cols));
        assert_eq!(
            (frame.cursor_row, frame.cursor_col),
            (expected_frame.cursor_row, expected_frame.cursor_col)
        );
    }
}

#[test]
fn typed_gallery_resize_preserves_held_data_and_unknown_geometry_refuses() {
    let mut output = Vec::new();
    let mut looks = 0;
    typing_fixture(&mut std::io::Cursor::new(b"MMMM MMMM"), &mut output, || {
        looks += 1;
        Some(if looks < 4 { (14, 72) } else { (14, 12) })
    })
    .unwrap();
    let mut term = aterm_core::terminal::Terminal::new(14, 12);
    term.process(&output);
    assert_eq!(term.row_text(0).unwrap().trim_end(), "$ MMMM MMMM");
    assert!(term.row_text(4).unwrap().trim_end().is_empty());

    for size in [None, Some((0, 72)), Some((14, 0))] {
        let mut output = Vec::new();
        assert!(typing_fixture(&mut std::io::Cursor::new(b"x"), &mut output, || size).is_err());
        assert!(output.is_empty());
    }
}

fn focus_terminal(keys: &[u8]) -> aterm_core::terminal::Terminal {
    let mut output = Vec::new();
    line_fixture(
        &mut std::io::Cursor::new(keys),
        &mut output,
        LineMode::Focus,
        || Some((14, 32)),
    )
    .unwrap();
    let mut term = aterm_core::terminal::Terminal::new(14, 32);
    term.process(&output);
    term
}

#[test]
fn focus_reports_restore_a_populated_composer_before_its_space_and_word_wrap() {
    let warm = format!("{}shared", "a".repeat(54));
    let mut keys = warm.as_bytes().to_vec();
    keys.extend_from_slice(b"\x1b[O\x1b[I");
    for (count, expected) in [
        (0, (5, 30, false)),
        (1, (5, 31, false)),
        (2, (5, 31, true)),
        (3, (6, 1, false)),
        (4, (6, 2, false)),
        (5, (6, 3, false)),
        (6, (6, 4, false)),
    ] {
        let mut input = keys.clone();
        input.extend_from_slice(&b" state"[..count]);
        let term = focus_terminal(&input);
        let cursor = term.grid().cursor();
        assert_eq!(
            (cursor.row, cursor.col, term.grid().pending_wrap()),
            expected,
            "after {count} suffix keys"
        );
        assert_eq!(
            term.row_text(4).as_deref(),
            Some(format!("> {}", "a".repeat(30)).as_str())
        );
        assert_eq!(term.row_text(7).unwrap().trim_end(), "focus in");
    }
    keys.extend_from_slice(b" state");
    let term = focus_terminal(&keys);
    assert_eq!(
        term.row_text(5).as_deref(),
        Some(format!("{}shared s", "a".repeat(24)).as_str())
    );
    assert_eq!(term.row_text(6).unwrap().trim_end(), "tate");

    // The original scenario still starts with an empty composer.
    let empty = focus_terminal(b"\x1b[O\x1b[Iecho");
    assert_eq!(empty.row_text(4).unwrap().trim_end(), "> echo");
    assert_eq!(empty.row_text(7).unwrap().trim_end(), "focus in");
}

#[test]
fn focus_restore_preserves_pending_wrap_and_plain_cup_is_a_negative_control() {
    for len in [30, 62] {
        let mut keys = vec![b'x'; len];
        keys.extend_from_slice(b"\x1b[O\x1b[I");
        let mut returned = focus_terminal(&keys);
        let row = if len == 30 { 4 } else { 5 };
        assert_eq!(returned.grid().cursor().row, row);
        assert_eq!(returned.grid().cursor().col, 31);
        assert!(returned.grid().pending_wrap());

        let mut lost = focus_terminal(&keys);
        lost.process(format!("\x1b[{};32H", row + 1).as_bytes());
        assert!(!lost.grid().pending_wrap(), "CUP loses the pending state");
        returned.process(b"z");
        lost.process(b"z");
        assert_eq!(returned.grid().cursor().row, row + 1);
        assert_eq!(returned.grid().cursor().col, 1);
        assert_eq!(
            returned.row_text(usize::from(row + 1)).unwrap().trim_end(),
            "z"
        );
        assert_eq!(lost.grid().cursor().row, row);
        assert!(lost.row_text(usize::from(row)).unwrap().ends_with('z'));
    }
}

#[test]
fn compiled_gallery_programs_preserve_an_occupied_script_and_argument_boundaries() {
    let root = crate::test_scratch_dir("compiled");
    let lab = root.path().join("lab with spaces");
    let script = root.path().join("paint.sh");
    let typing = gallery_program(root.path(), Scenario::Typing, Theme::Light, None, &lab).unwrap();
    assert_eq!(
        typing,
        vec![
            lab.to_string_lossy().into_owned(),
            "__trail-redraw-fixture".into(),
            "typing".into()
        ]
    );
    assert!(!script.exists(), "the compiled route creates no script");

    let kept = b"existing inert fixture data";
    std::fs::write(&script, kept).unwrap();
    for (scenario, mode) in [
        (Scenario::Typing, Some("typing")),
        (Scenario::Settings, None),
        (Scenario::RedrawStall, None),
        (Scenario::FoldRest, Some("fold")),
        (Scenario::FocusRoundtrip, Some("focus")),
        (Scenario::FocusWrap, Some("focus")),
        (Scenario::LastRow, Some("last-row")),
        (Scenario::Coloured, Some("coloured")),
        (Scenario::Flood, Some("flood")),
        (Scenario::RailG, Some("rail-g")),
    ] {
        let args = gallery_program(root.path(), scenario, Theme::Dark, None, &lab).unwrap();
        assert_eq!(args[0], lab.to_string_lossy().as_ref());
        assert_eq!(args[1], "__trail-redraw-fixture");
        assert_eq!(args.get(2).map(String::as_str), mode);
        assert_eq!(args.len(), 2 + usize::from(mode.is_some()));
        assert_eq!(std::fs::read(&script).unwrap(), kept);
    }
}

struct Wire {
    replies: std::io::Cursor<Vec<u8>>,
    sent: Vec<u8>,
}

impl std::io::Read for Wire {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        std::io::Read::read(&mut self.replies, buf)
    }
}

impl std::io::Write for Wire {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.sent.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn wire(replies: &str) -> RelayClient<Wire> {
    RelayClient::new(Wire {
        replies: std::io::Cursor::new(replies.as_bytes().to_vec()),
        sent: Vec::new(),
    })
}

#[test]
fn held_snapshot_reads_keep_numeric_status_lines_and_counted_bodies_separate() {
    let mut client =
        wire("OK 4 26 1 blinking_block\nOK 2\nfirst\nsecond\nOK matched=7\nERR denied\n");
    let cursor = relay_reply(&mut client, &["cursor"], ReplyShape::Line).unwrap();
    assert_eq!(cursor.stdout, "OK 4 26 1 blinking_block\n");
    let text = relay_reply(&mut client, &["text"], ReplyShape::Counted).unwrap();
    assert_eq!(text.stdout, "first\nsecond\n");
    let ready = relay_reply(
        &mut client,
        &["await", "match", "redraw.restored"],
        ReplyShape::Line,
    )
    .unwrap();
    assert_eq!(ready.code, 0);
    let refused = relay_reply(&mut client, &["trail", "status"], ReplyShape::Counted).unwrap();
    assert_eq!(refused.code, 1);
    assert!(refused.stdout.is_empty());
    assert_eq!(
        client.transport().sent,
        b"cursor\ntext\nawait match redraw.restored\ntrail status\n"
    );
}

fn png_bytes() -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut encoder = aterm_png::Encoder::new(&mut bytes, 1, 1);
    encoder.set_color(aterm_png::ColorType::Rgb);
    encoder.set_depth(aterm_png::BitDepth::Eight);
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(&[1, 2, 3]).unwrap();
    writer.finish().unwrap();
    bytes
}

#[test]
fn inline_capture_validates_png_bytes_and_dimensions_without_an_artifact_ack() {
    let png = png_bytes();
    let encoded = aterm_codec::base64::encode(&png).unwrap();
    let metadata = "image-meta image-meta-version=3 frame-kind=terminal";
    let body = format!("{metadata}\n1 1 {} {encoded}\n", png.len());
    let mut client = wire(&format!("OK 2\n{body}OK 1\ntrail next\n"));
    let reply = relay_reply(
        &mut client,
        &["image", "--meta", "--bytes"],
        ReplyShape::Counted,
    )
    .unwrap();
    let (decoded, meta) = image_bytes(&reply.stdout).unwrap();
    assert_eq!(decoded, png);
    assert_eq!(meta, metadata);
    assert_eq!(
        relay_reply(&mut client, &["trail", "status"], ReplyShape::Counted)
            .unwrap()
            .stdout,
        "trail next\n"
    );

    for bad in [
        body.replacen("\n1 1 ", "\n2 1 ", 1),
        format!("{metadata}\n1 1 {} {encoded}\n", png.len() + 1),
        format!("{metadata}\n1 1 18446744073709551615 {encoded}\n"),
        format!(
            "{metadata}\n1 1 {} {}!\n",
            png.len(),
            &encoded[..encoded.len() - 1]
        ),
        format!("{body}extra\n"),
        "image-meta frame-kind=terminal\n1 1 3 YWJj\n".to_string(),
    ] {
        assert!(image_bytes(&bad).is_err());
    }
}

#[test]
fn a_returned_capture_keeps_status_cursor_and_text_replies_separate() {
    let mut client = wire("OK 1\ntrail returned\nOK 4 26 1 blinking_block\nOK 1\nreturned text\n");
    let returned =
        frame_evidence(|request, shape| relay_reply(&mut client, request, shape).unwrap());
    assert_eq!(returned.status.stdout, "trail returned\n");
    assert_eq!(returned.cursor.stdout, "OK 4 26 1 blinking_block\n");
    assert_eq!(returned.text.stdout, "returned text\n");
    assert_eq!(client.transport().sent, b"trail status\ncursor\ntext\n");
}

#[test]
fn recording_cost_cannot_extend_the_redraw_hold_or_replace_its_boundary_marks() {
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Boundary {
        at: u64,
        open: bool,
        status: String,
    }

    #[derive(Default)]
    struct Recorder {
        now: u64,
        open: bool,
        status: String,
        boundaries: Option<(Boundary, Boundary)>,
        operations: Vec<(&'static str, bool)>,
    }

    impl Recorder {
        fn toggle(&mut self) -> Boundary {
            self.open = !self.open;
            self.status = if self.open { "held" } else { "released" }.into();
            Boundary {
                at: self.now,
                open: self.open,
                status: self.status.clone(),
            }
        }

        fn record(&mut self, clean: bool, cost: u64) {
            for operation in ["image", "sidecars", "artifact"] {
                self.operations.push((operation, self.open));
                self.now += cost;
            }
            if clean {
                for operation in ["clean image", "clean artifact"] {
                    self.operations.push((operation, self.open));
                    self.now += cost;
                }
            }
            self.status = "later recording state".into();
        }
    }

    let run = |clean, cost, before_release| {
        let mut recorder = Recorder::default();
        record_after_hold(
            &mut recorder,
            Recorder::toggle,
            |state, opened| {
                state.now = opened.at + REDRAW_HOLD_MS;
                if before_release {
                    state.record(clean, cost);
                }
                Ok::<(), ()>(())
            },
            |state, opened, released| {
                state.boundaries = Some((opened, released));
                state.record(clean, cost);
                Ok(())
            },
        )
        .unwrap();
        recorder
    };

    for clean in [false, true] {
        for cost in [0, 3_000] {
            let recorded = run(clean, cost, false);
            let (opened, released) = recorded.boundaries.as_ref().unwrap();
            assert_eq!(released.at - opened.at, REDRAW_HOLD_MS);
            assert!(opened.open && !released.open);
            assert_eq!(opened.status, "held");
            assert_eq!(released.status, "released");
            assert_eq!(recorded.status, "later recording state");
            assert!(recorded.operations.iter().all(|(_, open)| !open));

            // The previous driver encoded its open frame and twin before release.
            // The same fake detects that cost even though the intended hold is unchanged.
            let previous = run(clean, cost, true);
            let (opened, released) = previous.boundaries.as_ref().unwrap();
            assert!(previous.operations.iter().any(|(_, open)| *open));
            if cost > 0 {
                assert!(released.at - opened.at > REDRAW_HOLD_MS);
            }
        }
    }
}

#[test]
fn an_interrupted_hold_is_released_without_recording() {
    let mut held = false;
    let stopped = record_after_hold(
        &mut held,
        |held| {
            *held = !*held;
        },
        |held, _| {
            assert!(*held);
            Err(Stopped(libc::SIGINT))
        },
        |_, _, _| panic!("an interrupted hold records no capture"),
    );
    assert_eq!(stopped, Err(Stopped(libc::SIGINT)));
    assert!(!held);
}

#[test]
fn a_requested_rainbow_return_losing_eligibility_is_inconclusive() {
    let live = "trail resolved=rainbow-kitty config_enabled=true effective=true intensity=1.00 ribbon_segments=12 ribbon_drawn=30";
    let expected = expects_rainbow(&config_lines("rainbow", Theme::Dark, &[])).unwrap();
    assert!(expected);
    let mut wrongly_measured = Vec::new();
    for (loss, line) in [
        (
            "disabled",
            live.replace("config_enabled=true", "config_enabled=false"),
        ),
        (
            "ineffective",
            live.replace("effective=true", "effective=false"),
        ),
        ("unlit", live.replace("intensity=1.00", "intensity=0.00")),
        ("changed style", live.replace("rainbow-kitty", "comet")),
    ] {
        let measured = return_measurement(
            expected,
            &Reply {
                stdout: line,
                ..Reply::default()
            },
        )
        .unwrap();
        if measured != ReturnMeasurement::Inconclusive {
            wrongly_measured.push((loss, measured));
        }
    }
    assert!(
        wrongly_measured.is_empty(),
        "a requested rainbow was not measured: {wrongly_measured:?}"
    );
}

#[test]
fn a_return_without_a_live_rainbow_is_inconclusive_but_other_styles_are_not() {
    let live = "trail resolved=rainbow-kitty config_enabled=true effective=true intensity=1.00 ribbon_segments=12 ribbon_drawn=30";
    let status = |line: String| Reply {
        stdout: line,
        ..Reply::default()
    };
    assert_eq!(
        return_measurement(true, &status(live.into())).unwrap(),
        ReturnMeasurement::LiveRibbon
    );
    for dead in [
        live.replace("segments=12", "segments=0"),
        live.replace("drawn=30", "drawn=0"),
    ] {
        assert_eq!(
            return_measurement(true, &status(dead)).unwrap(),
            ReturnMeasurement::Inconclusive
        );
    }
    for style in ["comet", "off"] {
        let expected = expects_rainbow(&config_lines(style, Theme::Dark, &[])).unwrap();
        assert!(!expected);
        assert_eq!(
            return_measurement(expected, &status(live.replace("rainbow-kitty", style))).unwrap(),
            ReturnMeasurement::NotApplicable
        );
    }
    for missing in [
        live.replace("ribbon_drawn=30", ""),
        live.replace("intensity=1.00", "intensity=NaN"),
        live.replace("intensity=1.00", "intensity=-1.00"),
        live.replace("rainbow-kitty", "unknown"),
        live.replace("config_enabled=true", "config_enabled=maybe"),
        live.replace("effective=true", "effective=maybe"),
        live.replace("ribbon_segments=12", "ribbon_segments=NaN"),
    ] {
        for expected in [false, true] {
            assert!(return_measurement(expected, &status(missing.clone())).is_err());
        }
    }
    for expected in [false, true] {
        assert!(
            return_measurement(
                expected,
                &Reply {
                    code: 1,
                    ..Reply::default()
                }
            )
            .is_err()
        );
    }
}

#[test]
fn rainbow_measurement_intent_uses_the_final_config_and_shared_style_and_motion_parsers() {
    for (style, extra, expected) in [
        ("rainbow", None, true),
        ("off", None, false),
        ("comet", None, false),
        ("rainbow", Some("cursor_trail_style = 'comet'"), false),
        ("comet", Some("cursor_trail_style = 'rainbow'"), true),
        ("rainbow", Some("cursor_trail_style = 'off'"), false),
        ("rainbow", Some("cursor_trail = false"), false),
        ("rainbow", Some("motion = ' REDUCE '"), false),
        // `off` and `none` ask for no motion: the shared parser reads them as
        // Reduced (settings demo round 3), so no rainbow is expected.
        ("rainbow", Some("motion = 'off'"), false),
        ("rainbow", Some("motion = 'none'"), false),
        ("rainbow", Some("motion = 'unknown'"), true),
        ("rainbow", Some("serious_mode = true"), false),
        ("rainbow", Some("cursor_trail_intensity = 0"), false),
        ("rainbow", Some("cursor_trail_intensity = 0.0"), false),
        ("rainbow", Some("cursor_trail_intensity = -1.0"), false),
        ("rainbow", Some("cursor_trail_intensity = nan"), false),
        ("rainbow", Some("cursor_trail_intensity = inf"), false),
        ("rainbow", Some("cursor_trail_intensity = 0.1"), true),
        ("rainbow", Some("cursor_trail_intensity = 2"), true),
    ] {
        let extra: Vec<String> = extra.into_iter().map(str::to_owned).collect();
        let config = config_lines(style, Theme::Dark, &extra);
        assert_eq!(expects_rainbow(&config).unwrap(), expected, "{config:?}");
    }
    for extra in [
        "cursor_trail_style = 'unknown'",
        // A retired name is no style.
        "cursor_trail_style = 'nyan'",
        "cursor_trail_style = 'rainbow kitty pet'",
        "cursor_trail_style = 7",
        "cursor_trail = 'false'",
        "motion = false",
        "serious_mode = 'true'",
        "cursor_trail_intensity = '0'",
        "cursor_trail_intensity = [",
    ] {
        let config = config_lines("rainbow", Theme::Dark, &[extra.into()]);
        assert!(expects_rainbow(&config).is_err(), "{config:?}");
    }
}

fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_string()).collect()
}

fn scratch(tag: &str) -> crate::TestScratch {
    crate::TestScratch::new(tag)
}

#[test]
fn the_defaults_are_typing_on_both_themes_with_clean_twins() {
    let a = parse_args(&args(&["/x/g"])).unwrap();
    assert_eq!(a.out, "/x/g");
    assert!(a.styles.is_empty());
    assert_eq!(a.themes, vec![Theme::Dark, Theme::Light]);
    assert_eq!(a.scenarios, vec![Scenario::Typing]);
    assert!(a.clean);
    assert_eq!(
        (a.columns, a.lines, a.font_px, a.aterm),
        (None, None, None, None)
    );
}

#[test]
fn options_go_anywhere_and_scenarios_repeat_in_order_once() {
    let a = parse_args(&args(&[
        "--scenario",
        "steam,typing",
        "/x/g",
        "phaser",
        "--theme",
        "light",
        "classic mono",
        "--scenario",
        "typing,cjk",
        "--extra",
        "motion = \"reduced\"",
        "--columns",
        "80",
        "--no-clean",
        "--aterm",
        "/b/aterm",
    ]))
    .unwrap();
    assert_eq!(a.styles, vec!["phaser", "classic mono"]);
    assert_eq!(a.themes, vec![Theme::Light]);
    assert_eq!(
        a.scenarios,
        vec![Scenario::Steam, Scenario::Typing, Scenario::Cjk]
    );
    assert_eq!(a.extras, vec!["motion = \"reduced\""]);
    assert_eq!(a.columns, Some(80));
    assert!(!a.clean);
    assert_eq!(a.aterm.as_deref(), Some("/b/aterm"));
    let all = parse_args(&args(&["--scenario", "all", "/x/g"])).unwrap();
    assert_eq!(all.scenarios, Scenario::ALL.to_vec());
}

#[test]
fn focus_wrap_keeps_its_fixed_grid_without_changing_other_scenario_overrides() {
    let selected = parse_args(&args(&["--scenario", "focus-wrap", "/x/g"])).unwrap();
    assert_eq!(selected.scenarios, vec![Scenario::FocusWrap]);
    assert_eq!(Scenario::FocusWrap.grid(), (32, 14, 22));
    let custom = parse_args(&args(&[
        "--scenario",
        "all",
        "--columns",
        "80",
        "--lines",
        "18",
        "--font-px",
        "20",
        "/x/g",
    ]))
    .unwrap();
    assert_eq!(Scenario::FocusWrap.configured_grid(&custom), (32, 14, 20));
    assert_eq!(Scenario::Typing.configured_grid(&custom), (80, 18, 20));
    assert_eq!(
        Scenario::FocusRoundtrip.configured_grid(&custom),
        (80, 18, 20)
    );
}

#[test]
fn a_malformed_request_is_a_usage_error() {
    for bad in [
        &[][..],
        &["--theme", "sepia", "/x/g"],
        &["--scenario", "typing,nope", "/x/g"],
        &["--extra", "cursor_blink", "/x/g"],
        &["--extra", "[update] = 1", "/x/g"],
        &["--extra", "agents_auto_prime = true", "/x/g"],
        &["--columns", "0", "/x/g"],
        &["--lines", "many", "/x/g"],
        &["--bogus", "/x/g"],
        &["/x/g", "pha\"ser"],
        &["/x/g", "--theme"],
    ] {
        let e = parse_args(&args(bad)).expect_err(&format!("{bad:?} is refused"));
        assert_eq!(e.code, 2, "{bad:?}: {:?}", e.msg);
    }
}

#[test]
fn an_extra_line_replaces_the_base_line_of_its_key() {
    let base = config_lines("phaser", Theme::Dark, &[]);
    assert_eq!(
        base,
        vec![
            "cursor_trail_style = \"phaser\"",
            "cursor_pet = \"cat\"",
            "motion = \"full\"",
            "trail_sounds = false",
        ]
    );
    // The pet is named, the default unless an extra names another: a scratch file that
    // names a style and no pet would be migrated to the pet that style once had.
    let dog = config_lines("fire", Theme::Dark, &["cursor_pet = \"dog\"".to_string()]);
    let pets: Vec<&String> = dog.iter().filter(|l| toml_key(l) == "cursor_pet").collect();
    assert_eq!(pets, vec!["cursor_pet = \"dog\""]);
    let light = config_lines("fire", Theme::Light, &["motion=\"reduced\"".to_string()]);
    assert!(light.contains(&"theme = \"GitHub Light\"".to_string()));
    let motions: Vec<&String> = light.iter().filter(|l| toml_key(l) == "motion").collect();
    assert_eq!(motions, vec!["motion=\"reduced\""]);
    assert_eq!(light.last().map(String::as_str), Some("motion=\"reduced\""));
}

#[test]
fn names_are_the_audit_gallerys() {
    assert_eq!(slug("Rainbow Kitty: pet"), "rainbow-kitty--pet");
    assert_eq!(slug("classic mono"), "classic-mono");
    assert_eq!(
        instance_name("classic mono", Theme::Dark, Scenario::Typing),
        "classic-mono-dark"
    );
    assert_eq!(
        instance_name("off", Theme::Light, Scenario::EnterBottom),
        "off-light-enter-bottom"
    );
    for s in Scenario::ALL {
        assert_eq!(Scenario::parse(s.name()), Some(s));
    }
}

#[test]
fn every_paint_ends_in_an_interactive_shell_and_context_paints_its_theme() {
    for s in Scenario::ALL {
        for t in [Theme::Dark, Theme::Light] {
            let p = paint(s, t, None);
            assert!(
                p.starts_with("LC_ALL=en_US.UTF-8; export LC_ALL\n"),
                "{s:?}"
            );
            assert!(p.ends_with("\nexec /bin/sh -i\n"), "{s:?}");
        }
    }
    assert!(paint(Scenario::Context, Theme::Dark, None).contains("\\033[48;2;51;65;94m"));
    assert!(paint(Scenario::Context, Theme::Light, None).contains("\\033[48;2;182;227;255m"));
    assert!(!paint(Scenario::Context, Theme::Light, None).contains("@SEL@"));
}

#[test]
fn the_roster_is_the_pickers_options() {
    let prefs = "field key=cursor_trail_enabled label=\"Trail\" value=\"true\"\n\
                 field key=cursor_trail_style label=\"Cursor style\" value=\"beam\" effective=\"Beam\" kind=enum \
                 options=[off,rainbow,phaser,classic] visible=true\n";
    assert_eq!(
        roster_of(prefs),
        Some(args(&["off", "rainbow", "phaser", "classic"]))
    );
    assert_eq!(roster_of("field key=cursor_trail_style options=[]"), None);
    assert_eq!(roster_of("nothing here"), None);
}

#[test]
fn an_output_dir_inside_a_checkout_is_refused_however_it_is_spelled() {
    let root = scratch("out");
    let checkout = root.join("checkout");
    std::fs::create_dir_all(checkout.join("src")).unwrap();
    std::fs::create_dir_all(root.join("outside")).unwrap();
    std::os::unix::fs::symlink(&checkout, root.join("link")).unwrap();
    let co = vec![checkout.clone()];
    let s = |p: PathBuf| p.to_string_lossy().into_owned();

    for inside in [
        checkout.join("g"),
        checkout.join("src/new/deeper"),
        root.join("link/g"),
        root.join("outside/../checkout/g"),
        checkout.clone(),
    ] {
        let e = resolve_out(&s(inside.clone()), &co).expect_err(&format!("{inside:?}"));
        assert!(e.contains("inside the repository"), "{e}");
    }
    assert_eq!(
        resolve_out(&s(root.join("outside/new/deeper")), &co).unwrap(),
        root.join("outside/new/deeper")
    );
    assert_eq!(
        resolve_out(&s(root.join("link/../outside/g")), &co).unwrap(),
        root.join("outside/g"),
        "a link resolves before its `..`, as the kernel reads it"
    );
    let e = resolve_out(&s(root.join("outside/missing/../g")), &co).unwrap_err();
    assert!(e.contains("climbs"), "{e}");
}

#[test]
fn the_checkouts_are_read_from_gits_own_files() {
    let root = scratch("git");
    let main = root.join("main");
    let wt = root.join("wt");
    let meta = main.join(".git/worktrees/wt");
    std::fs::create_dir_all(&meta).unwrap();
    std::fs::create_dir_all(&wt).unwrap();
    std::fs::write(
        meta.join("gitdir"),
        format!("{}\n", wt.join(".git").display()),
    )
    .unwrap();
    std::fs::write(meta.join("commondir"), "../..\n").unwrap();
    std::fs::write(wt.join(".git"), format!("gitdir: {}\n", meta.display())).unwrap();

    let from_wt = checkouts(&wt);
    assert!(from_wt.contains(&main), "{from_wt:?}");
    assert!(from_wt.contains(&wt), "{from_wt:?}");
    let from_main = checkouts(&main);
    assert!(from_main.contains(&wt), "{from_main:?}");
    assert_eq!(checkouts(&root), vec![root.to_path_buf()]);
}

/// The cat a schedule of forward keys (ms from the first) earns, read at
/// each of `at` (ms from the first key): its opacity and whether it is
/// fading out, presented every 16 ms as a window presents it.
fn flying_kitty(keys: &[u64], at: &[u64]) -> Vec<(u8, bool)> {
    use aterm_effects::kitty_cursor::CursorCat;
    let t0 = std::time::Instant::now();
    let mut cat = CursorCat::default();
    let mut out = vec![(0u8, false); at.len()];
    let end = at
        .iter()
        .copied()
        .chain(keys.iter().copied())
        .max()
        .unwrap_or(0)
        + 16;
    let mut next_key = keys.iter().copied().peekable();
    for ms in 0..=end {
        let now = t0 + Duration::from_millis(ms);
        let mut keyed = false;
        while next_key.peek() == Some(&ms) {
            next_key.next();
            cat.on_key(now, true);
            keyed = true;
        }
        let read: Vec<usize> = (0..at.len()).filter(|&i| at[i] == ms).collect();
        if keyed || ms % 16 == 0 || !read.is_empty() {
            let f = cat.frame(now);
            for i in read {
                out[i] = (f.alpha, f.fade_out > 0.0);
            }
        }
    }
    out
}

/// THE BURST EARNS THE FLYING KITTY, whose head the typing momentum held in
/// the cat's band for its dwell earns (a burst that falls short leaves
/// `flying kitty` and `nyan rainbow` with no kitty in any gallery frame).
/// `burst`'s own keys at its pace, and at a slow frame's (each key 80 ms), fed
/// to the cat the window runs (`CursorCat`, presented every 16 ms): at its `fly`
/// frame (30 ms after key [`BURST_FLY_KEY`]), at `t0` and through `t470` the kitty
/// flies whole; and of its landing frames (`l…`) at least two catch it
/// landing (fading out on its exit), none before it has flown. NEGATIVE
/// CONTROL: the `typing` scenario's line at its 60 ms never earns it: no
/// frame from its first key to a second past its last draws the cat.
#[test]
fn the_burst_earns_the_flying_kitty_and_typing_does_not() {
    let n = burst_keys().len();
    assert!(
        n + 2 < 72 && BURST_FLY_KEY + 1 < n,
        "fixture: the line fits its row and flies before its end"
    );
    for every in [BURST_KEY_MS, 80] {
        let keys: Vec<u64> = (0..n as u64).map(|k| k * every).collect();
        let last = keys[n - 1];
        let mut at = vec![keys[BURST_FLY_KEY] + 30, last + 30];
        at.extend(BURST_AFTER_MS.iter().map(|ms| last + 30 + ms));
        let seen = flying_kitty(&keys, &at);
        let labels: Vec<String> = ["fly".to_string(), "t0".to_string()]
            .into_iter()
            .chain(BURST_AFTER_MS.iter().map(|&ms| burst_frame(ms)))
            .collect();
        for (label, &(alpha, fading)) in labels.iter().zip(&seen) {
            if label.starts_with('l') {
                continue;
            }
            assert!(
                alpha == 255 && !fading,
                "a key per {every} ms: at `{label}` the kitty is not flying whole (alpha \
                 {alpha}, fading {fading}): {seen:?}"
            );
        }
        let landing = labels
            .iter()
            .zip(&seen)
            .filter(|(l, (alpha, fading))| l.starts_with('l') && *fading && *alpha > 0)
            .count();
        assert!(
            landing >= 2,
            "a key per {every} ms: {landing} of the landing frames catch the kitty \
             landing: {:?}",
            labels.iter().zip(&seen).collect::<Vec<_>>()
        );
    }
    let typing = keys_of(TYPING_TEXT);
    let keys: Vec<u64> = (0..typing.len() as u64)
        .map(|k| k * TYPING_KEY_MS)
        .collect();
    let at: Vec<u64> = (0..=keys[keys.len() - 1] + 1000).step_by(10).collect();
    let drawn = flying_kitty(&keys, &at)
        .iter()
        .filter(|(alpha, _)| *alpha > 0)
        .count();
    assert_eq!(
        drawn, 0,
        "control: the typing scenario's line draws the kitty in {drawn} frames"
    );
}

#[test]
fn burst_frames_are_named_for_their_time() {
    assert_eq!(burst_frame(120), "t120");
    assert_eq!(burst_frame(1650), "l1650");
    assert_eq!(burst_frame(3600), "l3600");
    assert_eq!(burst_start_frame(3), "key3");
    assert!(
        BURST_START_KEYS.iter().all(|&k| k < BURST_FLY_KEY),
        "the start's frames come before `fly`"
    );
    assert_eq!(
        instance_name("classic mono", Theme::Dark, Scenario::Burst),
        "classic-mono-dark-burst"
    );
    assert_eq!(
        paint(Scenario::Burst, Theme::Dark, None),
        paint(Scenario::Typing, Theme::Dark, None)
    );
}

/// THE SPLIT PHOTOGRAPHS THE CARET'S PANE: `split` reads the window's
/// `panes` after it splits and refocuses, and goes on only when the focused
/// pane is the first in reading order (the one its paint drew the prompt
/// in). A layout row out of order is read in place; one pane, or none
/// focused, is no split. NEGATIVE CONTROL: the new pane left focused (the
/// second) is not the caret's.
#[test]
fn the_split_keeps_the_caret_s_pane_focused() {
    let after = "layout tab=0 panes=2 zoomed=false terminal=true\n\
                 pane session=2 rect=7,0,6x72 focused=false\n\
                 pane session=1 rect=0,0,6x72 focused=true\n";
    assert_eq!(panes_focus(after), Some(0));
    let left = "layout tab=0 panes=2 zoomed=false terminal=true\n\
                pane session=1 rect=0,0,6x72 focused=false\n\
                pane session=2 rect=7,0,6x72 focused=true\n";
    assert_eq!(panes_focus(left), Some(1), "control: the new pane focused");
    assert_eq!(
        panes_focus("layout tab=0 panes=1\npane session=1 rect=0,0,14x72 focused=true\n"),
        None
    );
    assert_eq!(
        panes_focus(
            "pane session=1 rect=0,0,6x72 focused=false\npane session=2 rect=7,0,6x72 focused=false\n"
        ),
        None
    );
    assert_eq!(SPLIT_PANES, ["split=h", "up"]);
}

/// A SPLIT'S FRAMES ARE JUDGED ON THE CARET'S PANE: the window's dims are
/// narrowed to the focused pane, its origin moved down by the pane's row
/// (the pane header above it) and across by its column, so the judge reads
/// the caret and the typed line where the pane draws them. NEGATIVE
/// CONTROL: judged on the window's dims, the caret's row 4 lies a row (26
/// px) above where the pane draws it.
#[test]
fn a_split_is_judged_on_the_caret_s_pane() {
    let window = "OK 14 72 1008 364 session=0 cell_w=14 cell_h=26 font_px=22.00 window=0 \
                  pad=12 pad_top=2 pad_bottom=12 head=0 tab_rows=0";
    let panes = "layout tab=0 panes=2 zoomed=false terminal=true\n\
                 pane session=0 rect=1,0,6x72 focused=true\n\
                 pane session=1 rect=9,0,5x72 focused=false\n";
    let pane = pane_dims(window, panes).expect("the focused pane's dims");
    let g = crate::trail_contrast::grid_from_dims(&pane).expect("the pane's grid");
    assert_eq!(
        (g.ox, g.oy, g.cw, g.ch, g.rows, g.cols),
        (12, 28, 14, 26, 6, 72)
    );
    let w = crate::trail_contrast::grid_from_dims(window).expect("the window's grid");
    assert_eq!(
        g.oy + 4 * g.ch - (w.oy + 4 * w.ch),
        26,
        "control: the window's grid reads the caret's row a row high"
    );
    let right = "layout tab=0 panes=2\n\
                 pane session=1 rect=0,0,14x35 focused=false\n\
                 pane session=2 rect=0,37,14x35 focused=true\n";
    let g = crate::trail_contrast::grid_from_dims(&pane_dims(window, right).unwrap()).unwrap();
    assert_eq!((g.ox, g.oy, g.cols), (12 + 37 * 14, 2, 35));
    assert_eq!(
        pane_dims(
            window,
            "layout tab=0 panes=1\npane session=1 rect=0,0,14x72 focused=true\n"
        ),
        None
    );
    assert_eq!(pane_dims("ERR no window", panes), None);
}

/// THE WIDENED SCENARIOS ARE NAMED AND PAINTED FOR WHAT THEY PHOTOGRAPH:
/// `split`'s burst frames are its own (none is a typing frame's label, so a
/// measure tells the two lines apart), and `move-up`'s program moves the
/// caret up [`MOVE_UP_ROWS`] rows in the same write as its mark, over
/// lettered rows its paint draws above the prompt, and holds it there until
/// its terminal goes. NEGATIVE CONTROLS: every other paint carries no such
/// program; with `sleep 4` back in `./k` (2026-11-05) this fired "the program
/// holds the move by reading its terminal, not by a clock", and a whole
/// gallery of that program with `u470` delayed to 6 s took the prompt's line
/// into that frame's sidecars, where this program's frame kept the page.
#[test]
fn the_widened_scenarios_name_their_frames_and_paint_their_page() {
    assert_eq!(
        instance_name("classic", Theme::Light, Scenario::MoveUp),
        "classic-light-move-up"
    );
    assert_eq!(
        instance_name("classic mono", Theme::Dark, Scenario::Split),
        "classic-mono-dark-split"
    );
    for label in SPLIT_BURST_FRAMES {
        assert!(
            !["mid", "t0", "t120", "t470"].contains(&label),
            "{label} is a typing frame's"
        );
    }
    let page = paint(Scenario::MoveUp, Theme::Light, None);
    let program = format!("printf '[vim] {MOVE_UP_MARK}\\033[{MOVE_UP_ROWS}A");
    assert!(page.contains(&program), "{page}");
    let held = &page[page.find(&program).expect("the move")..];
    assert!(
        held.contains("read -r held") && !held.contains("sleep"),
        "the program holds the move by reading its terminal, not by a clock: {held}"
    );
    let prompt_at = page.find("printf '\\033[8H'").expect("the prompt's row");
    let lettered = page[..prompt_at].matches("\\r\\n'").count();
    assert!(
        lettered > usize::from(MOVE_UP_ROWS),
        "the move crosses lettered rows: {lettered}"
    );
    for s in Scenario::ALL.into_iter().filter(|&s| s != Scenario::MoveUp) {
        assert!(
            !paint(s, Theme::Light, None).contains(MOVE_UP_MARK),
            "{s:?}"
        );
    }
}

fn last_row_terminal(keys: &[u8], (rows, cols): (u16, u16)) -> aterm_core::terminal::Terminal {
    let mut output = Vec::new();
    line_fixture(
        &mut std::io::Cursor::new(keys),
        &mut output,
        LineMode::LastRow,
        || Some((rows, cols)),
    )
    .unwrap();
    let mut term = aterm_core::terminal::Terminal::new(rows, cols);
    term.process(&output);
    term
}

/// `COLOURED` PAINTS EACH WORD IN ITS OWN INK, as a highlighter does, and in the theme's own once
/// the line is killed, so its frames' clean twins hold the inks the glass must keep. NEGATIVE
/// CONTROL: the typing fixture leaves the same line in the page's one ink.
#[test]
fn coloured_paints_each_word_in_its_own_ink() {
    let run = |mode, keys: &[u8]| {
        let mut output = Vec::new();
        line_fixture(&mut std::io::Cursor::new(keys), &mut output, mode, || {
            Some((14, 72))
        })
        .unwrap();
        let mut term = aterm_core::terminal::Terminal::new(14, 72);
        term.process(&output);
        let frame = term.cell_frame(14, 72);
        let row = &frame.cells[4];
        let ink = |word: usize| {
            let at = row
                .iter()
                .enumerate()
                .filter(|(i, c)| c.ch == ' ' && *i > 1)
                .nth(word.wrapping_sub(1))
                .map_or(2, |(i, _)| i + 1);
            row[at].fg
        };
        ink(0) != ink(1) && ink(1) != ink(2) && row[3].fg == ink(0)
    };
    assert!(run(LineMode::Coloured, b"echo the quick"));
    assert!(run(LineMode::Coloured, b"echo\x15echo the quick"));
    assert!(!run(LineMode::Typing, b"echo the quick"));
}

/// `FLOOD`'S SCREEN IS FULL OF WORDS, EACH IN AN INK OF ITS OWN, AND ITS KEYS TYPE THROUGH THEM TO
/// THE LAST CELL with the page never scrolling: every place holds a word on the grid, no two words
/// the screen has held (its own or a typed one) share an ink, and the typed word differs from the
/// one it replaces. NEGATIVE CONTROL: the typing fixture leaves the same keys in one ink, and the
/// screen scrolled by the same keys holds its first row no more.
#[test]
fn flood_fills_its_screen_with_words_in_their_own_inks_and_types_through_them() {
    let (cols, rows, _) = Scenario::Flood.grid();
    let (rows, cols) = (rows as usize, cols as usize);
    let (grid_rows, grid_cols) = (rows as u16, cols as u16);
    let places = rows * cols / FLOOD_WORD_CELLS;
    assert_eq!(rows * cols, FLOOD_KEYS, "the default grid, a key a cell");
    assert_eq!(rows * cols % FLOOD_WORD_CELLS, 0, "a word in every place");
    let run = |keys: &[u8]| {
        let mut output = Vec::new();
        flood_fixture(&mut std::io::Cursor::new(keys), &mut output, || {
            Some((grid_rows, grid_cols))
        })
        .unwrap();
        let mut term = aterm_core::terminal::Terminal::new(grid_rows, grid_cols);
        term.process(&output);
        term
    };
    let word_at = |term: &mut aterm_core::terminal::Terminal, place: usize| {
        let frame = term.cell_frame(rows, cols);
        let (row, col) = (
            place * FLOOD_WORD_CELLS / cols,
            place * FLOOD_WORD_CELLS % cols,
        );
        let cells = &frame.cells[row][col..col + FLOOD_WORD_CELLS];
        let text: String = cells.iter().map(|c| c.ch).collect();
        assert!(cells[..5].iter().all(|c| c.fg == cells[0].fg), "{text}");
        (text, cells[0].fg)
    };

    let mut laid = run(b"");
    let frame = laid.cell_frame(rows, cols);
    assert_eq!((frame.cursor_row, frame.cursor_col), (0, 0));
    let mut inks = Vec::new();
    for place in 0..places {
        let (text, ink) = word_at(&mut laid, place);
        assert_eq!(
            text,
            format!("{} ", FLOOD_WORDS[place % FLOOD_WORDS.len()]),
            "place {place}"
        );
        inks.push(ink);
    }

    let text = flood_text();
    assert_eq!(text.len(), FLOOD_KEYS);
    let mut typed = run(text.as_bytes());
    let frame = typed.cell_frame(rows, cols);
    assert_eq!(frame.cursor_row, rows - 1, "typed to the last row");
    for place in 0..places {
        let (now, ink) = word_at(&mut typed, place);
        let (was, _) = word_at(&mut laid, place);
        assert_ne!(now, was, "place {place} holds a typed word");
        assert_eq!(now.len(), FLOOD_WORD_CELLS);
        inks.push(ink);
    }
    let held = inks.len();
    inks.sort_by_key(|ink| format!("{ink:?}"));
    inks.dedup();
    assert_eq!(inks.len(), held, "no two words share an ink");

    // One key further and the screen's top is typed again, unscrolled.
    let again = run(format!("{text}g").as_bytes());
    assert_eq!(again.row_text(0).unwrap().chars().next(), Some('g'));
    assert_eq!(again.row_text(1), typed.row_text(1));

    let mut plain = typed_terminal_at(&text.as_bytes()[..40], 72);
    let row = &plain.cell_frame(14, 72).cells[4];
    assert_eq!(row[2].fg, row[8].fg, "the typing fixture keeps one ink");
}

/// `FLOOD`'S FRAMES PRESS WHAT THE CARET'S TYPED RUN READS AS TYPED: every press in order, a frame's
/// hop along a row no longer than [`FLOOD_KEYS_A_FRAME`], and each crossing of a wrap the three keys
/// from a row's last two cells to the next row's second cell. A longer hop over a wrap reads as a
/// jump and the ribbon stops on its first row (seen on a first try of this scene).
#[test]
fn flood_frames_fold_the_caret_over_each_wrap_as_typing() {
    let frames = flood_frames(72, FLOOD_KEYS);
    assert_eq!(frames.iter().sum::<usize>(), FLOOD_KEYS);
    assert!(frames.iter().all(|&n| n > 0 && n <= FLOOD_KEYS_A_FRAME));
    let (mut row, mut col) = (0, 0);
    for keys in frames {
        if col == 70 {
            assert!(keys <= 3, "the wrap's fold, {keys} keys");
            row += 1;
            col = 1;
        } else {
            col += keys;
            assert!(col <= 70 || row == 13, "{row},{col}");
        }
    }
    assert_eq!(row, 14, "every row's fold, and the last row's tail");
}

/// The lights a frame's under stream holds through a real cursor step, publication and word yield,
/// frame by frame, with the frames' keys `presses` on `flood`'s screen at the gallery's grid and pace
/// and its GitHub Light; `railed`: with the yield's budget lifted, as if a frame's lights had no bound.
#[cfg(target_os = "macos")]
fn flood_lights(presses: &[usize], rest_ms: u64, railed: bool) -> Vec<usize> {
    use aterm_core::terminal::{ClockReading, Rgb};
    use aterm_effects::cursor_fx::CursorFxInput;
    use aterm_effects::cursor_glow::glass_yield::with_no_light_budget;
    use aterm_effects::cursor_glow::{Geom, GlowConfig, GlowStyle, Light, Live, ProbeTrust};
    use aterm_effects::cursor_trail::TrailConfig;
    use aterm_effects::host::{PageFacts, motion_print_anchor};
    use aterm_effects::pipeline::{CursorStep, EffectsPipeline, RowProbe};
    use aterm_render::{Renderer, Theme as RenderTheme, linear_luminance};

    let (fg, bg) = (0x001f_2328_u32, 0x00ff_ffff_u32);
    let (columns, lines, px) = Scenario::Flood.grid();
    let (rows, cols) = (lines as usize, columns as usize);
    let mut renderer = Renderer::from_system(px as f32, RenderTheme::default()).expect("a face");
    renderer.debug_block_on_lazy_fallbacks();
    let (cw, ch) = renderer.cell_size();
    let admitted = PageFacts {
        thin_ink: renderer
            .glyph_ink()
            .thin_share(linear_luminance(fg), linear_luminance(bg)),
        ..PageFacts::OPAQUE
    }
    .glass();
    assert!(admitted, "premise: the page takes the glass");
    let mut term = aterm_core::terminal::Terminal::new(lines as u16, columns as u16);
    term.set_default_foreground(Rgb {
        r: 0x1f,
        g: 0x23,
        b: 0x28,
    });
    term.set_default_background(Rgb {
        r: 0xff,
        g: 0xff,
        b: 0xff,
    });
    let mut screen = FloodScreen::new(lines as u16, columns as u16);
    let mut bytes = Vec::new();
    screen.lay(&mut bytes).unwrap();
    term.process(&bytes);
    let mut fx = EffectsPipeline::for_window();
    let cfg = GlowConfig {
        style: GlowStyle::RainbowKitty,
        dark_theme: false,
        theme_fg: fg,
        theme_bg: bg,
        color: fg,
        duration: Duration::from_millis(650),
        length: usize::MAX,
        intensity: 1.0,
        light: Light::at(Live::Glass).on_page(admitted),
        ..Default::default()
    };
    let (w, h) = renderer.frame_size(rows, cols);
    let geom = Geom {
        cw,
        ch,
        rows,
        cols,
        origin_x: renderer.pad() as u16,
        origin_y: renderer.grid_top() as u16,
        win_w: w as u16,
        win_h: h as u16,
        head: 0,
    };
    let text = flood_text();
    let t0 = ClockReading::now().monotonic;
    let mut typed = 0;
    let mut lights = Vec::new();
    let mut row_cols = Vec::new();
    let rest = (rest_ms / BURST_KEY_MS) as usize;
    for frame in 0..presses.len() + rest {
        let pressed = t0 + Duration::from_millis(frame as u64 * BURST_KEY_MS);
        for at in typed..typed + presses.get(frame).copied().unwrap_or(0) {
            fx.cursor_mut().glow.note_synthetic_typed(pressed, 1);
            let mut bytes = Vec::new();
            screen.key(&mut bytes, text.as_bytes()[at]).unwrap();
            term.process_at(
                &bytes,
                ClockReading {
                    monotonic: pressed,
                    wall_ms: None,
                },
            );
        }
        typed += presses.get(frame).copied().unwrap_or(0);
        let now = pressed + Duration::from_millis(BURST_KEY_MS / 2);
        let base = term.cell_frame(rows, cols);
        fx.cursor_mut()
            .glow
            .observe_glyph_ink(Some(renderer.glyph_ink_for(&base)));
        let caret = (base.cursor_row as u16, base.cursor_col as u16);
        term.row_cols_into(base.cursor_row, &mut row_cols);
        fx.cursor_mut().glow.observe_ribbon_row(caret.0, &row_cols);
        let (print_anchor, print_anchor_glyph) = motion_print_anchor(&term);
        let _ = fx.cursor_step(
            &CursorStep::new(
                CursorFxInput {
                    now,
                    cur: Some(caret),
                    live_viewport: true,
                    cursor_style: term.cursor_style(),
                    blink_phase: true,
                    live_cursor: fg,
                    cursor_color_pinned: false,
                    default_bg: bg,
                    cursor_cell_bg: None,
                    geom,
                    focused: true,
                    shed_envelope: 1.0,
                    trail_brightness: 1.0,
                    body_allowed: true,
                    momentum_glow: true,
                    user_tinted: false,
                    accent_pinned: false,
                },
                &cfg,
                TrailConfig {
                    enabled: false,
                    duration: cfg.duration,
                    max_len: cfg.length,
                    color: cfg.color,
                    intensity: cfg.intensity,
                    warmth: 0.0,
                },
            )
            .with_row_probe(Some(RowProbe {
                row: caret.0,
                caret: caret.1,
                trust: ProbeTrust::Full,
                cols: &row_cols,
                flanks: None,
            }))
            .with_frame_cells(Some(base.cells.as_slice()))
            .with_frame_cells_cols(base.cols)
            .with_frame_marks(&base.combining)
            .with_print_anchor(print_anchor)
            .with_print_anchor_glyph(print_anchor_glyph),
            std::iter::empty(),
        );
        let mut input = base.clone();
        fx.publish_cursor(&mut input);
        if railed {
            with_no_light_budget(|| fx.yield_glass_unworded(&mut input));
        } else {
            fx.yield_glass_unworded(&mut input);
        }
        lights.push(input.glow_under_ramps.laid(input.glow_under.len()));
    }
    lights
}

/// `FLOOD` REACHES THE WORD YIELD'S FALLBACK, on the gallery's own grid and face: with the budget
/// lifted its foot rails lay more lights than `CursorGlow::MAX_QUADS`; as it ships the frame that
/// would is the first to differ, it keeps to the bound, and once the flood and the whole words' hold
/// end a frame's lights match the railed lay again. The hold runs `WHOLE_WORDS_HOLD` from a
/// refused try and no try is made while it holds, so it can end up to a hold past the last frame
/// over the bound: the screen rests that long and two frames more. NEGATIVE CONTROL: the same
/// screen typed by a typist, a key a frame, never comes near the bound.
#[cfg(target_os = "macos")]
#[test]
fn flood_runs_the_word_yield_past_max_quads_and_it_takes_whole_words() {
    use aterm_effects::cursor_glow::CursorGlow;
    use aterm_effects::cursor_glow::glass_yield::WHOLE_WORDS_HOLD;

    let (columns, _, _) = Scenario::Flood.grid();
    let presses = flood_frames(columns as usize, FLOOD_KEYS);
    let hold_ms = u64::try_from(WHOLE_WORDS_HOLD.as_millis()).expect("a hold of milliseconds");
    let rest_ms = hold_ms + 2 * BURST_KEY_MS;
    let railed = flood_lights(&presses, rest_ms, true);
    let shipped = flood_lights(&presses, rest_ms, false);
    let over: Vec<usize> = (0..railed.len())
        .filter(|&f| railed[f] > CursorGlow::MAX_QUADS)
        .collect();
    let (&first, &last) = (over.first().unwrap(), over.last().unwrap());
    assert!(
        over.len() >= 5,
        "the flood runs past the budget, at {over:?}, lights up to {}",
        railed.iter().max().unwrap()
    );
    assert_eq!(railed[..first], shipped[..first], "railed until it passes");
    assert!(
        shipped.iter().all(|&l| l <= CursorGlow::MAX_QUADS),
        "as it ships a frame keeps to the bound: {shipped:?}"
    );
    assert!(shipped[first] < railed[first], "the first frame past it");
    assert!(last + 1 < railed.len(), "the ribbon outlasts the flood");
    assert!(
        (railed.len() - 1 - last) as u64 * BURST_KEY_MS >= hold_ms,
        "the screen rests a hold past the last frame over the bound, {last} of {}",
        railed.len()
    );
    assert_ne!(railed.last(), Some(&0), "the ribbon outlasts the flood");
    assert_eq!(railed.last(), shipped.last(), "past the hold, the rail");
    let typist = flood_lights(&vec![1; 60], 0, true);
    assert!(
        typist.iter().all(|&l| l < CursorGlow::MAX_QUADS / 2),
        "{typist:?}"
    );
}

/// `RAIL-G` LEAVES THE CARET JUST PAST THE `G` OF `LAZY DOG`, the burst's own line and pace, on a row
/// under a row with a letter in every cell and blank rows below it, then, past a Ctrl+L that clears
/// the line, on the grid's last row. NEGATIVE CONTROL: `last-row`'s line runs on past that `g` at
/// each of the frames it keeps, and its `mid` stops short of it, so it shows none of this.
#[test]
fn rail_g_holds_the_caret_past_the_g_under_letters_and_on_the_last_row() {
    let (columns, lines, font_px) = Scenario::RailG.grid();
    assert_eq!(font_px, Scenario::LastRow.grid().2, "the gallery's 22 px");
    let (rows, cols) = (lines as usize, columns as usize);
    let (last, under) = (rows - 1, usize::from(RAIL_G_UNDER_ROW));
    assert!(under + 1 < last, "the first line has blank rows below it");
    assert!(BURST_TEXT.starts_with(RAIL_G_TEXT));
    assert!(RAIL_G_TEXT.ends_with("lazy dog"));
    let run = |keys: &[u8]| {
        let mut output = Vec::new();
        line_fixture(
            &mut std::io::Cursor::new(keys),
            &mut output,
            LineMode::RailG,
            || Some((lines as u16, columns as u16)),
        )
        .unwrap();
        let mut term = aterm_core::terminal::Terminal::new(lines as u16, columns as u16);
        term.process(&output);
        term
    };
    let full: String = LAST_ROW_FULL.chars().cycle().take(cols).collect();
    let row = |term: &aterm_core::terminal::Terminal, row: usize| {
        term.row_text(row).unwrap().trim_end().to_string()
    };
    let past_g = |term: &mut aterm_core::terminal::Terminal, at: usize| {
        let frame = term.cell_frame(rows, cols);
        assert_eq!(
            (frame.cursor_row, frame.cursor_col),
            (at, 2 + RAIL_G_TEXT.len())
        );
        assert_eq!(frame.cells[at][1 + RAIL_G_TEXT.len()].ch, 'g');
    };

    let mut typed = run(RAIL_G_TEXT.as_bytes());
    past_g(&mut typed, under);
    assert_eq!(row(&typed, under), format!("$ {RAIL_G_TEXT}"));
    assert_eq!(row(&typed, under - 1), full);
    assert!(row(&typed, under + 1).is_empty(), "nothing under it");

    let mut keys = RAIL_G_TEXT.as_bytes().to_vec();
    keys.push(0x0c);
    keys.extend_from_slice(RAIL_G_TEXT.as_bytes());
    let mut again = run(&keys);
    past_g(&mut again, last);
    assert_eq!(row(&again, last), format!("$ {RAIL_G_TEXT}"));
    assert_eq!(row(&again, last - 1), full);

    // On `last-row` the caret is past that `g` only on its way, at no frame it keeps.
    let at_key = |key: u64| 2 + key as usize + 1;
    let g_cell = 2 + RAIL_G_TEXT.len();
    assert_ne!(at_key(12), g_cell);
    assert_ne!(at_key(BURST_FLY_KEY as u64), g_cell);
    assert_ne!(2 + BURST_TEXT.len(), g_cell);
}

/// `LAST-ROW` TYPES ON THE GRID'S LAST ROW, UNDER LETTERS IT LAID ITSELF: the burst's line whole
/// on that row under a row with a letter in every cell, ragged rows over that, every row above
/// lettered; a Return leaves the prompt on that row under the answer, a row that reaches past the
/// caret at `rmid` and ends short of it at the line's end. NEGATIVE CONTROL: the typing fixture's
/// prompt has only blank rows over it, and `enter-bottom`'s page paints no letters.
#[test]
fn last_row_types_on_the_grids_last_row_under_full_and_ragged_rows() {
    let (columns, lines, _) = Scenario::LastRow.grid();
    let grid = (
        u16::try_from(lines).unwrap(),
        u16::try_from(columns).unwrap(),
    );
    let (rows, cols) = (usize::from(grid.0), usize::from(grid.1));
    let last = rows - 1;
    let over_the_prompt: String = LAST_ROW_FULL.chars().cycle().take(cols).collect();
    let row = |term: &aterm_core::terminal::Terminal, row: usize| {
        term.row_text(row).unwrap().trim_end().to_string()
    };

    let mut typed = last_row_terminal(BURST_TEXT.as_bytes(), grid);
    let frame = typed.cell_frame(rows, cols);
    assert_eq!(
        (frame.cursor_row, frame.cursor_col),
        (last, 2 + BURST_TEXT.len())
    );
    assert_eq!(row(&typed, last), format!("$ {BURST_TEXT}"));
    assert_eq!(row(&typed, last - 1), over_the_prompt);
    assert!(!over_the_prompt.contains(' '), "a letter in every cell");
    let ragged = row(&typed, last - 2);
    assert!(
        LAST_ROW_RAGGED.contains(&ragged.as_str()) && ragged.len() < cols,
        "{ragged}"
    );
    for at in 0..last {
        assert!(!row(&typed, at).is_empty(), "row {at} has letters");
    }

    let mut keys = BURST_TEXT.as_bytes().to_vec();
    keys.push(b'\r');
    keys.extend_from_slice(TYPING_TEXT.as_bytes());
    let mut answered = last_row_terminal(&keys, grid);
    let frame = answered.cell_frame(rows, cols);
    let caret_at_the_end = 2 + TYPING_TEXT.len();
    assert_eq!(
        (frame.cursor_row, frame.cursor_col),
        (last, caret_at_the_end)
    );
    assert_eq!(row(&answered, last), format!("$ {TYPING_TEXT}"));
    assert_eq!(row(&answered, last - 1), LAST_ROW_ANSWER);
    assert_eq!(row(&answered, last - 2), format!("$ {BURST_TEXT}"));
    assert_eq!(row(&answered, last - 3), over_the_prompt);
    let caret_at_rmid = 2 + 13;
    assert!(
        caret_at_rmid < LAST_ROW_ANSWER.len() && LAST_ROW_ANSWER.len() < caret_at_the_end,
        "the line runs under the answer and past its end"
    );

    // Any grid: the page keeps to its width, and Ctrl+L lays it again.
    let mut narrow = last_row_terminal(b"echo\x0c", (6, 40));
    assert_eq!(narrow.cell_frame(6, 40).cursor_row, 5);
    assert_eq!(row(&narrow, 5), "$ echo");
    assert_eq!(row(&narrow, 4).len(), 40);
    assert!(!row(&narrow, 3).is_empty() && row(&narrow, 3).len() < 40);

    let blank_over = typed_terminal(TYPING_TEXT.as_bytes());
    assert_eq!(row(&blank_over, 4), format!("$ {TYPING_TEXT}"));
    for at in 0..4 {
        assert!(
            row(&blank_over, at).is_empty(),
            "control: row {at} is blank"
        );
    }
    assert_eq!(
        paint(Scenario::EnterBottom, Theme::Dark, None),
        format!("{PAINT_HEAD}seq 1 40\nexec /bin/sh -i\n"),
        "control: enter-bottom's rows are a command's short numbers"
    );
}

fn rgb_png(width: u32, height: u32, rgb: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut encoder = aterm_png::Encoder::new(&mut bytes, width, height);
    encoder.set_color(aterm_png::ColorType::Rgb);
    encoder.set_depth(aterm_png::BitDepth::Eight);
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(rgb).unwrap();
    writer.finish().unwrap();
    bytes
}

/// A settings frame's clean twin is the frame with the bare card `settings card clean` answers laid at its place;
/// every other pixel is the frame's. A card that does not lie inside the frame, or whose bytes are not the ones
/// it declares, makes no twin.
#[test]
fn a_cards_clean_twin_lays_the_bare_card_at_its_place() {
    let lit: Vec<u8> = (0..4 * 3).flat_map(|i| [i as u8, 100, 200]).collect();
    let lit_png = rgb_png(4, 3, &lit);
    let card_png = rgb_png(2, 1, &[7, 7, 7, 9, 9, 9]);
    let encoded = aterm_codec::base64::encode(&card_png).unwrap();
    let reply = format!("OK 1\nclean 1 1 2 1 {} {encoded}\n", card_png.len());
    let twin = card_clean_twin(&lit_png, &reply).expect("a twin");
    let (px, w, h) = aterm_png::decode_rgba8(&twin, 64).expect("the twin decodes");
    assert_eq!((w, h), (4, 3));
    for y in 0..3 {
        for x in 0..4 {
            let got = &px[(y * 4 + x) * 4..][..3];
            let want: [u8; 3] = match (x, y) {
                (1, 1) => [7, 7, 7],
                (2, 1) => [9, 9, 9],
                _ => [(y * 4 + x) as u8, 100, 200],
            };
            assert_eq!(got, want, "pixel {x},{y}");
        }
    }
    let outside = format!("clean 3 1 2 1 {} {encoded}\n", card_png.len());
    assert!(
        card_clean_twin(&lit_png, &outside).is_err(),
        "a card past the frame's edge"
    );
    let short = format!("clean 1 1 2 1 {} {encoded}\n", card_png.len() - 1);
    assert!(
        card_clean_twin(&lit_png, &short).is_err(),
        "a byte count that is not the card's"
    );
}

/// A `settings card` reply's lines become the terminal frame's sidecars, a client run's `OK <n>` head passed over;
/// a reply without its dims line keeps none.
#[test]
fn a_card_reply_names_the_frames_sidecars() {
    let files = card_sidecars(
        "OK 5\ndims OK 2 4 36 32 cell_w=9 cell_h=16 pad=10 pad_top=20 head=0\ncursor OK 1 2 1 steady_block\n\
         status phase=52 pet_body=none word_icons=none\ntext ab\ntext\n",
    )
    .expect("the sidecars");
    let get = |suffix: &str| {
        files
            .iter()
            .find(|(s, _)| *s == suffix)
            .map(|(_, body)| body.as_str())
    };
    assert_eq!(
        get("dims.txt"),
        Some("OK 2 4 36 32 cell_w=9 cell_h=16 pad=10 pad_top=20 head=0\n")
    );
    assert_eq!(get("cursor.txt"), Some("OK 1 2 1 steady_block\n"));
    assert_eq!(
        get("status.txt"),
        Some("phase=52 pet_body=none word_icons=none\n")
    );
    assert_eq!(get("text.txt"), Some("ab\n\n"));
    let grid = crate::trail_contrast::grid_from_dims(get("dims.txt").unwrap()).expect("a grid");
    assert_eq!((grid.ox, grid.oy, grid.cw, grid.ch), (10, 20, 9, 16));
    assert!(card_sidecars("OK 1\ncursor OK 1 2 1 steady_block\n").is_err());
}

/// What a line fixture in `mode` leaves on a 14 by `cols` terminal after `keys`, a key
/// waiting within every wait when `key_waits`.
fn resident_terminal(
    mode: LineMode,
    keys: &[u8],
    cols: u16,
    key_waits: bool,
) -> aterm_core::terminal::Terminal {
    let mut output = Vec::new();
    line_fixture_waiting(
        &mut std::io::Cursor::new(keys),
        &mut output,
        mode,
        || Some((14, cols)),
        &mut |_| Ok(key_waits),
    )
    .unwrap();
    let mut term = aterm_core::terminal::Terminal::new(14, cols);
    term.process(&output);
    term
}

#[test]
fn bottom_prompt_types_on_the_last_row_of_a_shell_s_full_page() {
    let mut term = resident_terminal(LineMode::BottomPrompt, RESIDENT_TEXT.as_bytes(), 72, false);
    for (row, text) in crate::trail_scenes::SHELL_ROWS.iter().enumerate() {
        assert_eq!(term.row_text(row).as_deref(), Some(*text), "row {row}");
    }
    assert_eq!(term.row_text(13), Some(format!("$ {RESIDENT_TEXT}")));
    let frame = term.cell_frame(14, 72);
    assert_eq!(
        (frame.cursor_row, frame.cursor_col),
        (13, 2 + RESIDENT_TEXT.len())
    );
    // The row over the prompt is a command's output and reaches past the typed line's end.
    assert!(crate::trail_scenes::SHELL_ROWS[12].len() > 2 + RESIDENT_TEXT.len());
    // A narrow grid cuts the page at its last column and keeps the prompt on the last row.
    let narrow = resident_terminal(LineMode::BottomPrompt, b"ab", 40, false);
    assert_eq!(
        narrow.row_text(12).as_deref(),
        Some(&crate::trail_scenes::SHELL_ROWS[12][..40])
    );
    assert_eq!(narrow.row_text(13).as_deref(), Some("$ ab"));
}

#[test]
fn a_shell_line_s_return_runs_one_of_the_scenes_commands_between_marks() {
    assert_eq!(ENTER_LONG_TEXT.len(), 40);
    let mut keys = ENTER_LONG_TEXT.as_bytes().to_vec();
    keys.push(b'\r');
    let term = resident_terminal(LineMode::BottomPrompt, &keys, 72, false);
    assert_eq!(term.row_text(11), Some(format!("$ {ENTER_LONG_TEXT}")));
    assert_eq!(
        term.row_text(12).as_deref(),
        Some(&ENTER_LONG_TEXT["echo ".len()..])
    );
    assert_eq!(term.row_text(13).as_deref(), Some("$ "));
    let exit = |keys: &[u8], key_waits| {
        resident_terminal(LineMode::BottomPrompt, keys, 72, key_waits)
            .last_completed_command()
            .and_then(|command| command.exit_code)
    };
    assert_eq!(exit(&keys, false), Some(0));
    for (_, command, _, code) in VERDICT_COMMANDS {
        let keys = format!("{command}\r");
        assert_eq!(
            exit(keys.as_bytes(), false),
            Some(i32::from(code)),
            "{command}"
        );
    }
    assert_eq!(
        exit(b"rm -rf /\r", false),
        Some(127),
        "no line reaches a shell"
    );
    // A Ctrl-C that arrives while the sleep runs ends it with 130 and the fixture goes on.
    let interrupted = format!("{VERDICT_INTERRUPTED}\r\x03ok");
    let term = resident_terminal(LineMode::BottomPrompt, interrupted.as_bytes(), 72, true);
    assert_eq!(
        term.last_completed_command().and_then(|c| c.exit_code),
        Some(130)
    );
    assert_eq!(term.row_text(12).as_deref(), Some("^C"));
    assert_eq!(term.row_text(13).as_deref(), Some("$ ok"));
    // The build writes its lines and ends green.
    let build = format!("{BUILD_COMMAND}\r");
    let term = resident_terminal(LineMode::BottomPrompt, build.as_bytes(), 72, false);
    assert_eq!(
        term.last_completed_command().and_then(|c| c.exit_code),
        Some(0)
    );
    assert!(
        term.row_text(11)
            .is_some_and(|row| row.contains("Compiling aterm-crate-39")),
        "{:?}",
        term.row_text(11)
    );
    assert!(
        term.row_text(12)
            .is_some_and(|row| row.contains("Finished"))
    );
}

#[test]
fn the_composer_is_claude_code_s_box_and_repaints_its_row_at_each_key() {
    use crate::trail_scenes::{COMPOSER_FOOT, COMPOSER_TRANSCRIPT};
    let mut term = resident_terminal(LineMode::Composer, RESIDENT_TEXT.as_bytes(), 72, false);
    let rule = "\u{2500}".repeat(72);
    // Fourteen rows keep the transcript's last eight, then a blank row, as Claude Code leaves
    // one between its last message and the box.
    for (row, text) in COMPOSER_TRANSCRIPT[2..].iter().enumerate() {
        assert_eq!(term.row_text(row).as_deref(), Some(*text), "row {row}");
    }
    assert_eq!(term.row_text(8).as_deref(), Some(""));
    assert_eq!(term.row_text(9).as_deref(), Some(rule.as_str()));
    assert_eq!(
        term.row_text(10),
        Some(format!("{COMPOSER_PROMPT}{RESIDENT_TEXT}"))
    );
    assert_eq!(term.row_text(11).as_deref(), Some(rule.as_str()));
    assert_eq!(term.row_text(12).as_deref(), Some(COMPOSER_FOOT[0]));
    assert_eq!(term.row_text(13).as_deref(), Some(COMPOSER_FOOT[1]));
    let frame = term.cell_frame(14, 72);
    assert_eq!(
        (frame.cursor_row, frame.cursor_col),
        (10, 2 + RESIDENT_TEXT.len())
    );
    // The rules and the rows under the box are grey, the typed line the page's own ink:
    // drawn in the default ink, the rules were the brightest thing on a dark page.
    let ink = |row: usize, col: usize| frame.cells[row][col].fg;
    assert_eq!(ink(9, 0), [0x88; 3]);
    assert_eq!(ink(11, 71), [0x88; 3]);
    assert_eq!(ink(12, 2), [0x99; 3]);
    assert_eq!(ink(13, 5), [0x99; 3]);
    assert_ne!(ink(10, 4), [0x88; 3]);
    assert_ne!(ink(10, 4), [0x99; 3]);
    // A Backspace and a Return repaint the row alone: the rules and the rows under them stay.
    let term = resident_terminal(LineMode::Composer, b"abc\x7f\rxy", 72, false);
    assert_eq!(term.row_text(10), Some(format!("{COMPOSER_PROMPT}xy")));
    assert_eq!(term.row_text(9).as_deref(), Some(rule.as_str()));
    assert_eq!(term.row_text(11).as_deref(), Some(rule.as_str()));
    assert_eq!(term.row_text(12).as_deref(), Some(COMPOSER_FOOT[0]));
    assert_eq!(term.row_text(13).as_deref(), Some(COMPOSER_FOOT[1]));
}

#[test]
fn home_and_end_move_the_caret_along_the_typed_line() {
    let at = |keys: &[u8]| {
        let mut term = typed_terminal(keys);
        let row = term.row_text(4).unwrap_or_default();
        let frame = term.cell_frame(14, 72);
        (frame.cursor_row, frame.cursor_col, row)
    };
    let line = format!("$ {RESIDENT_TEXT}");
    let end = 2 + RESIDENT_TEXT.len();
    let home = [RESIDENT_TEXT.as_bytes(), b"\x1b[H"].concat();
    assert_eq!(at(&home), (4, 2, line.clone()));
    assert_eq!(at(&[&home[..], b"\x1b[F"].concat()), (4, end, line.clone()));
    // The application-keypad spellings, and a key typed from Home lands at the line's end.
    let ss3 = [RESIDENT_TEXT.as_bytes(), b"\x1bOH\x1bOF"].concat();
    assert_eq!(at(&ss3), (4, end, line.clone()));
    assert_eq!(
        at(&[&home[..], b"!"].concat()),
        (4, end + 1, format!("{line}!"))
    );
}

#[test]
fn the_coloured_prompt_inks_each_word_and_shows_the_suggestion_s_rest_in_grey() {
    let typed = &COLOURED_PROMPT_TEXT[.."git commit -m 'qu".len()];
    let mut term = resident_terminal(LineMode::ColouredPrompt, typed.as_bytes(), 72, false);
    assert_eq!(term.row_text(13), Some(format!("$ {COLOURED_PROMPT_TEXT}")));
    let frame = term.cell_frame(14, 72);
    assert_eq!((frame.cursor_row, frame.cursor_col), (13, 2 + typed.len()));
    let row = &frame.cells[13];
    let ink = |col: usize| row[col].fg;
    let (command, option, quote) = (
        ink(2),
        ink(2 + "git commit ".len()),
        ink(2 + typed.len() - 1),
    );
    let ghost = ink(2 + typed.len());
    assert_ne!(command, option);
    assert_ne!(option, quote);
    assert_eq!(
        ink(2 + "git ".len()),
        option,
        "a parameter and an option share an ink"
    );
    assert_ne!(ghost, quote);
    assert_eq!(ghost, ink(2 + COLOURED_PROMPT_TEXT.len() - 1));
    // A line that leaves the suggestion shows none.
    let off = resident_terminal(LineMode::ColouredPrompt, b"ls", 72, false);
    assert_eq!(off.row_text(13).as_deref(), Some("$ ls"));
}

#[test]
fn a_caret_is_named_set_by_the_program_and_added_to_the_instance_s_name() {
    let none = parse_args(&args(&["/x/g"])).unwrap();
    assert!(none.carets.is_empty());
    let both = parse_args(&args(&[
        "--caret",
        "both",
        "/x/g",
        "--caret",
        "underline,bar",
    ]))
    .unwrap();
    assert_eq!(
        both.carets,
        vec![Caret::Block, Caret::Bar, Caret::Underline]
    );
    assert_eq!(
        parse_args(&args(&["--caret", "all", "/x/g"]))
            .unwrap()
            .carets,
        Caret::ALL.to_vec()
    );
    assert_eq!(
        parse_args(&args(&["--caret", "beam", "/x/g"]))
            .unwrap_err()
            .code,
        2
    );
    assert!(
        paint(Scenario::Context, Theme::Dark, Some(Caret::Bar)).contains("printf '\\033[6 q'\n")
    );
    assert!(!paint(Scenario::Context, Theme::Dark, None).contains(" q'"));
    let root = crate::test_scratch_dir("caret");
    let lab = root.path().join("lab");
    for (scenario, mode) in [
        (Scenario::BottomPrompt, "bottom-prompt"),
        (Scenario::EnterLong, "bottom-prompt"),
        (Scenario::Wall, "bottom-prompt"),
        (Scenario::Verdict, "bottom-prompt"),
        (Scenario::LongBuild, "bottom-prompt"),
        (Scenario::Composer, "composer"),
        (Scenario::ColouredPrompt, "coloured-prompt"),
        (Scenario::Jump, "typing"),
    ] {
        let plain = gallery_program(root.path(), scenario, Theme::Dark, None, &lab).unwrap();
        assert_eq!(plain[2..], [mode.to_string()], "{scenario:?}");
        let bar =
            gallery_program(root.path(), scenario, Theme::Dark, Some(Caret::Bar), &lab).unwrap();
        assert_eq!(bar[2..], [mode.to_string(), "caret=bar".to_string()]);
    }
    assert_eq!(Scenario::Wall.grid(), (WALL_COLUMNS, 14, 22));
    assert_eq!("$ ".len() + BURST_TEXT.len() + 2, WALL_COLUMNS as usize);
    let wall = parse_args(&args(&["--scenario", "wall", "--columns", "70", "/x/g"])).unwrap();
    assert_eq!(Scenario::Wall.configured_grid(&wall), (70, 14, 22));
}
