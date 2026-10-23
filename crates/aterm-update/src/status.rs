// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Human-readable updater status record (`status.toml`).

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;

use crate::paths::Staging;

#[derive(Serialize)]
struct Status<'a> {
    schema: u32,
    updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    checked_at: Option<String>,
    enabled: bool,
    current_build: u64,
    staged_build: Option<u64>,
    staged_commit: Option<String>,
    outcome: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    delivery: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    latest_tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    latest_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    latest_authorized_build: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    latest_release_build: Option<u64>,
}

#[derive(Clone, Default)]
struct Delivery {
    note: Option<String>,
    latest: Option<LatestRecord>,
}

#[cfg(not(test))]
static DELIVERY: std::sync::Mutex<Delivery> = std::sync::Mutex::new(Delivery {
    note: None,
    latest: None,
});

#[cfg(test)]
thread_local! {
    static DELIVERY: std::cell::RefCell<Delivery> = const {
        std::cell::RefCell::new(Delivery {
            note: None,
            latest: None,
        })
    };
}

#[cfg(not(test))]
fn with_delivery<R>(f: impl FnOnce(&mut Delivery) -> R) -> R {
    f(&mut DELIVERY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner))
}

#[cfg(test)]
fn with_delivery<R>(f: impl FnOnce(&mut Delivery) -> R) -> R {
    DELIVERY.with(|slot| f(&mut slot.borrow_mut()))
}

/// Record how this check ended short of a healthy outcome (`deferred` / `blocked`).
pub(crate) fn set_delivery_note(note: &str) {
    with_delivery(|delivery| delivery.note = Some(note.to_string()));
}

/// Record the release tag this check authorized end to end against `source`.
pub(crate) fn set_latest_tag(
    tag: &str,
    source: &crate::Source,
    current_build: u64,
    release_build: u64,
) {
    let latest = LatestRecord {
        tag: tag.to_string(),
        source: source_slug(source),
        build: current_build,
        release_build,
    };
    with_delivery(|delivery| delivery.latest = Some(latest));
}

fn source_slug(source: &crate::Source) -> String {
    let mut slug = source.owner.clone();
    slug.push('/');
    slug.push_str(&source.repo);
    slug
}

#[derive(Clone)]
struct LatestRecord {
    tag: String,
    source: String,
    build: u64,
    release_build: u64,
}

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

/// The tag the ledger last authorized for this build and source.
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

/// The tag the ledger last authorized against `source`, whichever build recorded it.
pub(crate) fn authorized_tag(staging: &Staging, source: &crate::Source) -> Option<String> {
    read_latest(staging)
        .filter(|latest| latest.source.eq_ignore_ascii_case(&source_slug(source)))
        .map(|latest| latest.tag)
}

/// Forget the authorized tag, on disk and for the rest of this check, so the next check re-judges
/// the channel head.
pub(crate) fn clear_latest_tag(staging: &Staging) {
    with_delivery(|delivery| delivery.latest = None);
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

fn clear_delivery() {
    with_delivery(|delivery| *delivery = Delivery::default());
}

fn temp_path(staging: &Staging) -> PathBuf {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    staging
        .root
        .join(format!("status.toml.{}.{sequence}.tmp", std::process::id()))
}

#[cfg(not(test))]
static CHECK_NOTE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

#[cfg(test)]
thread_local! {
    static CHECK_NOTE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

#[cfg(not(test))]
fn store_check_note(note: Option<String>) {
    *CHECK_NOTE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = note;
}

#[cfg(test)]
fn store_check_note(note: Option<String>) {
    CHECK_NOTE.with(|slot| *slot.borrow_mut() = note);
}

#[cfg(not(test))]
fn check_note() -> Option<String> {
    CHECK_NOTE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

#[cfg(test)]
fn check_note() -> Option<String> {
    CHECK_NOTE.with(|slot| slot.borrow().clone())
}

/// Attach `note` to every status record until [`clear_check_note`].
pub(crate) fn set_check_note(note: String) {
    store_check_note(Some(note));
}

/// Forget the current check note and the delivery facts (the next check starts clean).
pub(crate) fn clear_check_note() {
    store_check_note(None);
    clear_delivery();
}

/// Atomically write the status record (temp + rename).
pub fn record(staging: &Staging, current_build: u64, outcome: &str) {
    let ready = crate::manifest::Ready::read_publishable(staging);
    let noted;
    let note = check_note();
    let outcome = match note.as_deref() {
        Some(note) if !outcome.contains(note) => {
            noted = format!("{outcome} · {note}");
            noted.as_str()
        }
        _ => outcome,
    };
    let delivery = with_delivery(|delivery| delivery.clone());
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
    let _ = std::fs::remove_file(&tmp);
}

#[cfg(test)]
mod tests {
    use super::*;

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
            roster_confirmed_at: Some(crate::install::now_rfc3339()),
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
        record(&s, 42, "held: a later record that authorized nothing");
        assert_eq!(latest_tag(&s, 42, &source).as_deref(), Some("v0.74.0"));
        let text = std::fs::read_to_string(&s.status).unwrap();
        assert!(text.contains("latest_tag = \"v0.74.0\""), "{text}");
        assert!(
            text.contains("latest_source = \"alabsystems/aterm\""),
            "the source is carried forward with the tag: {text}"
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
            roster_confirmed_at: Some(crate::install::now_rfc3339()),
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
        let model = aterm_models_update::native_update_web_cache_model();
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

        set_latest_tag("v0.85.0", &source, 1, 1);
        record(&staging, 1, "up to date");
        std::fs::remove_file(&staging.ready).unwrap();
        assert!(cache_decision_matches_model(&staging, 1, &source));
        clear_check_note();
        let _ = std::fs::remove_dir_all(&staging.root);
    }
}
