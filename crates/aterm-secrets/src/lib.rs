// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm secrets` ([`text`], [`age`], [`x25519`], [`envelope`], [`transport`], [`server`]).
//! Client and service flows ([`materialize`], [`client`], [`service`], [`cli`]).

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

/// The ALab vault: what `aterm init` offers when Enter is pressed.
pub const ALAB_DEFAULT_URL: &str = aterm_types::macos_setup::DEFAULT_VAULT_URL;

/// The ALab vault's signing key ([`ALAB_DEFAULT_URL`], [`ALAB_RETIRED`]).
pub const ALAB_SIGNER: &str = "gnUs9ywlj9oLJNA7jDsP6xGO34YTr7xR8ra22GXUoQ4=";

/// The keys the ALab vault rotated away from ([`ALAB_SIGNER`]).
pub const ALAB_RETIRED: &[&str] = &[];

/// The port `aterm secrets serve` listens on unless told otherwise.
pub const DEFAULT_PORT: u16 = 7484;

/// Default mDNS name registered by `aterm secrets serve` on macOS ([`ALAB_DEFAULT_URL`]).
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
