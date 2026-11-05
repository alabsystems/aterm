// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHAT A CREDIT CLAIM NEEDS IN PLACE BEFORE ITS TRY IS SPENT ([`super::step`]'s
//! [`super::Seams::left`]): Google Chrome, and Claude in Chrome's extension ([`EXTENSION_ID`])
//! turned on in one of its profiles: found as Claude Code's own `--chrome` check finds it (a
//! profile's `Extensions/<id>` folder), and on where that profile's settings name it with no
//! reason to keep it off ([`turned_on`]), since Chrome adds an extension its external list names
//! turned off until a person turns it on. A look finds each and puts in place what aterm can with
//! nobody's hand: Chrome by atpkg's brew (the [`CHROME_CASK`] cask, installed in the background
//! unless a person keeps it off, [`crate::task_launch::AtpkgStore::cask_held`], or removed a
//! Chrome that was here, [`Places::was_here`]), and the extension by Chrome's own list of
//! external extensions (a file naming the Web Store's update link, which Chrome reads as it
//! starts and adds the extension from, [`listing_text`]); a different file there is never
//! replaced. What only a person can do is left in the words ([`Readiness::left`]): to turn on the
//! extension Chrome added and to sign in to claude.ai there. Claude Code's native messaging host
//! is its own `--chrome` launch's to write, never aterm's. Every read and write goes through a
//! [`Probe`], so the rules are tested on a made-up machine.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

/// Claude in Chrome's extension id in the Chrome Web Store.
pub(crate) const EXTENSION_ID: &str = "fcoeoabgfenejglbffodgkkbkcdhcgfn";

/// The Chrome Web Store's update link an external-extension file names ([`listing_text`]).
pub(crate) const UPDATE_URL: &str = "https://clients2.google.com/service/update2/crx";

/// The cask atpkg's brew installs Google Chrome by.
#[cfg(target_os = "macos")]
pub(crate) const CHROME_CASK: &str = "google-chrome";

/// Google Chrome's app bundle.
pub(crate) const CHROME_APP: &str = "Google Chrome.app";

/// How long after an install of Chrome ended that left none in place a look starts another.
pub(crate) const INSTALL_AGAIN: Duration = Duration::from_secs(60 * 60);

/// The most bytes of an external-extension file a look reads.
pub(crate) const LISTING_MAX: usize = 4096;

/// The most bytes of a Chrome profile's settings file (`Secure Preferences`, `Preferences`) a
/// look reads: one far larger than any profile writes is no reading of it.
pub(crate) const PREFERENCES_MAX: usize = 8 * 1024 * 1024;

/// A profile's settings files that name its extensions, in the order a look reads them: Chrome on
/// a Mac keeps them in `Secure Preferences`, and an older one in `Preferences`.
pub(crate) const PREFERENCES: [&str; 2] = ["Secure Preferences", "Preferences"];

/// The file in Chrome's user data folder that Chrome writes as it first runs, and no one else.
pub(crate) const LOCAL_STATE: &str = "Local State";

/// Where a look reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Places {
    /// The folders a [`CHROME_APP`] is looked for in, in order.
    pub(crate) apps: Vec<PathBuf>,
    /// Chrome's user data folder: its profiles, and its list of external extensions.
    pub(crate) chrome: PathBuf,
    /// The mark aterm leaves once its install put a Chrome in place ([`INSTALLED_MARK`] in
    /// aterm's state folder), `None` where there is no state folder.
    pub(crate) installed: Option<PathBuf>,
}

/// The name of aterm's mark that its install put a Chrome in place ([`Places::installed`]).
pub(crate) const INSTALLED_MARK: &str = "claim-chrome-installed";

impl Places {
    /// On a Mac whose user home is `home`, where atpkg's brew installs a cask's app into
    /// `brew_apps` ([`atpkg::abrew::prefix::Prefix::applications`]): `/Applications`,
    /// `~/Applications`, then that folder; aterm's mark of its install in `state`.
    pub(crate) fn mac(home: &Path, brew_apps: PathBuf, state: Option<&Path>) -> Self {
        Self {
            apps: vec![
                PathBuf::from("/Applications"),
                home.join("Applications"),
                brew_apps,
            ],
            chrome: home.join("Library/Application Support/Google/Chrome"),
            installed: state.map(|state| state.join(INSTALLED_MARK)),
        }
    }

    /// Whether a Google Chrome was here before, found in none of [`Self::apps`] now: Chrome ran
    /// here (its [`LOCAL_STATE`]), or aterm's install put one in place ([`Self::installed`]).
    /// Its app gone, a person removed it, however they did (the Trash, a brew uninstall), and
    /// aterm installs none again.
    pub(crate) fn was_here(&self, probe: &dyn Probe) -> bool {
        probe.is_file(&self.chrome.join(LOCAL_STATE))
            || self
                .installed
                .as_deref()
                .is_some_and(|mark| probe.is_file(mark))
    }

    /// The extension's external-extension file in Chrome's user data folder.
    pub(crate) fn listing(&self) -> PathBuf {
        self.chrome
            .join("External Extensions")
            .join(format!("{EXTENSION_ID}.json"))
    }
}

/// The external-extension file aterm puts in place: the Web Store's update link, from which
/// Chrome adds the extension as it starts.
pub(crate) fn listing_text() -> String {
    format!("{{\n  \"external_update_url\": \"{UPDATE_URL}\"\n}}\n")
}

/// What a look reads and writes of the machine ([`look`]).
pub(crate) trait Probe {
    /// Whether `path` is a folder (an app bundle is one).
    fn is_dir(&self, path: &Path) -> bool;
    /// Whether `path` is a file.
    fn is_file(&self, path: &Path) -> bool;
    /// The names in folder `dir`, none where it cannot be read.
    fn names(&self, dir: &Path) -> Vec<OsString>;
    /// File `path`'s bytes, at most `max`, or `Ok(None)` where nothing is there.
    ///
    /// # Errors
    ///
    /// Why it could not be read, a file larger than `max` included.
    fn read(&self, path: &Path, max: usize) -> Result<Option<Vec<u8>>, String>;
    /// `bytes` put at `path`, atomically, where nothing is there: `Ok(false)` where something
    /// already is, which stays as it is.
    ///
    /// # Errors
    ///
    /// Why it could not be written.
    fn put_new(&self, path: &Path, bytes: &[u8]) -> Result<bool, String>;
    /// Why atpkg's brew installs no Chrome here, where it does not.
    fn held(&self) -> Option<String>;
    /// Start Chrome's install in the background; its end reaches [`Installs::ended`].
    fn install(&self);
}

/// How the Chrome install this process started stands ([`Installs`]).
#[derive(Clone, Debug, PartialEq, Eq)]
enum Install {
    /// None started.
    Never,
    /// One runs.
    Running,
    /// The last one ended at `at`, with why it failed where it did.
    Ended { at: Instant, failed: Option<String> },
}

/// The Chrome install a look starts ([`look`]): one at a time, and another only
/// [`INSTALL_AGAIN`] after one that left no Chrome in place.
pub(crate) struct Installs {
    state: Mutex<Install>,
}

impl Installs {
    pub(crate) const fn new() -> Self {
        Self {
            state: Mutex::new(Install::Never),
        }
    }

    /// With Chrome found nowhere at `now`: `None` where the caller is to start an install (none
    /// ran, or the last ended [`INSTALL_AGAIN`] ago), now marked running; else how Chrome stands.
    fn begin(&self, now: Instant) -> Option<Chrome> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        match &*state {
            Install::Running => Some(Chrome::Installing),
            Install::Ended { at, failed } if now.saturating_duration_since(*at) < INSTALL_AGAIN => {
                Some(Chrome::Failed(failed.clone().unwrap_or_else(|| {
                    "atpkg's brew finished, yet no Google Chrome.app is in place".to_string()
                })))
            }
            Install::Never | Install::Ended { .. } => {
                *state = Install::Running;
                None
            }
        }
    }

    /// The install ended at `now`: `result` says how.
    pub(crate) fn ended(&self, result: Result<(), String>, now: Instant) {
        *self.state.lock().unwrap_or_else(PoisonError::into_inner) = Install::Ended {
            at: now,
            failed: result.err(),
        };
    }
}

/// The host's one [`Installs`].
#[cfg(target_os = "macos")]
pub(crate) static INSTALLS: Installs = Installs::new();

/// Where Google Chrome stands ([`Readiness`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Chrome {
    /// In place: this app bundle.
    At(PathBuf),
    /// atpkg's brew is installing it.
    Installing,
    /// aterm installs none here: why (a person keeps it off, or removed the one that was here).
    Held(String),
    /// The last install left none in place: why. Another starts [`INSTALL_AGAIN`] after it.
    Failed(String),
}

/// Where Claude in Chrome's extension stands ([`Readiness`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Extension {
    /// In a Chrome profile, turned on there.
    Added,
    /// In a Chrome profile, turned on in none: Chrome adds an extension its external list names
    /// turned off, until a person turns it on. With why a profile's settings could not be read,
    /// where one could not.
    Off(Option<String>),
    /// Listed for Chrome to add from the Web Store, in no profile yet.
    Listed,
    /// Not listed: why.
    Unlisted(String),
}

/// What one look found ([`look`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Readiness {
    pub(crate) chrome: Chrome,
    pub(crate) extension: Extension,
}

impl Readiness {
    /// Chrome is in place and the extension turned on in one of its profiles.
    pub(crate) fn ready(&self) -> bool {
        matches!(self.chrome, Chrome::At(_)) && self.extension == Extension::Added
    }

    /// What is in place and what is left, a line each, where the claim waits ([`ready`]
    /// `false`), else `None`. Signing in to claude.ai in Chrome is always left: no file says it.
    ///
    /// [`ready`]: Self::ready
    pub(crate) fn left(&self) -> Option<String> {
        if self.ready() {
            return None;
        }
        let mut have: Vec<String> = Vec::new();
        let mut left: Vec<String> = Vec::new();
        match &self.chrome {
            Chrome::At(_) => have.push("Google Chrome".to_string()),
            Chrome::Installing => {
                left.push("Google Chrome, which atpkg's brew is installing now".to_string());
            }
            Chrome::Held(why) => left.push(format!(
                "Google Chrome, which aterm installs none of here ({why})"
            )),
            Chrome::Failed(why) => left.push(format!(
                "Google Chrome, whose install by atpkg's brew left none in place ({why}); aterm \
                 tries again later"
            )),
        }
        match &self.extension {
            Extension::Added => have.push("the Claude extension in a Chrome profile".to_string()),
            Extension::Off(unread) => {
                have.push("the Claude extension, added to Chrome".to_string());
                left.push(match unread {
                    None => {
                        "turn on the Claude extension in Chrome (chrome://extensions)".to_string()
                    }
                    Some(why) => format!(
                        "turn on the Claude extension in Chrome (chrome://extensions); aterm \
                         could not read whether it is on ({why})"
                    ),
                });
            }
            Extension::Listed => {
                have.push("the Claude extension, listed for Chrome to add".to_string());
                left.push("open Chrome and enable the Claude extension it adds".to_string());
            }
            Extension::Unlisted(why) => left.push(format!(
                "the Claude extension, which aterm could not list for Chrome ({why}); add it \
                 from https://claude.ai/chrome"
            )),
        }
        left.push("sign in to claude.ai in Chrome".to_string());
        let mut lines = Vec::new();
        if !have.is_empty() {
            lines.push(format!("In place: {}.", have.join("; ")));
        }
        lines.push(format!("Left: {}.", left.join("; ")));
        Some(lines.join("\n"))
    }
}

/// One look at `places` through `probe` at `now`: Chrome found in an app folder, else its
/// install started where none runs, a person keeps none off and none was here before
/// ([`Places::was_here`], [`Installs::begin`]); the extension found turned on in a profile, else
/// found turned off, else its listing put in place where nothing stands there yet, and an
/// equivalent listing someone else wrote counts.
pub(crate) fn look(
    places: &Places,
    probe: &dyn Probe,
    installs: &Installs,
    now: Instant,
) -> Readiness {
    let chrome = match places
        .apps
        .iter()
        .map(|folder| folder.join(CHROME_APP))
        .find(|app| probe.is_dir(app))
    {
        Some(app) => Chrome::At(app),
        None => match probe.held() {
            Some(why) => Chrome::Held(why),
            None if places.was_here(probe) => Chrome::Held(
                "a Google Chrome was here and is gone, which aterm takes as a person's removal; \
                 installing it again brings the claim back"
                    .to_string(),
            ),
            None => installs.begin(now).unwrap_or_else(|| {
                probe.install();
                Chrome::Installing
            }),
        },
    };
    let added: Vec<PathBuf> = probe
        .names(&places.chrome)
        .into_iter()
        .map(|profile| places.chrome.join(profile))
        .filter(|profile| probe.is_dir(&profile.join("Extensions").join(EXTENSION_ID)))
        .collect();
    let extension = if added.is_empty() {
        listed(&places.listing(), probe)
    } else {
        let mut unread = None;
        let on = added.iter().any(|profile| match turned_on(profile, probe) {
            Ok(on) => on,
            Err(why) => {
                unread.get_or_insert(why);
                false
            }
        });
        if on {
            Extension::Added
        } else {
            Extension::Off(unread)
        }
    };
    Readiness { chrome, extension }
}

/// Whether Claude in Chrome's extension is turned on in Chrome profile folder `profile`: the
/// first of its [`PREFERENCES`] files that names it under `extensions.settings` says so, on where
/// it gives no reason to keep it off (`disable_reasons` absent, `0` or empty, the form a newer
/// Chrome writes) and no `state` but `1` (an older Chrome's). Named in neither, it is not on:
/// Chrome writes an extension's settings as it adds it.
///
/// # Errors
///
/// Why a settings file that is there could not be read, or does not read as settings.
fn turned_on(profile: &Path, probe: &dyn Probe) -> Result<bool, String> {
    use aterm_json::Value;
    for name in PREFERENCES {
        let Some(bytes) = probe
            .read(&profile.join(name), PREFERENCES_MAX)
            .map_err(|why| format!("{name}: {why}"))?
        else {
            continue;
        };
        let settings = aterm_json::from_slice::<Value>(&bytes)
            .map_err(|e| format!("{name} is not JSON: {e}"))?;
        let Some(entry) = settings
            .get("extensions")
            .and_then(|extensions| extensions.get("settings"))
            .and_then(|all| all.get(EXTENSION_ID))
        else {
            continue;
        };
        let no_reason = match entry.get("disable_reasons") {
            None => true,
            Some(Value::Array(reasons)) => reasons.is_empty(),
            Some(reasons) => reasons.as_i64() == Some(0),
        };
        let state_on = entry
            .get("state")
            .is_none_or(|state| state.as_i64() == Some(1));
        return Ok(no_reason && state_on);
    }
    Ok(false)
}

/// The extension's listing at `path`: one already there judged, else [`listing_text`] put there.
fn listed(path: &Path, probe: &dyn Probe) -> Extension {
    let unread = |why: String| Extension::Unlisted(format!("its listing could not be read: {why}"));
    match probe.read(path, LISTING_MAX) {
        Ok(Some(bytes)) => judged(&bytes),
        Ok(None) => match probe.put_new(path, listing_text().as_bytes()) {
            Ok(true) => Extension::Listed,
            Ok(false) => match probe.read(path, LISTING_MAX) {
                Ok(Some(bytes)) => judged(&bytes),
                Ok(None) => {
                    Extension::Unlisted("its listing came and went as aterm wrote it".to_string())
                }
                Err(why) => unread(why),
            },
            Err(why) => Extension::Unlisted(format!("its listing could not be written: {why}")),
        },
        Err(why) => unread(why),
    }
}

/// An external-extension file found where the listing goes: listed where it names the Web
/// Store's update link however it is laid out, else someone else's, which stays.
fn judged(bytes: &[u8]) -> Extension {
    let names_the_store = aterm_json::from_slice::<aterm_json::Value>(bytes)
        .ok()
        .and_then(|value| {
            value
                .get("external_update_url")
                .and_then(aterm_json::Value::as_str)
                .map(|url| url == UPDATE_URL)
        })
        .unwrap_or(false);
    if names_the_store {
        Extension::Listed
    } else {
        Extension::Unlisted(format!(
            "a different file stands at Chrome's External Extensions/{EXTENSION_ID}.json, \
             which aterm leaves as it is"
        ))
    }
}

/// [`Probe::put_new`] on this machine: `bytes` written to a staged file beside `path` (owner-only,
/// flushed), then linked to `path`, which fails where anything stands there, so nothing is ever
/// replaced and no reader sees a part-written file; the staged name goes either way.
///
/// # Errors
///
/// Why the folder could not be made, or the file written or linked.
#[cfg(unix)]
pub(crate) fn put_new(path: &Path, bytes: &[u8]) -> Result<bool, String> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return Err("it names no file in a folder".to_string());
    };
    std::fs::create_dir_all(dir).map_err(|e| format!("its folder could not be made ({e})"))?;
    let staged = dir.join(format!(
        ".{}.aterm-{}",
        name.to_string_lossy(),
        std::process::id()
    ));
    let _ = std::fs::remove_file(&staged);
    let linked = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&staged)
        .and_then(|mut file| {
            file.write_all(bytes)?;
            aterm_durable::durable_data(&file)
        })
        .and_then(|()| match std::fs::hard_link(&staged, path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(e) => Err(e),
        });
    let _ = std::fs::remove_file(&staged);
    if linked.as_ref().is_ok_and(|&placed| placed) {
        let _ = aterm_durable::durable_dir(dir);
    }
    linked.map_err(|e| e.to_string())
}

/// This Mac, as a look reads and writes it: an install that put a Chrome in place at `app` (atpkg's
/// brew's) leaves the mark at `installed` ([`Places::installed`]).
#[cfg(target_os = "macos")]
pub(crate) struct Machine {
    pub(crate) app: PathBuf,
    pub(crate) installed: Option<PathBuf>,
}

/// The most profiles in Chrome's user data folder a look reads.
#[cfg(target_os = "macos")]
const PROFILES_MAX: usize = 64;

#[cfg(target_os = "macos")]
impl Probe for Machine {
    fn is_dir(&self, path: &Path) -> bool {
        path.is_dir()
    }

    fn is_file(&self, path: &Path) -> bool {
        std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file())
    }

    fn names(&self, dir: &Path) -> Vec<OsString> {
        std::fs::read_dir(dir)
            .map(|entries| {
                entries
                    .flatten()
                    .take(PROFILES_MAX)
                    .map(|entry| entry.file_name())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn read(&self, path: &Path, max: usize) -> Result<Option<Vec<u8>>, String> {
        use std::io::Read as _;
        let (file, meta) = match crate::logging::open_regular_nonblock_with_meta(path) {
            Ok(opened) => opened,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.to_string()),
        };
        let max_u64 = u64::try_from(max).unwrap_or(u64::MAX);
        if meta.len() > max_u64 {
            return Err(format!("it is larger than {max} bytes"));
        }
        let mut bytes =
            Vec::with_capacity(usize::try_from(meta.len().saturating_add(1)).unwrap_or(0));
        file.take(max_u64.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > max {
            return Err(format!("it is larger than {max} bytes"));
        }
        Ok(Some(bytes))
    }

    fn put_new(&self, path: &Path, bytes: &[u8]) -> Result<bool, String> {
        put_new(path, bytes)
    }

    fn held(&self) -> Option<String> {
        crate::task_launch::AtpkgStore::cask_held(CHROME_CASK)
    }

    fn install(&self) {
        let (app, installed) = (self.app.clone(), self.installed.clone());
        let spawned = std::thread::Builder::new()
            .name("claim-chrome-install".to_string())
            .spawn(move || {
                aterm_log::info!(
                    "credit claim: installing Google Chrome by atpkg's brew ({CHROME_CASK})"
                );
                let result = crate::task_launch::AtpkgStore::install_cask(CHROME_CASK);
                match &result {
                    Ok(()) => {
                        aterm_log::info!("credit claim: atpkg's brew installed Google Chrome");
                        // Its removal stands from here on ([`Places::was_here`]), even one before
                        // Chrome ever ran.
                        if let Some(mark) = installed.as_deref().filter(|_| app.is_dir())
                            && let Err(why) = put_new(mark, format!("{CHROME_CASK}\n").as_bytes())
                        {
                            aterm_log::warn!(
                                "credit claim: the mark of Chrome's install could not be \
                                 written: {why}"
                            );
                        }
                    }
                    Err(why) => aterm_log::warn!(
                        "credit claim: atpkg's brew did not install Google Chrome: {why}"
                    ),
                }
                INSTALLS.ended(result, Instant::now());
            });
        if let Err(e) = spawned {
            INSTALLS.ended(
                Err(format!("its thread could not start ({e})")),
                Instant::now(),
            );
        }
    }
}

/// This Mac's [`look`] at `now`, in [`Readiness::left`]'s words.
#[cfg(target_os = "macos")]
pub(crate) fn here(now: Instant) -> Option<String> {
    let Some(home) = aterm_types::dirs::home_dir() else {
        return Some("Left: a home folder to look for Google Chrome in.".to_string());
    };
    let brew_apps = atpkg::abrew::prefix::Prefix::resolve().applications();
    let app = brew_apps.join(CHROME_APP);
    let places = Places::mac(&home, brew_apps, aterm_types::dirs::state_dir().as_deref());
    let machine = Machine {
        app,
        installed: places.installed.clone(),
    };
    look(&places, &machine, &INSTALLS, now).left()
}

#[cfg(test)]
#[path = "harness_claim_ready_tests.rs"]
mod tests;
