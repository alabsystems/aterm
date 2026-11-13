// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::fold_installed_activation;
use crate::native_updater_service::{StagedUpdate, installed_activation_digest};

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

fn ledger() -> aterm_update::UpdateStatus {
    aterm_update::UpdateStatus {
        enabled: true,
        installable: true,
        current_build: 10,
        staged_build: None,
        staged_version: None,
        staged_commit: None,
        staged_dmg_sha256: None,
        changelog: None,
        outcome: "up to date".into(),
        updated_at: String::new(),
        failing_checks: 0,
        failing_kind: String::new(),
        failing_applies: 0,
        failing_since: String::new(),
        failing_persistent: false,
        failing_checks_kind: String::new(),
        channel_unreadable: false,
        linux: None,
    }
}

const DOWNLOAD_OUTCOME: &str = "staged 0.39.5 (build 11) — verified and ready to apply";

fn ledger_with_download() -> aterm_update::UpdateStatus {
    aterm_update::UpdateStatus {
        staged_build: Some(11),
        staged_version: Some("0.39.5".into()),
        staged_commit: Some("f".repeat(40)),
        staged_dmg_sha256: Some("c".repeat(64)),
        changelog: Some("notes".into()),
        outcome: DOWNLOAD_OUTCOME.into(),
        ..ledger()
    }
}

fn activation(build: u64, version: &str) -> StagedUpdate {
    StagedUpdate {
        build,
        version: version.into(),
        commit: Some(COMMIT.into()),
        dmg_sha256: installed_activation_digest(build, COMMIT),
        changelog: None,
        generation: 1,
    }
}

#[test]
fn the_reducers_activation_is_the_stage_in_its_own_words() {
    for (enabled, version, said) in [
        (true, "0.40.0", Some("0.40.0")),
        (true, "build 12", None),
        (false, "0.40.0", Some("0.40.0")),
        (false, "build 12", None),
    ] {
        let mut st = ledger_with_download();
        st.enabled = enabled;
        fold_installed_activation(&mut st, Some(&activation(12, version)));
        assert_eq!(st.current_build, 10, "the target is not the running build");
        assert_eq!(st.staged_build, Some(12), "{version}");
        assert_eq!(st.staged_version.as_deref(), Some(version));
        assert_eq!(st.staged_commit.as_deref(), Some(COMMIT), "{version}");
        assert_eq!(st.staged_dmg_sha256, None, "an activation has no download");
        assert_eq!(st.changelog, None, "nor the download's notes");
        assert_eq!(
            st.outcome,
            format!(
                "{} (ledger: {DOWNLOAD_OUTCOME})",
                crate::native_updater_service::NativeUpdaterService::installed_activation_outcome(
                    said
                )
            )
        );
    }
}

#[test]
fn nothing_but_a_newer_activation_folds() {
    let mut download = activation(12, "0.40.0");
    download.dmg_sha256 = "b".repeat(64);
    for (mut st, stage) in [
        (ledger(), Some(download)),
        (ledger(), Some(activation(10, "0.39.0"))),
        (ledger(), None),
    ] {
        let before = format!("{st:?}");
        fold_installed_activation(&mut st, stage.as_ref());
        assert_eq!(format!("{st:?}"), before, "{stage:?}");
    }
}
