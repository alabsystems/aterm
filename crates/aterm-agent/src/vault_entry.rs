// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE VAULT'S ENTRIES, READ IN PLACE FROM THIS MAC'S STORE (`docs/DESIGN-secrets-2026-10-20.md`
//! §7): the one way this crate reads a value the vault holds (the account pool, the fleet roster,
//! the accounts sheet, a CLI's roster, the mailbox configurations and passwords, the sign-in
//! seal). No file under `~/.secrets` is opened, listed or stat'ed: the files older builds wrote
//! there are the owner's backup, and nothing reads them.
//!
//! A decision between several entries (the pool before the fleet before the sheet) reads them
//! with [`read_set`], under one verified version of the vault, so it never mixes two versions. An
//! entry that is there but cannot be used ([`Read::Unavailable`]: its anchor set does not resolve,
//! the vault cannot be verified where this Mac is in it, its bytes are not bounded UTF-8 text)
//! keeps its place: it never lets a lower source stand in for it.

use std::path::Path;

/// How an explicit source names a vault entry where a path or URL would stand
/// (`aterm harness accounts sync vault:ai-pool.toml`), and how a selected entry prints.
pub const SOURCE_PREFIX: &str = "vault:";

/// One entry as this Mac's store answers for it now.
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum Read {
    /// The verified value, as bounded UTF-8 text.
    #[cfg_attr(
        all(not(unix), not(test)),
        expect(
            dead_code,
            reason = "the vault client builds on unix only: every other host's reader answers absent (tests build one)"
        )
    )]
    Present(String),
    /// No such entry for this Mac: no vault is configured here, this Mac is not (or no longer)
    /// in it, or its verified view holds no entry of that name.
    Absent,
    /// The entry holds its place but cannot be read now; the words say why, without its value.
    Unavailable(String),
}

impl std::fmt::Debug for Read {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Present(_) => f.write_str("Present(..)"),
            Self::Absent => f.write_str("Absent"),
            Self::Unavailable(why) => f.debug_tuple("Unavailable").field(why).finish(),
        }
    }
}

impl Read {
    /// Whether the entry holds its place in a precedence: present, usable or not.
    pub(crate) const fn occupies(&self) -> bool {
        !matches!(self, Self::Absent)
    }

    /// The text, where the entry is present.
    pub(crate) fn text(&self) -> Option<&str> {
        match self {
            Self::Present(text) => Some(text),
            Self::Absent | Self::Unavailable(_) => None,
        }
    }

    /// The text, `None` where the entry is absent.
    /// # Errors
    /// The entry is unavailable.
    pub(crate) fn into_text(self) -> Result<Option<String>, String> {
        match self {
            Self::Present(text) => Ok(Some(text)),
            Self::Absent => Ok(None),
            Self::Unavailable(why) => Err(why),
        }
    }
}

/// The source spelling of the entry `name` ([`SOURCE_PREFIX`]).
#[must_use]
pub fn source_name(name: &str) -> String {
    format!("{SOURCE_PREFIX}{name}")
}

/// The entry an explicit source spelled with [`SOURCE_PREFIX`] names.
#[must_use]
pub fn source_entry(source: &str) -> Option<&str> {
    source
        .strip_prefix(SOURCE_PREFIX)
        .filter(|name| !name.is_empty())
}

/// The entry `name` under `home`, bounded at `max` bytes ([`read_set`]).
pub(crate) fn read_text(home: &Path, name: &str, max: usize) -> Read {
    read_set(home, &[name], max)
        .pop()
        .unwrap_or_else(|| Read::Unavailable(format!("the vault entry {name} was not read")))
}

/// Each of `names` under `home`, in order, every one from the same verified version of the
/// vault, each bounded at `max` bytes.
pub(crate) fn read_set(home: &Path, names: &[&str], max: usize) -> Vec<Read> {
    #[cfg(unix)]
    {
        store::read_set(home, names, max)
    }
    #[cfg(not(unix))]
    {
        let _ = (home, max);
        names.iter().map(|_| Read::Absent).collect()
    }
}

/// Whether a vault is configured under `home` (whatever this Mac's standing in it).
pub(crate) fn configured(home: &Path) -> bool {
    #[cfg(unix)]
    {
        use aterm_secrets::client;
        client::config(&client::Paths::locate_for_home(home))
            .map_or(true, |config| config.is_some())
    }
    #[cfg(not(unix))]
    {
        let _ = home;
        false
    }
}

/// The names this Mac's store holds, without any value; empty where it holds none or its record
/// cannot be read.
pub(crate) fn names(home: &Path) -> Vec<String> {
    #[cfg(unix)]
    {
        use aterm_secrets::client;
        client::registered_names(&client::Paths::locate_for_home(home)).unwrap_or_default()
    }
    #[cfg(not(unix))]
    {
        let _ = home;
        Vec::new()
    }
}

/// A digest of what `names` hold now, for a scheduler deciding whether its inputs changed. It is
/// keyed by a secret this process drew at random, so it says nothing of any value outside the
/// process, and it is kept in memory only: two processes' digests never compare.
pub(crate) fn digest(home: &Path, names: &[&str], max: usize) -> [u8; 32] {
    let mut hash = aterm_digest::Sha256::new();
    hash.update(b"aterm-agent/vault-entry-digest/1\0");
    hash.update(process_key());
    for (name, read) in names.iter().zip(read_set(home, names, max)) {
        hash.update((name.len() as u64).to_be_bytes());
        hash.update(name.as_bytes());
        match read {
            Read::Present(text) => {
                hash.update([1]);
                hash.update((text.len() as u64).to_be_bytes());
                hash.update(text.as_bytes());
            }
            Read::Absent => hash.update([0]),
            Read::Unavailable(why) => {
                hash.update([2]);
                hash.update((why.len() as u64).to_be_bytes());
                hash.update(why.as_bytes());
            }
        }
    }
    hash.finalize()
}

/// When this process last saw what `names` hold under `home` change ([`digest`]), for the reader
/// `what` names: `None` while it has seen them only as they first were, since a first read is no
/// evidence of a change, and the time of the read that first saw them differ after that. The
/// memory is keyed by `home` and `what`, never by `names`, so an entry that joins `names` (a
/// per-account password the vault delivers) is a change, not a first read. A cooldown that ends
/// on a newer input ends on a vault change made while this process runs, and never on a restart.
pub(crate) fn changed_at(
    what: &'static str,
    home: &Path,
    names: &[&str],
    max: usize,
) -> Option<std::time::SystemTime> {
    type Seen = std::collections::BTreeMap<
        (std::path::PathBuf, &'static str),
        ([u8; 32], Option<std::time::SystemTime>),
    >;
    static SEEN: std::sync::Mutex<Seen> = std::sync::Mutex::new(std::collections::BTreeMap::new());
    let now = digest(home, names, max);
    let mut seen = SEEN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let held = seen
        .entry((home.to_path_buf(), what))
        .or_insert((now, None));
    if held.0 != now {
        *held = (now, Some(std::time::SystemTime::now()));
    }
    held.1
}

fn process_key() -> [u8; 32] {
    static KEY: std::sync::OnceLock<[u8; 32]> = std::sync::OnceLock::new();
    *KEY.get_or_init(|| {
        let mut key = [0; 32];
        if aterm_uds::rand::fill(&mut key).is_err() {
            // No randomness: a digest keyed by the process's own address and clock still never
            // leaves the process, which is all a scheduler's comparison needs.
            let mut hash = aterm_digest::Sha256::new();
            hash.update(std::process::id().to_be_bytes());
            hash.update(format!("{:?}", std::time::Instant::now()).as_bytes());
            key = hash.finalize();
        }
        key
    })
}

#[cfg(unix)]
mod store {
    use super::Read;
    use aterm_secrets::client::{self, EntryAdmission, EntryRead, Membership, Paths};
    use std::path::Path;

    pub(super) fn read_set(home: &Path, names: &[&str], max: usize) -> Vec<Read> {
        let paths = Paths::locate_for_home(home);
        let all = |read: Read| names.iter().map(|_| read.clone()).collect::<Vec<_>>();
        let config = match client::config(&paths) {
            Ok(Some(config)) => config,
            Ok(None) => return all(Read::Absent),
            Err(why) => {
                return all(Read::Unavailable(format!(
                    "this Mac's vault configuration cannot be read: {why}"
                )));
            }
        };
        if config.signer.is_none() {
            return names
                .iter()
                .map(|name| match client::read_in_place(&paths, name) {
                    Ok(Some(value)) => decode(name, value.as_slice(), max),
                    Ok(None) => Read::Absent,
                    Err(why) => Read::Unavailable(format!("the vault entry {name}: {why}")),
                })
                .collect();
        }
        let read = match client::try_commons_source_read(&paths) {
            Ok(Some(read)) => read,
            Ok(None) => return all(unverified(&paths)),
            Err(why) => {
                return all(Read::Unavailable(format!(
                    "this Mac's vault cannot be read: {why}"
                )));
            }
        };
        let look = read.into_look(&paths);
        let admitted = look.with_entry_reader(&paths, &config, |reader| {
            names
                .iter()
                .map(|name| match reader.observe(name) {
                    Ok(EntryRead::Present(entry)) => decode(name, entry.value().as_slice(), max),
                    Ok(EntryRead::Absent(_)) => Read::Absent,
                    Ok(EntryRead::Unavailable) => Read::Unavailable(format!(
                        "the vault entry {name} cannot be verified on this Mac"
                    )),
                    Err(why) => Read::Unavailable(format!("the vault entry {name}: {why}")),
                })
                .collect::<Vec<_>>()
        });
        match admitted {
            Ok(EntryAdmission::Accepted(reads)) => reads,
            Ok(EntryAdmission::Unavailable) => all(unverified(&paths)),
            Ok(EntryAdmission::Busy | EntryAdmission::Changed) => all(Read::Unavailable(
                "this Mac's vault is being updated; it is read again on the next pass".to_owned(),
            )),
            Err(why) => all(Read::Unavailable(format!(
                "this Mac's vault cannot be read: {why}"
            ))),
        }
    }

    /// What a vault with no verified view here stands for: nothing, where this Mac is not in it
    /// (it asks, was let go, or reads a plain file that is not there), else an entry that holds
    /// its place.
    fn unverified(paths: &Paths) -> Read {
        match client::status(paths).map(|status| status.membership) {
            Ok(Membership::In { .. }) => {
                Read::Unavailable("this Mac's vault cannot be verified now".to_owned())
            }
            Ok(
                Membership::Unconfigured
                | Membership::Pending
                | Membership::Revoked { .. }
                | Membership::Plain,
            ) => Read::Absent,
            Err(why) => Read::Unavailable(format!("this Mac's vault cannot be read: {why}")),
        }
    }

    fn decode(name: &str, bytes: &[u8], max: usize) -> Read {
        if bytes.len() > max {
            return Read::Unavailable(format!(
                "the vault entry {name} exceeds the {max}-byte limit"
            ));
        }
        match std::str::from_utf8(bytes) {
            Ok(text) => Read::Present(text.to_owned()),
            Err(_) => Read::Unavailable(format!("the vault entry {name} is not UTF-8 text")),
        }
    }
}

/// Seed `entries` into a file vault this test's `home` administers, as a member's sync would have
/// it: the vault is made at `home/fixture.vault` on the first call, and a later call edits and
/// publishes it again. An empty value removes the entry.
#[cfg(all(unix, any(test, debug_assertions)))]
pub fn seed(home: &Path, entries: &[(&str, &[u8])]) {
    seed_with(home, entries, |_| (false, None));
}

/// [`seed`] on a host where the vault client does not build (every one but unix): no vault can
/// be made there and every reader above answers absent, so a test that seeds one cannot run on
/// such a host. It compiles there, as the rest of its file does, and says so if it is run.
///
/// # Panics
/// Always.
#[cfg(all(not(unix), any(test, debug_assertions)))]
pub fn seed(home: &Path, entries: &[(&str, &[u8])]) {
    let _ = (home, entries);
    panic!("a test vault is seeded on unix only: the vault client does not build on this host");
}

/// [`seed`] of one entry `name` holding `text`.
#[cfg(any(test, debug_assertions))]
pub fn put(home: &Path, name: &str, text: impl AsRef<[u8]>) {
    seed(home, &[(name, text.as_ref())]);
}

/// The file vault at `home/fixture.vault` this test's `home` administers, made with this Mac
/// named `host` where the home has no vault yet ([`seed`] names it `seed`).
#[cfg(all(unix, any(test, debug_assertions)))]
pub fn ensure_vault(home: &Path, host: &str) -> aterm_secrets::client::Paths {
    use aterm_secrets::client;
    let paths = client::Paths::locate_for_home(home);
    if client::config(&paths).unwrap().is_none() {
        std::fs::create_dir_all(home).unwrap();
        client::create(
            &paths,
            home.join("fixture.vault").to_str().unwrap(),
            None,
            Some(host),
        )
        .unwrap();
    }
    paths
}

/// [`seed`], each entry's put-if-absent flag and anchor chosen by `policy`.
#[cfg(all(unix, any(test, debug_assertions)))]
pub fn seed_with(
    home: &Path,
    entries: &[(&str, &[u8])],
    policy: impl Fn(&str) -> (bool, Option<&'static str>),
) {
    use aterm_secrets::{client, text::Audience};
    let paths = ensure_vault(home, "seed");
    let admin = client::admin_open(&paths).unwrap();
    let mut doc = admin.doc.clone();
    for (name, value) in entries {
        if value.is_empty() {
            doc.unset_entry(&Audience::Everyone, name).unwrap();
        } else {
            let (if_absent, anchor) = policy(name);
            doc.set_entry_with(&Audience::Everyone, name, value, if_absent, anchor)
                .unwrap();
        }
    }
    client::admin_publish(&paths, &admin, &doc).unwrap();
}

#[cfg(test)]
#[path = "vault_entry_tests.rs"]
mod tests;
