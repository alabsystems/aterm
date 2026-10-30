// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE HARNESS FETCHES ITS OWN BROWSERS. Two pinned builds of Chrome for Testing, one source
//! for both ([`aterm_uds::host::chrome`]), each fetched into the cache root the finders read
//! ([`cache_root`]) only where none is found:
//! - the chrome-headless-shell ([`Archive::host`]), where no headless browser is found
//!   ([`super::headless_oauth::find_headless_browser`]) and a headless sign-in waits on one alone
//!   (the Grok gate's automatic rung, `super::grok_signin::LiveAuto`, where the vendor's page
//!   answers that browser, which accounts.x.ai's does not): the browser lane's own pin;
//! - the full Chrome for Testing that opens a window ([`Archive::host_window`]), where no such
//!   Chromium is found ([`super::browser_window::find_window_browser`]) and `aterm harness
//!   accounts browser` would open one ([`super::browser_window::fetch_window_browser`]).
//!
//! Each is downloaded by [`CURL`] by its absolute path over https only, held to the pin's size
//! and then its sha256, unpacked by atpkg's vetted extractor (each member beneath the tree,
//! links only within it), and moved into place with one rename once its executable is there.
//!
//! Fail-closed: no network, a partial download, a size or sha256 other than the pin's, an
//! archive that does not unpack or holds no executable installs nothing and says why; the
//! scratch it fetched into goes with it, and the cache root keeps one line saying why for that
//! archive, read for [`FAILURE_KEPT`] or until a later install of it succeeds ([`last_failure`]).
//! A stop (the caller's Ctrl-C,
//! a hangup) ends the download within one poll, curl's process group killed, and installs
//! nothing either ([`run_stoppable`]); a stop is no failure and is not kept. Nothing here runs
//! the browser.
//!
//! Gatekeeper: neither curl nor the extractor sets `com.apple.quarantine`, and nothing here
//! removes one, so what is placed carries no quarantine flag to strip, the headless shell and the
//! full build alike. Chrome for Testing's macOS builds are not Developer ID signed: each Mach-O
//! is ad hoc and linker-signed, and the bundle seals no resources, so `codesign --verify` of the
//! app reports `code has no resources but signature indicates they must be present` on Google's
//! own bytes as `/usr/bin/unzip` lays them too (measured on the 155.0.8059.84 mac-arm64 build,
//! 2026-10-25). No signature check stands in for the pin: the archive's sha256 is what admits it.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// The downloader, by its absolute path (`DESIGN-init-system` S12).
pub const CURL: &str = "/usr/bin/curl";

/// The longest one fetch of the headless shell may take (`curl --max-time`): the archive is
/// about 100 MB.
pub const FETCH_WITHIN: Duration = Duration::from_secs(5 * 60);

/// The longest one fetch of the full build may take (`curl --max-time`): the archive is about
/// 200 MB.
pub const WINDOW_FETCH_WITHIN: Duration = Duration::from_secs(10 * 60);

/// The most bytes an unpacked archive may hold: the pinned shell unpacks to about 250 MB and the
/// full build to at most 418 MB.
pub const UNPACKED_MAX: u64 = 1 << 30;

/// The prefix of a fetch's scratch directory in the cache root (never `chrome-`, which the
/// finders read; it covers the scratch an earlier build named `.fetch-chrome-headless-shell-`).
const SCRATCH_PREFIX: &str = ".fetch-";

/// The prefix of an archive's failure record in the cache root ([`last_failure`]): never
/// [`SCRATCH_PREFIX`], whose stale entries are removed.
const FAILED_PREFIX: &str = ".failed-";

/// The most bytes of a reason a failure record keeps.
const FAILED_WHY_MAX: usize = 240;

/// How long a failure record is read ([`last_failure`]): a failed fetch says what the network or
/// the cache was like then, and an older one no longer keeps `accounts browser` out of the steps
/// Grok Build's sign-in gate names (its setup, `super::grok_signin::Setup`, and the way out of
/// an automatic refusal), so one dropped connection does not keep it out for good. A later
/// failure writes it anew.
pub const FAILURE_KEPT: Duration = Duration::from_secs(60 * 60);

/// A scratch left by a fetch that was killed is removed once it is this old.
const STALE_SCRATCH: Duration = Duration::from_secs(60 * 60);

/// How often a running download looks for its end and for a stop ([`run_stoppable`]).
const POLL: Duration = Duration::from_millis(100);

/// What a download ended by a stop says ([`run_stoppable`]).
pub const STOPPED: &str = "it was stopped";

/// The suffix of the harness's own unpacked directory beside the browser lane's
/// (`verify provision` places the same pin without it, with a stamp of its own).
pub const DIR_SUFFIX: &str = "-harness";

/// The variables a fetch passes from this process: its network's proxy and certificates.
const PASSED: &[&str] = &[
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "HTTPS_PROXY",
    "https_proxy",
    "ALL_PROXY",
    "all_proxy",
    "NO_PROXY",
    "no_proxy",
];

/// Downloads one archive: [`Curl`], or a test's stand-in.
pub trait Fetch {
    /// Downloads `url` into the new file `dest`, at most `bytes` long and within `within`,
    /// ending at once (within a poll) once `stopped` says the caller was asked to stop.
    /// # Errors
    /// Why it did not finish (no network, a refusal, the time ran out, [`STOPPED`]).
    fn fetch(
        &mut self,
        url: &str,
        dest: &Path,
        bytes: u64,
        within: Duration,
        stopped: &dyn Fn() -> bool,
    ) -> Result<(), String>;
}

/// [`CURL`]: https only (redirects too), TLS 1.2 at least, no retry, at most the archive's size
/// and time ([`Archive::within`]), stoppable ([`run_stoppable`]).
#[derive(Debug, Clone, Copy, Default)]
pub struct Curl;

/// Runs `cmd` with no standard streams in a process group of its own until it exits, `within`
/// passes, or `stopped` says stop, looking every [`POLL`]; at either of the last two its whole
/// group is killed and reaped. A caller that holds the window-end signals (`aterm harness
/// signin`) is never reached by the terminal's own SIGINT through it, so the stop it notes is
/// how its Ctrl-C reaches the child.
/// # Errors
/// It will not run or cannot be waited on, it ran past `within`, or [`STOPPED`].
pub fn run_stoppable(
    mut cmd: aterm_uds::spawnfd::Command,
    within: Duration,
    stopped: &dyn Fn() -> bool,
) -> Result<Option<i32>, String> {
    use aterm_uds::spawnfd::Stdio;
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    cmd.process_group(0);
    if stopped() {
        return Err(STOPPED.to_owned());
    }
    let mut child = cmd.spawn().map_err(|e| format!("it will not run: {e}"))?;
    let now = Instant::now();
    let until = now
        .checked_add(within)
        .unwrap_or_else(|| now + Duration::from_secs(3600));
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status.code()),
            Ok(None) => {}
            Err(e) => {
                super::login_drive::end(&mut child);
                return Err(format!("it could not be waited on: {e}"));
            }
        }
        if stopped() {
            super::login_drive::end(&mut child);
            return Err(STOPPED.to_owned());
        }
        if Instant::now() >= until {
            super::login_drive::end(&mut child);
            return Err(format!("it did not finish within {} s", within.as_secs()));
        }
        std::thread::sleep(POLL);
    }
}

/// What a curl exit means, in words, for a fetch given `within`.
fn curl_words(code: Option<i32>, within: Duration) -> String {
    match code {
        Some(1) => "curl refused the URL's protocol (https only)".to_owned(),
        Some(6) => "the host could not be resolved (no network?)".to_owned(),
        Some(7) => "the host could not be reached (no network?)".to_owned(),
        Some(22) => "the server refused the download".to_owned(),
        Some(28) => format!("it did not finish within {} min", within.as_secs() / 60),
        Some(35 | 60) => "the TLS connection was refused".to_owned(),
        Some(63) => "it was larger than the pin".to_owned(),
        Some(code) => format!("curl exited {code}"),
        None => "curl was ended by a signal".to_owned(),
    }
}

impl Fetch for Curl {
    fn fetch(
        &mut self,
        url: &str,
        dest: &Path,
        bytes: u64,
        within: Duration,
        stopped: &dyn Fn() -> bool,
    ) -> Result<(), String> {
        let mut cmd = aterm_uds::spawnfd::command(CURL);
        cmd.args([
            "-q",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--tlsv1.2",
            "-sSfL",
            "--retry",
            "0",
            "--connect-timeout",
            "30",
        ])
        .arg("--max-filesize")
        .arg(bytes.to_string())
        .arg("--max-time")
        .arg(within.as_secs().to_string())
        .arg("-o")
        .arg(dest)
        .arg("--")
        .arg(url)
        .env_clear();
        for name in ["PATH", "HOME", "TMPDIR"].iter().chain(PASSED) {
            if let Some(value) = std::env::var_os(name).filter(|v| !v.is_empty()) {
                cmd.env(name, value);
            }
        }
        match run_stoppable(cmd, within + Duration::from_secs(30), stopped)? {
            Some(0) => Ok(()),
            code => Err(curl_words(code, within)),
        }
    }
}

/// One archive to install: where it is served, what it must be, and what it unpacks to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Archive {
    /// Where it is served (https).
    pub url: String,
    /// Its sha256, lowercase hex.
    pub sha256: String,
    /// Its size in bytes.
    pub bytes: u64,
    /// The directory it is placed as in the cache root.
    pub dir: String,
    /// Its executable, relative to that directory.
    pub binary: PathBuf,
    /// The longest its download may take ([`Fetch::fetch`]).
    pub within: Duration,
}

impl Archive {
    /// The pinned chrome-headless-shell of `(os, arch)` ([`aterm_uds::host::chrome`]), where the
    /// pin covers it; placed as the lane's directory name with [`DIR_SUFFIX`], which the finder
    /// reads as it reads the lane's.
    #[must_use]
    pub fn pinned(os: &str, arch: &str) -> Option<Archive> {
        use aterm_uds::host::chrome;
        let pin = chrome::pin_for(os, arch)?;
        let lane = chrome::pinned_dir(Path::new(""), pin.platform);
        Some(Archive {
            url: chrome::archive_url(pin.platform),
            sha256: pin.sha256.to_owned(),
            bytes: pin.bytes,
            dir: format!("{}{DIR_SUFFIX}", lane.display()),
            binary: chrome::pinned_binary(Path::new(""), pin.platform),
            within: FETCH_WITHIN,
        })
    }

    /// This host's pinned chrome-headless-shell ([`Archive::pinned`]).
    #[must_use]
    pub fn host() -> Option<Archive> {
        Archive::pinned(std::env::consts::OS, std::env::consts::ARCH)
    }

    /// The pinned full Chrome for Testing of `(os, arch)`, the build that opens a window
    /// ([`aterm_uds::host::chrome::WINDOW_PINNED`]), where the pin covers it; placed as
    /// `chrome-<version>-<platform>` with [`DIR_SUFFIX`], a versioned folder the window
    /// browser's finder reads, beside any a person unzipped there and never in its place.
    #[must_use]
    pub fn window(os: &str, arch: &str) -> Option<Archive> {
        use aterm_uds::host::chrome;
        let pin = chrome::window_pin_for(os, arch)?;
        let dir = chrome::window_pinned_dir(Path::new(""), pin.platform);
        Some(Archive {
            url: chrome::window_archive_url(pin.platform),
            sha256: pin.sha256.to_owned(),
            bytes: pin.bytes,
            dir: format!("{}{DIR_SUFFIX}", dir.display()),
            binary: chrome::window_pinned_binary(Path::new(""), pin.platform),
            within: WINDOW_FETCH_WITHIN,
        })
    }

    /// This host's pinned full Chrome for Testing ([`Archive::window`]).
    #[must_use]
    pub fn host_window() -> Option<Archive> {
        Archive::window(std::env::consts::OS, std::env::consts::ARCH)
    }
}

/// The browser cache root a fetch installs into: the first of the roots the finder reads
/// ([`super::headless_oauth::browser_cache_roots`]) that Spotlight leaves alone (`*.noindex`).
#[must_use]
pub fn cache_root(home: Option<&Path>) -> Option<PathBuf> {
    super::headless_oauth::browser_cache_roots(home)
        .into_iter()
        .find(|root| root.extension().is_some_and(|e| e == "noindex"))
}

/// The lowercase sha256 of the file at `path`.
fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file =
        std::fs::File::open(path).map_err(|e| format!("the download cannot be read: {e}"))?;
    let mut hash = aterm_digest::Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| format!("the download cannot be read: {e}"))?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(hash.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Whether [`CURL`] is here to fetch with: a file (or a link to one) with an execute bit. A
/// minimal Linux image may have none.
#[must_use]
pub fn curl_present() -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::metadata(CURL).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// Whether `path` is a regular file with an execute bit.
fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::symlink_metadata(path)
            .is_ok_and(|m| m.file_type().is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// Removes the scratch directories in `root` that fetches killed outright left behind.
fn remove_stale_scratch(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry
            .file_name()
            .to_string_lossy()
            .starts_with(SCRATCH_PREFIX)
        {
            continue;
        }
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .is_some_and(|age| age >= STALE_SCRATCH);
        if old {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Where `root` keeps why the last install of `archive` failed ([`last_failure`]).
fn failure_path(root: &Path, archive: &Archive) -> PathBuf {
    root.join(format!("{FAILED_PREFIX}{}", archive.dir))
}

/// Why the last install of `archive` into the cache `root` failed, where it did within
/// [`FAILURE_KEPT`] (by its record's modification time) and no later install of it succeeded
/// ([`install`]): one line, never a URL's query or a credential (it is the install's own error).
/// A stop is not kept.
#[must_use]
pub fn last_failure(root: &Path, archive: &Archive) -> Option<String> {
    last_failure_at(root, archive, SystemTime::now())
}

/// [`last_failure`] as of `now`: a record written more than [`FAILURE_KEPT`] before it is not
/// read (nor one whose time cannot be read).
fn last_failure_at(root: &Path, archive: &Archive, now: SystemTime) -> Option<String> {
    let record = failure_path(root, archive);
    let written = std::fs::symlink_metadata(&record)
        .ok()
        .filter(std::fs::Metadata::is_file)
        .and_then(|m| m.modified().ok())?;
    if now.duration_since(written).unwrap_or(Duration::ZERO) > FAILURE_KEPT {
        return None;
    }
    let text = super::accounts::read_bounded(&record, 16 * 1024)
        .ok()
        .flatten()?;
    let why = super::one_line(text.trim(), FAILED_WHY_MAX);
    Some(if why.is_empty() {
        "an earlier fetch failed".to_owned()
    } else {
        why
    })
}

/// Installs `archive` into the cache `root` through `fetch` and returns its executable: at once
/// where it is already in place; else downloaded into a fresh scratch in `root`, held to its
/// size and sha256, unpacked there, and renamed into place once its executable is in the tree,
/// never over a tree another fetch placed meanwhile (that one is returned). `stopped` is asked
/// before each step and through the download ([`Fetch::fetch`]). Once it is in place, an earlier
/// failure's record is removed; a failure the caller's stop did not cause is kept in `root`
/// ([`last_failure`]).
/// # Errors
/// Each says why and that nothing was installed: the root or scratch cannot be made, the download
/// failed or was cut short, its size or sha256 is not the pin's, it does not unpack, it holds no
/// executable, it cannot be put in place, or it was stopped ([`STOPPED`]).
pub fn install(
    root: &Path,
    archive: &Archive,
    fetch: &mut dyn Fetch,
    stopped: &dyn Fn() -> bool,
) -> Result<PathBuf, String> {
    let placed = place(root, archive, fetch, stopped);
    let record = failure_path(root, archive);
    match &placed {
        Ok(_) => {
            let _ = std::fs::remove_file(&record);
        }
        Err(why) if !stopped() => {
            // Best effort: a root that cannot be made keeps no record either.
            if root.is_dir() {
                let _ = super::upgrade_models::write_atomic(
                    &record,
                    &format!("{}\n", super::one_line(why, FAILED_WHY_MAX)),
                );
            }
        }
        Err(_) => {}
    }
    placed
}

/// [`install`] before its record.
fn place(
    root: &Path,
    archive: &Archive,
    fetch: &mut dyn Fetch,
    stopped: &dyn Fn() -> bool,
) -> Result<PathBuf, String> {
    let dest = root.join(&archive.dir);
    let binary = dest.join(&archive.binary);
    if is_executable(&binary) {
        return Ok(binary);
    }
    let none = |why: String| format!("{why}; nothing was installed");
    aterm_types::fs_restricted::create_dir_all_unshared(root, 0o700)
        .map_err(|e| none(format!("{} cannot be made: {e}", root.display())))?;
    remove_stale_scratch(root);
    let scratch = aterm_tempfile::Builder::new()
        .prefix(SCRATCH_PREFIX)
        .tempdir_in(root)
        .map_err(|e| {
            none(format!(
                "no scratch could be made in {}: {e}",
                root.display()
            ))
        })?;
    let file = scratch.path().join("archive.zip");
    fetch
        .fetch(&archive.url, &file, archive.bytes, archive.within, stopped)
        .map_err(|why| none(format!("the download failed: {why}")))?;
    if stopped() {
        return Err(none(format!("the download failed: {STOPPED}")));
    }
    let got = std::fs::metadata(&file)
        .map_err(|_| none("the download left no file".to_owned()))?
        .len();
    if got != archive.bytes {
        return Err(none(format!(
            "the download is {got} bytes and the pin is {}",
            archive.bytes
        )));
    }
    let hash = sha256_file(&file).map_err(none)?;
    if !hash.eq_ignore_ascii_case(&archive.sha256) {
        return Err(none(format!(
            "the download hashes to {hash} and the pin is {}",
            archive.sha256
        )));
    }
    let tree = scratch.path().join("tree");
    std::fs::create_dir(&tree).map_err(|e| none(format!("no tree could be made: {e}")))?;
    atpkg::extract::extract_tree(
        &file,
        &tree,
        atpkg::extract::Container::Zip,
        UNPACKED_MAX,
        atpkg::extract::ExtractOptions {
            strip_components: 0,
            symlinks: atpkg::extract::SymlinkPolicy::InRoot,
            allow_global_pax: false,
        },
    )
    .map_err(|e| none(format!("the archive did not unpack: {e}")))?;
    if !is_executable(&tree.join(&archive.binary)) {
        return Err(none(format!(
            "the archive holds no executable {}",
            archive.binary.display()
        )));
    }
    if stopped() {
        return Err(none(STOPPED.to_owned()));
    }
    // The rename lands only where nothing (or an empty directory) is: a tree another fetch
    // placed meanwhile (`accounts browser` fetches before it takes the sign-in lock, so two
    // terminals may) is never moved, since a browser may already run from it.
    match std::fs::rename(&tree, &dest) {
        Ok(()) => return Ok(binary),
        // Another fetch placed the same pin first: its tree stays, this one goes with the scratch.
        Err(_) if is_executable(&binary) => return Ok(binary),
        Err(_) => {}
    }
    // Something without the executable is there (a copy cut short by hand): set aside in the
    // scratch, which goes with it, and the tree renamed in its place. Looked at once more first,
    // so only a tree another fetch placed within these few calls can be set aside, and that one
    // only where something broken was there before both.
    if is_executable(&binary) {
        return Ok(binary);
    }
    let _ = std::fs::rename(&dest, scratch.path().join("replaced"));
    match std::fs::rename(&tree, &dest) {
        Ok(()) => Ok(binary),
        Err(_) if is_executable(&binary) => Ok(binary),
        Err(e) => Err(none(format!("{} cannot be placed: {e}", dest.display()))),
    }
}

#[cfg(test)]
#[path = "headless_fetch_tests.rs"]
mod tests;
