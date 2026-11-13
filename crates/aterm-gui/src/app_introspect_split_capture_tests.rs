// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::{App, WindowId, term_lock};
use std::time::{Duration, Instant};

fn configure_pane_backgrounds(app: &App, sid: u64) {
    let bg = aterm_render::Theme::default().bg;
    for session in [0, sid] {
        let term = &app.pool.get(session).expect("pane session").term;
        term_lock(term).set_default_background(aterm_core::terminal::Rgb {
            r: (bg >> 16) as u8,
            g: (bg >> 8) as u8,
            b: bg as u8,
        });
    }
}

#[test]
fn first_headless_capture_applies_the_configured_pet_species() {
    use aterm_effects::kitty_pet::PetSpecies;

    let capture = |pet: &str, split: bool| {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        if split {
            let focus = app.split_active_stub_tab(wid);
            configure_pane_backgrounds(&app, focus);
        }
        app.config.cursor_trail = Some(true);
        app.config.cursor_trail_style = Some("rainbow".into());
        app.config.cursor_pet = Some(pet.into());
        app.config.motion = Some("full".into());
        app.recompute_sparkle();
        app.sparkle = None;
        if split {
            app.windows.get_mut(&wid).expect("window").focused = true;
        }
        app.capture_words(wid, Instant::now());
        let ws = app.windows.get(&wid).expect("window");
        let sprites = aterm_render::sprites::published(&ws.input_scratch.fx_layers);
        assert!(
            !sprites.is_empty(),
            "the first capture emits the configured pet body"
        );
        (
            ws.fx.companion().brain().species(),
            sprites
                .iter()
                .map(|s| s.art.data().to_vec())
                .collect::<Vec<_>>(),
        )
    };

    for split in [false, true] {
        let cat = capture("cat", split);
        let dog = capture("dog", split);
        assert_eq!(
            cat.0,
            PetSpecies::Cat,
            "split={split}: negative control: cat stays cat"
        );
        assert_eq!(dog.0, PetSpecies::Dog, "split={split}");
        assert!(
            dog.1 != cat.1,
            "split={split}: the first captured arts must be the configured dog skin"
        );
    }
}

#[test]
fn a_split_paints_a_streak_anchored_in_the_pane_that_produced_it() {
    let run = |enabled: bool| {
        let t0 = Instant::now();
        let mut app = App::headless_for_test();
        app.config.output_streak = Some(crate::app_config::OutputStreakConfig {
            enabled: Some(enabled),
            ..Default::default()
        });
        let wid = WindowId(0);
        let sid = app.split_active_stub_tab(wid);
        app.recompute_sparkle();
        {
            let ws = app.windows.get_mut(&wid).expect("window");
            ws.focused = true;
        }
        app.capture_words(wid, t0);
        let term = app.pool.get(sid).expect("pane session").term.clone();
        term_lock(&term).process(b"\r\noutput in the focused pane\r\n");
        app.capture_words(wid, t0 + Duration::from_millis(120));
        let ws = app.windows.get(&wid).expect("window");
        let xs: Vec<u16> = ws.fx.nova().iter().map(|q| q.x).collect();
        let half = u32::from(ws.cols) * app.backend.cell_size().0 as u32 / 2;
        (xs, half)
    };
    let (xs, half) = run(true);
    assert!(
        !xs.is_empty(),
        "a split must not go dark: the composed pass owes streak light"
    );
    let min_x = xs.iter().copied().min().expect("min");
    assert!(
        u32::from(min_x) >= half,
        "streak light must be anchored inside the pane that earned it: \
         x {min_x} is left of the divider at {half}"
    );
    let (off, _) = run(false);
    assert!(
        off.is_empty(),
        "NON-VACUITY: with the streak off the same split draws none of it"
    );
}

#[test]
fn a_capture_carries_the_output_streak_and_an_off_config_carries_none() {
    let streak_quads = |enabled: bool| {
        let t0 = Instant::now();
        let mut app = App::headless_for_test();
        app.config.output_streak = Some(crate::app_config::OutputStreakConfig {
            enabled: Some(enabled),
            ..Default::default()
        });
        let wid = WindowId(0);
        {
            let ws = app.windows.get_mut(&wid).expect("window");
            ws.focused = true;
        }
        let term = app
            .front_terminal(wid)
            .expect("front terminal")
            .term
            .clone();
        app.capture_words(wid, t0);
        let before = app.windows[&wid].fx.nova().len();
        term_lock(&term).process(b"\r\nprogram output arrives here\r\n");
        app.capture_words(wid, t0 + Duration::from_millis(120));
        app.windows[&wid].fx.nova().len().saturating_sub(before)
    };
    let on = streak_quads(true);
    let off = streak_quads(false);
    assert!(
        on > 0,
        "an enabled streak must put light in the capture's nova channel"
    );
    assert_eq!(
        off, 0,
        "NON-VACUITY: with the streak off the same script draws none of it"
    );
}

#[test]
fn split_capture_splices_decorations() {
    let t0 = Instant::now();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let sid = app.split_active_stub_tab(wid);
    app.recompute_sparkle();
    {
        let ws = app.windows.get_mut(&wid).expect("window");
        ws.focused = true;
    }
    app.capture_words(wid, t0);
    for s in [0u64, sid] {
        let term = app.pool.get(s).expect("pane session").term.clone();
        term_lock(&term).process(b"\r\nhello kitty friend\r\n");
    }
    app.windows
        .get_mut(&wid)
        .expect("window")
        .pending_deco_birth = Some(t0);
    app.capture_words(wid, t0);
    app.capture_words(wid, t0 + Duration::from_millis(600));
    let ws = app.windows.get(&wid).expect("window");
    let sprites = aterm_render::sprites::published(&ws.input_scratch.fx_layers);
    assert!(
        !sprites.is_empty(),
        "a split capture carries the cats the glass draws (deco={} ink={})",
        ws.input_scratch.word_decorations.len(),
        ws.input_scratch.ink.len()
    );
    assert!(
        sprites.iter().all(|s| !s.art.is_empty()),
        "and the arts those sprites draw"
    );
}
