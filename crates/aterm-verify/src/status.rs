// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Read observed check completions without attributing them to a run or checkout.
//! Existing event files have no run-start identity: two incomplete appended runs
//! can be indistinguishable from one run. This reader grants no check or land verdict.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::event_log::{CheckRow, Finished, Judged};

const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_RECORD_BYTES: usize = 1024 * 1024;

/// All outcomes observed for one check, retaining duplicates and conflicts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservedCheck {
    /// Check name from the durable event.
    pub check: String,
    /// Distinct observed outcome words; more than one means conflicting observations.
    pub outcomes: BTreeSet<String>,
    /// Number of complete, parsed check events, including identical repeats.
    pub occurrences: usize,
}

/// File observations only. Missing run/source identity is never inferred from its contents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observed {
    /// Every successfully parsed finished check, ordered by check name.
    pub checks: Vec<ObservedCheck>,
    /// Number of complete parsed terminal records in the file.
    pub terminal_records: usize,
    /// Whether one terminal list matches all observed rows; not run attribution.
    pub terminal_consistency: &'static str,
    /// Complete lines which could not establish a schema-one event of their declared kind.
    pub malformed_complete_records: usize,
    /// Structurally valid events of other kinds, deliberately not projected.
    pub unprojected_records: usize,
    /// The final line lacks its newline, so it was not consumed as a complete event.
    pub trailing_partial: bool,
}

fn outcome(word: &str) -> bool {
    matches!(word, "ok" | "skip" | "FAIL" | "NOT RUN")
}

/// Fold the entire supplied event text, never a bounded output tail.
/// A newline is required before a record is considered complete.
#[must_use]
pub fn observe(text: &str) -> Observed {
    let mut checks = BTreeMap::<String, ObservedCheck>::new();
    let mut rows = Vec::new();
    let mut terminals = Vec::new();
    let mut malformed = 0;
    let mut unprojected = 0;
    let mut trailing_partial = false;
    for line in text.split_inclusive('\n') {
        let Some(line) = line.strip_suffix('\n') else {
            trailing_partial = true;
            break;
        };
        let line = line.strip_suffix('\r').unwrap_or(line);
        let value = line
            .strip_prefix("verify-event: ")
            .filter(|json| json.len() <= MAX_RECORD_BYTES)
            .and_then(|json| aterm_json::from_str::<aterm_json::Value>(json).ok());
        let Some(value) = value.filter(|value| value["schema"].as_u64() == Some(1)) else {
            malformed += 1;
            continue;
        };
        match value["event"].as_str() {
            Some("check-finished") => {
                let Some(row) = CheckRow::of_event(line)
                    .filter(|row| !row.check.is_empty() && outcome(&row.word))
                else {
                    malformed += 1;
                    continue;
                };
                let observed = checks
                    .entry(row.check.clone())
                    .or_insert_with(|| ObservedCheck {
                        check: row.check.clone(),
                        outcomes: BTreeSet::new(),
                        occurrences: 0,
                    });
                observed.outcomes.insert(row.word.clone());
                observed.occurrences += 1;
                rows.push(row);
            }
            Some("run-finished") => {
                let Some(finished) = Finished::of_event(line).filter(|finished| {
                    finished
                        .checks
                        .iter()
                        .all(|(check, word)| !check.is_empty() && outcome(word))
                }) else {
                    malformed += 1;
                    continue;
                };
                terminals.push(finished);
            }
            Some(_) => unprojected += 1,
            None => malformed += 1,
        }
    }
    let terminal_consistency = match terminals.as_slice() {
        [] => "not-observed",
        [_] if malformed != 0 || trailing_partial => "unavailable",
        [finished] => {
            let record = Judged {
                code: finished.code,
                checks: rows,
                secs: 0,
                finished: Some(finished.clone()),
            };
            if record.whole() {
                "matching-observed-rows"
            } else {
                "mismatch"
            }
        }
        _ => "multiple-terminal-records",
    };
    Observed {
        checks: checks.into_values().collect(),
        terminal_records: terminals.len(),
        terminal_consistency,
        malformed_complete_records: malformed,
        unprojected_records: unprojected,
        trailing_partial,
    }
}

impl Observed {
    /// Machine-readable observations, omitting failure bodies, paths and rerun arguments.
    #[must_use]
    pub fn json(&self) -> aterm_json::Value {
        aterm_json::json!({
            "schema": 1, "kind": "observed-check-completions",
            "run_identity": null, "source_identity": null,
            "appended_incomplete_runs": "indistinguishable",
            "checks": self.checks.iter().map(|check| aterm_json::json!({
                "check": check.check, "outcomes": check.outcomes.iter().map(String::as_str).collect::<Vec<_>>(),
                "occurrences": check.occurrences, "conflict": check.outcomes.len() > 1,
            })).collect::<Vec<_>>(),
            "terminal_records": self.terminal_records,
            "terminal_consistency": self.terminal_consistency,
            "malformed_complete_records": self.malformed_complete_records,
            "unprojected_records": self.unprojected_records,
            "trailing_partial": self.trailing_partial,
        })
    }

    fn render(&self) -> String {
        let mut out = String::from(
            "Observed check completions; run and source identity unrecorded.\nIncomplete appended runs cannot be distinguished.\n",
        );
        for check in &self.checks {
            // JSON quoting prevents event data from injecting terminal controls or extra rows.
            out.push_str(&format!(
                "{}: {} ({} observations{})\n",
                aterm_json::Value::String(check.check.clone()),
                check
                    .outcomes
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", "),
                check.occurrences,
                if check.outcomes.len() > 1 {
                    "; conflict"
                } else {
                    ""
                }
            ));
        }
        out.push_str(&format!("Terminal records: {}; consistency: {}.\nMalformed complete records: {}; unprojected events: {}; trailing partial: {}.\n",
            self.terminal_records, self.terminal_consistency, self.malformed_complete_records,
            self.unprojected_records, self.trailing_partial));
        out
    }
}

fn arguments(args: &[String]) -> Option<(PathBuf, bool)> {
    match args {
        [flag, path] if flag == "--events" => Some((PathBuf::from(path), false)),
        [flag, path, json] if flag == "--events" && json == "--json" => {
            Some((PathBuf::from(path), true))
        }
        [json, flag, path] if json == "--json" && flag == "--events" => {
            Some((PathBuf::from(path), true))
        }
        _ => None,
    }
}

/// Inspect an explicit event file without project discovery, execution, or admission.
/// Exit zero means the file was inspected, regardless of the checks' observed outcomes.
#[must_use]
pub fn run(args: &[String], cwd: &Path) -> i32 {
    const USAGE: &str = "usage: verify status --events <path> [--json]\nRead observed finished checks only; no run/source or landing verdict.\nExit: 0 inspected, 2 usage, 3 file unavailable.\n";
    if matches!(args, [help] if help == "--help" || help == "-h") {
        print!("{USAGE}");
        return crate::exit::PASS;
    }
    let Some((path, json)) = arguments(args) else {
        eprint!("{USAGE}");
        return crate::exit::USAGE;
    };
    let text = match aterm_uds::atomic_file::read_regular_text(&cwd.join(path), MAX_FILE_BYTES) {
        Ok(text) => text,
        Err(error) => {
            eprintln!("verify status: event file unavailable ({})", error.kind());
            return crate::exit::COULD_NOT_RUN;
        }
    };
    let observed = observe(&text);
    if json {
        println!("{}", observed.json());
    } else {
        print!("{}", observed.render());
    }
    crate::exit::PASS
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Check, Outcome};

    fn result(check: Check, value: Outcome) -> crate::report::CheckResult {
        crate::report::CheckResult {
            check,
            outcome: value,
            seconds: 1.0,
            detail: "private failure body".into(),
            findings: Vec::new(),
            rerun: "private rerun arguments".into(),
            stopped: false,
        }
    }

    fn event(check: Check) -> String {
        crate::report::check_event(&result(check, Outcome::Ok), &[])
    }

    #[test]
    fn full_durable_file_keeps_earlier_checks_despite_later_output_volume() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join("verify.events");
        let log = crate::event_log::EventLog::open(&path).unwrap();
        log.write(&event(Check::Doc));
        log.write(&event(Check::Lint));
        for index in 0..1100 {
            log.write(&crate::report::stage_started(index, &"stage".repeat(1000)));
        }
        log.write(&event(Check::Test));
        let text = std::fs::read_to_string(&path).unwrap();
        let observed = observe(&text);
        assert_eq!(
            observed
                .checks
                .iter()
                .map(|row| row.check.as_str())
                .collect::<Vec<_>>(),
            ["doc", "lint", "test"]
        );
        let mut tail = text.lines().rev().take(1000).collect::<Vec<_>>();
        tail.reverse();
        let tail = observe(&(tail.join("\n") + "\n"));
        assert_eq!(
            tail.checks
                .iter()
                .map(|row| row.check.as_str())
                .collect::<Vec<_>>(),
            ["test"]
        );
        let serialized = observed.json().to_string();
        assert!(!serialized.contains("private failure") && !serialized.contains("private rerun"));
    }

    #[test]
    fn incomplete_appended_runs_are_indistinguishable_from_one_run() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let one = dir.path().join("one.events");
        let two = dir.path().join("two.events");
        let single = crate::event_log::EventLog::open(&one).unwrap();
        single.write(&event(Check::Doc));
        single.write(&event(Check::Lint));
        // The real writer permits separate incomplete invocations to append the same file.
        crate::event_log::EventLog::open(&two)
            .unwrap()
            .write(&event(Check::Doc));
        crate::event_log::EventLog::open(&two)
            .unwrap()
            .write(&event(Check::Lint));
        let one_incomplete_run = std::fs::read_to_string(one).unwrap();
        let separate_incomplete_runs = std::fs::read_to_string(two).unwrap();
        assert_eq!(one_incomplete_run, separate_incomplete_runs);
        let observed = observe(&one_incomplete_run);
        assert_eq!(observed, observe(&separate_incomplete_runs));
        assert_eq!(observed.terminal_consistency, "not-observed");
        assert_eq!(observed.json()["run_identity"], aterm_json::Value::Null);
        assert_eq!(observed.json()["source_identity"], aterm_json::Value::Null);
    }

    #[test]
    fn duplicates_conflicts_and_terminal_lists_do_not_become_latest_run_verdicts() {
        let doc = result(Check::Doc, Outcome::Ok);
        let complete = format!(
            "{}{}",
            event(Check::Doc),
            crate::report::run_event(&[doc], 0)
        );
        assert_eq!(
            observe(&complete).terminal_consistency,
            "matching-observed-rows"
        );
        let twice = observe(&format!("{complete}{complete}"));
        assert_eq!(twice.terminal_consistency, "multiple-terminal-records");
        assert_eq!(twice.checks[0].occurrences, 2);
        let failed = crate::report::check_event(
            &result(Check::Doc, Outcome::Fail(crate::Severity::GateFailed)),
            &[],
        );
        let conflict = observe(&format!("{complete}{failed}"));
        assert_eq!(conflict.checks[0].outcomes.len(), 2);
        assert_eq!(conflict.terminal_consistency, "mismatch");
    }

    #[test]
    fn malformed_complete_and_partial_records_are_distinct_and_never_filled_in() {
        let partial = observe(&format!("{}verify-event: {{", event(Check::Doc)));
        assert!(partial.trailing_partial);
        assert_eq!(partial.malformed_complete_records, 0);
        let invalid = observe(
            "verify-event: {\nverify-event: {\"schema\":2,\"event\":\"check-finished\"}\nverify-event: {\"schema\":1,\"event\":\"check-finished\"}\n",
        );
        assert_eq!(invalid.malformed_complete_records, 3);
        assert!(invalid.checks.is_empty());
    }

    #[test]
    fn explicit_file_reader_needs_no_checkout_or_toolchain_and_never_runs_checks() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join("observations.events");
        std::fs::write(&path, event(Check::Doc)).unwrap();
        assert_eq!(
            crate::run_cli(
                &[
                    "status".into(),
                    "--events".into(),
                    path.display().to_string(),
                    "--json".into(),
                ],
                dir.path()
            ),
            crate::exit::PASS
        );
        assert_eq!(
            crate::run_cli(
                &["status".into(), "--events".into(), "absent.events".into(),],
                dir.path()
            ),
            crate::exit::COULD_NOT_RUN
        );
    }
}
