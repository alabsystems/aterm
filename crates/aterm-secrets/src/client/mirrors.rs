// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A vault's other addresses: `mirrors` in `source.toml` (DESIGN-secrets §7 Sync, §10), for a
//! standalone vault, which publishes no `SwarmPolicy::servers`. `url` stays the vault's one
//! source, and a build before this one reads the file as its `url` and nothing else. No join,
//! re-ask, verdict, put-back, records post or admin publish goes to a mirror. A pass reads each mirror beside
//! the vault's other servers ([`super::collect_sync_peer_urls`]) under the one pin `source.toml`
//! keeps and the one floor `state.toml` keeps, and writes nothing to it ([`ReadOnly`]). No entry
//! fails the read of `source.toml`: one this build does not keep costs that address alone.

use super::{
    Config, Error, Paths, Source, StateLock, alab_vault, config, policy_peer_urls, read_toml,
    save_config,
};
use crate::discover::same_endpoint;
use crate::transport;
use std::path::Path;

#[cfg(test)]
mod tests;

/// The most mirrors `source.toml` keeps.
pub const MAX_MIRRORS: usize = 7;

/// The longest mirror URL kept, as a member's bundle bounds its server's.
const MAX_URL: usize = 512;

/// The most characters of an entry left out that a note shows ([`mirror_notes`]).
const SHOWN_CHARS: usize = 120;

/// What `source.toml`'s `mirrors` holds, as this build reads it ([`sorted`]).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct Sorted {
    /// The entries kept, in written order.
    pub(super) kept: Vec<String>,
    /// Each entry left out (as a person reads it), and why.
    pub(super) dropped: Vec<(String, String)>,
}

/// `text` as a person reads it in a note: URL credentials omitted, printable ASCII,
/// each other character a `?`, cut short.
fn shown(text: &str) -> String {
    let safe;
    let text = if text.contains("://") {
        safe = transport::describe(text.trim());
        safe.as_str()
    } else {
        text
    };
    let mut out: String = text
        .chars()
        .take(SHOWN_CHARS)
        .map(|c| if (' '..='~').contains(&c) { c } else { '?' })
        .collect();
    if text.chars().count() > SHOWN_CHARS {
        out.push_str("...");
    }
    out
}

/// The entries of `value` (`source.toml`'s `mirrors`) this build keeps for the vault `name` at
/// `source`, and each it leaves out with why. Nothing here fails: a mirror carries no authority,
/// so an entry this build cannot read (another vault's, a `#` pin, a later build's form) costs
/// that address, never the vault.
pub(super) fn sorted(source: &Source, name: &str, value: Option<&aterm_toml::Value>) -> Sorted {
    let mut out = Sorted::default();
    let Some(value) = value else {
        return out;
    };
    let Source::Url(url) = source else {
        out.dropped.push((
            "mirrors".into(),
            "a vault at a path has no other addresses".into(),
        ));
        return out;
    };
    let aterm_toml::Value::Array(entries) = value else {
        out.dropped
            .push(("mirrors".into(), "not a list of URLs".into()));
        return out;
    };
    for (n, entry) in entries.iter().enumerate() {
        let Some(text) = entry.as_str() else {
            out.dropped
                .push((format!("entry {}", n + 1), "not a URL string".into()));
            continue;
        };
        match kept_entry(url, name, &out.kept, text) {
            Ok(clean) => out.kept.push(clean),
            Err(why) => out.dropped.push((shown(text), why)),
        }
    }
    out
}

/// `text` as a mirror of the vault `name` whose `url` it is, beside the mirrors `kept` before it:
/// an `http(s)://` URL of that vault's name with no `#` pin, not `url`'s endpoint or one kept
/// already, within [`MAX_MIRRORS`]. Else why not.
fn kept_entry(url: &str, name: &str, kept: &[String], text: &str) -> Result<String, String> {
    if text.contains('#') {
        return Err("a mirror carries no #pin: the vault's own pin reads every address".into());
    }
    if text.len() > MAX_URL {
        return Err(format!("longer than {MAX_URL} bytes"));
    }
    let clean = match transport::parse_source(text, Path::new("/")) {
        Ok((Source::Url(clean), None)) => clean,
        Ok(_) => return Err("not an http(s):// URL".into()),
        Err(error) => return Err(error.to_string()),
    };
    match transport::vault_name(&Source::Url(clean.clone())) {
        Ok(named) if named == name => {}
        Ok(named) => return Err(format!("it names the vault {named}, not {name}")),
        Err(error) => return Err(error.to_string()),
    }
    if same_endpoint(&clean, url) {
        return Err("it is the vault's url".into());
    }
    if kept.iter().any(|kept| same_endpoint(kept, &clean)) {
        return Err("it is listed already".into());
    }
    if kept.len() >= MAX_MIRRORS {
        return Err(format!("only the first {MAX_MIRRORS} mirrors are read"));
    }
    Ok(clean)
}

/// Each entry of `source.toml`'s `mirrors` this build leaves out, and why (`aterm secrets
/// status`, `aterm secrets mirrors`), read from the file again: [`Config::mirrors`] holds only
/// the entries kept. None when nothing is configured or the file cannot be read.
#[must_use]
pub fn mirror_notes(paths: &Paths) -> Vec<String> {
    let Ok(Some(config)) = config(paths) else {
        return Vec::new();
    };
    let Ok(Some(table)) = read_toml(&paths.source_file(), "source.toml", 4 << 20) else {
        return Vec::new();
    };
    sorted(&config.source, &config.name, table.get("mirrors"))
        .dropped
        .into_iter()
        .map(|(entry, why)| format!("{entry}: not read ({why})"))
        .collect()
}

/// A mirror a person typed for `config`'s vault (`aterm secrets mirrors add`): its URL without a
/// `#<key>`, which must be the pin when given, so no address brings a key of its own.
///
/// # Errors
/// A path source, or `typed` is not a URL [`Config::mirrors`] would keep, or names another key.
pub fn mirror_typed(config: &Config, typed: &str) -> crate::Result<String> {
    let Source::Url(url) = &config.source else {
        return Err(Error::new("a vault at a path has no other addresses"));
    };
    let (base, pin) = match transport::parse_source(typed, Path::new("/"))? {
        (Source::Url(base), pin) => (base, pin),
        (Source::Path(_), _) => {
            return Err(Error::new(format!(
                "{}: a mirror is an http(s):// URL",
                shown(typed)
            )));
        }
    };
    if let Some(pin) = pin
        && config.signer != Some(pin)
    {
        return Err(Error::new(format!(
            "{}: the #key is not this vault's pin ({}); a mirror serves the vault this Mac pins",
            shown(typed),
            config.signer.map_or_else(
                || "none".to_owned(),
                |signer| crate::envelope::fingerprint(&signer)
            )
        )));
    }
    kept_entry(url, &config.name, &[], &base)
        .map_err(|why| Error::new(format!("{}: {why}", shown(typed))))
}

/// How long `aterm secrets mirrors add` waits for a mirror to answer ([`probe_mirror`]).
const PROBE: std::time::Duration = std::time::Duration::from_secs(5);

/// What a mirror a person adds serves now ([`probe_mirror`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirrorProbe {
    /// A version of the vault that verifies under this Mac's pin, at this seq.
    Serves(u64),
    /// No version of the vault yet (404).
    Empty,
    /// No answer, and why.
    Unreached(String),
}

/// Read the mirror `url` of `config`'s vault once, within [`PROBE`], verifying what it serves
/// under the pin and the keys `config` follows (no floor: this reads, it applies nothing).
///
/// # Errors
/// `config` pins no key, or `url` serves bytes that do not verify as this vault (another vault
/// of its name, or no vault at all), so listing it would only cost reads.
pub fn probe_mirror(config: &Config, url: &str) -> crate::Result<MirrorProbe> {
    let Some(pin) = config.signer.as_ref() else {
        return Err(Error::new("a vault with no pinned key has no mirrors"));
    };
    match transport::fetch_within(url, None, PROBE) {
        Ok(super::Fetched::Body(bytes)) => {
            match crate::envelope::verify_for_reader(
                &bytes,
                &config.name,
                pin,
                &config.followed,
                None,
            ) {
                Ok((envelope, _, _)) => Ok(MirrorProbe::Serves(envelope.seq)),
                Err(error) => Err(Error::new(format!(
                    "{}: it serves no version of {} this Mac's pin verifies ({error}); nothing saved",
                    transport::describe(url),
                    config.name
                ))),
            }
        }
        Ok(super::Fetched::Missing) => Ok(MirrorProbe::Empty),
        Ok(super::Fetched::NoNewer(_)) => Ok(MirrorProbe::Unreached("it sent no version".into())),
        Err(error) => Ok(MirrorProbe::Unreached(error.to_string())),
    }
}

/// Save `mirrors` as the other addresses of the vault `expected` names, read after its `url` in
/// this order, under the state lock and only over the configuration the caller read. Nothing
/// else of it changes.
///
/// # Errors
/// A path source, an entry [`config`] would not keep, the configuration changed since it was
/// read, or `source.toml` cannot be written.
pub fn set_mirrors(paths: &Paths, expected: &Config, mirrors: &[String]) -> crate::Result<Config> {
    let Source::Url(url) = &expected.source else {
        return Err(Error::new("a vault at a path has no other addresses"));
    };
    let mut kept: Vec<String> = Vec::new();
    for mirror in mirrors {
        let clean = kept_entry(url, &expected.name, &kept, mirror)
            .map_err(|why| Error::new(format!("{}: {why}", shown(mirror))))?;
        kept.push(clean);
    }
    let _lock = StateLock::take(paths)?;
    if config(paths)?.as_ref() != Some(expected) {
        return Err(Error::new(
            "the configured vault changed after it was read; try again",
        ));
    }
    let mut config = expected.clone();
    config.mirrors = kept;
    save_config(paths, &config)?;
    Ok(config)
}

/// The mirrors of a pass's vault that it reads and never writes to: each of [`Config::mirrors`]
/// that nothing else names a server of the vault (its policy's servers, the caller's own peers,
/// the compiled-in ALab addresses). A records post, an admin's heal and a put-back go to
/// [`Self::servers`]; joins, re-asks and queue reads never reach a mirror at all
/// ([`super::PeerUse::vault_read`]).
#[derive(Debug, Default)]
pub(super) struct ReadOnly(Vec<String>);

impl ReadOnly {
    /// `config`'s read-only mirrors beside the peers `explicit` names. A vault with no mirror
    /// has none, and reads nothing to say so.
    pub(super) fn of(paths: &Paths, config: &Config, explicit: &[String]) -> Self {
        if config.mirrors.is_empty() {
            return Self::default();
        }
        let policy = policy_peer_urls(paths, config);
        let defaults: &[&str] = if alab_vault(config) {
            crate::ALAB_DEFAULT_URLS
        } else {
            &[]
        };
        let servers: Vec<&str> = policy
            .iter()
            .chain(explicit)
            .map(String::as_str)
            .chain(defaults.iter().copied())
            .collect();
        Self(
            config
                .mirrors
                .iter()
                .filter(|mirror| !servers.iter().any(|server| same_endpoint(server, mirror)))
                .cloned()
                .collect(),
        )
    }

    /// Whether `url` is one of these mirrors.
    pub(super) fn holds(&self, url: &str) -> bool {
        self.0.iter().any(|mirror| same_endpoint(mirror, url))
    }

    /// The subscriber a read of `url` names (`X-Aterm-Subscriber`): `subscriber` for a server
    /// of the vault, none for one of these mirrors. A mirror can be a copy a third party keeps,
    /// and the header would tell it this Mac's recipient on every read from every network.
    pub(super) fn subscriber_for<'a>(
        &self,
        url: &str,
        subscriber: Option<&'a str>,
    ) -> Option<&'a str> {
        subscriber.filter(|_| !self.holds(url))
    }

    /// `primary` and each of `peers` that is a server of the vault, not one of these mirrors:
    /// where a pass writes.
    pub(super) fn servers(&self, primary: &str, peers: &[String]) -> Vec<String> {
        std::iter::once(primary)
            .chain(
                peers
                    .iter()
                    .map(String::as_str)
                    .filter(|peer| !self.holds(peer)),
            )
            .map(str::to_owned)
            .collect()
    }
}
