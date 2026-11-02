// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read-only release progress, including failures emitted before verification ends.

use std::io::Read;
use std::path::Path;

use crate::ledger::{Error, GitCli};

#[derive(Default)]
struct Events {
    first_failure: Option<aterm_json::Value>,
    latest: Option<aterm_json::Value>,
    count: u64,
    rejected_lines: u64,
    incomplete_tail: bool,
}

fn failed(event: &aterm_json::Value) -> bool {
    let failure = |value: &aterm_json::Value| matches!(value.as_str(), Some("FAIL" | "NOT RUN"));
    failure(&event["outcome"])
        || event["decisions"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| failure(&row["outcome"])))
}

fn read_events(mut reader: impl Read) -> std::io::Result<Events> {
    // A test may print an arbitrarily long line. Never retain its whole output
    // just to inspect the verifier's small structured records.
    const LIMIT: usize = 64 * 1024;
    let mut result = Events::default();
    let mut line = Vec::new();
    let mut overflow = false;
    let mut bytes = [0; 8192];
    loop {
        let n = reader.read(&mut bytes)?;
        if n == 0 {
            result.incomplete_tail = overflow || !line.is_empty();
            return Ok(result); // A partially written final record is not a decision.
        }
        for &byte in &bytes[..n] {
            if byte != b'\n' {
                if line.len() < LIMIT && !overflow {
                    line.push(byte);
                } else {
                    overflow = true;
                }
                continue;
            }
            if overflow {
                result.rejected_lines += 1;
            } else if let Some(json) = line
                .strip_prefix(b"verify-event: ")
                .or_else(|| line.strip_prefix(b"release-event: "))
            {
                match aterm_json::from_slice::<aterm_json::Value>(json) {
                    Ok(event)
                        if event["schema"].as_u64() == Some(1)
                            && matches!(
                                event["event"].as_str(),
                                Some(
                                    "stage-finished"
                                        | "test-binary-finished"
                                        | "test-case-failed"
                                        | "release-stage"
                                )
                            ) =>
                    {
                        result.count += 1;
                        if result.first_failure.is_none() && failed(&event) {
                            result.first_failure = Some(event.clone());
                        }
                        result.latest = Some(event);
                    }
                    _ => result.rejected_lines += 1,
                }
            }
            line.clear();
            overflow = false;
        }
    }
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
    let logs = match holder.as_ref() {
        Some(holder) => Some(std::path::PathBuf::from(&holder.logs)),
        None => match std::fs::read_to_string(dir.join("latest-run")) {
            Ok(path) => Some(std::path::PathBuf::from(path)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(Error::new(error.to_string())),
        },
    };
    let log = logs.as_ref().map(|path| path.join("06-verify.log"));
    let events = match log.as_ref().map(std::fs::File::open) {
        Some(Ok(file)) => read_events(file).map_err(|error| Error::new(error.to_string()))?,
        Some(Err(error)) if error.kind() != std::io::ErrorKind::NotFound => {
            return Err(Error::new(error.to_string()));
        }
        _ => Events::default(),
    };
    let transcript = logs.as_ref().map(|path| path.join("transcript.log"));
    let release_events = match transcript.as_ref().map(std::fs::File::open) {
        Some(Ok(file)) => read_events(file).map_err(|error| Error::new(error.to_string()))?,
        Some(Err(error)) if error.kind() != std::io::ErrorKind::NotFound => {
            return Err(Error::new(error.to_string()));
        }
        _ => Events::default(),
    };
    let step = release_events
        .latest
        .as_ref()
        .and_then(|event| event["stage"].as_str())
        .map(str::to_string)
        .or_else(|| holder.as_ref().and_then(|holder| holder.step.clone()));
    let state = if holder.is_some() {
        "running"
    } else if journal.is_some() || logs.is_some() {
        "not-running"
    } else {
        "none"
    };
    let journals = assign_journal(journal, release_events.latest.as_ref(), holder.is_some());
    let journal = journals.current;
    let measurement_exception = journal
        .as_ref()
        .and_then(|journal| {
            journal.artifact.as_ref().map(|artifact| {
                crate::accepted_measure::accepted(
                    &GitCli::new(repo),
                    &journal.commit,
                    &artifact.macho_sha256,
                )
            })
        })
        .transpose()
        .map_err(Error::new)?
        .flatten();
    let value = aterm_json::json!({
        "schema": 1,
        "state": state,
        "journal_finished": journal.as_ref().is_some_and(crate::release::Journal::finished),
        "measurement_exception": measurement_exception,
        "journal": journal,
        "journal_binding": journals.binding,
        "previous_journal": journals.previous,
        "pid": holder.as_ref().map(|holder| holder.pid),
        "step": step,
        "release_log": transcript,
        "release_events": release_events.count,
        "latest_release_event": release_events.latest,
        "observations_complete": events.rejected_lines == 0 && release_events.rejected_lines == 0
            && !events.incomplete_tail && !release_events.incomplete_tail,
        "logs": logs,
        "verification_log": log,
        "verification_events": events.count,
        "first_reported_failure": events.first_failure.or(release_events.first_failure),
        "latest_verification_event": events.latest,
        "rejected_log_lines": events.rejected_lines,
    });
    if json {
        println!("{value}");
    } else {
        println!("release: {state}");
        if let Some(log) = &value["verification_log"].as_str() {
            println!("verification log: {log}");
        }
        if !value["first_reported_failure"].is_null() {
            println!(
                "first reported failure: {}",
                value["first_reported_failure"]
            );
        }
        if !value["latest_verification_event"].is_null() {
            println!(
                "latest verification event: {}",
                value["latest_verification_event"]
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_preflight_cannot_inherit_a_finished_releases_journal() {
        let old = crate::release::Journal {
            commit: "old-source".into(),
            version: "0.148.0".into(),
            done: vec!["check".into()],
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
            Ok(crate::cli::Cmd::ReleaseStatus { json: true })
        ));
        assert!(matches!(
            crate::cli::parse(&args(&["release", "status"])),
            Ok(crate::cli::Cmd::ReleaseStatus { json: false })
        ));
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
    fn oversized_and_invalid_lines_do_not_hide_a_later_unavailable_stage() {
        let mut bytes = vec![b'x'; 128 * 1024];
        bytes.extend_from_slice(b"\nverify-event: not json\nverify-event: {\"schema\":1,\"event\":\"stage-finished\",\"decisions\":[{\"outcome\":\"NOT RUN\"}]}\n");
        let events = read_events(bytes.as_slice()).unwrap();
        assert_eq!(events.rejected_lines, 2);
        assert_eq!(events.count, 1);
        assert!(events.first_failure.is_some());
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
