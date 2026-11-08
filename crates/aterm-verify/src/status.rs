// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Read observed check completions, partitioning exact per-event invocation identities.
//! Legacy records remain unattributed; captured source facts never prove the checkout.
//! This reader grants no check, resource-retirement or land verdict.

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

/// One diagnostic invocation's observed rows, not a qualification or checkout proof.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invocation {
    pub id: String,
    pub source_observation: Option<String>,
    pub source_conflict: bool,
    pub observed: Box<Observed>,
}

/// File observations only. Missing run/source identity is never inferred from its contents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observed {
    /// Exact per-event invocation partitions. Metadata conflicts retain unknown source.
    pub invocations: Vec<Invocation>,
    /// Records with absent or invalid attribution, never assigned by neighboring headers.
    pub unattributed: Option<Box<Observed>>,
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
fn record_value(line: &str) -> Option<aterm_json::Value> {
    let json = line
        .strip_prefix("verify-event: ")
        .filter(|json| json.len() <= MAX_RECORD_BYTES)?;
    let value = aterm_json::from_str::<aterm_json::Value>(json).ok()?;
    (value["schema"].as_u64() == Some(1)).then_some(value)
}

fn observe_one(text: &str) -> Observed {
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
        let Some(value) = record_value(line) else {
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
        invocations: Vec::new(),
        unattributed: None,
        checks: checks.into_values().collect(),
        terminal_records: terminals.len(),
        terminal_consistency,
        malformed_complete_records: malformed,
        unprojected_records: unprojected,
        trailing_partial,
    }
}

/// Read exact event identities without inferring them from file order, checkout or headers.
#[must_use]
pub fn observe(text: &str) -> Observed {
    let mut all = observe_one(text);
    let mut groups = BTreeMap::<String, (String, Option<String>, bool)>::new();
    let mut unknown = String::new();
    for line in text.split_inclusive('\n') {
        let value = line
            .strip_suffix('\n')
            .and_then(|line| record_value(line.strip_suffix('\r').unwrap_or(line)));
        let binding = value
            .as_ref()
            .filter(|v| v["schema"].as_u64() == Some(1))
            .and_then(|v| {
                let attribution = crate::event_log::attribution(v)?;
                Some((attribution.id, attribution.source_observation))
            });
        if let Some((id, source)) = binding {
            let group = groups
                .entry(id)
                .or_insert_with(|| (String::new(), source.clone(), false));
            if group.1 != source {
                group.2 = true;
            }
            group.0.push_str(line);
        } else {
            unknown.push_str(line);
        }
    }
    all.invocations = groups
        .into_iter()
        .map(|(id, (text, source, conflict))| Invocation {
            id,
            source_observation: if conflict { None } else { source },
            source_conflict: conflict,
            observed: Box::new(observe_one(&text)),
        })
        .collect();
    if !unknown.is_empty() {
        all.unattributed = Some(Box::new(observe_one(&unknown)));
    }
    all
}

impl Observed {
    /// Machine-readable observations, omitting failure bodies, paths and rerun arguments.
    #[must_use]
    fn summary_json(&self) -> aterm_json::Value {
        aterm_json::json!({
            "schema": 2, "kind": "observed-check-completions",
            "aggregate_scope": "all-records",
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

    /// Global observations and explicit invocation groups; no qualification verdict.
    #[must_use]
    pub fn json(&self) -> aterm_json::Value {
        let mut summary = self.summary_json();
        let Some(object) = summary.as_object_mut() else {
            return summary;
        };
        object.insert("invocations".into(), aterm_json::json!(self.invocations.iter().map(|run| aterm_json::json!({
            "id":run.id, "source_observation":run.source_observation.as_ref().and_then(|s| aterm_json::from_str::<aterm_json::Value>(s).ok()),
            "source_conflict":run.source_conflict, "observed":run.observed.summary_json(),
        })).collect::<Vec<_>>()));
        object.insert(
            "unattributed".into(),
            self.unattributed
                .as_ref()
                .map_or(aterm_json::Value::Null, |rows| rows.summary_json()),
        );
        summary
    }

    fn render(&self) -> String {
        let mut out = String::from(
            "Aggregate observed completions across all records.\nUnattributed incomplete runs cannot be distinguished.\n",
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
        for run in &self.invocations {
            out.push_str(&format!(
                "Invocation {}: source observation {}\n",
                aterm_json::Value::String(run.id.clone()),
                if run.source_conflict {
                    "conflicting"
                } else if run.source_observation.is_some() {
                    "recorded (not checkout proof)"
                } else {
                    "unknown"
                }
            ));
            for check in &run.observed.checks {
                out.push_str(&format!(
                    "  {}: {} ({} observations)\n",
                    aterm_json::Value::String(check.check.clone()),
                    check
                        .outcomes
                        .iter()
                        .map(|s| aterm_json::Value::String(s.clone()).to_string())
                        .collect::<Vec<_>>()
                        .join(", "),
                    check.occurrences
                ));
            }
        }
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
    const USAGE: &str = "usage: verify status --events <path> [--json]\nRead observed finished checks, invocation groups and captured source observations; no landing verdict.\nExit: 0 inspected, 2 usage, 3 file unavailable.\n";
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

    fn writer(head: Option<String>) -> crate::event_log::EventLog {
        let ctx = crate::Ctx::new(
            PathBuf::from("/fixture"),
            crate::Scope::crate_only("aterm-verify"),
            crate::EnvSnapshot::default(),
            PathBuf::from("/fixture/scratch"),
        )
        .with_checks(&[Check::Doc])
        .with_head_at_start(head);
        let mut writer = crate::event_log::EventLog::unrecorded();
        writer.bind(crate::event_log::Context::captured(&ctx));
        writer
    }

    #[test]
    fn an_oversized_rejected_record_cannot_supply_attribution_and_crlf_uses_the_same_gate() {
        let writer = writer(None);
        let bound = writer.prepare(&event(Check::Doc));
        let crlf = format!("{}\r\n", bound.trim_end_matches('\n'));
        assert_eq!(observe(&crlf).invocations.len(), 1);
        let mut value: aterm_json::Value =
            aterm_json::from_str(bound.trim().strip_prefix("verify-event: ").unwrap()).unwrap();
        value.as_object_mut().unwrap().insert(
            "pad".into(),
            aterm_json::Value::String("x".repeat(MAX_RECORD_BYTES + 1)),
        );
        let observed = observe(&format!("verify-event: {value}\n"));
        assert!(observed.invocations.is_empty());
        assert_eq!(observed.malformed_complete_records, 1);
        assert_eq!(observed.unattributed.unwrap().malformed_complete_records, 1);
        assert!(observed.checks.is_empty());
    }

    #[test]
    fn incomplete_overlapping_invocations_partition_by_each_event_and_leave_legacy_unknown() {
        let first = writer(None);
        let second = writer(None);
        // Neither writer needs a header for its actual check record to retain attribution.
        let text = format!(
            "{}{}{}",
            first.prepare(&event(Check::Doc)),
            second.prepare(&event(Check::Doc)),
            event(Check::Fmt)
        );
        let observed = observe(&text);
        assert_eq!(observed.invocations.len(), 2);
        assert!(
            observed
                .invocations
                .iter()
                .all(|run| run.observed.checks[0].occurrences == 1)
        );
        assert_eq!(
            observed.unattributed.as_ref().unwrap().checks[0].check,
            "fmt"
        );
        assert!(observed.json()["run_identity"].is_null());
        assert!(observed.json()["source_identity"].is_null());
        assert_eq!(
            observed.json()["aggregate_scope"].as_str(),
            Some("all-records")
        );
        // A preceding bound start record cannot claim an adjacent legacy record.
        let header_only = format!(
            "{}{}",
            first.prepare("verify-event: {\"schema\":1,\"event\":\"invocation-started\"}"),
            event(Check::Doc)
        );
        let observed = observe(&header_only);
        assert!(observed.invocations[0].observed.checks.is_empty());
        assert_eq!(observed.unattributed.unwrap().checks[0].check, "doc");
    }

    #[test]
    fn conflicting_source_metadata_and_partial_invalid_ids_never_become_checkout_proof() {
        let first = writer(Some("a".repeat(40)));
        let other = writer(Some("b".repeat(40)));
        let original = first.prepare(&event(Check::Doc));
        let mut foreign: aterm_json::Value = aterm_json::from_str(
            other
                .prepare(&event(Check::Fmt))
                .trim()
                .strip_prefix("verify-event: ")
                .unwrap(),
        )
        .unwrap();
        foreign
            .as_object_mut()
            .unwrap()
            .get_mut("invocation")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert(
                "id".into(),
                aterm_json::Value::String(first.id().unwrap().to_owned()),
            );
        let mut invalid = foreign.clone();
        invalid
            .as_object_mut()
            .unwrap()
            .get_mut("invocation")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("id".into(), aterm_json::Value::String("not-an-id".into()));
        let text =
            format!("{original}verify-event: {foreign}\nverify-event: {invalid}\nverify-event: {{");
        let observed = observe(&text);
        assert_eq!(observed.invocations.len(), 1);
        assert!(observed.invocations[0].source_conflict);
        assert!(observed.invocations[0].source_observation.is_none());
        assert!(observed.unattributed.is_some());
        assert!(observed.trailing_partial);
        assert!(observed.json()["source_identity"].is_null());
        assert!(observed.json()["invocations"][0]["source_observation"].is_null());
    }

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
    fn legacy_incomplete_appended_runs_are_indistinguishable_from_one_run() {
        use std::io::Write as _;
        let dir = aterm_tempfile::tempdir().unwrap();
        let one = dir.path().join("one.events");
        let two = dir.path().join("two.events");
        // Historical report bytes predate per-event attribution. Current EventLog writers
        // deliberately cannot reproduce this ambiguity: each now supplies its own identity.
        std::fs::write(&one, format!("{}{}", event(Check::Doc), event(Check::Lint))).unwrap();
        for row in [event(Check::Doc), event(Check::Lint)] {
            std::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(&two)
                .unwrap()
                .write_all(row.as_bytes())
                .unwrap();
        }
        let one_incomplete_run = std::fs::read_to_string(one).unwrap();
        let separate_incomplete_runs = std::fs::read_to_string(two).unwrap();
        assert_eq!(one_incomplete_run, separate_incomplete_runs);
        let observed = observe(&one_incomplete_run);
        assert_eq!(observed, observe(&separate_incomplete_runs));
        assert!(observed.invocations.is_empty());
        assert!(observed.unattributed.is_some());
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
