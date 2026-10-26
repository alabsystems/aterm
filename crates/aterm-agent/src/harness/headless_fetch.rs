// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE HARNESS FETCHES ITS OWN HEADLESS BROWSER. Where no headless browser is found
//! ([`super::headless_oauth::find_headless_browser`]) and a headless sign-in waits on one alone
//! (the Grok gate's automatic rung, `super::grok_signin::LiveAuto`), the pinned
//! chrome-headless-shell is fetched into the cache root that finder reads
//! ([`cache_root`]): the browser lane's own pin ([`aterm_uds::host::chrome`], one source for
//! both), downloaded by [`CURL`] by its absolute path over https only, held to the pin's size
//! and then its sha256, unpacked by atpkg's vetted extractor (each member beneath the tree,
//! links only within it), and moved into place with one rename once its executable is there.
//!
//! Fail-closed: no network, a partial download, a size or sha256 other than the pin's, an
//! archive that does not unpack or holds no executable installs nothing and says why; the
//! scratch it fetched into goes with it. A stop (the caller's Ctrl-C, a hangup) ends the
//! download within one poll, curl's process group killed, and installs nothing either
//! ([`run_stoppable`]). Nothing here runs the browser.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// The downloader, by its absolute path (`DESIGN-init-system` S12).
pub const CURL: &str = "/usr/bin/curl";

/// The longest one fetch may take (`curl --max-time`): the archive is about 100 MB.
pub const FETCH_WITHIN: Duration = Duration::from_secs(5 * 60);

/// The most bytes an unpacked archive may hold: the pinned shell unpacks to about 250 MB.
pub const UNPACKED_MAX: u64 = 1 << 30;

/// The prefix of a fetch's scratch directory in the cache root (never `chrome-headless-shell-`,
/// which the finder reads).
const SCRATCH_PREFIX: &str = ".fetch-chrome-headless-shell-";

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
    /// Downloads `url` into the new file `dest`, at most `bytes` long, ending at once (within a
    /// poll) once `stopped` says the caller was asked to stop.
    /// # Errors
    /// Why it did not finish (no network, a refusal, the time ran out, [`STOPPED`]).
    fn fetch(
        &mut self,
        url: &str,
        dest: &Path,
        bytes: u64,
        stopped: &dyn Fn() -> bool,
    ) -> Result<(), String>;
}

/// [`CURL`]: https only (redirects too), TLS 1.2 at least, no retry, at most the pin's size and
/// [`FETCH_WITHIN`], stoppable ([`run_stoppable`]).
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
    let until = Instant::now() + within;
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

/// What a curl exit means, in words.
fn curl_words(code: Option<i32>) -> String {
    match code {
        Some(6) => "the host could not be resolved (no network?)".to_owned(),
        Some(7) => "the host could not be reached (no network?)".to_owned(),
        Some(22) => "the server refused the download".to_owned(),
        Some(28) => format!(
            "it did not finish within {} min",
            FETCH_WITHIN.as_secs() / 60
        ),
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
        .arg(FETCH_WITHIN.as_secs().to_string())
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
        match run_stoppable(cmd, FETCH_WITHIN + Duration::from_secs(30), stopped)? {
            Some(0) => Ok(()),
            code => Err(curl_words(code)),
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
        })
    }

    /// This host's pinned chrome-headless-shell ([`Archive::pinned`]).
    #[must_use]
    pub fn host() -> Option<Archive> {
        Archive::pinned(std::env::consts::OS, std::env::consts::ARCH)
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

/// Installs `archive` into the cache `root` through `fetch` and returns its executable: at once
/// where it is already in place; else downloaded into a fresh scratch in `root`, held to its
/// size and sha256, unpacked there, and renamed into place once its executable is in the tree.
/// `stopped` is asked before each step and through the download ([`Fetch::fetch`]).
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
        .fetch(&archive.url, &file, archive.bytes, stopped)
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
    if std::fs::symlink_metadata(&dest).is_ok() {
        // A directory without the executable (a copy cut short by hand): set aside in the
        // scratch, which goes with it, so the rename below lands on nothing.
        let _ = std::fs::rename(&dest, scratch.path().join("replaced"));
    }
    match std::fs::rename(&tree, &dest) {
        Ok(()) => Ok(binary),
        // Another fetch placed the same pin first.
        Err(_) if is_executable(&binary) => Ok(binary),
        Err(e) => Err(none(format!("{} cannot be placed: {e}", dest.display()))),
    }
}

#[cfg(test)]
#[path = "headless_fetch_tests.rs"]
mod tests;
