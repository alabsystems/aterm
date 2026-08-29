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

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

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

/// A scratch path no other in-flight writer of this record can be holding.
///
/// The rename is what makes the write atomic; picking the SOURCE file is what has to
/// be exclusive, and a per-pid name only got that half right. It separates processes,
/// but [`record`] is called from several lanes inside one process — the background
/// check, the apply path and the control socket — so two threads could land on the
/// same `status.toml.<pid>.tmp`: the loser's `write` truncates and rewrites the file
/// the winner is about to rename, and the winner publishes the loser's (or a spliced)
/// bytes. The counter closes that, so each writer renames exactly what it wrote (F18).
fn temp_path(staging: &Staging) -> PathBuf {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    staging
        .root
        .join(format!("status.toml.{}.{sequence}.tmp", std::process::id()))
}

/// A note a check wants carried onto EVERY status record it writes after a
/// notable event — a revocation retiring a staged build — because `status.toml` is
/// a single overwritten line and the check goes on to record its terminal outcome
/// moments later, which used to erase the one sentence that explained where the
/// stage went (2026-08-19 round-2 audit). Set by the event, cleared at the start of
/// the next check.
static CHECK_NOTE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Attach `note` to every status record until [`clear_check_note`].
pub(crate) fn set_check_note(note: String) {
    *CHECK_NOTE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(note);
}

/// Forget the current check note (the next check starts clean).
pub(crate) fn clear_check_note() {
    *CHECK_NOTE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
}

/// Atomically write the status record (temp + rename). Best-effort: failures are
/// silent — status is diagnostics, never load-bearing.
pub fn record(staging: &Staging, current_build: u64, outcome: &str) {
    let ready = crate::manifest::Ready::read_publishable(staging);
    let noted;
    let outcome = match CHECK_NOTE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_deref()
    {
        Some(note) if !outcome.contains(note) => {
            noted = format!("{outcome} · {note}");
            noted.as_str()
        }
        _ => outcome,
    };
    let status = Status {
        schema: 1,
        updated_at: crate::install::now_rfc3339(),
        enabled: crate::enabled(),
        current_build,
        staged_build: ready.as_ref().map(|r| r.build_number),
        staged_commit: ready.and_then(|r| r.commit),
        outcome,
    };
    let Ok(text) = aterm_toml::to_string(&status) else {
        return;
    };
    let tmp = temp_path(staging);
    if std::fs::write(&tmp, text).is_ok() && std::fs::rename(&tmp, &staging.status).is_ok() {
        return;
    }
    // BOTH arms have to reclaim. Under the old per-pid name a leaked scratch was
    // overwritten by the next `record`, so a failed rename cost one stale file forever-at-
    // most; a per-writer name has no such self-healing, and every rename failure (a
    // `status.toml` replaced by a directory, a permissions fault on `Updates/`) would
    // strand a distinct `status.toml.<pid>.<seq>.tmp` that nothing sweeps.
    let _ = std::fs::remove_file(&tmp);
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
        let _: aterm_toml::Value = aterm_toml::from_str(&text).expect("status is valid TOML");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Concurrent writers inside ONE process must not share a scratch file. Keying the
    /// temp on the pid alone handed every thread the same path, so one writer's `write`
    /// could truncate another's bytes in the window before its `rename` — and the
    /// rename would then publish a half-written record. Threads, not just repeated
    /// calls, because that is the shape the old name actually collided in.
    #[test]
    fn concurrent_writers_never_share_a_temp_path() {
        let root = std::env::temp_dir().join(format!("aterm-status-tmp-{}", std::process::id()));
        let staging = Staging {
            apply_lock: root.join("apply.lock"),
            stage_lock: root.join("stage.lock"),
            download: root.join("download"),
            staged_app: root.join("staged/aterm.app"),
            ready: root.join("ready.toml"),
            status: root.join("status.toml"),
            root: root.clone(),
        };

        let paths: std::collections::BTreeSet<_> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| scope.spawn(|| (0..8).map(|_| temp_path(&staging)).collect::<Vec<_>>()))
                .collect();
            handles
                .into_iter()
                .flat_map(|h| h.join().expect("writer thread"))
                .collect()
        });

        assert_eq!(
            paths.len(),
            64,
            "every writer must get its own scratch file"
        );
        for path in &paths {
            assert_eq!(path.parent(), Some(root.as_path()));
            assert!(
                path.extension().is_some_and(|e| e == "tmp"),
                "scratch must stay distinguishable from the record itself: {path:?}"
            );
            assert_ne!(
                path, &staging.status,
                "the scratch path may never be the published record"
            );
        }
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
            machine_id: None,
            roster_seq: None,
        };
        std::fs::write(&staging.ready, ready.to_toml().unwrap()).unwrap();

        record(&staging, 53, "staged marker observed");
        let without_app: aterm_toml::Value =
            aterm_toml::from_str(&std::fs::read_to_string(&staging.status).unwrap()).unwrap();
        assert!(
            without_app.get("staged_build").is_none(),
            "status cannot grant readiness from marker metadata alone"
        );

        std::fs::create_dir_all(&staging.staged_app).unwrap();
        record(&staging, 53, "staged bundle observed");
        let empty_app: aterm_toml::Value =
            aterm_toml::from_str(&std::fs::read_to_string(&staging.status).unwrap()).unwrap();
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
        let with_app: aterm_toml::Value =
            aterm_toml::from_str(&std::fs::read_to_string(&staging.status).unwrap()).unwrap();
        assert_eq!(
            with_app
                .get("staged_build")
                .and_then(aterm_toml::Value::as_integer),
            Some(54)
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
