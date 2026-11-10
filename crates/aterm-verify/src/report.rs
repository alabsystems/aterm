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
            return format!("verify-event: {}", aterm_json::json!({
                "schema": 1, "event": "compile-unit-message-unavailable", "stage": stage,
                "detail": "duplicate fields in a unit message",
            }));
        };
        let identity = unit_identity(value);
        let unit = identity.and_then(|identity| units.get(identity)).unwrap_or(&unavailable);
        if value.get("reason").and_then(Value::as_str) == Some("compilation-unit-dispatched") {
            let reported = unit.joined().map(|_| true);
            return format!("verify-event: {}", aterm_json::json!({
                "schema": 1, "event": "compile-unit-dispatched", "stage": stage,
                "producer_unit_sha256": identity,
                "compile_kind": dispatch.map(|d| d.kind), "compile_mode": dispatch.map(|d| d.mode),
                "fresh": dispatch.map(|d| d.fresh), "dirty_reason": dispatch.and_then(|d| d.cause),
                "evidence_status": if dispatch.is_some() { "producer-dispatch" } else { "dispatch-unavailable" },
                "artifact_reported": reported,
            }));
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
        format!("verify-event: {}", aterm_json::json!({
            "schema": 1, "event": "compile-unit-finished", "stage": stage,
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
        }))
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
        aterm_json::json!({
            "schema": 1,
            "event": "stage-started",
            "stage_index": index,
            "stage": title,
            "started_at_unix_ms": at,
            "at_unix_ms": at,
            "seconds": 0.0,
            "cpu_s": null,
        })
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
        aterm_json::json!({
            "schema": 1,
            "event": "stage-finished",
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
        aterm_json::json!({
            "schema": 1,
            "event": if finished.is_some() { "row-finished" } else { "row-started" },
            "stage": stage, "row": row, "at_unix_ms": aterm_time::unix_millis(),
            "seconds": seconds.as_secs_f64(),
            "cpu_s": usage.map(|u| u.cpu.as_secs_f64()),
            "cpu_scope": "sampled-child-process-groups",
            "cpu_coverage": "lower-bound",
        })
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
            let value: aterm_json::Value = aterm_json::from_str(json).ok()?;
            matches!(
                value["event"].as_str(),
                Some("measure-row-started" | "measure-row-finished")
            )
            .then(|| line.to_owned())
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
        aterm_json::json!({
            "schema": 1,
            "event": "check-finished",
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
        aterm_json::json!({
            "schema": 1,
            "event": "run-finished",
            "code": code,
            "checks": checks,
        })
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
mod tests {
    /// Evidence hashes the linked executable. Wrong targets, a missing executable, and one
    /// changed harness byte cannot authorize reuse of the same measurement.
    #[test]
    fn measure_evidence_is_the_linked_harness_content() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let executable = dir.path().join("paint");
        std::fs::write(&executable, b"first linked harness").unwrap();
        let artifact = |kind: &str| aterm_json::json!({"reason":"compiler-artifact", "manifest_path":"/r/crates/aterm-conformance/Cargo.toml", "target":{"name":"paint","kind":[kind]}, "executable":executable});
        let wire = artifact("bench");
        let digest = |wire: &aterm_json::Value| {
            let line = super::compile_unit_events("compile", &wire.to_string())
                .pop()
                .unwrap();
            let event: aterm_json::Value =
                aterm_json::from_str(line.strip_prefix("verify-event: ").unwrap()).unwrap();
            event["measure_harness_sha256"].as_str().map(str::to_owned)
        };
        let first = digest(&wire).unwrap();
        std::fs::write(&executable, b"second linked harness").unwrap();
        assert_ne!(digest(&wire).unwrap(), first);
        assert!(digest(&artifact("test")).is_none());
        std::fs::remove_file(executable).unwrap();
        assert!(digest(&wire).is_none());
    }
    use super::*;

    #[test]
    fn provisional_messages_expose_only_strict_bounded_metadata() {
        let capture = include_str!("../tests/fixtures/targo-unit-dispatch-v1.jsonl");
        let dispatch = capture.lines().next().unwrap();
        let artifact = capture.lines().nth(1).unwrap();
        for line in [dispatch, artifact] {
            let wire = provisional_compile_event("test compile", line).unwrap();
            let value: aterm_json::Value =
                aterm_json::from_str(wire.strip_prefix("verify-event: ").unwrap()).unwrap();
            assert_eq!(value["provisional"].as_bool(), Some(true));
            assert_eq!(value["sample_complete"].as_bool(), Some(false));
            for key in [
                "fresh",
                "dirty_reason",
                "producer_unit_sha256",
                "manifest_path",
                "package_id",
                "executable",
                "artifact_reported",
            ] {
                assert!(value.get(key).is_none(), "{key}");
            }
            assert!(!wire.contains("/Users/"));
        }
        for wire in [
            dispatch.replace("\"build\"", "\"unknown-mode\""),
            dispatch.replace("\"host\"", "\"unknown-kind\""),
            dispatch.replace("\"changed-env\"", "\"unknown-cause\""),
            dispatch.replace("\"reason\":", "\"extra\":true,\"reason\":"),
            dispatch.replace("\"fresh\":false,", ""),
            dispatch.replace(
                "3585558e13bc333691c3e05830d0ce2175baa184bf4ff01aa9f76465b5784241",
                "invalid",
            ),
            dispatch.replace("\"reason\":", "\"reason\":\"build-finished\",\"reason\":"),
            artifact.replace(
                "\"name\":\"dispatch_serialization_probe\"",
                "\"name\":\"/private/path\"",
            ),
            artifact.replace("\"kind\":[\"lib\"]", "\"kind\":[\"unrecognized\"]"),
            artifact.replace("\"target\":{", "\"target\":{\"name\":\"duplicate\","),
            artifact.replace("\"features\":[]", "\"features\":[{\"x\":1,\"x\":2}]"),
        ] {
            assert!(provisional_compile_event("test compile", &wire).is_none());
        }
    }

    #[test]
    fn duplicate_wire_fields_cannot_establish_a_unit_cause_or_artifact_match() {
        let capture = include_str!("../tests/fixtures/targo-unit-dispatch-v1.jsonl");
        let dispatch = capture.lines().next().unwrap();
        let artifact = capture.lines().nth(1).unwrap();
        for (name, first, second) in [
            (
                "dispatch cause",
                dispatch.replace(
                    "\"dirty_reason\":\"changed-env\"",
                    "\"dirty_reason\":\"forced\",\"dirty_reason\":\"changed-env\"",
                ),
                artifact.to_owned(),
            ),
            (
                "artifact freshness",
                dispatch.to_owned(),
                artifact.replace("\"fresh\":false", "\"fresh\":true,\"fresh\":false"),
            ),
            (
                "artifact reason",
                dispatch.to_owned(),
                artifact.replacen(
                    "\"reason\":\"compiler-artifact\"",
                    "\"reason\":\"build-finished\",\"reason\":\"compiler-artifact\"",
                    1,
                ),
            ),
            (
                "hidden artifact reason",
                dispatch.to_owned(),
                artifact.replacen(
                    "\"reason\":\"compiler-artifact\"",
                    "\"reason\":\"compiler-artifact\",\"reason\":\"build-finished\"",
                    1,
                ),
            ),
        ] {
            let events = compile_unit_events("ambiguous wire", &format!("{first}\n{second}\n"));
            let values: Vec<aterm_json::Value> = events
                .iter()
                .map(|line| {
                    aterm_json::from_str::<aterm_json::Value>(
                        line.strip_prefix("verify-event: ").unwrap(),
                    )
                    .unwrap()
                })
                .collect();
            assert_eq!(values.len(), 2, "{name}: {events:?}");
            assert!(
                values.iter().all(|event| event
                    .get("rebuild_reason")
                    .is_none_or(|reason| *reason == aterm_json::Value::Null)),
                "{name}: {events:?}"
            );
            assert!(
                values.iter().all(|event| event
                    .get("artifact_reported")
                    .is_none_or(|reported| *reported == aterm_json::Value::Null)),
                "{name}: {events:?}"
            );
        }

        // An ambiguous identity field can name two units. Invalidate both exact observed
        // identities without erasing an independent third unit's valid evidence.
        let x = aterm_json::from_str::<aterm_json::Value>(dispatch).unwrap();
        let x = x["trust_unit_identity_sha256"].as_str().unwrap();
        let y = "b".repeat(64);
        let z = "c".repeat(64);
        let ambiguous = dispatch.replacen(
            &format!("\"trust_unit_identity_sha256\":\"{x}\""),
            &format!(
                "\"trust_unit_identity_sha256\":\"{x}\",\"trust_unit_identity_sha256\":\"{y}\""
            ),
            1,
        );
        assert_ne!(ambiguous, dispatch);
        let output = [
            dispatch.to_owned(),
            artifact.to_owned(),
            producer_dispatch(&y).to_string(),
            producer_artifact(&y).to_string(),
            producer_dispatch(&z).to_string(),
            producer_artifact(&z).to_string(),
            ambiguous.clone(),
        ]
        .join("\n");
        let events: Vec<aterm_json::Value> = compile_unit_events("scoped ambiguity", &output)
            .iter()
            .map(|line| {
                aterm_json::from_str::<aterm_json::Value>(
                    line.strip_prefix("verify-event: ").unwrap(),
                )
                .unwrap()
            })
            .collect();
        for identity in [x, y.as_str()] {
            let affected: Vec<_> = events
                .iter()
                .filter(|event| event["producer_unit_sha256"].as_str() == Some(identity))
                .collect();
            assert_eq!(affected.len(), 2);
            assert!(affected.iter().all(|event| {
                event
                    .get("rebuild_reason")
                    .is_none_or(|v| *v == aterm_json::Value::Null)
                    && event
                        .get("artifact_reported")
                        .is_none_or(|v| *v == aterm_json::Value::Null)
            }));
        }
        let independent = events
            .iter()
            .find(|event| {
                event["producer_unit_sha256"].as_str() == Some(z.as_str())
                    && event["event"].as_str() == Some("compile-unit-finished")
            })
            .unwrap();
        assert_eq!(independent["rebuild_reason"].as_str(), Some("changed-file"));
        assert_eq!(
            events
                .iter()
                .filter(|event| event["event"].as_str() == Some("compile-unit-message-unavailable"))
                .count(),
            1
        );

        let no_identity = ambiguous.replace(x, "invalid").replace(&y, "invalid-too");
        let output = [
            producer_dispatch(&z).to_string(),
            producer_artifact(&z).to_string(),
            no_identity,
        ]
        .join("\n");
        let events = compile_unit_events("unidentified ambiguity", &output);
        assert!(
            events
                .iter()
                .any(|event| event.contains("\"rebuild_reason\":\"changed-file\""))
        );
        // No taint survives this invocation.
        let healthy = compile_unit_events("healthy", capture);
        assert!(
            healthy
                .iter()
                .any(|event| event.contains("\"rebuild_reason\":\"changed-env\""))
        );
    }

    #[test]
    fn decoded_ambiguous_artifact_kind_preserves_every_identity_taint() {
        let x = "a".repeat(64);
        let y = "b".repeat(64);
        let z = "c".repeat(64);
        let ambiguous = format!(
            r#"{{"re\u0061son":"compiler-\u0061rtifact","trust_unit_identity_sha256":"{x}","trust_unit_identity_sha256":"{y}"}}"#
        );
        let output = [
            producer_dispatch(&x).to_string(),
            producer_artifact(&x).to_string(),
            producer_dispatch(&y).to_string(),
            producer_artifact(&y).to_string(),
            producer_dispatch(&z).to_string(),
            producer_artifact(&z).to_string(),
            ambiguous,
        ]
        .join("\n");
        let events: Vec<aterm_json::Value> =
            compile_unit_events("escaped ambiguous artifact", &output)
                .iter()
                .map(|line| {
                    aterm_json::from_str(line.strip_prefix("verify-event: ").unwrap()).unwrap()
                })
                .collect();
        for identity in [&x, &y, &z] {
            let event = events
                .iter()
                .find(|event| {
                    event["event"].as_str() == Some("compile-unit-finished")
                        && event["producer_unit_sha256"].as_str() == Some(identity.as_str())
                })
                .unwrap();
            if identity == &z {
                assert_eq!(event["rebuild_reason"].as_str(), Some("changed-file"));
            } else {
                assert!(event["rebuild_reason"].is_null());
                assert_eq!(
                    event["rebuild_reason_status"].as_str(),
                    Some("ambiguous-message")
                );
            }
        }
        assert_eq!(crate::compile_account::read(&output).units.unknown, 1);
    }

    fn producer_artifact(identity: &str) -> aterm_json::Value {
        aterm_json::json!({
            "reason": "compiler-artifact", "trust_unit_identity_sha256": identity,
            "trust_compile_mode": "test", "trust_compile_kind": "host", "fresh": false,
        })
    }

    // The producer's bounded targo.compilation-unit-dispatched.v1 wire envelope.
    fn producer_dispatch(identity: &str) -> aterm_json::Value {
        aterm_json::json!({
            "reason": "compilation-unit-dispatched", "schema": "targo.compilation-unit-dispatched.v1",
            "trust_unit_identity_sha256": identity, "trust_compile_mode": "test",
            "trust_compile_kind": "host", "fresh": false, "dirty_reason": "changed-file",
        })
    }

    fn retained_units(messages: &[aterm_json::Value]) -> Vec<aterm_json::Value> {
        let output = messages
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        compile_unit_events("compile", &output)
            .iter()
            .map(|line| {
                aterm_json::from_str::<aterm_json::Value>(
                    line.strip_prefix("verify-event: ").unwrap(),
                )
                .unwrap()
            })
            .collect()
    }

    #[test]
    fn compile_units_replay_a_real_producer_pair_but_do_not_invent_its_missing_dispatch() {
        // Exact records 1 and 3 from the controlled producer capture documented in
        // docs/measured/compiler-unit-evidence-2026-11-04.md, without path redaction.
        let capture = include_str!("../tests/fixtures/targo-unit-dispatch-v1.jsonl");
        let events: Vec<aterm_json::Value> = compile_unit_events("captured producer", capture)
            .iter()
            .map(|line| {
                aterm_json::from_str::<aterm_json::Value>(
                    line.strip_prefix("verify-event: ").unwrap(),
                )
                .unwrap()
            })
            .collect();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["artifact_reported"].as_bool(), Some(true));
        assert_eq!(events[1]["rebuild_reason"].as_str(), Some("changed-env"));
        assert_eq!(
            events[0]["producer_unit_sha256"],
            events[1]["producer_unit_sha256"]
        );
        assert_eq!(events[1]["fresh"].as_bool(), Some(false));
        let artifact = capture.lines().nth(1).unwrap();
        let without = compile_unit_events("captured artifact only", artifact);
        let without: aterm_json::Value =
            aterm_json::from_str(without[0].strip_prefix("verify-event: ").unwrap()).unwrap();
        assert_eq!(without["rebuild_reason"], aterm_json::Value::Null);
        assert_eq!(
            without["rebuild_reason_status"].as_str(),
            Some("not-reported-by-producer")
        );
    }

    #[test]
    fn nested_wire_ambiguity_taints_an_otherwise_matching_unit() {
        let capture = include_str!("../tests/fixtures/targo-unit-dispatch-v1.jsonl");
        let artifact = capture.lines().nth(1).unwrap();
        let ambiguous = artifact.replacen('{', r#"{"extra":{"nested":[{"key":1,"key":2}]},"#, 1);
        let events = compile_unit_events("nested ambiguity", &format!("{capture}\n{ambiguous}"));
        let finished: Vec<aterm_json::Value> = events
            .iter()
            .map(|line| aterm_json::from_str(line.strip_prefix("verify-event: ").unwrap()).unwrap())
            .collect();
        assert_eq!(
            finished
                .iter()
                .filter(|event| event["event"].as_str() == Some("compile-unit-finished"))
                .count(),
            1
        );
        assert!(
            finished
                .iter()
                .any(|event| event["event"].as_str() == Some("compile-unit-message-unavailable"))
        );
        assert!(
            finished
                .iter()
                .filter(|event| event["event"].as_str() == Some("compile-unit-finished"))
                .all(|event| event["rebuild_reason"].is_null()
                    && event["rebuild_reason_status"].as_str() == Some("ambiguous-message"))
        );
    }

    #[test]
    fn compile_units_join_only_matching_producer_dispatches_in_this_call() {
        let first = "a".repeat(64);
        let second = "b".repeat(64);
        let a = producer_dispatch(&first);
        let mut b = producer_dispatch(&second);
        b.as_object_mut()
            .unwrap()
            .insert("dirty_reason".into(), aterm_json::json!("features-changed"));
        let events = retained_units(&[
            a.clone(),
            b,
            producer_artifact(&second),
            producer_artifact(&first),
            a,
        ]);
        assert_eq!(
            events[2]["rebuild_reason"].as_str(),
            Some("features-changed")
        );
        assert_eq!(events[3]["rebuild_reason"].as_str(), Some("changed-file"));
        assert_eq!(events[0]["artifact_reported"].as_bool(), Some(true));

        // A dispatched build script need not emit any compiler artifact. Neither a dispatch
        // nor a later invocation's artifact establishes completion in this invocation.
        let mut script = producer_dispatch(&first);
        script.as_object_mut().unwrap().insert(
            "trust_compile_mode".into(),
            aterm_json::json!("run-custom-build"),
        );
        assert_eq!(
            retained_units(&[script])[0]["artifact_reported"],
            aterm_json::Value::Null
        );
        assert_eq!(
            retained_units(&[producer_artifact(&first)])[0]["rebuild_reason"],
            aterm_json::Value::Null
        );
    }

    #[test]
    fn compile_units_refuse_mismatched_or_conflicting_dispatch_evidence() {
        use aterm_json::{Value, json};
        let identity = "a".repeat(64);
        for (field, invalid) in [
            ("schema", json!("targo.compilation-unit-dispatched.v2")),
            ("trust_unit_identity_sha256", json!("b".repeat(64))),
            ("trust_unit_identity_sha256", json!("invalid")),
            ("trust_compile_mode", json!("build")),
            ("trust_compile_kind", json!("target")),
            ("fresh", json!(true)),
            ("dirty_reason", json!("unsupported-sensitive-prose")),
            ("extra", json!("unsupported-sensitive-prose")),
        ] {
            let mut dispatch = producer_dispatch(&identity);
            dispatch
                .as_object_mut()
                .unwrap()
                .insert(field.into(), invalid);
            let events = retained_units(&[dispatch, producer_artifact(&identity)]);
            assert_eq!(
                events[1]["rebuild_reason"],
                Value::Null,
                "{field}: {events:?}"
            );
            assert_eq!(
                events[0]["artifact_reported"],
                Value::Null,
                "{field}: {events:?}"
            );
            assert!(
                !events[0]
                    .to_string()
                    .contains("unsupported-sensitive-prose")
            );
        }
        let mut conflicting = producer_dispatch(&identity);
        conflicting
            .as_object_mut()
            .unwrap()
            .insert("dirty_reason".into(), json!("forced"));
        let events = retained_units(&[
            producer_dispatch(&identity),
            conflicting,
            producer_artifact(&identity),
        ]);
        assert_eq!(events[2]["rebuild_reason"], Value::Null);
        assert_eq!(
            events[2]["rebuild_reason_status"].as_str(),
            Some("conflicting-dispatch")
        );
        assert_eq!(events[0]["artifact_reported"], Value::Null);

        let mut conflicting = producer_artifact(&identity);
        conflicting
            .as_object_mut()
            .unwrap()
            .insert("trust_compile_kind".into(), json!("target"));
        let events = retained_units(&[
            producer_dispatch(&identity),
            producer_artifact(&identity),
            conflicting,
        ]);
        assert_eq!(events[1]["rebuild_reason"], Value::Null);
        assert_eq!(events[0]["artifact_reported"], Value::Null);
    }

    #[test]
    fn compile_units_distinguish_fresh_work_unknown_causes_and_fresh_build_classification() {
        use aterm_json::{Value, json};
        let identity = "a".repeat(64);
        let mut dispatch = producer_dispatch(&identity);
        dispatch
            .as_object_mut()
            .unwrap()
            .insert("dirty_reason".into(), json!("unknown"));
        let events = retained_units(&[dispatch.clone(), producer_artifact(&identity)]);
        assert_eq!(events[1]["rebuild_reason"], Value::Null);
        assert_eq!(
            events[1]["rebuild_reason_status"].as_str(),
            Some("producer-unknown")
        );
        for cause in ["fresh-build", "output-integrity"] {
            dispatch
                .as_object_mut()
                .unwrap()
                .insert("dirty_reason".into(), json!(cause));
            assert_eq!(retained_units(&[dispatch.clone(), producer_artifact(&identity)])[1]["rebuild_reason"].as_str(), Some(cause));
        }
        dispatch
            .as_object_mut()
            .unwrap()
            .insert("fresh".into(), json!(true));
        dispatch
            .as_object_mut()
            .unwrap()
            .insert("dirty_reason".into(), Value::Null);
        let mut artifact = producer_artifact(&identity);
        artifact
            .as_object_mut()
            .unwrap()
            .insert("fresh".into(), json!(true));
        let events = retained_units(&[dispatch, artifact]);
        assert_eq!(events[1]["rebuild_reason"], Value::Null);
        assert_eq!(
            events[1]["rebuild_reason_status"].as_str(),
            Some("not-applicable")
        );
        assert_eq!(events[0]["artifact_reported"].as_bool(), Some(true));
    }

    #[test]
    fn compile_units_retain_producer_context_without_inventing_a_rebuild_cause() {
        let unit = aterm_json::json!({
            "reason": "compiler-artifact", "package_id": "path+file:///r/lib#1",
            "manifest_path": "/r/lib/Cargo.toml",
            "target": {"name": "lib", "kind": ["lib"], "src_path": "/r/lib/src/lib.rs", "extra": "unreported"},
            "profile": {"opt_level": "0", "test": true}, "features": ["a"],
            "trust_compile_target": "aarch64-apple-darwin", "trust_compile_kind": "host",
            "trust_compile_mode": "test", "trust_unit_identity_sha256": "a".repeat(64),
            "fresh": false, "dirty_reason": "source-changed", "extra": "unreported",
        });
        let events = compile_unit_events("test compile", &unit.to_string());
        assert_eq!(events.len(), 1);
        let event: aterm_json::Value =
            aterm_json::from_str(events[0].strip_prefix("verify-event: ").unwrap()).unwrap();
        assert_eq!(event["package_id"], unit["package_id"]);
        assert_eq!(event["target"]["name"], unit["target"]["name"]);
        assert_eq!(event["profile"], unit["profile"]);
        assert_eq!(event["features"], unit["features"]);
        assert_eq!(
            event["producer_unit_sha256"],
            unit["trust_unit_identity_sha256"]
        );
        assert_eq!(event["compile_target"], unit["trust_compile_target"]);
        assert_eq!(event["fresh"].as_bool(), Some(false));
        assert_eq!(event["rebuild_reason"], aterm_json::Value::Null);
        assert_eq!(
            event["rebuild_reason_status"].as_str(),
            Some("not-reported-by-producer")
        );
        assert!(!events[0].contains("unreported"));
        assert!(!events[0].contains("source-changed"));
    }

    #[test]
    fn compile_units_keep_unknown_freshness_distinct_from_a_rebuild() {
        let output = concat!(
            "not JSON\n{broken\n",
            "{\"reason\":\"build-finished\",\"fresh\":false}\n",
            "{\"reason\":\"compiler-artifact\",\"fresh\":\"false\",\"trust_unit_identity_sha256\":\"invalid\"}\n",
            "{\"reason\":\"compiler-artifact\",\"fresh\":true}\n",
        );
        let events = compile_unit_events("compile", output);
        assert_eq!(events.len(), 2);
        let read = |line: &str| {
            aterm_json::from_str::<aterm_json::Value>(line.strip_prefix("verify-event: ").unwrap())
                .unwrap()
        };
        let unknown = read(&events[0]);
        assert_eq!(unknown["fresh"], aterm_json::Value::Null);
        assert_eq!(unknown["producer_unit_sha256"], aterm_json::Value::Null);
        assert_eq!(
            unknown["rebuild_reason_status"].as_str(),
            Some("freshness-unknown")
        );
        assert_eq!(
            read(&events[1])["rebuild_reason_status"].as_str(),
            Some("not-applicable")
        );
    }

    #[test]
    fn a_started_stage_carries_its_index_without_claiming_a_result() {
        let line = stage_started(7, "compile \"helper\"");
        let event: aterm_json::Value =
            aterm_json::from_str(line.strip_prefix("verify-event: ").unwrap()).unwrap();
        assert_eq!(event["event"].as_str(), Some("stage-started"));
        assert_eq!(event["stage_index"].as_u64(), Some(7));
        assert_eq!(event["stage"].as_str(), Some("compile \"helper\""));
        assert!(event["started_at_unix_ms"].as_u64().is_some());
        assert_eq!(event["at_unix_ms"], event["started_at_unix_ms"]);
        assert_eq!(event["seconds"].as_f64(), Some(0.0));
        assert!(event["cpu_s"].is_null());
        assert!(event.get("decisions").is_none());
        assert!(event.get("outcome").is_none());
    }

    #[test]
    fn live_stage_event_keeps_findings_and_unavailable_checks_distinct() {
        let mut report = Report::new("measurement");
        report.fail("throughput below floor");
        report.cannot_run("window never focused");
        let line = stage_event(
            7,
            Some(Check::Measure),
            &report,
            Duration::from_secs(12),
            Duration::from_secs(3),
            None,
        );
        let event: aterm_json::Value =
            aterm_json::from_str(line.strip_prefix("verify-event: ").unwrap()).unwrap();
        assert_eq!(event["waiting_s"].as_f64(), Some(12.0));
        assert_eq!(event["stage_index"].as_u64(), Some(7));
        assert_eq!(event["execution_s"].as_f64(), Some(3.0));
        assert_eq!(event["seconds"].as_f64(), Some(3.0));
        assert_eq!(event["at_unix_ms"], event["ended_at_unix_ms"]);
        // When it ended, so a release's timeline dates a stage's red where it happened.
        assert!(
            event["ended_at_unix_ms"].as_u64().is_some_and(|ms| ms > 0),
            "{event}"
        );
        assert!(event["child_cpu"].is_null());
        assert!(event["cpu_s"].is_null());
        assert_eq!(event["decisions"][0]["outcome"].as_str(), Some("FAIL"));
        assert_eq!(event["decisions"][1]["outcome"].as_str(), Some("NOT RUN"));
    }

    #[test]
    fn row_events_keep_wall_cpu_and_unread_cpu_distinct() {
        let read = |line: String| {
            aterm_json::from_str::<aterm_json::Value>(line.strip_prefix("verify-event: ").unwrap())
                .unwrap()
        };
        let start = read(row_event("objc drives", "ime", None));
        assert_eq!(start["event"].as_str(), Some("row-started"));
        assert_eq!(start["seconds"].as_f64(), Some(0.0));
        assert!(start["cpu_s"].is_null());
        let end = read(row_event(
            "objc drives",
            "ime",
            Some((
                Duration::from_secs(3),
                Some(crate::exec::TreeUsage {
                    cpu: Duration::from_secs(2),
                    ..Default::default()
                }),
            )),
        ));
        assert_eq!(end["event"].as_str(), Some("row-finished"));
        assert_eq!(end["seconds"].as_f64(), Some(3.0));
        assert_eq!(end["cpu_s"].as_f64(), Some(2.0));
        assert_eq!(end["stage"].as_str(), Some("objc drives"));
        assert_eq!(end["row"].as_str(), Some("ime"));
        assert!(end["at_unix_ms"].as_u64().is_some());
        let absent = read(row_event(
            "objc drives",
            "ime",
            Some((Duration::from_secs(3), None)),
        ));
        assert!(absent["cpu_s"].is_null());
    }

    #[test]
    fn only_measurement_row_events_are_forwarded_with_original_timestamps() {
        let row =
            "verify-event: {\"event\":\"measure-row-finished\",\"at_unix_ms\":123,\"seconds\":2}";
        let text = format!(
            "ordinary output\nverify-event: broken\nverify-event: {{\"event\":\"stage-finished\"}}\n{row}\n"
        );
        assert_eq!(measurement_events(&text), [row]);
    }

    #[test]
    fn observed_cpu_is_a_separate_lower_bound_not_execution_wall_time() {
        let line = stage_event(
            8,
            None,
            &Report::new("parallel build"),
            Duration::from_secs(12),
            Duration::from_secs(3),
            Some(crate::memory::ObservedCpu {
                nanoseconds: 8_000_000_000,
                processes: 4,
            }),
        );
        let event: aterm_json::Value =
            aterm_json::from_str(line.strip_prefix("verify-event: ").unwrap()).unwrap();
        assert_eq!(event["child_cpu"]["observed_s"].as_f64(), Some(8.0));
        assert_eq!(event["child_cpu"]["observed_processes"].as_u64(), Some(4));
        assert_eq!(event["child_cpu"]["coverage"].as_str(), Some("lower-bound"));
        assert_eq!(event["execution_s"].as_f64(), Some(3.0));
    }

    fn result(check: Check, outcome: Outcome) -> CheckResult {
        CheckResult {
            check,
            outcome,
            seconds: 1.0,
            detail: String::new(),
            findings: Vec::new(),
            rerun: format!("targo --unverified verify {}", check.name()),
            stopped: false,
        }
    }

    #[test]
    fn the_worst_row_decides_and_a_failed_test_is_its_own_finding() {
        let mut lint = Report::new("tippy");
        lint.pass("tippy -p a");
        let r = judge(Check::Lint, &[&lint], 2.0, "-p a", "rerun".into(), None);
        assert_eq!(r.outcome, Outcome::Ok);

        let mut skipped = Report::new("doc");
        skipped.skip("targo test --doc (no library)");
        let r = judge(Check::Doc, &[&skipped], 0.0, "-p a", "rerun".into(), None);
        assert_eq!(r.outcome, Outcome::Skip);
        skipped.skip("another (no tool)");
        let r = judge(Check::Doc, &[&skipped], 0.0, "-p a", "rerun".into(), None);
        assert_eq!(r.findings.len(), 2, "each reason listed under it");

        let mut test = Report::new("test");
        test.pass("compile");
        test.fail("a row");
        test.cannot_run("no disk");
        let r = judge(Check::Test, &[&test], 0.0, "", "rerun".into(), None);
        assert_eq!(r.outcome, Outcome::Fail(Severity::GateFailed));
        let ids: Vec<&str> = r.findings.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(
            ids,
            ["a row", "NOT RUN no disk"],
            "the failure, then what could not run beside it under its own word"
        );
        assert_eq!(r.detail, "a row");
        assert_eq!(duration(0.0), "0.0s");
        assert_eq!(duration(-1.5), "0.0s");
        assert_eq!(duration(f64::NAN), "0.0s");
        assert!(!duration(f64::INFINITY).is_empty());
    }

    #[test]
    fn the_exit_code_is_the_worst_check_and_a_release_skip_is_could_not_run() {
        let ok = result(Check::Fmt, Outcome::Ok);
        let skip = result(Check::Doc, Outcome::Skip);
        let fail = result(Check::Lint, Outcome::Fail(Severity::GateFailed));
        let unrun = result(Check::Test, Outcome::Fail(Severity::CouldNotRun));
        let code = |results: &[CheckResult], release: bool| verdict(results, release, 1.0).1;
        assert_eq!(code(&[ok.clone(), skip.clone()], false), crate::exit::PASS);
        assert_eq!(
            code(&[ok.clone(), skip], true),
            crate::exit::COULD_NOT_RUN,
            "`release` needs every check"
        );
        assert_eq!(
            code(&[ok.clone(), fail, unrun.clone()], false),
            crate::exit::FAILED
        );
        assert_eq!(code(&[ok, unrun], false), crate::exit::COULD_NOT_RUN);
        assert_eq!(duration(f64::NAN), "0.0s");
        assert_eq!(duration(-1.0), "0.0s");
        assert_eq!(duration(f64::INFINITY), "0.0s");
        assert!(duration(1e30).ends_with("15s"));
        assert_eq!(duration(185.0), "3m05s");
    }

    #[test]
    fn a_check_the_first_red_stopped_is_counted_apart_from_what_could_not_run() {
        let words = "stopped: the release failed at fmt";
        let mut halted = Report::new("tippy");
        halted.cannot_run(format!("tippy — {words}"));
        let mut machine = Report::new("doc");
        machine.cannot_run("targo test --doc — no disk");
        let lint = judge(
            Check::Lint,
            &[&halted],
            0.0,
            "",
            "rerun".into(),
            Some(words),
        );
        let doc = judge(
            Check::Doc,
            &[&machine],
            0.0,
            "",
            "rerun".into(),
            Some(words),
        );
        assert!(lint.stopped, "every NOT RUN row says the halt's words");
        assert!(
            !doc.stopped,
            "the machine kept it from running, not the halt"
        );
        let fmt = result(Check::Fmt, Outcome::Fail(Severity::GateFailed));
        let (line, code) = verdict(&[fmt, lint.clone(), doc], true, 1.0);
        assert_eq!(code, crate::exit::FAILED);
        assert_eq!(
            line,
            "verify: FAIL — fmt failed; doc could not run; \
             1 not run (stopped at the first red: lint); 0 ok in 1.0s"
        );
        let (line, code) = verdict(&[lint], true, 1.0);
        assert_eq!(
            (line.as_str(), code),
            (
                "verify: COULD NOT RUN — lint could not run; 0 ok in 1.0s",
                crate::exit::COULD_NOT_RUN
            ),
            "beside no failure nothing was stopped at a red"
        );
        let bare = judge(Check::Lint, &[&halted], 0.0, "", "rerun".into(), None);
        assert!(!bare.stopped, "a run with no halt stopped nothing");
    }

    #[test]
    fn a_checkout_that_moved_mid_run_could_not_run_whatever_its_checks_said() {
        let fail = result(Check::Cells, Outcome::Fail(Severity::GateFailed));
        let ran = verdict(&[fail], false, 1.0);
        let (line, code) = moved_checkout(ran.clone(), Some("fe2021965c11"), Some("b46f0cf64d22"));
        assert_eq!(code, crate::exit::COULD_NOT_RUN);
        assert!(
            line.contains("moved from fe2021965c to b46f0cf64d") && line.contains("cells failed"),
            "names both commits and keeps the run's own verdict: {line}"
        );
        let pass = verdict(&[result(Check::Fmt, Outcome::Ok)], false, 1.0);
        assert_eq!(
            moved_checkout(pass, Some("a"), Some("b")).1,
            crate::exit::COULD_NOT_RUN,
            "a pass on two trees is no pass"
        );

        assert_eq!(
            moved_checkout(ran.clone(), Some("fe2021965c11"), Some("fe2021965c11")),
            ran,
            "one HEAD throughout keeps the run's verdict"
        );
        assert_eq!(
            moved_checkout(ran.clone(), None, Some("b46f0cf64d22")),
            ran,
            "an unread HEAD is no move"
        );
    }
}

#[cfg(test)]
mod check_event_tests {
    use super::*;

    fn line_json(line: &str) -> aterm_json::Value {
        let json = line
            .strip_prefix("verify-event: ")
            .and_then(|l| l.strip_suffix('\n'))
            .expect("one newline-terminated verify-event line");
        assert!(!json.contains('\n'), "a single line");
        aterm_json::from_slice(json.as_bytes()).unwrap()
    }

    /// A check's `check-finished` event names its outcome by the word its line opens with, with
    /// its rows, and stays far under the release reader's 64 KiB line limit however long and
    /// however escaped what it carries is. Negative control: [`clip_json`] counting each
    /// character as one byte lets the escaped rows run past the bound.
    #[test]
    fn a_check_finished_event_is_one_bounded_line_whatever_its_rows_say() {
        let mut report = Report::new("Test");
        report.pass("compiled");
        report.cannot_run("the disk ran out");
        let result = judge(
            Check::Test,
            &[&report],
            12.5,
            "",
            "verify test --all".into(),
            None,
        );
        let event = line_json(&check_event(&result, &[&report]));
        assert_eq!(event["event"].as_str(), Some("check-finished"));
        assert_eq!(event["check"].as_str(), Some("test"));
        assert_eq!(event["outcome"].as_str(), Some("NOT RUN"));
        assert_eq!(event["seconds"].as_f64(), Some(12.5));
        assert_eq!(event["rows"][1]["outcome"].as_str(), Some("NOT RUN"));
        assert_eq!(event["rows_total"].as_u64(), Some(2));

        let mut worst = Report::new("Lint");
        let hostile = "\u{1}\"\\".repeat(10_000);
        for _ in 0..1_000 {
            worst.fail(hostile.clone());
        }
        let mut result = judge(Check::Lint, &[&worst], 1.0, "", hostile.clone(), None);
        result.detail.clone_from(&hostile);
        for _ in 0..100 {
            result.findings.push(crate::ladder::Finding {
                id: hostile.clone(),
                rerun: None,
                message: vec![hostile.clone()],
            });
        }
        let line = check_event(&result, &[&worst]);
        assert!(line.len() < 48 * 1024, "{} bytes", line.len());
        let event = line_json(&line);
        assert_eq!(event["outcome"].as_str(), Some("FAIL"));
        assert_eq!(event["rows"].as_array().map(Vec::len), Some(EVENT_ROWS));
        assert_eq!(event["rows_total"].as_u64(), Some(1_000));
        assert_eq!(
            event["findings"].as_array().map(Vec::len),
            Some(FINDINGS_SHOWN)
        );
        assert!(event["detail"].as_str().unwrap().ends_with('…'));
        assert_eq!(clip_json("short", 512), "short");
    }
}
