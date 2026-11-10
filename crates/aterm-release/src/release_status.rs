// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read-only release progress, including failures emitted before verification ends.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

const MAX_OPEN_STAGES: usize = 256;

use crate::ledger::{Error, GitCli};

struct OpenStage {
    title: String,
    started_at_unix_ms: Option<u64>,
}

#[derive(Default)]
struct Events {
    first_failure: Option<aterm_json::Value>,
    latest: Option<aterm_json::Value>,
    latest_release_stage: Option<aterm_json::Value>,
    count: u64,
    rejected_lines: u64,
    incomplete_tail: bool,
    stage_activity_observed: bool,
    open_stages: BTreeMap<u64, OpenStage>,
}

fn valid_stage_title(title: &str) -> bool {
    !title.is_empty() && title.len() <= 4096 && !title.chars().any(char::is_control)
}

impl Events {
    fn observe_failure(&mut self, event: &aterm_json::Value) {
        if self.first_failure.is_none() && failed(event) {
            self.first_failure = Some(event.clone());
        }
        let Some(first) = self.first_failure.as_mut() else {
            return;
        };
        if first["event"].as_str() != Some("test-case-failed")
            || first["binary_completed"].as_bool() != Some(false)
            || event["event"].as_str() != Some("test-binary-finished")
        {
            return;
        }
        let Some(test) = first["test"].as_str().filter(|test| !test.is_empty()) else {
            return;
        };
        if event["test"].as_str() != Some(test) {
            return;
        }
        // Keep the first case and its observation time. Completion enriches that
        // failure rather than replacing it with a later success or another case.
        let fields = first.as_object_mut().expect("a failure event is an object");
        if let Some(detail) = fields.get("detail").cloned() {
            fields.insert("initial_detail".into(), detail);
        }
        fields.insert("binary_completed".into(), true.into());
        fields.insert("binary_result".into(), event.clone());
        fields.insert(
            "detail".into(),
            "test binary finished; binary_result records its final outcome and failure details"
                .into(),
        );
    }

    fn observe_stage(&mut self, event: &aterm_json::Value) -> bool {
        let started = event["event"].as_str() == Some("stage-started");
        let finished = event["event"].as_str() == Some("stage-finished");
        if !started && !finished {
            return true;
        }
        let Some(index) = event.get("stage_index") else {
            return !started; // Older completion-only logs carry no activity evidence.
        };
        let (Some(index), Some(title)) = (index.as_u64(), event["stage"].as_str()) else {
            return false;
        };
        if !valid_stage_title(title) {
            return false;
        }
        if self
            .open_stages
            .get(&index)
            .is_some_and(|old| old.title != title)
        {
            return false;
        }
        if started {
            if self.open_stages.len() >= MAX_OPEN_STAGES && !self.open_stages.contains_key(&index) {
                return false;
            }
            let stamp = event.get("started_at_unix_ms");
            if stamp.is_some_and(|stamp| !stamp.is_null() && stamp.as_u64().is_none()) {
                return false;
            }
            self.open_stages.entry(index).or_insert_with(|| OpenStage {
                title: title.to_owned(),
                started_at_unix_ms: stamp.and_then(aterm_json::Value::as_u64),
            });
        } else {
            self.open_stages.remove(&index);
        }
        self.stage_activity_observed = true;
        true
    }

    fn unfinished_stages(&self) -> aterm_json::Value {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|elapsed| u64::try_from(elapsed.as_millis()).ok());
        self.unfinished_stages_at(now)
    }

    fn unfinished_stages_at(&self, now: Option<u64>) -> aterm_json::Value {
        if !self.stage_activity_observed {
            return aterm_json::Value::Null;
        }
        let stages: Vec<_> = self
            .open_stages
            .iter()
            .map(|(index, stage)| {
                let mut fields = aterm_json::Map::from([
                    ("stage_index".to_string(), (*index).into()),
                    ("stage".to_string(), stage.title.clone().into()),
                ]);
                if let Some(started) = stage.started_at_unix_ms {
                    fields.insert("started_at_unix_ms".into(), started.into());
                    fields.insert(
                        "elapsed_wall_ms".into(),
                        now.and_then(|now| now.checked_sub(started)).into(),
                    );
                }
                aterm_json::Value::Object(fields)
            })
            .collect();
        aterm_json::Value::Array(stages)
    }
}

fn failed(event: &aterm_json::Value) -> bool {
    let failure = |value: &aterm_json::Value| matches!(value.as_str(), Some("FAIL" | "NOT RUN"));
    failure(&event["outcome"])
        || event["decisions"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| failure(&row["outcome"])))
}

/// The longest log line [`event_lines`] reads: a test may print an arbitrarily long line, and
/// the verifier's and the release's structured records are small.
pub(crate) const LINE_LIMIT: usize = 64 * 1024;

/// One line of a release or verify log, as [`event_lines`] hands it on.
pub(crate) enum Line<'a> {
    /// A `verify-event: ` line's JSON text.
    Verify(&'a [u8]),
    /// A `release-event: ` line's JSON text.
    Release(&'a [u8]),
    /// A line longer than [`LINE_LIMIT`], read past and never retained.
    TooLong,
}

/// Read `reader`'s structured lines (`verify-event: `, `release-event: `) in bounded memory,
/// handing each to `each`, every other line read past: the one reader of these lines that
/// `ship status` and the check records ([`crate::check_record`]) share. Returns whether the read
/// ended inside a line: a partly written last record is not a decision.
///
/// # Errors
/// The read itself.
pub(crate) fn event_lines(
    mut reader: impl Read,
    mut each: impl FnMut(Line<'_>),
) -> std::io::Result<bool> {
    let mut line = Vec::new();
    let mut overflow = false;
    let mut bytes = [0; 8192];
    loop {
        let n = reader.read(&mut bytes)?;
        if n == 0 {
            return Ok(overflow || !line.is_empty());
        }
        let mut rest = &bytes[..n];
        while let Some(pos) = rest.iter().position(|&b| b == b'\n') {
            let chunk = &rest[..pos];
            rest = &rest[pos + 1..];
            if line.is_empty() && !overflow {
                if chunk.len() > LINE_LIMIT {
                    each(Line::TooLong);
                } else if let Some(json) = chunk.strip_prefix(b"verify-event: ") {
                    each(Line::Verify(json));
                } else if let Some(json) = chunk.strip_prefix(b"release-event: ") {
                    each(Line::Release(json));
                }
            } else {
                if !overflow {
                    if line.len() + chunk.len() <= LINE_LIMIT {
                        line.extend_from_slice(chunk);
                    } else {
                        overflow = true;
                    }
                }
                if overflow {
                    each(Line::TooLong);
                } else if let Some(json) = line.strip_prefix(b"verify-event: ") {
                    each(Line::Verify(json));
                } else if let Some(json) = line.strip_prefix(b"release-event: ") {
                    each(Line::Release(json));
                }
                line.clear();
                overflow = false;
            }
        }
        if !rest.is_empty() && !overflow {
            let room = LINE_LIMIT.saturating_sub(line.len());
            if rest.len() <= room {
                line.extend_from_slice(rest);
            } else {
                line.extend_from_slice(&rest[..room]);
                overflow = true;
            }
        }
    }
}

fn read_events(reader: impl Read) -> std::io::Result<Events> {
    let mut result = Events::default();
    let incomplete_tail = event_lines(reader, |line| {
        let json = match line {
            Line::TooLong => {
                result.rejected_lines += 1;
                return;
            }
            Line::Verify(json) | Line::Release(json) => json,
        };
        match aterm_json::from_slice::<aterm_json::Value>(json) {
            Ok(event)
                if event["schema"].as_u64() == Some(1)
                    && matches!(
                        event["event"].as_str(),
                        Some(
                            "stage-started"
                                | "stage-finished"
                                | "check-finished"
                                | "test-binary-finished"
                                | "test-case-failed"
                                | "release-stage"
                                | "build-child-finished"
                                | "step-child-finished"
                                | "compile-finished"
                                | "compile-unit-dispatched"
                                | "compile-unit-finished"
                                | "compile-unit-message-unavailable"
                                | "compile-unit-timed"
                                | "inputs"
                                | "handed"
                                | "continued"
                                | "target-retired"
                                | "disk"
                                | "publish-line"
                        )
                    ) =>
            {
                if !result.observe_stage(&event) {
                    result.rejected_lines += 1;
                }
                result.count += 1;
                result.observe_failure(&event);
                if event["event"].as_str() == Some("release-stage") {
                    if event["stage"].as_str().is_some_and(valid_stage_title) {
                        result.latest_release_stage = Some(event.clone());
                    } else {
                        result.rejected_lines += 1;
                    }
                }
                result.latest = Some(event);
            }
            _ => result.rejected_lines += 1,
        }
    })?;
    result.incomplete_tail = incomplete_tail;
    Ok(result)
}

struct Journals {
    current: Option<crate::release::Journal>,
    previous: Option<crate::release::Journal>,
    binding: &'static str,
}

fn assign_journal(
    journal: Option<crate::release::Journal>,
    event: Option<&aterm_json::Value>,
    running: bool,
) -> Journals {
    let binding = match (&journal, event) {
        (None, _) => "absent",
        (Some(_), None) if running => "unresolved",
        (Some(_), None) => "retained",
        (Some(journal), Some(event)) => {
            match (event["source"].as_str(), event["version"].as_str()) {
                (Some(source), Some(version))
                    if source == journal.commit && version == journal.version =>
                {
                    "current"
                }
                (None | Some("?"), _) | (_, None | Some("?")) => "unresolved",
                _ => "previous",
            }
        }
    };
    if matches!(binding, "current" | "retained") {
        Journals {
            current: journal,
            previous: None,
            binding,
        }
    } else {
        Journals {
            current: None,
            previous: journal,
            binding,
        }
    }
}

fn valid_log_dir(text: &str) -> Option<std::path::PathBuf> {
    if text.is_empty() || text.len() > 8192 || text.chars().any(char::is_control) {
        return None;
    }
    let path = std::path::PathBuf::from(text);
    path.is_absolute().then_some(path)
}

fn read_latest_run(path: &Path) -> Result<Option<std::path::PathBuf>, Error> {
    let bytes = match crate::durable::read_regular_bytes(path, 8192) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::FileTooLarge => {
            return Err(Error::new("latest-run log pointer exceeds size limit"));
        }
        Err(error) => return Err(Error::new(error.to_string())),
    };
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| Error::new("latest-run log pointer is not valid UTF-8"))?
        .trim_end_matches(['\r', '\n']);
    valid_log_dir(text)
        .map(Some)
        .ok_or_else(|| Error::new("latest-run log pointer is empty or invalid"))
}

fn resolve_logs(
    holder: Option<&crate::release::Holder>,
    latest_run: &Path,
) -> Result<Option<std::path::PathBuf>, Error> {
    if let Some(holder) = holder
        && !holder.logs.is_empty()
    {
        return valid_log_dir(&holder.logs)
            .map(Some)
            .ok_or_else(|| Error::new("running holder log pointer is invalid"));
    }
    read_latest_run(latest_run)
}

/// Step 1's preflight verifiers write into its own log, and step 4's `verify release` into its
/// tree branch's; every later step keeps the bundle half's, the final qualification.
fn check_log(logs: &Path, step: Option<&str>) -> std::path::PathBuf {
    use crate::release::{Branch, Step};
    let named = |phase: Step| Some(phase.name()) == step;
    if named(Step::Checks) {
        logs.join(format!(
            "{:02}-{}.log",
            Step::Checks.number(),
            Step::Checks.name()
        ))
    } else if named(Step::Qualify) {
        logs.join(format!(
            "{:02}-{}-{}.log",
            Step::Qualify.number(),
            Step::Qualify.name(),
            Branch::Tree.name()
        ))
    } else {
        logs.join(format!(
            "{:02}-{}.log",
            Step::Bundle.number(),
            Step::Bundle.name()
        ))
    }
}

/// Read the journal and its logs without claiming, resuming or building a release.
///
/// # Errors
/// The repository, journal, log pointer or existing verification log cannot be read.
pub fn run(repo: &Path, json: bool) -> Result<(), Error> {
    let dir = crate::release::release_dir(&GitCli::new(repo)).map_err(Error::new)?;
    let journal = crate::release::Journal::load(&dir.join(crate::release::JOURNAL_FILE))
        .map_err(Error::new)?;
    let holder = crate::release::live_holder(
        &dir.join(crate::release::RUNNING_FILE),
        &mut crate::release_steps::started_of,
    );
    let logs = resolve_logs(holder.as_ref(), &dir.join("latest-run"))?;
    let transcript = logs.as_ref().map(|path| path.join("transcript.log"));
    let release_events = match transcript.as_deref().map(crate::durable::open_regular_file) {
        Some(Ok(file)) => read_events(file).map_err(|error| Error::new(error.to_string()))?,
        Some(Err(error)) if error.kind() != std::io::ErrorKind::NotFound => {
            return Err(Error::new(error.to_string()));
        }
        _ => Events::default(),
    };
    let step = release_events
        .latest_release_stage
        .as_ref()
        .and_then(|event| event["stage"].as_str())
        .map(str::to_string)
        .or_else(|| {
            holder
                .as_ref()
                .and_then(|holder| holder.step.as_deref())
                .filter(|step| valid_stage_title(step))
                .map(str::to_string)
        });
    let log = logs.as_ref().map(|path| check_log(path, step.as_deref()));
    let events = match log.as_deref().map(crate::durable::open_regular_file) {
        Some(Ok(file)) => read_events(file).map_err(|error| Error::new(error.to_string()))?,
        Some(Err(error)) if error.kind() != std::io::ErrorKind::NotFound => {
            return Err(Error::new(error.to_string()));
        }
        _ => Events::default(),
    };
    let timeline = match logs
        .as_deref()
        .map(crate::release_timeline::Timeline::of_logs)
    {
        Some(Ok(timeline)) => timeline.value(),
        Some(Err(error)) if error.kind() != std::io::ErrorKind::NotFound => {
            return Err(Error::new(error.to_string()));
        }
        _ => aterm_json::Value::Null,
    };
    let state = if holder.is_some() {
        "running"
    } else if journal.is_some() || logs.is_some() {
        "not-running"
    } else {
        "none"
    };
    let journals = assign_journal(
        journal,
        release_events.latest_release_stage.as_ref(),
        holder.is_some(),
    );
    let journal = journals.current;
    let measurement_exception = journal
        .as_ref()
        .and_then(|journal| {
            journal.artifact.as_ref().map(|artifact| {
                crate::check_record::accepted(
                    &GitCli::new(repo),
                    &journal.commit,
                    &artifact.macho_sha256,
                    crate::build_inputs::current,
                )
            })
        })
        .transpose()
        .map_err(Error::new)?
        .flatten();
    let unfinished_verification_stages = events.unfinished_stages();
    let observations_complete = events.rejected_lines == 0
        && release_events.rejected_lines == 0
        && !events.incomplete_tail
        && !release_events.incomplete_tail;
    let rejected_log_lines = events
        .rejected_lines
        .saturating_add(release_events.rejected_lines);
    let first_reported_failure = events
        .first_failure
        .or(release_events.first_failure)
        .unwrap_or(aterm_json::Value::Null);
    let latest_release_event = release_events.latest.unwrap_or(aterm_json::Value::Null);
    let latest_verification_event = events.latest.unwrap_or(aterm_json::Value::Null);
    let op_status = operational_status(logs.as_deref());
    let value = aterm_json::Value::Object(aterm_json::Map::from([
        ("schema".to_string(), aterm_json::Value::Number(1u64.into())),
        (
            "state".to_string(),
            aterm_json::Value::String(state.to_string()),
        ),
        (
            "journal_finished".to_string(),
            aterm_json::Value::Bool(
                journal
                    .as_ref()
                    .is_some_and(crate::release::Journal::finished),
            ),
        ),
        (
            "measurement_exception".to_string(),
            aterm_json::json!(measurement_exception),
        ),
        ("journal".to_string(), aterm_json::json!(journal)),
        (
            "journal_binding".to_string(),
            aterm_json::Value::String(journals.binding.to_string()),
        ),
        (
            "previous_journal".to_string(),
            aterm_json::json!(journals.previous),
        ),
        (
            "pid".to_string(),
            holder.as_ref().map_or(aterm_json::Value::Null, |holder| {
                aterm_json::Value::Number((holder.pid as u64).into())
            }),
        ),
        (
            "step".to_string(),
            step.map_or(aterm_json::Value::Null, aterm_json::Value::String),
        ),
        ("release_log".to_string(), aterm_json::json!(transcript)),
        (
            "release_events".to_string(),
            aterm_json::Value::Number((release_events.count as u64).into()),
        ),
        ("latest_release_event".to_string(), latest_release_event),
        (
            "observations_complete".to_string(),
            aterm_json::Value::Bool(observations_complete),
        ),
        ("logs".to_string(), aterm_json::json!(logs)),
        ("operational_status".to_string(), op_status),
        ("verification_log".to_string(), aterm_json::json!(log)),
        (
            "verification_events".to_string(),
            aterm_json::Value::Number((events.count as u64).into()),
        ),
        (
            "unfinished_verification_stages".to_string(),
            unfinished_verification_stages,
        ),
        ("first_reported_failure".to_string(), first_reported_failure),
        (
            "latest_verification_event".to_string(),
            latest_verification_event,
        ),
        (
            "rejected_log_lines".to_string(),
            aterm_json::Value::Number((rejected_log_lines as u64).into()),
        ),
        ("timeline".to_string(), timeline),
    ]));
    if json {
        println!("{value}");
    } else {
        println!("{}", text_status(&value));
    }
    Ok(())
}

fn scrub_controls(text: String) -> String {
    if !text.chars().any(char::is_control) {
        return text;
    }
    text.chars()
        .map(|ch| if ch.is_control() { '\u{fffd}' } else { ch })
        .collect()
}

fn text_status(value: &aterm_json::Value) -> String {
    let mut lines = vec![scrub_controls(format!(
        "release: {}",
        value["state"].as_str().unwrap_or("unknown")
    ))];
    for (label, field) in [
        ("stage", "step"),
        ("release log", "release_log"),
        ("verification log", "verification_log"),
    ] {
        if let Some(text) = value[field].as_str() {
            lines.push(scrub_controls(format!("{label}: {text}")));
        }
    }
    if let Some(stages) = value["unfinished_verification_stages"].as_array() {
        for stage in stages {
            if let Some(title) = stage["stage"].as_str() {
                let age = stage["elapsed_wall_ms"]
                    .as_u64()
                    .map_or_else(String::new, |age| {
                        format!(" ({} s since start, wall clock)", age / 1000)
                    });
                lines.push(scrub_controls(format!(
                    "unfinished verification stage: {title}{age}"
                )));
            }
        }
    } else if value["state"].as_str() == Some("running") && value["step"].as_str() == Some("verify")
    {
        lines.push("verification stage activity: not recorded".into());
    }
    for (label, field) in [
        ("first reported failure", "first_reported_failure"),
        ("latest release event", "latest_release_event"),
        ("latest verification event", "latest_verification_event"),
    ] {
        if !value[field].is_null() {
            lines.push(scrub_controls(format!("{label}: {}", value[field])));
        }
    }
    if let Some(line) = timeline_line(&value["timeline"]) {
        lines.push(line);
    }
    if value["observations_complete"].as_bool() == Some(false) {
        lines.push("observations: incomplete; a log record is partial or unreadable".into());
    }
    lines.join("\n")
}

/// One line of where the run's time went, from the timeline's totals; `None` before any step ended.
fn timeline_line(timeline: &aterm_json::Value) -> Option<String> {
    let path = timeline["critical_path_s"].as_f64()?;
    let mut words = vec![format!("timeline: {path:.0} s on the critical path")];
    if let Some(red) = timeline["time_to_first_red_s"].as_f64() {
        words.push(format!("first red after {red:.0} s"));
    }
    if let Some(hashes) = timeline["full_hashes"].as_u64() {
        words.push(format!("{hashes} full input hash(es)"));
    }
    if let Some(walks) = timeline["disk_walks"].as_u64() {
        words.push(format!("{walks} disk walk(s)"));
    }
    if let (Some(fresh), Some(compiled)) = (
        timeline["units"]["fresh"].as_u64(),
        timeline["units"]["compiled"].as_u64(),
    ) {
        words.push(format!("{fresh} units fresh / {compiled} compiled"));
    }
    Some(words.join(", "))
}

fn operational_status(logs: Option<&Path>) -> aterm_json::Value {
    let Some(logs) = logs else {
        return aterm_json::Value::Null;
    };
    let result = (|| -> Result<aterm_json::Value, String> {
        let bytes =
            crate::durable::read_regular_bytes(&logs.join("operational-status.json"), 256 * 1024)
                .map_err(|e| {
                if e.kind() == std::io::ErrorKind::FileTooLarge {
                    "operational record exceeds size limit".to_owned()
                } else {
                    e.to_string()
                }
            })?;
        let record: aterm_json::Value =
            aterm_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        if record["schema"].as_u64() != Some(1)
            || record["attempt"].as_str() != Some(logs.to_string_lossy().as_ref())
        {
            return Err("operational record belongs to another attempt".into());
        }
        Ok(record)
    })();
    result.unwrap_or_else(|reason| {
        aterm_json::Value::Object(aterm_json::Map::from([
            (
                "state".to_string(),
                aterm_json::Value::String("unknown".to_string()),
            ),
            ("reason".to_string(), aterm_json::Value::String(reason)),
        ]))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_case_failure_gains_only_its_own_binary_completion() {
        let first = aterm_json::json!({"schema":1,"event":"test-case-failed",
            "outcome":"FAIL","test":"gui --lib","case":"broken",
            "elapsed_s":72,"binary_completed":false,"detail":"result pending"});
        let unrelated = aterm_json::json!({"schema":1,"event":"test-binary-finished",
            "test":"other --lib","outcome":"ok","exit_code":0});
        let complete = aterm_json::json!({"schema":1,"event":"test-binary-finished",
            "test":"gui --lib","outcome":"FAIL","exit_code":101,
            "execution_s":800,"failures":["broken"],"detail":"assertion failed"});
        let prefix = format!("verify-event: {first}\nverify-event: {unrelated}\n");
        let pending = read_events(prefix.as_bytes()).unwrap();
        assert_eq!(pending.first_failure, Some(first.clone()));
        let partial = read_events(format!("{prefix}verify-event: {complete}").as_bytes()).unwrap();
        assert_eq!(partial.first_failure, Some(first.clone()));
        assert!(partial.incomplete_tail);

        let later = aterm_json::json!({"schema":1,"event":"test-binary-finished",
            "test":"gui --lib","outcome":"ok","exit_code":0});
        let log = format!("{prefix}verify-event: {complete}\nverify-event: {later}\n");
        let events = read_events(log.as_bytes()).unwrap();
        let observed = events.first_failure.unwrap();
        assert_eq!(observed["case"], first["case"]);
        assert_eq!(observed["elapsed_s"], first["elapsed_s"]);
        assert_eq!(observed["outcome"], first["outcome"]);
        assert_eq!(observed["binary_completed"].as_bool(), Some(true));
        assert_eq!(observed["initial_detail"], first["detail"]);
        assert_eq!(observed["binary_result"], complete);
        assert!(!observed["detail"].as_str().unwrap().contains("pending"));
    }

    #[test]
    fn unfinished_stage_age_preserves_the_first_start_and_clock_uncertainty() {
        let mut events = Events::default();
        let start = |stamp| {
            aterm_json::json!({"event":"stage-started",
            "stage_index":3,"stage":"lint","started_at_unix_ms":stamp})
        };
        assert!(events.observe_stage(&start(1_000)));
        assert!(events.observe_stage(&start(2_000)));
        let stages = events.unfinished_stages_at(Some(4_500));
        assert_eq!(stages[0]["started_at_unix_ms"].as_u64(), Some(1_000));
        assert_eq!(stages[0]["elapsed_wall_ms"].as_u64(), Some(3_500));
        let status = aterm_json::json!({"unfinished_verification_stages":stages});
        assert!(text_status(&status).contains("lint (3 s since start, wall clock)"));
        assert!(events.unfinished_stages_at(Some(900))[0]["elapsed_wall_ms"].is_null());
        assert!(events.unfinished_stages_at(None)[0]["elapsed_wall_ms"].is_null());
        let malformed = aterm_json::json!({"event":"stage-started",
            "stage_index":4,"stage":"test","started_at_unix_ms":"unknown"});
        assert!(!events.observe_stage(&malformed));
        assert_eq!(events.open_stages.len(), 1);
    }

    #[test]
    fn preflight_progress_and_failures_are_read_from_the_enclosing_step() {
        let dir = aterm_tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("05-bundle.log"), b"").unwrap();
        let start = aterm_json::json!({"schema":1,"event":"stage-started",
            "stage_index":1,"stage":"checker compile"});
        let failure = aterm_json::json!({"schema":1,"event":"test-case-failed",
            "outcome":"FAIL","test":"source closure"});
        for (phase, log) in [
            (crate::release::Step::Checks, "01-checks.log"),
            (crate::release::Step::Qualify, "04-qualify-tree.log"),
        ] {
            std::fs::write(
                dir.path().join(log),
                format!("verify-event: {start}\nverify-event: {failure}\n"),
            )
            .unwrap();
            let selected = check_log(dir.path(), Some(phase.name()));
            assert_eq!(selected, dir.path().join(log));
            let events = read_events(std::fs::File::open(selected).unwrap()).unwrap();
            assert_eq!(events.first_failure, Some(failure.clone()));
            assert_eq!(
                events.unfinished_stages(),
                aterm_json::json!([
                    {"stage_index":1,"stage":"checker compile"}
                ])
            );
        }
        for phase in [
            None,
            Some("bundle"),
            Some("publish"),
            Some("check"),
            Some("../outside"),
        ] {
            assert_eq!(
                check_log(dir.path(), phase),
                dir.path().join("05-bundle.log")
            );
        }
    }

    #[test]
    fn compiler_telemetry_keeps_the_current_release_stage_and_journal_binding() {
        let stage = aterm_json::json!({"schema":1,"event":"release-stage",
            "source":"current-source","version":"0.150.0","stage":"qualify","outcome":"started"});
        let child = aterm_json::json!({"schema":1,"event":"build-child-finished",
            "package":"aterm","outcome":"ok","execution_s":3});
        let log = format!("release-event: {stage}\nrelease-event: {child}\n");
        let events = read_events(log.as_bytes()).unwrap();
        assert_eq!(events.latest, Some(child));
        assert_eq!(events.latest_release_stage, Some(stage));
        let journal = crate::release::Journal {
            commit: "current-source".into(),
            version: "0.150.0".into(),
            ..Default::default()
        };
        let bound = assign_journal(Some(journal), events.latest_release_stage.as_ref(), true);
        assert_eq!(bound.binding, "current");
        assert!(bound.current.is_some());
        assert!(
            check_log(
                Path::new("logs"),
                events
                    .latest_release_stage
                    .as_ref()
                    .and_then(|event| event["stage"].as_str())
            )
            .ends_with("04-qualify-tree.log")
        );
    }

    #[test]
    fn concurrent_stages_are_paired_by_index_and_failure_does_not_hide_open_work() {
        let rows = [
            aterm_json::json!({"schema":1,"event":"stage-started","stage_index":1,"stage":"compile"}),
            aterm_json::json!({"schema":1,"event":"stage-started","stage_index":2,"stage":"compile"}),
            aterm_json::json!({"schema":1,"event":"stage-finished","stage_index":1,"stage":"compile",
                "decisions":[{"outcome":"FAIL"}]}),
        ];
        let text = rows
            .iter()
            .map(|row| format!("verify-event: {row}\n"))
            .collect::<String>();
        let events = read_events(text.as_bytes()).unwrap();
        assert_eq!(events.count, 3);
        assert!(events.first_failure.is_some());
        assert_eq!(
            events.unfinished_stages(),
            aterm_json::json!([
                {"stage_index":2,"stage":"compile"}
            ])
        );
        let status = aterm_json::json!({"state":"running","step":"verify",
            "unfinished_verification_stages":events.unfinished_stages()});
        assert!(text_status(&status).contains("unfinished verification stage: compile"));
        let done = format!(
            "{text}verify-event: {}\n",
            aterm_json::json!({
            "schema":1,"event":"stage-finished","stage_index":2,"stage":"compile"})
        );
        assert_eq!(
            read_events(done.as_bytes()).unwrap().unfinished_stages(),
            aterm_json::json!([])
        );
    }

    #[test]
    fn stage_activity_is_unknown_for_old_logs_and_partial_finishes_leave_work_open() {
        let old = b"verify-event: {\"schema\":1,\"event\":\"stage-finished\",\"stage\":\"old\"}\n";
        assert!(
            read_events(old.as_slice())
                .unwrap()
                .unfinished_stages()
                .is_null()
        );
        let partial = concat!(
            "verify-event: {\"schema\":1,\"event\":\"stage-started\",\"stage_index\":3,\"stage\":\"lint\"}\n",
            "verify-event: {\"schema\":1,\"event\":\"stage-finished\",\"stage_index\":3"
        );
        let events = read_events(partial.as_bytes()).unwrap();
        assert!(events.incomplete_tail);
        assert_eq!(
            events.open_stages.get(&3).map(|stage| stage.title.as_str()),
            Some("lint")
        );
    }

    #[test]
    fn stage_activity_rejects_rebinding_and_bounds_retained_work() {
        let mut events = Events::default();
        for index in 0..MAX_OPEN_STAGES {
            assert!(events.observe_stage(&aterm_json::json!({
                "event":"stage-started","stage_index":index,"stage":"build"})));
        }
        assert!(!events.observe_stage(&aterm_json::json!({
            "event":"stage-started","stage_index":MAX_OPEN_STAGES,"stage":"extra"})));
        assert!(!events.observe_stage(&aterm_json::json!({
            "event":"stage-finished","stage_index":0,"stage":"different"})));
        assert_eq!(events.open_stages.len(), MAX_OPEN_STAGES);
        assert!(events.observe_stage(&aterm_json::json!({
            "event":"stage-finished","stage_index":0,"stage":"build"})));
        assert!(events.observe_stage(&aterm_json::json!({
            "event":"stage-started","stage_index":MAX_OPEN_STAGES,"stage":"extra"})));
    }

    /// The build-inputs seal's counting lines (`build_inputs::current`, one per call, in every
    /// mode it prints) are events of the run, never rejected lines, so a log that holds them
    /// still reads as complete. CONTROL: an event of a name nothing prints is rejected.
    /// NEGATIVE CONTROL: without `inputs` among the known events each is a rejected line.
    #[test]
    fn the_inputs_seal_lines_are_events_of_the_run() {
        let rows = ["hashed", "rechecked", "adopted"].map(|mode| {
            aterm_json::json!({"schema":1,"event":"inputs","mode":mode,"entries":10,"secs":0.5})
        });
        let lines = rows
            .iter()
            .map(|row| format!("release-event: {row}\n"))
            .collect::<String>();
        let events = read_events(lines.as_bytes()).unwrap();
        assert_eq!((events.rejected_lines, events.count), (0, 3));
        let unknown = "release-event: {\"schema\":1,\"event\":\"inputs-guessed\"}\n";
        assert_eq!(read_events(unknown.as_bytes()).unwrap().rejected_lines, 1);
    }

    /// The record an exec'd image writes, naming the preceding image's logs (`continued`, which
    /// [`crate::release_timeline::Timeline`] follows), and the `handed` spelling a cutter already
    /// running at the B1 boundary wrote are events of the run and no failure. NEGATIVE CONTROL:
    /// without either among the known events that cut's observations become incomplete.
    #[test]
    fn a_continued_images_record_is_an_event_of_the_run() {
        for event in ["handed", "continued"] {
            let text = format!(
                "release-event: {}\n",
                aterm_json::json!({"schema": 1, "event": event, "from": "/state/logs/outer"})
            );
            let events = read_events(text.as_bytes()).unwrap();
            assert_eq!((events.rejected_lines, events.count), (0, 1), "{text}");
            assert!(events.first_failure.is_none());
        }
    }

    /// A frozen cutter's retirement measurement is a first-class release event, so the next
    /// cutter can read the exact zero from its transcript. Changing its name makes the negative
    /// control's otherwise identical line rejected.
    #[test]
    fn target_retirement_records_its_remaining_deps_count() {
        let line = |event| {
            format!(
                "release-event: {}\n",
                aterm_json::json!({"schema": 1, "event": event, "deps_entries": 0})
            )
        };
        let accepted = read_events(line("target-retired").as_bytes()).unwrap();
        assert_eq!((accepted.rejected_lines, accepted.count), (0, 1));
        let unknown = read_events(line("target-retire-mutant").as_bytes()).unwrap();
        assert_eq!((unknown.rejected_lines, unknown.count), (1, 0));
    }

    /// aterm-verify's per-unit compile evidence, a line for each unit of each compile
    /// ([`aterm_verify::report::compile_unit_events`]: dispatched, finished, or a message whose
    /// unit cannot be told), is an event of the run and no failure, so a verify that compiled
    /// still reads as complete. NEGATIVE CONTROL: without the three among the known events each
    /// is a rejected line and the run's observations are incomplete.
    #[test]
    fn a_compiles_unit_lines_are_events_of_the_run() {
        let unit = |reason: &str, identity: &str| {
            aterm_json::json!({
                "reason": reason, "schema": "targo.compilation-unit-dispatched.v1",
                "trust_unit_identity_sha256": identity, "trust_compile_mode": "test",
                "trust_compile_kind": "host", "fresh": false, "dirty_reason": "changed-file",
            })
            .to_string()
        };
        let (a, b) = ("a".repeat(64), "b".repeat(64));
        let two_units = unit("compilation-unit-dispatched", &a).replacen(
            &format!("\"trust_unit_identity_sha256\":\"{a}\""),
            &format!(
                "\"trust_unit_identity_sha256\":\"{a}\",\"trust_unit_identity_sha256\":\"{b}\""
            ),
            1,
        );
        let output = [
            unit("compilation-unit-dispatched", &a),
            unit("compiler-artifact", &a),
            two_units,
        ]
        .join("\n");
        let lines = aterm_verify::report::compile_unit_events("test", &output);
        let names: std::collections::BTreeSet<String> = lines
            .iter()
            .map(|line| {
                let json = line.strip_prefix("verify-event: ").unwrap();
                let event: aterm_json::Value = aterm_json::from_str(json).unwrap();
                event["event"].as_str().unwrap().to_owned()
            })
            .collect();
        assert_eq!(
            names,
            [
                "compile-unit-dispatched",
                "compile-unit-finished",
                "compile-unit-message-unavailable"
            ]
            .map(str::to_owned)
            .into(),
            "{lines:#?}"
        );
        let text = lines
            .iter()
            .map(|line| format!("{line}\n"))
            .collect::<String>();
        let events = read_events(text.as_bytes()).unwrap();
        assert_eq!((events.rejected_lines, events.count), (0, 3), "{lines:#?}");
        assert!(events.first_failure.is_none());
    }

    /// A compile's timed units (`compile-unit-timed`, one per compiler its stage's reads dated)
    /// are events of the run. NEGATIVE CONTROL: without them among the known events each is a
    /// rejected line and the run's observations are incomplete.
    #[test]
    fn a_compiles_timed_units_are_events_of_the_run() {
        let span = aterm_verify::memory::UnitSpan {
            unit: aterm_verify::unitgraph::UnitName {
                package: "aterm-gui".to_owned(),
                kind: "aterm_gui test".to_owned(),
            },
            queued_at_unix_ms: Some(1),
            codegen_requested: Some(true),
            started_at_unix_ms: 2,
            ended_at_unix_ms: 3,
            cpu_s: 1.0,
            peak_bytes: 4,
        };
        let text: String =
            aterm_verify::compile_account::span_events("test", &[span.clone(), span])
                .iter()
                .map(|line| format!("{line}\n"))
                .collect();
        let events = read_events(text.as_bytes()).unwrap();
        assert_eq!((events.rejected_lines, events.count), (0, 2), "{text}");
    }

    /// A check's `check-finished` line (A19's record source) is an event of the run, and a
    /// failed one is a reported failure. NEGATIVE CONTROL: without `check-finished` among the
    /// known events it is a rejected line and the run's observations are incomplete.
    #[test]
    fn a_check_finished_line_is_an_event_of_the_run() {
        let lines = "verify-event: {\"schema\":1,\"event\":\"check-finished\",\"check\":\"fmt\",\"outcome\":\"ok\",\"seconds\":2.5}\n\
             verify-event: {\"schema\":1,\"event\":\"check-finished\",\"check\":\"lint\",\"outcome\":\"FAIL\",\"seconds\":9.0}\n";
        let events = read_events(lines.as_bytes()).unwrap();
        assert_eq!((events.rejected_lines, events.count), (0, 2));
        assert_eq!(
            events
                .first_failure
                .as_ref()
                .and_then(|e| e["check"].as_str()),
            Some("lint")
        );
    }

    #[test]
    fn malformed_activity_never_hides_a_reported_failure_or_prints_control_titles() {
        let rows = [
            aterm_json::json!({"schema":1,"event":"stage-started","stage_index":4,"stage":"lint"}),
            aterm_json::json!({"schema":1,"event":"stage-finished","stage_index":4,
                "stage":"different","decisions":[{"outcome":"FAIL"}]}),
            aterm_json::json!({"schema":1,"event":"stage-started","stage_index":5,"stage":"bad\nname"}),
        ];
        let text = rows
            .iter()
            .map(|row| format!("verify-event: {row}\n"))
            .collect::<String>();
        let events = read_events(text.as_bytes()).unwrap();
        assert_eq!(events.count, 3);
        assert_eq!(events.rejected_lines, 2);
        assert!(events.first_failure.is_some());
        assert_eq!(
            events.open_stages.get(&4).map(|stage| stage.title.as_str()),
            Some("lint")
        );
        assert!(!events.open_stages.contains_key(&5));
    }

    #[test]
    fn text_status_exposes_build_progress_before_verification_starts() {
        let event = aterm_json::json!({
            "event": "build-child-finished", "execution_s": 12,
            "child_cpu": {"observed_s": 9}, "outcome": "PASS",
        });
        let status = aterm_json::json!({
            "state": "running", "step": "build", "release_log": "/cut/transcript.log",
            "latest_release_event": event, "observations_complete": true,
        });
        let text = text_status(&status);
        assert!(text.contains("stage: build"));
        assert!(text.contains("release log: /cut/transcript.log"));
        assert!(text.contains(&event.to_string()));
        assert!(!text.contains("verification log:"));
        assert!(!text.contains("incomplete"));
    }

    #[test]
    fn text_status_keeps_failure_visible_alongside_later_progress_and_partial_logs() {
        let failure = aterm_json::json!({"outcome": "FAIL", "case": "first"});
        let latest = aterm_json::json!({"outcome": "PASS", "case": "later"});
        let status = aterm_json::json!({
            "state": "running", "step": "verify",
            "first_reported_failure": failure, "latest_verification_event": latest,
            "observations_complete": false,
        });
        let text = text_status(&status);
        assert!(text.contains(&format!("first reported failure: {failure}")));
        assert!(text.contains(&format!("latest verification event: {latest}")));
        assert!(text.contains("observations: incomplete"));
    }

    #[test]
    fn operational_record_is_bound_to_the_selected_attempt() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join("operational-status.json");
        let record = aterm_json::json!({"schema":1,"attempt":dir.path(),
            "observation":{"capability":{"state":"unknown","reason":"peer unavailable"}}});
        std::fs::write(&path, record.to_string()).unwrap();
        let read = operational_status(Some(dir.path()));
        assert_eq!(read, record);
        assert!(read["observation"]["capability"].get("value").is_none());
        let wrong = aterm_json::json!({"schema":1,"attempt":"another attempt","observation":{}});
        std::fs::write(&path, wrong.to_string()).unwrap();
        assert_eq!(
            operational_status(Some(dir.path()))["state"].as_str(),
            Some("unknown")
        );
        std::fs::remove_file(path).unwrap();
        assert_eq!(
            operational_status(Some(dir.path()))["state"].as_str(),
            Some("unknown")
        );
    }

    #[test]
    fn a_new_preflight_cannot_inherit_a_finished_releases_journal() {
        let old = crate::release::Journal {
            commit: "old-source".into(),
            version: "0.148.0".into(),
            done: vec!["check".into(), "tap-aligned".into()],
            ..Default::default()
        };
        let event = aterm_json::json!({"source":"?", "version":"?"});
        let journals = assign_journal(Some(old.clone()), Some(&event), true);
        assert_eq!(journals.binding, "unresolved");
        assert!(
            journals.current.is_none(),
            "no current completion or exception can be derived"
        );
        assert!(journals.previous.unwrap().finished());
        let event = aterm_json::json!({"source":"new-source", "version":"0.149.0"});
        let journals = assign_journal(Some(old.clone()), Some(&event), true);
        assert_eq!(journals.binding, "previous");
        assert!(journals.current.is_none());
        let event = aterm_json::json!({"source":"new-source", "version":"0.148.0"});
        assert!(
            assign_journal(Some(old.clone()), Some(&event), true)
                .current
                .is_none(),
            "matching the version alone does not bind an artifact"
        );
        let event = aterm_json::json!({"source":"old-source", "version":"0.148.0"});
        let journals = assign_journal(Some(old), Some(&event), true);
        assert_eq!(journals.binding, "current");
        assert!(journals.current.unwrap().finished());
        assert!(journals.previous.is_none());
    }

    #[test]
    fn a_running_attempt_without_events_does_not_claim_an_old_journal() {
        let old = crate::release::Journal::default();
        assert!(
            assign_journal(Some(old.clone()), None, true)
                .current
                .is_none()
        );
        assert!(assign_journal(Some(old), None, false).current.is_some());
    }

    #[test]
    fn status_is_a_read_only_verb_with_explicit_json_output() {
        let args = |items: &[&str]| {
            items
                .iter()
                .map(|item| (*item).to_string())
                .collect::<Vec<_>>()
        };
        assert!(matches!(
            crate::cli::parse(&args(&["release", "status", "--json"])),
            Ok(crate::cli::Cmd::ReleaseStatus {
                json: true,
                fleet: false
            })
        ));
        assert!(matches!(
            crate::cli::parse(&args(&["release", "status"])),
            Ok(crate::cli::Cmd::ReleaseStatus {
                json: false,
                fleet: false
            })
        ));
        assert!(matches!(
            crate::cli::parse(&args(&["release", "status", "--fleet"])),
            Ok(crate::cli::Cmd::ReleaseStatus {
                json: false,
                fleet: true
            })
        ));
        assert!(crate::cli::parse(&args(&["release", "status", "--fleet", "--bump"])).is_err());
        for extra in ["--bump", "--forget", "--prepare"] {
            assert!(crate::cli::parse(&args(&["release", "status", extra])).is_err());
        }
    }

    #[test]
    fn failure_remains_visible_after_later_success_and_an_incomplete_write() {
        let bytes = concat!(
            "ordinary test output\n",
            "verify-event: {\"schema\":1,\"event\":\"test-binary-finished\",\"outcome\":\"FAIL\",\"test\":\"first\",\"waiting_s\":3,\"execution_s\":7}\n",
            "verify-event: {\"schema\":1,\"event\":\"stage-finished\",\"decisions\":[{\"outcome\":\"ok\"}]}\n",
            "verify-event: {\"schema\":1"
        );
        let events = read_events(bytes.as_bytes()).unwrap();
        assert_eq!(events.count, 2);
        assert!(events.incomplete_tail);
        let first = events.first_failure.unwrap();
        assert_eq!(first["test"].as_str(), Some("first"));
        assert_eq!(first["waiting_s"].as_u64(), Some(3));
        assert_eq!(first["execution_s"].as_u64(), Some(7));
        assert_eq!(
            events.latest.unwrap()["event"].as_str(),
            Some("stage-finished")
        );
    }

    #[test]
    fn build_cpu_and_failure_survive_status_readback() {
        let events = read_events(b"release-event: {\"schema\":1,\"event\":\"build-child-finished\",\"outcome\":\"FAIL\",\"child_cpu\":{\"observed_s\":8},\"execution_s\":3}\n".as_slice()).unwrap();
        assert_eq!(events.count, 1);
        let failure = events.first_failure.unwrap();
        assert_eq!(failure["child_cpu"]["observed_s"].as_u64(), Some(8));
        assert_eq!(failure["execution_s"].as_u64(), Some(3));
    }

    #[test]
    fn oversized_and_invalid_lines_do_not_hide_a_later_unavailable_stage() {
        let mut bytes = vec![b'x'; 128 * 1024];
        bytes.extend_from_slice(b"\nverify-event: not json\nverify-event: {\"schema\":1,\"event\":\"stage-finished\",\"decisions\":[{\"outcome\":\"NOT RUN\"}]}\n");
        let events = read_events(bytes.as_slice()).unwrap();
        assert_eq!(events.rejected_lines, 2);
        assert_eq!(events.count, 1);
        assert!(events.first_failure.is_some());
    }

    #[test]
    fn latest_run_pointer_is_bounded_and_trims_trailing_newline() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let pointer = dir.path().join("latest-run");
        assert_eq!(read_latest_run(&pointer).unwrap(), None);

        let expected = dir.path().join("release-logs");
        std::fs::write(&pointer, format!("{}\n", expected.display())).unwrap();
        assert_eq!(read_latest_run(&pointer).unwrap(), Some(expected));

        std::fs::write(&pointer, "\n").unwrap();
        assert!(read_latest_run(&pointer).is_err());

        std::fs::write(&pointer, "relative/release-logs\n").unwrap();
        assert!(read_latest_run(&pointer).is_err());

        std::fs::write(&pointer, vec![b'a'; 8193]).unwrap();
        assert!(read_latest_run(&pointer).is_err());
    }

    /// ORPHANED CHILD HOLDER FALLS BACK TO `latest-run` INSTEAD OF CWD: a `running.child.<pid>`
    /// note carries `logs: ""` and `step: Some(...)`, so `resolve_logs` must read `latest-run`
    /// rather than `PathBuf::from("")`, while rejecting relative holder paths and symlinks.
    /// NEGATIVE CONTROL: a parent holder with an absolute `logs` path resolves directly to it.
    #[test]
    fn orphaned_child_holder_falls_back_to_latest_run_and_refuses_relative_or_symlink_logs() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let pointer = dir.path().join("latest-run");
        let attempt = dir.path().join("attempt-1");
        let direct = dir.path().join("attempt-2");
        std::fs::create_dir_all(&attempt).unwrap();
        std::fs::write(&pointer, format!("{}\n", attempt.display())).unwrap();

        let child = crate::release::Holder {
            pid: 4242,
            started: "now".into(),
            logs: String::new(),
            step: Some("verify".into()),
            legacy_handed: false,
        };
        assert_eq!(
            resolve_logs(Some(&child), &pointer).unwrap(),
            Some(attempt.clone())
        );

        let parent = crate::release::Holder {
            logs: direct.display().to_string(),
            ..child.clone()
        };
        assert_eq!(resolve_logs(Some(&parent), &pointer).unwrap(), Some(direct));

        let relative = crate::release::Holder {
            logs: "relative/logs".into(),
            ..child
        };
        assert!(resolve_logs(Some(&relative), &pointer).is_err());

        #[cfg(unix)]
        {
            let target = dir.path().join("real-pointer");
            std::fs::write(&target, format!("{}\n", attempt.display())).unwrap();
            let link = dir.path().join("linked-pointer");
            std::os::unix::fs::symlink(&target, &link).unwrap();
            assert!(
                read_latest_run(&link).is_err(),
                "symlinked latest-run must be refused"
            );

            let real_op = dir.path().join("real-operational.json");
            let record = aterm_json::json!({"schema":1,"attempt":attempt,"observation":{}});
            std::fs::write(&real_op, record.to_string()).unwrap();
            std::os::unix::fs::symlink(&real_op, attempt.join("operational-status.json")).unwrap();
            assert_eq!(
                operational_status(Some(&attempt))["state"].as_str(),
                Some("unknown"),
                "symlinked operational-status.json must not be trusted"
            );
        }
    }

    /// MALFORMED `release-stage` AND CONTROL CHARACTERS ARE REJECTED OR SCRUBBED: a `release-stage`
    /// record with an empty or control-character `stage` increments `rejected_lines` without
    /// overwriting `latest_release_stage`, while `text_status` scrubs `\x7f` and Unicode C1
    /// controls (`\u{9b}`). NEGATIVE CONTROL: a clean `release-stage` updates `latest_release_stage`
    /// with zero rejected lines.
    #[test]
    fn release_stage_validates_stage_name_and_text_status_scrubs_c1_controls() {
        let good = aterm_json::json!({
            "schema": 1,
            "event": "release-stage",
            "source": "abc",
            "version": "0.150.0",
            "stage": "verify",
        });
        let bad_ctrl = aterm_json::json!({
            "schema": 1,
            "event": "release-stage",
            "source": "abc",
            "version": "0.150.0",
            "stage": "build\u{9b}31m",
            "outcome": "FAIL",
        });
        let bad_empty = aterm_json::json!({
            "schema": 1,
            "event": "release-stage",
            "source": "abc",
            "version": "0.150.0",
            "stage": "",
        });
        let log = format!(
            "release-event: {good}\nrelease-event: {bad_ctrl}\nrelease-event: {bad_empty}\n"
        );
        let events = read_events(log.as_bytes()).unwrap();
        assert_eq!(events.count, 3);
        assert_eq!(events.rejected_lines, 2);
        assert_eq!(events.latest_release_stage, Some(good));
        assert!(
            events.first_failure.is_some(),
            "failure outcome on a rejected stage line remains visible"
        );

        let status = aterm_json::json!({
            "state": "running",
            "step": "verify",
            "first_reported_failure": {"case": "bad\x7f\u{9b}2Jtitle", "outcome": "FAIL"},
            "observations_complete": true,
        });
        let rendered = text_status(&status);
        assert!(
            !rendered.chars().any(|ch| ch != '\n' && ch.is_control()),
            "rendered text_status must contain no non-newline control chars: {rendered:?}"
        );
    }
}

#[cfg(test)]
#[test]
fn a_running_test_case_failure_is_visible_before_the_binary_finishes() {
    let events = read_events(b"verify-event: {\"schema\":1,\"event\":\"test-case-failed\",\"outcome\":\"FAIL\",\"case\":\"broken\",\"binary_completed\":false}\n".as_slice()).unwrap();
    assert_eq!(
        events.first_failure.unwrap()["case"].as_str(),
        Some("broken")
    );
    assert!(!events.incomplete_tail);
}
