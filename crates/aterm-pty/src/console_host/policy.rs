// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The console-host POLICY, with every Windows call injected so it runs (and is
//! tested) on any host: a choice resolved once and kept, the once-per-failure
//! fallback note, where `auto` finds an OpenConsole, which programs keep conhost
//! under `auto`, the copy out of Windows Terminal's package, and the report
//! lines. The Windows backend (`windows/openconsole.rs`) holds the process's one
//! [`HostState`] and passes it the real file, package and version lookups.

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
pub(crate) const INBOX_REASON: &str = "aterm.toml conpty is \"inbox\" or unset";

/// A process's console-host state: the frontend's choice, its resolution (made
/// on first use after the choice was set, then kept: a host that failed is not
/// retried until the choice changes), and a fallback not yet logged.
#[derive(Debug)]
pub(crate) struct HostState {
    choice: ConsoleHostChoice,
    resolved: Option<Result<OpenConsoleExe, String>>,
    unlogged: Option<String>,
}

impl HostState {
    /// [`ConsoleHostChoice::Inbox`]: an embedder that never states a choice (the
    /// CLI's passthrough session, the backend's own tests) and a window whose
    /// aterm.toml has no `conpty` keep the host every earlier build used.
    pub(crate) const fn new() -> Self {
        Self {
            choice: ConsoleHostChoice::Inbox,
            resolved: None,
            unlogged: None,
        }
    }

    /// Set the choice every LATER spawn uses. A changed choice is resolved
    /// afresh on the next use; the same choice keeps its resolution, a failure
    /// included.
    pub(crate) fn set_choice(&mut self, choice: ConsoleHostChoice) {
        if self.choice != choice {
            self.choice = choice;
            self.resolved = None;
        }
    }

    /// The fallback to the Windows console host not reported yet: at most once
    /// per failure, for the frontend's log (this crate has none).
    pub(crate) fn take_unlogged(&mut self) -> Option<String> {
        self.unlogged.take()
    }

    /// The resolution of the current choice, made now if none was. The first
    /// resolution of an opted-in choice that found no OpenConsole is handed to
    /// the log.
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
        report(&self.choice, &resolved)
    }

    /// The OpenConsole a spawn of `program` should launch now, `None` for the
    /// inbox host. Under `auto` a program of the MSYS2/Cygwin runtime keeps the
    /// inbox host ([`runs_a_posix_runtime`]); a configured path is the user's
    /// word for every tab.
    pub(crate) fn host_for_spawn(
        &mut self,
        program: &Path,
        resolve: impl FnOnce(&ConsoleHostChoice) -> Result<OpenConsoleExe, String>,
        is_file: impl Fn(&Path) -> bool,
    ) -> Option<OpenConsoleExe> {
        let exe = self.resolution(resolve).as_ref().ok().cloned();
        if self.choice == ConsoleHostChoice::Auto && runs_a_posix_runtime(program, is_file) {
            return None;
        }
        exe
    }

    /// `exe` failed to start for a spawn: every later spawn under this choice
    /// takes the inbox host, and the log hears why once.
    pub(crate) fn note_launch_failure(&mut self, exe: &OpenConsoleExe, why: &str) {
        let still_this = matches!(&self.resolved, Some(Ok(current)) if current == exe);
        if still_this {
            let failed = Err(did_not_start(why));
            self.unlogged = Some(report(&self.choice, &failed).line);
            self.resolved = Some(failed);
        }
    }
}

/// `choice` resolved by `resolve`, except [`ConsoleHostChoice::Inbox`], which
/// resolves to the inbox host without consulting anything: the default looks
/// for no file and copies nothing.
pub(crate) fn resolve_choice(
    choice: &ConsoleHostChoice,
    resolve: impl FnOnce(&ConsoleHostChoice) -> Result<OpenConsoleExe, String>,
) -> Result<OpenConsoleExe, String> {
    match choice {
        ConsoleHostChoice::Inbox => Err(INBOX_REASON.to_string()),
        ConsoleHostChoice::Auto | ConsoleHostChoice::Path(_) => resolve(choice),
    }
}

pub(crate) fn did_not_start(why: &str) -> String {
    format!("OpenConsole did not start ({why})")
}

/// The report line for a resolution: the host, then where it came from.
pub(crate) fn report(
    choice: &ConsoleHostChoice,
    resolved: &Result<OpenConsoleExe, String>,
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
            let except = if *choice == ConsoleHostChoice::Auto {
                "; Git Bash and other MSYS2/Cygwin programs keep the Windows console host"
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
                    "Windows console host (the default; aterm.toml conpty = \"auto\" tries \
                     OpenConsole)"
                        .to_string(),
                    false,
                ),
                ConsoleHostChoice::Auto => (format!("Windows console host; {why}"), true),
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

/// aterm.toml's own path: absolute, and a file. A relative path would resolve
/// against whatever directory the window was started in, so it is refused.
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

/// The first half of `auto`: the bundled file wins, then the first Windows
/// Terminal package holding one, else the reason there is none.
pub(crate) fn auto_source(
    exe_dir: Option<&Path>,
    is_file: impl Fn(&Path) -> bool,
    windows_terminal: impl FnOnce() -> Option<(String, PathBuf)>,
) -> Result<AutoSource, String> {
    if let Some(bundled) = exe_dir.map(|dir| dir.join("OpenConsole.exe"))
        && is_file(&bundled)
    {
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

/// Whether `program` runs on the MSYS2 or Cygwin runtime (Git Bash's
/// `bin\bash.exe` launcher, the `usr\bin` programs beside `msys-2.0.dll`, a
/// Cygwin `bin` beside `cygwin1.dll`), which `auto` keeps on the Windows
/// console host.
///
/// WHY, as the 2026-09-29 snapshot recorded it (Git for Windows bash 5.3 on the
/// MSYS2 runtime 3.6, a resize and then a paste or a typed line, 12 times per
/// run): the runtime drops the first input record after a size change (the
/// account is in the Windows backend's `resize_guard`), and the guard record
/// that absorbs the drop reaches it only through conhost. The runtime reads in
/// VT-input mode, and for such a reader OpenConsole delivered text alone:
/// character key-downs, no key-ups, and NOTHING for the guard's Shift key-up (a
/// record-dump reader in ENABLE_VIRTUAL_TERMINAL_INPUT saw `a x y q` where
/// conhost gave it the key-ups and the guard). Git Bash under OpenConsole kept
/// 9, 5 and 8 of 12 inputs whole with the one guard record, 8 and 6 with two, 6
/// with six; under conhost 12 of 12, twice. No input that a VT reader would
/// ignore survived OpenConsole as a record, so the only whole fix is the host
/// that delivers the guard.
pub(crate) fn runs_a_posix_runtime(program: &Path, is_file: impl Fn(&Path) -> bool) -> bool {
    let Some(dir) = program.parent() else {
        return false;
    };
    [
        dir.join("msys-2.0.dll"),
        dir.join("cygwin1.dll"),
        // Git for Windows' `bin\bash.exe` is a native launcher for
        // `usr\bin\bash.exe`, which runs on the runtime.
        dir.join("..").join("usr").join("bin").join("msys-2.0.dll"),
    ]
    .iter()
    .any(|dll| is_file(dll))
}

/// Copy `source` to `<cache>/<package>/OpenConsole.exe` unless an identical
/// copy is already there. Other packages' cache entries are preserved. A failed
/// write or publish leaves its exclusive partial in place and falls back; no
/// error path removes a pathname whose identity could have changed.
pub(crate) fn copy_out(source: &Path, cache: &Path, package: &str) -> io::Result<PathBuf> {
    let dir = cache.join(package);
    let dest = dir.join("OpenConsole.exe");
    let want = std::fs::read(source)?;
    if std::fs::read(&dest).is_ok_and(|have| have == want) {
        return Ok(dest);
    }
    std::fs::create_dir_all(&dir)?;
    // Written beside the destination and renamed over it, so a concurrent
    // aterm never launches a half-written file.
    let partial = dir.join(format!("OpenConsole.exe.{}.partial", std::process::id()));
    {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&partial)?;
        file.write_all(&want)?;
    }
    std::fs::rename(&partial, &dest)?;
    Ok(dest)
}

/// OpenConsole's arguments after its quoted path: exactly the five every build
/// since winconpty's first has taken. Anything more is a risk with no gain: the
/// snapshot recorded 1.24 REJECTING the retired `--resizeQuirk`, `--win32input`,
/// `--vtmode`, `--passthrough` and `--ambiguousIsWide` (it exited in 19.8 ms and
/// the shell died 0xC0000142 having printed nothing), and grapheme text
/// measurement is already its default.
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

    // The default does nothing: an unset choice resolves to the inbox host
    // without looking for a file, copying one, or noting anything for the log.
    #[test]
    fn the_default_choice_consults_nothing_and_logs_nothing() {
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
        assert!(!report.fell_back, "the default is not a fallback");
        // Setting it again to `inbox` keeps it so.
        host.set_choice(ConsoleHostChoice::from_config(Some("inbox")));
        assert_eq!(
            host.host_for_spawn(&program, |_| panic!("no"), |_| false),
            None
        );
    }

    // An opted-in choice resolves once: the OpenConsole is handed to every
    // spawn, and a failed resolution is handed to the log once, never per tab.
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

    // A host that resolved but failed to START sticks as a failure for its
    // choice, logged once; a stale failure of an older resolution is ignored.
    #[test]
    fn a_launch_failure_sticks_until_the_choice_changes() {
        let program = rooted(&["pwsh", "pwsh.exe"]);
        let mut host = HostState::new();
        host.set_choice(ConsoleHostChoice::Auto);
        let oc = host
            .host_for_spawn(&program, |_| Ok(exe(Origin::Bundled)), |_| false)
            .expect("resolved");
        host.note_launch_failure(&oc, "it wrote nothing in 250 ms");
        let note = host.take_unlogged().expect("logged");
        assert_eq!(
            note,
            "Windows console host; OpenConsole did not start (it wrote nothing in 250 ms)"
        );
        assert_eq!(
            host.host_for_spawn(&program, |_| panic!("kept"), |_| false),
            None
        );
        // A failure reported for an OpenConsole this choice no longer resolves
        // to changes nothing.
        host.note_launch_failure(&exe(Origin::Configured), "late");
        assert_eq!(host.take_unlogged(), None);
        // A changed choice resolves afresh.
        host.set_choice(ConsoleHostChoice::Path(rooted(&["oc", "OpenConsole.exe"])));
        assert_eq!(
            host.host_for_spawn(&program, |_| Ok(exe(Origin::Configured)), |_| false),
            Some(exe(Origin::Configured))
        );
    }

    // `auto` keeps an MSYS2/Cygwin program on the inbox host; a configured path
    // is the user's word for every tab.
    #[test]
    fn auto_keeps_posix_runtime_programs_on_the_inbox_host() {
        let bash = rooted(&["Git", "usr", "bin", "bash.exe"]);
        let dll = rooted(&["Git", "usr", "bin", "msys-2.0.dll"]);
        let is_file = |p: &Path| p == dll;
        let mut host = HostState::new();
        host.set_choice(ConsoleHostChoice::Auto);
        assert_eq!(
            host.host_for_spawn(&bash, |_| Ok(exe(Origin::Bundled)), is_file),
            None
        );
        host.set_choice(ConsoleHostChoice::Path(rooted(&["oc", "OpenConsole.exe"])));
        assert_eq!(
            host.host_for_spawn(&bash, |_| Ok(exe(Origin::Configured)), is_file),
            Some(exe(Origin::Configured))
        );
    }

    // The command line's tail is EXACTLY the five stable arguments, the two
    // handles in hex: 1.24 rejects every retired flag.
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

    // Git Bash's launcher, the runtime's own `usr/bin`, MSYS2 and Cygwin are
    // runtime programs; pwsh, cmd and a lone bash.exe with no runtime beside it
    // are not.
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

    // The report says the state, then (for an opted-in host that failed) the
    // one action.
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
                "OpenConsole 1.24.2607.10001 (from Windows Terminal 1.24.11911.0, copied to {}); \
                 Git Bash and other MSYS2/Cygwin programs keep the Windows console host",
                copied.path.display()
            )
        );

        let inbox = report(&ConsoleHostChoice::Inbox, &Err(INBOX_REASON.to_string()));
        assert_eq!(inbox.kind, ConsoleHostKind::Inbox);
        assert!(!inbox.fell_back);
        assert_eq!(
            inbox.line,
            "Windows console host (the default; aterm.toml conpty = \"auto\" tries OpenConsole)"
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

    // The copy lands once, is not rewritten while identical (its mtime, set to
    // a known instant, stays), and preserves other Windows Terminal versions.
    #[test]
    fn copy_out_keeps_one_copy_per_current_package() {
        let root = std::env::temp_dir().join(format!("aterm-conpty-copy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("source.exe");
        std::fs::write(&source, b"MZ one").unwrap();
        let cache = root.join("conpty");
        let old = cache.join("Microsoft.WindowsTerminal_1.23.1.0_x64__8wekyb3d8bbwe");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("OpenConsole.exe"), b"MZ old").unwrap();

        let pkg = "Microsoft.WindowsTerminal_1.24.11911.0_x64__8wekyb3d8bbwe";
        let dest = copy_out(&source, &cache, pkg).expect("copy");
        assert_eq!(dest, cache.join(pkg).join("OpenConsole.exe"));
        assert_eq!(std::fs::read(&dest).unwrap(), b"MZ one");
        assert_eq!(
            std::fs::read(old.join("OpenConsole.exe")).unwrap(),
            b"MZ old",
            "another package's existing copy is preserved"
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
}
