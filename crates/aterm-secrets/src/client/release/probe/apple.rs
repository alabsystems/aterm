// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One Apple submission, with recovery by its exact nonce-named archive and receipt hash.

use super::{
    engine::{Outcome, Receipt},
    model::{Journal, submission_id},
};
use crate::{Error, Result};
use aterm_json::Value;
use aterm_uds::spawnfd::Command;
use std::path::Path;
use std::process::Output;
use std::time::Instant;

pub(super) trait Tool {
    fn run(
        &self,
        command: &mut Command,
        deadline: Instant,
    ) -> std::result::Result<Output, ToolError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ToolError {
    #[cfg(test)]
    NotStarted,
    #[cfg(test)]
    Uncertain,
    Classified {
        program: &'static str,
        step: &'static str,
        class: &'static str,
        not_started: bool,
    },
}

fn descriptor(command: &Command) -> (&'static str, &'static str) {
    let program = Path::new(command.get_program())
        .file_name()
        .and_then(|s| s.to_str());
    let has = |value: &str| command.get_args().any(|arg| arg == value);
    match program {
        Some("codesign") if has("--timestamp=none") => ("codesign", "prepare-ad-hoc"),
        Some("codesign")
            if has("--extract-certificates")
                || command.get_args().any(|arg| {
                    arg.as_encoded_bytes()
                        .starts_with(b"--extract-certificates=")
                }) =>
        {
            ("codesign", "certificate")
        }
        Some("codesign") if has("--verify") => ("codesign", "verify-signature"),
        Some("codesign") if has("--display") => ("codesign", "describe-signature"),
        Some("codesign") => ("codesign", "sign"),
        Some("probe") => ("probe", "execute"),
        Some("ditto") => ("ditto", "archive"),
        Some("security") => ("security", "keychain"),
        Some("notarytool") if has("submit") => ("notarytool", "submit"),
        Some("notarytool") if has("history") => ("notarytool", "history"),
        Some("notarytool") if has("info") => ("notarytool", "info"),
        Some("notarytool") => ("notarytool", "log"),
        _ => ("other", "subprocess"),
    }
}

pub(super) fn operation(command: &Command) -> String {
    let (program, step) = descriptor(command);
    format!("step={step} program={program}")
}

impl ToolError {
    pub(super) fn not_started(self) -> bool {
        match self {
            #[cfg(test)]
            Self::NotStarted => true,
            #[cfg(test)]
            Self::Uncertain => false,
            Self::Classified { not_started, .. } => not_started,
        }
    }

    pub(super) fn refused(command: &Command, class: &'static str) -> Self {
        let (program, step) = descriptor(command);
        Self::Classified {
            program,
            step,
            class,
            not_started: true,
        }
    }

    pub(super) fn observed(
        command: &Command,
        error: &aterm_uds::deadline::CompleteOutputError,
        started: bool,
    ) -> Self {
        use aterm_uds::deadline::{CompleteOutputError as E, RunError};
        let (class, not_started) = match error {
            E::Run(RunError::Spawn(_)) if !started => ("spawn", true),
            E::Run(RunError::Spawn(_)) => ("reader-spawn", false),
            E::Run(RunError::Capture(_)) => ("capture", false),
            E::Run(RunError::Wait(_)) => ("wait", false),
            E::Run(RunError::TimedOut) => ("deadline", false),
            E::StdoutRead(_) => ("stdout-read", false),
            E::StdoutTooLarge { .. } => ("stdout-too-large", false),
            E::StdoutIncomplete => ("stdout-incomplete", false),
        };
        let (program, step) = descriptor(command);
        Self::Classified {
            program,
            step,
            class,
            not_started,
        }
    }
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(test)]
            Self::NotStarted => f.write_str("probe subprocess did not start"),
            #[cfg(test)]
            Self::Uncertain => f.write_str("probe subprocess result is incomplete"),
            Self::Classified {
                program,
                step,
                class,
                ..
            } => {
                write!(f, "probe step={step} program={program} class={class}")
            }
        }
    }
}
impl From<ToolError> for Error {
    fn from(error: ToolError) -> Self {
        Self::new(error.to_string())
    }
}

pub(super) struct Notary<'a> {
    pub tool: &'a dyn Tool,
    pub program: &'a Path,
    pub profile: &'a str,
    pub archive: &'a Path,
    pub journal: &'a Journal,
}

impl Notary<'_> {
    fn command(&self, verb: &str) -> Command {
        let mut command = aterm_uds::spawnfd::command(self.program);
        command.arg(verb);
        command
    }

    fn json(&self, command: &mut Command, deadline: Instant) -> Result<Option<Value>> {
        let output = self.tool.run(command, deadline)?;
        if !output.status.success() {
            return Ok(None);
        }
        let json = aterm_json::from_slice(&output.stdout)
            .map_err(|_| Error::new("Apple probe response is not complete JSON"))?;
        Ok(Some(json))
    }

    pub(super) fn submit(&self, deadline: Instant) -> Result<Outcome> {
        let mut command = self.command("submit");
        command.arg(self.archive).args([
            "--keychain-profile",
            self.profile,
            "--output-format",
            "json",
            "--no-wait",
        ]);
        // Any error after dispatch may still have created a submission. Even an
        // unsuccessful child is followed only by history observation, never resubmission.
        let output = match self.tool.run(&mut command, deadline) {
            Ok(output) => output,
            Err(error) if error.not_started() => {
                return Ok(Outcome::NotStarted("Apple submission did not start"));
            }
            Err(_) => {
                return Ok(Outcome::Pending("Apple submission needs observation"));
            }
        };
        if !output.status.success() {
            return Ok(Outcome::Pending("Apple submission needs observation"));
        }
        let value: Value = aterm_json::from_slice(&output.stdout)
            .map_err(|_| Error::new("Apple submission response is not complete JSON"))?;
        let id = value
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| submission_id(id))
            .ok_or_else(|| Error::new("Apple submission returned no valid identity"))?;
        Ok(Outcome::Applied(Receipt::Submitted(id.to_owned())))
    }

    pub(super) fn observe_submission(&self, deadline: Instant) -> Result<Outcome> {
        let mut command = self.command("history");
        command.args([
            "--keychain-profile",
            self.profile,
            "--output-format",
            "json",
        ]);
        let Some(value) = self.json(&mut command, deadline)? else {
            return Ok(Outcome::Pending("Apple submission history is unavailable"));
        };
        let name = self
            .archive
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| Error::new("invalid owned probe archive name"))?;
        let rows = value
            .get("history")
            .and_then(Value::as_array)
            .filter(|rows| rows.len() <= 1000)
            .ok_or_else(|| Error::new("Apple submission history exceeds its bound"))?;
        let matches: Vec<_> = rows
            .iter()
            .filter(|row| row.get("name").and_then(Value::as_str) == Some(name))
            .collect();
        match matches.as_slice() {
            [] => Ok(Outcome::Pending("Apple submission remains unconfirmed")),
            [row] => {
                let id = row
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| submission_id(id))
                    .ok_or_else(|| {
                        Error::new("Apple history has an invalid submission identity")
                    })?;
                Ok(Outcome::Applied(Receipt::Submitted(id.to_owned())))
            }
            _ => Err(Error::new("Apple history has ambiguous probe submissions")),
        }
    }

    pub(super) fn accepted(&self, deadline: Instant) -> Result<Outcome> {
        let id = self
            .journal
            .submission
            .as_deref()
            .filter(|id| submission_id(id))
            .ok_or_else(|| Error::new("probe has no valid Apple submission identity"))?;
        let mut command = self.command("info");
        command.args([
            id,
            "--keychain-profile",
            self.profile,
            "--output-format",
            "json",
        ]);
        let Some(value) = self.json(&mut command, deadline)? else {
            return Ok(Outcome::Pending(
                "Apple submission information is unavailable",
            ));
        };
        if value.get("id").and_then(Value::as_str) != Some(id) {
            return Err(Error::new("Apple information names another submission"));
        }
        match value.get("status").and_then(Value::as_str) {
            Some("In Progress") => return Ok(Outcome::Pending("Apple submission is processing")),
            Some("Accepted") => {}
            _ => return Err(Error::new("Apple did not accept the probe artifact")),
        }
        let mut command = self.command("log");
        command.args([id, "--keychain-profile", self.profile]);
        let Some(log) = self.json(&mut command, deadline)? else {
            return Ok(Outcome::Pending("Apple acceptance log is unavailable"));
        };
        let artifact = self.journal.artifact.as_ref().unwrap();
        let name = self
            .archive
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if log.get("jobId").and_then(Value::as_str) != Some(id)
            || log.get("status").and_then(Value::as_str) != Some("Accepted")
            || log.get("statusCode").and_then(Value::as_u64) != Some(0)
            || log.get("sha256").and_then(Value::as_str) != Some(artifact.sha256.as_str())
            || log.get("archiveFilename").and_then(Value::as_str) != Some(name)
        {
            return Err(Error::new(
                "Apple acceptance does not match this probe archive",
            ));
        }
        Ok(Outcome::Applied(Receipt::Accepted {
            submission: id.to_owned(),
            sha256: artifact.sha256.clone(),
        }))
    }
}

#[cfg(test)]
mod tests;
