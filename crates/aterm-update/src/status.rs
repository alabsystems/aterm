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
    /// RFC3339 UTC time this record was written — the file's ANY-WRITER clock.
    updated_at: String,
    /// Last completed-check receipt's timestamp, copied for operator visibility.
    /// The dedup gate reads the source/build-bound check.toml receipt itself;
    /// no generic status writer may manufacture a completed check.
    #[serde(skip_serializing_if = "Option::is_none")]
    checked_at: Option<String>,
    /// Whether the updater runs on this platform ([`crate::enabled`]: macOS). Not the
    /// Settings switch — `[update] enabled = false` stops only the background checker
    /// ([`crate::automatic`]). No pinned anchor is required (the default Tier REPO);
    /// inertness on unsigned/repo builds comes from `bundle::resolve`, not from this flag.
    enabled: bool,
    /// The running build number.
    current_build: u64,
    /// Build number currently staged for next-launch apply, if any.
    staged_build: Option<u64>,
    /// Git commit of the staged build's source (from the ready marker), if known —
    /// so an operator can bind the staged build to a repo commit from this file alone.
    staged_commit: Option<String>,
    /// Last decision, e.g. "up to date", "staged 0.3.0 (build N)",
    /// "deferred: install location not writable".
    outcome: &'a str,
    /// `deferred` (the host asked us to wait) or `blocked` (the download host did not
    /// serve an asset the release names — booked as `pipeline`). Absent on a healthy
    /// check. Never contains whitespace: it is one token of the status line.
    #[serde(skip_serializing_if = "Option::is_none")]
    delivery: Option<String>,
    /// The release tag this ledger last AUTHORIZED end to end (verified, and then
    /// staged, covered, or found up to date). The web lane's steady state: a check whose
    /// evergreen pointer names this tag stops at its one HEAD — but only while the
    /// ledger still describes THIS machine's decision: the reader ([`latest_tag`])
    /// honours it only when `latest_authorized_build` is the caller's build (a manual downgrade
    /// or an applied stage moved the build, so the old verdict no longer applies) and
    /// `latest_source` is the caller's channel (a repointed updater has never judged
    /// the new repository's release). STICKY across records — every writer (the apply
    /// lane included) carries the file's value forward unless the check set a new one —
    /// because `status.toml` is one overwritten line; CLEARED by
    /// [`clear_latest_tag`] when a stage is retired, so the next check re-fetches and
    /// re-stages instead of reporting "up to date" on a retired build forever.
    #[serde(skip_serializing_if = "Option::is_none")]
    latest_tag: Option<String>,
    /// The `owner/repo` the `latest_tag` was authorized against. Optional (schema 1);
    /// an older file without it is read as "no tag", which costs one unmetered re-fetch.
    #[serde(skip_serializing_if = "Option::is_none")]
    latest_source: Option<String>,
    /// The running build that actually authorized `latest_tag`. Kept separate from
    /// `current_build`: an unrelated status write must not rebind cached authority.
    #[serde(skip_serializing_if = "Option::is_none")]
    latest_authorized_build: Option<u64>,
    /// The signed release build that the cached decision judged. If it is newer
    /// than the running build, the shortcut also requires a still-publishable stage
    /// covering it. Missing fields in an older ledger force one fresh check.
    #[serde(skip_serializing_if = "Option::is_none")]
    latest_release_build: Option<u64>,
}

/// What the current check learned about its delivery, carried onto every record it
/// writes (the ledger is one overwritten line, so a fact learned mid-check must survive
/// the check's terminal outcome). Set by the check as it goes, cleared at the start of
/// the next one.
#[derive(Clone, Default)]
struct Delivery {
    note: Option<String>,
    /// A tag the CURRENT check authorized, with the `owner/repo` it was authorized
    /// against; `None` means "carry the file's forward".
    latest: Option<LatestRecord>,
}

static DELIVERY: std::sync::Mutex<Delivery> = std::sync::Mutex::new(Delivery {
    note: None,
    latest: None,
});

/// Record how this check ended short of a healthy outcome (`deferred` / `blocked`).
pub(crate) fn set_delivery_note(note: &str) {
    DELIVERY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .note = Some(note.to_string());
}

/// Record the release tag THIS check authorized end to end, against `source`. Written
/// onto every record from now on (and carried forward by every later writer), so the
/// next check on the same build and channel can stop at its HEAD when the
/// pointer still names it.
pub(crate) fn set_latest_tag(
    tag: &str,
    source: &crate::Source,
    current_build: u64,
    release_build: u64,
) {
    DELIVERY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .latest = Some(LatestRecord {
        tag: tag.to_string(),
        source: source_slug(source),
        build: current_build,
        release_build,
    });
}

/// `owner/repo`, the spelling `latest_source` records.
fn source_slug(source: &crate::Source) -> String {
    let mut slug = source.owner.clone();
    slug.push('/');
    slug.push_str(&source.repo);
    slug
}

/// What the ledger on disk says about the last authorized release, raw: the tag, the
/// source it was recorded against, and the build that recorded it.
#[derive(Clone)]
struct LatestRecord {
    tag: String,
    source: String,
    build: u64,
    release_build: u64,
}

/// Read [`LatestRecord`] off the ledger. Absent, unreadable or empty tag ⇒ `None`.
fn read_latest(staging: &Staging) -> Option<LatestRecord> {
    let text = std::fs::read_to_string(&staging.status).ok()?;
    let v: aterm_toml::Value = text.parse().ok()?;
    let tag = v
        .get("latest_tag")
        .and_then(aterm_toml::Value::as_str)
        .filter(|s| !s.is_empty())?
        .to_string();
    Some(LatestRecord {
        tag,
        source: v
            .get("latest_source")
            .and_then(aterm_toml::Value::as_str)?
            .to_string(),
        build: v
            .get("latest_authorized_build")
            .and_then(aterm_toml::Value::as_integer)
            .and_then(|b| u64::try_from(b).ok())?,
        release_build: v
            .get("latest_release_build")
            .and_then(aterm_toml::Value::as_integer)
            .and_then(|b| u64::try_from(b).ok())?,
    })
}

/// The tag the ledger last authorized FOR THIS BUILD AND SOURCE, if any — the only
/// tag the steady-state shortcut may trust. `None` whenever the ledger is
/// absent, unreadable or empty, was written by a DIFFERENT build (the machine moved by
/// an apply, a manual install or a downgrade, so the old verdict is about another
/// build), or was authorized against a DIFFERENT `owner/repo` (a repointed updater has
/// never judged this repository's release; an older file that recorded no source is
/// treated the same way), or the newer stage it depended on is no longer publishable.
/// The next check then fetches and re-judges the head, which costs unmetered requests only.
pub(crate) fn latest_tag(
    staging: &Staging,
    current_build: u64,
    source: &crate::Source,
) -> Option<String> {
    let latest = read_latest(staging)?;
    if latest.build != current_build {
        return None;
    }
    let slug = source_slug(source);
    if !latest.source.eq_ignore_ascii_case(&slug) {
        return None;
    }
    if latest.release_build > current_build
        && !crate::manifest::Ready::read_publishable(staging)
            .is_some_and(|ready| ready.build_number >= latest.release_build)
    {
        return None;
    }
    Some(latest.tag)
}

/// Forget the authorized tag — on disk and for the rest of this process's check — so
/// the next check re-fetches and re-judges the channel head. Called when a published
/// stage is RETIRED ([`Staging::retire_published`]): "up to date (channel head vX)"
/// would otherwise be the answer of every later check while the stage that verdict
/// rested on is gone, and the machine would sit on the old build until the publisher
/// cut a NEW tag. Best-effort, like every write to this file; even if another writer
/// restores an old tag, the reader rechecks its required stage. A lost carry-forward
/// costs one unmetered re-fetch.
pub(crate) fn clear_latest_tag(staging: &Staging) {
    DELIVERY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .latest = None;
    let Ok(text) = std::fs::read_to_string(&staging.status) else {
        return;
    };
    let Ok(mut v) = text.parse::<aterm_toml::Value>() else {
        return;
    };
    let Some(table) = v.as_table_mut() else {
        return;
    };
    if table.remove("latest_tag").is_none() {
        return;
    }
    table.remove("latest_source");
    table.remove("latest_authorized_build");
    table.remove("latest_release_build");
    let Ok(text) = aterm_toml::to_string(&v) else {
        return;
    };
    let tmp = temp_path(staging);
    if std::fs::write(&tmp, text).is_err() || std::fs::rename(&tmp, &staging.status).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Forget the current check's delivery facts (the next check starts clean).
fn clear_delivery() {
    *DELIVERY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Delivery::default();
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

/// Forget the current check note and the delivery facts (the next check starts clean).
pub(crate) fn clear_check_note() {
    *CHECK_NOTE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    clear_delivery();
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
    let delivery = DELIVERY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    // STICKY: a writer that did not authorize a release (the apply lane, a deferral, a
    // refusal) carries the file's `latest_tag` (and its source) forward rather than
    // erasing it — the ledger is one overwritten line, and losing the tag costs the
    // next check a full (unmetered) re-fetch for nothing.
    let latest = delivery.latest.or_else(|| read_latest(staging));
    let updated_at = crate::install::now_rfc3339();
    let checked_at = crate::check_receipt::completed_at(staging);
    let status = Status {
        schema: 1,
        updated_at,
        checked_at,
        enabled: crate::enabled(),
        current_build,
        staged_build: ready.as_ref().map(|r| r.build_number),
        staged_commit: ready.and_then(|r| r.commit),
        outcome,
        delivery: delivery.note,
        latest_tag: latest.as_ref().map(|record| record.tag.clone()),
        latest_source: latest.as_ref().map(|record| record.source.clone()),
        latest_authorized_build: latest.as_ref().map(|record| record.build),
        latest_release_build: latest.as_ref().map(|record| record.release_build),
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

    /// A delivery note rides every record of the check that set it, and the next check
    /// starts clean. Neither touches the health ledger.
    #[test]
    fn a_delivery_note_rides_the_check_and_the_next_check_starts_clean() {
        let _guard = crate::STRANDED_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let s = Staging::scratch("st-delivery");
        let root = s.root.clone();
        clear_check_note();
        set_delivery_note("deferred");
        record(
            &s,
            42,
            "update check deferred: the release host answered HTTP 429",
        );
        let text = std::fs::read_to_string(&s.status).unwrap();
        let v: aterm_toml::Value = aterm_toml::from_str(&text).expect("valid TOML");
        assert_eq!(
            v.get("schema").and_then(aterm_toml::Value::as_integer),
            Some(1)
        );
        assert_eq!(
            v.get("delivery").and_then(aterm_toml::Value::as_str),
            Some("deferred")
        );
        assert!(!s.health().exists(), "a deferral never writes health.toml");
        clear_check_note();
        record(&s, 42, "up to date");
        let text = std::fs::read_to_string(&s.status).unwrap();
        assert!(!text.contains("delivery"), "{text}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn record_writes_a_parseable_status_file() {
        let s = Staging::scratch("st");
        let root = s.root.clone();
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
        let staging = Staging::scratch("status-tmp");
        let root = staging.root.clone();

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
        let staging = Staging::scratch("status-publishable");
        let root = staging.root.clone();
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

    /// Invariant (f): `status.toml` stays schema 1 with `latest_tag` OPTIONAL. An older
    /// ledger (no such key) reads as "no tag" and is otherwise untouched; a record that
    /// authorized a tag writes it; a later writer that set nothing (the apply lane, a
    /// deferral) carries the file's forward; an empty value is "no tag"; and an OLDER
    /// reader — one that knows none of the new keys — still parses the new file.
    #[test]
    fn latest_tag_is_schema_one_optional_sticky_and_readable_by_an_older_reader() {
        let _guard = crate::STRANDED_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let s = Staging::scratch("st-tag");
        let root = s.root.clone();
        let source = crate::Source {
            owner: "alabsystems".into(),
            repo: "aterm".into(),
        };
        // An OLDER ledger, exactly as a pre-pointer build wrote it.
        std::fs::write(
            &s.status,
            "schema = 1\nupdated_at = \"2026-09-01T00:00:00Z\"\ncurrent_build = 41\n\
             outcome = \"up to date\"\nlane = \"anonymous\"\n",
        )
        .unwrap();
        assert_eq!(
            latest_tag(&s, 41, &source),
            None,
            "an old file carries no tag"
        );
        // A check that authorized a tag records it, with the source it judged.
        clear_check_note();
        set_latest_tag("v0.74.0", &source, 42, 42);
        record(&s, 42, "up to date (channel head v0.74.0)");
        let text = std::fs::read_to_string(&s.status).unwrap();
        let v: aterm_toml::Value = aterm_toml::from_str(&text).expect("valid TOML");
        assert_eq!(
            v.get("schema").and_then(aterm_toml::Value::as_integer),
            Some(1)
        );
        assert_eq!(
            v.get("latest_tag").and_then(aterm_toml::Value::as_str),
            Some("v0.74.0")
        );
        assert_eq!(
            v.get("latest_source").and_then(aterm_toml::Value::as_str),
            Some("alabsystems/aterm")
        );
        // A later writer that set nothing — `record` cleared the delivery facts — carries
        // the file's tag forward rather than erasing it.
        record(&s, 42, "held: a later record that authorized nothing");
        assert_eq!(latest_tag(&s, 42, &source).as_deref(), Some("v0.74.0"));
        let text = std::fs::read_to_string(&s.status).unwrap();
        assert!(text.contains("latest_tag = \"v0.74.0\""), "{text}");
        assert!(
            text.contains("latest_source = \"alabsystems/aterm\""),
            "the source is carried forward with the tag: {text}"
        );
        // THE BINDING. The tag is trusted only by the build that recorded it and only
        // for the source it was judged against: a record written by build 42 answers
        // nothing to build 41 (a manual downgrade) or 43 (an applied stage) — and
        // nothing to a repointed updater, whose new repository this ledger never
        // judged. GitHub slugs are case-insensitive, so a case variant is the same
        // source.
        for other in [41, 43] {
            assert_eq!(latest_tag(&s, other, &source), None, "build {other}");
        }
        assert_eq!(
            latest_tag(
                &s,
                42,
                &crate::Source {
                    owner: "example".into(),
                    repo: "mirror".into(),
                }
            ),
            None
        );
        assert_eq!(
            latest_tag(
                &s,
                42,
                &crate::Source {
                    owner: "Alabsystems".into(),
                    repo: "ATERM".into(),
                }
            )
            .as_deref(),
            Some("v0.74.0")
        );
        // A tag recorded without a source (an earlier build of this design) binds to
        // nothing: it is re-judged, at the cost of unmetered requests only.
        std::fs::write(
            &s.status,
            "schema = 1\ncurrent_build = 42\nlatest_tag = \"v0.74.0\"\n",
        )
        .unwrap();
        assert_eq!(latest_tag(&s, 42, &source), None);
        // RETIREMENT clears the tag on disk (and the source with it) but nothing else
        // in the record, and a record written afterwards does not resurrect it.
        set_latest_tag("v0.74.0", &source, 42, 42);
        record(&s, 42, "staged 0.74.0");
        assert_eq!(latest_tag(&s, 42, &source).as_deref(), Some("v0.74.0"));
        s.retire_published();
        assert_eq!(latest_tag(&s, 42, &source), None);
        let text = std::fs::read_to_string(&s.status).unwrap();
        assert!(!text.contains("latest_"), "{text}");
        assert!(text.contains("outcome = \"staged 0.74.0\""), "{text}");
        assert!(text.contains("current_build = 42"), "{text}");
        record(&s, 42, "a later record");
        assert_eq!(latest_tag(&s, 42, &source), None);
        // Clearing a ledger that has no tag, or no file, is a no-op.
        std::fs::write(&s.status, "schema = 1\ncurrent_build = 42\n").unwrap();
        clear_latest_tag(&s);
        assert_eq!(
            std::fs::read_to_string(&s.status).unwrap(),
            "schema = 1\ncurrent_build = 42\n"
        );
        let _ = std::fs::remove_file(&s.status);
        clear_latest_tag(&s);
        assert!(!s.status.exists());
        std::fs::write(
            &s.status,
            "schema = 1\nupdated_at = \"2026-09-01T00:00:00Z\"\ncurrent_build = 42\n\
             outcome = \"held\"\nlatest_tag = \"v0.74.0\"\nlatest_source = \"alabsystems/aterm\"\n",
        )
        .unwrap();
        let text = std::fs::read_to_string(&s.status).unwrap();
        // An OLDER reader knows schema/updated_at/current_build/outcome and nothing
        // else; unknown keys are ignored by a TOML table read, so the new file parses.
        let v: aterm_toml::Value = aterm_toml::from_str(&text).unwrap();
        assert_eq!(
            v.get("current_build")
                .and_then(aterm_toml::Value::as_integer),
            Some(42)
        );
        assert!(
            v.get("outcome")
                .and_then(aterm_toml::Value::as_str)
                .is_some_and(|o| o.contains("held")),
            "{text}"
        );
        // An empty tag is no tag, and an unreadable ledger is no tag.
        std::fs::write(
            &s.status,
            "schema = 1\ncurrent_build = 42\nlatest_tag = \"\"\nlatest_source = \"alabsystems/aterm\"\n",
        )
        .unwrap();
        assert_eq!(latest_tag(&s, 42, &source), None);
        std::fs::write(&s.status, "not toml {{{").unwrap();
        assert_eq!(latest_tag(&s, 42, &source), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    fn write_cache_stage(staging: &Staging, build: u64) {
        let commit = "0123456789abcdef0123456789abcdef01234567";
        let ready = crate::manifest::Ready {
            build_number: build,
            version: "0.85.0".into(),
            commit: Some(commit.into()),
            dmg_sha256: "ab".repeat(32),
            team_id: "T".into(),
            staged_at: String::new(),
            changelog: None,
            machine_id: None,
            roster_seq: None,
        };
        let contents = staging.staged_app.join("Contents");
        std::fs::create_dir_all(&contents).unwrap();
        std::fs::write(
            contents.join("Info.plist"),
            format!(
                "<plist><dict><key>CFBundleVersion</key><string>{build}</string>\
                 <key>ATermGitCommit</key><string>{commit}</string></dict></plist>"
            ),
        )
        .unwrap();
        std::fs::write(&staging.ready, ready.to_toml().unwrap()).unwrap();
    }

    fn cache_decision_matches_model(staging: &Staging, build: u64, source: &crate::Source) -> bool {
        let model = aterm_spec::derive::native_update_web_cache_model();
        let latest = read_latest(staging).expect("test wrote a complete cache record");
        let mut state = model.init_state();
        state.insert("running", i64::try_from(build).unwrap());
        state.insert("author", i64::try_from(latest.build).unwrap());
        state.insert("release", i64::try_from(latest.release_build).unwrap());
        state.insert(
            "stage",
            crate::manifest::Ready::read_publishable(staging)
                .map(|ready| i64::try_from(ready.build_number).unwrap())
                .unwrap_or(0),
        );
        state.insert(
            "same_source",
            i64::from(latest.source.eq_ignore_ascii_case(&source_slug(source))),
        );
        let allowed = model.action_enabled("UseCache", &state);
        assert_eq!(
            latest_tag(staging, build, source).is_some(),
            allowed,
            "the shipping cache reader must agree with the derived guard: {state:?}"
        );
        allowed
    }

    fn historical_cache_would_skip(staging: &Staging, build: u64) -> bool {
        // The old reader trusted the status writer's current_build and never
        // inspected the stage that the authorized release still needed.
        let text = std::fs::read_to_string(&staging.status).unwrap();
        let value: aterm_toml::Value = text.parse().unwrap();
        value.get("latest_tag").is_some()
            && value
                .get("current_build")
                .and_then(aterm_toml::Value::as_integer)
                == Some(i64::try_from(build).unwrap())
    }

    #[test]
    fn cache_rechecks_its_stage_and_never_rebinds_authority_on_a_status_write() {
        let _guard = crate::STRANDED_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let staging = Staging::scratch("cache-evidence");
        let source = crate::Source {
            owner: "alabsystems".into(),
            repo: "aterm".into(),
        };
        clear_check_note();
        write_cache_stage(&staging, 2);
        set_latest_tag("v0.85.0", &source, 1, 2);
        record(&staging, 1, "verified stage published");
        assert!(cache_decision_matches_model(&staging, 1, &source));

        // A failed replacement can remove the old marker before the new marker
        // commits. No explicit retirement/clear ran: the reader must notice.
        std::fs::remove_file(&staging.ready).unwrap();
        clear_check_note();
        record(&staging, 1, "replacement publication failed");
        assert!(!cache_decision_matches_model(&staging, 1, &source));
        assert!(
            historical_cache_would_skip(&staging, 1),
            "negative control: the old shortcut would strand this missing stage"
        );

        write_cache_stage(&staging, 2);
        std::fs::remove_dir_all(&staging.staged_app).unwrap();
        assert!(!cache_decision_matches_model(&staging, 1, &source));
        write_cache_stage(&staging, 1);
        assert!(!cache_decision_matches_model(&staging, 1, &source));
        write_cache_stage(&staging, 2);
        assert!(cache_decision_matches_model(&staging, 1, &source));
        std::fs::write(staging.staged_app.join("Contents/Info.plist"), "corrupt").unwrap();
        assert!(!cache_decision_matches_model(&staging, 1, &source));
        write_cache_stage(&staging, 3);
        assert!(cache_decision_matches_model(&staging, 1, &source));

        // A new process writes a refusal before its first network check. Neither
        // an in-process carry nor a disk-only carry may mint authority for it.
        set_latest_tag("v0.85.0", &source, 1, 2);
        for in_memory in [true, false] {
            if !in_memory {
                clear_check_note();
            }
            record(&staging, 0, "apply refused before the first check");
            assert!(!cache_decision_matches_model(&staging, 0, &source));
            assert!(historical_cache_would_skip(&staging, 0));
            assert_eq!(read_latest(&staging).unwrap().build, 1);
        }
        assert!(!cache_decision_matches_model(
            &staging,
            1,
            &crate::Source {
                owner: "example".into(),
                repo: "mirror".into(),
            }
        ));

        // A signed release already covered by the running build needs no stage.
        set_latest_tag("v0.85.0", &source, 1, 1);
        record(&staging, 1, "up to date");
        std::fs::remove_file(&staging.ready).unwrap();
        assert!(cache_decision_matches_model(&staging, 1, &source));
        clear_check_note();
        let _ = std::fs::remove_dir_all(staging.root);
    }
}
