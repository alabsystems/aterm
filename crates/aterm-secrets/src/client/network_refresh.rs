// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Retained refresh outcomes, independent of local usability and successful receipts.

use super::*;

/// How long a retained outcome stands before a pass that ends the same way is retained in its
/// place ([`retain`]): the subscriber's passes complete about every 25 s, and each one that
/// ended as the one before replaced `state.toml` for its completion time alone (2026-11-11).
const PERSIST_AGAIN_MS: u64 = 60_000;

/// Result of an actual network-refresh pass, without its endpoint or error payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VaultNetworkRefreshOutcome {
    /// A network response was followed by a completed refresh pass.
    Succeeded,
    /// The network refresh failed, even if a local-only repair succeeded.
    Failed,
}

/// Historical result for the current configuration and local recipient only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VaultNetworkRefresh {
    completed_at_ms: u64,
    outcome: VaultNetworkRefreshOutcome,
}

impl VaultNetworkRefresh {
    /// Original completion time; metadata reads never advance it. Persistence is coalesced
    /// ([`PERSIST_AGAIN_MS`]), so under one outcome this is a retained completion rather than
    /// the latest one.
    pub const fn completed_at_ms(self) -> u64 {
        self.completed_at_ms
    }

    /// Closed outcome; it does not establish local usability or remote currency.
    pub const fn outcome(self) -> VaultNetworkRefreshOutcome {
        self.outcome
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Attempt {
    configuration: [u8; 32],
    recipient: Option<[u8; 32]>,
}

impl Attempt {
    fn start(paths: &Paths, config: &Config) -> Option<Self> {
        matches!(config.source, Source::Url(_)).then_some(())?;
        Some(Self {
            configuration: network_observation::configuration_binding(config),
            recipient: read_identity(paths).ok()?.map(|key| key.recipient().0),
        })
    }
}

/// Evidence is set only after a transport call. A cache replacement cannot set `responded`.
#[derive(Default)]
pub(super) struct Refresh {
    started: bool,
    responded: bool,
    failed: bool,
}

impl Refresh {
    pub(super) fn observed(&mut self, result: &crate::Result<transport::ObservedFetch>) {
        self.started = true;
        match result {
            Err(_) => self.failed = true,
            Ok(observed) => {
                self.responded =
                    observed.http204.is_some() || matches!(observed.fetched, Fetched::Body(_));
                self.failed = matches!(observed.fetched, Fetched::Missing);
            }
        }
    }

    /// A selected peer body or completed source repair recovered the failed request.
    pub(super) fn peer_response(&mut self) {
        self.started = true;
        self.responded = true;
        self.failed = false;
    }

    /// A recursive refresh owns its own completion; the superseded pass publishes nothing.
    pub(super) fn delegated(&mut self) {
        self.started = false;
    }

    fn outcome(&self, completed: bool) -> Option<VaultNetworkRefreshOutcome> {
        if !self.started {
            None
        } else if !completed || self.failed {
            Some(VaultNetworkRefreshOutcome::Failed)
        } else if self.responded {
            Some(VaultNetworkRefreshOutcome::Succeeded)
        } else {
            // A normal unanswered long-poll neither creates nor clears a failure.
            None
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct Receipt {
    configuration: [u8; 32],
    recipient: Option<[u8; 32]>,
    observation: VaultNetworkRefresh,
}

/// Finish after the pass releases its state locks, including on early error returns.
/// Optional diagnostics never change the ordinary refresh result.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbVaultNetworkRefresh",
        action = "Case",
        reason = "The native fixture chooses transport outcome and captured configuration/recipient context"
    )
)]
pub(super) fn run(
    paths: &Paths,
    config: &Config,
    pass: impl FnOnce(&mut Refresh) -> crate::Result<SyncOutcome>,
) -> crate::Result<SyncOutcome> {
    let attempt = Attempt::start(paths, config);
    let mut refresh = Refresh::default();
    let result = pass(&mut refresh);
    if refresh.started && matches!(result, Ok(SyncOutcome::PutBack { .. })) {
        // PutBack returns only after the ordinary source publication completed.
        refresh.peer_response();
    }
    let completed = result.as_ref().is_ok_and(
        |outcome| !matches!(outcome, SyncOutcome::Applied { report, .. } if apply_failed(report)),
    );
    if let (Some(attempt), Some(outcome), Some(completed_at_ms)) = (
        attempt,
        refresh.outcome(completed),
        aterm_time::unix_millis(),
    ) {
        retain(paths, config, &attempt, outcome, completed_at_ms);
    }
    result
}

/// Completion order, not wall-clock sorting, chooses the last retained outcome.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVaultNetworkRefresh",
        action = "Complete",
        project = "aterm_secrets::client::network_refresh::conformance_tests::refresh_transition"
    )
)]
pub(super) fn record(
    state: &mut State,
    config: &Config,
    recipient: Option<[u8; 32]>,
    attempt: &Attempt,
    outcome: VaultNetworkRefreshOutcome,
    completed_at_ms: u64,
) -> bool {
    let configuration = network_observation::configuration_binding(config);
    if !matches!(config.source, Source::Url(_))
        || attempt.configuration != configuration
        || attempt.recipient != recipient
        || completed_at_ms == 0
        || completed_at_ms > i64::MAX as u64
    {
        return false;
    }
    state.network_refresh = Some(Receipt {
        configuration,
        recipient,
        observation: VaultNetworkRefresh {
            completed_at_ms,
            outcome,
        },
    });
    true
}

fn retain(
    paths: &Paths,
    expected: &Config,
    attempt: &Attempt,
    outcome: VaultNetworkRefreshOutcome,
    completed_at_ms: u64,
) {
    let _ = (|| -> crate::Result<()> {
        use std::os::unix::fs::MetadataExt as _;
        let Some(_guard) = StateLock::try_take_existing(paths)? else {
            return Ok(());
        };
        if config(paths)?.as_ref() != Some(expected) {
            return Ok(());
        }
        let metadata = std::fs::symlink_metadata(paths.state_file())
            .map_err(|error| Error::new(error.to_string()))?;
        if !metadata.is_file()
            || metadata.uid() != fsx::euid()
            || metadata.nlink() != 1
            || metadata.mode() & 0o7777 != 0o600
        {
            return Ok(());
        }
        let recipient = read_identity(paths)?.map(|key| key.recipient().0);
        let mut state = load_state(paths)?;
        let retained = state.network_refresh.clone();
        if record(
            &mut state,
            expected,
            recipient,
            attempt,
            outcome,
            completed_at_ms,
        ) && !stands(retained.as_ref(), state.network_refresh.as_ref())
        {
            save_state(paths, &state)?;
        }
        Ok(())
    })();
}

/// Whether the `retained` receipt still stands for the one a pass just `recorded`: the same
/// configuration, recipient and outcome, completed less than [`PERSIST_AGAIN_MS`] before it.
/// Another outcome, another context, a first receipt and one completed before the retained one
/// (the clock was set back) are retained at once.
fn stands(retained: Option<&Receipt>, recorded: Option<&Receipt>) -> bool {
    let (Some(retained), Some(recorded)) = (retained, recorded) else {
        return false;
    };
    retained.configuration == recorded.configuration
        && retained.recipient == recorded.recipient
        && retained.observation.outcome == recorded.observation.outcome
        && recorded
            .observation
            .completed_at_ms
            .checked_sub(retained.observation.completed_at_ms)
            .is_some_and(|since| since < PERSIST_AGAIN_MS)
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVaultNetworkRefresh",
        action = "Project",
        project = "aterm_secrets::client::network_refresh::conformance_tests::refresh_transition"
    )
)]
pub(super) fn project(
    config: &Config,
    recipient: Option<[u8; 32]>,
    state: &State,
) -> Option<VaultNetworkRefresh> {
    let receipt = state.network_refresh.as_ref()?;
    (matches!(config.source, Source::Url(_))
        && receipt.configuration == network_observation::configuration_binding(config)
        && receipt.recipient == recipient)
        .then_some(receipt.observation)
}

/// Read optional refresh history without fetching, repairing files or renewing its time.
/// # Errors
/// A writer holds the observation lock, or existing configuration, identity or state
/// cannot be safely read. Contention is not an absent retained observation.
pub fn observe_network_refresh(
    paths: &Paths,
    expected: &Config,
) -> crate::Result<Option<VaultNetworkRefresh>> {
    let _guard = match StateLock::observe_classified(paths)? {
        StateObservation::Absent => return Ok(None),
        StateObservation::Busy => return Err(Error::new("vault network-refresh history is busy")),
        StateObservation::Held(guard) => guard,
    };
    if config(paths)?.as_ref() != Some(expected) {
        return Ok(None);
    }
    let recipient = read_identity(paths)?.map(|key| key.recipient().0);
    Ok(project(expected, recipient, &load_state(paths)?))
}

impl Receipt {
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbVaultNetworkRefresh",
            action = "Reload",
            project = "aterm_secrets::client::network_refresh::conformance_tests::refresh_transition"
        )
    )]
    pub(super) fn parse(table: &aterm_toml::Table) -> Option<Self> {
        let table = table.get("network_refresh")?.as_table()?;
        if table.get("version")?.as_integer()? != 1 {
            return None;
        }
        let digest = |text: &str| {
            if text.len() != 64 {
                return None;
            }
            let bytes = aterm_codec::hex::decode_vec(text.as_bytes())?;
            <[u8; 32]>::try_from(bytes.as_slice()).ok()
        };
        let recipient = match table.get("recipient")?.as_str()? {
            "" => None,
            text => Some(digest(text)?),
        };
        let completed_at_ms = u64::try_from(table.get("completed_at_ms")?.as_integer()?).ok()?;
        if completed_at_ms == 0 {
            return None;
        }
        let outcome = match table.get("outcome")?.as_str()? {
            "succeeded" => VaultNetworkRefreshOutcome::Succeeded,
            "failed" => VaultNetworkRefreshOutcome::Failed,
            _ => return None,
        };
        Some(Self {
            configuration: digest(table.get("configuration")?.as_str()?)?,
            recipient,
            observation: VaultNetworkRefresh {
                completed_at_ms,
                outcome,
            },
        })
    }

    pub(super) fn write(&self, text: &mut String) {
        text.push_str("\n[network_refresh]\nversion = 1\n");
        text.push_str(&format!(
            "configuration = {}\nrecipient = {}\ncompleted_at_ms = {}\noutcome = {}\n",
            toml_str(&hex(&self.configuration)),
            toml_str(
                &self
                    .recipient
                    .map(|recipient| hex(&recipient))
                    .unwrap_or_default()
            ),
            self.observation.completed_at_ms,
            toml_str(match self.observation.outcome {
                VaultNetworkRefreshOutcome::Succeeded => "succeeded",
                VaultNetworkRefreshOutcome::Failed => "failed",
            }),
        ));
    }
}

#[cfg(test)]
mod conformance_tests;
#[cfg(test)]
mod tests;
