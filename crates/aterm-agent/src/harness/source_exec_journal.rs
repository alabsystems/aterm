// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Durable possible-execution history for one live source replacement. This
//! journal grants no execution, SDK construction, model work or recovery. Its
//! owning native operation retains those live proofs and the original images.
//! A failed write is unknown, never evidence that execution did not happen.

use aterm_journal::{Limits, Log};
use aterm_secrets::client::Paths;
use ring::hmac;
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

const SUBJECT: &str = "native/source-exec/1";
const MAGIC: &[u8; 4] = b"ASE1";
const BODY: usize = 4 + 1 + 32 + 32 + 8 + 32;
const LIMITS: Limits = Limits {
    records: 4,
    bytes: 4 * (BODY + SUBJECT.len() + aterm_journal::APPEND_OVERHEAD),
    record_bytes: BODY,
};

/// Exact immutable host-copy identity. These bytes are data, not admission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub digest: [u8; 32],
    pub bytes: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Prepared = 0,
    MayExecute = 1,
    Activated = 2,
    Settled = 3,
}

/// One original open writer. Neither an acknowledgement nor the on-disk record
/// can reconstruct a native execution grant after its owner is lost.
pub struct Journal {
    log: Log,
    key: hmac::Key,
    intent: [u8; 32],
    phase: Phase,
    attempted: Option<Snapshot>,
    sequence: u64,
    go_sequence: Option<u64>,
    activation_sequence: Option<u64>,
}

impl std::fmt::Debug for Journal {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.write_str("SourceExecutionHistory(<private>)")
    }
}

fn invalid() -> io::Error {
    io::Error::other("native source execution history unavailable")
}

impl Journal {
    /// Create a new native-owned history before source teardown. `intent` must
    /// bind the exact original/target images, source kernel generation, claim,
    /// command (including its fresh constructor), primary and deadline.
    ///
    /// # Errors
    /// Refuses missing machine identity, nonprivate storage, entropy failures
    /// and failed durable publication. Never substitutes an ephemeral history.
    pub fn prepare(intent: &[u8]) -> io::Result<Self> {
        let paths = Paths::locate().map_err(|_| invalid())?;
        let identity = aterm_secrets::client::read_machine_identity(&paths)
            .map_err(|_| invalid())?
            .ok_or_else(invalid)?;
        Self::create_at(
            &paths.dir.join("native-source-exec"),
            identity.secret_bytes(),
            intent,
        )
    }

    fn create_at(root: &Path, secret: &[u8], intent: &[u8]) -> io::Result<Self> {
        if intent.is_empty() || intent.len() > 128 * 1024 {
            return Err(invalid());
        }
        aterm_journal::create_private_dir_durable(root)?;
        // Each live owner has its own writer, so an upgrade in another session
        // never waits for this operation's entire lifetime. A caller supplies
        // no pathname and cannot select or overwrite an older operation.
        let nonce = aterm_uds::rand::hex_token::<16>()?;
        let dir = root.join(&nonce);
        if dir.try_exists()? {
            return Err(invalid());
        }
        aterm_journal::create_private_dir_durable(&dir)?;
        let machine = hmac::Key::new(hmac::HMAC_SHA256, secret);
        let mut domain = b"aterm/native-source-exec/1\0".to_vec();
        domain.extend_from_slice(nonce.as_bytes());
        let derived = hmac::sign(&machine, &domain);
        let key = hmac::Key::new(hmac::HMAC_SHA256, derived.as_ref());
        let intent: [u8; 32] = hmac::sign(&key, intent).as_ref().try_into().unwrap();
        let mut anchor = domain;
        anchor.extend_from_slice(&intent);
        let mut log = Log::open_until(
            &dir,
            "source",
            &anchor,
            false,
            LIMITS,
            Instant::now() + Duration::from_secs(1),
        )?;
        // The random namespace must be fresh. Replaying another writer here
        // would not establish any of its original live admission evidence.
        if !log.read_range(astream_wire::Offset(0), 1)?.is_empty() {
            return Err(invalid());
        }
        log.finish_open()?;
        let body = encode(&key, intent, Phase::Prepared, None);
        let sequence = log.append(SUBJECT, body)?.0;
        Ok(Self {
            log,
            key,
            intent,
            phase: Phase::Prepared,
            attempted: None,
            sequence,
            go_sequence: None,
            activation_sequence: None,
        })
    }

    /// True from the first possible-execution write attempt, even when its
    /// durability barrier fails. Callers must retain data and image custody.
    #[must_use]
    pub fn possible_execution(&self) -> bool {
        self.attempted.is_some()
    }

    /// Commit possible execution BEFORE granting the fresh SDK constructor or
    /// publishing any executable command. Exact retries use the original row.
    ///
    /// # Errors
    /// Refuses changed snapshot identity, invalid size, damaged custody and
    /// failed durability. An error never retracts an earlier successful call.
    pub fn may_execute(&mut self, snapshot: Snapshot) -> io::Result<u64> {
        if snapshot.bytes == 0 || snapshot.bytes > 256 * 1024 * 1024 {
            return Err(invalid());
        }
        if self.attempted.is_some_and(|old| old != snapshot) {
            return Err(invalid());
        }
        if matches!(
            self.phase,
            Phase::MayExecute | Phase::Activated | Phase::Settled
        ) {
            return if self.log.is_poisoned() {
                Err(invalid())
            } else {
                self.go_sequence.ok_or_else(invalid)
            };
        }
        self.attempted = Some(snapshot);
        let body = encode(&self.key, self.intent, Phase::MayExecute, Some(snapshot));
        self.sequence = self.log.append(SUBJECT, body)?.0;
        self.go_sequence = Some(self.sequence);
        self.phase = Phase::MayExecute;
        Ok(self.sequence)
    }

    /// Record completed restoration only after the native owner verifies the
    /// actual replacement's fresh constructor and current activation policy.
    /// This record itself never supplies those proofs or releases model work.
    ///
    /// # Errors
    /// Refuses activation before a committed possible-execution row or after
    /// journal custody is damaged. Unknown writes retain the original data.
    pub fn activated(&mut self) -> io::Result<u64> {
        if self.phase == Phase::Prepared || self.log.is_poisoned() {
            return Err(invalid());
        }
        if matches!(self.phase, Phase::Activated | Phase::Settled) {
            return self.activation_sequence.ok_or_else(invalid);
        }
        let body = encode(&self.key, self.intent, Phase::Activated, self.attempted);
        self.sequence = self.log.append(SUBJECT, body)?.0;
        self.phase = Phase::Activated;
        self.activation_sequence = Some(self.sequence);
        Ok(self.sequence)
    }

    /// Record explicit successful handover before releasing native custody.
    /// The caller must independently verify the live activated connection.
    ///
    /// # Errors
    /// Refuses settlement before activation or with damaged journal custody.
    pub fn settled(&mut self) -> io::Result<u64> {
        if !matches!(self.phase, Phase::Activated | Phase::Settled) || self.log.is_poisoned() {
            return Err(invalid());
        }
        if self.phase == Phase::Settled {
            return Ok(self.sequence);
        }
        let body = encode(&self.key, self.intent, Phase::Settled, self.attempted);
        self.sequence = self.log.append(SUBJECT, body)?.0;
        self.phase = Phase::Settled;
        Ok(self.sequence)
    }
}

fn encode(key: &hmac::Key, intent: [u8; 32], phase: Phase, data: Option<Snapshot>) -> Vec<u8> {
    let mut body = Vec::with_capacity(BODY);
    body.extend_from_slice(MAGIC);
    body.push(phase as u8);
    body.extend_from_slice(&intent);
    body.extend_from_slice(&data.map_or([0; 32], |value| value.digest));
    body.extend_from_slice(&data.map_or(0, |value| value.bytes).to_le_bytes());
    body.extend_from_slice(hmac::sign(key, &body).as_ref());
    body
}

#[cfg(test)]
#[path = "source_exec_journal_tests.rs"]
mod tests;
