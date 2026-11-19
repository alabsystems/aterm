// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Debug fixture fault for one authenticated native source settlement.
use std::cell::RefCell;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Expectation {
    sid: String,
    primary: String,
    #[serde(rename = "sourceBuild")]
    source_build: u64,
    build: u64,
    pid: u32,
    birth: u64,
}

impl Expectation {
    #[cfg(target_os = "macos")]
    pub(super) fn original_peer(&self) -> (&str, u32, u64) {
        (&self.sid, self.pid, self.birth)
    }

    fn valid(&self) -> bool {
        self.pid > 1
            && self.birth > 0
            && self.source_build > 0
            && self.build > 0
            && self.source_build != self.build
            && self.primary.len() == 36
            && self.primary.bytes().enumerate().all(|(index, byte)| {
                if matches!(index, 8 | 13 | 18 | 23) {
                    byte == b'-'
                } else {
                    byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
                }
            })
            && self
                .primary
                .bytes()
                .any(|byte| matches!(byte, b'1'..=b'9' | b'a'..=b'f'))
    }

    fn matches(&self, native: &NativeObservation<'_>) -> bool {
        self.sid == native.sid
            && self.primary == native.primary
            && self.source_build == native.source_build
            && self.build == native.build
            && self.pid == native.pid
            && self.birth == native.birth
    }
}

/// Facts supplied by the real native handler after its ownership checks.
pub(crate) struct NativeObservation<'a> {
    pub sid: &'a str,
    pub operation: &'a str,
    pub primary: &'a str,
    pub source_build: u64,
    pub build: u64,
    pub pid: u32,
    pub birth: u64,
    pub generation: i32,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub(super) struct Plan {
    operation: String,
    primary: String,
    build: u64,
    pid: u32,
}

struct Prepared {
    plan: Plan,
    source_generation: i32,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub(super) struct Attempt {
    sid: String,
    plan: Plan,
    connection: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub(super) struct RetainedClaim {
    revision: u64,
    expires_us: u64,
}

#[derive(Clone, serde::Serialize)]
pub(super) struct Observation {
    attempt: Attempt,
    cleanup: Option<Cleanup>,
    readback: Option<Readback>,
    commits: u32,
    #[serde(rename = "settleRequests")]
    settle_requests: u32,
    #[serde(skip)]
    settle_generation: i32,
}

#[derive(Clone, serde::Serialize)]
struct Readback {
    operation: String,
    primary: String,
    build: u64,
    pid: u32,
    generation: i32,
    connection: u64,
    state: &'static str,
}

#[derive(Clone, serde::Serialize)]
struct Cleanup {
    retained_claim: Option<RetainedClaim>,
}

struct Armed {
    expected: Expectation,
    prepared: Option<Prepared>,
    settle_requests: u32,
    observation: Option<Observation>,
}

pub(super) struct Fault {
    sid: String,
    state: Mutex<Option<Armed>>,
}

impl Fault {
    pub(super) fn new(sid: String) -> Self {
        Self {
            sid,
            state: Mutex::new(None),
        }
    }

    pub(super) fn arm(&self, expected: Expectation) -> Result<(), &'static str> {
        if expected.sid != self.sid || !expected.valid() {
            return Err("invalid source settlement ACK loss plan");
        }
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        match state.as_ref() {
            Some(original) if original.expected != expected => {
                Err("source settlement ACK loss plan changed")
            }
            Some(_) => Ok(()),
            None => {
                *state = Some(Armed {
                    expected,
                    prepared: None,
                    settle_requests: 0,
                    observation: None,
                });
                Ok(())
            }
        }
    }

    fn bind_prepare(&self, native: &NativeObservation<'_>) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(armed) = state.as_mut()
            && armed.prepared.is_none()
            && armed.expected.matches(native)
            && aterm_codec::hex::is_lower(native.operation, 32)
        {
            armed.prepared = Some(Prepared {
                plan: Plan {
                    operation: native.operation.into(),
                    primary: native.primary.into(),
                    build: native.build,
                    pid: native.pid,
                },
                source_generation: native.generation,
            });
        }
    }

    pub(super) fn select(&self, line: &str, pid: u32, connection: u64) -> Option<Attempt> {
        let (sid, command) = line.strip_prefix('@')?.split_once(' ')?;
        let operation = command.strip_prefix("native-act settle source-exec operation=")?;
        if sid != self.sid {
            return None;
        }
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let armed = state.as_mut()?;
        let plan = &armed.prepared.as_ref()?.plan;
        if pid != plan.pid || operation != plan.operation {
            return None;
        }
        armed.settle_requests = armed.settle_requests.saturating_add(1);
        if armed.observation.is_some() {
            return None;
        }
        Some(Attempt {
            sid: self.sid.clone(),
            plan: plan.clone(),
            connection,
        })
    }

    pub(super) fn hold(&self, attempt: &Attempt, status: &[u8], generation: i32) -> bool {
        if status
            != format!(
                "OK native source-exec state=settled operation={} primary={} build={}\n",
                attempt.plan.operation, attempt.plan.primary, attempt.plan.build
            )
            .as_bytes()
        {
            return false;
        }
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let Some(armed) = state.as_mut() else {
            return false;
        };
        let Some(prepared) = armed.prepared.as_ref() else {
            return false;
        };
        if prepared.plan != attempt.plan
            || generation <= prepared.source_generation
            || armed.observation.is_some()
        {
            return false;
        }
        armed.observation = Some(Observation {
            attempt: attempt.clone(),
            cleanup: None,
            readback: None,
            commits: 1,
            settle_requests: 0,
            settle_generation: generation,
        });
        true
    }

    pub(super) fn record_cleanup(&self, attempt: &Attempt, retained_claim: Option<RetainedClaim>) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(observation) = state.as_mut().and_then(|armed| armed.observation.as_mut())
            && &observation.attempt == attempt
            && observation.cleanup.is_none()
        {
            observation.cleanup = Some(Cleanup { retained_claim });
        }
    }

    fn readback(&self, native: &NativeObservation<'_>, connection: u64) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(armed) = state.as_mut()
            && armed.expected.matches(native)
            && let Some(observation) = armed.observation.as_mut()
            && observation.attempt.plan.operation == native.operation
            && observation.attempt.connection != connection
            && observation.settle_generation == native.generation
            && observation.readback.is_none()
        {
            observation.readback = Some(Readback {
                operation: native.operation.into(),
                primary: native.primary.into(),
                build: native.build,
                pid: native.pid,
                generation: native.generation,
                connection,
                state: "settled",
            });
        }
    }

    pub(super) fn observation(&self) -> Option<Observation> {
        let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let armed = state.as_ref()?;
        let mut observation = armed.observation.clone()?;
        observation.settle_requests = armed.settle_requests;
        Some(observation)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Witness {
    Commit(i32),
    Eof(RetainedClaim),
}

thread_local! {
    static FIXTURE: RefCell<Option<Arc<Fault>>> = const { RefCell::new(None) };
    static OBSERVING: RefCell<Option<(Attempt, Option<Witness>)>> = const { RefCell::new(None) };
}

struct RestoreFixture(Option<Arc<Fault>>);

impl Drop for RestoreFixture {
    fn drop(&mut self) {
        let _ = FIXTURE.with(|slot| slot.replace(self.0.take()));
    }
}

pub(super) fn observe_dispatch<R>(fault: &Arc<Fault>, act: impl FnOnce() -> R) -> R {
    let _restore = RestoreFixture(FIXTURE.with(|slot| slot.replace(Some(Arc::clone(fault)))));
    act()
}

pub(crate) fn record_prepare(native: NativeObservation<'_>) {
    FIXTURE.with(|slot| {
        if let Some(fault) = slot.borrow().as_ref() {
            fault.bind_prepare(&native);
        }
    });
}

pub(crate) fn record_readback(native: NativeObservation<'_>, connection: u64) {
    FIXTURE.with(|slot| {
        if let Some(fault) = slot.borrow().as_ref() {
            fault.readback(&native, connection);
        }
    });
}

struct RestoreObservation(Option<(Attempt, Option<Witness>)>);

impl Drop for RestoreObservation {
    fn drop(&mut self) {
        let _ = OBSERVING.with(|slot| slot.replace(self.0.take()));
    }
}

pub(super) fn witness<R>(attempt: &Attempt, act: impl FnOnce() -> R) -> (R, Option<Witness>) {
    let _restore =
        RestoreObservation(OBSERVING.with(|slot| slot.replace(Some((attempt.clone(), None)))));
    let value = act();
    let observed = OBSERVING.with(|slot| slot.borrow().as_ref().and_then(|(_, proof)| *proof));
    (value, observed)
}

fn record(sid: &str, operation: &str, pid: u32, connection: u64, evidence: Witness) {
    OBSERVING.with(|slot| {
        if let Some((expected, observed)) = slot.borrow_mut().as_mut()
            && expected.sid == sid
            && expected.plan.operation == operation
            && expected.plan.pid == pid
            && expected.connection == connection
        {
            *observed = Some(evidence);
        }
    });
}

pub(crate) fn record_commit(
    sid: &str,
    operation: &str,
    pid: u32,
    connection: u64,
    generation: i32,
) {
    record(sid, operation, pid, connection, Witness::Commit(generation));
}

pub(crate) fn record_eof(
    sid: &str,
    operation: &str,
    pid: u32,
    connection: u64,
    revision: u64,
    expires_us: u64,
) {
    record(
        sid,
        operation,
        pid,
        connection,
        Witness::Eof(RetainedClaim {
            revision,
            expires_us,
        }),
    );
}

pub(super) fn await_peer_eof(
    stream: &aterm_uds::CtlStream,
    buffered: bool,
    stopping: &AtomicBool,
    deadline: Instant,
) -> io::Result<()> {
    if buffered
        || stopping.load(Ordering::Acquire)
        || aterm_uds::hangup::read_until(stream, &mut [0; 1], deadline)? != 0
        || stopping.load(Ordering::Acquire)
    {
        return Err(io::Error::other(
            "source settlement ACK loss did not observe SDK EOF",
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "native_source_settle_ack_fault_tests.rs"]
mod tests;
