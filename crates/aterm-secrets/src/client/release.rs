// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Local release-credential materialization after an authenticated vault apply
//! (docs/DESIGN-alab-swarm-2026-10-27.md §5.2, C1). The signed `release-notary` entry can
//! populate the `notary` keychain profile `ship release` notarizes with; an existing profile
//! that authenticates is preserved. Notarytool runs only where all of these hold, with no
//! child process spawned by their preflight reads:
//!
//! - this Mac is a release host: `~/.aterm/release-ring/release-credentials.toml` (or the 0.145
//!   `~/.aterm/release-credentials.toml` before its vault paths rotate) is there, and the release
//!   roster names this Mac's vault name (the name it asked the vault under, never
//!   `~/.aterm/release-ring/machine.toml`, which is the release ring every member holds and names
//!   m22, nor `~/.aterm/machine.toml`) as a machine it has not revoked. The swarm policy's
//!   release-host list replaces this read once that entry exists;
//! - xcrun has a developer directory that holds `notarytool` (the Command Line Tools', or the
//!   selected Xcode's): without one, `/usr/bin/xcrun` puts Apple's install dialog on screen;
//! - the entry differs from the last one handled successfully ([`APPLIED`]), and from one
//!   this subscriber already tried and failed with.
//!
//! The name `notary`, which 0.145 reads after every applied version, is never read here.

use super::*;
use std::ffi::OsString;
use zeroize::Zeroizing;

/// The vault entry this reads.
pub(crate) const ENTRY: &str = "release-notary";

/// The record, in the vault's state folder, of the entry's digest last handled successfully
/// while the profile authenticated (SHA-256 hex, as `state.toml`'s manifest already keeps it).
const APPLIED: &str = "release-notary.applied";

/// The release-credentials profile, under the home (`~/.aterm/release-ring/`, with fallback to
/// the 0.145 `~/.aterm/` location while an older vault has not yet rotated its path).
const RELEASE_PROFILE: &str = ".aterm/release-ring/release-credentials.toml";
const LEGACY_RELEASE_PROFILE: &str = ".aterm/release-credentials.toml";

/// Where the vault puts the release roster, under the home: read when the profile names none,
/// or names one that is not there.
const VAULT_ROSTER: &str = ".aterm/release-ring/roster/aterm-machines.toml";
const LEGACY_VAULT_ROSTER: &str = ".aterm/roster/aterm-machines.toml";

/// The largest profile or roster read.
const MAX_READ: usize = 64 * 1024;

/// Where `xcode-select -s` keeps the developer directory a person chose.
const XCODE_SELECT_LINK: &str = "/var/db/xcode_select_link";

/// The developer directory xcrun takes with no choice made and Xcode installed.
const XCODE_DEVELOPER: &str = "/Applications/Xcode.app/Contents/Developer";

/// The Command Line Tools' developer directory.
const CLT_DEVELOPER: &str = "/Library/Developer/CommandLineTools";

/// What a round says while this Mac is no release host.
const NOT_RELEASE_HOST: &str = "release-notary is left alone: this Mac is no release host \
     (no ~/.aterm/release-ring/release-credentials.toml, or the release roster does not name \
     this Mac's vault name)";

/// What a round says while xcrun has nothing to run notarytool with.
const WAITS_FOR_CLT: &str = "release-notary waits for Command Line Tools: xcrun has no \
     developer directory with notarytool, so nothing runs it";

pub(super) struct Notary {
    apple_id: Zeroizing<String>,
    team_id: Zeroizing<String>,
    password: Zeroizing<String>,
}

fn parse_notary(bytes: &[u8]) -> crate::Result<Notary> {
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
    /// The developer directory xcrun selects, where it holds notarytool ([`developer_dir_with`]).
    pub(super) developer_dir: &'a dyn Fn() -> Option<PathBuf>,
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
    use std::{io::Write, process::Output};

    /// The keychain profile `notarytool store-credentials` keeps, which `ship release` names.
    const PROFILE: &str = "notary";

    fn history() -> crate::Result<Output> {
        aterm_uds::spawnfd::command("/usr/bin/xcrun")
            .args(["notarytool", "history", "--keychain-profile", PROFILE])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .map_err(|error| Error::new(format!("could not run notarytool: {error}")))
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
        let mut child = aterm_uds::spawnfd::command("/usr/bin/script")
            .args([
                "-q",
                "/dev/null",
                "/usr/bin/xcrun",
                "notarytool",
                "store-credentials",
                PROFILE,
                "--apple-id",
                notary.apple_id.as_str(),
                "--team-id",
                notary.team_id.as_str(),
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| Error::new(format!("could not start notarytool: {error}")))?;
        let mut input = child
            .stdin
            .take()
            .ok_or_else(|| Error::new("notarytool has no password input"))?;
        input
            .write_all(notary.password.as_bytes())
            .and_then(|()| input.write_all(b"\n"))
            .map_err(|error| Error::new(format!("could not answer notarytool: {error}")))?;
        drop(input);
        let status = child
            .wait()
            .map_err(|error| Error::new(format!("could not wait for notarytool: {error}")))?;
        if status.success() {
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
/// `home`, as `ship release` reads it; only that line is read.
fn named_roster(text: &str, home: &Path) -> Option<PathBuf> {
    text.lines().find_map(|line| {
        let value = line
            .trim()
            .strip_prefix("machine_roster")?
            .trim_start()
            .strip_prefix('=')?
            .trim();
        let value = value.strip_prefix('"')?.strip_suffix('"')?;
        match value.strip_prefix("~/") {
            Some(rest) => Some(home.join(rest.trim_start_matches('/'))),
            None => Some(PathBuf::from(value)),
        }
    })
}

/// Whether the release roster `text` names `host` as a machine and has not revoked it. Names
/// only: this narrows where notarytool runs, and is no trust decision (the entry itself is the
/// vault's signed one), so the roster's signature and dates are `ship release`'s to check.
fn roster_names(text: &str, host: &str) -> bool {
    let Ok(aterm_toml::Value::Table(roster)) = aterm_toml::from_str::<aterm_toml::Value>(text)
    else {
        return false;
    };
    let strings = |value: Option<&aterm_toml::Value>| -> Vec<String> {
        value
            .and_then(aterm_toml::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(aterm_toml::Value::as_str)
            .map(str::to_owned)
            .collect()
    };
    let listed = roster
        .get("machine")
        .and_then(aterm_toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(aterm_toml::Value::as_table)
        .any(|machine| machine.get("id").and_then(aterm_toml::Value::as_str) == Some(host));
    listed && !strings(roster.get("revoked")).iter().any(|id| id == host)
}

/// Whether this Mac, called `host` in its vault, is a release host: its home holds the
/// release-credentials profile, and the roster that profile names (else the one the vault puts
/// beside it) names `host`.
fn release_host(home: &Path, host: &str) -> bool {
    let profile = match fsx::read_private(&home.join(RELEASE_PROFILE), MAX_READ) {
        Ok(Some(profile)) => profile,
        Ok(None) => match fsx::read_private(&home.join(LEGACY_RELEASE_PROFILE), MAX_READ) {
            Ok(Some(profile)) => profile,
            Ok(None) | Err(_) => return false,
        },
        Err(_) => return false,
    };
    let named = profile.as_str().and_then(|text| named_roster(text, home));
    drop(profile);
    named
        .into_iter()
        .chain([home.join(VAULT_ROSTER), home.join(LEGACY_VAULT_ROSTER)])
        .find_map(|roster| fsx::read_owned(&roster, MAX_READ).ok().flatten())
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .is_some_and(|text| roster_names(&text, host))
}

/// The entry digest last handled successfully ([`APPLIED`]).
fn applied(paths: &Paths) -> Option<String> {
    let bytes = fsx::read_private(&paths.dir.join(APPLIED), 128).ok()??;
    Some(bytes.as_str()?.trim().to_owned())
}

/// Handle the current signed entry, preserving an already working profile: its line and the
/// successfully handled entry's digest. `None` when the entry cannot be read as the vault
/// applied it just now (another apply under way): the next round looks again. The password
/// goes only to the PTY's stdin and never to argv, output, logs or errors.
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
    if (tools.history)()? {
        return Ok(Some(("notary profile already authenticates", digest)));
    }
    (tools.store)(&notary)?;
    if (tools.history)()? {
        Ok(Some(("notary profile stored and live-checked", digest)))
    } else {
        Err(Error::new(
            "notarytool stored a credential but did not authenticate on the live check",
        ))
    }
}

/// What one round's look at the entry decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Step {
    /// No entry, its digest was handled successfully, or this subscriber tried it and failed:
    /// nothing to do.
    Quiet,
    /// The entry waits: why.
    Waits(&'static str),
    /// notarytool ran: its line, or why it failed.
    Ran(Result<&'static str, String>),
}

/// The subscriber's notary rounds: the entry it tried and failed with (tried again only once
/// the entry changes, or by the next subscriber, as after an update), and the last line said.
#[derive(Debug, Default)]
pub(super) struct NotaryRounds {
    tried: Option<String>,
    said: Option<Step>,
}

impl NotaryRounds {
    /// A healthy round's look ([`Self::step`] with the live tools), saying a line when what it
    /// decided changed, and each failure.
    pub(super) fn after_round(&mut self, paths: &Paths, config: &Config, log: &dyn Fn(&str)) {
        let mut history = LIVE_HISTORY;
        let mut store = LIVE_STORE;
        let mut tools = Tools {
            developer_dir: &LIVE_DEVELOPER_DIR,
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

    /// One round: no work without a `release-notary` entry, or for its last successfully
    /// handled digest or this subscriber's last failed attempt. Otherwise a release host and
    /// developer directory are checked with file reads before notarytool runs. A successfully
    /// handled entry's digest is kept in [`APPLIED`]; a failed attempt is kept for this
    /// subscriber's life.
    pub(super) fn step(&mut self, paths: &Paths, config: &Config, tools: &mut Tools<'_>) -> Step {
        let Ok(state) = load_state(paths) else {
            return Step::Quiet;
        };
        let Some(digest) = state.manifest.files.get(ENTRY) else {
            return Step::Quiet;
        };
        if applied(paths).as_ref() == Some(digest) || self.tried.as_ref() == Some(digest) {
            return Step::Quiet;
        }
        if !release_host(&paths.home, &config.host) {
            return Step::Waits(NOT_RELEASE_HOST);
        }
        if (tools.developer_dir)().is_none() {
            return Step::Waits(WAITS_FOR_CLT);
        }
        match materialize_with(paths, tools) {
            Ok(Some((line, stored))) => {
                if let Err(error) = fsx::write_atomic(
                    &paths.dir.join(APPLIED),
                    format!("{stored}\n").as_bytes(),
                    0o600,
                ) {
                    // Suppress this digest for the rest of this subscriber's life even if
                    // its record could not be kept. A new subscriber or changed entry can
                    // check again; a profile that still authenticates needs no replacement.
                    self.tried = Some(stored);
                    return Step::Ran(Err(format!(
                        "{line}, but its record could not be kept: {error}"
                    )));
                }
                Step::Ran(Ok(line))
            }
            Ok(None) => Step::Quiet,
            Err(error) => {
                self.tried = Some(digest.clone());
                Step::Ran(Err(error.to_string()))
            }
        }
    }
}

#[cfg(test)]
mod tests;
