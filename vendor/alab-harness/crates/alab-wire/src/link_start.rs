// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Caller-owned fresh attempts under the optional C3 child protocol.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use super::*;

// Process lifetime, not socket lifetime. No transfer capsule carries this counter.
static CHILD_GENERATION: AtomicU64 = AtomicU64::new(0);

/// What the C3 pre-spawn hook established. No outcome grants input, process
/// custody, replacement permission or hosted own-image adoption.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChildStart {
    /// The host acknowledged and proved this pending generation before the attempt.
    Pending {
        /// Diagnostic identity; the Link owns its later completion.
        generation: NonZeroU64,
    },
    /// No negotiated C3 start barrier; existing C4 and absent-capability words stand.
    NoBarrier,
    /// No claim supports the attempt. A failed held C3 begin closed both sockets.
    Unclaimed(Failure),
}

fn next_generation(counter: &AtomicU64, observed: u64) -> Result<NonZeroU64, Failure> {
    counter.fetch_max(observed, Ordering::SeqCst);
    let previous = counter
        .try_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
            value.checked_add(1)
        })
        .map_err(|_| Failure::Refused("child-start generation-exhausted".into()))?;
    NonZeroU64::new(previous + 1)
        .ok_or_else(|| Failure::Refused("child-start generation-exhausted".into()))
}

fn ask_until(
    conn: &mut Connection,
    deadline: Instant,
    now: Instant,
    step: &str,
    request: Option<String>,
) -> Result<String, Failure> {
    deadline
        .checked_duration_since(now)
        .filter(|left| !left.is_zero())
        .ok_or_else(|| Failure::Gone("child-start deadline".into()))?;
    let request = request.ok_or_else(|| Failure::Refused(format!("{step} request-unsendable")))?;
    conn.request_line_until(&request, deadline)
        .map_err(|error| Failure::Gone(format!("{step} {error}")))
}

impl Claimed {
    fn prepare_child(
        &mut self,
        config: &Config,
        vendor: Option<&str>,
        counter: &AtomicU64,
        now: &mut impl FnMut() -> Instant,
    ) -> Result<NonZeroU64, Failure> {
        let deadline = now()
            .checked_add(REQUEST_TIMEOUT)
            .ok_or_else(|| Failure::Gone("child-start deadline overflow".into()))?;
        let sid = self.locator.sid().to_string();
        let status = ask_until(
            &mut self.conn,
            deadline,
            now(),
            "child-start-status",
            claim::status_verb(&sid),
        )?;
        let observed = c3_child::highest(&status).map_err(|why| Failure::Refused(why.into()))?;
        let generation = next_generation(counter, observed)?;
        let scopes = Contract::Three.scopes(config.scopes());
        let mut failure = None;
        for _ in 0..2 {
            let result = (|| {
                let reply = ask_until(
                    &mut self.conn,
                    deadline,
                    now(),
                    "child-start",
                    c3_child::claim(
                        config,
                        &sid,
                        self.pid,
                        Child {
                            vendor,
                            ..Child::default()
                        },
                        None,
                        Mode::Start,
                        Some(generation),
                    ),
                )?;
                if !ok(&reply) {
                    return Err(Failure::Refused(format!(
                        "child-start {}",
                        reply_words(&reply)
                    )));
                }
                let reply = ask_until(
                    &mut self.conn,
                    deadline,
                    now(),
                    "child-start-proof",
                    claim::status_verb(&sid),
                )?;
                let status = c3_child::pending(&reply, &self.holder, &scopes, generation)
                    .ok_or_else(|| Failure::Refused("child-start unproven-pending".into()))?;
                if now() >= deadline {
                    return Err(Failure::Gone("child-start deadline".into()));
                }
                Ok(status)
            })();
            match result {
                Ok(status) => {
                    self.native.generation = Some(generation);
                    self.native.pending = true;
                    self.grant = Grant::of_status(Contract::Three, &status, &scopes);
                    self.status = Some(status);
                    return Ok(generation);
                }
                Err(why) => {
                    let gone = matches!(why, Failure::Gone(_)) || self.conn.is_poisoned();
                    failure = Some(why);
                    if gone || now() >= deadline {
                        break;
                    }
                }
            }
        }
        Err(failure.unwrap_or_else(|| Failure::Gone("child-start deadline".into())))
    }
}

impl Link {
    /// Prepare every fresh fork/spawn attempt, including a failed attempt's
    /// fallback. The caller must first consume its prior original wait handle's
    /// terminal event and call [`Link::child_ended`]; an unknown observation is
    /// not retirement. This hook never ends a process or grants permission to do so.
    ///
    /// A negotiated begin uses one original 500-ms deadline for high-water sync,
    /// begin, pending proof and at most one same-generation retry. Failure closes
    /// both C3 sockets. It withdraws no cached host hold. Capability-absent C3
    /// and all C4 claims return `NoBarrier`.
    pub fn before_spawn(&mut self, vendor: Option<&str>, now_ms: u64) -> ChildStart {
        self.before_spawn_clock(vendor, now_ms, &CHILD_GENERATION, Instant::now)
    }

    fn before_spawn_clock(
        &mut self,
        vendor: Option<&str>,
        now_ms: u64,
        counter: &AtomicU64,
        mut now: impl FnMut() -> Instant,
    ) -> ChildStart {
        let result = match &mut self.state {
            State::Held(claimed) if claimed.grant.contract == Contract::Four => {
                return ChildStart::NoBarrier;
            }
            State::Held(_) if !self.config.c3_child_tracking() => return ChildStart::NoBarrier,
            State::Held(claimed) if claimed.native.known.is_some() => Err(Failure::Refused(
                "child-start prior child not retired".into(),
            )),
            State::Held(claimed) if !claimed.standing => {
                Err(Failure::Gone("child-start no-standing-claim".into()))
            }
            State::Held(claimed) if claimed.native.mode == Mode::Absent => {
                return ChildStart::NoBarrier;
            }
            State::Held(claimed) => {
                claimed.recheck_at_ms = 0;
                // A fresh attempt cannot inherit an earlier attempt's acknowledged
                // pending generation, even if this new begin fails before its ACK.
                claimed.native.generation = None;
                claimed.native.pending = true;
                if claimed.native.mode == Mode::Proof {
                    return ChildStart::NoBarrier;
                }
                claimed.prepare_child(&self.config, vendor, counter, &mut now)
            }
            State::Absent | State::Lost { .. } => {
                return ChildStart::Unclaimed(Failure::Gone(
                    "child-start no-standing-claim".into(),
                ));
            }
        };
        match result {
            Ok(generation) => {
                self.notes
                    .push(format!("aterm child-start generation={generation}"));
                ChildStart::Pending { generation }
            }
            Err(why) => {
                self.lose(why.why(), now_ms);
                self.notes
                    .push(format!("aterm child-start unclaimed {}", why.why()));
                ChildStart::Unclaimed(why)
            }
        }
    }
}

#[cfg(test)]
#[path = "link_start_tests.rs"]
mod tests;
