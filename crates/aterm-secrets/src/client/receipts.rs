// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The network receipts `state.toml` retains, read together: a signed body's materialization, an
//! HTTP 204 response and a refresh outcome. Each is historical evidence: reading it fetches
//! nothing, renews no time and establishes no local usability.

use super::*;

/// What one observation of the vault's state lock found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Observed<T> {
    /// The files read under a shared observation of the lock.
    Read(T),
    /// A writer holds the lock, or the lock file was replaced as it was opened; nothing was read.
    Busy,
    /// No vault folder or no lock file in it is there; nothing was read or made.
    Absent,
}

/// The receipts of `state.toml` that still match the configuration, recipient and state they
/// were read with, each with only its closed fields.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Receipts {
    /// The last signed URL body admitted to local files, where its sequence, envelope and
    /// records are still the state's (`network_observation::project_for`).
    pub materialization: Option<VaultNetworkMaterialization>,
    /// The retained actual HTTP 204 response (`http204_observation::project`).
    pub http204: Option<VaultHttp204Response>,
    /// The last retained refresh outcome (`network_refresh::project`).
    pub refresh: Option<VaultNetworkRefresh>,
}

/// The three receipts of `state.toml`, read under one shared observation of the state lock and
/// one read each of the configuration ([`config_whole`]: where `source.toml` names no `followed`,
/// the chain its signer was reached by is the held envelope's), the identity and `state.toml`.
/// Busy while a writer holds the lock, and a read with no receipts where the vault configured is
/// not `expected` (no identity or state is read then). It never fetches, waits for a writer,
/// repairs a permission, writes a file or renews a time.
/// # Errors
/// The existing configuration, held envelope (for that chain), identity or state cannot be
/// safely read, or the lock cannot be observed (a vault folder that is a symlink, a lock mode
/// that lets other users read it, an I/O failure). A held envelope that cannot be read is this
/// read's error, never the empty chain [`config`] takes it as: every receipt is bound to the
/// configuration with its chain, so such a chain would project none of them, as though none was
/// retained.
pub fn observe_receipts(paths: &Paths, expected: &Config) -> crate::Result<Observed<Receipts>> {
    receipts_of(paths, Some(expected))
}

/// [`observe_receipts`] for whatever vault `source.toml` configures, its receipts projected for
/// that configuration: a read with no receipts where none is configured. A services reading
/// ([`super::service_reading::Reader`]) reads them so, with no configuration of its own to
/// expect.
/// # Errors
/// As [`observe_receipts`].
pub(super) fn observe_configured_receipts(paths: &Paths) -> crate::Result<Observed<Receipts>> {
    receipts_of(paths, None)
}

/// [`observe_receipts`] where `expected` names the configuration, and
/// [`observe_configured_receipts`] where it is `None`.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVaultHttp204",
        action = "Read",
        project = "aterm_secrets::client::http204_observation::conformance_tests::http204_transition"
    )
)]
fn receipts_of(paths: &Paths, expected: Option<&Config>) -> crate::Result<Observed<Receipts>> {
    let _guard = match StateLock::observe_classified(paths)? {
        StateObservation::Absent => return Ok(Observed::Absent),
        StateObservation::Busy => return Ok(Observed::Busy),
        StateObservation::Held(guard) => guard,
    };
    let configured = config_whole(paths)?;
    let Some(config) = configured
        .as_ref()
        .filter(|configured| expected.is_none_or(|expected| expected == *configured))
    else {
        return Ok(Observed::Read(Receipts::default()));
    };
    let recipient = read_identity(paths)?.map(|key| key.recipient().0);
    let state = load_state(paths)?;
    Ok(Observed::Read(Receipts {
        materialization: network_observation::project_for(config, recipient, &state),
        http204: http204_observation::project(config, recipient, &state),
        refresh: network_refresh::project(config, recipient, &state),
    }))
}
