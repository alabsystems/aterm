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
    /// RFC3339 UTC epoch until which every aterm process on this machine holds off
    /// GitHub — the server's own `x-ratelimit-reset` for this IP, jittered and clamped
    /// (`github::hold_until_reset`). Present ONLY on a rate-limit deferral that knew the
    /// reset; readers default it, so the record's schema stays 1. The sibling checker
    /// gate (`checker_skip`) reads it FIRST, and releases exactly here rather than by
    /// widening its window.
    #[serde(skip_serializing_if = "Option::is_none")]
    held_until: Option<String>,
    /// Which lane the last check read the channel on: `web` (the unmetered download
    /// host, no credential), or `token:<rung id>` (the releases API, a repointed
    /// source; the id is `aterm_update_core::token::rung_id`'s fixed, whitespace-free
    /// spelling — `env`, `keychain`, `file`, `github-env`, `gh-env`, `gh-cli`).
    #[serde(skip_serializing_if = "Option::is_none")]
    lane: Option<String>,
    /// `deferred` (the host asked us to wait), `blocked` (web lane: the download host
    /// did not serve an asset the release names) or `api-failed` (token lane: the
    /// releases API did not) — the last two booked as `pipeline`. Absent on a healthy
    /// check. Never contains whitespace: it is one token of the status line.
    #[serde(skip_serializing_if = "Option::is_none")]
    delivery: Option<String>,
    /// The release tag this ledger last AUTHORIZED end to end (verified, and then
    /// staged, covered, or found up to date). The web lane's steady state: a check whose
    /// evergreen pointer names this tag stops at its one HEAD — but only while the
    /// ledger still describes THIS machine's decision: the reader ([`latest_tag`])
    /// honours it only when `current_build` is the caller's build (a manual downgrade
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
    /// The API budget this check's LIST headers reported, exactly as GitHub said it —
    /// absent when the headers did not carry it (always, on the web lane).
    #[serde(skip_serializing_if = "Option::is_none")]
    budget_remaining: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    budget_limit: Option<u32>,
    /// RFC3339 UTC rendering of `x-ratelimit-reset`.
    #[serde(skip_serializing_if = "Option::is_none")]
    budget_reset: Option<String>,
}

/// What the current check learned about its delivery lane and budget, carried onto
/// every record it writes (the ledger is one overwritten line, so a fact learned at
/// the LIST must survive the check's terminal outcome). Set by the check as it goes,
/// cleared at the start of the next one.
#[derive(Clone, Default)]
struct Delivery {
    lane: Option<String>,
    note: Option<String>,
    /// A tag the CURRENT check authorized, with the `owner/repo` it was authorized
    /// against; `None` means "carry the file's forward".
    latest: Option<(String, String)>,
    budget_remaining: Option<u32>,
    budget_limit: Option<u32>,
    budget_reset: Option<u64>,
}

static DELIVERY: std::sync::Mutex<Delivery> = std::sync::Mutex::new(Delivery {
    lane: None,
    note: None,
    latest: None,
    budget_remaining: None,
    budget_limit: None,
    budget_reset: None,
});

/// Record the lane the check settled on and the budget its headers reported.
pub(crate) fn set_delivery(
    lane: String,
    budget_remaining: Option<u32>,
    budget_limit: Option<u32>,
    budget_reset: Option<u64>,
) {
    let mut delivery = DELIVERY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    delivery.lane = Some(lane);
    delivery.budget_remaining = budget_remaining;
    delivery.budget_limit = budget_limit;
    delivery.budget_reset = budget_reset;
}

/// Record how this check ended short of a healthy outcome (`deferred` / `blocked`).
pub(crate) fn set_delivery_note(note: &str) {
    DELIVERY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .note = Some(note.to_string());
}

/// Record the release tag THIS check authorized end to end, against `source`. Written
/// onto every record from now on (and carried forward by every later writer), so the
/// next web-lane check on the same build and channel can stop at its HEAD when the
/// pointer still names it.
pub(crate) fn set_latest_tag(tag: &str, source: &crate::Source) {
    DELIVERY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .latest = Some((tag.to_string(), source_slug(source)));
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
struct LatestRecord {
    tag: String,
    source: Option<String>,
    build: Option<u64>,
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
            .and_then(aterm_toml::Value::as_str)
            .map(str::to_string),
        build: v
            .get("current_build")
            .and_then(aterm_toml::Value::as_integer)
            .and_then(|b| u64::try_from(b).ok()),
    })
}

/// The tag the ledger last authorized FOR THIS BUILD AND SOURCE, if any — the only
/// tag the web lane's steady-state shortcut may trust. `None` whenever the ledger is
/// absent, unreadable or empty, was written by a DIFFERENT build (the machine moved by
/// an apply, a manual install or a downgrade, so the old verdict is about another
/// build), or was authorized against a DIFFERENT `owner/repo` (a repointed updater has
/// never judged this repository's release; an older file that recorded no source is
/// treated the same way). The next check then fetches and re-judges the head, which
/// costs unmetered requests only.
pub(crate) fn latest_tag(
    staging: &Staging,
    current_build: u64,
    source: &crate::Source,
) -> Option<String> {
    let latest = read_latest(staging)?;
    if latest.build != Some(current_build) {
        return None;
    }
    let slug = source_slug(source);
    if !latest
        .source
        .as_deref()
        .is_some_and(|recorded| recorded.eq_ignore_ascii_case(&slug))
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
/// cut a NEW tag. Best-effort, like every write to this file; a lost clear costs
/// nothing (the stale verdict is re-judged on the next moved pointer) and a lost
/// carry-forward costs one unmetered re-fetch.
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
    record_with_hold(staging, current_build, None, outcome);
}

/// [`record`] for a rate-limit deferral that knows when the budget renews: the record
/// carries `held_until = until_epoch` (RFC3339), and NOTHING else changes — no
/// `health.toml` entry, no verdict. A hold is a fact about GitHub's clock, not about
/// this machine.
pub(crate) fn record_held(staging: &Staging, current_build: u64, until_epoch: u64, outcome: &str) {
    record_with_hold(staging, current_build, Some(until_epoch), outcome);
}

fn record_with_hold(staging: &Staging, current_build: u64, held_until: Option<u64>, outcome: &str) {
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
    let (latest_tag, latest_source) = match delivery.latest {
        Some((tag, source)) => (Some(tag), Some(source)),
        None => match read_latest(staging) {
            Some(latest) => (Some(latest.tag), latest.source),
            None => (None, None),
        },
    };
    let status = Status {
        schema: 1,
        updated_at: crate::install::now_rfc3339(),
        enabled: crate::enabled(),
        current_build,
        staged_build: ready.as_ref().map(|r| r.build_number),
        staged_commit: ready.and_then(|r| r.commit),
        outcome,
        held_until: held_until.map(aterm_types::rfc3339::format_rfc3339),
        lane: delivery.lane,
        delivery: delivery.note,
        latest_tag,
        latest_source,
        budget_remaining: delivery.budget_remaining,
        budget_limit: delivery.budget_limit,
        budget_reset: delivery
            .budget_reset
            .map(aterm_types::rfc3339::format_rfc3339),
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

    /// A held record carries the epoch and the lane at schema 1, a plain record carries
    /// neither key (readers default them), and neither touches the health ledger.
    #[test]
    fn record_held_writes_held_until_and_lane_with_schema_one() {
        let _guard = crate::STRANDED_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let s = Staging::scratch("st-held");
        let root = s.root.clone();
        clear_check_note();
        set_delivery("anonymous".into(), Some(0), Some(60), Some(1_788_392_970));
        record_held(
            &s,
            42,
            1_788_392_999,
            "update check deferred: GitHub rate limit hit",
        );
        let text = std::fs::read_to_string(&s.status).unwrap();
        let v: aterm_toml::Value = aterm_toml::from_str(&text).expect("valid TOML");
        assert_eq!(
            v.get("schema").and_then(aterm_toml::Value::as_integer),
            Some(1)
        );
        assert_eq!(
            v.get("held_until").and_then(aterm_toml::Value::as_str),
            Some(aterm_types::rfc3339::format_rfc3339(1_788_392_999).as_str())
        );
        assert_eq!(
            v.get("lane").and_then(aterm_toml::Value::as_str),
            Some("anonymous")
        );
        assert_eq!(
            v.get("budget_remaining")
                .and_then(aterm_toml::Value::as_integer),
            Some(0)
        );
        assert_eq!(
            v.get("budget_limit")
                .and_then(aterm_toml::Value::as_integer),
            Some(60)
        );
        assert_eq!(
            v.get("budget_reset").and_then(aterm_toml::Value::as_str),
            Some(aterm_types::rfc3339::format_rfc3339(1_788_392_970).as_str())
        );
        assert!(
            v.get("delivery").is_none(),
            "no delivery note was set: {text}"
        );
        assert!(!s.health().exists(), "a hold never writes health.toml");
        // A plain record after the hold carries no epoch — the key is absent, not empty.
        record(&s, 42, "up to date");
        let text = std::fs::read_to_string(&s.status).unwrap();
        assert!(!text.contains("held_until"), "{text}");
        assert!(
            text.contains("lane = \"anonymous\""),
            "the lane survives the check: {text}"
        );
        // The next check starts clean.
        clear_check_note();
        record(&s, 42, "up to date");
        let text = std::fs::read_to_string(&s.status).unwrap();
        assert!(!text.contains("lane"), "{text}");
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
        set_delivery("web".into(), None, None, None);
        set_latest_tag("v0.74.0", &source);
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
        assert_eq!(
            v.get("lane").and_then(aterm_toml::Value::as_str),
            Some("web")
        );
        assert!(
            v.get("budget_remaining").is_none(),
            "the web lane measures no budget: {text}"
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
        set_latest_tag("v0.74.0", &source);
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
}
