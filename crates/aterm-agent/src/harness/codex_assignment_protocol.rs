// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Closed, credential-free session-assignment requests. Parsed values are
//! correlation and evidence, never native grant or boundary authority.

use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Waiting,
    Prepared,
    Installing,
    AwaitingAck,
    Applied,
    Cancelled,
    Refused,
    Reconciling,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    Busy,
    Unavailable,
    Denied,
    StaleRevision,
    SourceChanged,
    TargetUnavailable,
    Policy,
    Cancelled,
    Superseded,
    NeedsReconciliation,
    UnsupportedConsumer,
    AuthenticationRequired,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionMode {
    Manual,
    Auto,
}

/// Public operation metadata; all credential material and principal hashes stay
/// on the private Source channel. This value itself grants no assignment.
pub struct Status {
    pub operation: String,
    pub phase: Phase,
    pub boundary: Option<String>,
    pub assignment_generation: Option<u64>,
    pub credential_generation: Option<u64>,
    pub target_ref: Option<String>,
    pub selection_mode: Option<SelectionMode>,
    pub reason: Option<Reason>,
}

#[derive(Serialize)]
pub struct SessionBinding {
    pub sid: String,
    pub process_pid: u32,
    pub process_birth: String,
    pub epoch: String,
}

impl Status {
    fn valid(&self) -> bool {
        let no_assignment = self.boundary.is_none()
            && self.assignment_generation.is_none()
            && self.credential_generation.is_none();
        if !word(&self.operation, 32)
            || self.boundary.as_ref().is_some_and(|v| !word(v, 32))
            || self.target_ref.as_ref().is_some_and(|v| !word(v, 64))
            || self.assignment_generation == Some(0)
            || self.credential_generation == Some(0)
        {
            return false;
        }
        if self.selection_mode.is_none() {
            return no_assignment
                && self.target_ref.is_none()
                && matches!(
                    self.phase,
                    Phase::Waiting | Phase::Cancelled | Phase::Refused
                );
        }
        match self.phase {
            Phase::Waiting => no_assignment,
            Phase::Prepared => no_assignment && self.target_ref.is_some() && self.reason.is_none(),
            Phase::Installing | Phase::AwaitingAck | Phase::Applied => {
                self.target_ref.is_some()
                    && self.boundary.is_some()
                    && self.assignment_generation.is_some()
                    && self.credential_generation.is_some()
                    && self.reason.is_none()
            }
            Phase::Reconciling | Phase::Cancelled | Phase::Refused => true,
        }
    }

    /// Encode one bounded status frame. Only the authenticated host supplies
    /// session metadata; encoding does not prove that binding or a phase change.
    pub fn encode(&self, session: &SessionBinding) -> Option<String> {
        fn safe(value: &str, limit: usize) -> bool {
            !value.is_empty() && value.len() <= limit && !value.chars().any(char::is_control)
        }
        if !self.valid()
            || session.process_pid <= 1
            || !safe(&session.sid, 64)
            || !session.sid.starts_with("s-")
            || !safe(&session.process_birth, 128)
            || !safe(&session.epoch, 128)
        {
            return None;
        }
        #[derive(Serialize)]
        struct Frame<'a> {
            schema: u8,
            operation: &'a str,
            native_intent: &'a str,
            session: &'a SessionBinding,
            phase: Phase,
            boundary: &'a Option<String>,
            assignment_generation: Option<u64>,
            credential_generation: Option<u64>,
            target_ref: &'a Option<String>,
            selection_mode: Option<SelectionMode>,
            reason: Option<Reason>,
        }
        let body = aterm_json::to_string(&Frame {
            schema: 1,
            operation: &self.operation,
            native_intent: &self.operation,
            session,
            phase: self.phase,
            boundary: &self.boundary,
            assignment_generation: self.assignment_generation,
            credential_generation: self.credential_generation,
            target_ref: &self.target_ref,
            selection_mode: self.selection_mode,
            reason: self.reason,
        })
        .ok()?;
        let line = format!("OK codex-accounts-change assignment {body}\n");
        (line.len() <= 16 * 1024).then_some(line)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    Auto,
    Account(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthState {
    pub generation: u64,
    pub owner: u64,
    pub source: u64,
    pub login: u64,
}

#[derive(Clone, PartialEq, Eq)]
pub enum Command {
    Prepare {
        pool_revision: String,
        expected_assignment: u64,
        selection: Selection,
    },
    Commit {
        boundary: String,
        source_credential_generation: u64,
        auth: AuthState,
    },
    Poll,
    Cancel,
    Ack {
        boundary: String,
        assignment_generation: u64,
        credential_generation: u64,
        digest: String,
        account: String,
        user: String,
        auth: AuthState,
    },
}

#[derive(Clone, PartialEq, Eq)]
pub struct Request {
    pub operation: String,
    pub command: Command,
}

impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Prepare { .. } => "AssignmentPrepare(redacted)",
            Self::Commit { .. } => "AssignmentCommit(redacted)",
            Self::Poll => "AssignmentPoll",
            Self::Cancel => "AssignmentCancel",
            Self::Ack { .. } => "AssignmentAck(redacted)",
        })
    }
}

impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AssignmentRequest(redacted)")
    }
}

fn word(value: &str, count: usize) -> bool {
    value.len() == count
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn take_hex(fields: &mut BTreeMap<&str, &str>, key: &str, count: usize) -> Option<String> {
    let value = fields.remove(key)?;
    word(value, count).then(|| value.to_owned())
}

fn take_count(fields: &mut BTreeMap<&str, &str>, key: &str, positive: bool) -> Option<u64> {
    let value = fields.remove(key)?;
    if value.is_empty()
        || (value.len() > 1 && value.starts_with('0'))
        || !value.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let count = value.parse().ok()?;
    (!positive || count > 0).then_some(count)
}

fn take_auth(fields: &mut BTreeMap<&str, &str>) -> Option<AuthState> {
    Some(AuthState {
        generation: take_count(fields, "auth_generation", false)?,
        owner: take_count(fields, "owner_generation", false)?,
        source: take_count(fields, "source_generation", false)?,
        login: take_count(fields, "login_generation", false)?,
    })
}

impl Request {
    /// Decode the exact command tail after `codex-accounts-change`. No unknown,
    /// duplicate, noncanonical or unbounded field is accepted.
    pub fn parse(input: &str) -> Option<Self> {
        if input.is_empty()
            || input.len() > 4096
            || !input.is_ascii()
            || input.bytes().any(|b| b.is_ascii_control())
        {
            return None;
        }
        let mut words = input.split(' ');
        let verb = words.next()?;
        let mut fields = BTreeMap::new();
        for pair in words {
            let (key, value) = pair.split_once('=')?;
            if key.is_empty() || value.is_empty() || fields.insert(key, value).is_some() {
                return None;
            }
        }
        if fields.remove("schema")? != "1" {
            return None;
        }
        let operation = take_hex(&mut fields, "operation", 32)?;
        let command = match verb {
            "assignment-prepare" => {
                let pool_revision = take_hex(&mut fields, "pool_revision", 64)?;
                let expected_assignment = take_count(&mut fields, "expected_assignment", false)?;
                let selection = match fields.remove("selection")? {
                    "auto" => Selection::Auto,
                    value if word(value, 64) => Selection::Account(value.to_owned()),
                    _ => return None,
                };
                Command::Prepare {
                    pool_revision,
                    expected_assignment,
                    selection,
                }
            }
            "assignment-commit" => Command::Commit {
                boundary: take_hex(&mut fields, "boundary", 32)?,
                source_credential_generation: take_count(
                    &mut fields,
                    "source_credential_generation",
                    true,
                )?,
                auth: take_auth(&mut fields)?,
            },
            "assignment-poll" => Command::Poll,
            "assignment-cancel" => Command::Cancel,
            "assignment-ack" => Command::Ack {
                boundary: take_hex(&mut fields, "boundary", 32)?,
                assignment_generation: take_count(&mut fields, "assignment_generation", true)?,
                credential_generation: take_count(&mut fields, "credential_generation", true)?,
                digest: take_hex(&mut fields, "digest", 64)?,
                account: take_hex(&mut fields, "account", 64)?,
                user: take_hex(&mut fields, "user", 64)?,
                auth: take_auth(&mut fields)?,
            },
            _ => return None,
        };
        fields.is_empty().then_some(Self { operation, command })
    }
}

#[cfg(test)]
#[path = "codex_assignment_protocol_tests.rs"]
mod tests;
