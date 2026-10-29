// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm secrets` ([`text`], [`age`], [`x25519`], [`envelope`], [`transport`], [`server`]), the
//! join phrase and its CPace ([`phrase`], [`pake`]), and the membership log ([`membership_log`]).
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
pub mod membership_log;
pub mod pake;
pub mod phrase;
pub mod swarm;
pub mod swarm_roster;
pub mod text;
pub mod transport;
pub mod x25519;

/// Memoize X25519 in this test binary ([`x25519::seam`]): a test that reads one vault at every
/// lock it takes computes each scalar and point once. A load-time hook, so before any test
/// thread; test and debug builds on Apple targets and Linux.
#[macro_export]
macro_rules! memoized_x25519_in_tests {
    () => {
        #[cfg(all(test, any(target_vendor = "apple", target_os = "linux")))]
        const _: () = {
            extern "C" fn memoize_x25519() {
                $crate::x25519::seam::memoize();
            }
            #[used]
            #[cfg_attr(
                target_vendor = "apple",
                unsafe(link_section = "__DATA,__mod_init_func,mod_init_funcs")
            )]
            #[cfg_attr(target_os = "linux", unsafe(link_section = ".init_array"))]
            static MEMOIZE_X25519: extern "C" fn() = memoize_x25519;
        };
    };
}

pub mod cli;
#[cfg(unix)]
pub mod client;
#[cfg(unix)]
pub mod converge;
#[cfg(unix)]
pub mod discover;
#[cfg(unix)]
pub mod fsx;
#[cfg(unix)]
pub mod heard;
#[cfg(unix)]
pub mod materialize;
#[cfg(unix)]
pub mod members;
#[cfg(unix)]
pub mod need;
#[cfg(unix)]
pub mod network;
#[cfg(unix)]
pub mod server;
#[cfg(unix)]
pub mod service;
pub mod sync_diagnostic;
#[cfg(unix)]
pub mod wake;

mod secret;
pub use secret::SecretBytes;

/// The ALab vault: what `aterm init` joins when it answers on this network.
pub const ALAB_DEFAULT_URL: &str = aterm_types::macos_setup::DEFAULT_VAULT_URL;

/// Every compiled-in ALab vault URL a client probes and joins across ([`ALAB_DEFAULT_URL`] first).
pub const ALAB_DEFAULT_URLS: &[&str] = aterm_types::macos_setup::DEFAULT_VAULT_URLS;

/// Whether `url` is one of [`ALAB_DEFAULT_URLS`].
#[must_use]
pub fn is_alab_default_url(url: &str) -> bool {
    ALAB_DEFAULT_URLS.contains(&url)
}

/// The ALab vault's name in a store ([`ALAB_DEFAULT_URL`]'s last segment).
pub(crate) const ALAB_VAULT: &str = "alab";

/// The ALab vault's signing key ([`ALAB_DEFAULT_URL`], [`ALAB_RETIRED`]).
pub const ALAB_SIGNER: &str = "gnUs9ywlj9oLJNA7jDsP6xGO34YTr7xR8ra22GXUoQ4=";

/// The keys the ALab vault rotated away from ([`ALAB_SIGNER`]).
pub const ALAB_RETIRED: &[&str] = &[];

/// ALab's genesis, by its digest in hex: what `aterm secrets genesis` printed when m22 signed
/// Genesis v2 (`docs/DESIGN-fluid-swarm-2026-10-28.md` F12). A build checks ALab's genesis by
/// this digest alone, never by a signer ([`membership_log::Log::alab`]). `None` until F12b
/// compiles it in; the check is dormant until then.
pub const ALAB_GENESIS: Option<&str> = None;

/// Reserved `[everyone]` vault entry carrying swarm labels, aliases, hints, and deny-lists.
pub const SWARM_ROSTER_ENTRY: &str = swarm_roster::ROSTER_ENTRY;

/// Reserved admin vault entry carrying steward policy, network id, and reserved labels.
pub const SWARM_POLICY_ENTRY: &str = swarm_roster::POLICY_ENTRY;

/// Reserved admin vault entry carrying the active steward lease (`holder`, `epoch`, `until`).
pub const SWARM_LEASE_ENTRY: &str = swarm_roster::LEASE_ENTRY;

/// Whether an entry is consumed in place and must never become a secret file.
pub fn reserved_name(name: &str) -> bool {
    let bare = name
        .strip_prefix("~/.secrets/")
        .or_else(|| {
            name.get(..11)
                .filter(|p| p.eq_ignore_ascii_case("~/.secrets/"))
                .and_then(|_| name.get(11..))
        })
        .unwrap_or(name);
    bare.eq_ignore_ascii_case("aterm-fleet-code")
        || bare.eq_ignore_ascii_case(SWARM_ROSTER_ENTRY)
        || bare.eq_ignore_ascii_case(SWARM_POLICY_ENTRY)
        || bare.eq_ignore_ascii_case(SWARM_LEASE_ENTRY)
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
    /// The local Commons control journal is held by another writer. No current
    /// control observation was obtained; retry after that writer releases it.
    JournalBusy,
    /// Another local Commons custody observation held its lock past a bounded
    /// wait. Nothing was observed; retry after it finishes.
    CustodyBusy,
    /// A connect to a private or link-local address (a vault on this LAN) found no route:
    /// what macOS answers while aterm may not yet reach the local network (its Local Network
    /// question is open, or was answered Don't Allow), and also what a LAN host that is down
    /// gives, which nothing here tells apart. Asked again shortly, it may answer; it is no
    /// evidence that no vault is there. Only on macOS, and only until a look heard the switch
    /// on (`transport::set_local_network_granted`): elsewhere nothing gates the local network,
    /// and once the switch is known on the host did not answer; either way the same failure is
    /// [`ErrorKind::Other`].
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
    fn the_compiled_alab_genesis_is_a_digest_when_there_is_one() {
        let compiled = crate::membership_log::genesis::digest_compiled().expect("64 hex digits");
        assert_eq!(compiled.is_some(), super::ALAB_GENESIS.is_some());
    }

    #[test]
    fn the_server_advertises_the_name_and_port_the_default_url_reaches() {
        let label = super::alab_advertised();
        assert_eq!(label, "alab-secrets");
        assert!(crate::envelope::valid_name(label));
        assert_eq!(
            super::ALAB_DEFAULT_URLS.first().copied(),
            Some(super::ALAB_DEFAULT_URL)
        );
        assert!(
            super::ALAB_DEFAULT_URL
                .starts_with(&format!("http://{label}.local:{}/v1/", super::DEFAULT_PORT)),
            "{}",
            super::ALAB_DEFAULT_URL
        );
    }
}
