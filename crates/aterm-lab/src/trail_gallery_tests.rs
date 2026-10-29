// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for `trail-gallery` argument parsing, per-instance configuration, and checkout output path guards.

use super::*;

use std::sync::atomic::{AtomicU32, Ordering};

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
fn an_open_capture_never_waits_for_its_own_frame_to_close() {
    let mut client = wire(
        "OK 1\ntrail open\nOK 1\ntrail returned\nOK 4 26 1 blinking_block\nOK 1\nreturned text\n",
    );
    let open = frame_evidence(FrameRead::Open, |request, shape| {
        relay_reply(&mut client, request, shape).unwrap()
    });
    assert!(open.cursor.is_none() && open.text.is_none());
    assert_eq!(client.transport().sent, b"trail status\n");
    let returned = frame_evidence(FrameRead::Returned, |request, shape| {
        relay_reply(&mut client, request, shape).unwrap()
    });
    assert_eq!(returned.status.stdout, "trail returned\n");
    assert_eq!(
        returned.cursor.unwrap().stdout,
        "OK 4 26 1 blinking_block\n"
    );
    assert_eq!(returned.text.unwrap().stdout, "returned text\n");
    assert_eq!(
        client.transport().sent,
        b"trail status\ntrail status\ncursor\ntext\n"
    );
}

#[test]
fn a_requested_rainbow_return_losing_eligibility_is_inconclusive() {
    let live = "trail resolved=rainbow-kitty config_enabled=true effective=true intensity=1.00 ribbon_segments=12 ribbon_drawn=30";
    let expected = expects_rainbow(&config_lines("rainbow kitty pet", Theme::Dark, &[])).unwrap();
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
        ("rainbow kitty pet", None, true),
        ("nyan", None, true),
        ("off", None, false),
        ("comet", None, false),
        (
            "rainbow kitty pet",
            Some("cursor_trail_style = 'comet'"),
            false,
        ),
        (
            "comet",
            Some("cursor_trail_style = 'rainbow kitty pet'"),
            true,
        ),
        (
            "rainbow kitty pet",
            Some("cursor_trail_style = 'off'"),
            false,
        ),
        ("rainbow kitty pet", Some("cursor_trail = false"), false),
        ("rainbow kitty pet", Some("motion = ' REDUCE '"), false),
        // `off` and `none` ask for no motion: the shared parser reads them as
        // Reduced (settings demo round 3), so no rainbow is expected.
        ("rainbow kitty pet", Some("motion = 'off'"), false),
        ("rainbow kitty pet", Some("motion = 'none'"), false),
        ("rainbow kitty pet", Some("motion = 'unknown'"), true),
        ("rainbow kitty pet", Some("serious_mode = true"), false),
        (
            "rainbow kitty pet",
            Some("cursor_trail_intensity = 0"),
            false,
        ),
        (
            "rainbow kitty pet",
            Some("cursor_trail_intensity = 0.0"),
            false,
        ),
        (
            "rainbow kitty pet",
            Some("cursor_trail_intensity = -1.0"),
            false,
        ),
        (
            "rainbow kitty pet",
            Some("cursor_trail_intensity = nan"),
            false,
        ),
        (
            "rainbow kitty pet",
            Some("cursor_trail_intensity = inf"),
            false,
        ),
        (
            "rainbow kitty pet",
            Some("cursor_trail_intensity = 0.1"),
            true,
        ),
        (
            "rainbow kitty pet",
            Some("cursor_trail_intensity = 2"),
            true,
        ),
    ] {
        let extra: Vec<String> = extra.into_iter().map(str::to_owned).collect();
        let config = config_lines(style, Theme::Dark, &extra);
        assert_eq!(expects_rainbow(&config).unwrap(), expected, "{config:?}");
    }
    for extra in [
        "cursor_trail_style = 'unknown'",
        "cursor_trail_style = 7",
        "cursor_trail = 'false'",
        "motion = false",
        "serious_mode = 'true'",
        "cursor_trail_intensity = '0'",
        "cursor_trail_intensity = [",
    ] {
        let config = config_lines("rainbow kitty pet", Theme::Dark, &[extra.into()]);
        assert!(expects_rainbow(&config).is_err(), "{config:?}");
    }
}

fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_string()).collect()
}

fn scratch(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "lab-trail-gallery-{tag}-{}-{n}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    std::fs::canonicalize(&dir).expect("its physical path")
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
        "rainbow kitty pet",
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
    assert_eq!(a.styles, vec!["phaser", "rainbow kitty pet"]);
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
            "motion = \"full\"",
            "trail_sounds = false",
        ]
    );
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
        instance_name("rainbow kitty pet", Theme::Dark, Scenario::Typing),
        "rainbow-kitty-pet-dark"
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
            let p = paint(s, t);
            assert!(
                p.starts_with("LC_ALL=en_US.UTF-8; export LC_ALL\n"),
                "{s:?}"
            );
            assert!(p.ends_with("\nexec /bin/sh -i\n"), "{s:?}");
        }
    }
    assert!(paint(Scenario::Context, Theme::Dark).contains("\\033[48;2;51;65;94m"));
    assert!(paint(Scenario::Context, Theme::Light).contains("\\033[48;2;182;227;255m"));
    assert!(!paint(Scenario::Context, Theme::Light).contains("@SEL@"));
}

#[test]
fn the_roster_is_the_pickers_options() {
    let prefs = "field key=cursor_trail_enabled label=\"Trail\" value=\"true\"\n\
                 field key=cursor_trail_style label=\"Cursor style\" value=\"beam\" effective=\"Beam\" kind=enum \
                 options=[off,rainbow kitty pet,phaser,classic] visible=true\n";
    assert_eq!(
        roster_of(prefs),
        Some(args(&["off", "rainbow kitty pet", "phaser", "classic"]))
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
    let _ = std::fs::remove_dir_all(&root);
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
    assert_eq!(checkouts(&root), vec![root.clone()]);
    let _ = std::fs::remove_dir_all(&root);
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
        instance_name("nyan rainbow", Theme::Dark, Scenario::Burst),
        "nyan-rainbow-dark-burst"
    );
    assert_eq!(
        paint(Scenario::Burst, Theme::Dark),
        paint(Scenario::Typing, Theme::Dark)
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
/// lettered rows its paint draws above the prompt. NEGATIVE CONTROL: every
/// other paint carries no such program.
#[test]
fn the_widened_scenarios_name_their_frames_and_paint_their_page() {
    assert_eq!(
        instance_name("classic", Theme::Light, Scenario::MoveUp),
        "classic-light-move-up"
    );
    assert_eq!(
        instance_name("rainbow kitty pet", Theme::Dark, Scenario::Split),
        "rainbow-kitty-pet-dark-split"
    );
    for label in SPLIT_BURST_FRAMES {
        assert!(
            !["mid", "t0", "t120", "t470"].contains(&label),
            "{label} is a typing frame's"
        );
    }
    let page = paint(Scenario::MoveUp, Theme::Light);
    let program = format!("printf '[vim] {MOVE_UP_MARK}\\033[{MOVE_UP_ROWS}A");
    assert!(page.contains(&program), "{page}");
    let prompt_at = page.find("printf '\\033[8H'").expect("the prompt's row");
    let lettered = page[..prompt_at].matches("\\r\\n'").count();
    assert!(
        lettered > usize::from(MOVE_UP_ROWS),
        "the move crosses lettered rows: {lettered}"
    );
    for s in Scenario::ALL.into_iter().filter(|&s| s != Scenario::MoveUp) {
        assert!(!paint(s, Theme::Light).contains(MOVE_UP_MARK), "{s:?}");
    }
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
