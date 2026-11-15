// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_fresh_plane_uses_its_fence_and_a_typed_plane_keeps_its_decay() {
    let used = std::cell::RefCell::new(Vec::new());
    for plane in [Plane::Fresh, Plane::AfterTyping] {
        settle_plane(
            plane,
            || {
                used.borrow_mut().push("fence");
                Ok(())
            },
            || used.borrow_mut().push("decay"),
        )
        .unwrap();
    }
    assert_eq!(*used.borrow(), ["fence", "decay"]);
    assert!(
        settle_plane(
            Plane::Fresh,
            || could_not_run("no evidence"),
            || panic!("a delay must not replace the fence")
        )
        .is_err()
    );
}

#[test]
fn plane_settle_needs_every_live_trail_component_dark() {
    let quiet = "trail ribbon_active=false sparks=0 glow_active=false pet_active=true licensed=100 spawns=100";
    assert_eq!(plane_dark(quiet), Ok(true));
    for (old, new) in [
        ("ribbon_active=false", "ribbon_active=true"),
        ("sparks=0", "sparks=1"),
        ("glow_active=false", "glow_active=true"),
    ] {
        assert_eq!(plane_dark(&quiet.replace(old, new)), Ok(false));
        assert!(plane_dark(&quiet.replace(old, "")).is_err());
    }
}

fn head_row(scene: Scene, recording: bool, hosted: bool) -> PaintRow {
    PaintRow {
        scene,
        recording,
        hosted,
        ..WARM_ROW
    }
}

/// Every take writes its recording either way, before any table its scene opens (a key after a
/// table header would be that table's), and a hosted take turns the glow on ahead of it too.
#[test]
fn a_takes_config_sets_its_recording_ahead_of_every_table() {
    for recording in [true, false] {
        let head = config_head(&head_row(Scene::ColdStreamer, recording, true));
        let table = head.find("[sparkle_words]").expect("the streamer's table");
        let line = format!("temporal_recording = {recording}");
        assert!(
            head.find(&line).expect("the recording line") < table,
            "{head}"
        );
        assert_eq!(head.matches("temporal_recording").count(), 1, "{head}");
        for glow in HOSTED_GLOW.lines() {
            assert!(head.find(glow).expect("the glow line") < table, "{head}");
        }
        assert!(
            head.starts_with(Style::Rainbow.config_line().expect("a spelled style")),
            "{head}"
        );
    }
    assert!(
        !config_head(&head_row(Scene::Prompt, true, false)).contains("cursor_momentum_glow"),
        "an unhosted take leaves the glow at its default"
    );
}
