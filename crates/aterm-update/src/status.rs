// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The operator-readable status record (`…/aterm/Updates/status.toml`).
//!
//! A silent updater has no UI, so this file IS the observability surface: it
//! records when the updater last ran, what it decided, and whether a build is
//! staged. An operator can `cat` it to answer "is this machine receiving updates,
//! and why didn't the last one apply?" without any in-app prompt. Diagnostics also
//! go to the app log via [`crate::log`]/[`crate::warn`]; this file is the durable,
//! at-a-glance summary.

use serde::Serialize;

use crate::paths::Staging;

/// Snapshot written after each check / apply decision.
#[derive(Serialize)]
struct Status<'a> {
    schema: u32,
    /// RFC3339 UTC time this record was written.
    updated_at: String,
    /// Whether the updater is configured to act: a macOS installed `.app` and not
    /// opted out via `ATERM_NO_AUTO_UPDATE`. No pinned anchor is required (the
    /// default Tier REPO); inertness on unsigned/repo builds comes from
    /// `bundle::resolve`, not from this flag.
    enabled: bool,
    /// The running build number.
    current_build: u64,
    /// Build number currently staged for next-launch apply, if any.
    staged_build: Option<u64>,
    /// Git commit of the staged build's source (from the ready marker), if known —
    /// so an operator can bind the staged build to a repo commit from this file alone.
    staged_commit: Option<String>,
    /// Last decision, e.g. "up to date", "staged 0.3.0 (build N)", "idle: no
    /// token", "deferred: install location not writable".
    outcome: &'a str,
}

/// Atomically write the status record (temp + rename). Best-effort: failures are
/// silent — status is diagnostics, never load-bearing.
pub fn record(staging: &Staging, current_build: u64, outcome: &str) {
    let ready = crate::manifest::Ready::read_publishable(staging);
    let status = Status {
        schema: 1,
        updated_at: crate::install::now_rfc3339(),
        enabled: crate::enabled(),
        current_build,
        staged_build: ready.as_ref().map(|r| r.build_number),
        staged_commit: ready.and_then(|r| r.commit),
        outcome,
    };
    let Ok(text) = toml::to_string(&status) else {
        return;
    };
    // Per-pid temp so two instances writing status concurrently can't truncate each
    // other's in-progress write before the atomic rename (F18).
    let tmp = staging
        .root
        .join(format!("status.toml.{}.tmp", std::process::id()));
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, &staging.status);
    } else {
        let _ = std::fs::remove_file(&tmp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_writes_a_parseable_status_file() {
        let root = std::env::temp_dir().join(format!("aterm-st-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let s = Staging {
            apply_lock: root.join("apply.lock"),
            stage_lock: root.join("stage.lock"),
            download: root.join("download"),
            staged_app: root.join("staged").join("aterm.app"),
            ready: root.join("ready.toml"),
            status: root.join("status.toml"),
            root: root.clone(),
        };
        record(&s, 42, "up to date (test)");
        let text = std::fs::read_to_string(&s.status).expect("status file written");
        assert!(text.contains("current_build = 42"), "got: {text}");
        assert!(text.contains("up to date (test)"), "got: {text}");
        // It must be valid TOML.
        let _: toml::Value = toml::from_str(&text).expect("status is valid TOML");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn record_never_advertises_marker_without_publishable_stage() {
        let root =
            std::env::temp_dir().join(format!("aterm-status-publishable-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let staging = Staging {
            apply_lock: root.join("apply.lock"),
            stage_lock: root.join("stage.lock"),
            download: root.join("download"),
            staged_app: root.join("staged/aterm.app"),
            ready: root.join("ready.toml"),
            status: root.join("status.toml"),
            root: root.clone(),
        };
        let ready = crate::manifest::Ready {
            build_number: 54,
            version: "0.54.1".into(),
            commit: Some("0123456789abcdef0123456789abcdef01234567".into()),
            dmg_sha256: "ab".repeat(32),
            team_id: "T".into(),
            staged_at: String::new(),
            changelog: None,
        };
        std::fs::write(&staging.ready, ready.to_toml().unwrap()).unwrap();

        record(&staging, 53, "staged marker observed");
        let without_app: toml::Value =
            toml::from_str(&std::fs::read_to_string(&staging.status).unwrap()).unwrap();
        assert!(
            without_app.get("staged_build").is_none(),
            "status cannot grant readiness from marker metadata alone"
        );

        std::fs::create_dir_all(&staging.staged_app).unwrap();
        record(&staging, 53, "staged bundle observed");
        let empty_app: toml::Value =
            toml::from_str(&std::fs::read_to_string(&staging.status).unwrap()).unwrap();
        assert!(
            empty_app.get("staged_build").is_none(),
            "empty app directory cannot grant status readiness"
        );
        let contents = staging.staged_app.join("Contents");
        std::fs::create_dir_all(&contents).unwrap();
        std::fs::write(
            contents.join("Info.plist"),
            "<plist><dict><key>CFBundleVersion</key><string>54</string>\
             <key>ATermGitCommit</key>\
             <string>0123456789abcdef0123456789abcdef01234567</string></dict></plist>",
        )
        .unwrap();
        record(&staging, 53, "staged bundle observed");
        let with_app: toml::Value =
            toml::from_str(&std::fs::read_to_string(&staging.status).unwrap()).unwrap();
        assert_eq!(
            with_app
                .get("staged_build")
                .and_then(toml::Value::as_integer),
            Some(54)
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
