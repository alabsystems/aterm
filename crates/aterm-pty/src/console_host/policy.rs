// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The console-host POLICY, with every Windows call injected so it runs (and is tested) on any host: a choice resolved once and kept, the once-per-failure fallback note, a silent host counted before its silence sticks ([`HostState::note_silent_start`]), where the bundled default and `auto` find an OpenConsole, which programs keep conhost under them (those of the MSYS2/Cygwin runtime, when this process cannot guard their input there), the copy out of Windows Terminal's package (`auto` only), and the report lines.

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use super::{ConsoleHostChoice, ConsoleHostKind, ConsoleHostReport};

/// Where an OpenConsole.exe came from, for the host report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Origin {
    /// Beside the running executable.
    Bundled,
    /// Copied out of this Windows Terminal package (its full name).
    WindowsTerminal(String),
    /// aterm.toml `conpty = "<path>"`.
    Configured,
}

/// An OpenConsole.exe a session may launch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OpenConsoleExe {
    pub(crate) path: PathBuf,
    pub(crate) origin: Origin,
    /// The file version (`a.b.c.d`), when the file carries one.
    pub(crate) version: Option<String>,
}

/// Why a choice of [`ConsoleHostChoice::Inbox`] resolves to no OpenConsole.
pub(crate) const INBOX_REASON: &str = "aterm.toml conpty is \"inbox\"";

/// Why the default, [`ConsoleHostChoice::Bundled`], resolves to no OpenConsole: this build has none beside it (a bare `cargo build`, or a folder that lost it), and the default never takes Windows Terminal's.
pub(crate) const NOT_BUNDLED: &str = "no OpenConsole.exe beside aterm (reinstall aterm to restore \
                                      it; aterm.toml conpty = \"auto\" also tries Windows Terminal's)";

/// How many spawns in a row may find their OpenConsole silent before every later spawn under the choice takes the inbox host ([`HostState::note_silent_start`]).
pub(crate) const SILENT_STARTS: u32 = 3;

/// A process's console-host state: the frontend's choice, its resolution (made on first use after the choice was set, then kept: a host that failed is not retried until the choice changes), a fallback not yet logged, and whether this process can guard an MSYS2 program's input under OpenConsole.
#[derive(Debug)]
pub(crate) struct HostState {
    choice: ConsoleHostChoice,
    resolved: Option<Result<OpenConsoleExe, String>>,
    unlogged: Option<String>,
    /// The Windows backend can start the resize guard's client (its
    /// `console_guard`), so the bundled default and `auto` give MSYS2/Cygwin
    /// programs OpenConsole too ([`Self::host_for_spawn`]).
    guarded: bool,
    /// Spawns in a row whose host said nothing to a client that was still
    /// waiting ([`Self::note_silent_start`]); a host that speaks resets it.
    silent_starts: u32,
}

impl HostState {
    /// [`ConsoleHostChoice::Inbox`]: an embedder that never states a choice (the CLI's passthrough session, the backend's own tests) keeps the host every earlier build used.
    pub(crate) const fn new() -> Self {
        Self {
            choice: ConsoleHostChoice::Inbox,
            resolved: None,
            unlogged: None,
            guarded: false,
            silent_starts: 0,
        }
    }

    /// Whether this process can start the resize guard's client: an MSYS2 program's input then survives a resize under OpenConsole, and the bundled default and `auto` stop keeping those programs on the inbox host.
    pub(crate) fn set_guarded(&mut self, guarded: bool) {
        self.guarded = guarded;
    }

    /// Set the choice every LATER spawn uses.
    pub(crate) fn set_choice(&mut self, choice: ConsoleHostChoice) {
        if self.choice != choice {
            self.choice = choice;
            self.resolved = None;
            self.silent_starts = 0;
        }
    }

    /// The fallback to the Windows console host not reported yet: at most once per failure, for the frontend's log (this crate has none).
    pub(crate) fn take_unlogged(&mut self) -> Option<String> {
        self.unlogged.take()
    }

    /// The resolution of the current choice, made now if none was. The first
    /// resolution of a choice that names an OpenConsole (every one but
    /// `inbox`, the default included) and found none is handed to the log.
    fn resolution(
        &mut self,
        resolve: impl FnOnce(&ConsoleHostChoice) -> Result<OpenConsoleExe, String>,
    ) -> &Result<OpenConsoleExe, String> {
        let choice = &self.choice;
        let unlogged = &mut self.unlogged;
        self.resolved.get_or_insert_with(|| {
            let resolved = resolve_choice(choice, resolve);
            if resolved.is_err() && *choice != ConsoleHostChoice::Inbox {
                *unlogged = Some(report(choice, &resolved).line);
            }
            resolved
        })
    }

    /// The console host a new tab gets under the current choice.
    pub(crate) fn report(
        &mut self,
        resolve: impl FnOnce(&ConsoleHostChoice) -> Result<OpenConsoleExe, String>,
    ) -> ConsoleHostReport {
        let resolved = self.resolution(resolve).clone();
        report_guarded(&self.choice, &resolved, self.guarded)
    }

    /// The OpenConsole a spawn of `program` should launch now, `None` for the inbox host.
    pub(crate) fn host_for_spawn(
        &mut self,
        program: &Path,
        resolve: impl FnOnce(&ConsoleHostChoice) -> Result<OpenConsoleExe, String>,
        is_file: impl Fn(&Path) -> bool,
    ) -> Option<OpenConsoleExe> {
        if matches!(&self.resolved, Some(Ok(exe))
            if matches!(exe.origin, Origin::WindowsTerminal(_)) && !is_file(&exe.path))
        {
            self.resolved = None;
            self.silent_starts = 0;
        }
        let exe = self.resolution(resolve).as_ref().ok().cloned()?;
        if spares_posix_runtimes(&self.choice)
            && !self.guarded
            && runs_a_posix_runtime(program, is_file)
        {
            return None;
        }
        Some(exe)
    }

    /// `exe` failed to start for a spawn (it exited, or wrote something other than VT): every later spawn under this choice takes the inbox host, and the log hears why once.
    pub(crate) fn note_launch_failure(&mut self, exe: &OpenConsoleExe, why: &str) {
        if self.is_current(exe) {
            let failed = Err(did_not_start(why));
            self.unlogged = Some(report(&self.choice, &failed).line);
            self.resolved = Some(failed);
        }
    }

    /// `exe` said nothing within the health window to a spawn's client that was still waiting for it, and neither of them exited: that tab takes the inbox host, and the log hears why.
    pub(crate) fn note_silent_start(&mut self, exe: &OpenConsoleExe, why: &str) {
        if !self.is_current(exe) {
            return;
        }
        self.silent_starts += 1;
        if self.silent_starts >= SILENT_STARTS {
            self.note_launch_failure(exe, why);
        } else {
            self.unlogged = Some(format!(
                "Windows console host for one tab; {} (the next tab tries it again)",
                did_not_start(why)
            ));
        }
    }

    /// `exe` answered a spawn's client: a silence before it was not a failure.
    pub(crate) fn note_started(&mut self, exe: &OpenConsoleExe) {
        if self.is_current(exe) {
            self.silent_starts = 0;
        }
    }

    /// Whether `exe` is still what the current choice resolved to (a report on
    /// a host a changed choice dropped changes nothing).
    fn is_current(&self, exe: &OpenConsoleExe) -> bool {
        matches!(&self.resolved, Some(Ok(current)) if current == exe)
    }
}

/// `choice` resolved by `resolve`, except [`ConsoleHostChoice::Inbox`], which resolves to the inbox host without consulting anything: `inbox` looks for no file and copies nothing.
pub(crate) fn resolve_choice(
    choice: &ConsoleHostChoice,
    resolve: impl FnOnce(&ConsoleHostChoice) -> Result<OpenConsoleExe, String>,
) -> Result<OpenConsoleExe, String> {
    match choice {
        ConsoleHostChoice::Inbox => Err(INBOX_REASON.to_string()),
        ConsoleHostChoice::Bundled | ConsoleHostChoice::Auto | ConsoleHostChoice::Path(_) => {
            resolve(choice)
        }
    }
}

/// The choices under which a program of the MSYS2/Cygwin runtime keeps the
/// inbox host when this process cannot guard its input under OpenConsole: the
/// bundled default and `auto`, aterm's own picks.
fn spares_posix_runtimes(choice: &ConsoleHostChoice) -> bool {
    matches!(choice, ConsoleHostChoice::Bundled | ConsoleHostChoice::Auto)
}

pub(crate) fn did_not_start(why: &str) -> String {
    format!("OpenConsole did not start ({why})")
}

/// The report line for a resolution: the host, then where it came from, for a process that can guard an MSYS2 program's input under OpenConsole (every shipped layout; [`report_guarded`] says when one cannot).
pub(crate) fn report(
    choice: &ConsoleHostChoice,
    resolved: &Result<OpenConsoleExe, String>,
) -> ConsoleHostReport {
    report_guarded(choice, resolved, true)
}

/// [`report`], then, under the bundled default and `auto` when this process cannot guard their input (`guarded` false, [`HostState::set_guarded`]), that MSYS2/Cygwin programs keep the Windows console host.
pub(crate) fn report_guarded(
    choice: &ConsoleHostChoice,
    resolved: &Result<OpenConsoleExe, String>,
    guarded: bool,
) -> ConsoleHostReport {
    match resolved {
        Ok(exe) => {
            let name = match &exe.version {
                Some(v) => format!("OpenConsole {v}"),
                None => "OpenConsole".to_string(),
            };
            let from = match &exe.origin {
                Origin::Bundled => format!("bundled beside aterm: {}", exe.path.display()),
                Origin::WindowsTerminal(package) => format!(
                    "from Windows Terminal {}, copied to {}",
                    package_version(package),
                    exe.path.display()
                ),
                Origin::Configured => format!("aterm.toml conpty = \"{}\"", exe.path.display()),
            };
            let except = if spares_posix_runtimes(choice) && !guarded {
                "; Git Bash and other MSYS2/Cygwin programs keep the Windows console host \
                 (no aterm.exe beside aterm to guard their input after a resize)"
            } else {
                ""
            };
            ConsoleHostReport {
                kind: ConsoleHostKind::OpenConsole,
                fell_back: false,
                line: format!("{name} ({from}){except}"),
            }
        }
        Err(why) => {
            let (line, fell_back) = match choice {
                ConsoleHostChoice::Inbox => (
                    "Windows console host (aterm.toml conpty = \"inbox\")".to_string(),
                    false,
                ),
                ConsoleHostChoice::Bundled | ConsoleHostChoice::Auto => {
                    (format!("Windows console host; {why}"), true)
                }
                ConsoleHostChoice::Path(p) => (
                    format!(
                        "Windows console host; {why} — fix or remove aterm.toml conpty = \"{}\"",
                        p.display()
                    ),
                    true,
                ),
            };
            ConsoleHostReport {
                kind: ConsoleHostKind::Inbox,
                fell_back,
                line,
            }
        }
    }
}

/// `1.24.11911.0` out of `Microsoft.WindowsTerminal_1.24.11911.0_x64__8wekyb3d8bbwe`
/// (the package full name's second `_` field), or the whole name when it has
/// no such field.
fn package_version(full_name: &str) -> &str {
    full_name.split('_').nth(1).unwrap_or(full_name)
}

/// aterm.toml's own path: absolute, and a file.
pub(crate) fn configured(
    path: &Path,
    is_file: impl Fn(&Path) -> bool,
    version_of: impl Fn(&Path) -> Option<String>,
) -> Result<OpenConsoleExe, String> {
    if !path.is_absolute() {
        return Err(format!("{} is not an absolute path", path.display()));
    }
    if !is_file(path) {
        return Err(format!("{} is not a file", path.display()));
    }
    Ok(OpenConsoleExe {
        version: version_of(path),
        path: path.to_path_buf(),
        origin: Origin::Configured,
    })
}

/// Where `auto` finds an OpenConsole.exe.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum AutoSource {
    /// Beside the running executable.
    Bundled(PathBuf),
    /// Inside this Windows Terminal package (full name, the file in it).
    WindowsTerminal { package: String, source: PathBuf },
}

/// The default's source: the `OpenConsole.exe` beside the running executable (`exe_dir`), else [`NOT_BUNDLED`].
pub(crate) fn bundled_source(
    exe_dir: Option<&Path>,
    is_file: impl Fn(&Path) -> bool,
) -> Result<PathBuf, String> {
    exe_dir
        .map(|dir| dir.join("OpenConsole.exe"))
        .filter(|bundled| is_file(bundled))
        .ok_or_else(|| NOT_BUNDLED.to_string())
}

/// The first half of `auto`: the bundled file wins ([`bundled_source`]), then the first Windows Terminal package holding one, else the reason there is none.
pub(crate) fn auto_source(
    exe_dir: Option<&Path>,
    is_file: impl Fn(&Path) -> bool,
    windows_terminal: impl FnOnce() -> Option<(String, PathBuf)>,
) -> Result<AutoSource, String> {
    if let Ok(bundled) = bundled_source(exe_dir, is_file) {
        return Ok(AutoSource::Bundled(bundled));
    }
    match windows_terminal() {
        Some((package, source)) => Ok(AutoSource::WindowsTerminal { package, source }),
        None => Err(
            "no OpenConsole.exe beside aterm and no Windows Terminal installed to take one from"
                .to_string(),
        ),
    }
}

/// Whether `program` runs on the MSYS2 or Cygwin runtime (Git Bash's `bin\bash.exe` launcher, the `usr\bin` programs beside `msys-2.0.dll`, a Cygwin `bin` beside `cygwin1.dll`): the programs whose input needs the resize guard.
pub(crate) fn runs_a_posix_runtime(program: &Path, is_file: impl Fn(&Path) -> bool) -> bool {
    let Some(dir) = program.parent() else {
        return false;
    };
    [
        dir.join("msys-2.0.dll"),
        dir.join("cygwin1.dll"),
        dir.join("..").join("usr").join("bin").join("msys-2.0.dll"),
    ]
    .iter()
    .any(|dll| is_file(dll))
}

const MAX_OPENCONSOLE_BYTES: u64 = 32 * 1024 * 1024;

fn read_openconsole_bounded(path: &Path) -> io::Result<Vec<u8>> {
    use std::io::Read as _;
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let file = opts.open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    if meta.len() > MAX_OPENCONSOLE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "OpenConsole.exe exceeds size limit",
        ));
    }
    let mut buf = Vec::with_capacity(usize::try_from(meta.len().saturating_add(1)).unwrap_or(0));
    file.take(MAX_OPENCONSOLE_BYTES + 1).read_to_end(&mut buf)?;
    if buf.len() as u64 > MAX_OPENCONSOLE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "OpenConsole.exe exceeds size limit",
        ));
    }
    Ok(buf)
}

/// Whether `package` names one folder inside the cache on every platform: one normal path
/// component, with no separator of either kind, no drive (`C:pkg`) and no NUL, and not `.` or `..`.
fn one_package_folder(package: &str) -> bool {
    if package.contains(['/', '\\', ':', '\0']) {
        return false;
    }
    let mut components = Path::new(package).components();
    matches!(components.next(), Some(std::path::Component::Normal(_)))
        && components.next().is_none()
}

/// Copy `source` to `<cache>/<package>/OpenConsole.exe` unless an identical copy is already there, then prune the copies other packages left ([`prune_other_packages`]), so the cache holds one host however many Windows Terminal updates pass (each left about 1 MB behind before). A `package` that is not one folder name ([`one_package_folder`]) is refused before anything is read or written: joined to `cache`, it would copy outside it and prune beside it.
pub(crate) fn copy_out(source: &Path, cache: &Path, package: &str) -> io::Result<PathBuf> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    if !one_package_folder(package) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "refusing an OpenConsole.exe cache package that is not one folder name",
        ));
    }
    let dir = cache.join(package);
    if let Ok(meta) = std::fs::symlink_metadata(&dir)
        && !meta.is_dir()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "refusing to copy OpenConsole.exe through a non-directory cache entry",
        ));
    }
    let dest = dir.join("OpenConsole.exe");
    let want = read_openconsole_bounded(source)?;
    let up_to_date = std::fs::symlink_metadata(&dest).is_ok_and(|m| m.file_type().is_file())
        && read_openconsole_bounded(&dest).is_ok_and(|have| have == want);
    if !up_to_date {
        std::fs::create_dir_all(&dir)?;
        if !std::fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "refusing to copy OpenConsole.exe through a non-directory cache entry",
            ));
        }
        let pid = std::process::id();
        let mut opened = None;
        for attempt in 0..64u32 {
            let name = if attempt == 0 {
                format!("OpenConsole.exe.{pid}.partial")
            } else {
                let seq = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                format!("OpenConsole.exe.{pid}.{seq}.partial")
            };
            let candidate = dir.join(name);
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;
                opts.custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS);
            }
            match opts.open(&candidate) {
                Ok(file) => {
                    opened = Some((candidate, file));
                    break;
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists && attempt < 63 => {}
                Err(e) => return Err(e),
            }
        }
        let (partial, mut file) = opened.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::AlreadyExists,
                "could not allocate a unique OpenConsole.exe partial file",
            )
        })?;
        let wrote = file.write_all(&want);
        drop(file);
        if let Err(e) = wrote.and_then(|()| std::fs::rename(&partial, &dest)) {
            let _ = std::fs::remove_file(&partial);
            return Err(e);
        }
    }
    prune_other_packages(cache, package);
    Ok(dest)
}

/// Remove what [`copy_out`] wrote for every package but `keep`: in each other
/// entry of `cache` that is a real directory (a link is never followed), its
/// `OpenConsole.exe` and any `OpenConsole.exe.<pid>.partial`, then the
/// directory itself if that emptied it.
fn prune_other_packages(cache: &Path, keep: &str) {
    let Ok(entries) = std::fs::read_dir(cache) else {
        return;
    };
    for entry in entries.flatten() {
        // `DirEntry::file_type` does not follow links: a symlink or junction
        // to a directory is not a directory here.
        if entry.file_name() == keep || !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let dir = entry.path();
        let Ok(files) = std::fs::read_dir(&dir) else {
            continue;
        };
        for file in files.flatten() {
            let name = file.file_name();
            let name = name.to_string_lossy();
            let ours = name == "OpenConsole.exe"
                || (name.starts_with("OpenConsole.exe.") && name.ends_with(".partial"));
            if ours && file.file_type().is_ok_and(|t| t.is_file()) {
                let _ = std::fs::remove_file(file.path());
            }
        }
        let _ = std::fs::remove_dir(&dir);
    }
}

/// OpenConsole's arguments after its quoted path: exactly the five every build since winconpty's first has taken.
pub(crate) fn host_arguments(cols: i16, rows: i16, signal: usize, server: usize) -> String {
    format!(
        " --headless --width {cols} --height {rows} --signal 0x{signal:x} --server 0x{server:x}"
    )
}

#[cfg(test)]
#[path = "policy_tests.rs"]
mod tests;
