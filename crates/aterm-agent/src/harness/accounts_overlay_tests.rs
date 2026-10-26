// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::ai::connector::{Cli, Seam, Shared};

const TEST_OVERLAY_SEAM: Seam = Seam::Overlay {
    var: "TEST_OVERLAY_HOME",
    shared: &[
        Shared::Dir("sessions"),
        Shared::File("history.jsonl"),
        Shared::File("config.toml"),
        Shared::File("AGENTS.override.md"),
        Shared::File("AGENTS.md"),
        Shared::Suffix(".sqlite"),
    ],
    markers: &[],
};

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
    let seam = TEST_OVERLAY_SEAM;
    let first = ensure(seam, &overlay, &default_home).expect("ensure");
    assert!(first.drift.is_empty(), "{:?}", first.drift);
    for name in [
        "sessions",
        "history.jsonl",
        "config.toml",
        "AGENTS.override.md",
        "AGENTS.md",
        "state_5.sqlite",
    ] {
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
    let got = ensure(TEST_OVERLAY_SEAM, &overlay, &default_home).expect("ensure");
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
fn a_database_only_the_overlay_holds_is_no_drift() {
    let (_tmp, default_home, overlay) = homes();
    let seam = TEST_OVERLAY_SEAM;
    ensure(seam, &overlay, &default_home).expect("ensure");
    std::fs::write(overlay.join("state_5.sqlite"), b"overlay's").expect("db");
    std::fs::write(overlay.join("state_5.sqlite-wal"), b"wal").expect("wal");
    assert_eq!(
        ensure(seam, &overlay, &default_home).expect("again"),
        Ensured::default(),
        "nothing to compare while the default home has none"
    );
}

#[test]
fn a_seam_that_is_no_overlay_makes_nothing() {
    let (_tmp, default_home, overlay) = homes();
    for cli in [Cli::Claude, Cli::Codex, Cli::Grok] {
        let got = ensure(cli.accounts().seam, &overlay, &default_home).expect("ensure");
        assert_eq!(got, Ensured::default(), "{}", cli.id());
    }
    assert!(
        !overlay.exists(),
        "no home is made for a seam that is not one"
    );
}

#[test]
fn a_kimi_overlay_shares_its_sessions_locks_and_trust_and_keeps_its_login_and_catalog() {
    let (_tmp, default_home, overlay) = homes();
    for own in [
        "config.toml",
        "device_id",
        "workspaces.json",
        "session_index.jsonl",
    ] {
        std::fs::write(default_home.join(own), b"default's").expect(own);
    }
    let seam = Cli::Kimi.accounts().seam;
    let first = ensure(seam, &overlay, &default_home).expect("ensure");
    assert!(first.drift.is_empty(), "{:?}", first.drift);
    // kimi offers the older kimi-cli's data to every home holding no marker of its own.
    let marker = ".skip-migration-from-kimi-cli";
    assert_eq!(first.marked, [marker]);
    let meta = std::fs::symlink_metadata(overlay.join(marker)).expect("marker");
    assert!(meta.is_file() && meta.len() == 0, "{meta:?}");
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    }
    assert!(
        std::fs::symlink_metadata(default_home.join(marker)).is_err(),
        "the default home answers the offer itself"
    );
    for name in [
        "sessions",
        "locks",
        "workspace-trust",
        "user-history",
        "tui.toml",
    ] {
        assert!(
            first.made.iter().any(|m| m == name),
            "{name}: {:?}",
            first.made
        );
        assert_eq!(
            std::fs::read_link(overlay.join(name)).expect(name),
            default_home.join(name)
        );
    }
    for dir in ["sessions", "locks", "workspace-trust"] {
        assert!(
            default_home.join(dir).is_dir(),
            "{dir} is made in the default home"
        );
    }
    for own in [
        "config.toml",
        "credentials",
        "device_id",
        "logs",
        "cache",
        "bin",
        "workspaces.json",
        "session_index.jsonl",
        "AGENTS.md",
    ] {
        assert!(
            std::fs::symlink_metadata(overlay.join(own)).is_err(),
            "{own} is the overlay's own"
        );
    }

    // A real `workspaces.json` (kimi renames its own copy over the link) is no drift.
    std::fs::write(overlay.join("workspaces.json"), b"overlay's").expect("catalog");
    assert_eq!(
        ensure(seam, &overlay, &default_home).expect("again"),
        Ensured::default()
    );

    // `AGENTS.md` is linked once the default home has it, and not before.
    std::fs::write(default_home.join("AGENTS.md"), b"# the person's").expect("instructions");
    let later = ensure(seam, &overlay, &default_home).expect("later");
    assert_eq!(later.made, ["AGENTS.md"]);
    assert_eq!(
        std::fs::read(overlay.join("AGENTS.md")).expect("read through the link"),
        b"# the person's"
    );
}

#[test]
fn a_file_linked_only_when_present_is_drift_once_the_default_home_has_it() {
    let (_tmp, default_home, overlay) = homes();
    let seam = Cli::Kimi.accounts().seam;
    std::fs::create_dir_all(&overlay).expect("overlay");
    std::fs::write(overlay.join("AGENTS.md"), b"own").expect("own instructions");
    let got = ensure(seam, &overlay, &default_home).expect("ensure");
    assert!(
        !got.drift.iter().any(|d| d.name == "AGENTS.md"),
        "nothing to compare while the default home has none: {:?}",
        got.drift
    );
    std::fs::write(default_home.join("AGENTS.md"), b"default's").expect("default's");
    let got = ensure(seam, &overlay, &default_home).expect("again");
    assert_eq!(
        got.drift,
        [Drift {
            name: "AGENTS.md".into(),
            is: Is::File
        }]
    );
    assert_eq!(
        std::fs::read(overlay.join("AGENTS.md")).expect("kept"),
        b"own"
    );
}

#[test]
fn a_shared_dir_made_in_a_missing_default_home_is_private() {
    use std::os::unix::fs::PermissionsExt;
    let (tmp, _, overlay) = homes();
    let default_home = tmp.path().join("never-ran");
    ensure(Cli::Kimi.accounts().seam, &overlay, &default_home).expect("ensure");
    for dir in [
        default_home.clone(),
        default_home.join("sessions"),
        default_home.join("locks"),
    ] {
        let mode = std::fs::metadata(&dir).expect("made").permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "{}", dir.display());
    }
}

#[test]
fn a_marker_the_overlay_already_holds_is_left_as_it_is() {
    let (_tmp, default_home, overlay) = homes();
    let marker = ".skip-migration-from-kimi-cli";
    std::fs::create_dir_all(&overlay).expect("overlay");
    std::fs::write(overlay.join(marker), b"kimi's own").expect("marker");
    let got = ensure(Cli::Kimi.accounts().seam, &overlay, &default_home).expect("ensure");
    assert!(got.marked.is_empty(), "{:?}", got.marked);
    assert_eq!(
        std::fs::read(overlay.join(marker)).expect("kept"),
        b"kimi's own"
    );
}

#[test]
fn a_present_link_is_removed_once_the_default_home_has_none_and_a_foreign_one_is_left() {
    let (tmp, default_home, overlay) = homes();
    let seam = Cli::Kimi.accounts().seam;
    std::fs::write(default_home.join("AGENTS.md"), b"# the person's").expect("instructions");
    assert_eq!(
        ensure(seam, &overlay, &default_home)
            .expect("linked")
            .unlinked,
        Vec::<String>::new()
    );
    std::fs::remove_file(default_home.join("AGENTS.md")).expect("the person deletes it");
    let got = ensure(seam, &overlay, &default_home).expect("ensure");
    assert_eq!(got.unlinked, ["AGENTS.md"]);
    assert!(
        std::fs::symlink_metadata(overlay.join("AGENTS.md")).is_err(),
        "no link leads nowhere"
    );

    let elsewhere = tmp.path().join("elsewhere.md");
    std::os::unix::fs::symlink(&elsewhere, overlay.join("AGENTS.md")).expect("foreign link");
    let got = ensure(seam, &overlay, &default_home).expect("again");
    assert!(got.unlinked.is_empty(), "{:?}", got.unlinked);
    assert_eq!(
        std::fs::read_link(overlay.join("AGENTS.md")).expect("kept"),
        elsewhere
    );
}
