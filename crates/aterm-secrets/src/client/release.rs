// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Local release-credential materialization after an authenticated vault apply
//! (docs/DESIGN-alab-swarm-2026-10-27.md §5.2, C1). The signed `release-notary` entry can
//! populate the `notary` keychain profile `ship release` notarizes with; an existing profile
//! that authenticates is preserved. Notarytool runs only where all of these hold, with no
//! child process spawned by their preflight reads:
//!
//! - this member holds a private release-ring profile and key that prove one signer under
//!   the current master-signed roster. The shared signer is independent of the physical
//!   member's hostname; an expired, revoked, mismatched or untrusted ring runs nothing;
//! - xcrun has a developer directory that holds `notarytool` (the Command Line Tools', or the
//!   selected Xcode's): without one, `/usr/bin/xcrun` puts Apple's install dialog on screen;
//! - the login keychain is unlocked ([`aterm_uds::keychain::default_unlocked`]): notarytool
//!   reads and stores its profile there, and macOS asks for a locked keychain's password in a
//!   dialog;
//! - a successful check is reused for at most thirty minutes in this subscriber, and a new
//!   subscriber checks again;
//! - a failed attempt is kept on disk ([`Failed`]) and backs off for one minute, doubling with
//!   each further failure of the same entry up to a day, across restarts. A run that did not
//!   finish (the mark of a dialog nobody answered) waits the whole day at once. A changed
//!   entry is eligible immediately, and the explicit sync a release makes always runs.
//!
//! The name `notary`, which 0.145 reads after every applied version, is never read here.

use super::*;
use std::ffi::OsString;
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

pub mod probe;
mod signing;
#[cfg(any(test, debug_assertions))]
pub use signing::test_signing_observation;
pub use signing::{SigningStatus, effective_signing_identity, signing_status, sync_signing};

/// The retained release-roster generation, read without changing vault or ring state.
/// # Errors
/// The retained floor is unreadable or is not a valid unsigned generation.
pub fn roster_floor(paths: &Paths) -> crate::Result<u64> {
    super::release_migration::roster_floor(paths)
}

/// The vault entry this reads.
pub(crate) const ENTRY: &str = crate::need::catalog::NOTARY_ENTRY;

const NOTARY_RECHECK: Duration = Duration::from_secs(30 * 60);

/// The wait after an entry's first failed attempt; each further failure doubles it.
const NOTARY_RETRY: Duration = Duration::from_secs(60);

/// The longest wait between attempts at an entry that keeps failing, and the wait after a run
/// that did not finish.
const NOTARY_RETRY_MAX: Duration = Duration::from_secs(24 * 60 * 60);

/// The failed attempt's record, in the vault's state directory ([`Failed`]).
const NOTARY_FAILED_FILE: &str = "release-notary-failed";

/// What an error says when a notarytool run was cut off at its deadline: nothing it waits a
/// minute for is the network's; a dialog nobody answered is.
const UNANSWERED: &str = "did not complete";

/// The release-credentials profile, under the home. The 0.145 copies under `~/.aterm/` are
/// duplicates recorded for retirement (ATERM-KI-1112-980): no code reads them.
const RELEASE_PROFILE: &str = ".aterm/release-ring/release-credentials.toml";

/// The canonical roster, considered alongside the profile's named roster.
const VAULT_ROSTER: &str = ".aterm/release-ring/roster/aterm-machines.toml";

/// The largest profile or roster read.
const MAX_READ: usize = 64 * 1024;

/// Where `xcode-select -s` keeps the developer directory a person chose.
const XCODE_SELECT_LINK: &str = "/var/db/xcode_select_link";

/// The developer directory xcrun takes with no choice made and Xcode installed.
const XCODE_DEVELOPER: &str = "/Applications/Xcode.app/Contents/Developer";

/// The Command Line Tools' developer directory.
const CLT_DEVELOPER: &str = "/Library/Developer/CommandLineTools";

/// What a round says while this Mac is no release host.
const NOT_RELEASE_HOST: &str = "release-notary waits for an authorized release ring \
     (a private matching profile/key and current master-signed roster)";

/// What a round says on a Mac while xcrun has nothing to run notarytool with.
const WAITS_FOR_CLT: &str = "release-notary waits for Command Line Tools: xcrun has no \
     developer directory with notarytool, so nothing runs it";

/// What a round says while the login keychain would ask for its password.
const WAITS_FOR_KEYCHAIN: &str = "release-notary waits for an unlocked login keychain: \
     notarytool reads and stores its profile there, and macOS would ask for the keychain's \
     password in a dialog";

/// What a round says off a Mac, where no developer directory ever comes.
const MAC_ONLY: &str = "release-notary runs on a Mac: notarytool comes with Apple's developer \
     tools, which this machine has none of";

/// What a round says while it finds no developer directory, on this host.
fn no_developer_dir() -> &'static str {
    if cfg!(target_os = "macos") {
        WAITS_FOR_CLT
    } else {
        MAC_ONLY
    }
}

pub(super) struct Notary {
    apple_id: Zeroizing<String>,
    team_id: Zeroizing<String>,
    password: Zeroizing<String>,
}

pub(super) fn parse_notary(bytes: &[u8]) -> crate::Result<Notary> {
    let text =
        std::str::from_utf8(bytes).map_err(|_| Error::new("release-notary entry is not UTF-8"))?;
    let mut apple_id = None;
    let mut team_id = None;
    let mut password = None;
    let mut use_aterm = false;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| Error::new("release-notary entry needs name=value lines"))?;
        let key = key.trim();
        let value = value.trim();
        if value.is_empty() || value.contains(['\0', '"', '\'', '\\']) {
            return Err(Error::new("release-notary entry has an invalid value"));
        }
        let slot = match key {
            "apple_id" => &mut apple_id,
            "team_id" => &mut team_id,
            "password" => &mut password,
            "use" => {
                if value != "aterm" {
                    return Err(Error::new("release-notary entry's use must be aterm"));
                }
                use_aterm = true;
                continue;
            }
            _ => return Err(Error::new("release-notary entry has an unknown field")),
        };
        if slot.replace(value.to_owned()).is_some() {
            return Err(Error::new("release-notary entry repeats a field"));
        }
    }
    if !use_aterm {
        return Err(Error::new("release-notary entry needs use=aterm"));
    }
    Ok(Notary {
        apple_id: Zeroizing::new(
            apple_id.ok_or_else(|| Error::new("release-notary entry needs apple_id"))?,
        ),
        team_id: Zeroizing::new(
            team_id.ok_or_else(|| Error::new("release-notary entry needs team_id"))?,
        ),
        password: Zeroizing::new(
            password.ok_or_else(|| Error::new("release-notary entry needs password"))?,
        ),
    })
}

/// What a round reaches past its own reads: the developer directory xcrun would run
/// notarytool from, and the two notarytool runs. A test's never run notarytool.
pub(super) struct Tools<'a> {
    /// A private profile/key pair authorized by the pinned release-master roster.
    pub(super) authorized_ring: &'a dyn Fn(&Paths) -> bool,
    /// The developer directory xcrun selects, where it holds notarytool ([`developer_dir_with`]).
    pub(super) developer_dir: &'a dyn Fn() -> Option<PathBuf>,
    /// Whether the login keychain answers a read without asking for its password.
    pub(super) keychain_answers: &'a dyn Fn() -> bool,
    /// Whether the `notary` profile authenticates now (`Ok(false)`: there is none).
    pub(super) history: &'a mut dyn FnMut() -> crate::Result<bool>,
    /// Store the profile from the entry.
    pub(super) store: &'a mut dyn FnMut(&Notary) -> crate::Result<()>,
}

/// The developer directory `/usr/bin/xcrun` would run notarytool from, read with no child
/// process, where that directory holds notarytool: `DEVELOPER_DIR` when set, else the one
/// `xcode-select -s` chose, else Xcode's where it is installed, else the Command Line Tools'.
/// A selected Xcode application bundle names its `Contents/Developer`, as the shims infer.
/// `None` where xcrun would find no notarytool, which is where it asks a person to install the
/// tools.
fn developer_dir_with(
    env: Option<OsString>,
    read_link: &dyn Fn(&Path) -> Option<PathBuf>,
    is_dir: &dyn Fn(&Path) -> bool,
    is_file: &dyn Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let chosen = env
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| read_link(Path::new(XCODE_SELECT_LINK)));
    let dir = match chosen {
        Some(dir) if dir.is_absolute() => dir,
        Some(_) => return None,
        None if is_dir(Path::new(XCODE_DEVELOPER)) => PathBuf::from(XCODE_DEVELOPER),
        None => PathBuf::from(CLT_DEVELOPER),
    };
    let dir = if dir.extension().is_some_and(|extension| extension == "app") {
        dir.join("Contents/Developer")
    } else {
        dir
    };
    is_file(&dir.join("usr/bin/notarytool")).then_some(dir)
}

/// The notarytool runs of a live round, and its read of xcrun's developer directory
/// ([`developer_dir_with`]). A test build has none of them ([`LIVE_HISTORY`], [`LIVE_STORE`],
/// [`LIVE_DEVELOPER_DIR`]), so no test, nor a mutant of one, ever reaches xcrun.
#[cfg(not(test))]
mod live {
    use super::*;
    use aterm_uds::spawnfd::Stdio;
    use std::process::Output;
    use std::time::Instant;

    /// The keychain profile `notarytool store-credentials` keeps, which `ship release` names.
    const PROFILE: &str = "notary";

    fn history() -> crate::Result<Output> {
        let mut command = aterm_uds::spawnfd::command("/usr/bin/xcrun");
        command
            .args(["notarytool", "history", "--keychain-profile", PROFILE])
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        aterm_uds::deadline::output_until(
            &mut command,
            Instant::now() + crate::keychain_tools::WAIT,
        )
        .map_err(|_| {
            Error::new(
                "notarytool authentication did not complete within its minute; automatic setup \
                 waits a day before it tries again",
            )
        })
    }

    pub(super) fn history_is_current() -> crate::Result<bool> {
        let output = history()?;
        if output.status.success() {
            return Ok(true);
        }
        let detail = String::from_utf8_lossy(&output.stderr);
        if detail.contains("No Keychain password item found") {
            return Ok(false);
        }
        Err(Error::new(
            "notarytool history did not authenticate; the existing profile was left unchanged",
        ))
    }

    pub(super) fn store(notary: &Notary) -> crate::Result<()> {
        let mut command = aterm_uds::spawnfd::command("/usr/bin/xcrun");
        command
            .args([
                "notarytool",
                "store-credentials",
                PROFILE,
                "--apple-id",
                notary.apple_id.as_str(),
                "--team-id",
                notary.team_id.as_str(),
                "--password",
                notary.password.as_str(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // Supplying every field and closed stdin gives notarytool no interactive prompt.
        // The password is never formatted into an error or logged; as for security(1),
        // argv is visible only to this user, who already owns the private credential file.
        let output = aterm_uds::deadline::output_until(
            &mut command,
            Instant::now() + crate::keychain_tools::WAIT,
        )
        .map_err(|_| {
            Error::new(
                "notarytool credential storage did not complete within its minute; automatic \
                 setup waits a day before it tries again",
            )
        })?;
        if output.status.success() {
            Ok(())
        } else {
            Err(Error::new("notarytool did not store the credential"))
        }
    }

    /// [`developer_dir_with`] of this Mac.
    pub(super) fn developer_dir() -> Option<PathBuf> {
        developer_dir_with(
            std::env::var_os("DEVELOPER_DIR"),
            &|path| std::fs::read_link(path).ok(),
            &Path::is_dir,
            &Path::is_file,
        )
    }
}

/// The developer directory a live round reads.
#[cfg(not(test))]
const LIVE_DEVELOPER_DIR: fn() -> Option<PathBuf> = live::developer_dir;
/// A test build's: none.
#[cfg(test)]
const LIVE_DEVELOPER_DIR: fn() -> Option<PathBuf> = || None;

/// A live round's read of the login keychain's lock: only a keychain known to be unlocked
/// answers.
#[cfg(not(test))]
const LIVE_KEYCHAIN: fn() -> bool = || aterm_uds::keychain::default_unlocked() == Some(true);
/// A test build's: locked, so nothing reads the person's keychain.
#[cfg(test)]
const LIVE_KEYCHAIN: fn() -> bool = || false;

/// A live round's check of the profile.
#[cfg(not(test))]
const LIVE_HISTORY: fn() -> crate::Result<bool> = live::history_is_current;
/// A test build's: it refuses, running nothing.
#[cfg(test)]
const LIVE_HISTORY: fn() -> crate::Result<bool> =
    || Err(Error::new("a test build runs no notarytool"));

/// A live round's store of the profile.
#[cfg(not(test))]
const LIVE_STORE: fn(&Notary) -> crate::Result<()> = live::store;
/// A test build's: it refuses, running nothing.
#[cfg(test)]
const LIVE_STORE: fn(&Notary) -> crate::Result<()> =
    |_| Err(Error::new("a test build runs no notarytool"));

/// The `machine_roster` the release-credentials profile `text` names, a leading `~/` read under
/// `home`, as `ship release` reads it; only that assignment is parsed into a TOML value.
/// Returns `Err(())` when the profile is malformed, duplicates `machine_roster`, or names an
/// empty or relative roster path so callers fail closed rather than falling back to another path.
fn parse_named_roster(text: &str, home: &Path) -> std::result::Result<Option<PathBuf>, ()> {
    let mut found = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            break;
        }
        let (name, rhs) = line.split_once('=').ok_or(())?;
        if rhs.contains("\"\"\"") || rhs.contains("'''") {
            return Err(());
        }
        if name.trim().trim_matches(['"', '\'']) != "machine_roster" {
            continue;
        }
        if found.is_some() {
            return Err(());
        }
        let doc = aterm_toml::from_str::<aterm_toml::Value>(line).map_err(|_| ())?;
        let value = doc
            .get("machine_roster")
            .and_then(aterm_toml::Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or(())?;
        let path = match value.strip_prefix("~/") {
            Some(rest) => {
                let rest = rest.trim_start_matches('/');
                if rest.is_empty() || !home.is_absolute() {
                    return Err(());
                }
                home.join(rest)
            }
            None => {
                let path = PathBuf::from(value);
                if !path.is_absolute() {
                    return Err(());
                }
                path
            }
        };
        found = Some(path);
    }
    Ok(found)
}

#[cfg(test)]
fn named_roster(text: &str, home: &Path) -> Option<PathBuf> {
    parse_named_roster(text, home).ok().flatten()
}

/// Whether this member holds a private ring key/profile that proves its declared signer
/// under the pinned release master. The physical member's hostname is not the ring identity.
fn authorized_ring(paths: &Paths) -> bool {
    let Some(now) = aterm_time::unix_secs().and_then(|now| i64::try_from(now).ok()) else {
        return false;
    };
    authorized_ring_with(paths, aterm_update_core::pins::PAPER_MASTER_PUBKEYS, now)
}

fn authorized_ring_with(paths: &Paths, anchors: &[&str], now: i64) -> bool {
    authorized_ring_check(paths, anchors, now, true)
}

fn authorized_ring_readonly(paths: &Paths) -> bool {
    let Some(now) = aterm_time::unix_secs().and_then(|now| i64::try_from(now).ok()) else {
        return false;
    };
    authorized_ring_check(
        paths,
        aterm_update_core::pins::PAPER_MASTER_PUBKEYS,
        now,
        false,
    )
}

fn authorized_ring_check(paths: &Paths, anchors: &[&str], now: i64, advance: bool) -> bool {
    let _guard = if advance {
        let Ok(guard) = StateLock::take_at(paths, &paths.dir.join("release-ring.lock")) else {
            return false;
        };
        Some(guard)
    } else {
        None
    };
    let home = &paths.home;
    let Ok(Some(profile)) = fsx::read_private(&home.join(RELEASE_PROFILE), MAX_READ) else {
        return false;
    };
    let key_path = home.join(".aterm/release-ring/machine.key");
    let Ok(Some(key)) = fsx::read_private(&key_path, MAX_READ) else {
        return false;
    };
    let Some(text) = profile.as_str() else {
        return false;
    };
    let Ok(named) = parse_named_roster(text, home) else {
        return false;
    };
    let read_pair = |path: PathBuf| -> crate::Result<Option<(Vec<u8>, Vec<u8>)>> {
        let Some(roster) = fsx::read_owned(&path, MAX_READ)? else {
            return Ok(None);
        };
        let mut signature = path.into_os_string();
        signature.push(".sig");
        let signature = fsx::read_owned(Path::new(&signature), 64)?
            .ok_or_else(|| Error::new("release roster signature is missing"))?;
        Ok(Some((roster, signature)))
    };
    let mut candidates = Vec::new();
    for path in named.into_iter().chain([home.join(VAULT_ROSTER)]) {
        match read_pair(path) {
            Ok(Some(pair)) => candidates.push(pair),
            Ok(None) => {}
            Err(_) => return false,
        }
    }
    let Some((index, _)) = crate::release_ring::newest_roster(
        anchors,
        candidates
            .iter()
            .enumerate()
            .map(|(index, (raw, signature))| (index, raw.as_slice(), signature.as_slice())),
    ) else {
        return false;
    };
    let (roster, signature) = candidates.swap_remove(index);
    let floor_path = paths.dir.join("release-ring-roster-floor");
    let Ok(floor) = roster_floor(paths) else {
        return false;
    };
    let Ok(signer) = crate::release_ring::authorize_with(
        profile.as_slice(),
        key.as_slice(),
        roster.to_vec(),
        &signature,
        anchors,
        floor,
        now,
    ) else {
        return false;
    };
    !advance
        || signer.roster_seq == floor
        || fsx::write_atomic(
            &floor_path,
            format!("{}\n", signer.roster_seq).as_bytes(),
            0o600,
        )
        .is_ok()
}

/// Handle the current signed entry, preserving an already working profile: its line and the
/// successfully handled entry's digest. `None` when the entry cannot be read as the vault
/// applied it just now (another apply under way): the next round looks again. The password
/// is passed directly to the bounded tool and never copied to output, logs or errors.
fn materialize_with(
    paths: &Paths,
    tools: &mut Tools<'_>,
) -> crate::Result<Option<(&'static str, String)>> {
    let expected =
        config(paths)?.ok_or_else(|| Error::new("no vault configured; run aterm secrets join"))?;
    let Some(entry) = read_verified_entry(paths, &expected, ENTRY)? else {
        return Ok(None);
    };
    let digest = materialize::digest_hex(entry.value().as_slice());
    let notary = parse_notary(entry.value().as_slice())?;
    let authenticates = (tools.history)()?;
    check_notary_current(paths, &entry, tools)?;
    if authenticates {
        return Ok(Some(("notary profile already authenticates", digest)));
    }
    (tools.store)(&notary)?;
    let authenticates = (tools.history)()?;
    check_notary_current(paths, &entry, tools)?;
    if authenticates {
        Ok(Some(("notary profile stored and live-checked", digest)))
    } else {
        Err(Error::new(
            "notarytool stored a credential but did not authenticate on the live check",
        ))
    }
}

fn check_notary_current(
    paths: &Paths,
    entry: &super::entry::VerifiedEntry,
    tools: &Tools<'_>,
) -> crate::Result<()> {
    if !(tools.authorized_ring)(paths) || entry.snapshot().read(paths)?.is_none() {
        return Err(Error::new(
            "notary credential or release authority changed during authentication; retry",
        ));
    }
    Ok(())
}

/// Revalidate and materialize the authenticated shared notary credential, without prompting.
/// Returns `None` when no configured vault, authorized release ring or entry is available.
/// An existing profile that authenticates is preserved. A previous successful check never
/// suppresses this explicitly requested repair.
///
/// # Errors
/// The vault state or entry cannot be verified, developer tools are unavailable, or the
/// bounded credential storage/authentication operation fails.
pub fn sync_notary(paths: &Paths) -> crate::Result<Option<String>> {
    if !cfg!(target_os = "macos") || config(paths)?.is_none() {
        return Ok(None);
    }
    let mut history = LIVE_HISTORY;
    let mut store = LIVE_STORE;
    let mut tools = Tools {
        authorized_ring: &authorized_ring,
        developer_dir: &LIVE_DEVELOPER_DIR,
        keychain_answers: &LIVE_KEYCHAIN,
        history: &mut history,
        store: &mut store,
    };
    sync_notary_with(paths, &mut tools).map(|done| done.map(|(line, _)| line.to_owned()))
}

fn sync_notary_with(
    paths: &Paths,
    tools: &mut Tools<'_>,
) -> crate::Result<Option<(&'static str, String)>> {
    if !(tools.authorized_ring)(paths) || !load_state(paths)?.manifest.files.contains_key(ENTRY) {
        return Ok(None);
    }
    if (tools.developer_dir)().is_none() {
        return Err(Error::new(no_developer_dir()));
    }
    if !(tools.keychain_answers)() {
        return Err(Error::new(WAITS_FOR_KEYCHAIN));
    }
    let done = materialize_with(paths, tools);
    // What the release asked for is also what the subscriber's next round reads: a failure
    // here backs its retries off, and a success ends a backoff.
    if let (Ok(state), Some(now)) = (load_state(paths), aterm_time::unix_secs())
        && let Some(digest) = state.manifest.files.get(ENTRY)
    {
        match &done {
            Ok(Some(_)) => Failed::clear(paths),
            Ok(None) => {}
            Err(error) => {
                let _ = Failed::again(Failed::read(paths), digest, now, &error.to_string())
                    .write(paths);
            }
        }
    }
    done
}

/// An entry's failed attempt, kept on disk so that no restart forgets it: which entry, when,
/// how many attempts at it have failed in a row, and whether the last was cut off unanswered.
/// A round reads it before anything runs ([`NotaryRounds::step`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Failed {
    digest: String,
    at: u64,
    failures: u32,
    unanswered: bool,
    why: String,
}

impl Failed {
    fn path(paths: &Paths) -> PathBuf {
        paths.dir.join(NOTARY_FAILED_FILE)
    }

    /// The record on disk; `None` where there is none or it does not read as one.
    pub(super) fn read(paths: &Paths) -> Option<Self> {
        let bytes = fsx::read_private(&Self::path(paths), MAX_READ).ok()??;
        let mut fields = std::collections::BTreeMap::new();
        for line in bytes.as_str()?.lines() {
            let (name, value) = line.split_once('=')?;
            fields.insert(name, value);
        }
        Some(Self {
            digest: (*fields.get("digest")?).to_owned(),
            at: fields.get("at")?.parse().ok()?,
            failures: fields.get("failures")?.parse().ok().filter(|n| *n > 0)?,
            unanswered: match *fields.get("unanswered")? {
                "1" => true,
                "0" => false,
                _ => return None,
            },
            why: (*fields.get("why")?).to_owned(),
        })
    }

    /// The failure of `digest` at `now` for `why`, one more than `before` counted of that
    /// same entry.
    fn again(before: Option<Self>, digest: &str, now: u64, why: &str) -> Self {
        let failures = before
            .filter(|before| before.digest == digest)
            .map_or(1, |before| before.failures.saturating_add(1));
        Self {
            digest: digest.to_owned(),
            at: now,
            failures,
            unanswered: why.contains(UNANSWERED),
            why: why.replace(['\n', '\r'], " "),
        }
    }

    fn write(&self, paths: &Paths) -> crate::Result<()> {
        fsx::write_atomic(
            &Self::path(paths),
            format!(
                "digest={}\nat={}\nfailures={}\nunanswered={}\nwhy={}\n",
                self.digest,
                self.at,
                self.failures,
                u8::from(self.unanswered),
                self.why
            )
            .as_bytes(),
            0o600,
        )
    }

    fn clear(paths: &Paths) {
        let _ = std::fs::remove_file(Self::path(paths));
    }

    /// How long this failure holds the next attempt off.
    fn wait(&self) -> Duration {
        if self.unanswered {
            return NOTARY_RETRY_MAX;
        }
        let doublings = self.failures.saturating_sub(1).min(16);
        NOTARY_RETRY
            .saturating_mul(1 << doublings)
            .min(NOTARY_RETRY_MAX)
    }

    /// Whether this failure still holds an attempt at `digest` off at `now`.
    fn holds(&self, digest: &str, now: u64) -> bool {
        self.digest == digest
            && now
                .checked_sub(self.at)
                .is_some_and(|since| since < self.wait().as_secs())
    }
}

/// What one round's look at the entry decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Step {
    /// No entry, a recent successful check, or this subscriber is backing off: nothing to do.
    Quiet,
    /// The entry waits: why.
    Waits(&'static str),
    /// notarytool ran: its line, or why it failed.
    Ran(Result<&'static str, String>),
}

/// The subscriber's notary rounds: this subscriber's last successful check, the failure a
/// record could not be written for, and the last line said. A failed entry's backoff is on
/// disk ([`Failed`]), so transient failures recover without a restart and a lasting one is not
/// tried again by each new subscriber.
#[derive(Debug, Default)]
pub(super) struct NotaryRounds {
    signing: signing::Rounds,
    checked: Option<(String, Instant)>,
    unrecorded: Option<Failed>,
    said: Option<Step>,
}

impl NotaryRounds {
    /// A healthy round's look ([`Self::step`] with the live tools), saying a line when what it
    /// decided changed, and each failure.
    pub(super) fn after_round(&mut self, paths: &Paths, config: &Config, log: &dyn Fn(&str)) {
        self.signing.after_round(paths, config, log);
        let mut history = LIVE_HISTORY;
        let mut store = LIVE_STORE;
        let mut tools = Tools {
            authorized_ring: &authorized_ring,
            developer_dir: &LIVE_DEVELOPER_DIR,
            keychain_answers: &LIVE_KEYCHAIN,
            history: &mut history,
            store: &mut store,
        };
        let step = self.step(paths, config, &mut tools);
        if self.said.as_ref() != Some(&step) || matches!(step, Step::Ran(Err(_))) {
            match &step {
                Step::Quiet => {}
                Step::Waits(why) => log(why),
                Step::Ran(Ok(line)) => log(line),
                Step::Ran(Err(why)) => log(&format!("notary provisioning: {why}")),
            }
        }
        self.said = Some(step);
    }

    /// Recheck an unchanged entry after thirty minutes and after every subscriber restart.
    /// The release ring, developer directory and login keychain are checked before notarytool
    /// runs. A failed attempt backs off on disk ([`Failed`]); a changed credential is eligible
    /// immediately.
    pub(super) fn step(&mut self, paths: &Paths, _config: &Config, tools: &mut Tools<'_>) -> Step {
        self.step_with_clock(paths, tools, Instant::now, aterm_time::unix_secs)
    }

    /// [`Self::step`] reading `clock` for this subscriber's own successful check and `wall`
    /// for the failure on disk. With no wall clock nothing can be backed off across a restart,
    /// so nothing runs.
    fn step_with_clock(
        &mut self,
        paths: &Paths,
        tools: &mut Tools<'_>,
        clock: impl Fn() -> Instant,
        wall: impl Fn() -> Option<u64>,
    ) -> Step {
        let now = clock();
        let Ok(state) = load_state(paths) else {
            return Step::Quiet;
        };
        let Some(digest) = state.manifest.files.get(ENTRY) else {
            return Step::Quiet;
        };
        let Some(wall_now) = wall() else {
            return Step::Quiet;
        };
        // The later of the record on disk and one that could not be written.
        let failed = [Failed::read(paths), self.unrecorded.clone()]
            .into_iter()
            .flatten()
            .max_by_key(|failed| failed.at);
        if self.checked.as_ref().is_some_and(|(checked, at)| {
            checked == digest && now.saturating_duration_since(*at) < NOTARY_RECHECK
        }) || failed
            .as_ref()
            .is_some_and(|failed| failed.holds(digest, wall_now))
        {
            return Step::Quiet;
        }
        if !(tools.authorized_ring)(paths) {
            return Step::Waits(NOT_RELEASE_HOST);
        }
        if (tools.developer_dir)().is_none() {
            return Step::Waits(no_developer_dir());
        }
        if !(tools.keychain_answers)() {
            return Step::Waits(WAITS_FOR_KEYCHAIN);
        }
        self.checked = None;
        match materialize_with(paths, tools) {
            Ok(Some((line, stored))) => {
                self.checked = Some((stored, clock()));
                self.unrecorded = None;
                Failed::clear(paths);
                Step::Ran(Ok(line))
            }
            Ok(None) => Step::Quiet,
            Err(error) => {
                let why = error.to_string();
                let again = Failed::again(failed, digest, wall().unwrap_or(wall_now), &why);
                self.unrecorded = again.write(paths).is_err().then_some(again);
                Step::Ran(Err(why))
            }
        }
    }
}

#[cfg(test)]
mod tests;
