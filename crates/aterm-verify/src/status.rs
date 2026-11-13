// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Read check completions and incomplete case/compiler metadata by diagnostic invocation.
//! Legacy records remain unattributed; captured source facts never prove the checkout.
//! This reader grants no check, resource-retirement or land verdict.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::event_log::{CheckRow, Finished, Judged};

const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
use crate::event_log::MAX_RECORD_BYTES;
const COMPILER_STAGE_ROWS: usize = 64;
const LONG_CASE_ROWS: usize = 64;
const CASE_NAME_BYTES: usize = 4096;
const PHASE_END_ROWS: usize = 64;

/// Historical END metadata, never reconstructed phase timing or current activity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservedPhaseEnd {
    pub invocation_id: Option<String>,
    pub test_run: u64,
    pub phase_sequence: u32,
    pub occurrences: usize,
    pub conflict: bool,
    end: Option<crate::test_progress::End>,
}

/// Historical observations keyed by exact reported names, not binary or original case identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservedLongCase {
    pub test: String,
    pub case: String,
    pub occurrences: usize,
    /// Timestamp from the last supported record in supplied file order, not current activity.
    pub last_reported_at_unix_ms: u64,
}

// Decode raw JSON before a map can erase duplicate known fields. Extra producer fields
// (including elapsed time, bodies and locations) are neither interpreted nor copied.
#[derive(serde::Deserialize)]
struct LongCaseWire {
    schema: u64,
    event: crate::event_log::EventKind,
    test: String,
    case: String,
    outcome: String,
    binary_completed: bool,
    sample_complete: bool,
    halts: bool,
    at_unix_ms: u64,
}

impl LongCaseWire {
    fn read(json: &str) -> Option<Self> {
        let wire: Self = aterm_json::from_str(json).ok()?;
        (wire.schema == 1
            && wire.event == crate::event_log::EventKind::TestCaseRunningLong
            && wire.outcome == "RUNNING"
            && !wire.binary_completed
            && !wire.sample_complete
            && !wire.halts
            && !wire.test.is_empty()
            && wire.test.len() <= CASE_NAME_BYTES
            && wire.case.len() <= CASE_NAME_BYTES)
            .then_some(wire)
    }
}

/// Last safe provisional message in supplied file order, not an active compiler or unit count.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservedCompilerStage {
    pub stage: String,
    pub observed_message_records: usize,
    latest: crate::compiler_observation::Metadata,
}

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

/// Incomplete case evidence, keyed by the full reported test and optional case names.
/// Names may already have been clipped by the producer; they are not process identities.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservedCase {
    pub test: String,
    pub case: Option<String>,
    pub outcomes: BTreeSet<String>,
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
    /// Supported failed-case observations, independently of finished checks and terminals.
    pub early_cases: Vec<ObservedCase>,
    /// Incomplete historical long-case observations, independent of check/terminal verdicts.
    pub long_cases: Vec<ObservedLongCase>,
    pub long_case_records: usize,
    pub long_case_omitted_records: usize,
    pub phase_ends: Vec<ObservedPhaseEnd>,
    pub phase_end_records: usize,
    pub phase_end_omitted_records: usize,
    /// At most 64 stage rows; grouping uses full stage labels before display clipping.
    pub compiler_progress: Vec<ObservedCompilerStage>,
    pub compiler_message_records: usize,
    /// Valid observations for stage keys not retained because the row cap was reached.
    pub compiler_omitted_records: usize,
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
fn record_value(line: &str) -> Option<crate::event_log::RawEvent> {
    let json = line
        .strip_prefix("verify-event: ")
        .filter(|json| json.len() <= MAX_RECORD_BYTES)?;
    crate::event_log::raw_event(json)
}

pub(crate) fn supported_progress_payload(
    kind: crate::event_log::EventKind,
    json: &str,
) -> Option<bool> {
    use crate::event_log::EventKind;
    match kind {
        EventKind::TestPhaseStarted => Some(crate::test_progress::supported_start(json)),
        EventKind::TestPhaseEnded => Some(crate::test_progress::End::read(json).is_some()),
        EventKind::TestCaseRunningLong => Some(LongCaseWire::read(json).is_some()),
        EventKind::CompileMessageObserved => {
            Some(crate::compiler_observation::Observation::read(json).is_some())
        }
        _ => None,
    }
}

fn observe_one(text: &str) -> Observed {
    let mut checks = BTreeMap::<String, ObservedCheck>::new();
    let mut cases = BTreeMap::<(String, Option<String>), ObservedCase>::new();
    let mut long_cases = BTreeMap::<(String, String), ObservedLongCase>::new();
    let mut long_case_records = 0;
    let mut long_case_omitted_records = 0;
    let mut phase_ends = BTreeMap::<(Option<String>, u64, u32), ObservedPhaseEnd>::new();
    let mut phase_end_records = 0;
    let mut phase_end_omitted_records = 0;
    let mut compiler = BTreeMap::<String, ObservedCompilerStage>::new();
    let mut compiler_message_records = 0;
    let mut compiler_omitted_records = 0;
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
        let Some(record) = record_value(line) else {
            malformed += 1;
            continue;
        };
        let value = record.value;
        match value["event"].as_str() {
            Some("test-phase-ended") => {
                let Some(end) = line
                    .strip_prefix("verify-event: ")
                    .and_then(crate::test_progress::End::read)
                else {
                    unprojected += 1;
                    continue;
                };
                phase_end_records += 1;
                let id = record.attribution.map(|binding| binding.id);
                let key = (id.clone(), end.run, end.sequence);
                if let Some(row) = phase_ends.get_mut(&key) {
                    row.occurrences += 1;
                    if row.end.as_ref() != Some(&end) {
                        row.conflict = true;
                        row.end = None;
                    }
                } else if phase_ends.len() < PHASE_END_ROWS {
                    phase_ends.insert(
                        key,
                        ObservedPhaseEnd {
                            invocation_id: id,
                            test_run: end.run,
                            phase_sequence: end.sequence,
                            occurrences: 1,
                            conflict: false,
                            end: Some(end),
                        },
                    );
                } else {
                    phase_end_omitted_records += 1;
                }
            }
            Some("test-case-running-long") => {
                let Some(wire) = line
                    .strip_prefix("verify-event: ")
                    .and_then(LongCaseWire::read)
                else {
                    unprojected += 1;
                    continue;
                };
                long_case_records += 1;
                let key = (wire.test.clone(), wire.case.clone());
                if let Some(row) = long_cases.get_mut(&key) {
                    row.occurrences += 1;
                    row.last_reported_at_unix_ms = wire.at_unix_ms;
                } else if long_cases.len() < LONG_CASE_ROWS {
                    long_cases.insert(
                        key,
                        ObservedLongCase {
                            test: wire.test,
                            case: wire.case,
                            occurrences: 1,
                            last_reported_at_unix_ms: wire.at_unix_ms,
                        },
                    );
                } else {
                    long_case_omitted_records += 1;
                }
            }
            Some("compile-message-observed") => {
                let observation = line
                    .strip_prefix("verify-event: ")
                    .and_then(crate::compiler_observation::Observation::read);
                let Some(observation) = observation else {
                    unprojected += 1;
                    continue;
                };
                compiler_message_records += 1;
                if let Some(row) = compiler.get_mut(&observation.stage) {
                    row.observed_message_records += 1;
                    row.latest = observation.metadata;
                } else if compiler.len() < COMPILER_STAGE_ROWS {
                    compiler.insert(
                        observation.stage.clone(),
                        ObservedCompilerStage {
                            stage: observation.stage,
                            observed_message_records: 1,
                            latest: observation.metadata,
                        },
                    );
                } else {
                    compiler_omitted_records += 1;
                }
            }
            Some("test-case-failed") => {
                let Some((test, case, word)) = failed_case_metadata(&value) else {
                    // Previously unprojected events cannot alter check/terminal consistency
                    // merely because their case metadata is unsupported.
                    unprojected += 1;
                    continue;
                };
                let observed = cases
                    .entry((test.clone(), case.clone()))
                    .or_insert_with(|| ObservedCase {
                        test,
                        case,
                        outcomes: BTreeSet::new(),
                        occurrences: 0,
                    });
                observed.outcomes.insert(word);
                observed.occurrences += 1;
            }
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
        early_cases: cases.into_values().collect(),
        long_cases: long_cases.into_values().collect(),
        long_case_records,
        long_case_omitted_records,
        phase_ends: phase_ends.into_values().collect(),
        phase_end_records,
        phase_end_omitted_records,
        compiler_progress: compiler.into_values().collect(),
        compiler_message_records,
        compiler_omitted_records,
        terminal_records: terminals.len(),
        terminal_consistency,
        malformed_complete_records: malformed,
        unprojected_records: unprojected,
        trailing_partial,
    }
}

fn failed_case_metadata(value: &aterm_json::Value) -> Option<(String, Option<String>, String)> {
    if value["binary_completed"].as_bool() != Some(false)
        || value["sample_complete"].as_bool() != Some(false)
    {
        return None;
    }
    let test = value["test"].as_str().filter(|test| !test.is_empty())?;
    let word = value["outcome"]
        .as_str()
        .filter(|word| matches!(*word, "FAIL" | "NOT RUN"))?;
    let case = value.get("case")?;
    let case = if case.is_null() {
        None
    } else {
        Some(case.as_str()?.to_owned())
    };
    Some((test.to_owned(), case, word.to_owned()))
}

/// Read exact event identities without inferring them from file order, checkout or headers.
#[must_use]
pub fn observe(text: &str) -> Observed {
    let mut all = observe_one(text);
    let mut groups = BTreeMap::<String, (String, Option<String>, bool)>::new();
    let mut unknown = String::new();
    for line in text.split_inclusive('\n') {
        let record = line
            .strip_suffix('\n')
            .and_then(|line| record_value(line.strip_suffix('\r').unwrap_or(line)));
        let binding = record
            .and_then(|record| record.attribution)
            .map(|binding| (binding.id, binding.source_observation));
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
            "case_observations_complete": false,
            "phase_end_observations_complete":false,
            "phase_end_scope":"historical END records only; unattributed counters may collide; no current phase or reconstructed duration",
            "phase_end_row_cap":PHASE_END_ROWS,
            "phase_end_records":self.phase_end_records,
            "phase_end_omitted_records":self.phase_end_omitted_records,
            "phase_ends":self.phase_ends.iter().map(|row| aterm_json::json!({
                "invocation_id":row.invocation_id,"check":row.end.as_ref().map(|end|end.check.name()),"test_run":row.test_run,
                "phase_sequence":row.phase_sequence,"phase":row.end.as_ref().map(|end|end.phase),"occurrences":row.occurrences,
                "conflict":row.conflict,"seconds":row.end.as_ref().map(crate::test_progress::End::seconds),
                "scope_exit":row.end.as_ref().map(|end| end.scope_exit),
                "provisional":true,"sample_complete":false,
            })).collect::<Vec<_>>(),
            "long_case_observations_complete":false,
            "long_case_scope":"historical reported spec/case names; not binary identity or current activity",
            "long_case_timestamp_order":"last supported record in supplied file order",
            "long_case_row_cap":LONG_CASE_ROWS,
            "long_case_records":self.long_case_records,
            "long_case_omitted_records":self.long_case_omitted_records,
            "long_cases":self.long_cases.iter().map(|row| {
                let test = crate::report::clip_json(&row.test, crate::report::EVENT_ROW_BYTES);
                let case = crate::report::clip_json(&row.case, crate::report::EVENT_ROW_BYTES);
                aterm_json::json!({"test_truncated":test != row.test,"case_truncated":case != row.case,
                    "test":test,"case":case,"occurrences":row.occurrences,
                    "last_reported_at_unix_ms":row.last_reported_at_unix_ms,
                    "reported_outcome":"RUNNING","binary_completed":false,"sample_complete":false,"halts":false})
            }).collect::<Vec<_>>(),
            "compiler_progress_complete":false,
            "compiler_progress_order":"last complete supported record in supplied file order",
            "compiler_progress_row_cap":COMPILER_STAGE_ROWS,
            "compiler_message_records":self.compiler_message_records,
            "compiler_omitted_records":self.compiler_omitted_records,
            "compiler_progress":self.compiler_progress.iter().map(|row| {
                let stage = crate::report::clip_json(&row.stage, crate::report::EVENT_ROW_BYTES);
                aterm_json::json!({"stage_truncated":stage != row.stage, "stage":stage,
                    "observed_message_records":row.observed_message_records,
                    "provisional":true,"sample_complete":false,"latest":row.latest.json()})
            }).collect::<Vec<_>>(),
            "early_cases": self.early_cases.iter().map(|case| {
                let test = crate::report::clip_json(&case.test, crate::report::EVENT_ROW_BYTES);
                let name = case.case.as_ref().map(|name| crate::report::clip_json(name, crate::report::EVENT_ROW_BYTES));
                aterm_json::json!({
                    "test_truncated":test != case.test, "case_truncated":name != case.case,
                    "test":test, "case":name,
                    "outcomes":case.outcomes.iter().map(String::as_str).collect::<Vec<_>>(),
                    "occurrences":case.occurrences, "conflict":case.outcomes.len() > 1,
                })
            }).collect::<Vec<_>>(),
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
        self.render_cases(&mut out, "");
        self.render_long_cases(&mut out, "");
        self.render_phase_ends(&mut out, "");
        self.render_compiler(&mut out, "");
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
            run.observed.render_cases(&mut out, "  ");
            run.observed.render_long_cases(&mut out, "  ");
            run.observed.render_phase_ends(&mut out, "  ");
            run.observed.render_compiler(&mut out, "  ");
        }
        out
    }

    fn render_cases(&self, out: &mut String, prefix: &str) {
        if self.early_cases.is_empty() {
            return;
        }
        out.push_str(&format!(
            "{prefix}Early case observations (incomplete; not check completions):\n"
        ));
        for case in &self.early_cases {
            let test = crate::report::clip_json(&case.test, crate::report::EVENT_ROW_BYTES);
            let name = case
                .case
                .as_ref()
                .map(|name| crate::report::clip_json(name, crate::report::EVENT_ROW_BYTES));
            out.push_str(&format!(
                "{prefix}  test={} case={}: {} ({} observations{}{}{})\n",
                aterm_json::Value::String(test.clone()),
                name.clone()
                    .map_or(aterm_json::Value::Null, aterm_json::Value::String),
                case.outcomes
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join(", "),
                case.occurrences,
                if case.outcomes.len() > 1 {
                    "; conflict"
                } else {
                    ""
                },
                if test != case.test {
                    "; test name truncated"
                } else {
                    ""
                },
                if name != case.case {
                    "; case name truncated"
                } else {
                    ""
                }
            ));
        }
    }

    fn render_compiler(&self, out: &mut String, prefix: &str) {
        if self.compiler_message_records == 0 {
            return;
        }
        out.push_str(&format!("{prefix}Compiler observations (provisional; incomplete; last supported record in supplied file order):\n"));
        out.push_str(&format!(
            "{prefix}  {} message records; row cap {}; {} records omitted by row cap.\n",
            self.compiler_message_records, COMPILER_STAGE_ROWS, self.compiler_omitted_records
        ));
        for row in &self.compiler_progress {
            let stage = crate::report::clip_json(&row.stage, crate::report::EVENT_ROW_BYTES);
            out.push_str(&format!(
                "{prefix}  stage={}{}: {} ({} observed records; sample_complete=false)\n",
                aterm_json::Value::String(stage.clone()),
                if stage != row.stage {
                    " [truncated]"
                } else {
                    ""
                },
                row.latest.json(),
                row.observed_message_records
            ));
        }
    }

    fn render_long_cases(&self, out: &mut String, prefix: &str) {
        if self.long_case_records == 0 {
            return;
        }
        out.push_str(&format!("{prefix}Historical long-case observations (incomplete; reported names, not binary identity or current activity):\n"));
        out.push_str(&format!(
            "{prefix}  {} records; row cap {}; {} records omitted by row cap.\n",
            self.long_case_records, LONG_CASE_ROWS, self.long_case_omitted_records
        ));
        for row in &self.long_cases {
            let test = crate::report::clip_json(&row.test, crate::report::EVENT_ROW_BYTES);
            let case = crate::report::clip_json(&row.case, crate::report::EVENT_ROW_BYTES);
            out.push_str(&format!("{prefix}  test={} case={}: reported RUNNING ({} observations; last supplied record timestamp {}; sample_complete=false{}).\n",
                aterm_json::Value::String(test.clone()), aterm_json::Value::String(case.clone()),
                row.occurrences, row.last_reported_at_unix_ms,
                if test != row.test || case != row.case { "; display names truncated" } else { "" }));
        }
    }

    fn render_phase_ends(&self, out: &mut String, prefix: &str) {
        if self.phase_end_records == 0 {
            return;
        }
        out.push_str(&format!("{prefix}Historical test-phase END observations (incomplete; no current phase or reconstructed duration):\n"));
        out.push_str(&format!(
            "{prefix}  {} records; row cap {}; {} records omitted by row cap.\n",
            self.phase_end_records, PHASE_END_ROWS, self.phase_end_omitted_records
        ));
        for row in &self.phase_ends {
            let metadata = row.end.as_ref().map_or_else(
                || "duration/scope unknown (conflicting records)".into(),
                |end| {
                    format!(
                        "check={} phase={} seconds={} scope_exit={}",
                        end.check.name(),
                        aterm_json::json!(end.phase),
                        end.seconds(),
                        aterm_json::json!(end.scope_exit)
                    )
                },
            );
            out.push_str(&format!("{prefix}  invocation={} run={} sequence={}: {} ({} observations; sample_complete=false).\n",
                aterm_json::json!(row.invocation_id), row.test_run, row.phase_sequence,
                metadata, row.occurrences));
        }
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
    const USAGE: &str = "usage: verify status --events <path> [--json]\nRead recorded check completions, incomplete case/compiler observations, invocation groups and captured source observations; no landing verdict.\nExit: 0 inspected, 2 usage, 3 file unavailable.\n";
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
#[path = "status_tests.rs"]
mod tests;
