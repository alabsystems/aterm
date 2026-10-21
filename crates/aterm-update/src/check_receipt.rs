// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The receipt of completed network checks. Apply and status writers never refresh its
//! clock or deferral, and another source cannot reuse it.
//!
//! It also counts, machine-wide, how many completed checks in a row have found the
//! channel head's app build in flight (a release published source-first:
//! `github::app_build_in_flight`) — the count the check loop's quick re-checks
//! (`cadence::IN_FLIGHT_RETRY`, for `cadence::IN_FLIGHT_CHECKS` of them) and every
//! sibling's dedup window (`checker_skip_for`) key on, so whichever process checks
//! next, the machine walks one ladder.

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
    /// The channel head this check found with its app build in flight, and how many
    /// consecutive completed checks of this source found it so. Not keyed on the build:
    /// after an update the old build's sessions and the new build's window both check,
    /// and a per-build count would keep both on the first rung.
    #[serde(skip_serializing_if = "Option::is_none")]
    head_in_flight: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    head_in_flight_checks: Option<u32>,
}

/// Stamp a completed check. Returns how many consecutive completed checks, this one
/// included, found `head_in_flight` in flight (`0` when `None` or when the write failed:
/// a count nothing records is no count).
pub(crate) fn record(
    staging: &Staging,
    current_build: u64,
    source: &Source,
    deferred: bool,
    head_in_flight: Option<&str>,
) -> u32 {
    record_at(
        staging,
        current_build,
        source,
        deferred,
        head_in_flight,
        crate::unix_now_secs(),
    )
}

fn record_at(
    staging: &Staging,
    current_build: u64,
    source: &Source,
    deferred: bool,
    head_in_flight: Option<&str>,
    now: u64,
) -> u32 {
    // The run continues only across receipts of this source, any build, naming the same
    // head; a completed check that found none ends it. A failed check writes no receipt.
    let checks = head_in_flight.map(|head| {
        crate::read_ledger_text(&path(staging))
            .and_then(|text| text.parse::<aterm_toml::Value>().ok())
            .filter(|prior| {
                same_source(prior, source)
                    && prior
                        .get("head_in_flight")
                        .and_then(aterm_toml::Value::as_str)
                        == Some(head)
            })
            .map_or(0, |prior| head_in_flight_checks(&prior))
            .saturating_add(1)
    });
    let record = Receipt {
        schema: 1,
        updated_at: aterm_types::rfc3339::format_rfc3339(now),
        current_build,
        source: source_key(source),
        outcome: if deferred { "deferred" } else { "completed" },
        head_in_flight: head_in_flight.map(str::to_owned),
        head_in_flight_checks: checks,
    };
    let Ok(text) = aterm_toml::to_string(&record) else {
        return 0;
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let tmp = staging.root.join(format!(
        "check.{}.{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    if std::fs::write(&tmp, text).is_ok() && std::fs::rename(&tmp, path(staging)).is_ok() {
        return checks.unwrap_or(0);
    }
    let _ = std::fs::remove_file(tmp);
    0
}

/// The `head_in_flight_checks` a parsed receipt records, `0` when it records none.
pub(crate) fn head_in_flight_checks(receipt: &aterm_toml::Value) -> u32 {
    receipt
        .get("head_in_flight_checks")
        .and_then(aterm_toml::Value::as_integer)
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(0)
}

pub(crate) fn matches(text: &aterm_toml::Value, current_build: u64, source: &Source) -> bool {
    same_source(text, source)
        && text
            .get("current_build")
            .and_then(aterm_toml::Value::as_integer)
            .and_then(|build| u64::try_from(build).ok())
            == Some(current_build)
}

/// A schema-1 receipt of `source`, whatever build wrote it.
fn same_source(text: &aterm_toml::Value, source: &Source) -> bool {
    text.get("schema").and_then(aterm_toml::Value::as_integer) == Some(1)
        && text
            .get("source")
            .and_then(aterm_toml::Value::as_str)
            .is_some_and(|recorded| recorded.eq_ignore_ascii_case(&source_key(source)))
}

/// For display only; authorization uses `matches` and the receipt.
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
        let model = aterm_models_update::native_update_check_receipt_model();
        let now = crate::unix_now_secs();
        let base = Duration::from_secs(3); // 70% freshness = two seconds.
        for age in 0..=2 {
            for same_source in [false, true] {
                for same_build in [false, true] {
                    record_at(&staging, 42, &source("one"), false, None, now - age);
                    let bytes = std::fs::read(path(&staging)).unwrap();
                    // The apply lane's real status writer must not touch the receipt.
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
        let _ = std::fs::remove_dir_all(&staging.root);
    }

    /// The dedup window may not outlast the quick wait. At the shipped two-minute base
    /// a completed check 60 s ago skips every checker (the 84 s window). The same
    /// receipt carrying a head in flight sizes the window on the one-minute wait (42 s,
    /// over at 60 s; the negative control: sized on the base, the checker's own
    /// one-minute re-check would read its own stamp as a sibling's and wait out the
    /// window), for every count up to `cadence::IN_FLIGHT_CHECKS`; once those are
    /// spent, or on a deferral, the ordinary window stands.
    #[test]
    fn a_head_in_flight_shrinks_the_dedup_window_to_its_quick_rung() {
        let _guard = crate::STRANDED_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        crate::status::clear_check_note();
        let staging = Staging::scratch("check-receipt-in-flight");
        let source = source("one");
        let base = Duration::from_secs(crate::cadence::INTERVAL_SECS);
        let now = crate::unix_now_secs();
        let skip = || crate::checker_skip_for(&staging, 42, &source, base, now);

        record_at(&staging, 42, &source, false, None, now - 60);
        assert!(skip().is_some(), "no head in flight: the base window skips");

        let head = Some("v0.92.0");
        assert_eq!(record_at(&staging, 42, &source, false, None, now - 60), 0);
        assert_eq!(record_at(&staging, 42, &source, false, head, now - 60), 1);
        assert!(skip().is_none(), "check 1: a 42 s window is over at 60 s");
        assert_eq!(record_at(&staging, 42, &source, false, head, now - 30), 2);
        assert_eq!(
            skip().map(|(_, until)| until),
            Some(now - 30 + 42),
            "check 2: the 42 s window still holds at 30 s"
        );
        let spent = crate::cadence::IN_FLIGHT_CHECKS;
        let mut count = 2;
        while count < spent {
            count = record_at(&staging, 42, &source, false, head, now - 60);
        }
        assert!(skip().is_none(), "check {count}: still the 42 s window");
        assert_eq!(
            record_at(&staging, 42, &source, false, head, now - 60),
            spent + 1
        );
        assert!(
            skip().is_some(),
            "the quick waits are spent: the base window again"
        );

        // A deferral's widened window is never shortened by a head in flight.
        std::fs::write(
            path(&staging),
            format!(
                "schema = 1\nupdated_at = \"{}\"\ncurrent_build = 42\nsource = \"one/channel\"\n\
                 outcome = \"deferred\"\nhead_in_flight = \"v0.92.0\"\nhead_in_flight_checks = 1\n",
                aterm_types::rfc3339::format_rfc3339(now - 100)
            ),
        )
        .unwrap();
        assert!(skip().is_some(), "a deferral holds its own window");
        let _ = std::fs::remove_dir_all(&staging.root);
    }

    /// The count is the source's and the
    /// head's: interleaved checks of builds 42 and 41 count 1, 2, 3, …, and the one
    /// past `cadence::IN_FLIGHT_CHECKS` has spent the quick waits. Negative controls: a
    /// different source, a different head, and a completed check that found none each
    /// restart the run.
    #[test]
    fn two_builds_checking_one_head_in_flight_share_one_ladder() {
        let _guard = crate::STRANDED_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let staging = Staging::scratch("check-receipt-two-builds");
        let now = crate::unix_now_secs();
        let head = Some("v0.93.0");
        let spent = crate::cadence::IN_FLIGHT_CHECKS;
        let counts: Vec<u32> = [42, 41]
            .into_iter()
            .cycle()
            .take(spent as usize + 1)
            .map(|build| record_at(&staging, build, &source("one"), false, head, now))
            .collect();
        assert_eq!(counts, (1..=spent + 1).collect::<Vec<_>>());
        assert!(crate::cadence::in_flight_retry(spent).is_some());
        assert!(
            crate::cadence::in_flight_retry(spent + 1).is_none(),
            "the quick waits are spent"
        );
        assert_eq!(
            record_at(&staging, 41, &source("two"), false, head, now),
            1,
            "another source"
        );
        assert_eq!(
            record_at(&staging, 42, &source("two"), false, Some("v0.94.0"), now),
            1,
            "another head"
        );
        assert_eq!(record_at(&staging, 42, &source("two"), false, None, now), 0);
        assert_eq!(
            record_at(&staging, 41, &source("two"), false, Some("v0.94.0"), now),
            1,
            "a check that found none ended the run"
        );
        let _ = std::fs::remove_dir_all(&staging.root);
    }
}
