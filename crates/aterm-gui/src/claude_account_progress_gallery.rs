// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Explicit synthetic account-recovery scenes rendered by the real App capture route.

use std::sync::{Arc, OnceLock};

use aterm_agent::harness::account_progress::{AccountProgress, ProgressPhase};

use super::probe;

/// Writes opt-in review images; it never opens a provider or reads account credentials.
#[test]
#[ignore = "writes App-rendered fixture PNGs: set ATERM_ACCOUNT_PROGRESS_GALLERY"]
fn account_progress_gallery() {
    let Some(dir) = std::env::var_os("ATERM_ACCOUNT_PROGRESS_GALLERY") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).expect("gallery output directory");
    let mut manifest = String::from(
        "# Synthetic owner-bound account facts and Claude-style screen; no live account state.\nscene\tcolumns\trows\tfile\n",
    );
    for (name, cols, phase, eligible, wall, detail) in [
        (
            "blocked",
            120,
            ProgressPhase::Waiting,
            0,
            Some("weekly"),
            "No usable sign-in route",
        ),
        (
            "blocked-narrow",
            60,
            ProgressPhase::Waiting,
            0,
            Some("weekly"),
            "No usable sign-in route",
        ),
        (
            "signing-in",
            120,
            ProgressPhase::SigningIn {
                label: "fixture-alternate".into(),
                route: Some("fixture route".into()),
            },
            0,
            Some("weekly"),
            "A sign-in attempt is active",
        ),
        (
            "transcript-wall",
            120,
            ProgressPhase::Watching,
            1,
            None,
            "No recovery action is in flight",
        ),
        (
            "watching",
            120,
            ProgressPhase::Watching,
            1,
            None,
            "No recovery action is in flight",
        ),
    ] {
        let progress = AccountProgress {
            observed_at: 1_793_040_000,
            account: Some("fixture-current".into()),
            known: Some(2),
            eligible: Some(eligible),
            phase,
            wall: wall.map(str::to_owned),
            details: vec![detail.into()],
            next_at: None,
        };
        let mut facts = probe::facts();
        facts.account = progress.account.clone();
        facts.account_progress = Some(progress);
        if name == "transcript-wall" {
            facts.usage.get_or_insert_with(Default::default).wall =
                Some(aterm_agent::harness::session_usage::Wall {
                    label: "7d",
                    resets: "tomorrow".into(),
                });
        }
        let live = probe::Live {
            status: if wall.is_some() {
                "You've hit your limit · synthetic fixture"
            } else {
                probe::DONE
            },
            ..probe::AT_REST
        };
        let (mut app, wid, session) = probe::glass_with(&live, facts);
        app.config.serious_mode = Some(true);
        app.config.cursor_trail = Some(false);
        app.config.cursor_trail_style = Some("off".into());
        app.recompute_sparkle();
        if app.windows[&wid].cols != cols {
            assert!(app.apply_term_resize(wid, probe::ROWS, cols));
        }
        let mut screen = probe::screen_at(&live, probe::ROWS, cols);
        screen[0] = format!("SYNTHETIC FIXTURE · {name} · no live account");
        probe::feed(&app, session, &screen);
        probe::refill(&mut app, wid);
        let _ = probe::pass(&mut app, wid);
        let before = (app.windows[&wid].rows, app.windows[&wid].cols);
        app.redraw_window(wid);
        let png = crate::scripted_redraw_fixture::headless(&mut app, |app| {
            let (reply, rx) = std::sync::mpsc::channel();
            app.render_image(crate::control::ImageReq {
                handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
                target: crate::control_auth::ConfinedImage::in_memory(),
                clean: false,
                session: None,
                want_bytes: true,
                want_metadata: false,
                frame_metadata: Arc::new(OnceLock::new()),
                cancel: crate::control::CaptureCancellation::new(),
                reply,
            });
            rx.recv_timeout(std::time::Duration::from_secs(60))
                .expect("image reply")
                .expect("fixture image")
                .value
                .2
                .expect("PNG bytes")
        });
        assert_eq!((app.windows[&wid].rows, app.windows[&wid].cols), before);
        let file = format!("fixture-{name}.png");
        std::fs::write(dir.join(&file), png).expect("fixture PNG");
        manifest.push_str(&format!("{name}\t{cols}\t{}\t{file}\n", probe::ROWS));
    }
    std::fs::write(dir.join("manifest.tsv"), manifest).expect("fixture manifest");
}
