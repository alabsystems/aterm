// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A row's own browser profile opened in a window, for a person to sign in to the vendor's site
//! once (`aterm harness accounts browser`, docs/DESIGN-ai-accounts-multi-cli-2026-10-23.md §6).
//! The window takes the headless approver's profile and its flags
//! ([`super::headless_oauth::profile_args`]) with no headless or DevTools flag, so the browser
//! is not marked as automated and the cookies it saves are the ones the next headless launch
//! reads. It is ended gently once the person is done (a hangup, which keeps the cookies, never a
//! terminate) and killed only after its grace. One launch at a time holds a profile
//! ([`ProfileLock`]). Before the window its password manager's offers are turned off
//! ([`quiet_password_manager`]); after it the profile is opened headlessly once more
//! ([`tidy_profile`]) to drop every identity provider's cookies, since its mock keychain keeps
//! them under a key any program of the user could read, and to ask whether the vendor's web app
//! keeps its session there. No cookie's or stored value is read or printed.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_json::{Value, json};

/// A Chromium that opens a window, by its executable inside an app bundle in an applications
/// folder: Google Chrome for Testing, else Chromium. Never branded Google Chrome, the owner's
/// own browser, whose keychain a mock keychain was never measured against.
const APP_BUNDLES: &[&str] = &[
    "Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
    "Chromium.app/Contents/MacOS/Chromium",
];

/// The executable inside a Chrome for Testing archive unpacked under the browser cache, for each
/// platform the pin covers ([`aterm_uds::host::chrome::window_pinned_binary`]): in
/// `chrome-<version>-<platform>/` (where [`fetch_window_browser`] places the pinned one), or
/// straight under the cache, where its zip unpacks as is.
fn cache_builds() -> Vec<PathBuf> {
    use aterm_uds::host::chrome;
    chrome::WINDOW_PINNED
        .iter()
        .map(|pin| chrome::window_pinned_binary(Path::new(""), pin.platform))
        .collect()
}

/// The Chromium names a Linux desk finds on `PATH`.
const LINUX_ON_PATH: &[&str] = &["chromium", "chromium-browser"];

/// Whether `path` is branded Google Chrome of any channel (the owner's own browser: Google
/// Chrome, its Beta, Dev and Canary, `google-chrome-*`, anything under `/opt/google`), never a
/// window browser. Google Chrome for Testing is not branded.
fn branded_chrome(path: &Path) -> bool {
    let branded = |name: &str| {
        (name.starts_with("Google Chrome") && !name.starts_with("Google Chrome for Testing"))
            || name.starts_with("google-chrome")
    };
    path.starts_with("/opt/google")
        || path
            .components()
            .any(|part| part.as_os_str().to_str().is_some_and(branded))
}

/// Whether `path` is a browser a window may open in: a file, not a headless shell and not
/// branded Google Chrome.
fn window_capable(path: &Path) -> bool {
    path.is_file() && !super::headless_oauth::is_shell_binary(path) && !branded_chrome(path)
}

/// The dotted version of a `chrome-<version>-<platform>` cache directory, for newest-first order.
fn cache_version(name: &str) -> Option<Vec<u64>> {
    let rest = name.strip_prefix("chrome-")?;
    if rest.starts_with("headless-shell") {
        return None;
    }
    let version = rest.split('-').next()?;
    version.split('.').map(|part| part.parse().ok()).collect()
}

/// [`find_window_browser`] over its inputs: `env` (`$ATERM_BROWSER`, `PATH`), the browser
/// cache's roots, the applications folders and the names looked up on `PATH`. In order:
/// `$ATERM_BROWSER` where it names a window browser ([`window_capable`]); the newest Chrome for
/// Testing unpacked under a cache root (in a versioned folder, else its zip's own folder there);
/// Chrome for Testing or Chromium in an applications folder;
/// one of `on_path` on `PATH`. Never a headless shell or branded Google Chrome.
#[must_use]
pub fn find_window_browser_in(
    env: &dyn Fn(&str) -> Option<OsString>,
    cache_roots: &[PathBuf],
    app_dirs: &[PathBuf],
    on_path: &[&str],
) -> Option<PathBuf> {
    if let Some(named) = env(super::headless_oauth::BROWSER_BIN_ENV).filter(|v| !v.is_empty()) {
        let named = PathBuf::from(named);
        if window_capable(&named) {
            return Some(named);
        }
    }
    let subs = cache_builds();
    for root in cache_roots {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        let unpacked = subs.iter().map(|sub| root.join(sub));
        let mut builds: Vec<(Vec<u64>, PathBuf)> = entries
            .filter_map(Result::ok)
            .filter_map(|e| {
                let version = cache_version(e.file_name().to_str()?)?;
                Some((version, e.path()))
            })
            .collect();
        builds.sort();
        let versioned = builds
            .iter()
            .rev()
            .flat_map(|(_, dir)| subs.iter().map(move |sub| dir.join(sub)));
        if let Some(found) = versioned.chain(unpacked).find(|c| window_capable(c)) {
            return Some(found);
        }
    }
    for dir in app_dirs {
        for app in APP_BUNDLES {
            let candidate = dir.join(app);
            if window_capable(&candidate) {
                return Some(candidate);
            }
        }
    }
    let path = env("PATH")?;
    std::env::split_paths(&path)
        .filter(|dir| dir.is_absolute())
        .flat_map(|dir| on_path.iter().map(move |name| dir.join(name)))
        .find(|candidate| window_capable(candidate))
}

/// The Chromium a row's profile opens in a window on this machine ([`find_window_browser_in`]):
/// on macOS from the browser cache and `/Applications` or `~/Applications`, on Linux from the
/// cache and `PATH`.
#[must_use]
pub fn find_window_browser(home: Option<&Path>) -> Option<PathBuf> {
    let roots = super::headless_oauth::browser_cache_roots(home);
    let mut apps = Vec::new();
    if cfg!(target_os = "macos") {
        apps.push(PathBuf::from("/Applications"));
        if let Some(home) = home {
            apps.push(home.join("Applications"));
        }
    }
    let on_path: &[&str] = if cfg!(target_os = "linux") {
        LINUX_ON_PATH
    } else {
        &[]
    };
    find_window_browser_in(&|name| std::env::var_os(name), &roots, &apps, on_path)
}

/// [`no_window_desk`] over its inputs: whether this is a Unix host, whether it is Linux, and
/// `env` (`DISPLAY`, `WAYLAND_DISPLAY`).
#[must_use]
pub fn no_window_desk_in(
    unix: bool,
    linux: bool,
    env: &dyn Fn(&str) -> Option<OsString>,
) -> Option<&'static str> {
    if !unix {
        return Some("not run: a sign-in window opens on macOS and Linux only");
    }
    let shown = |name: &str| env(name).is_some_and(|v| !v.is_empty());
    if linux && !shown("DISPLAY") && !shown("WAYLAND_DISPLAY") {
        return Some(
            "not run: this session has no display (no DISPLAY or WAYLAND_DISPLAY) to open the window on",
        );
    }
    None
}

/// Why no sign-in window can open on this machine's desk, which `accounts browser` refuses on
/// before anything else about the window and [`window_route`] reads first: a host that is neither
/// macOS nor Linux, or a Linux session with no display (`DISPLAY`, `WAYLAND_DISPLAY`).
#[must_use]
pub fn no_window_desk() -> Option<&'static str> {
    no_window_desk_in(cfg!(unix), cfg!(target_os = "linux"), &|name| {
        std::env::var_os(name)
    })
}

/// Whether `accounts browser` can open a window on this machine ([`window_route`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowRoute {
    /// A Chromium that opens a window is installed ([`find_window_browser`]).
    Found(PathBuf),
    /// None is, and `accounts browser` fetches the pinned Chrome for Testing first
    /// ([`fetch_window_browser`]): the pin covers this machine, a cache root is known, the
    /// downloader is there ([`super::headless_fetch::curl_present`]), and no fetch of it has
    /// failed there within [`super::headless_fetch::FAILURE_KEPT`]
    /// ([`super::headless_fetch::last_failure`]).
    Fetchable,
    /// No window opens here, or none is installed and none would be fetched: why (no desk to
    /// open one on, no pin for this machine, no cache root, no downloader, or the last fetch's
    /// failure).
    Unreachable(String),
}

impl WindowRoute {
    /// Whether `accounts browser` would open a window here: one found, or one it fetches.
    #[must_use]
    pub fn reachable(&self) -> bool {
        !matches!(self, WindowRoute::Unreachable(_))
    }
}

/// [`window_route`] over its inputs: why no window opens on this desk (`desk`,
/// [`no_window_desk`]), the window browser `found`, else this machine's pinned full build
/// `archive`, the cache `root` it would be fetched into, and whether its downloader is there
/// (`curl`).
#[must_use]
pub fn window_route_in(
    desk: Option<&str>,
    found: Option<PathBuf>,
    archive: Option<&super::headless_fetch::Archive>,
    root: Option<&Path>,
    curl: bool,
) -> WindowRoute {
    if let Some(why) = desk {
        return WindowRoute::Unreachable(why.to_owned());
    }
    if let Some(found) = found {
        return WindowRoute::Found(found);
    }
    let Some(archive) = archive else {
        return WindowRoute::Unreachable(
            "the pin covers no Chrome for Testing build for this machine".to_owned(),
        );
    };
    let Some(root) = root else {
        return WindowRoute::Unreachable("no cache directory is known".to_owned());
    };
    if !curl {
        return WindowRoute::Unreachable(format!(
            "{}, which fetches the pinned Chrome for Testing, is not here",
            super::headless_fetch::CURL
        ));
    }
    match super::headless_fetch::last_failure(root, archive) {
        Some(why) => WindowRoute::Unreachable(format!(
            "the pinned Chrome for Testing's last fetch failed: {why}"
        )),
        None => WindowRoute::Fetchable,
    }
}

/// Whether `accounts browser` can open a window on this machine: none where this desk opens no
/// window ([`no_window_desk`]), else one installed ([`find_window_browser`]), else the pinned full
/// Chrome for Testing it would fetch into the browser cache
/// ([`super::headless_fetch::cache_root`]) with [`super::headless_fetch::CURL`], where no fetch
/// of it has failed lately.
#[must_use]
pub fn window_route(home: Option<&Path>) -> WindowRoute {
    window_route_in(
        no_window_desk(),
        find_window_browser(home),
        super::headless_fetch::Archive::host_window().as_ref(),
        super::headless_fetch::cache_root(home).as_deref(),
        super::headless_fetch::curl_present(),
    )
}

/// What a fetch ([`fetch_window_browser`]) says when its caller was stopped as the build was
/// put in place: it stays there, where the next `accounts browser` finds it, and no window opens.
pub const STOPPED_IN_PLACE: &str =
    "it was stopped as the build was put in place, where the next run finds it";

/// The archive [`fetch_window_browser`] fetches on this machine and the cache root it fetches
/// it into: this machine's pinned full Chrome for Testing
/// ([`super::headless_fetch::Archive::host_window`]) and the browser cache root
/// ([`super::headless_fetch::cache_root`]).
/// # Errors
/// The pin covers no build for this machine, or no cache directory is known.
pub fn window_fetch_plan(
    home: Option<&Path>,
) -> Result<(super::headless_fetch::Archive, PathBuf), String> {
    let archive = super::headless_fetch::Archive::host_window().ok_or_else(|| {
        "the pin covers no Chrome for Testing build for this machine; nothing was installed"
            .to_owned()
    })?;
    let root = super::headless_fetch::cache_root(home)
        .ok_or_else(|| "no cache directory is known; nothing was installed".to_owned())?;
    Ok((archive, root))
}

/// Fetches this machine's pinned full Chrome for Testing into the browser cache root
/// ([`window_fetch_plan`]), where [`find_window_browser`] then finds it
/// ([`fetch_window_browser_into`]); its executable. `accounts browser` runs it where no window
/// browser is found.
/// # Errors
/// The pin covers no build for this machine, no cache directory is known, or
/// [`fetch_window_browser_into`]'s own.
pub fn fetch_window_browser(
    home: Option<&Path>,
    fetch: &mut dyn super::headless_fetch::Fetch,
    stopped: &dyn Fn() -> bool,
) -> Result<PathBuf, String> {
    let (archive, root) = window_fetch_plan(home)?;
    fetch_window_browser_into(&root, &archive, fetch, stopped)
}

/// Installs `archive` into the cache `root` through `fetch` ([`super::headless_fetch::install`]:
/// held to its size and sha256, nothing installed otherwise, a failure kept for
/// [`window_route`]), ending once `stopped` says so, and returns its executable where it is a
/// browser that opens a window ([`window_capable`]: never a headless shell).
/// # Errors
/// The install's own; the executable placed is no browser that opens a window; or `stopped`
/// said so once the build was in place ([`STOPPED_IN_PLACE`]: the build stays, nothing opens).
pub fn fetch_window_browser_into(
    root: &Path,
    archive: &super::headless_fetch::Archive,
    fetch: &mut dyn super::headless_fetch::Fetch,
    stopped: &dyn Fn() -> bool,
) -> Result<PathBuf, String> {
    let placed = super::headless_fetch::install(root, archive, fetch, stopped)?;
    if !window_capable(&placed) {
        return Err(format!(
            "{} is no browser that opens a window",
            placed.display()
        ));
    }
    // A stop noted while the tree was renamed into place, after the install's last look.
    if stopped() {
        return Err(STOPPED_IN_PLACE.to_owned());
    }
    Ok(placed)
}

/// The browser's name for a person: `Google Chrome for Testing`, `Chromium`, else its file name.
#[must_use]
pub fn browser_name(browser: &Path) -> String {
    let text = browser.to_string_lossy();
    if text.contains("Google Chrome for Testing")
        || text.ends_with("chrome-linux64/chrome")
        || text.ends_with("chrome-linux-arm64/chrome")
    {
        "Google Chrome for Testing".to_owned()
    } else if text.contains("Chromium") || text.contains("chromium") {
        "Chromium".to_owned()
    } else {
        browser
            .file_name()
            .map_or_else(|| text.into_owned(), |n| n.to_string_lossy().into_owned())
    }
}

/// The window's arguments: the profile's own flags ([`super::headless_oauth::profile_args`]),
/// then `url`. No headless flag, no DevTools pipe and no blink-feature switch: nothing marks
/// the window as automated.
#[must_use]
pub fn window_args(profile: &Path, url: &str) -> Vec<OsString> {
    let mut args = super::headless_oauth::profile_args(profile);
    args.push(url.into());
    args
}

/// This machine's host name, as Chromium writes it into a profile's lock.
#[cfg(unix)]
fn host_name() -> Option<String> {
    unsafe extern "C" {
        fn gethostname(name: *mut core::ffi::c_char, len: usize) -> i32;
    }
    let mut buf = [0u8; 256];
    // SAFETY: `buf` is `buf.len()` writable bytes that outlive the call, which writes at most
    // that many.
    let rc = unsafe { gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if rc != 0 {
        return None;
    }
    let len = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    Some(String::from_utf8_lossy(&buf[..len]).into_owned())
}

/// Whether process `pid` lives on this machine as this user's (`kill(pid, 0)` answers it): a
/// process of another user (`EPERM`, launchd's pid 1 among them) is none of the browsers this
/// user's profiles are opened in.
#[cfg(unix)]
fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 sends nothing; it only asks whether `pid` exists and may be signalled.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Whether the live process `pid` started after the `SingletonLock` `lock` was written (its
/// link's own modification time), so it reuses the pid of the browser that wrote it, which is
/// gone: the browser holding a lock started before writing it. `false` where either time is
/// not known (no start record on this system).
#[cfg(unix)]
fn started_after(pid: i32, lock: &Path) -> bool {
    let Some(birth) = aterm_uds::process::read_process_birth(pid) else {
        return false;
    };
    let Ok(written) = std::fs::symlink_metadata(lock).and_then(|m| m.modified()) else {
        return false;
    };
    let started = std::time::UNIX_EPOCH
        + Duration::from_secs(birth.seconds)
        + Duration::from_micros(birth.microseconds);
    started > written
}

/// Who holds a browser profile open ([`profile_holder`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Holder {
    /// A browser of this machine, by Chromium's `SingletonLock`: its pid.
    Browser(i32),
    /// A browser of another machine, by Chromium's `SingletonLock`, which may be stale there.
    Elsewhere {
        /// The pid the lock names.
        pid: i32,
        /// The machine it names.
        host: String,
    },
    /// Another of aterm's own launches of this machine (a headless sign-in, which writes no
    /// `SingletonLock`, or another window), by [`ProfileLock`]: its pid, where it wrote one.
    Aterm(Option<i32>),
}

impl Holder {
    /// For a person: who has the profile open and what to do, after "the row's browser profile
    /// is" (`open in another browser (pid 42); quit it and try again`).
    #[must_use]
    pub fn words(&self, profile: &Path) -> String {
        match self {
            Holder::Browser(pid) => {
                format!("open in another browser (pid {pid}); quit it and try again")
            }
            Holder::Elsewhere { pid, host } => format!(
                "locked by a browser of another machine (pid {pid} on {host}); if none has it \
                 open there, remove {} and try again",
                profile.join("SingletonLock").display()
            ),
            Holder::Aterm(Some(pid)) => format!(
                "in use by another of aterm's sign-ins (pid {pid}); wait for it to end and try \
                 again"
            ),
            Holder::Aterm(None) => {
                "in use by another of aterm's sign-ins; wait for it to end and try again".to_owned()
            }
        }
    }

    /// [`Holder::words`] without the pid or the lock's path, for a caller whose errors are
    /// static text.
    #[must_use]
    pub fn brief(&self) -> &'static str {
        match self {
            Holder::Browser(_) => {
                "the account's browser profile is open in another browser; quit it and try again"
            }
            Holder::Elsewhere { .. } => {
                "the account's browser profile is locked by a browser of another machine; if none \
                 has it open there, remove the profile's SingletonLock and try again"
            }
            Holder::Aterm(_) => {
                "the account's browser profile is in use by another of aterm's sign-ins; wait for \
                 it to end and try again"
            }
        }
    }
}

/// Who holds `profile` by Chromium's `SingletonLock` link (`<host>-<pid>`, read with
/// `read_link` only): a live process of this machine, or any of another machine. `None` with no
/// lock, an unreadable one, or one whose process on this machine is gone (a stale lock, which
/// Chromium itself clears): no longer this user's ([`alive`]), or a later process that reuses
/// its pid ([`started_after`], as after a crash or a reboot).
#[cfg(unix)]
fn singleton_holder(profile: &Path) -> Option<Holder> {
    let lock = profile.join("SingletonLock");
    let target = std::fs::read_link(&lock).ok()?;
    let target = target.to_string_lossy();
    let (host, pid) = target.rsplit_once('-')?;
    let pid: i32 = pid.parse().ok().filter(|pid| *pid > 0)?;
    if host_name().as_deref() == Some(host) {
        (alive(pid) && !started_after(pid, &lock)).then_some(Holder::Browser(pid))
    } else {
        Some(Holder::Elsewhere {
            pid,
            host: super::one_line(host, 64),
        })
    }
}

/// Who holds the browser profile `profile` open: a browser by Chromium's `SingletonLock`
/// ([`Holder::Browser`], [`Holder::Elsewhere`]), else another of aterm's launches by its
/// [`ProfileLock`] ([`Holder::Aterm`]). `None` where neither does.
#[cfg(unix)]
#[must_use]
pub fn profile_holder(profile: &Path) -> Option<Holder> {
    if let Some(holder) = singleton_holder(profile) {
        return Some(holder);
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(profile.join(PROFILE_LOCK_FILE))
        .ok()?;
    match ProfileLock::try_take(file) {
        Ok(Ok(probe)) => {
            drop(probe);
            None
        }
        Ok(Err(_)) => Some(Holder::Aterm(lock_pid(profile))),
        Err(_) => None,
    }
}

/// Non-Unix: no profile is read as held.
#[cfg(not(unix))]
#[must_use]
pub fn profile_holder(_profile: &Path) -> Option<Holder> {
    None
}

/// The file in a browser profile whose `flock(2)` one of aterm's launches holds while it has the
/// profile open ([`ProfileLock`]); it holds that launch's pid.
pub const PROFILE_LOCK_FILE: &str = "aterm-profile.lock";

/// The pid written into `profile`'s [`PROFILE_LOCK_FILE`].
fn lock_pid(profile: &Path) -> Option<i32> {
    std::fs::read_to_string(profile.join(PROFILE_LOCK_FILE))
        .ok()?
        .trim()
        .parse()
        .ok()
        .filter(|pid| *pid > 0)
}

/// Why a profile could not be claimed ([`ProfileLock::claim`]).
#[derive(Debug)]
pub enum Refusal {
    /// Another launch has it open.
    Held(Holder),
    /// Its lock file could not be made or locked.
    Unlockable(std::io::Error),
}

impl Refusal {
    /// For a person, after "the row's browser profile is" ([`Holder::words`]).
    #[must_use]
    pub fn words(&self, profile: &Path) -> String {
        match self {
            Refusal::Held(holder) => holder.words(profile),
            Refusal::Unlockable(e) => format!("not lockable ({e})"),
        }
    }

    /// [`Refusal::words`] as static text ([`Holder::brief`]).
    #[must_use]
    pub fn brief(&self) -> &'static str {
        match self {
            Refusal::Held(holder) => holder.brief(),
            Refusal::Unlockable(_) => "the account's browser profile's lock could not be taken",
        }
    }
}

/// One of aterm's launches' hold on a browser profile, so no other of aterm's launches opens it
/// at the same time: an exclusive `flock(2)` on its [`PROFILE_LOCK_FILE`], released on drop.
/// A headless shell writes no `SingletonLock`, so without it a window could open a profile a
/// headless sign-in is writing.
#[derive(Debug)]
pub struct ProfileLock(std::fs::File);

impl Drop for ProfileLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

impl ProfileLock {
    fn try_take(file: std::fs::File) -> std::io::Result<Result<ProfileLock, std::fs::File>> {
        #[expect(
            clippy::disallowed_methods,
            reason = "the profile lock's own take: its Drop releases by LOCK_UN"
        )]
        let taken = file.try_lock();
        match taken {
            Ok(()) => Ok(Ok(ProfileLock(file))),
            Err(std::fs::TryLockError::WouldBlock) => Ok(Err(file)),
            Err(std::fs::TryLockError::Error(e)) => Err(e),
        }
    }

    /// Claims the browser profile `profile` (an existing directory) for one launch: refused
    /// where a browser holds its `SingletonLock` or another of aterm's launches its
    /// [`PROFILE_LOCK_FILE`]; else that file is locked, and this process's pid written in it.
    /// Off unix no `SingletonLock` is read, as [`profile_holder`] reads none there, and the file
    /// keeps the private folder's permissions rather than mode 0600.
    /// # Errors
    /// Who holds it, or why its lock could not be taken.
    pub fn claim(profile: &Path) -> Result<ProfileLock, Refusal> {
        use std::io::Write as _;
        #[cfg(unix)]
        if let Some(holder) = singleton_holder(profile) {
            return Err(Refusal::Held(holder));
        }
        let mut options = std::fs::OpenOptions::new();
        options.create(true).truncate(false).read(true).write(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let file = options
            .open(profile.join(PROFILE_LOCK_FILE))
            .map_err(Refusal::Unlockable)?;
        match Self::try_take(file).map_err(Refusal::Unlockable)? {
            Ok(lock) => {
                let mut held = &lock.0;
                let _ = held.set_len(0);
                let _ = writeln!(held, "{}", std::process::id());
                Ok(lock)
            }
            Err(_) => Err(Refusal::Held(Holder::Aterm(lock_pid(profile)))),
        }
    }
}

/// Turns off the password manager's offers in the browser profile `profile`: its
/// `Default/Preferences` gets `credentials_enable_service` and
/// `profile.password_manager_enabled` false, its other settings kept (a file that is not a JSON
/// object is replaced). A password saved there would sit under the mock keychain's key, which
/// any program of the user could read.
/// # Errors
/// The file could not be written.
pub fn quiet_password_manager(profile: &Path) -> std::io::Result<()> {
    let dir = profile.join("Default");
    aterm_types::fs_restricted::ensure_private_dir(&dir)?;
    let path = dir.join("Preferences");
    let mut prefs = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| aterm_json::from_str::<Value>(&text).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    if let Some(top) = prefs.as_object_mut() {
        top.insert("credentials_enable_service".to_owned(), Value::Bool(false));
        let inner = top.entry("profile".to_owned()).or_insert_with(|| json!({}));
        if !inner.is_object() {
            *inner = json!({});
        }
        if let Some(inner) = inner.as_object_mut() {
            inner.insert("password_manager_enabled".to_owned(), Value::Bool(false));
        }
    }
    let text = aterm_json::to_string(&prefs).map_err(std::io::Error::other)?;
    let staged = dir.join("Preferences.aterm-new");
    std::fs::write(&staged, text)?;
    std::fs::rename(&staged, &path)
}

/// The sites of identity providers whose cookies are dropped under any top-level domain
/// (`google.com`, `google.co.uk`, `accounts.youtube.com`): a Google sign-in sets its session on
/// both.
const IDP_SITES: &[&str] = &["google", "youtube"];

/// The registrable domains of the other identity providers ([`super::headless_oauth::OTHER_IDPS`]
/// and their account sites), whose cookies are dropped with their subdomains'.
const IDP_DOMAINS: &[&str] = &[
    "apple.com",
    "icloud.com",
    "x.com",
    "twitter.com",
    "microsoftonline.com",
    "live.com",
    "microsoft.com",
    "github.com",
    "facebook.com",
];

/// Whether a cookie of `domain` (as a cookie store names it, `.google.com`) is an identity
/// provider's ([`IDP_SITES`], [`IDP_DOMAINS`]), which a profile never keeps past its window.
#[must_use]
pub fn idp_cookie(domain: &str) -> bool {
    let host = domain.trim().trim_start_matches('.').to_ascii_lowercase();
    if IDP_DOMAINS
        .iter()
        .any(|d| host == *d || host.strip_suffix(d).is_some_and(|sub| sub.ends_with('.')))
    {
        return true;
    }
    let labels: Vec<&str> = host.split('.').collect();
    if labels.iter().any(|l| {
        l.is_empty()
            || !l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || l.starts_with('-')
            || l.ends_with('-')
    }) {
        return false;
    }
    labels.iter().enumerate().any(|(at, label)| {
        if !IDP_SITES.contains(label) {
            return false;
        }
        match &labels[at + 1..] {
            [tld] => (2..=3).contains(&tld.len()) && tld.bytes().all(|b| b.is_ascii_alphabetic()),
            [sld, cctld] => {
                matches!(
                    *sld,
                    "co" | "com" | "org" | "net" | "ac" | "gov" | "edu" | "ne" | "or"
                ) && cctld.len() == 2
                    && cctld.bytes().all(|b| b.is_ascii_alphabetic())
            }
            _ => false,
        }
    })
}

/// What [`tidy_profile`] did to a profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tidied {
    /// The identity providers' cookies dropped.
    pub dropped: usize,
    /// Whether the vendor's web app keeps its session there (its key in its origin's
    /// `localStorage`); `None` where no key was asked after.
    pub session: Option<bool>,
}

/// The page served, in place of the vendor's own, where the session's key is asked after: no
/// request leaves the browser.
const BLANK_PAGE: &str = "<!doctype html><title>aterm</title>";

/// One CDP call within `deadline`, its error naming the method ([`tidy_over_cdp`]).
fn call_by(
    cdp: &mut super::headless_oauth::CdpClient,
    session: Option<&str>,
    method: &str,
    params: Value,
    deadline: Instant,
) -> Result<Value, String> {
    cdp.call(
        session,
        method,
        params,
        deadline.saturating_duration_since(Instant::now()),
    )
    .map_err(|why| format!("{method}: {why}"))
}

/// The keys (name, domain, path and partition) of the identity providers' cookies
/// ([`idp_cookie`]) in `cdp`'s store, never their values.
fn idp_cookie_keys(
    cdp: &mut super::headless_oauth::CdpClient,
    deadline: Instant,
) -> Result<Vec<Value>, String> {
    call_by(cdp, None, "Storage.getCookies", json!({}), deadline).map(|got| {
        got.get("cookies")
            .and_then(Value::as_array)
            .map(|all| {
                all.iter()
                    .filter(|c| {
                        c.get("domain")
                            .and_then(Value::as_str)
                            .is_some_and(idp_cookie)
                    })
                    .map(|c| {
                        let mut key = json!({
                            "name": c.get("name").cloned().unwrap_or(Value::Null),
                            "domain": c.get("domain").cloned().unwrap_or(Value::Null),
                            "path": c.get("path").cloned().unwrap_or(Value::Null),
                        });
                        if let (Some(part), Some(map)) =
                            (c.get("partitionKey"), key.as_object_mut())
                        {
                            map.insert("partitionKey".to_owned(), part.clone());
                        }
                        key
                    })
                    .collect::<Vec<Value>>()
            })
            .unwrap_or_default()
    })
}

/// A blank page target in `cdp`'s browser, attached: its target id and page session.
fn blank_target(
    cdp: &mut super::headless_oauth::CdpClient,
    deadline: Instant,
) -> Result<(String, String), String> {
    let target = call_by(
        cdp,
        None,
        "Target.createTarget",
        json!({"url": "about:blank"}),
        deadline,
    )?;
    let target_id = target
        .get("targetId")
        .and_then(Value::as_str)
        .ok_or("Target.createTarget: no targetId")?
        .to_owned();
    let attached = call_by(
        cdp,
        None,
        "Target.attachToTarget",
        json!({"targetId": target_id, "flatten": true}),
        deadline,
    )?;
    let page = attached
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or("Target.attachToTarget: no sessionId")?
        .to_owned();
    Ok((target_id, page))
}

/// Every identity provider's cookie deleted through the page session `page` by its key
/// ([`idp_cookie_keys`]), and the store read again to show none is left: how many there were.
fn drop_idp_cookies(
    cdp: &mut super::headless_oauth::CdpClient,
    page: &str,
    deadline: Instant,
) -> Result<usize, String> {
    let doomed = idp_cookie_keys(cdp, deadline)?;
    for key in &doomed {
        call_by(
            cdp,
            Some(page),
            "Network.deleteCookies",
            key.clone(),
            deadline,
        )?;
    }
    let left_over = idp_cookie_keys(cdp, deadline)?.len();
    if left_over > 0 {
        return Err(format!(
            "{left_over} identity-provider cookie(s) stayed in the profile after their delete"
        ));
    }
    Ok(doomed.len())
}

/// [`tidy_profile`] over `cdp` until `deadline`: every identity provider's cookie
/// ([`idp_cookie`]) deleted by its name, domain and path (and partition) and the store read again
/// to show none is left; then, where `session` names the vendor's origin and key, a blank page
/// served at that origin (every request intercepted and answered here, the service worker
/// bypassed) asked whether its `localStorage` holds the key, never its value. The values the
/// browser hands over are never kept or printed.
/// # Errors
/// What failed, without a cookie's or a stored value.
pub fn tidy_over_cdp(
    cdp: &mut super::headless_oauth::CdpClient,
    session: Option<(&str, &str)>,
    deadline: Instant,
) -> Result<Tidied, String> {
    let (target_id, page) = blank_target(cdp, deadline)?;
    let dropped = drop_idp_cookies(cdp, &page, deadline)?;
    let session = match session {
        None => None,
        Some((origin, key)) => Some(session_kept(cdp, &page, origin, key, deadline)?),
    };
    let _ = cdp.call(
        None,
        "Target.closeTarget",
        json!({"targetId": target_id}),
        Duration::from_secs(2),
    );
    let _ = cdp.call(None, "Browser.close", json!({}), Duration::from_secs(2));
    Ok(Tidied { dropped, session })
}

/// The file a browser profile holds from before a window opens on it until the pass after the
/// window has dropped its identity providers' cookies ([`mark_untidied`], [`clear_untidied`]): a
/// window whose pass never ran (its `accounts browser` killed, crashed or cut off) leaves it,
/// and the next headless launch on the profile makes that pass up first ([`make_up_tidy`]).
pub const UNTIDIED_MARK: &str = "aterm-untidied";

/// How long a launch's [`make_up_tidy`] may take before the launch goes on.
pub const MAKE_UP_WITHIN: Duration = Duration::from_secs(30);

/// Marks `profile` as holding a window whose pass after it has not run yet ([`UNTIDIED_MARK`]).
/// # Errors
/// The mark could not be written.
pub fn mark_untidied(profile: &Path) -> std::io::Result<()> {
    std::fs::write(profile.join(UNTIDIED_MARK), "")
}

/// Removes `profile`'s [`UNTIDIED_MARK`] once its identity providers' cookies are gone.
/// # Errors
/// The mark is there and could not be removed.
pub fn clear_untidied(profile: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(profile.join(UNTIDIED_MARK)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// A headless launch's make-up, over its `cdp` before it does anything else, of a window's pass
/// that never ran on `profile` ([`UNTIDIED_MARK`]): every identity provider's cookie dropped in
/// a blank page of its own ([`drop_idp_cookies`], the browser left open), and the mark removed
/// once none is left. `None` where the profile is not marked; else how many were dropped, or
/// why it failed (the mark kept, for the next launch to try again).
pub fn make_up_tidy(
    cdp: &mut super::headless_oauth::CdpClient,
    profile: &Path,
    deadline: Instant,
) -> Option<Result<usize, String>> {
    if !profile.join(UNTIDIED_MARK).exists() {
        return None;
    }
    let made = blank_target(cdp, deadline).and_then(|(target_id, page)| {
        let dropped = drop_idp_cookies(cdp, &page, deadline);
        let _ = cdp.call(
            None,
            "Target.closeTarget",
            json!({"targetId": target_id}),
            Duration::from_secs(2),
        );
        dropped
    });
    if made.is_ok() {
        let _ = clear_untidied(profile);
    }
    Some(made)
}

/// Whether `origin`'s `localStorage` in the page session `page` holds `key` ([`tidy_over_cdp`]):
/// the page sent to `origin` with every request paused and answered here (its own page by
/// [`BLANK_PAGE`], anything else failed), then asked once it is at `origin`.
fn session_kept(
    cdp: &mut super::headless_oauth::CdpClient,
    page: &str,
    origin: &str,
    key: &str,
    deadline: Instant,
) -> Result<bool, String> {
    let left = || deadline.saturating_duration_since(Instant::now());
    let call = |cdp: &mut super::headless_oauth::CdpClient, method: &str, params: Value| {
        cdp.call(Some(page), method, params, left())
            .map_err(|why| format!("{method}: {why}"))
    };
    call(cdp, "Network.enable", json!({}))?;
    call(
        cdp,
        "Network.setBypassServiceWorker",
        json!({"bypass": true}),
    )?;
    call(
        cdp,
        "Fetch.enable",
        json!({"patterns": [{"urlPattern": "*", "requestStage": "Request"}]}),
    )?;
    let start = format!("{origin}/");
    let origin_json = aterm_json::to_string(origin).map_err(|e| e.to_string())?;
    let key_json = aterm_json::to_string(key).map_err(|e| e.to_string())?;
    let start_json = aterm_json::to_string(&start).map_err(|e| e.to_string())?;
    call(
        cdp,
        "Runtime.evaluate",
        json!({"expression": format!("location.href = {start_json}; true")}),
    )?;
    let body = aterm_codec::base64::encode(BLANK_PAGE.as_bytes()).map_err(|e| e.to_string())?;
    let ask = format!(
        "(() => {{ if (location.origin !== {origin_json}) return null; \
         try {{ return localStorage.getItem({key_json}) !== null; }} catch (e) {{ return null; }} \
         }})()"
    );
    let mut served = false;
    while Instant::now() < deadline {
        for paused in cdp.take_events("Fetch.requestPaused", Some(page)) {
            let Some(id) = paused.pointer("/params/requestId").and_then(Value::as_str) else {
                continue;
            };
            let url = paused
                .pointer("/params/request/url")
                .and_then(Value::as_str)
                .unwrap_or("");
            let id = id.to_owned();
            if !served && url == start {
                served = true;
                call(
                    cdp,
                    "Fetch.fulfillRequest",
                    json!({
                        "requestId": id,
                        "responseCode": 200,
                        "responseHeaders": [{"name": "Content-Type", "value": "text/html"}],
                        "body": body,
                    }),
                )?;
            } else {
                call(
                    cdp,
                    "Fetch.failRequest",
                    json!({"requestId": id, "errorReason": "BlockedByClient"}),
                )?;
            }
        }
        if served {
            let got = call(
                cdp,
                "Runtime.evaluate",
                json!({"expression": ask, "returnByValue": true}),
            );
            if let Ok(got) = got
                && let Some(kept) = got.pointer("/result/value").and_then(Value::as_bool)
            {
                return Ok(kept);
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(format!(
        "the session's page at {origin} did not load within its time"
    ))
}

/// Opens `profile` headlessly in `browser` over its DevTools pipe and tidies it within `within`
/// ([`tidy_over_cdp`]), then closes it and gives it its grace
/// ([`super::headless_oauth::BrowserChild`]) to write the profile.
/// # Errors
/// The browser did not start, or [`tidy_over_cdp`]'s.
#[cfg(unix)]
pub fn tidy_profile(
    browser: &super::headless_oauth::HeadlessBrowser,
    profile: &Path,
    session: Option<(&str, &str)>,
    within: Duration,
) -> Result<Tidied, String> {
    use aterm_uds::spawnfd::{ChildStdio, LaneCommand, spawn_with_two_fds};
    use std::os::fd::OwnedFd;
    let (cmd_read, cmd_write) =
        std::io::pipe().map_err(|e| format!("no DevTools pipe could be made ({e})"))?;
    let (out_read, out_write) =
        std::io::pipe().map_err(|e| format!("no DevTools pipe could be made ({e})"))?;
    let mut cmd = LaneCommand::new(&browser.path);
    cmd.args(super::headless_oauth::headless_launch_args(
        browser, profile,
    ))
    .stdin(ChildStdio::Null)
    .stdout(ChildStdio::Null)
    .stderr(ChildStdio::Null);
    let child = spawn_with_two_fds(cmd, OwnedFd::from(cmd_read), OwnedFd::from(out_write))
        .map_err(|e| format!("{} did not start ({e})", browser_name(&browser.path)))?;
    let _guard = super::headless_oauth::BrowserChild::new(child);
    let mut cdp = super::headless_oauth::CdpClient::new(cmd_write, out_read);
    tidy_over_cdp(&mut cdp, session, Instant::now() + within)
}

/// Non-Unix: nothing is tidied.
/// # Errors
/// Always.
#[cfg(not(unix))]
pub fn tidy_profile(
    _browser: &super::headless_oauth::HeadlessBrowser,
    _profile: &Path,
    _session: Option<(&str, &str)>,
    _within: Duration,
) -> Result<Tidied, String> {
    Err("a profile is tidied on macOS and Linux only".to_owned())
}

/// The origin (`https://host`) of `url`.
#[must_use]
pub fn origin_of(url: &str) -> Option<String> {
    let host = super::login_drive::https_host(url)?;
    Some(format!("https://{host}"))
}

/// How a window ended ([`run_window`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowEnd {
    /// The person said they were done (Enter, or the end of stdin), and the browser exited on
    /// its hangup.
    Done,
    /// The person quit the browser (it exited 0 by itself).
    Quit,
    /// The person pressed Ctrl-C, and the browser exited on its hangup.
    Interrupted,
    /// The window's time limit ran out, and the browser exited on its hangup.
    TimeLimit,
    /// The browser exited by itself otherwise: its code, or 128 plus the signal that ended it.
    Exited(i32),
    /// The browser was still running when its grace after the hangup ran out, so its process
    /// group was killed.
    Killed,
}

impl WindowEnd {
    /// Its ledger word.
    #[must_use]
    pub fn word(self) -> String {
        match self {
            WindowEnd::Done => "done".to_owned(),
            WindowEnd::Quit => "quit".to_owned(),
            WindowEnd::Interrupted => "interrupted".to_owned(),
            WindowEnd::TimeLimit => "time-limit".to_owned(),
            WindowEnd::Exited(code) => format!("exited-{code}"),
            WindowEnd::Killed => "killed".to_owned(),
        }
    }
}

#[cfg(unix)]
fn exit_end(status: std::process::ExitStatus) -> WindowEnd {
    use std::os::unix::process::ExitStatusExt as _;
    match (status.code(), status.signal()) {
        (Some(0), _) => WindowEnd::Quit,
        (Some(code), _) => WindowEnd::Exited(code),
        (None, Some(signal)) => WindowEnd::Exited(128 + signal),
        (None, None) => WindowEnd::Exited(-1),
    }
}

/// Opens `profile` in `browser` at `url` in a window of its own process group (so this
/// terminal's Ctrl-C never reaches it), nothing on its stdio, and waits until it exits by
/// itself, `ended` names an end ([`WindowEnd::Done`], [`WindowEnd::Interrupted`]) or `hang`
/// runs out. To end it, it is sent a hangup (never a terminate: a hangup is a clean close that
/// keeps its cookies), given `grace` to exit, and only then its process group is killed.
/// # Errors
/// The browser did not start, or could not be waited for (and its group was killed).
#[cfg(unix)]
pub fn run_window(
    browser: &Path,
    profile: &Path,
    url: &str,
    hang: Duration,
    grace: Duration,
    ended: &dyn Fn() -> Option<WindowEnd>,
) -> Result<WindowEnd, String> {
    run_window_with(browser, &[], profile, url, hang, grace, ended)
}

/// [`run_window`] with `lead` before the window's own arguments (a test binary's `--exact` run of
/// its stand-in).
/// # Errors
/// As [`run_window`]'s.
#[cfg(unix)]
pub(crate) fn run_window_with(
    browser: &Path,
    lead: &[OsString],
    profile: &Path,
    url: &str,
    hang: Duration,
    grace: Duration,
    ended: &dyn Fn() -> Option<WindowEnd>,
) -> Result<WindowEnd, String> {
    use aterm_uds::spawnfd::Stdio;
    let mut cmd = aterm_uds::spawnfd::command(browser);
    cmd.args(lead)
        .args(window_args(profile, url))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("{} did not start: {e}", browser_name(browser)))?;
    let pid = i32::try_from(child.id()).unwrap_or(0);
    let kill_group = |child: &mut aterm_uds::spawnfd::Child| {
        // The group this window made for its own child, which is not yet reaped, so the
        // group's id names no other process.
        let _ = aterm_uds::process::kill_group_until_empty(pid);
        let _ = child.kill();
        let _ = child.wait();
    };
    let deadline = std::time::Instant::now() + hang;
    let asked = loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(exit_end(status)),
            Ok(None) => {}
            Err(e) => {
                kill_group(&mut child);
                return Err(format!("the browser could not be waited for: {e}"));
            }
        }
        if let Some(end) = ended() {
            break end;
        }
        if std::time::Instant::now() >= deadline {
            break WindowEnd::TimeLimit;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    if pid > 0 {
        // SAFETY: a hangup to this window's own child, not yet reaped.
        unsafe { libc::kill(pid, libc::SIGHUP) };
    }
    let until = std::time::Instant::now() + grace;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return Ok(asked),
            Ok(None) if std::time::Instant::now() < until => {
                std::thread::sleep(Duration::from_millis(25))
            }
            _ => break,
        }
    }
    kill_group(&mut child);
    Ok(WindowEnd::Killed)
}

/// Non-Unix: no window opens.
/// # Errors
/// Always.
#[cfg(not(unix))]
pub fn run_window(
    _browser: &Path,
    _profile: &Path,
    _url: &str,
    _hang: Duration,
    _grace: Duration,
    _ended: &dyn Fn() -> Option<WindowEnd>,
) -> Result<WindowEnd, String> {
    Err("a sign-in window opens on macOS and Linux only".to_owned())
}

#[cfg(test)]
#[path = "browser_window_tests.rs"]
mod tests;
