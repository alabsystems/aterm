// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Completed network checks have their own receipt. Apply/status writers never
//! refresh this clock or erase a server hold, and another source cannot reuse it.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;

use crate::{Source, paths::Staging};

pub(crate) fn path(staging: &Staging) -> PathBuf {
    staging.root.join("check.toml")
}

pub(crate) fn source_key(source: &Source) -> String {
    format!("{}/{}", source.owner, source.repo)
}

#[derive(Serialize)]
struct Receipt {
    schema: u32,
    updated_at: String,
    current_build: u64,
    source: String,
    outcome: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    held_until: Option<String>,
}

pub(crate) fn record(
    staging: &Staging,
    current_build: u64,
    source: &Source,
    deferred: bool,
    held_until: Option<u64>,
) {
    record_at(
        staging,
        current_build,
        source,
        deferred,
        held_until,
        crate::unix_now_secs(),
    );
}

fn record_at(
    staging: &Staging,
    current_build: u64,
    source: &Source,
    deferred: bool,
    held_until: Option<u64>,
    now: u64,
) {
    let record = Receipt {
        schema: 1,
        updated_at: aterm_types::rfc3339::format_rfc3339(now),
        current_build,
        source: source_key(source),
        outcome: if deferred { "deferred" } else { "completed" },
        held_until: held_until.map(aterm_types::rfc3339::format_rfc3339),
    };
    let Ok(text) = aterm_toml::to_string(&record) else {
        return;
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let tmp = staging.root.join(format!(
        "check.{}.{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    if std::fs::write(&tmp, text).is_ok() && std::fs::rename(&tmp, path(staging)).is_ok() {
        return;
    }
    let _ = std::fs::remove_file(tmp);
}

pub(crate) fn matches(text: &aterm_toml::Value, current_build: u64, source: &Source) -> bool {
    text.get("schema").and_then(aterm_toml::Value::as_integer) == Some(1)
        && text
            .get("current_build")
            .and_then(aterm_toml::Value::as_integer)
            .and_then(|build| u64::try_from(build).ok())
            == Some(current_build)
        && text
            .get("source")
            .and_then(aterm_toml::Value::as_str)
            .is_some_and(|recorded| recorded.eq_ignore_ascii_case(&source_key(source)))
}

/// Operator-facing projection only; authorization uses `matches` plus the receipt.
pub(crate) fn completed_at(staging: &Staging) -> Option<String> {
    let text = crate::read_ledger_text(&path(staging))?;
    let value: aterm_toml::Value = text.parse().ok()?;
    value
        .get("updated_at")
        .and_then(aterm_toml::Value::as_str)
        .filter(|stamp| !stamp.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn source(owner: &str) -> Source {
        Source {
            owner: owner.into(),
            repo: "channel".into(),
        }
    }

    #[test]
    fn only_a_completed_check_for_this_source_and_build_can_defer_discovery() {
        let _guard = crate::STRANDED_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        crate::status::clear_check_note();
        let staging = Staging::scratch("check-receipt-provenance");
        let model = aterm_spec::derive::native_update_check_receipt_model();
        let now = crate::unix_now_secs();
        let base = Duration::from_secs(3); // 70% freshness = two seconds.
        for age in 0..=2 {
            for same_source in [false, true] {
                for same_build in [false, true] {
                    record_at(&staging, 42, &source("one"), false, None, now - age);
                    let bytes = std::fs::read(path(&staging)).unwrap();
                    // This is the genuine writer used during AND after a check by
                    // the apply lane. It must not acquire the check's provenance.
                    crate::status::record(
                        &staging,
                        42,
                        "staged build did not apply: terminal busy",
                    );
                    assert_eq!(std::fs::read(path(&staging)).unwrap(), bytes);
                    let mut state = model.init_state();
                    state.insert("actual_age", i64::try_from(age).unwrap());
                    state.insert("stamped_age", i64::try_from(age).unwrap());
                    state.insert("same_source", i64::from(same_source));
                    state.insert("same_build", i64::from(same_build));
                    state.insert("written", 1);
                    let skipped = crate::checker_skip_for(
                        &staging,
                        if same_build { 42 } else { 43 },
                        &source(if same_source { "ONE" } else { "two" }),
                        base,
                        now,
                    )
                    .is_some();
                    assert_eq!(skipped, model.action_enabled("Skip", &state), "{state:?}");
                    if age == 2 && same_source && same_build {
                        let generic: aterm_toml::Value = std::fs::read_to_string(&staging.status)
                            .unwrap()
                            .parse()
                            .unwrap();
                        let generic_stamp = generic.get("updated_at").unwrap().as_str().unwrap();
                        assert!(
                            !crate::rfc3339_older_than(generic_stamp, 2),
                            "negative control: the historical any-writer timestamp is fresh"
                        );
                        state.insert("skipped", 1);
                        assert!(!model.check_invariant("OnlyCompletedCheckDefers", &state));
                    }
                }
            }
        }
        let _ = std::fs::remove_dir_all(staging.root);
    }

    #[test]
    fn apply_writes_cannot_clear_a_completed_checks_server_hold() {
        let _guard = crate::STRANDED_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        crate::status::clear_check_note();
        let staging = Staging::scratch("check-receipt-hold");
        let source = source("one");
        let now = crate::unix_now_secs();
        record_at(&staging, 42, &source, true, Some(now + 600), now);
        crate::status::record(&staging, 42, "update apply refused: live terminals");
        let (_, timer) =
            crate::checker_skip_for(&staging, 42, &source, Duration::from_secs(1800), now).unwrap();
        assert_eq!(timer.held, Some(now + 600));
        assert!(
            crate::checker_skip_for(&staging, 42, &source, Duration::from_secs(1800), now + 600)
                .is_none()
        );
        // Receipt failures do not wedge: malformed/legacy records authorize no skip.
        std::fs::write(
            path(&staging),
            "schema = 1\nupdated_at = \"2026-09-14T00:00:00Z\"\n",
        )
        .unwrap();
        assert!(
            crate::checker_skip_for(&staging, 42, &source, Duration::from_secs(1800), now)
                .is_none()
        );
        let _ = std::fs::remove_dir_all(staging.root);
    }
}
