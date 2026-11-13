// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-check output, finding summaries, and final verdict formatting.

use std::fmt::Write as _;
use std::time::Duration;

use crate::checks::Check;
use crate::ladder::{Finding, Outcome, Report, Severity};

/// How many findings a failed check lists before pointing at the log.
pub const FINDINGS_SHOWN: usize = 10;

use crate::compiler_message::UnitMessage;

/// The bounded v1 dispatch evidence emitted by Targo, not a process-start or success receipt.
#[derive(Clone, Copy, PartialEq, Eq)]
struct UnitDispatch<'a> {
    mode: &'a str,
    kind: &'a str,
    fresh: bool,
    cause: Option<&'a str>,
}

fn unit_identity(value: &aterm_json::Value) -> Option<&str> {
    value
        .get("trust_unit_identity_sha256")?
        .as_str()
        .filter(|hash| aterm_codec::hex::is_lower(hash, 64))
}

fn unit_dispatch(value: &aterm_json::Value) -> Option<UnitDispatch<'_>> {
    use aterm_json::Value;
    if value.as_object()?.len() != 7
        || value.get("schema")?.as_str()? != "targo.compilation-unit-dispatched.v1"
        || value.get("reason")?.as_str()? != "compilation-unit-dispatched"
    {
        return None;
    }
    let mode = value.get("trust_compile_mode")?.as_str()?;
    let kind = value.get("trust_compile_kind")?.as_str()?;
    if !crate::compiler_observation::dispatch_mode(mode)
        || !crate::compiler_observation::compile_kind(kind)
    {
        return None;
    }
    let fresh = value.get("fresh")?.as_bool()?;
    let cause = match value.get("dirty_reason")? {
        Value::Null if fresh => None,
        Value::String(cause)
            if !fresh
                && matches!(
                    cause.as_str(),
                    "rustc-changed"
                        | "features-changed"
                        | "declared-features-changed"
                        | "target-configuration-changed"
                        | "path-to-source-changed"
                        | "profile-configuration-changed"
                        | "rustflags-changed"
                        | "config-settings-changed"
                        | "compile-kind-changed"
                        | "local-lengths-changed"
                        | "precalculated-components-changed"
                        | "fingerprint-method-changed"
                        | "dep-info-output-changed"
                        | "rerun-if-changed-output-file-changed"
                        | "rerun-if-changed-output-paths-changed"
                        | "env-vars-changed"
                        | "env-var-changed"
                        | "local-fingerprint-type-changed"
                        | "number-of-dependencies-changed"
                        | "unit-dependency-name-changed"
                        | "unit-dependency-info-changed"
                        | "missing-file"
                        | "unable-to-read-file"
                        | "failed-to-read-metadata"
                        | "file-size-changed"
                        | "changed-file"
                        | "changed-checksum"
                        | "missing-checksum"
                        | "changed-env"
                        | "stale-dependency"
                        | "stale-dep-fingerprint"
                        | "output-integrity"
                        | "forced"
                        | "fresh-build"
                        | "unknown"
                ) =>
        {
            Some(cause.as_str())
        }
        _ => return None,
    };
    unit_identity(value)?;
    Some(UnitDispatch {
        mode,
        kind,
        fresh,
        cause,
    })
}

/// One complete compiler message observed, without whole-output joins or completion evidence.
pub(crate) fn provisional_compile_event(stage: &str, line: &str) -> Option<String> {
    use crate::compiler_observation::{Metadata, Observation};
    let UnitMessage::Unit(value) = aterm_json::from_str::<UnitMessage>(line).ok()? else {
        return None;
    };
    let reason = value.get("reason")?.as_str()?;
    let metadata = match reason {
        "compilation-unit-dispatched" => {
            let dispatch = unit_dispatch(&value)?;
            Metadata::dispatch(dispatch.mode, dispatch.kind)?
        }
        "compiler-artifact" => {
            let target = value.get("target")?;
            let name = target.get("name")?.as_str()?;
            let kinds = crate::compiler_message::artifact_target_kinds(&value).ok()?;
            Metadata::artifact(name, &kinds)?
        }
        _ => return None,
    };
    Some(Observation::new(stage, metadata)?.event())
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct UnitContext<'a> {
    mode: Option<&'a str>,
    kind: Option<&'a str>,
    fresh: Option<bool>,
}

#[derive(Default)]
struct UnitEvidence<'a> {
    dispatch: Option<Option<UnitDispatch<'a>>>,
    artifact: Option<UnitContext<'a>>,
    conflicting: bool,
    ambiguous: bool,
}

impl<'a> UnitEvidence<'a> {
    fn observe_dispatch(&mut self, dispatch: Option<UnitDispatch<'a>>) {
        if let Some(prior) = self.dispatch {
            self.conflicting |= prior != dispatch;
        } else {
            self.dispatch = Some(dispatch);
        }
    }

    fn observe_artifact(&mut self, value: &'a aterm_json::Value) {
        use aterm_json::Value;
        let context = UnitContext {
            mode: value.get("trust_compile_mode").and_then(Value::as_str),
            kind: value.get("trust_compile_kind").and_then(Value::as_str),
            fresh: value.get("fresh").and_then(Value::as_bool),
        };
        if let Some(prior) = self.artifact {
            self.conflicting |= prior != context;
        } else {
            self.artifact = Some(context);
        }
    }

    fn joined(&self) -> Option<UnitDispatch<'a>> {
        let dispatch = self.dispatch??;
        (!self.conflicting
            && !self.ambiguous
            && self.artifact
                == Some(UnitContext {
                    mode: Some(dispatch.mode),
                    kind: Some(dispatch.kind),
                    fresh: Some(dispatch.fresh),
                }))
        .then_some(dispatch)
    }

    fn cause(&self, fresh: Option<bool>) -> (Option<&'a str>, &'static str) {
        if self.ambiguous {
            return (None, "ambiguous-message");
        }
        if self.dispatch.is_none() {
            return (
                None,
                match fresh {
                    Some(true) => "not-applicable",
                    Some(false) => "not-reported-by-producer",
                    None => "freshness-unknown",
                },
            );
        }
        if self.dispatch == Some(None) {
            return (None, "dispatch-unavailable");
        }
        let Some(dispatch) = self.joined() else {
            return (None, "conflicting-dispatch");
        };
        match dispatch.cause {
            None => (None, "not-applicable"),
            Some("unknown") => (None, "producer-unknown"),
            cause => (cause, "producer-dispatch"),
        }
    }
}

/// Retain the producer's artifact context before test discovery consumes its JSON messages.
/// Cargo's artifact schema alone reports freshness, not why a unit was rebuilt. Only a
/// recognized producer dispatch with the same unit identity, mode, kind and freshness can
/// supply a cause. `fresh=false` alone proves neither a cache miss nor a source change. Optional
/// Trust fields identify the exact producer unit when that producer supplies them; ordinary
/// Cargo target metadata alone does not identify the compilation triple or host/target role.
#[must_use]
pub fn compile_unit_events(stage: &str, output: &str) -> Vec<String> {
    use aterm_json::Value;
    let messages: Vec<UnitMessage> = output
        .lines()
        .filter_map(|line| aterm_json::from_str::<UnitMessage>(line).ok())
        .filter(|message| !matches!(message, UnitMessage::Other))
        .collect();
    // This index exists only for this one compiler output, never across calls or invocations.
    let mut units = std::collections::BTreeMap::<&str, UnitEvidence<'_>>::new();
    let mut dispatches = Vec::with_capacity(messages.len());
    for message in &messages {
        let UnitMessage::Unit(value) = message else {
            dispatches.push(None);
            if let UnitMessage::Ambiguous { identities, .. } = message {
                for identity in identities {
                    units.entry(identity).or_default().ambiguous = true;
                }
            }
            continue;
        };
        let dispatched =
            value.get("reason").and_then(Value::as_str) == Some("compilation-unit-dispatched");
        let dispatch = dispatched.then(|| unit_dispatch(value)).flatten();
        dispatches.push(dispatch);
        if let Some(identity) = unit_identity(value) {
            let unit = units.entry(identity).or_default();
            if dispatched {
                unit.observe_dispatch(dispatch);
            } else {
                unit.observe_artifact(value);
            }
        }
    }
    fn fields(value: Option<&Value>, keys: &[&str]) -> Option<Value> {
        let object = value?.as_object()?;
        Some(Value::Object(
            keys.iter()
                .filter_map(|key| {
                    object
                        .get(*key)
                        .map(|value| ((*key).to_owned(), value.clone()))
                })
                .collect(),
        ))
    }
    let unavailable = UnitEvidence::default();
    messages.iter().zip(dispatches).map(|(message, dispatch)| {
        let UnitMessage::Unit(value) = message else {
            return format!("verify-event: {}", crate::event_log::event_value(crate::event_log::EventKind::CompileUnitMessageUnavailable, aterm_json::json!({
                  "stage": stage,
                "detail": "duplicate fields in a unit message",
            })));
        };
        let identity = unit_identity(value);
        let unit = identity.and_then(|identity| units.get(identity)).unwrap_or(&unavailable);
        if value.get("reason").and_then(Value::as_str) == Some("compilation-unit-dispatched") {
            let reported = unit.joined().map(|_| true);
            return format!("verify-event: {}", crate::event_log::event_value(crate::event_log::EventKind::CompileUnitDispatched, aterm_json::json!({
                  "stage": stage,
                "producer_unit_sha256": identity,
                "compile_kind": dispatch.map(|d| d.kind), "compile_mode": dispatch.map(|d| d.mode),
                "fresh": dispatch.map(|d| d.fresh), "dirty_reason": dispatch.and_then(|d| d.cause),
                "evidence_status": if dispatch.is_some() { "producer-dispatch" } else { "dispatch-unavailable" },
                "artifact_reported": reported,
            })));
        }
        let string = |key| value.get(key).and_then(Value::as_str);
        let fresh = value.get("fresh").and_then(Value::as_bool);
        let (cause, cause_status) = unit.cause(fresh);
        // Only the four measuring benches supply cross-tree bundle evidence. Hash the
        // executable itself, including its linked harness implementation and dependencies.
        let harness_sha256 = value.get("target").filter(|target| {
            target["kind"].as_array().is_some_and(|k| k.len() == 1 && k[0].as_str() == Some("bench"))
                && target["name"].as_str().is_some_and(|name| crate::stages::MEASURING_TARGETS.contains(&name))
        }).and_then(|_| value["manifest_path"].as_str())
            .filter(|path| path.ends_with("/crates/aterm-conformance/Cargo.toml"))
            .and_then(|_| value["executable"].as_str())
            .and_then(|path| crate::test_index::digest(std::path::Path::new(path)).ok());
        format!("verify-event: {}", crate::event_log::event_value(crate::event_log::EventKind::CompileUnitFinished, aterm_json::json!({
              "stage": stage,
            "package_id": string("package_id"),
            "manifest_path": string("manifest_path"),
            "target": fields(value.get("target"), &["name", "kind", "crate_types", "src_path", "edition"]),
            "profile": fields(value.get("profile"), &["opt_level", "debuginfo", "debug_assertions", "overflow_checks", "test"]),
            "features": value.get("features").filter(|v| v.as_array().is_some_and(|a| a.iter().all(|v| v.as_str().is_some()))),
            "compile_target": string("trust_compile_target"),
            "compile_kind": string("trust_compile_kind"),
            "compile_mode": string("trust_compile_mode"),
            "producer_unit_sha256": identity,
            "fresh": fresh,
            "rebuild_reason": cause,
            "rebuild_reason_status": cause_status,
            "measure_harness_sha256": harness_sha256,
        })))
    }).collect()
}

/// Capture the running verifier before it starts a compiler that may uplift its path. Both
/// release halves use the same Cargo launch; workspace compilation must not change this key.
pub(crate) fn running_driver() -> Option<&'static str> {
    static DRIVER: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    DRIVER
        .get_or_init(|| {
            std::env::current_exe()
                .ok()
                .and_then(|path| crate::test_index::digest(&path).ok())
        })
        .as_deref()
}

/// A stage admitted to execute, paired with its completion by the run-local index.
#[must_use]
pub fn stage_started(index: usize, title: &str) -> String {
    let at = aterm_time::unix_millis();
    format!(
        "verify-event: {}\n",
        crate::event_log::event_value(
            crate::event_log::EventKind::StageStarted,
            aterm_json::json!({
                "stage_index": index,
                "stage": title,
                "started_at_unix_ms": at,
                "at_unix_ms": at,
                "seconds": 0.0,
                "cpu_s": null,
            })
        )
    )
}

/// A completed stage's decisions and timing, emitted before sibling stages finish.
/// Waiting is time from the run's start until this stage actually starts executing;
/// it includes dependencies and resource admission, not CPU time.
/// Child CPU is a sampled lower bound, excluding verifier work and descendants
/// missed between samples. With no observed process counter, the field is JSON
/// null rather than zero.
/// Finding diagnostics reuse the report's existing excerpts. Only this added payload is
/// bounded; pre-existing decisions and finding IDs retain their existing representation.
#[must_use]
pub fn stage_event(
    index: usize,
    check: Option<Check>,
    report: &Report,
    waiting: Duration,
    execution: Duration,
    cpu: Option<crate::memory::ObservedCpu>,
) -> String {
    let at = aterm_time::unix_millis();
    let decisions: Vec<_> = report
        .outcomes()
        .map(|(outcome, label)| aterm_json::json!({"outcome": word(outcome), "detail": label}))
        .collect();
    let findings = report.findings();
    let diagnostics = stage_finding_diagnostics(&findings);
    format!(
        "verify-event: {}\n",
        crate::event_log::event_value(
            crate::event_log::EventKind::StageFinished,
            aterm_json::json!({
                "stage_index": index,
                "check": check.map(Check::name),
                "stage": report.title,
                "ended_at_unix_ms": at,
                "at_unix_ms": at,
                "seconds": execution.as_secs_f64(),
                "cpu_s": cpu.map(|cpu| Duration::from_nanos(cpu.nanoseconds).as_secs_f64()),
                "waiting_s": waiting.as_secs_f64(),
                "execution_s": execution.as_secs_f64(),
                "child_cpu": cpu.map(|cpu| aterm_json::json!({
                    "scope": "sampled-child-process-groups",
                    "coverage": "lower-bound",
                    "observed_s": Duration::from_nanos(cpu.nanoseconds).as_secs_f64(),
                    "observed_processes": cpu.processes,
                })),
                "decisions": decisions,
                "findings": findings.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(),
                "finding_diagnostics": diagnostics,
            })
        )
    )
}

/// Limit the complete added JSON object, including its counters and encoding overhead.
/// These are excerpts from the existing findings, not a new classification or raw-log copy.
const STAGE_DIAGNOSTIC_BYTES: usize = 16 * 1024;
const STAGE_MESSAGE_LINES: usize = crate::findings::MESSAGE_LINES;

fn stage_finding_diagnostics(findings: &[Finding]) -> aterm_json::Value {
    let envelope = |rows: &[aterm_json::Value]| {
        aterm_json::json!({
            "rows":rows, "rows_total":findings.len(), "rows_omitted":findings.len() - rows.len(),
            "row_limit":FINDINGS_SHOWN, "encoded_byte_limit":STAGE_DIAGNOSTIC_BYTES,
            "complete":false,
        })
    };
    let mut rows = Vec::new();
    for finding in findings.iter().take(FINDINGS_SHOWN) {
        let id = clip_json(&finding.id, EVENT_ROW_BYTES);
        let message: Vec<_> = finding
            .message
            .iter()
            .take(STAGE_MESSAGE_LINES)
            .map(|line| clip_json(line, EVENT_ROW_BYTES))
            .collect();
        let truncated_lines = message
            .iter()
            .zip(&finding.message)
            .filter(|(shown, original)| *shown != *original)
            .count();
        rows.push(aterm_json::json!({
            "id_truncated":id != finding.id, "id":id, "message":message,
            "message_lines_total":finding.message.len(),
            "message_lines_omitted":finding.message.len().saturating_sub(STAGE_MESSAGE_LINES),
            "message_truncated_lines":truncated_lines,
        }));
        if envelope(&rows).to_string().len() > STAGE_DIAGNOSTIC_BYTES {
            rows.pop();
            break;
        }
    }
    envelope(&rows)
}

#[cfg(test)]
#[path = "report_stage_event_tests.rs"]
mod stage_event_tests;

/// One stage's compile or drive boundary. CPU is the child's sampled process-group
/// lower bound, with the same scope as the parent stage, never elapsed wall time.
#[must_use]
pub(crate) fn row_event(
    stage: &str,
    row: &str,
    finished: Option<(Duration, Option<crate::exec::TreeUsage>)>,
) -> String {
    let (seconds, usage) = finished.unwrap_or_default();
    format!(
        "verify-event: {}",
        crate::event_log::event_value(
            if finished.is_some() {
                crate::event_log::EventKind::RowFinished
            } else {
                crate::event_log::EventKind::RowStarted
            },
            aterm_json::json!({
                "stage": stage, "row": row, "at_unix_ms": aterm_time::unix_millis(),
                "seconds": seconds.as_secs_f64(),
                "cpu_s": usage.map(|u| u.cpu.as_secs_f64()),
                "cpu_scope": "sampled-child-process-groups",
                "cpu_coverage": "lower-bound",
            })
        )
    )
}

/// The measuring harness owns these execution timestamps. Its output is buffered,
/// so keep them when copying row events into the verifier's durable event stream.
#[must_use]
pub(crate) fn measurement_events(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|line| {
            let json = line.strip_prefix("verify-event: ")?;
            match crate::event_log::observation(json) {
                crate::event_log::Observation::Known {
                    kind:
                        crate::event_log::EventKind::MeasureRowStarted
                        | crate::event_log::EventKind::MeasureRowFinished,
                    ..
                } => Some(line.to_owned()),
                _ => None,
            }
        })
        .collect()
}

/// One check's result: what its stages' rows add up to.
#[derive(Clone, Debug, PartialEq)]
pub struct CheckResult {
    pub check: Check,
    pub outcome: Outcome,
    pub seconds: f64,
    /// One line: the scope it ran over, or why it failed, skipped or could
    /// not run.
    pub detail: String,
    /// The failures, itemized; for a skip or a could-not-run, every reason.
    pub findings: Vec<Finding>,
    /// The command that reruns just this check.
    pub rerun: String,
    /// It could not run only because the run's first proven red stopped it: each of its NOT RUN
    /// rows carries the halt's words ([`crate::sched::Halt`]). The verdict counts it apart from
    /// what the machine kept from running.
    pub stopped: bool,
}

/// The most bytes a `check-finished` event spends on its check's detail ([`check_event`]).
pub const EVENT_DETAIL_BYTES: usize = 4096;
/// The most stage rows a `check-finished` event lists, and the most bytes each row's label, a
/// finding's id or the rerun command takes there.
pub const EVENT_ROWS: usize = 64;
/// See [`EVENT_ROWS`].
pub const EVENT_ROW_BYTES: usize = 512;

/// `text`'s longest prefix whose JSON string encoding takes at most `max` bytes, cut at a
/// character and ended with `…` when cut: what keeps a structured line within the bound its
/// readers hold every line to (the release's 64 KiB). A control character counts as its six-byte
/// `\u` escape, a quote or a backslash as two, any other character as its UTF-8 length.
#[must_use]
pub fn clip_json(text: &str, max: usize) -> String {
    if text.chars().map(json_char_bytes).sum::<usize>() <= max {
        return text.to_owned();
    }
    let room = max.saturating_sub('…'.len_utf8());
    let mut used = 0;
    let mut out = String::new();
    for c in text.chars() {
        used += json_char_bytes(c);
        if used > room {
            break;
        }
        out.push(c);
    }
    out.push('…');
    out
}

/// An upper bound on one character's JSON string encoding, excluding its quotes.
pub(crate) fn json_char_bytes(c: char) -> usize {
    match c {
        '"' | '\\' => 2,
        c if u32::from(c) < 0x20 => 6,
        c => c.len_utf8(),
    }
}

/// A finished check's `check-finished` event, one newline-terminated line: its outcome word, its
/// own time, its detail, its stage rows (each with its word), its findings' ids and the command
/// that reruns it. The release's cutter keeps one record per check from these (A19), so the line
/// is bounded well under the 64 KiB its readers hold a line to: the detail to
/// [`EVENT_DETAIL_BYTES`], [`EVENT_ROWS`] rows of [`EVENT_ROW_BYTES`] each, [`FINDINGS_SHOWN`]
/// findings, each part's JSON encoding counted ([`clip_json`]); the totals say what was left out.
#[must_use]
pub fn check_event(r: &CheckResult, reports: &[&Report]) -> String {
    let all: Vec<(Outcome, &str)> = reports.iter().flat_map(|rep| rep.outcomes()).collect();
    let rows: Vec<_> = all
        .iter()
        .take(EVENT_ROWS)
        .map(|(o, label)| {
            aterm_json::json!({"outcome": word(*o), "detail": clip_json(label, EVENT_ROW_BYTES)})
        })
        .collect();
    let findings: Vec<String> = r
        .findings
        .iter()
        .take(FINDINGS_SHOWN)
        .map(|f| clip_json(&f.id, EVENT_ROW_BYTES))
        .collect();
    format!(
        "verify-event: {}\n",
        crate::event_log::event_value(
            crate::event_log::EventKind::CheckFinished,
            aterm_json::json!({
                "check": r.check.name(),
                "outcome": word(r.outcome),
                "seconds": r.seconds,
                "detail": clip_json(&r.detail, EVENT_DETAIL_BYTES),
                "rows": rows,
                "rows_total": all.len(),
                "findings": findings,
                "findings_total": r.findings.len(),
                "rerun": clip_json(&r.rerun, EVENT_ROW_BYTES),
            })
        )
    )
}

/// The run's `run-finished` event, one newline-terminated line after its check results: its exit
/// code and each check it finished with its word, so a reader can tell a whole record from one
/// that lost a line (`verify land`, design B10). `run_cli` may append disk accounting to the
/// events file (`--events`) afterward.
#[must_use]
pub fn run_event(results: &[CheckResult], code: i32) -> String {
    let checks: Vec<_> = results
        .iter()
        .map(|r| aterm_json::json!({"check": r.check.name(), "outcome": word(r.outcome)}))
        .collect();
    format!(
        "verify-event: {}\n",
        crate::event_log::event_value(
            crate::event_log::EventKind::RunFinished,
            aterm_json::json!({
                "code": code,
                "checks": checks,
            })
        )
    )
}

/// The word a check's line opens with.
#[must_use]
pub const fn word(o: Outcome) -> &'static str {
    match o {
        Outcome::Ok => "ok",
        Outcome::Skip => "skip",
        Outcome::Fail(Severity::GateFailed) => "FAIL",
        Outcome::Fail(Severity::CouldNotRun) => "NOT RUN",
    }
}

/// Folds a check's stage reports into its [`CheckResult`] (worst outcome wins). `halt` is the
/// words the run's first proven red stopped the rest with, once it did: a check whose every NOT
/// RUN row says them was [`CheckResult::stopped`].
#[must_use]
pub fn judge(
    check: Check,
    reports: &[&Report],
    seconds: f64,
    scope: &str,
    rerun: String,
    halt: Option<&str>,
) -> CheckResult {
    let rows: Vec<(Outcome, &str)> = reports.iter().flat_map(|r| r.outcomes()).collect();
    let worst = [
        Outcome::Fail(Severity::GateFailed),
        Outcome::Fail(Severity::CouldNotRun),
        Outcome::Skip,
    ]
    .into_iter()
    .find(|o| rows.iter().any(|(r, _)| r == o))
    .unwrap_or(Outcome::Ok);
    let labels: Vec<&str> = rows
        .iter()
        .filter(|(o, _)| *o == worst)
        .map(|(_, l)| *l)
        .collect();
    let (detail, findings) = match worst {
        Outcome::Ok => (scope.to_string(), Vec::new()),
        Outcome::Fail(Severity::GateFailed) => {
            let mut findings: Vec<Finding> = reports.iter().flat_map(|r| r.findings()).collect();
            let detail = match findings.iter().filter(|f| f.rerun.is_some()).count() {
                0 => labels.first().copied().unwrap_or_default().to_string(),
                1 => "1 test failed".to_string(),
                n => format!("{n} tests failed"),
            };
            let unrun = Outcome::Fail(Severity::CouldNotRun);
            findings.extend(
                rows.iter()
                    .filter(|(o, _)| *o == unrun)
                    .map(|(_, l)| Finding {
                        id: format!("{} {l}", word(unrun)),
                        rerun: None,
                        message: Vec::new(),
                    }),
            );
            (detail, findings)
        }
        _ => {
            let detail = match (labels.as_slice(), worst) {
                ([], _) => String::new(),
                ([one], _) => (*one).to_string(),
                (all, Outcome::Skip) => format!("{} of its rows skipped:", all.len()),
                (all, _) => format!("{} of its rows could not run:", all.len()),
            };
            let reasons = if labels.len() > 1 {
                labels
                    .iter()
                    .map(|l| Finding {
                        id: (*l).to_string(),
                        rerun: None,
                        message: Vec::new(),
                    })
                    .collect()
            } else {
                Vec::new()
            };
            (detail, reasons)
        }
    };
    let stopped = worst == Outcome::Fail(Severity::CouldNotRun)
        && halt.is_some_and(|words| labels.iter().all(|l| l.contains(words)));
    CheckResult {
        check,
        outcome: worst,
        seconds,
        detail,
        findings,
        rerun,
        stopped,
    }
}

/// `1.2s`, `47s`, `3m05s`.
#[must_use]
pub fn duration(secs: f64) -> String {
    if !secs.is_finite() || secs <= 0.0 {
        return "0.0s".to_string();
    }
    if secs < 10.0 {
        format!("{secs:.1}s")
    } else if secs < 60.0 {
        format!("{secs:.0}s")
    } else {
        let whole = Duration::try_from_secs_f64(secs)
            .unwrap_or(Duration::MAX)
            .as_secs();
        format!("{}m{:02}s", whole / 60, whole % 60)
    }
}

/// Formats a check result line and any findings/rerun commands.
#[must_use]
pub fn render(r: &CheckResult, width: usize) -> String {
    let mut s = format!(
        "{:<7} {:<width$} {:>6}",
        word(r.outcome),
        r.check.name(),
        duration(r.seconds),
    );
    if !r.detail.is_empty() {
        let _ = write!(s, "  {}", r.detail);
    }
    s.push('\n');
    let pad = "        ";
    let shown = r.findings.iter().take(FINDINGS_SHOWN);
    for f in shown {
        // A finding that is the row the detail already names adds only its message.
        if f.id != r.detail {
            let _ = writeln!(s, "{pad}{}", f.id);
        }
        for line in &f.message {
            let _ = writeln!(s, "{pad}  {line}");
        }
        if let Some(cmd) = &f.rerun {
            let _ = writeln!(s, "{pad}  rerun: {cmd}");
        }
    }
    if r.findings.len() > FINDINGS_SHOWN {
        let _ = writeln!(
            s,
            "{pad}… {} more in the log",
            r.findings.len() - FINDINGS_SHOWN
        );
    }
    if matches!(r.outcome, Outcome::Fail(_)) {
        let _ = writeln!(s, "{pad}rerun: {}", r.rerun);
    }
    s
}

/// Formats the summary verdict line and returns the exit code. Beside a failure, the checks its
/// halt stopped ([`CheckResult::stopped`]) are counted as not run at the first red, apart from
/// what could not run.
#[must_use]
pub fn verdict(results: &[CheckResult], release: bool, seconds: f64) -> (String, i32) {
    let named = |o: Outcome| -> Vec<&str> {
        results
            .iter()
            .filter(|r| r.outcome == o)
            .map(|r| r.check.name())
            .collect()
    };
    let failed = named(Outcome::Fail(Severity::GateFailed));
    let unrun_of = |stopped: bool| -> Vec<&str> {
        results
            .iter()
            .filter(|r| r.outcome == Outcome::Fail(Severity::CouldNotRun))
            // Only a failure stops the rest: beside none, a stopped check is one that could not run.
            .filter(|r| (r.stopped && !failed.is_empty()) == stopped)
            .map(|r| r.check.name())
            .collect()
    };
    let unrun = unrun_of(false);
    let stopped = unrun_of(true);
    let skipped = named(Outcome::Skip);
    let ok = named(Outcome::Ok).len();
    let mut tail = vec![format!("{ok} ok")];
    if !skipped.is_empty() {
        tail.push(format!(
            "{} skipped ({})",
            skipped.len(),
            skipped.join(", ")
        ));
    }
    let tail = format!("{} in {}", tail.join(", "), duration(seconds));
    if !failed.is_empty() {
        let mut head = format!("{} failed", failed.join(", "));
        if !unrun.is_empty() {
            let _ = write!(head, "; {} could not run", unrun.join(", "));
        }
        if !stopped.is_empty() {
            let _ = write!(
                head,
                "; {} not run (stopped at the first red: {})",
                stopped.len(),
                stopped.join(", ")
            );
        }
        return (
            format!("verify: FAIL — {head}; {tail}"),
            crate::exit::FAILED,
        );
    }
    if !unrun.is_empty() {
        return (
            format!(
                "verify: COULD NOT RUN — {} could not run; {tail}",
                unrun.join(", ")
            ),
            crate::exit::COULD_NOT_RUN,
        );
    }
    if release && !skipped.is_empty() {
        return (
            format!(
                "verify: COULD NOT RUN — `release` needs every check, and {} skipped; {tail}",
                skipped.join(", ")
            ),
            crate::exit::COULD_NOT_RUN,
        );
    }
    (format!("verify: PASS — {tail}"), crate::exit::PASS)
}

/// The verdict once the checkout's HEAD was `from` when the run began and `to` when it ended: a
/// checkout that moved to another commit mid-run (another session's fast-forward of a shared
/// checkout) had its stages read both trees, so the run's own `line` and `code` hold for neither
/// and the run could not run. Either HEAD unread, or one HEAD throughout, keeps them.
#[must_use]
pub fn moved_checkout(
    (line, code): (String, i32),
    from: Option<&str>,
    to: Option<&str>,
) -> (String, i32) {
    fn short(sha: &str) -> &str {
        sha.get(..10).unwrap_or(sha)
    }
    match (from, to) {
        (Some(from), Some(to)) if from != to => (
            format!(
                "verify: COULD NOT RUN — the checkout moved from {} to {} while this ran, so its checks read both trees; rerun where nothing moves the checkout (a worktree of its own). The run's own verdict: {}",
                short(from),
                short(to),
                line.strip_prefix("verify: ").unwrap_or(&line)
            ),
            crate::exit::COULD_NOT_RUN,
        ),
        _ => (line, code),
    }
}

#[cfg(test)]
#[path = "report_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "report_check_event_tests.rs"]
mod check_event_tests;
