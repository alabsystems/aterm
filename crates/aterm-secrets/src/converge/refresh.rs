// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Refresh requests and observations, never authority over repository effects.

use serde::{Deserialize, Serialize};

/// An opaque identity of admitted repository inputs and refresh selection.
/// Checkout HEAD/index/object changes are deliberately excluded.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefreshScope([u8; 32]);

impl RefreshScope {
    /// A domain-separated digest supplied by the executable's input reader.
    #[must_use]
    pub const fn new(digest: [u8; 32]) -> Self {
        Self(digest)
    }
}

impl std::fmt::Debug for RefreshScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RefreshScope(..)")
    }
}

/// Published scheduling epochs from the account's existing converge receipt.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefreshVersion {
    /// The input transition most recently admitted.
    pub input: u64,
    /// The last admitted fresh attempt, including unfinished attempts.
    pub attempt: u64,
    /// The last completely fulfilled attempt.
    pub fulfilled: u64,
}

/// One retained request. A later source observation may replace the desired
/// scope, but cannot erase an earlier observed transition or forced boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefreshRequest {
    /// Expected inputs, when the producer could observe them.
    pub scope: Option<RefreshScope>,
    /// Input epoch observed before this still-unacknowledged transition.
    pub base_input: u64,
    /// A source transition occurred, even if its final bytes returned to an old value.
    pub changed: bool,
    /// Fulfillment must come from an attempt admitted strictly after this boundary.
    pub after_attempt: Option<u64>,
}

impl RefreshRequest {
    /// Ensure the current source has a recent completed refresh. Startup and
    /// periodic requests can share an already completed wave.
    #[must_use]
    pub const fn ensure(scope: Option<RefreshScope>, version: RefreshVersion) -> Self {
        Self {
            scope,
            base_input: version.input,
            changed: false,
            after_attempt: None,
        }
    }

    /// A relevant source transition, retained until this generation is fulfilled.
    #[must_use]
    pub const fn inputs(scope: RefreshScope, version: RefreshVersion) -> Self {
        Self {
            scope: Some(scope),
            base_input: version.input,
            changed: true,
            after_attempt: None,
        }
    }

    /// A new route, rejoin or explicit request. Capture this once at request begin,
    /// not again on every retry or after waiting for the align lock.
    #[must_use]
    pub const fn after(scope: Option<RefreshScope>, version: RefreshVersion) -> Self {
        Self {
            scope,
            base_input: version.input,
            changed: false,
            after_attempt: Some(version.attempt),
        }
    }

    /// Coalesce pending work without losing a transition or a later forced boundary.
    #[must_use]
    pub fn merge(self, newer: Self) -> Self {
        Self {
            scope: newer.scope.or(self.scope),
            base_input: match (self.changed, newer.changed) {
                (true, true) => self.base_input.max(newer.base_input),
                (true, false) => self.base_input,
                _ => newer.base_input,
            },
            changed: self.changed || newer.changed,
            after_attempt: self.after_attempt.max(newer.after_attempt),
        }
    }
}

/// One owned job's requested repository work.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum RepositoryRequest {
    /// Local acquisition/readiness repair, without forcing existing parents to refresh.
    #[default]
    Repair,
    /// Shared input generation or an explicit later freshness boundary.
    Refresh(RefreshRequest),
}

impl RepositoryRequest {
    /// The effect policy after the receipt has admitted or coalesced this request.
    #[must_use]
    pub const fn work(self) -> super::RepositoryWork {
        match self {
            Self::Repair => super::RepositoryWork::Repair,
            Self::Refresh(_) => super::RepositoryWork::Refresh,
        }
    }

    /// Keep the strongest pending work and the newest desired input scope.
    #[must_use]
    pub fn merge(self, newer: Self) -> Self {
        match (self, newer) {
            (Self::Refresh(older), Self::Refresh(newer)) => Self::Refresh(older.merge(newer)),
            (Self::Refresh(_), Self::Repair) => self,
            _ => newer,
        }
    }
}

/// Shared receipt facts observed without granting any effect authority.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RefreshObservation {
    /// Published input/attempt/fulfillment epochs.
    pub version: RefreshVersion,
    /// Input scope of the latest admitted generation.
    pub scope: Option<RefreshScope>,
    /// A request still owed, including a failed attempt's original boundary.
    pub pending: Option<RefreshRequest>,
    /// Remaining shared retry delay on the observer's current epoch clock.
    pub retry_ms: Option<u64>,
    /// Remaining time until the existing thirty-minute refresh boundary.
    pub recheck_ms: Option<u64>,
}

impl RefreshObservation {
    /// Whether a complete recent shared attempt acknowledges this exact request boundary.
    #[must_use]
    pub fn fulfills(self, request: RefreshRequest) -> bool {
        self.version.fulfilled > 0
            && self.pending.is_none()
            && self.recheck_ms.is_some_and(|remaining| remaining > 0)
            && request.scope.is_some_and(|scope| self.scope == Some(scope))
            && (!request.changed || self.version.input > request.base_input)
            && request
                .after_attempt
                .is_none_or(|boundary| self.version.fulfilled > boundary)
    }

    /// Shared backoff applies only if its retained request covers this scope and boundary.
    #[must_use]
    pub fn retry_for(self, request: RefreshRequest) -> Option<u64> {
        let pending = self.pending?;
        if pending.scope == request.scope
            && (!request.changed || (pending.changed && pending.base_input >= request.base_input))
            && pending.after_attempt >= request.after_attempt
        {
            self.retry_ms
        } else {
            None
        }
    }
}

#[cfg(test)]
#[path = "refresh_tests.rs"]
mod tests;
