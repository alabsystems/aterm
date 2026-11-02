// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Retained unsigned HTTP 204 responses, independent of vault admission and materialization.

use super::*;

const PERSIST_AGAIN_MS: u64 = 60_000;

/// A retained actual HTTP 204 response from the configured URL. It establishes neither a
/// signed vault version nor local materialization, remote currency or provider sign-in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VaultHttp204Response {
    responded_at_ms: u64,
}

impl VaultHttp204Response {
    /// This slice observes only the configured URL, never a selected peer or local path.
    pub const fn source(self) -> VaultNetworkSource {
        VaultNetworkSource::ConfiguredUrl
    }

    /// When the actual response completed, retained unchanged across reads and reloads.
    /// Persistence is coalesced, so this is a retained response rather than the latest one.
    pub const fn responded_at_ms(self) -> u64 {
        self.responded_at_ms
    }
}

/// Current local context and the exact request, captured before the transport call.
#[derive(Clone, Copy, Debug)]
pub(super) struct Request {
    configuration: [u8; 32],
    /// This process's local key context, independently of the request's subscriber header.
    /// The header itself is included in `binding`; neither identifies an authenticated server.
    recipient: Option<[u8; 32]>,
    binding: [u8; 32],
}

impl Request {
    pub(super) fn start(
        paths: &Paths,
        config: &Config,
        after: Option<u64>,
        subscriber: Option<&str>,
    ) -> Option<Self> {
        let Source::Url(url) = &config.source else {
            return None;
        };
        Some(Self {
            configuration: network_observation::configuration_binding(config),
            recipient: read_identity(paths).ok()?.map(|key| key.recipient().0),
            binding: transport::request_binding(url, after, subscriber),
        })
    }
}

#[derive(Clone, Debug)]
pub(super) struct Receipt {
    configuration: [u8; 32],
    recipient: Option<[u8; 32]>,
    responded_at_ms: u64,
    persisted_at_ms: u64,
}

/// Record only a real response bound to this exact request and still-matching local context.
/// Repeated observations in the same context coalesce writes for a minute; their old response
/// time stays unchanged until another actual response is retained.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVaultHttp204",
        action = "Receive",
        project = "aterm_secrets::client::http204_observation::conformance_tests::http204_transition"
    )
)]
pub(super) fn record(
    state: &mut State,
    config: &Config,
    recipient: Option<[u8; 32]>,
    request: &Request,
    reply: &transport::NoContentReply,
    persisted_at_ms: u64,
) -> bool {
    let configuration = network_observation::configuration_binding(config);
    if !matches!(config.source, Source::Url(_))
        || request.configuration != configuration
        || request.recipient != recipient
        || !reply.matches_binding(request.binding)
        || persisted_at_ms == 0
        || persisted_at_ms > i64::MAX as u64
    {
        return false;
    }
    if state.http204_response.as_ref().is_some_and(|previous| {
        previous.configuration == configuration
            && previous.recipient == recipient
            && persisted_at_ms.saturating_sub(previous.persisted_at_ms) < PERSIST_AGAIN_MS
    }) {
        return false;
    }
    state.http204_response = Some(Receipt {
        configuration,
        recipient,
        responded_at_ms: reply.responded_at_ms(),
        persisted_at_ms,
    });
    true
}

pub(super) fn project(
    config: &Config,
    recipient: Option<[u8; 32]>,
    state: &State,
) -> Option<VaultHttp204Response> {
    let receipt = state.http204_response.as_ref()?;
    (matches!(config.source, Source::Url(_))
        && receipt.configuration == network_observation::configuration_binding(config)
        && receipt.recipient == recipient)
        .then_some(VaultHttp204Response {
            responded_at_ms: receipt.responded_at_ms,
        })
}

/// Best effort only: a busy, absent, unsafe or unwritable state does not change sync's result.
/// This runs before any peer or cache replacement can erase the configured URL's response.
pub(super) fn retain(
    paths: &Paths,
    expected: &Config,
    request: &Request,
    reply: Option<&transport::NoContentReply>,
) {
    let Some(reply) = reply else {
        return;
    };
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
        let Some(now_ms) = aterm_time::unix_millis() else {
            return Ok(());
        };
        if record(&mut state, expected, recipient, request, reply, now_ms) {
            save_state(paths, &state)?;
        }
        Ok(())
    })();
}

/// Read retained route-response evidence under a nonblocking shared state guard. This does not
/// fetch, repair permissions, create state or require vault membership or materialization.
/// # Errors
/// The existing local configuration, identity or state cannot be safely read.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVaultHttp204",
        action = "Read",
        project = "aterm_secrets::client::http204_observation::conformance_tests::http204_transition"
    )
)]
pub fn observe_http204_response(
    paths: &Paths,
    expected: &Config,
) -> crate::Result<Option<VaultHttp204Response>> {
    let Some(_guard) = StateLock::observe(paths)? else {
        return Ok(None);
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
            machine = "DbVaultHttp204",
            action = "Reload",
            project = "aterm_secrets::client::http204_observation::conformance_tests::http204_transition"
        )
    )]
    pub(super) fn parse(table: &aterm_toml::Table) -> Option<Self> {
        let table = table.get("http204_response")?.as_table()?;
        let integer = |key| {
            table
                .get(key)?
                .as_integer()
                .and_then(|n| u64::try_from(n).ok())
                .filter(|n| *n > 0)
        };
        let digest = |text: &str| {
            if text.len() != 64 {
                return None;
            }
            let bytes = aterm_codec::hex::decode_vec(text.as_bytes())?;
            <[u8; 32]>::try_from(bytes.as_slice()).ok()
        };
        if integer("version")? != 1 {
            return None;
        }
        let recipient = match table.get("recipient")?.as_str()? {
            "" => None,
            text => Some(digest(text)?),
        };
        Some(Self {
            configuration: digest(table.get("configuration")?.as_str()?)?,
            recipient,
            responded_at_ms: integer("responded_at_ms")?,
            persisted_at_ms: integer("persisted_at_ms")?,
        })
    }

    pub(super) fn write(&self, text: &mut String) {
        text.push_str("\n[http204_response]\nversion = 1\n");
        text.push_str(&format!(
            "configuration = {}\nrecipient = {}\nresponded_at_ms = {}\npersisted_at_ms = {}\n",
            toml_str(&hex(&self.configuration)),
            toml_str(
                &self
                    .recipient
                    .map(|recipient| hex(&recipient))
                    .unwrap_or_default()
            ),
            self.responded_at_ms,
            self.persisted_at_ms
        ));
    }
}

#[cfg(test)]
mod conformance_tests;
#[cfg(test)]
mod tests;
