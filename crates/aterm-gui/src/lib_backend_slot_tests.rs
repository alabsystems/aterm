// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{App, Backend, BackendSlot, FONT_PX, Renderer, Theme, pad_for_scale, seed_cell_px};

#[test]
fn finalize_backend_joins_once_and_runs_deferred_setup() {
    let mut app = App::headless_for_test();
    let theme = app.theme;
    let handle = std::thread::spawn(move || -> (Backend, bool, Option<String>) {
        let mut r = Renderer::from_system(FONT_PX, theme).expect("system font for test build");
        r.seal_admitted_font_sources();
        (Backend::Cpu(r), false, None)
    });
    app.backend = BackendSlot::Pending(Some(handle));
    app.use_gpu = true; // wrong-on-purpose intent seed; the join must correct it
    app.font_family = Some("Seeded Family".into()); // admitted: the seed survives
    assert!(app.backend.is_pending());

    app.finalize_backend();

    assert!(!app.backend.is_pending(), "slot Ready after the join");
    assert!(!app.use_gpu, "the build's ACTUAL outcome replaces the seed");
    assert_eq!(
        app.font_family.as_deref(),
        Some("Seeded Family"),
        "an ADMITTED family keeps its seed as the live rebuild source"
    );
    assert_eq!(
        app.backend.pad(),
        pad_for_scale(1.0),
        "the deferred 1x pad seed was applied at the join"
    );
    app.finalize_backend();
    assert!(!app.backend.is_pending());
    assert_eq!(app.backend.pad(), pad_for_scale(1.0));
}

#[test]
fn finalize_backend_drops_a_font_family_the_worker_rejected() {
    if !crate::message_inbox::lane_zero_alone() {
        return;
    }
    let mut app = App::headless_for_test();
    let theme = app.theme;
    let handle = std::thread::spawn(move || -> (Backend, bool, Option<String>) {
        let mut r = Renderer::from_system(FONT_PX, theme).expect("system font for test build");
        r.seal_admitted_font_sources();
        (
            Backend::Cpu(r),
            false,
            Some("font_family \"No Such Family\" is not an admissible font".into()),
        )
    });
    app.backend = BackendSlot::Pending(Some(handle));
    app.font_family = Some("No Such Family".into());
    let _ = crate::message_inbox::take_queued();
    app.finalize_backend();
    assert_eq!(
        app.font_family, None,
        "a rejected family must be dropped at the join — the built-in candidate regime is \
         what the startup renderer actually uses"
    );
    assert!(
        crate::message_inbox::take_queued().iter().any(|m| {
            m.message.key.as_deref() == Some(crate::message_reporters::KEY_FONT_FAMILY)
        }),
        "the join says the refusal of the build it joined"
    );
}

#[test]
fn a_discarded_warms_font_refusal_never_reaches_the_inbox() {
    if !crate::message_inbox::lane_zero_alone() {
        return;
    }
    const FAMILY: &str = "Zz No Such Family";
    assert!(
        crate::app_config::Config::font_family_admission(Some(FAMILY)).is_err(),
        "the early warm must have a refusal to leak"
    );
    let _ = crate::message_inbox::take_queued();
    let dir = aterm_tempfile::tempdir().expect("scratch dir");
    let path = dir.path().join("aterm").join("aterm.toml");
    std::fs::create_dir_all(dir.path().join("aterm")).expect("config dir");
    crate::test_scratch::write_config(&path, format!("font_family = {FAMILY:?}\n"));
    crate::test_env::scoped("XDG_CONFIG_HOME", dir.path(), || {
        let read = crate::WarmConfigRead::take(Some(&path));
        let early = crate::warm_config(
            crate::WarmLaunch::Headless,
            crate::app_config::CarriedZoom::default(),
        );
        let _ = early.backend_handle.join().expect("the early worker");
        std::fs::write(&path, "\n").expect("the hold's edit");
        assert!(crate::warm_config_moved(&read, Some(&path)));
        let late = crate::warm_config(
            crate::WarmLaunch::Headless,
            crate::app_config::CarriedZoom::default(),
        );
        let _ = late.backend_handle.join().expect("the re-warm's worker");
    });
    let stale: Vec<_> = crate::message_inbox::take_queued()
        .into_iter()
        .filter(|m| m.message.key.as_deref() == Some(crate::message_reporters::KEY_FONT_FAMILY))
        .map(|m| m.message.detail)
        .collect();
    assert!(
        stale.is_empty(),
        "the discarded warm's refusal reached the inbox: {stale:?}"
    );
}

#[test]
#[should_panic(expected = "published an unsealed font generation")]
fn finalize_backend_rejects_unsealed_generation() {
    let mut app = App::headless_for_test();
    let theme = app.theme;
    let handle = std::thread::spawn(move || -> (Backend, bool, Option<String>) {
        let renderer = Renderer::from_system(FONT_PX, theme).expect("system font for test build");
        (Backend::Cpu(renderer), false, None)
    });
    app.backend = BackendSlot::Pending(Some(handle));
    app.finalize_backend();
}

#[test]
#[should_panic(expected = "before attach_os_window")]
fn pending_slot_access_is_fail_loud() {
    let slot = BackendSlot::Pending(Some(std::thread::spawn(
        move || -> (Backend, bool, Option<String>) {
            let mut r = Renderer::from_system(FONT_PX, Theme::default())
                .expect("system font for test build");
            r.seal_admitted_font_sources();
            (Backend::Cpu(r), false, None)
        },
    )));
    let _ = slot.cell_size();
}

#[test]
fn seed_cell_px_is_positive_and_tracks_font_px() {
    let (w13, h13) = seed_cell_px(13.0);
    assert!(w13 >= 1);
    assert!(h13 > w13, "cells are taller than wide");
    let (w26, h26) = seed_cell_px(26.0);
    assert!(w26 > w13 && h26 > h13, "seed grows with the font");
}
