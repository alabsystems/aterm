// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::ai::connector::Cli;

fn homes() -> (aterm_tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = aterm_tempfile::Builder::new()
        .prefix("aterm-accounts-overlay-")
        .tempdir()
        .expect("tmp");
    let default_home = tmp.path().join("codex-home");
    let overlay = tmp.path().join("logins").join("work");
    std::fs::create_dir_all(&default_home).expect("default home");
    (tmp, default_home, overlay)
}

#[test]
fn shared_names_become_links_to_the_default_home_and_a_second_call_makes_none() {
    let (_tmp, default_home, overlay) = homes();
    std::fs::write(default_home.join("state_5.sqlite"), b"db").expect("db");
    std::fs::write(default_home.join("state_5.sqlite-wal"), b"wal").expect("wal");
    std::fs::write(default_home.join("auth.json"), b"{}").expect("login");
    let seam = Cli::Codex.accounts().seam;
    let first = ensure(seam, &overlay, &default_home).expect("ensure");
    assert!(first.drift.is_empty(), "{:?}", first.drift);
    for name in ["sessions", "history.jsonl", "config.toml", "state_5.sqlite"] {
        assert!(
            first.made.iter().any(|m| m == name),
            "{name}: {:?}",
            first.made
        );
        let at = overlay.join(name);
        assert!(
            std::fs::symlink_metadata(&at)
                .expect(name)
                .file_type()
                .is_symlink(),
            "{name} is a link"
        );
        assert_eq!(
            std::fs::read_link(&at).expect(name),
            default_home.join(name)
        );
    }
    assert!(
        default_home.join("sessions").is_dir(),
        "the linked dir was made"
    );
    for own in ["auth.json", "state_5.sqlite-wal", "models_cache.json"] {
        assert!(
            std::fs::symlink_metadata(overlay.join(own)).is_err(),
            "{own} is the overlay's own"
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&overlay)
            .expect("overlay")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
    }
    let again = ensure(seam, &overlay, &default_home).expect("again");
    assert_eq!(again, Ensured::default(), "a second call is a no-op");
}

#[test]
fn a_real_entry_or_a_foreign_link_is_left_and_reported_as_drift() {
    let (tmp, default_home, overlay) = homes();
    let sessions = overlay.join("sessions");
    std::fs::create_dir_all(&sessions).expect("own sessions");
    std::fs::write(sessions.join("rollout.jsonl"), b"kept").expect("rollout");
    std::fs::write(overlay.join("history.jsonl"), b"own").expect("history");
    let elsewhere = tmp.path().join("elsewhere.toml");
    std::os::unix::fs::symlink(&elsewhere, overlay.join("config.toml")).expect("foreign link");
    let got = ensure(Cli::Codex.accounts().seam, &overlay, &default_home).expect("ensure");
    assert!(got.drift.contains(&Drift {
        name: "sessions".into(),
        is: Is::Dir
    }));
    assert!(got.drift.contains(&Drift {
        name: "history.jsonl".into(),
        is: Is::File
    }));
    assert!(got.drift.contains(&Drift {
        name: "config.toml".into(),
        is: Is::LinkTo(elsewhere.clone())
    }));
    assert!(!got.made.iter().any(|m| m == "sessions"));
    assert_eq!(
        std::fs::read(sessions.join("rollout.jsonl")).expect("untouched"),
        b"kept"
    );
    assert!(
        !std::fs::symlink_metadata(&sessions)
            .expect("sessions")
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        std::fs::read_link(overlay.join("config.toml")).expect("kept"),
        elsewhere
    );
}

#[test]
fn a_seam_that_is_no_overlay_makes_nothing() {
    let (_tmp, default_home, overlay) = homes();
    for cli in [Cli::Claude, Cli::Grok] {
        let got = ensure(cli.accounts().seam, &overlay, &default_home).expect("ensure");
        assert_eq!(got, Ensured::default(), "{}", cli.id());
    }
    assert!(
        !overlay.exists(),
        "no home is made for a seam that is not one"
    );
}
