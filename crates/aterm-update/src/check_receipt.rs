// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Completed network checks have their own receipt. Apply/status writers never
//! refresh this clock or its deferral, and another source cannot reuse it.
//!
//! It also counts, machine-wide, how many completed checks in a row have found the
//! channel head's APP BUILD IN FLIGHT (a release published source-first:
//! `github::app_build_in_flight`) — the count the check loop's quick re-check ladder
//! (`cadence::IN_FLIGHT_RETRY`) and every sibling's dedup window
//! (`checker_skip_for`) key on, so whichever process checks next, the machine walks
//! one ladder.

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
    /// The channel head whose app build this check found IN FLIGHT (a release
    /// published source-first, above the last tag the machine authorized —
    /// `github::app_build_in_flight`), and how many consecutive completed checks of
    /// this source, machine-wide and WHATEVER BUILD made them, have found that same
    /// head so. Absent otherwise. Not keyed on the build: after an update the old
    /// build's session processes and the new build's window both check, and a count
    /// each reset to 1 on the other's receipt kept both on the 2-minute rung for as
    /// long as the head stayed in flight (audit F5, 2026-09-24).
    #[serde(skip_serializing_if = "Option::is_none")]
    head_in_flight: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    head_in_flight_checks: Option<u32>,
}

/// Stamp a completed check. Returns how many consecutive completed checks — this one
/// included — have found `head_in_flight` in flight (`0` when it is `None`, or when the
/// receipt could not be written: a count nothing records is no count).
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
    // The run continues only across receipts of this source, of any build, naming
    // the SAME head: a different head is a different release in flight, and any
    // completed check that found none ended the run. A failed check writes no
    // receipt, so it neither counts nor ends the run.
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

    /// A deferral recorded by a completed check survives every later apply/status
    /// write, and widens every sibling's window; a malformed or legacy record
    /// authorizes no skip.
    #[test]
    fn apply_writes_cannot_clear_a_completed_checks_deferral() {
        let _guard = crate::STRANDED_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        crate::status::clear_check_note();
        let staging = Staging::scratch("check-receipt-deferral");
        let source = source("one");
        let now = crate::unix_now_secs();
        let base = Duration::from_secs(1800);
        // 25 minutes old: past the base window (21 min), inside the widened one (42).
        record_at(&staging, 42, &source, true, None, now - 25 * 60);
        crate::status::record(&staging, 42, "update apply refused: live terminals");
        let (reason, _) = crate::checker_skip_for(&staging, 42, &source, base, now).unwrap();
        assert!(reason.contains("deferred"), "{reason}");
        // Negative control: the same age as a completed check skips nothing.
        record_at(&staging, 42, &source, false, None, now - 25 * 60);
        assert!(crate::checker_skip_for(&staging, 42, &source, base, now).is_none());
        // Receipt failures do not wedge: malformed/legacy records authorize no skip.
        std::fs::write(
            path(&staging),
            "schema = 1\nupdated_at = \"2026-09-14T00:00:00Z\"\n",
        )
        .unwrap();
        assert!(crate::checker_skip_for(&staging, 42, &source, base, now).is_none());
        let _ = std::fs::remove_dir_all(staging.root);
    }

    /// THE DEDUP WINDOW MAY NOT OUTLAST THE QUICK LADDER. A completed check 100 s
    /// ago skips every checker on the 10-minute base (the 7-minute window — the
    /// negative control: sized on the base, the checker's own 2-minute re-check read
    /// its own stamp as a sibling's and waited out the window). The same receipt
    /// carrying a head in flight sizes the window on the rung its count names (84 s
    /// for the 2-minute rung, 168 s for the 4-minute one, 336 s for the 8-minute
    /// one), and once the rungs are spent, or on a deferral, the ordinary window
    /// stands.
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

        record_at(&staging, 42, &source, false, None, now - 100);
        assert!(skip().is_some(), "no head in flight: the base window skips");

        let head = Some("v0.92.0");
        assert_eq!(record_at(&staging, 42, &source, false, None, now - 100), 0);
        assert_eq!(record_at(&staging, 42, &source, false, head, now - 100), 1);
        assert!(skip().is_none(), "rung 1: an 84 s window is over at 100 s");
        assert_eq!(record_at(&staging, 42, &source, false, head, now - 100), 2);
        assert_eq!(
            skip().map(|(_, until)| until),
            Some(now - 100 + 168),
            "rung 2: the 168 s window still holds at 100 s"
        );
        assert_eq!(record_at(&staging, 42, &source, false, head, now - 400), 3);
        assert!(skip().is_none(), "rung 3: a 336 s window is over at 400 s");
        assert_eq!(record_at(&staging, 42, &source, false, head, now - 400), 4);
        assert!(
            skip().is_some(),
            "the rungs are spent: the base window again"
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
        let _ = std::fs::remove_dir_all(staging.root);
    }

    /// TWO BUILDS CHECKING WALK ONE LADDER (audit F5, 2026-09-24). After an update
    /// the old build's session processes and the new build's window both check;
    /// keyed on the build, each reset the other's count to 1, so both stayed on the
    /// 2-minute rung all night. The count is the source's and the head's: the
    /// interleaved checks of builds 42 and 41 count 1, 2, 3, 4, and the fourth has
    /// spent the quick rungs. Negative controls: a different source's receipt, a
    /// different head, and a completed check that found none each start the run
    /// over.
    #[test]
    fn two_builds_checking_one_head_in_flight_share_one_ladder() {
        let _guard = crate::STRANDED_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let staging = Staging::scratch("check-receipt-two-builds");
        let now = crate::unix_now_secs();
        let head = Some("v0.93.0");
        let counts: Vec<u32> = [42, 41, 42, 41]
            .into_iter()
            .map(|build| record_at(&staging, build, &source("one"), false, head, now))
            .collect();
        assert_eq!(counts, [1, 2, 3, 4]);
        assert!(crate::cadence::in_flight_retry(3).is_some());
        assert!(
            crate::cadence::in_flight_retry(4).is_none(),
            "the quick rungs are spent"
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
        let _ = std::fs::remove_dir_all(staging.root);
    }
}
