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
    let text = crate::read_ledger_text(&staging.status)?;
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
    let Some(text) = crate::read_ledger_text(&staging.status) else {
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
    if crate::install::write_private_file(&tmp, text.as_bytes()).is_err()
        || std::fs::rename(&tmp, &staging.status).is_err()
    {
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
    record_with(staging, current_build, outcome, None);
}

/// [`record`] with `note` as its delivery note in place of the one this check set
/// ([`set_delivery_note`]): a mark of this one record alone, which no later record of this process
/// carries, as a strand's ([`crate::unreadable::NOTE`]) is.
pub(crate) fn record_noted(staging: &Staging, current_build: u64, outcome: &str, note: &str) {
    record_with(staging, current_build, outcome, Some(note));
}

/// [`record`], its delivery note `note` where one is given, else this check's.
fn record_with(staging: &Staging, current_build: u64, outcome: &str, note: Option<&str>) {
    let ready = crate::manifest::Ready::read_publishable(staging);
    let noted;
    let check = check_note();
    let outcome = match check.as_deref() {
        Some(check) if !outcome.contains(check) => {
            noted = format!("{outcome} · {check}");
            noted.as_str()
        }
        _ => outcome,
    };
    let mut delivery = with_delivery(|delivery| delivery.clone());
    if let Some(note) = note {
        delivery.note = Some(note.to_owned());
    }
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
    if crate::install::write_private_file(&tmp, text.as_bytes()).is_ok()
        && std::fs::rename(&tmp, &staging.status).is_ok()
    {
        return;
    }
    let _ = std::fs::remove_file(&tmp);
}

#[cfg(test)]
#[path = "status_tests.rs"]
mod tests;
