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

    /// The resolution of the current choice, made now if none was.
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

/// Copy `source` to `<cache>/<package>/OpenConsole.exe` unless an identical copy is already there, then prune the copies other packages left ([`prune_other_packages`]), so the cache holds one host however many Windows Terminal updates pass (each left about 1 MB behind before).
pub(crate) fn copy_out(source: &Path, cache: &Path, package: &str) -> io::Result<PathBuf> {
    let dir = cache.join(package);
    let dest = dir.join("OpenConsole.exe");
    let want = std::fs::read(source)?;
    if !std::fs::read(&dest).is_ok_and(|have| have == want) {
        std::fs::create_dir_all(&dir)?;
        let partial = dir.join(format!("OpenConsole.exe.{}.partial", std::process::id()));
        {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&partial)?;
            file.write_all(&want)?;
        }
        std::fs::rename(&partial, &dest)?;
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
mod tests {
    use super::*;

    /// A path under this host's filesystem root, so the same test reads the
    /// same on Windows and off it.
    fn rooted(parts: &[&str]) -> PathBuf {
        let mut path = PathBuf::from(if cfg!(windows) { r"C:\" } else { "/" });
        for part in parts {
            path.push(part);
        }
        path
    }

    fn exe(origin: Origin) -> OpenConsoleExe {
        OpenConsoleExe {
            path: rooted(&["oc", "OpenConsole.exe"]),
            origin,
            version: Some("1.24.2607.10001".to_string()),
        }
    }

    #[test]
    fn the_inbox_choice_consults_nothing_and_logs_nothing() {
        let mut host = HostState::new();
        let program = rooted(&["Windows", "System32", "cmd.exe"]);
        let none = host.host_for_spawn(
            &program,
            |_| panic!("the inbox host resolves nothing"),
            |_| panic!("the inbox host probes no file"),
        );
        assert_eq!(none, None);
        assert_eq!(host.take_unlogged(), None);
        let report = host.report(|_| panic!("still nothing"));
        assert_eq!(report.kind, ConsoleHostKind::Inbox);
        assert!(
            !report.fell_back,
            "the inbox host asked for is not a fallback"
        );
        host.set_choice(ConsoleHostChoice::from_config(Some("inbox")));
        assert_eq!(
            host.host_for_spawn(&program, |_| panic!("no"), |_| false),
            None
        );
        assert_eq!(
            host.report(|_| panic!("no")).line,
            "Windows console host (aterm.toml conpty = \"inbox\")"
        );
    }

    #[test]
    fn the_default_runs_the_bundled_host_or_says_once_that_there_is_none() {
        let dir = rooted(&["Programs", "aterm"]);
        assert_eq!(
            bundled_source(Some(&dir), |p| p == dir.join("OpenConsole.exe")),
            Ok(dir.join("OpenConsole.exe"))
        );
        assert_eq!(
            bundled_source(Some(&dir), |_| false),
            Err(NOT_BUNDLED.to_string())
        );
        assert_eq!(bundled_source(None, |_| true), Err(NOT_BUNDLED.to_string()));

        let program = rooted(&["pwsh", "pwsh.exe"]);
        let mut host = HostState::new();
        host.set_choice(ConsoleHostChoice::from_config(None));
        assert_eq!(
            host.host_for_spawn(&program, |_| Ok(exe(Origin::Bundled)), |_| false),
            Some(exe(Origin::Bundled))
        );
        assert_eq!(
            host.host_for_spawn(&program, |_| panic!("kept"), |_| false),
            Some(exe(Origin::Bundled))
        );
        assert_eq!(host.take_unlogged(), None);
        let found = host.report(|_| panic!("kept"));
        assert_eq!(found.kind, ConsoleHostKind::OpenConsole);
        assert!(!found.fell_back, "{found}");

        let mut bare = HostState::new();
        bare.set_choice(ConsoleHostChoice::Bundled);
        assert_eq!(
            bare.host_for_spawn(&program, |_| Err(NOT_BUNDLED.to_string()), |_| false),
            None
        );
        let note = bare.take_unlogged().expect("a missing host is noted");
        assert_eq!(note, format!("Windows console host; {NOT_BUNDLED}"));
        assert_eq!(
            bare.host_for_spawn(&program, |_| panic!("kept"), |_| false),
            None
        );
        assert_eq!(bare.take_unlogged(), None, "once");
        let report = bare.report(|_| panic!("kept"));
        assert_eq!(report.kind, ConsoleHostKind::Inbox);
        assert!(
            report.fell_back,
            "the default named a host it lacks: {report}"
        );
    }

    #[test]
    fn an_opted_in_choice_resolves_once_and_logs_a_failure_once() {
        let program = rooted(&["pwsh", "pwsh.exe"]);
        let mut host = HostState::new();
        host.set_choice(ConsoleHostChoice::Auto);
        let got = host.host_for_spawn(&program, |_| Ok(exe(Origin::Bundled)), |_| false);
        assert_eq!(got, Some(exe(Origin::Bundled)));
        let again = host.host_for_spawn(&program, |_| panic!("kept"), |_| false);
        assert_eq!(again, Some(exe(Origin::Bundled)));
        assert_eq!(host.take_unlogged(), None);

        let path = rooted(&["missing", "OpenConsole.exe"]);
        host.set_choice(ConsoleHostChoice::Path(path.clone()));
        let none = host.host_for_spawn(&program, |_| Err("it is not a file".into()), |_| false);
        assert_eq!(none, None);
        let note = host
            .take_unlogged()
            .expect("the failure is handed to the log");
        assert!(
            note.starts_with("Windows console host; it is not a file"),
            "{note}"
        );
        assert!(note.contains(&path.display().to_string()), "{note}");
        assert_eq!(host.take_unlogged(), None, "once");
        assert_eq!(
            host.host_for_spawn(&program, |_| panic!("the failure is kept"), |_| false),
            None
        );
        assert_eq!(host.take_unlogged(), None, "a second tab logs nothing new");
        let report = host.report(|_| panic!("kept"));
        assert!(report.fell_back, "{report}");
    }

    #[test]
    fn a_launch_failure_sticks_until_the_choice_changes() {
        let program = rooted(&["pwsh", "pwsh.exe"]);
        let mut host = HostState::new();
        host.set_choice(ConsoleHostChoice::Auto);
        let oc = host
            .host_for_spawn(&program, |_| Ok(exe(Origin::Bundled)), |_| false)
            .expect("resolved");
        host.note_launch_failure(&oc, "it exited with code 0x00000001 after 20 ms");
        let note = host.take_unlogged().expect("logged");
        assert_eq!(
            note,
            "Windows console host; OpenConsole did not start (it exited with code 0x00000001 \
             after 20 ms)"
        );
        assert_eq!(
            host.host_for_spawn(&program, |_| panic!("kept"), |_| false),
            None
        );
        host.note_launch_failure(&exe(Origin::Configured), "late");
        assert_eq!(host.take_unlogged(), None);
        host.set_choice(ConsoleHostChoice::Path(rooted(&["oc", "OpenConsole.exe"])));
        assert_eq!(
            host.host_for_spawn(&program, |_| Ok(exe(Origin::Configured)), |_| false),
            Some(exe(Origin::Configured))
        );
    }

    #[test]
    fn a_silent_host_costs_one_tab_until_it_is_silent_three_times_running() {
        let program = rooted(&["pwsh", "pwsh.exe"]);
        let mut host = HostState::new();
        host.set_choice(ConsoleHostChoice::Bundled);
        let oc = host
            .host_for_spawn(&program, |_| Ok(exe(Origin::Bundled)), |_| false)
            .expect("resolved");
        let why = "it wrote nothing in 250 ms";
        let silent = |host: &mut HostState| {
            host.note_silent_start(&oc, why);
            host.take_unlogged().expect("each fallback is logged")
        };
        for _ in 0..2 {
            for _ in 1..SILENT_STARTS {
                assert_eq!(
                    silent(&mut host),
                    "Windows console host for one tab; OpenConsole did not start (it wrote \
                     nothing in 250 ms) (the next tab tries it again)"
                );
                assert_eq!(
                    host.host_for_spawn(&program, |_| panic!("kept"), |_| false),
                    Some(oc.clone()),
                    "the next tab gets it again"
                );
            }
            host.note_started(&oc);
        }
        for _ in 1..SILENT_STARTS {
            silent(&mut host);
        }
        assert_eq!(
            silent(&mut host),
            "Windows console host; OpenConsole did not start (it wrote nothing in 250 ms)"
        );
        assert_eq!(
            host.host_for_spawn(&program, |_| panic!("kept"), |_| false),
            None,
            "three in a row stick"
        );
        host.set_choice(ConsoleHostChoice::Auto);
        let oc = host
            .host_for_spawn(&program, |_| Ok(exe(Origin::Bundled)), |_| false)
            .expect("resolved afresh");
        host.note_silent_start(&oc, why);
        assert_eq!(
            host.host_for_spawn(&program, |_| panic!("kept"), |_| false),
            Some(oc)
        );
    }

    #[test]
    fn auto_gives_posix_runtime_programs_openconsole_only_when_guarded() {
        let bash = rooted(&["Git", "usr", "bin", "bash.exe"]);
        let dll = rooted(&["Git", "usr", "bin", "msys-2.0.dll"]);
        let is_file = |p: &Path| p == dll;
        let mut host = HostState::new();
        for choice in [ConsoleHostChoice::Bundled, ConsoleHostChoice::Auto] {
            host.set_guarded(false);
            host.set_choice(choice.clone());
            assert_eq!(
                host.host_for_spawn(&bash, |_| Ok(exe(Origin::Bundled)), is_file),
                None,
                "{choice:?}, no guard client: Git Bash keeps conhost"
            );
            assert!(host.report(|_| panic!("kept")).line.contains("Git Bash"));
            host.set_guarded(true);
            assert_eq!(
                host.host_for_spawn(&bash, |_| panic!("kept"), is_file),
                Some(exe(Origin::Bundled)),
                "{choice:?}, guarded: Git Bash takes OpenConsole too"
            );
            assert!(!host.report(|_| panic!("kept")).line.contains("Git Bash"));
        }
        host.set_guarded(false);
        host.set_choice(ConsoleHostChoice::Path(rooted(&["oc", "OpenConsole.exe"])));
        assert_eq!(
            host.host_for_spawn(&bash, |_| Ok(exe(Origin::Configured)), is_file),
            Some(exe(Origin::Configured))
        );
    }

    #[test]
    fn the_host_arguments_are_the_five_stable_ones() {
        assert_eq!(
            host_arguments(120, 30, 0x2c4, 0x1a8),
            " --headless --width 120 --height 30 --signal 0x2c4 --server 0x1a8"
        );
    }

    #[test]
    fn auto_prefers_the_bundled_host_then_windows_terminal() {
        let dir = rooted(&["aterm"]);
        let bundled = auto_source(Some(&dir), |_| true, || panic!("not consulted"));
        assert_eq!(
            bundled,
            Ok(AutoSource::Bundled(dir.join("OpenConsole.exe")))
        );
        let source = rooted(&["wt", "OpenConsole.exe"]);
        let wt = auto_source(
            Some(&dir),
            |_| false,
            || Some(("WT_1.24_x64__x".to_string(), source.clone())),
        );
        assert_eq!(
            wt,
            Ok(AutoSource::WindowsTerminal {
                package: "WT_1.24_x64__x".to_string(),
                source: source.clone(),
            })
        );
        let none = auto_source(None, |_| false, || None).expect_err("nothing to launch");
        assert!(none.contains("no OpenConsole.exe beside aterm"), "{none}");
        assert!(none.contains("no Windows Terminal"), "{none}");
    }

    #[test]
    fn the_posix_runtimes_are_recognised_by_their_dll() {
        let files = [
            rooted(&["Git", "bin", "..", "usr", "bin", "msys-2.0.dll"]),
            rooted(&["Git", "usr", "bin", "msys-2.0.dll"]),
            rooted(&["msys64", "usr", "bin", "msys-2.0.dll"]),
            rooted(&["cygwin64", "bin", "cygwin1.dll"]),
        ];
        let is_file = |p: &Path| files.iter().any(|f| f == p);
        for runtime in [
            rooted(&["Git", "bin", "bash.exe"]),
            rooted(&["Git", "usr", "bin", "bash.exe"]),
            rooted(&["msys64", "usr", "bin", "zsh.exe"]),
            rooted(&["cygwin64", "bin", "bash.exe"]),
        ] {
            assert!(runs_a_posix_runtime(&runtime, is_file), "{runtime:?}");
        }
        for native in [
            rooted(&["PowerShell", "7", "pwsh.exe"]),
            rooted(&["Windows", "System32", "cmd.exe"]),
            rooted(&["tools", "bash.exe"]),
            PathBuf::from("bash.exe"),
        ] {
            assert!(!runs_a_posix_runtime(&native, is_file), "{native:?}");
        }
    }

    #[test]
    fn a_configured_path_must_be_an_absolute_file() {
        let relative =
            configured(Path::new("OpenConsole.exe"), |_| true, |_| None).expect_err("relative");
        assert!(relative.contains("not an absolute path"), "{relative}");
        let missing = rooted(&["definitely", "not", "here", "OpenConsole.exe"]);
        let err = configured(&missing, |_| false, |_| None).expect_err("missing");
        assert!(err.contains("is not a file"), "{err}");
        let here = configured(&missing, |_| true, |_| Some("1.2.3.4".into())).expect("a file");
        assert_eq!(here.origin, Origin::Configured);
        assert_eq!(here.version.as_deref(), Some("1.2.3.4"));
    }

    #[test]
    fn the_report_line_names_the_host_and_why() {
        let package = "Microsoft.WindowsTerminal_1.24.11911.0_x64__8wekyb3d8bbwe";
        let copied = OpenConsoleExe {
            path: rooted(&["s", "conpty", package, "OpenConsole.exe"]),
            origin: Origin::WindowsTerminal(package.to_string()),
            version: Some("1.24.2607.10001".to_string()),
        };
        let r = report(&ConsoleHostChoice::Auto, &Ok(copied.clone()));
        assert_eq!(r.kind, ConsoleHostKind::OpenConsole);
        assert!(!r.fell_back);
        assert_eq!(
            r.line,
            format!(
                "OpenConsole 1.24.2607.10001 (from Windows Terminal 1.24.11911.0, copied to {})",
                copied.path.display()
            )
        );
        let unguarded = report_guarded(&ConsoleHostChoice::Auto, &Ok(copied.clone()), false);
        assert_eq!(
            unguarded.line,
            format!(
                "OpenConsole 1.24.2607.10001 (from Windows Terminal 1.24.11911.0, copied to {}); \
                 Git Bash and other MSYS2/Cygwin programs keep the Windows console host (no \
                 aterm.exe beside aterm to guard their input after a resize)",
                copied.path.display()
            )
        );

        let inbox = report(&ConsoleHostChoice::Inbox, &Err(INBOX_REASON.to_string()));
        assert_eq!(inbox.kind, ConsoleHostKind::Inbox);
        assert!(!inbox.fell_back);
        assert_eq!(
            inbox.line,
            "Windows console host (aterm.toml conpty = \"inbox\")"
        );

        let bundled = OpenConsoleExe {
            path: rooted(&["aterm", "OpenConsole.exe"]),
            origin: Origin::Bundled,
            version: Some("1.25.2609.30003".to_string()),
        };
        let default = report(&ConsoleHostChoice::Bundled, &Ok(bundled.clone()));
        assert_eq!(default.kind, ConsoleHostKind::OpenConsole);
        assert_eq!(
            default.line,
            format!(
                "OpenConsole 1.25.2609.30003 (bundled beside aterm: {})",
                bundled.path.display()
            )
        );
        let none = report(&ConsoleHostChoice::Bundled, &Err(NOT_BUNDLED.to_string()));
        assert!(none.fell_back);
        assert_eq!(
            none.line,
            "Windows console host; no OpenConsole.exe beside aterm (reinstall aterm to restore \
             it; aterm.toml conpty = \"auto\" also tries Windows Terminal's)"
        );

        let path = rooted(&["oc", "OpenConsole.exe"]);
        let failed = report(
            &ConsoleHostChoice::Path(path.clone()),
            &Err(did_not_start("it exited with code 0xc0000142 after 20 ms")),
        );
        assert!(failed.fell_back);
        assert_eq!(
            failed.line,
            format!(
                "Windows console host; OpenConsole did not start (it exited with code 0xc0000142 \
                 after 20 ms) — fix or remove aterm.toml conpty = \"{}\"",
                path.display()
            )
        );
    }

    #[test]
    fn package_version_is_the_second_field_of_the_full_name() {
        assert_eq!(
            package_version("Microsoft.WindowsTerminal_1.24.11911.0_x64__8wekyb3d8bbwe"),
            "1.24.11911.0"
        );
        assert_eq!(package_version("odd"), "odd");
    }

    #[test]
    fn copy_out_keeps_one_copy_and_prunes_other_packages() {
        let root = std::env::temp_dir().join(format!("aterm-conpty-copy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("source.exe");
        std::fs::write(&source, b"MZ one").unwrap();
        let cache = root.join("conpty");
        let old = cache.join("Microsoft.WindowsTerminal_1.23.1.0_x64__8wekyb3d8bbwe");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("OpenConsole.exe"), b"MZ old").unwrap();
        std::fs::write(old.join("OpenConsole.exe.4242.partial"), b"MZ o").unwrap();
        let other = cache.join("Microsoft.WindowsTerminalPreview_1.25.1.0_x64__8wekyb3d8bbwe");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("OpenConsole.exe"), b"MZ preview").unwrap();
        std::fs::write(other.join("notes.txt"), b"not aterm's").unwrap();

        let pkg = "Microsoft.WindowsTerminal_1.24.11911.0_x64__8wekyb3d8bbwe";
        let dest = copy_out(&source, &cache, pkg).expect("copy");
        assert_eq!(dest, cache.join(pkg).join("OpenConsole.exe"));
        assert_eq!(std::fs::read(&dest).unwrap(), b"MZ one");
        assert!(
            !old.exists(),
            "an older package's copy and partial are pruned"
        );
        assert!(!other.join("OpenConsole.exe").exists(), "every other copy");
        assert_eq!(
            std::fs::read(other.join("notes.txt")).unwrap(),
            b"not aterm's",
            "a file copy_out never writes is never removed"
        );

        let stamp = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        std::fs::File::options()
            .write(true)
            .open(&dest)
            .unwrap()
            .set_modified(stamp)
            .unwrap();
        copy_out(&source, &cache, pkg).expect("second copy");
        assert_eq!(
            std::fs::metadata(&dest).unwrap().modified().unwrap(),
            stamp,
            "an identical copy is not rewritten"
        );
        std::fs::write(&source, b"MZ two").unwrap();
        copy_out(&source, &cache, pkg).expect("changed source");
        assert_eq!(std::fs::read(&dest).unwrap(), b"MZ two");
        let leftovers: Vec<_> = std::fs::read_dir(cache.join(pkg))
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(leftovers.len(), 1, "no partial file stays: {leftovers:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn the_prune_never_follows_a_link() {
        let root = std::env::temp_dir().join(format!("aterm-conpty-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (cache, outside) = (root.join("conpty"), root.join("outside"));
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("OpenConsole.exe"), b"MZ elsewhere").unwrap();
        std::os::unix::fs::symlink(&outside, cache.join("linked")).unwrap();
        let source = root.join("source.exe");
        std::fs::write(&source, b"MZ one").unwrap();
        copy_out(&source, &cache, "current").expect("copy");
        assert!(outside.join("OpenConsole.exe").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_pruned_windows_terminal_copy_is_resolved_afresh() {
        let program = rooted(&["pwsh", "pwsh.exe"]);
        let copied = OpenConsoleExe {
            path: rooted(&["s", "conpty", "WT_1.23", "OpenConsole.exe"]),
            origin: Origin::WindowsTerminal("WT_1.23".to_string()),
            version: None,
        };
        let fresh = OpenConsoleExe {
            path: rooted(&["s", "conpty", "WT_1.24", "OpenConsole.exe"]),
            origin: Origin::WindowsTerminal("WT_1.24".to_string()),
            version: None,
        };
        let both = |p: &Path| p == copied.path || p == fresh.path;
        let mut host = HostState::new();
        host.set_choice(ConsoleHostChoice::Auto);
        let first = host.host_for_spawn(&program, |_| Ok(copied.clone()), both);
        assert_eq!(first, Some(copied.clone()));
        let kept = host.host_for_spawn(&program, |_| panic!("still there"), both);
        assert_eq!(kept, Some(copied.clone()));
        let pruned = |p: &Path| p == fresh.path;
        let again = host.host_for_spawn(&program, |_| Ok(fresh.clone()), pruned);
        assert_eq!(again, Some(fresh));
        assert_eq!(host.take_unlogged(), None, "nothing failed");

        let mut bundled = HostState::new();
        bundled.set_choice(ConsoleHostChoice::Auto);
        let _ = bundled.host_for_spawn(&program, |_| Ok(exe(Origin::Bundled)), |_| false);
        assert_eq!(
            bundled.host_for_spawn(&program, |_| panic!("kept"), |_| false),
            Some(exe(Origin::Bundled))
        );
    }
}
