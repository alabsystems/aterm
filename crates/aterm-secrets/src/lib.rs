// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm secrets` — one credentials file for every machine
//! (docs/DESIGN-secrets-2026-10-20.md).
//!
//! The secrets are one text file ([`text`]). An admin encrypts it per audience to the age
//! X25519 key of every machine allowed to read it ([`age`], over the first-party
//! [`x25519`]), signs the result with the vault's Ed25519 key ([`envelope`]) and publishes
//! it at a URL or a path ([`transport`]), where a keyless long-poll server ([`server`])
//! hands it out. Every machine verifies it against the key it pinned when it joined,
//! decrypts what is addressed to it and writes each secret into `~/.secrets`
//! ([`materialize`]); [`client`] is those flows, [`service`] keeps the subscriber and the
//! server running, [`cli`] is the verb.
//!
//! Nothing here prints, logs or returns in an error a secret value.

#![forbid(unsafe_code)]

#[cfg(test)]
aterm_durable::plain_durable_flushes_in_tests!();

pub mod age;
pub mod b64;
pub mod bech32;
pub mod envelope;
pub mod join;
pub mod text;
pub mod transport;
pub mod x25519;

pub mod cli;
#[cfg(unix)]
pub mod client;
#[cfg(unix)]
pub mod fsx;
#[cfg(unix)]
pub mod materialize;
#[cfg(unix)]
pub mod server;
#[cfg(unix)]
pub mod service;

mod secret;
pub use secret::SecretBytes;

/// The ALab vault: what `aterm init` offers when Enter is pressed. A name, never an
/// address: the server registers `alab-secrets.local` over mDNS wherever it runs.
pub const ALAB_DEFAULT_URL: &str = aterm_types::macos_setup::DEFAULT_VAULT_URL;

/// The ALab vault's signing key (standard padded base64 of the Ed25519 public key), the
/// pin a machine that accepts [`ALAB_DEFAULT_URL`] uses instead of trusting on first use,
/// and the pin `aterm secrets serve` holds for `alab`: the key `aterm secrets create` made
/// on m22 on 2026-10-20 (fingerprint `fed4-97da-a265-0615-9f56`). An empty pin would mean
/// the default URL is trusted on first use like any other.
///
/// After a rotation of the ALab vault the next release sets this to the new key and adds
/// the old one to [`ALAB_RETIRED`] (the rotating `aterm secrets` command prints both, for a
/// vault it knows as the ALab one by these keys). Every version carries the handoff chain
/// from the vault's first key, so a build that pins an older key still follows the vault; but
/// every admin held the old key, so the one removed can still sign with it, and until a build
/// carries both changes a new Mac trusts what it signs.
pub const ALAB_SIGNER: &str = "gnUs9ywlj9oLJNA7jDsP6xGO34YTr7xR8ra22GXUoQ4=";

/// The keys the ALab vault rotated away from (padded base64, oldest first), each one an
/// admin removed may still hold: no reader and no server takes an envelope one of them
/// signed, and a machine pinned to one follows [`ALAB_SIGNER`] instead, as does one whose pin
/// was reached through one's endorsement of a key neither here nor [`ALAB_SIGNER`] (the
/// removed admin's fork, followed before a build retired its key). So every key the vault
/// rotated away from belongs here, not only the latest: the vault's own successor of each
/// must be one this build knows. A retired key stays in the vault's handoff chain, where its
/// endorsement of the key after it is still what a machine pinned before it follows.
pub const ALAB_RETIRED: &[&str] = &[];

/// The port `aterm secrets serve` listens on unless told otherwise.
pub const DEFAULT_PORT: u16 = 7484;

/// The mDNS name `aterm secrets serve` registers on macOS unless told otherwise: the
/// `<label>.local` of [`ALAB_DEFAULT_URL`] (`alab-secrets`), so a Mac serving the default vault
/// answers the URL every new Mac's setup joins.
pub fn alab_advertised() -> &'static str {
    ALAB_DEFAULT_URL
        .split_once("://")
        .and_then(|(_, rest)| rest.split_once(".local"))
        .map_or("", |(label, _)| label)
}

/// An error that names what failed, never a secret value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

impl Error {
    /// An error with this message.
    #[inline]
    pub fn new(message: impl Into<String>) -> Self {
        Error(message.into())
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Error(error.to_string())
    }
}

/// This crate's result.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    #[test]
    fn the_alab_signer_is_the_ed25519_key_of_the_vault_m22_made() {
        let key = crate::b64::decode_padded(super::ALAB_SIGNER).expect("base64");
        let key = <[u8; 32]>::try_from(key.as_slice()).expect("32 bytes");
        assert_eq!(
            crate::envelope::fingerprint(&key),
            "fed4-97da-a265-0615-9f56"
        );
    }

    /// A retired entry that does not decode would retire nothing, and a retired
    /// `ALAB_SIGNER` would refuse every version the vault signs.
    #[test]
    fn every_retired_alab_key_is_a_key_and_none_is_the_signer() {
        for text in super::ALAB_RETIRED {
            let key = crate::b64::decode_padded(text).expect("canonical padded base64");
            assert_eq!(key.len(), 32, "{text}");
            assert_ne!(*text, super::ALAB_SIGNER, "the current key is retired");
        }
        let compiled = crate::envelope::Retired::compiled();
        assert_eq!(compiled.keys.len(), super::ALAB_RETIRED.len());
        assert_eq!(
            compiled.current.map(|key| crate::b64::encode_padded(&key)),
            Some(super::ALAB_SIGNER.to_string())
        );
    }

    /// What a server advertises by default is the name the default URL reaches, on the port
    /// it listens on by default.
    #[test]
    fn the_server_advertises_the_name_and_port_the_default_url_reaches() {
        let label = super::alab_advertised();
        assert_eq!(label, "alab-secrets");
        assert!(crate::envelope::valid_name(label));
        assert!(
            super::ALAB_DEFAULT_URL
                .starts_with(&format!("http://{label}.local:{}/v1/", super::DEFAULT_PORT)),
            "{}",
            super::ALAB_DEFAULT_URL
        );
    }
}
