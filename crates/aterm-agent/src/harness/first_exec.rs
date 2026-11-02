// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A program the harness starts and may kill before it ends, held in place from before its start
//! until it showed it ran: macOS may assess a program's first exec for up to
//! [`aterm_uds::deadline::FIRST_EXEC_HANG`], and syspolicyd crashes when the file goes
//! mid-assessment.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// The folder under the harness state directory that holds the programs the harness started and
/// has not seen run ([`hold`], [`held`]).
pub const HOLDS: &str = "first-exec";

/// Why a program a hold names stays where it is.
pub const KEPT: &str = "the harness started it and has not seen it run, so macOS may still be \
                        checking its first start";

/// The program file a hold names: what `path` file inside each hold folder spells.
const PATH: &str = "path";

/// The holds folder of the harness state directory `state`.
#[must_use]
pub fn holds_dir(state: &Path) -> PathBuf {
    state.join(HOLDS)
}

/// One prepared launch's independent image marks. Dropped without proof, they stay held for
/// [`aterm_uds::deadline::FIRST_EXEC_HANG`]; uncertain helper marks retain that lifetime too.
#[must_use = "a hold nothing releases keeps its program for FIRST_EXEC_HANG"]
pub(crate) struct Hold {
    main: Vec<Mark>,
    helpers: Vec<Mark>,
    launch: Launch,
}

struct Mark {
    store: Option<atpkg::store::FirstExec>,
    dir: Option<PathBuf>,
    holds: PathBuf,
    files: Vec<(PathBuf, std::fs::File)>,
}

/// The only arguments a prepared policy-bearing launch may receive.
pub(crate) enum ProbeWord {
    Version,
    Help,
}

impl ProbeWord {
    fn word(&self) -> &'static str {
        match self {
            Self::Version => "--version",
            Self::Help => "--help",
        }
    }
}

/// The command and the mark come from one inspection. Neither probe reopens a wrapper or
/// alias to select the executable after this admission.
pub(crate) struct Prepared {
    pub(crate) command: aterm_uds::spawnfd::Command,
    pub(crate) hold: Option<Hold>,
}

pub(crate) fn prepare(
    exe: &Path,
    holds: Option<&Path>,
    word: ProbeWord,
) -> std::io::Result<Prepared> {
    prepare_using(exe, holds, word, hold)
}

fn prepare_using(
    exe: &Path,
    holds: Option<&Path>,
    word: ProbeWord,
    register: impl FnOnce(&Path, &Path) -> std::io::Result<Hold>,
) -> std::io::Result<Prepared> {
    let hold = holds.map(|holds| register(holds, exe)).transpose()?;
    let command = match &hold {
        Some(hold) => hold.launch.command(word),
        None => Launch::inspect(exe)?.command(word),
    };
    Ok(Prepared { command, hold })
}

enum Launch {
    Direct {
        program: PathBuf,
        argv0: PathBuf,
        env: atpkg::shim_env::ShimEnv,
    },
    #[cfg(windows)]
    WindowsSpelling(WindowsSpelling),
    #[cfg(unix)]
    Policy {
        images: Vec<atpkg::platform::ProbeImage>,
        body: String,
        shell0: PathBuf,
    },
}

impl Launch {
    #[cfg(all(test, unix))]
    fn target(&self) -> &Path {
        match self {
            Self::Direct { program, .. } => program,
            #[cfg(unix)]
            Self::Policy { images, .. } => {
                &images
                    .iter()
                    .find(|image| !image.helper)
                    .expect("a snapshot has a main image")
                    .physical
            }
        }
    }

    fn images(&self) -> Vec<(&Path, bool)> {
        match self {
            Self::Direct { program, .. } => vec![(program.as_path(), false)],
            #[cfg(windows)]
            Self::WindowsSpelling(spelling) => vec![(spelling.physical.as_path(), false)],
            #[cfg(unix)]
            Self::Policy { images, .. } => images
                .iter()
                .map(|image| (image.physical.as_path(), image.helper))
                .collect(),
        }
    }

    fn command(&self, word: ProbeWord) -> aterm_uds::spawnfd::Command {
        let mut command = match self {
            Self::Direct {
                program,
                argv0,
                env,
            } => {
                let mut command = aterm_uds::spawnfd::command(program);
                #[cfg(unix)]
                command.arg0(argv0);
                #[cfg(not(unix))]
                let _ = argv0; // inspection refused a spelling std cannot preserve.
                command.envs(env.entries().iter().map(|(name, value)| (name, value)));
                command
            }
            #[cfg(windows)]
            Self::WindowsSpelling(spelling) => spelling.command(),
            #[cfg(unix)]
            Self::Policy { body, shell0, .. } => {
                let mut command = aterm_uds::spawnfd::command("/bin/sh");
                command.arg("-c").arg(body).arg(shell0);
                command
            }
        };
        command.arg(word.word());
        command
    }
}

impl Hold {
    /// Main execution ended naturally. A shell may hide a helper's signal, so helper marks
    /// conservatively retain their ordinary TTL rather than borrowing this main's proof.
    pub(crate) fn ran(self) {
        for mark in self.main {
            mark.ran();
        }
    }

    fn never_started(self) {
        for mark in self.main.into_iter().chain(self.helpers) {
            mark.ran();
        }
    }
}

impl Mark {
    fn ran(self) {
        if let Some(store) = self.store {
            store.ran();
        }
        if let Some(dir) = self.dir {
            let Ok(_gate) = registration_gate(&self.holds) else {
                return;
            };
            if self.files.iter().any(|(path, file)| {
                !std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_file())
                    || !astream_sys::file_matches_path(file, path).unwrap_or(false)
            }) {
                return;
            }
            for (path, _) in self.files {
                if std::fs::remove_file(path).is_err() {
                    return;
                }
            }
            let _ = std::fs::remove_dir(dir);
        }
    }
}

/// Release `hold`, if any ([`Hold::ran`]).
pub(crate) fn ran(hold: &mut Option<Hold>) {
    if let Some(hold) = hold.take() {
        hold.ran();
    }
}

/// No child existed, so every mark this attempted launch acquired can be released.
pub(crate) fn never_started(hold: &mut Option<Hold>) {
    if let Some(hold) = hold.take() {
        hold.never_started();
    }
}

/// Capture `exe`'s launch and hold every admitted main/helper image before starting: store
/// images use atpkg's marks ([`atpkg::store::hold_first_exec`]); other images use `holds`
/// ([`held`]). The same descriptor supplies the probe command, without a second lookup.
/// # Errors
/// The hold could not be written: start nothing.
pub(crate) fn hold(holds: &Path, exe: &Path) -> std::io::Result<Hold> {
    let store = atpkg::store::resolve_configured().map(|layout| layout.prefix.join("store"));
    hold_in(holds, exe, store.as_deref())
}

fn hold_in(holds: &Path, exe: &Path, store: Option<&Path>) -> std::io::Result<Hold> {
    let launch = Launch::inspect(exe)?;
    register_launch(holds, launch, store)
}

fn register_launch(holds: &Path, launch: Launch, store: Option<&Path>) -> std::io::Result<Hold> {
    let store = store
        .map(|store| match std::fs::canonicalize(store) {
            Ok(path) => Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(store.to_path_buf()),
            Err(error) => Err(error),
        })
        .transpose()?;
    // A main and helper at the same physical path retain separate ownership/proof roles.
    // A natural main exit cannot release the uncertain helper's independently owned mark.
    let images: std::collections::BTreeSet<(PathBuf, bool)> = launch
        .images()
        .into_iter()
        .map(|(image, helper)| (image.to_path_buf(), helper))
        .collect();
    let mut main = Vec::new();
    let mut helpers = Vec::new();
    for (target, helper) in images {
        match register_target(holds, &target, store.as_deref()) {
            Ok(mark) => {
                if helper {
                    helpers.push(mark)
                } else {
                    main.push(mark)
                }
            }
            Err(error) => {
                for mark in main.into_iter().chain(helpers) {
                    mark.ran();
                }
                return Err(error);
            }
        }
    }
    Ok(Hold {
        main,
        helpers,
        launch,
    })
}

fn register_target(holds: &Path, target: &Path, store: Option<&Path>) -> std::io::Result<Mark> {
    let _gate = registration_gate(holds)?;
    if store.is_some_and(|store| target.starts_with(store)) {
        return Ok(Mark {
            store: Some(atpkg::store::hold_first_exec(target)?),
            dir: None,
            holds: holds.to_path_buf(),
            files: Vec::new(),
        });
    }
    let now = std::time::SystemTime::now();
    if let Ok(entries) = std::fs::read_dir(holds) {
        for entry in entries.flatten() {
            if !directory_holds(&entry.path(), now) {
                remove_expired_mark(&entry.path());
            }
        }
    }
    static SEQ: AtomicU64 = AtomicU64::new(0);
    std::fs::create_dir_all(holds)?;
    let dir = holds.join(format!(
        "{}-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed),
        now.duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    ));
    std::fs::create_dir(&dir)?;
    let written = (|| -> std::io::Result<Vec<(PathBuf, std::fs::File)>> {
        use std::io::Write as _;
        let path = dir.join(PATH);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let mut file = options.open(&path)?;
        file.write_all(target.as_os_str().as_encoded_bytes())?;
        aterm_uds::deadline::try_stamp_exec(&dir)?;
        let stamp_path = dir.join(aterm_uds::deadline::EXEC_STAMP);
        let stamp = astream_sys::open_nonblocking(&stamp_path, astream_sys::Links::Refuse)?;
        Ok(vec![(path, file), (stamp_path, stamp)])
    })();
    let files = match written {
        Ok(files) => files,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(e);
        }
    };
    Ok(Mark {
        store: None,
        dir: Some(dir),
        holds: holds.to_path_buf(),
        files,
    })
}

/// One stable gate beside the inventory, not inside any version or disposable hold directory.
/// Every acquisition is nonblocking; no code removes or replaces its pathname.
fn registration_gate(holds: &Path) -> std::io::Result<super::disk::reserve::PassLock> {
    if let Ok(meta) = std::fs::symlink_metadata(holds)
        && !meta.file_type().is_dir()
    {
        return Err(std::io::Error::other(
            "first exec inventory is not a real directory",
        ));
    }
    let mut name = std::ffi::OsString::from(".");
    name.push(
        holds
            .file_name()
            .ok_or_else(|| std::io::Error::other("no hold inventory name"))?,
    );
    name.push(".gate");
    super::disk::reserve::take_pass(&holds.with_file_name(name))?.ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            "first exec inventory is busy",
        )
    })
}

/// Whether a hold under `holds` younger than [`aterm_uds::deadline::FIRST_EXEC_HANG`] of awake
/// time at `now` names `tree` or a file in it, `tree` read with its links resolved as [`hold`]
/// wrote it; a fresh hold whose program cannot be read holds every tree.
#[must_use]
pub fn held(holds: &Path, tree: &Path, now: std::time::SystemTime) -> bool {
    if let Ok(meta) = std::fs::symlink_metadata(holds)
        && !meta.file_type().is_dir()
    {
        return true;
    }
    held_among(
        std::fs::read_dir(holds)
            .map(|entries| entries.map(|entry| entry.map(|entry| entry.path()))),
        tree,
        now,
    )
}

fn held_among(
    entries: std::io::Result<impl Iterator<Item = std::io::Result<PathBuf>>>,
    tree: &Path,
    now: std::time::SystemTime,
) -> bool {
    let entries = match entries {
        Ok(entries) => entries,
        Err(error) => return error.kind() != std::io::ErrorKind::NotFound,
    };
    let tree = std::fs::canonicalize(tree).unwrap_or_else(|_| tree.to_path_buf());
    for entry in entries {
        let Ok(dir) = entry else { return true };
        if directory_holds(&dir, now) {
            let bytes = read_regular_head(&dir.join(PATH), 64 * 1024 + 1);
            if bytes
                .ok()
                .filter(|bytes| bytes.len() <= 64 * 1024)
                .is_none_or(|bytes| PathBuf::from(os_string(bytes)).starts_with(&tree))
            {
                return true;
            }
        }
    }
    false
}

/// First-exec inventories contain metadata only. Unknown kind, stamp, or clock never proves
/// expiry; valid stamps retain the existing awake-clock lifetime and restart rule.
fn directory_holds(dir: &Path, now: std::time::SystemTime) -> bool {
    directory_holds_at(dir, now, aterm_uds::process::awake_ms())
}

fn directory_holds_at(dir: &Path, now: std::time::SystemTime, awake: Option<u64>) -> bool {
    if !std::fs::symlink_metadata(dir).is_ok_and(|meta| meta.file_type().is_dir()) {
        return true;
    }
    let Ok(bytes) = read_regular_head(&dir.join(aterm_uds::deadline::EXEC_STAMP), 129) else {
        return true;
    };
    if bytes.len() > 128 {
        return true;
    }
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return true;
    };
    let blank = text.is_empty();
    let text = text.trim();
    let stamp = if blank {
        None
    } else if !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()) {
        let Ok(stamp) = text.parse::<u64>() else {
            return true;
        };
        Some(stamp)
    } else {
        return true;
    };
    let age = match (stamp, awake) {
        (Some(stamp), Some(awake)) => match awake.checked_sub(stamp) {
            Some(ms) => std::time::Duration::from_millis(ms),
            None => return false,
        },
        _ => match std::fs::symlink_metadata(dir)
            .ok()
            .and_then(|meta| meta.modified().ok())
            .and_then(|at| now.duration_since(at).ok())
        {
            Some(age) => age,
            None => return true,
        },
    };
    age < aterm_uds::deadline::FIRST_EXEC_HANG
}

fn remove_expired_mark(dir: &Path) {
    let Ok(entries) =
        std::fs::read_dir(dir).and_then(|entries| entries.collect::<Result<Vec<_>, _>>())
    else {
        return;
    };
    if entries.len() != 2
        || entries.iter().any(|entry| {
            !entry.file_type().is_ok_and(|kind| kind.is_file())
                || ![PATH, aterm_uds::deadline::EXEC_STAMP]
                    .iter()
                    .any(|name| entry.file_name() == *name)
        })
    {
        return;
    }
    let _ = std::fs::remove_file(dir.join(PATH));
    let _ = std::fs::remove_file(dir.join(aterm_uds::deadline::EXEC_STAMP));
    let _ = std::fs::remove_dir(dir);
}

/// Recheck the holds immediately before removing a native version.
pub(crate) fn remove_when_unheld<T>(
    holds: &Path,
    tree: &Path,
    now: std::time::SystemTime,
    remove: impl FnOnce() -> std::io::Result<T>,
) -> std::io::Result<T> {
    let _gate = registration_gate(holds)?;
    if held(holds, tree, now) {
        return Err(std::io::Error::other(KEPT));
    }
    remove()
}

/// The path [`hold`] wrote as `bytes`.
fn os_string(bytes: Vec<u8>) -> std::ffi::OsString {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt as _;
        std::ffi::OsString::from_vec(bytes)
    }
    #[cfg(not(unix))]
    {
        std::ffi::OsString::from(String::from_utf8_lossy(&bytes).into_owned())
    }
}

impl Launch {
    /// Inspect exactly once without starting. A missing candidate reaches ordinary spawn
    /// failure; unreadable/special or unresolved managed wrappers refuse before registration.
    fn inspect(exe: &Path) -> std::io::Result<Self> {
        Self::inspect_using(exe, None)
    }

    fn inspect_using(
        exe: &Path,
        #[cfg(unix)] forwarder: Option<&atpkg::platform::ProbeForwarder>,
        #[cfg(not(unix))] _forwarder: Option<&()>,
    ) -> std::io::Result<Self> {
        let resolved = physical_or_missing(exe)?;
        let bytes = match read_regular_head(&resolved, 64 * 1024 + 1) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error),
        };
        let text = std::str::from_utf8(&bytes).ok();
        let managed_name = exe
            .parent()
            .is_some_and(|parent| parent.file_name().is_some_and(|name| name == "agents"))
            && exe
                .file_stem()
                .and_then(|name| name.to_str())
                .and_then(atpkg::vendor_direct::spec)
                .is_some();
        let managed = bytes.starts_with(b"#!/bin/sh\n# atpkg shim ")
            || (managed_name
                && (bytes.starts_with(b"#!") || exe.extension().is_some_and(|ext| ext == "cmd")));
        let named = text.and_then(super::upgrade_catalog::twin_binary);
        #[cfg(unix)]
        if managed || named.is_some() {
            let current =
                aterm_uds::tether::executable().map(|executable| atpkg::platform::ProbeForwarder {
                    executable: executable.to_path_buf(),
                    parent_pid: std::process::id(),
                });
            let forwarder = forwarder.or(current.as_ref()).ok_or_else(|| {
                std::io::Error::other("no unchanged in-place probe forwarder is available")
            })?;
            let captured = text
                .filter(|_| bytes.len() <= 64 * 1024)
                .and_then(|text| atpkg::platform::probe_shim(exe, text, forwarder))
                .ok_or_else(|| {
                    std::io::Error::other("the managed probe launch cannot be pinned")
                })?;
            return Ok(Self::Policy {
                images: captured.images,
                body: captured.body,
                shell0: exe.to_path_buf(),
            });
        }
        #[cfg(not(unix))]
        {
            if managed || named.is_some() {
                return Err(std::io::Error::other(
                    "this platform cannot pin the wrapper",
                ));
            }
            if resolved != exe {
                #[cfg(windows)]
                if let Some(spelling) = WindowsSpelling::admit(exe, &resolved) {
                    return Ok(Self::WindowsSpelling(spelling));
                }
                return Err(std::io::Error::other(
                    "this platform cannot preserve alias argv0",
                ));
            }
        }
        Ok(Self::Direct {
            program: resolved,
            argv0: exe.to_path_buf(),
            env: atpkg::shim_env::ShimEnv::NONE,
        })
    }
}

/// A Windows native path whose only canonicalization difference is the ordinary/verbatim
/// spelling. Keep the caller's program spelling for std's argv0, while its hold uses the
/// physical spelling. This does not admit a junction/symlink whose resolved path differs.
#[cfg(any(windows, test))]
struct WindowsSpelling {
    program: PathBuf,
    physical: PathBuf,
}

#[cfg(any(windows, test))]
impl WindowsSpelling {
    fn admit(program: &Path, physical: &Path) -> Option<Self> {
        use aterm_sysprobe::paths::{plain, windows_key};
        // Validate both roots/components, but do not compare folded keys: folding could
        // hide a different target on a case-sensitive Windows directory.
        windows_key(program)?;
        windows_key(physical)?;
        (plain(program).as_os_str() == plain(physical).as_os_str()).then(|| Self {
            program: program.to_path_buf(),
            physical: physical.to_path_buf(),
        })
    }

    fn command(&self) -> aterm_uds::spawnfd::Command {
        // No separate argv0 API is claimed off Unix. These two strings name the same
        // admitted native path; std receives exactly the spelling the caller supplied.
        aterm_uds::spawnfd::command(&self.program)
    }
}

fn physical_or_missing(path: &Path) -> std::io::Result<PathBuf> {
    let mut path = if path.is_absolute() {
        path.to_path_buf()
    } else if path
        .parent()
        .is_some_and(|parent| !parent.as_os_str().is_empty())
    {
        std::env::current_dir()?.join(path)
    } else {
        return Err(std::io::Error::other(
            "a bare PATH search is not an admitted probe image",
        ));
    };
    let mut missing = Vec::new();
    loop {
        match std::fs::canonicalize(&path) {
            Ok(mut physical) => {
                for name in missing.into_iter().rev() {
                    physical.push(name);
                }
                return Ok(physical);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match std::fs::symlink_metadata(&path) {
                    Ok(_) => {
                        return Err(std::io::Error::other(
                            "an existing probe alias has no resolved target",
                        ));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
                let name = path
                    .file_name()
                    .ok_or_else(|| std::io::Error::other("no physical probe ancestor"))?;
                missing.push(name.to_os_string());
                if !path.pop() {
                    return Err(error);
                }
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(all(test, unix))]
fn inspect_program_file(exe: &Path) -> std::io::Result<PathBuf> {
    let inert = atpkg::platform::ProbeForwarder {
        executable: std::env::current_exe()?,
        parent_pid: std::process::id(),
    };
    Launch::inspect_using(exe, Some(&inert)).map(|launch| launch.target().to_path_buf())
}

// The paired liveness fixture observes completion even when bounded inspection refuses its
// FIFO. Production admission uses the fallible inspector directly and starts nothing then.
#[cfg(all(test, unix))]
fn program_file(exe: &Path) -> PathBuf {
    inspect_program_file(exe).unwrap_or_else(|_| exe.to_path_buf())
}

/// Bound inspection on the admitted regular handle. On Unix the open never waits for a FIFO.
fn read_regular_head(path: &Path, limit: u64) -> std::io::Result<Vec<u8>> {
    use std::io::Read as _;
    let file = astream_sys::open_nonblocking(path, astream_sys::Links::Refuse)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other("not a regular program or hold file"));
    }
    let mut bytes = Vec::new();
    file.take(limit).read_to_end(&mut bytes)?;
    Ok(bytes)
}

#[cfg(all(test, unix))]
#[path = "first_exec_tests.rs"]
mod tests;

#[cfg(all(test, unix))]
#[path = "first_exec_regression_tests.rs"]
mod regression_tests;

#[cfg(test)]
#[path = "first_exec_windows_spelling_tests.rs"]
mod windows_spelling_tests;
