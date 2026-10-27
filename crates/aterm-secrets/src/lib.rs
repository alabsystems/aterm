// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm secrets` ([`text`], [`age`], [`x25519`], [`envelope`], [`transport`], [`server`]).
//! Client and service flows ([`materialize`], [`client`], [`service`], [`cli`]).

#![forbid(unsafe_code)]
#![cfg_attr(
    all(unix, anchor_rules, any(test, debug_assertions)),
    feature(macro_attr)
)]

#[cfg(test)]
aterm_durable::plain_durable_flushes_in_tests!();

pub mod age;
pub mod b64;
pub mod bech32;
pub mod commons;
pub mod envelope;
pub mod join;
pub mod text;
pub mod transport;
pub mod x25519;

pub mod cli;
#[cfg(unix)]
pub mod client;
#[cfg(unix)]
pub mod converge;
#[cfg(unix)]
pub mod fsx;
#[cfg(unix)]
pub mod heard;
#[cfg(unix)]
pub mod materialize;
#[cfg(unix)]
pub mod network;
#[cfg(unix)]
pub mod server;
#[cfg(unix)]
pub mod service;

mod secret;
pub use secret::SecretBytes;

/// The ALab vault: what `aterm init` joins when it answers on this network.
pub const ALAB_DEFAULT_URL: &str = aterm_types::macos_setup::DEFAULT_VAULT_URL;

/// The ALab vault's name in a store ([`ALAB_DEFAULT_URL`]'s last segment).
#[cfg(any(unix, test))]
pub(crate) const ALAB_VAULT: &str = "alab";

/// The ALab vault's signing key ([`ALAB_DEFAULT_URL`], [`ALAB_RETIRED`]).
pub const ALAB_SIGNER: &str = "gnUs9ywlj9oLJNA7jDsP6xGO34YTr7xR8ra22GXUoQ4=";

/// The keys the ALab vault rotated away from ([`ALAB_SIGNER`]).
pub const ALAB_RETIRED: &[&str] = &[];

/// Whether an entry is consumed in place and must never become a secret file.
pub fn reserved_name(name: &str) -> bool {
    name.eq_ignore_ascii_case("aterm-fleet-code")
        || name.eq_ignore_ascii_case("~/.secrets/aterm-fleet-code")
        || commons::reserved_name(name)
}

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
pub struct Error(pub String, ErrorKind);

/// What an [`Error`] tells a caller beyond its text, where the caller acts on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ErrorKind {
    /// Anything else.
    #[default]
    Other,
    /// A connect to a private or link-local address (a vault on this LAN) found no route:
    /// what macOS answers while aterm may not yet reach the local network (its Local Network
    /// question is open, or was answered Don't Allow), and also what a LAN host that is down
    /// gives, which nothing here tells apart. Asked again shortly, it may answer; it is no
    /// evidence that no vault is there. Only on macOS: elsewhere nothing gates the local
    /// network, and the same failure is [`ErrorKind::Other`].
    LocalNetworkPending,
    /// A request that ran past its deadline: a long-poll the server still held, a lookup with
    /// no answer, a slow network or a host that sleeps, which nothing here tells apart.
    TimedOut,
    /// A publish the vault refused before storing it: a version not newer than the one it holds
    /// (another admin's publish came first), or another publish holding its store or path (a
    /// server's 409). It cannot have landed.
    NotStored,
    /// A publish the vault would not take at all, decided before it stored anything: a server
    /// that refused what it carries or the Mac that sent it (its 403) or holds its limit of
    /// vaults (507), or a first write to a path that does not hold the bytes afterwards (a
    /// missing or unwritable folder, another publisher's lock or file). It cannot have landed,
    /// and it is not a race with a publish of the same bytes: asked again, it gets the same
    /// answer until someone changes the server or the path.
    Refused,
}

impl Error {
    /// An error with this message.
    #[inline]
    pub fn new(message: impl Into<String>) -> Self {
        Error(message.into(), ErrorKind::Other)
    }

    /// An error of `kind` with this message.
    pub fn of_kind(kind: ErrorKind, message: impl Into<String>) -> Self {
        Error(message.into(), kind)
    }

    /// What kind of failure this is.
    pub fn kind(&self) -> ErrorKind {
        self.1
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
        Error::new(error.to_string())
    }
}

/// This crate's result.
pub type Result<T> = std::result::Result<T, Error>;

/// Write one line to stderr, the log a launchd or systemd job keeps. A log that cannot take it
/// (a full disk, a closed descriptor) loses the line rather than the job: `eprintln!` panics
/// when its write fails, which would end a server or subscriber over a log line.
#[cfg(unix)]
pub(crate) fn log_line(line: &str) {
    use std::io::Write as _;
    let _ = writeln!(std::io::stderr().lock(), "{line}");
}

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
