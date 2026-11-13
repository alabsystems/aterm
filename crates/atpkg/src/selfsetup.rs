// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Account shell setup and ownership-checked self-uninstall verbs.

use std::borrow::Cow;
use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
#[path = "selfsetup_app_swap.rs"]
mod app_swap;
#[cfg(target_os = "macos")]
pub use app_swap::verb as app_swap_verb;

/// Start marker of the managed shell-profile block.
pub const PATH_BLOCK_START: &str = "# >>> aterm ALab toolset (managed by install.sh) >>>";
/// End marker of the managed shell-profile block.
pub const PATH_BLOCK_END: &str = "# <<< aterm ALab toolset <<<";
/// Marker line written at the top of the managed Linux desktop entry.
pub const DESKTOP_MARKER: &str =
    "# Managed by aterm install.sh — its uninstall removes only an entry carrying this line.";
const SHIPPED_MAN_PAGES: [&str; 3] = ["aterm.1", "aterm-ctl.1", "aterm-gui.1"];
const ICON_SIZES: [&str; 5] = ["32x32", "64x64", "128x128", "256x256", "512x512"];
/// Embedded hicolor app icons for [`ICON_SIZES`].
#[cfg(target_os = "linux")]
pub const HICOLOR_ICONS: [(&str, &[u8]); 5] = [
    (
        "32x32",
        include_bytes!("../../../assets/linux/icons/hicolor/32x32/apps/aterm.png"),
    ),
    (
        "64x64",
        include_bytes!("../../../assets/linux/icons/hicolor/64x64/apps/aterm.png"),
    ),
    (
        "128x128",
        include_bytes!("../../../assets/linux/icons/hicolor/128x128/apps/aterm.png"),
    ),
    (
        "256x256",
        include_bytes!("../../../assets/linux/icons/hicolor/256x256/apps/aterm.png"),
    ),
    (
        "512x512",
        include_bytes!("../../../assets/linux/icons/hicolor/512x512/apps/aterm.png"),
    ),
];
const CARRIED_MAN_PAGES: [(&str, &str, &[u8]); 1] =
    [("1", "aterm.1", include_bytes!("../../../man/aterm.1"))];
const BUNDLE_ID: &str = "com.aterm.aterm";
const CHILD_HANG: Duration = Duration::from_secs(60);

/// Account installation paths used by `shell-setup` and `self-uninstall`.
#[derive(Clone, Debug)]
pub struct Places {
    /// `$HOME`.
    pub home: PathBuf,
    /// `$XDG_DATA_HOME`, else `~/.local/share`.
    pub xdg_data: PathBuf,
    /// `$XDG_CONFIG_HOME`, else `~/.config`.
    pub xdg_config: PathBuf,
    /// The folder zsh reads its `.zshrc` from: `$ZDOTDIR`, else `$HOME` ([`crate::zdotdir`]).
    pub zdotdir: crate::zdotdir::ZshDir,
    /// `ATERM_INSTALL_DIR`: the one app location of a scoped run.
    pub install_dir: Option<PathBuf>,
    /// `ATERM_BIN_DIR`, else `~/.local/bin`.
    pub bin_dir: PathBuf,
    /// `ATERM_STORE_DIR`, else `~/.local/lib/aterm/bin`.
    pub store_dir: PathBuf,
    /// `ATERM_MAN_DIR`, else `~/.local/share/man`.
    pub man_root: PathBuf,
}

impl Places {
    /// Default installation paths under `home`.
    #[must_use]
    pub fn defaults(home: &Path, xdg_data: Option<&Path>, xdg_config: Option<&Path>) -> Self {
        Self {
            home: home.to_path_buf(),
            xdg_data: xdg_data.map_or_else(|| home.join(".local/share"), Path::to_path_buf),
            xdg_config: xdg_config.map_or_else(|| home.join(".config"), Path::to_path_buf),
            zdotdir: crate::zdotdir::ZshDir::Home,
            install_dir: None,
            bin_dir: home.join(".local/bin"),
            store_dir: home.join(".local/lib/aterm/bin"),
            man_root: home.join(".local/share/man"),
        }
    }

    /// zsh's `.zshrc`, in [`Places::zdotdir`]; `None` while that folder is unknown.
    #[must_use]
    pub fn zshrc(&self) -> Option<PathBuf> {
        self.zdotdir.dir(&self.home).map(|dir| dir.join(".zshrc"))
    }

    fn bin_dirs(&self) -> Vec<PathBuf> {
        let local = self.home.join(".local/bin");
        let mut dirs = vec![self.bin_dir.clone()];
        if self.bin_dir != local {
            dirs.push(local);
        }
        dirs
    }

    fn app_dirs(&self) -> Vec<PathBuf> {
        self.install_dir.clone().map_or_else(
            || {
                vec![
                    PathBuf::from("/Applications"),
                    self.home.join("Applications"),
                ]
            },
            |dir| vec![dir],
        )
    }

    fn support(&self) -> PathBuf {
        self.home.join("Library/Application Support/aterm")
    }
}

/// Shell environment snapshot for [`shell_setup`].
#[derive(Clone, Debug, Default)]
pub struct ShellEnv {
    /// The account's login shell ([`crate::hooks::login_shell`], [`rc_target`]).
    pub shell: Option<String>,
    /// `$PATH`.
    pub path: Option<OsString>,
    /// `$MANPATH`.
    pub manpath: Option<String>,
}

impl ShellEnv {
    fn of_process() -> Self {
        Self {
            shell: crate::hooks::login_shell().map(|shell| shell.to_string_lossy().into_owned()),
            path: std::env::var_os("PATH"),
            manpath: std::env::var("MANPATH").ok(),
        }
    }

    fn shell_name(&self) -> &str {
        self.shell
            .as_deref()
            .map_or("", |s| s.rsplit('/').next().unwrap_or(s))
    }

    fn on_path(&self, name: &str) -> Option<PathBuf> {
        let path = self.path.as_ref()?;
        std::env::split_paths(path)
            .map(|dir| dir.join(name))
            .find(|p| p.is_file())
    }
}

fn bundle_identifier(app: &Path) -> Option<String> {
    aterm_containment::consent::bundle_id_of(app, &[])
}

fn bundle_is_ours(app: &Path) -> bool {
    bundle_identifier(app).as_deref() == Some(BUNDLE_ID)
}

/// Whether `app` may be an aterm: only a readable identifier naming another bundle proves it is
/// not. The test for what an uninstall keeps, where [`bundle_is_ours`] is the test for what it
/// deletes: an aterm whose `Info.plist` is lost or garbled still runs on the account's store.
fn bundle_may_be_aterm(app: &Path) -> bool {
    bundle_identifier(app).is_none_or(|id| id == BUNDLE_ID)
}

fn link_target(path: &Path) -> Option<String> {
    std::fs::read_link(path)
        .ok()
        .map(|t| t.to_string_lossy().into_owned())
}

fn bin_entry_is_ours(path: &Path, store: &Path) -> bool {
    link_target(path).is_some_and(|t| {
        t.contains("/aterm.app/Contents/MacOS/")
            || Path::new(&t).starts_with(store) && Path::new(&t) != store
    })
}

fn link_bundle(link: &Path) -> Option<PathBuf> {
    let t = link_target(link)?;
    let t = if t.starts_with('/') {
        t
    } else {
        format!("{}/{t}", link.parent()?.display())
    };
    let at = t.find("/aterm.app/Contents/MacOS/")?;
    Some(PathBuf::from(format!("{}/aterm.app", &t[..at])))
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(x), Ok(y)) => x.dev() == y.dev() && x.ino() == y.ino(),
        _ => false,
    }
}

fn dir_split(path: &Path) -> Option<(PathBuf, PathBuf)> {
    let mut path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    loop {
        let mut head = PathBuf::from("/");
        let mut comps = path.components().peekable();
        while let Some(c) = comps.peek() {
            let next = match c {
                Component::RootDir | Component::Prefix(_) => {
                    comps.next();
                    continue;
                }
                Component::CurDir => {
                    comps.next();
                    continue;
                }
                other => head.join(other.as_os_str()),
            };
            if !next.is_dir() {
                break;
            }
            head = next;
            comps.next();
        }
        let head = std::fs::canonicalize(&head).ok()?;
        let mut tail: Vec<OsString> = Vec::new();
        let mut undid = false;
        let mut base = head.clone();
        for c in comps {
            match c {
                Component::Normal(n) => tail.push(n.to_os_string()),
                Component::ParentDir => {
                    undid = true;
                    if tail.pop().is_none() {
                        base = base
                            .parent()
                            .map_or_else(|| base.clone(), Path::to_path_buf);
                    }
                }
                Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
            }
        }
        if undid {
            path = tail.iter().fold(base, |p, t| p.join(t));
            continue;
        }
        return Some((head, tail.iter().collect()));
    }
}

fn dir_within(a: &Path, b: &Path) -> bool {
    let (Some((mut ah, at)), Some((bh, bt))) = (dir_split(a), dir_split(b)) else {
        return false;
    };
    if bt.as_os_str().is_empty() {
        loop {
            if same_file(&ah, &bh) {
                return true;
            }
            match ah.parent() {
                Some(p) => ah = p.to_path_buf(),
                None => return false,
            }
        }
    }
    same_file(&ah, &bh) && at.starts_with(&bt)
}

fn first_line_bounded(exe: &Path, args: &[impl AsRef<OsStr>], within: Duration) -> String {
    let mut cmd = aterm_uds::spawnfd::command(exe);
    cmd.args(args);
    let deadline = Instant::now()
        .checked_add(within)
        .unwrap_or_else(|| Instant::now() + Duration::from_secs(365 * 86_400));
    aterm_uds::deadline::output_until(&mut cmd, deadline)
        .map(|out| {
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .next()
                .unwrap_or_default()
                .to_string()
        })
        .unwrap_or_default()
}

fn answers_as_aterm(path: &Path, run_as: RunAs) -> bool {
    let line = match run_as {
        _ if !is_executable_file(path) => return false,
        RunAs::Me => first_line_bounded(path, &["--version"], CHILD_HANG),
        RunAs::Account(uid) => first_line_bounded(
            Path::new(SUDO),
            &as_account_argv(uid, path, &["--version"]),
            CHILD_HANG,
        ),
        RunAs::Nobody => return false,
    };
    line.starts_with("aterm ")
}

fn is_executable_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Why `program` may still be in macOS's check of its first exec (which crashes when the file
/// goes mid-check), or `None` once a `--version` run of it ended without our kill (at `hang`, or
/// on a failed wait) or the kernel refused to exec it ([`never_execs`]), which it does before
/// that check. An exec waits on the check, so any end but our kill comes after it. Through sudo,
/// a shell runs the program and says when the run ends ([`ENDED`], [`shell_end_unproven`]).
fn first_exec_unproven(program: &Path, run_as: RunAs, hang: Duration) -> Option<String> {
    const UNASKED: &str = "was not asked, and only its answer shows macOS is done checking its \
                           first start";
    let mut cmd = match run_as {
        RunAs::Me => aterm_uds::spawnfd::command(program),
        RunAs::Account(uid) => {
            if let Some(rc) = sudo_refuses(uid) {
                return Some(format!(
                    "{UNASKED}: sudo could not run it as uid {uid} (exit {rc})"
                ));
            }
            let mut cmd = aterm_uds::spawnfd::command(Path::new(SUDO));
            // sudo's and the shell's lines in the words [`says_never_execs`] reads.
            cmd.env("LC_ALL", "C")
                .args(as_account_argv(
                    uid,
                    Path::new(SH[0]),
                    &[&SH[1..], &["-c", ENDED]].concat(),
                ))
                .arg(program);
            cmd
        }
        RunAs::Nobody => return Some(format!("{UNASKED}: {NOT_AS_ROOT}")),
    };
    cmd.arg("--version");
    let deadline = Instant::now()
        .checked_add(hang)
        .unwrap_or_else(|| Instant::now() + Duration::from_secs(365 * 86_400));
    ran_unproven(
        program,
        run_as,
        aterm_uds::deadline::output_until(&mut cmd, deadline),
        hang,
    )
}

/// What [`first_exec_unproven`]'s `--version` run of `program` as `run_as` came to.
fn ran_unproven(
    program: &Path,
    run_as: RunAs,
    ran: Result<std::process::Output, aterm_uds::deadline::RunError>,
    hang: Duration,
) -> Option<String> {
    let why = match ran {
        Ok(out) if matches!(run_as, RunAs::Account(_)) => shell_end_unproven(&out, program)?,
        Ok(_) => return None,
        Err(aterm_uds::deadline::RunError::Spawn(e)) if never_execs(&e) => return None,
        Err(aterm_uds::deadline::RunError::TimedOut) => format!("did not answer within {hang:?}"),
        Err(e) => e.to_string(),
    };
    Some(format!(
        "{why}, so macOS may still be checking its first start, and removing the bundle now can \
         strand every program start then waiting on that check — uninstall again in {} minutes, \
         or after a restart",
        aterm_uds::deadline::FIRST_EXEC_HANG.as_secs() / 60
    ))
}

/// Whether a run as `run_as` that ended with `code` may have run nothing: through sudo, which
/// exits 1 on its own refusal and when it cannot exec the program, and hands a file the kernel
/// refuses as no program (ENOEXEC) to `/bin/sh`, which exits 126 or 127.
fn sudo_may_not_have_run(run_as: RunAs, code: Option<i32>) -> bool {
    matches!(run_as, RunAs::Account(_)) && matches!(code, Some(1 | 126 | 127))
}

/// Why `out`, sudo's run of [`ENDED`] asking `program`, does not show the program ran: `None`
/// once the program printed anything or the shell wrote `ended <status>`. With no such line, the
/// end is sudo's (its refusal, or a shell it cannot exec) or the shell's (a failed fork). An
/// `ended 126` or `ended 127` with nothing printed may be the shell's own failure to exec the
/// program (denied, gone, a folder), which proves nothing unless it names a kernel refusal
/// ([`says_never_execs`]).
fn shell_end_unproven(out: &std::process::Output, program: &Path) -> Option<String> {
    let stdout = String::from_utf8_lossy(&out.stdout);
    let (printed, ended) = match stdout
        .strip_suffix('\n')
        .and_then(|s| s.rsplit_once("ended "))
        .and_then(|(p, c)| Some((p, c.replace('+', "?").parse::<i32>().ok()?)))
    {
        Some((printed, code)) => (printed, Some(code)),
        None => (&*stdout, None),
    };
    if !printed.is_empty() {
        return None;
    }
    let said = String::from_utf8_lossy(&out.stderr);
    let saying = match said.lines().map(str::trim).rfind(|line| !line.is_empty()) {
        Some(line) => format!("saying `{line}`"),
        None => "with nothing printed".to_owned(),
    };
    match ended {
        None => Some(format!(
            "may not have started: sudo ended {} with nothing on standard output, {saying}",
            out.status
        )),
        Some(code @ (126 | 127)) if !says_never_execs(&out.stderr, program) => Some(format!(
            "may not have started: the shell sudo ran to ask it could not run it (exit {code}), \
             {saying}"
        )),
        Some(_) => None,
    }
}

/// Whether a run of `program` as `run_as` that ended with `code`, `stderr` its words, is the
/// kernel's refusal to exec it that sudo names ([`says_never_execs`]): it never ran, and no
/// check of macOS's holds it.
fn sudo_refused_exec(run_as: RunAs, code: Option<i32>, stderr: &[u8], program: &Path) -> bool {
    sudo_may_not_have_run(run_as, code) && says_never_execs(stderr, program)
}

/// Whether `stderr` says the kernel refused to exec `program` ([`never_execs`]), in sudo's words
/// (`unable to execute <program>: <why>`) or a shell's (`<name>: <program>: <why>`, or `cannot
/// execute binary file` for ENOEXEC). A program that printed such a line itself ran.
fn says_never_execs(stderr: &[u8], program: &Path) -> bool {
    let text = String::from_utf8_lossy(stderr);
    let program = program.display();
    let mut whys: Vec<String> = EXEC_REFUSALS.iter().map(|&code| strerror(code)).collect();
    let binary = "cannot execute binary file";
    whys.extend([
        binary.to_owned(),
        format!("{binary}: {}", strerror(libc::ENOEXEC)),
    ]);
    let by = [
        format!("unable to execute {program}: "),
        format!(": {program}: "),
    ];
    text.lines().map(str::trim_end).any(|line| {
        by.iter()
            .any(|by| whys.iter().any(|why| line.ends_with(&format!("{by}{why}"))))
    })
}

/// `code`'s words as the C library says them, which sudo and the shell print.
fn strerror(code: i32) -> String {
    let why = std::io::Error::from_raw_os_error(code).to_string();
    why.split(" (os error").next().unwrap_or(&why).to_owned()
}

/// The programs in `app` aterm's tools may have started: each file in `Contents/MacOS` but a
/// link, since an older toolset's `aterm-cli` starts its siblings (`atpkg` for `pkg`).
///
/// # Errors
/// The directory, an entry in it or an entry's metadata cannot be read. A missing directory
/// holds no programs; any other unread inventory proves nothing about their first starts.
fn bundle_programs(app: &Path) -> Result<Vec<PathBuf>, String> {
    let dir = app.join("Contents/MacOS");
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("cannot read {}: {e}", dir.display())),
    };
    let mut programs = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("cannot read an entry in {}: {e}", dir.display()))?;
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        if metadata.is_file() && metadata.permissions().mode() & 0o111 != 0 {
            programs.push(path);
        }
    }
    programs.sort();
    Ok(programs)
}

/// The errors of an exec the kernel refuses as no program it can run here (its format or CPU
/// type), which it does before macOS's check, so no exec of it waits on one.
#[cfg(target_vendor = "apple")]
const EXEC_REFUSALS: [i32; 4] = [
    libc::ENOEXEC,
    libc::EBADARCH,
    libc::EBADEXEC,
    libc::EBADMACHO,
];
/// See the Apple twin.
#[cfg(not(target_vendor = "apple"))]
const EXEC_REFUSALS: [i32; 1] = [libc::ENOEXEC];

/// Whether `e`, a spawn's error, is one of [`EXEC_REFUSALS`].
fn never_execs(e: &std::io::Error) -> bool {
    e.raw_os_error()
        .is_some_and(|code| EXEC_REFUSALS.contains(&code))
}

/// Why `app` stays: the first of its programs ([`bundle_programs`]) but `proven`, one this sweep
/// already saw exit, whose answer does not show macOS is done checking it
/// ([`first_exec_unproven`]). A dry run asks nothing and says what it would ask.
fn bundle_unproven(
    s: &mut Sweep<'_>,
    p: &Places,
    app: &Path,
    proven: Option<&Path>,
    hang: Duration,
) -> Option<String> {
    let programs = match bundle_programs(app) {
        Ok(programs) => programs,
        Err(why) => {
            return Some(format!(
                "{why}; whether every program has finished its first start cannot be told"
            ));
        }
    };
    for program in programs {
        if proven == Some(program.as_path()) {
            continue;
        }
        let run_as = runs_as(aterm_uds::peer::our_uid(), &p.home, &program);
        let ask = match run_as {
            RunAs::Account(uid) => format!(
                "{SUDO} -n -H -u #{uid} -- {} -c '{ENDED}' {} --version",
                SH.join(" "),
                program.display()
            ),
            RunAs::Me | RunAs::Nobody => format!("{} --version", program.display()),
        };
        if s.dry && run_as != RunAs::Nobody {
            s.say(&format!("would ask {ask}, then if it answers:"));
        } else if let Some(why) = first_exec_unproven(&program, run_as, hang) {
            return Some(format!("`{ask}` {why}"));
        }
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunAs {
    Me,
    Account(u32),
    Nobody,
}

const NOT_AS_ROOT: &str = "root runs a program only as the account whose own home HOME is, or \
                           for its own HOME one no account can replace (run the uninstall as \
                           that account)";

/// Who acts on `home` under `our_uid`: root itself only for its own home, an account for its own.
fn home_as(our_uid: u32, home: &Path) -> RunAs {
    if our_uid != 0 {
        return RunAs::Me;
    }
    if let Some((uid, _)) = acaffeine_account(our_uid, home) {
        return RunAs::Account(uid);
    }
    if aterm_uds::account::home_of(0).is_some_and(|own| same_file(&own, home)) {
        RunAs::Me
    } else {
        RunAs::Nobody
    }
}

/// Execution identity for `program` during a sweep of `home` under `our_uid`.
fn runs_as(our_uid: u32, home: &Path, program: &Path) -> RunAs {
    match home_as(our_uid, home) {
        RunAs::Me if our_uid == 0 && !(ours_already(program) || roots_alone(program)) => {
            RunAs::Nobody
        }
        who => who,
    }
}

/// Why `shell-setup` under `our_uid` writes nothing in `home`: root wiring an account's home would
/// leave it root's profile, man pages and completions, which that account cannot then change.
fn setup_refusal(our_uid: u32, home: &Path, args: &[String]) -> Option<String> {
    let whose = match home_as(our_uid, home) {
        RunAs::Me => return None,
        RunAs::Account(uid) => format!("is uid {uid}'s own home"),
        RunAs::Nobody => "is not root's own home".to_string(),
    };
    let command: String = args
        .iter()
        .map(|arg| {
            if arg.contains(char::is_whitespace) {
                format!(" '{arg}'")
            } else {
                format!(" {arg}")
            }
        })
        .collect();
    Some(format!(
        "HOME ({}) {whose}, and root writes no files there that its account could then not \
         change: run `aterm pkg shell-setup{command}` as that account",
        home.display()
    ))
}

fn ours_already(program: &Path) -> bool {
    let (Ok(exe), Ok(resolved)) = (
        std::env::current_exe().and_then(std::fs::canonicalize),
        std::fs::canonicalize(program),
    ) else {
        return false;
    };
    if resolved != program {
        return false;
    }
    let bundle = exe
        .ancestors()
        .find(|dir| dir.extension().is_some_and(|ext| ext == "app"));
    resolved == exe || bundle.is_some_and(|bundle| resolved.starts_with(bundle))
}

/// Whether `program` and every ancestor (spelled and resolved) can only be modified by root.
fn roots_alone(program: &Path) -> bool {
    let resolved = std::fs::canonicalize(program).ok();
    [Some(program.to_path_buf()), resolved]
        .iter()
        .flatten()
        .all(|path| {
            path.ancestors()
                .filter(|at| !at.as_os_str().is_empty())
                .all(|at| {
                    std::fs::symlink_metadata(at).is_ok_and(|m| {
                        let kind = m.file_type();
                        root_only_entry(
                            m.uid(),
                            m.gid(),
                            m.mode(),
                            kind.is_dir(),
                            kind.is_symlink(),
                        )
                    })
                })
        })
}

const fn root_only_entry(uid: u32, gid: u32, mode: u32, dir: bool, link: bool) -> bool {
    if link {
        return uid == 0;
    }
    let sticky = dir && mode & 0o1000 != 0;
    uid == 0 && (sticky || mode & 0o002 == 0) && (sticky || gid == 0 || mode & 0o020 == 0)
}

fn sudo_refuses(uid: u32) -> Option<i32> {
    let rc = run_quietly(Path::new(SUDO), &as_account_argv(uid, Path::new(TRUE), &[]));
    (rc != 0).then_some(rc)
}

fn store_entry_is_ours(path: &Path, run_as: RunAs) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let meta = std::fs::symlink_metadata(path);
    match name {
        "aterm" => answers_as_aterm(path, run_as),
        ".aterm-update" => meta.is_ok_and(|m| m.is_dir()),
        n if n.starts_with(".aterm-install.") => meta.is_ok_and(|m| m.is_file()),
        _ => link_target(path).as_deref() == Some("aterm"),
    }
}

fn remove(path: &Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(path)?;
    if meta.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

fn present(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// The one account among the owners of the folders on the way to a path: `Ok(None)` when root
/// owns them all, `Err(())` when two accounts do.
fn way_account(owners: impl IntoIterator<Item = u32>) -> Result<Option<u32>, ()> {
    let mut account = None;
    for uid in owners.into_iter().filter(|&uid| uid != 0) {
        match account {
            Some(seen) if seen != uid => return Err(()),
            _ => account = Some(uid),
        }
    }
    Ok(account)
}

/// [`way_account`] of the folders above `path`, spelled and resolved: whoever owns one can swap
/// what lies below it between a root sweep's look and its act.
fn account_on_the_way(path: &Path) -> Result<Option<u32>, ()> {
    let parent = path.parent().unwrap_or(path);
    let resolved = std::fs::canonicalize(parent).ok();
    let folders: Vec<PathBuf> = [Some(parent.to_path_buf()), resolved]
        .into_iter()
        .flatten()
        .flat_map(|dir| dir.ancestors().map(Path::to_path_buf).collect::<Vec<_>>())
        .filter(|at| !at.as_os_str().is_empty())
        .collect();
    way_account(
        folders
            .iter()
            .filter_map(|at| std::fs::symlink_metadata(at).ok().map(|m| m.uid())),
    )
}

fn two_accounts() -> std::io::Error {
    std::io::Error::other("two accounts own folders on the way to it, and root acts as neither")
}

/// `path` opened with `opts` when it is a regular file and, under root, one owned by the account
/// owning the way to it (root when none does): a link or a swapped folder then never makes
/// root's read hang on a pipe or its write land in another owner's file.
fn open_checked_with_meta(
    path: &Path,
    opts: &mut std::fs::OpenOptions,
    root: bool,
) -> std::io::Result<(std::fs::File, std::fs::Metadata)> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let file = opts
        .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY | libc::O_CLOEXEC)
        .open(path)?;
    let opened = file.metadata()?;
    let owner = if root {
        Some(
            account_on_the_way(path)
                .map_err(|()| two_accounts())?
                .unwrap_or(0),
        )
    } else {
        None
    };
    if !opened.is_file() || owner.is_some_and(|owner| opened.uid() != owner) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "not a regular file of the account it is for",
        ));
    }
    Ok((file, opened))
}

fn open_checked(
    path: &Path,
    opts: &mut std::fs::OpenOptions,
    root: bool,
) -> std::io::Result<std::fs::File> {
    open_checked_with_meta(path, opts, root).map(|(file, _)| file)
}

/// `path`'s text, from a regular file only: a pipe behind a link is refused, not waited on.
/// Reading needs no owner (only what a sweep writes does), so root still sees a block it may not
/// rewrite.
fn read_checked(path: &Path) -> std::io::Result<String> {
    use std::io::Read as _;
    const CAP: usize = 1024 * 1024;
    let (file, meta) = open_checked_with_meta(path, std::fs::OpenOptions::new().read(true), false)?;
    if meta.len() > CAP as u64 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "file exceeds read cap",
        ));
    }
    let mut buf = Vec::with_capacity(usize::try_from(meta.len().saturating_add(1)).unwrap_or(0));
    file.take(CAP as u64 + 1).read_to_end(&mut buf)?;
    if buf.len() > CAP {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "file exceeds read cap",
        ));
    }
    String::from_utf8(buf).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

struct Sweep<'w> {
    dry: bool,
    /// How a root sweep runs a command as an account; `None` when the sweep is not root's.
    as_root: Option<fn(&Path, &[OsString]) -> i32>,
    removed: usize,
    skipped: usize,
    kept: usize,
    shared_kept: usize,
    outsider: Option<PathBuf>,
    out: &'w mut dyn Write,
    err: &'w mut dyn Write,
}

impl Sweep<'_> {
    const fn act(&self) -> &'static str {
        if self.dry { "would remove" } else { "remove" }
    }

    fn say(&mut self, line: &str) {
        let _ = writeln!(self.out, "atpkg: {line}");
    }

    /// Whether this call actually removed the owned entry, rather than planning,
    /// finding it already absent, or leaving it in place.
    fn rm(&mut self, path: &Path, label: &str) -> bool {
        if self.dry {
            let act = self.act();
            self.say(&format!("{act} {label}: {}", path.display()));
        } else {
            match self.unlink(path, true) {
                Ok(()) => self.say(&format!("removed {label}: {}", path.display())),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    self.say(&format!("gone already: {label}: {}", path.display()));
                    return false;
                }
                Err(e) => {
                    let why = if self.as_root.is_some() {
                        format!("cannot remove: {e}")
                    } else {
                        "cannot remove, try sudo".to_string()
                    };
                    let _ = writeln!(
                        self.err,
                        "atpkg: SKIPPED {label} ({why}): {}",
                        path.display()
                    );
                    self.skipped += 1;
                    return false;
                }
            }
        }
        self.removed += 1;
        !self.dry
    }

    /// Remove `path`: everything in it when `tree`, else only an empty folder. Under root it goes
    /// as the account owning a folder on the way to it (by root itself only when none does), so
    /// what that account swaps in mid-sweep is never something only root could remove.
    fn unlink(&self, path: &Path, tree: bool) -> std::io::Result<()> {
        let own = || {
            if tree {
                remove(path)
            } else {
                std::fs::remove_dir(path)
            }
        };
        let Some(run) = self.as_root else {
            return own();
        };
        let uid = match account_on_the_way(path) {
            Ok(None) => return own(),
            Ok(Some(uid)) => uid,
            Err(()) => return Err(two_accounts()),
        };
        std::fs::symlink_metadata(path)?;
        let (program, args): (&str, &[&str]) = if tree {
            (RM, &["-rf", "--"])
        } else {
            (RMDIR, &["--"])
        };
        let mut argv = as_account_argv(uid, Path::new(program), args);
        argv.push(path.into());
        let rc = run(Path::new(SUDO), &argv);
        if present(path) {
            return Err(std::io::Error::other(format!(
                "uid {uid}, which owns a folder on the way to it, could not (exit {rc})"
            )));
        }
        Ok(())
    }

    fn skip(&mut self, path: &str, reason: &str) {
        let _ = writeln!(self.err, "atpkg: SKIPPED {path} — {reason}");
        self.skipped += 1;
    }

    fn rm_shared(&mut self, path: &Path, label: &str) {
        if self.outsider.is_some() {
            self.say(&format!("left in place: {label}: {}", path.display()));
            self.kept += 1;
            self.shared_kept += 1;
        } else {
            self.rm(path, label);
        }
    }
}

fn outside_scope(p: &Places, bundle: &Path) -> bool {
    let Some(scope) = &p.install_dir else {
        return false;
    };
    let entry = bundle.file_name().is_some_and(|n| n == "aterm.app")
        && bundle
            .parent()
            .is_some_and(|dir| dir_within(dir, scope) && dir_within(scope, dir));
    !entry
}

fn other_aterm(p: &Places, bundle: &Path) -> bool {
    bundle.is_dir() && outside_scope(p, bundle) && bundle_may_be_aterm(bundle)
}

fn scope_outsider(p: &Places) -> Option<PathBuf> {
    let mut cands = vec![p.home.join("Applications/aterm.app")];
    if home_is_the_accounts(aterm_uds::peer::our_uid(), &p.home) {
        cands.push(PathBuf::from("/Applications/aterm.app"));
    }
    for bd in p.bin_dirs() {
        for name in ["aterm", "atpkg"] {
            cands.extend(link_bundle(&bd.join(name)));
        }
    }
    cands.into_iter().find(|b| other_aterm(p, b))
}

fn uninstall_bundle(p: &Places) -> Option<PathBuf> {
    p.app_dirs()
        .iter()
        .map(|d| d.join("aterm.app"))
        .find(|app| {
            bundle_is_ours(app)
                && crate::toolchain::is_executable_file(&app.join("Contents/MacOS/atpkg"))
        })
}

struct UninstallHelper {
    root: PathBuf,
    program: PathBuf,
    args: &'static [&'static str],
    stored: bool,
}

fn uninstall_helper(p: &Places) -> Option<UninstallHelper> {
    if let Some(app) = uninstall_bundle(p) {
        return Some(UninstallHelper {
            program: app.join("Contents/MacOS/atpkg"),
            root: app,
            args: &["uninstall", "--all"],
            stored: false,
        });
    }
    let program = p.store_dir.join("aterm");
    let run_as = runs_as(aterm_uds::peer::our_uid(), &p.home, &program);
    (store_left(p).is_none() && answers_as_aterm(&program, run_as)).then(|| UninstallHelper {
        root: program.clone(),
        program,
        args: &["pkg", "uninstall", "--all"],
        stored: true,
    })
}

const CAFFEINE_CODE: &str = "acaffeine-code.noindex";

/// aCaffeine's files under the support folder.
const CAFFEINE_FILES: [&str; 6] = [
    CAFFEINE_CODE,
    "acaffeine",
    "acaffeine.state",
    "acaffeine.lock",
    "acaffeine.bridge",
    "acaffeine.trouble",
];

const SWEPT_DECLINE_TAG: &str = "# Recorded by aterm pkg self-uninstall for an aterm it kept.";

const CAFFEINE_LABEL_PREFIX: &str = "com.aterm.aterm.acaffeine";

fn sweep_acaffeine(s: &mut Sweep<'_>, p: &Places, app_dirs: &[PathBuf]) -> Vec<PathBuf> {
    let kept = sweep_acaffeine_with(
        s,
        p,
        app_dirs,
        aterm_uds::peer::our_uid(),
        || acaffeine_account(aterm_uds::peer::our_uid(), &p.home),
        run_quietly,
    );
    sweep_old_caffeine(
        s,
        p,
        acaffeine_account(aterm_uds::peer::our_uid(), &p.home),
        make_as_account,
    );
    kept
}

/// The copies of aterm under aCaffeine's name before 0.145.0, `aterm caffeine`.
const OLD_CAFFEINE_CODE: &str = "caffeine-code.noindex";

/// What the mug left in the support folder under its old name, besides its lock.
const OLD_CAFFEINE_FILES: [&str; 4] = [
    OLD_CAFFEINE_CODE,
    "caffeine",
    "caffeine.state",
    "caffeine.bridge",
];

/// The old mug's lock, which stays as aCaffeine's does: an old build's mug may still take it.
const OLD_CAFFEINE_LOCK: &str = "caffeine.lock";

/// The old installed app's login job label; a dev build's adds `.dev.` and 8 lowercase hex
/// digits (`aterm_acaffeine::legacy::is_label`).
const OLD_CAFFEINE_LABEL: &str = "com.aterm.aterm.caffeine";

fn old_caffeine_label(label: &str) -> bool {
    label_of(label, OLD_CAFFEINE_LABEL)
}

/// Whether `label` is one an aCaffeine build makes (`aterm_acaffeine::launchd::is_label`): the
/// installed app's, or a dev build's under the same grammar as [`OLD_CAFFEINE_LABEL`]'s.
fn acaffeine_label(label: &str) -> bool {
    label_of(label, CAFFEINE_LABEL_PREFIX)
}

/// Whether `label` is `installed`, or `installed`, `.dev.` and 8 lowercase hex digits.
fn label_of(label: &str, installed: &str) -> bool {
    label == installed
        || label
            .strip_prefix(installed)
            .and_then(|rest| rest.strip_prefix(".dev."))
            .is_some_and(|hash| {
                hash.len() == 8
                    && hash
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
}

/// The login job that resumes an unfinished first-run setup with no window
/// (`aterm_types::macos_setup::SETUP_RESUME_LABEL`): its definition runs the aterm this sweep
/// removes, so one left behind would fail at every login. It goes as an old Caffeine login item
/// does, the job launchd loaded ending with this login; a sweep that leaves an aterm outside its
/// scope leaves it for that one.
fn sweep_setup_job(s: &mut Sweep<'_>, p: &Places) {
    let plist = p.home.join("Library/LaunchAgents").join(format!(
        "{}.plist",
        aterm_types::macos_setup::SETUP_RESUME_LABEL
    ));
    if std::fs::symlink_metadata(&plist).is_ok_and(|meta| meta.is_file()) {
        s.rm_shared(&plist, "setup login item");
    }
}

/// What the mug left under its old name, which no build since 0.145.0 has as a verb, so no helper
/// here turns its login job off or quits it (an older aterm this sweep removes still has it, and
/// its mug, if one runs, keeps its lock until it is quit from its menu or the login ends, so its
/// files are left for the person to remove). Its login plists in `~/Library/LaunchAgents` go:
/// each runs `<aterm> caffeine serve`, and one whose aterm is gone fails at every login. Its
/// files go while its idle lock is held, as aCaffeine's do, and the lock stays. A sweep that
/// leaves an aterm outside its scope leaves them all for it.
fn sweep_old_caffeine(
    s: &mut Sweep<'_>,
    p: &Places,
    account: Option<(u32, u32)>,
    make: fn(&Path, u32) -> bool,
) {
    let support = p.support();
    let listed = || -> Vec<PathBuf> {
        OLD_CAFFEINE_FILES
            .iter()
            .map(|name| support.join(name))
            .filter(|path| present(path))
            .collect()
    };
    let plists: Vec<PathBuf> = sorted_entries(&p.home.join("Library/LaunchAgents"))
        .into_iter()
        .filter(|plist| {
            plist
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_suffix(".plist"))
                .is_some_and(old_caffeine_label)
                && std::fs::symlink_metadata(plist).is_ok_and(|meta| meta.is_file())
        })
        .collect();
    for plist in &plists {
        s.rm_shared(plist, "old Caffeine login item");
    }
    let lock = support.join(OLD_CAFFEINE_LOCK);
    let files = listed();
    if s.outsider.is_some() {
        for path in files {
            s.rm_shared(&path, "old Caffeine file");
        }
        return;
    }
    if !files.is_empty() {
        if s.dry {
            s.say(
                "old Caffeine file removal requires an acquired idle lock; the lock itself stays",
            );
            if present(&lock) {
                if !cleanup_lock_free(&lock, account) {
                    s.skip(
                        &lock.display().to_string(),
                        &old_lock_kept(&support, "would be"),
                    );
                    return;
                }
                s.say(&format!(
                    "would leave old Caffeine lock rendezvous: {}",
                    lock.display()
                ));
                s.kept += 1;
            } else if !would_make_lock(s, &lock, account, "old Caffeine") {
                return;
            }
            for path in files {
                if path == support.join(OLD_CAFFEINE_CODE) {
                    sweep_acaffeine_code(s, &path, std::time::SystemTime::now());
                } else {
                    s.rm_shared(&path, "old Caffeine file");
                }
            }
            return;
        }
        let Some(_hold) = acaffeine_cleanup_hold(&lock, account, make) else {
            s.skip(&lock.display().to_string(), &old_lock_kept(&support, "are"));
            return;
        };
        for path in listed() {
            if path == support.join(OLD_CAFFEINE_CODE) {
                sweep_acaffeine_code(s, &path, std::time::SystemTime::now());
            } else {
                s.rm_shared(&path, "old Caffeine file");
            }
        }
    }
    if present(&lock) {
        s.say(&format!(
            "{} old Caffeine lock rendezvous: {}",
            if s.dry { "would leave" } else { "left" },
            lock.display()
        ));
        s.kept += 1;
    }
}

/// Why the old name's files in `support` are kept (`are`, or `would be` in a dry run) when its
/// lock cannot be taken, and how to remove them once the old mug is quit.
fn old_lock_kept(support: &Path, are: &str) -> String {
    format!(
        "an old Caffeine mug holds this lock, or it could not be acquired; its files {are} kept: \
         once that mug is quit (Quit Caffeine in its menu, or a logout), remove {} and \
         {OLD_CAFFEINE_LOCK} from {} by hand",
        OLD_CAFFEINE_FILES.join(", "),
        support.display()
    )
}

/// Whether a real run's [`acaffeine_cleanup_hold`] could take the lock at `lock`, which is there,
/// for `account`: a regular file of the account's with one name, which no mug holds. A dry run's
/// probe: it opens the file without making it, writes nothing and lets go at once.
fn cleanup_lock_free(lock: &Path, account: Option<(u32, u32)>) -> bool {
    use std::os::unix::fs::OpenOptionsExt as _;
    let owner = account.map_or_else(aterm_uds::peer::our_uid, |(uid, _)| uid);
    let Ok(file) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(lock)
    else {
        return false;
    };
    file.metadata()
        .is_ok_and(|meta| meta.is_file() && meta.uid() == owner && meta.nlink() == 1)
        && crate::lock::Flock::try_lock(file).is_ok()
}

/// A dry run's word for the lock rendezvous at `lock` that a real run's
/// [`acaffeine_cleanup_hold`] would make, `what`'s: made, as the account's under root, and left,
/// counted kept; or, where that hold makes none (root for no account), a skip saying the files
/// would be kept, and `false`.
fn would_make_lock(
    s: &mut Sweep<'_>,
    lock: &Path,
    account: Option<(u32, u32)>,
    what: &str,
) -> bool {
    if account.is_none() && aterm_uds::peer::our_uid() == 0 {
        s.skip(
            &lock.display().to_string(),
            &format!("{what} lock could not be made; its files would be kept"),
        );
        return false;
    }
    s.say(&format!(
        "would make and leave {what} lock rendezvous: {}",
        lock.display()
    ));
    s.kept += 1;
    true
}

fn sweep_acaffeine_with(
    s: &mut Sweep<'_>,
    p: &Places,
    app_dirs: &[PathBuf],
    our_uid: u32,
    account: impl FnOnce() -> Option<(u32, u32)>,
    mut run: impl FnMut(&Path, &[OsString]) -> i32,
) -> Vec<PathBuf> {
    let support = p.support();
    let agents = p.home.join("Library/LaunchAgents");
    for (temp, what) in cut_short_saves(s, &support, &agents) {
        s.rm_shared(&temp, what);
    }
    let listed = || -> Vec<PathBuf> {
        CAFFEINE_FILES
            .iter()
            .map(|name| support.join(name))
            .filter(|path| present(path))
            .collect()
    };
    let files = listed();
    let login_plist = sorted_entries(&p.home.join("Library/LaunchAgents"))
        .iter()
        .filter_map(|plist| plist.file_name().and_then(|n| n.to_str()))
        .any(|name| name.starts_with(CAFFEINE_LABEL_PREFIX) && name.ends_with(".plist"));
    if files.is_empty() && !login_plist {
        return Vec::new();
    }
    let account = account();
    let own_quit: &[&str] = &["acaffeine", "quit", "own"];
    let steps: &[&[&str]] = if s.outsider.is_some() {
        &[&["acaffeine", "startup", "off"], own_quit]
    } else {
        &[&["acaffeine", "startup", "off"], &["acaffeine", "quit"]]
    };
    let mut helpers: Vec<(PathBuf, PathBuf)> = app_dirs
        .iter()
        .map(|dir| dir.join("aterm.app"))
        .filter_map(|app| {
            let aterm = app.join("Contents/MacOS/aterm");
            (bundle_is_ours(&app) && crate::toolchain::is_executable_file(&aterm))
                .then_some((app, aterm))
        })
        .collect();
    let stored = p.store_dir.join("aterm");
    if store_left(p).is_none() && is_executable_file(&stored) {
        helpers.push((stored.clone(), stored.clone()));
    }
    let run_as = |aterm: &Path| {
        account.map_or_else(
            || runs_as(our_uid, &p.home, aterm),
            |(uid, _)| RunAs::Account(uid),
        )
    };
    let mut supported = false;
    let mut failed = Vec::new();
    helpers.retain(|(app, aterm)| {
        if run_as(aterm) != RunAs::Nobody {
            return true;
        }
        s.skip(
            &helper_line(aterm, &["help", "acaffeine"], account),
            &format!("{NOT_AS_ROOT}; {} is kept for it", app.display()),
        );
        failed.push(app.clone());
        false
    });
    if let Some((uid, _)) = account
        && !s.dry
        && !helpers.is_empty()
    {
        let rc = run(Path::new(SUDO), &as_account_argv(uid, Path::new(TRUE), &[]));
        if rc != 0 {
            for (app, aterm) in std::mem::take(&mut helpers) {
                s.skip(
                    &helper_line(&aterm, &["help", "acaffeine"], account),
                    &format!(
                        "sudo could not run it as uid {uid} (exit {rc}); {} is kept for it",
                        app.display()
                    ),
                );
                failed.push(app);
            }
        }
    }
    helpers.retain(|(_, aterm)| *aterm != stored || answers_as_aterm(aterm, run_as(aterm)));
    let asked = !helpers.is_empty();
    for (app, aterm) in helpers {
        if s.dry {
            s.say(&format!(
                "would ask {}, then if supported:",
                helper_line(&aterm, &["help", "acaffeine"], account)
            ));
            for args in steps {
                s.say(&format!("would run {}", helper_line(&aterm, args, account)));
            }
            continue;
        }
        let rc = run_helper(&aterm, &["help", "acaffeine"], account, &mut run);
        if rc != 0 {
            if account.is_some() {
                s.skip(
                    &helper_line(&aterm, &["help", "acaffeine"], account),
                    &format!(
                        "could not confirm the helper ran as its account (exit {rc}); {} is kept for it",
                        app.display()
                    ),
                );
                failed.push(app);
            }
            continue;
        }
        supported = true;
        for args in steps {
            let line = helper_line(&aterm, args, account);
            match run_helper(&aterm, args, account, &mut run) {
                0 => s.say(&format!("ran {line}")),
                2 if *args == own_quit => s.say(&format!(
                    "{line}: this build cannot quit only its own mug; one it runs keeps running"
                )),
                rc => {
                    if !failed.contains(&app) {
                        failed.push(app.clone());
                    }
                    s.skip(
                        &line,
                        &format!(
                            "it failed (exit {rc}) — run it by hand to see why; {} is kept \
                             for it",
                            app.display()
                        ),
                    );
                }
            }
        }
    }
    if s.outsider.is_some() {
        for path in files {
            s.rm_shared(&path, "aCaffeine file");
        }
        return failed;
    }
    let files = listed();
    if files.is_empty() {
        return failed;
    }
    let lock = support.join("acaffeine.lock");
    let lone_lock = (our_uid != 0 || account.is_some())
        && !login_plist
        && files.iter().all(|path| *path == lock);
    // A dry run asks no helper: what it lists follows a yes, and its line above says so.
    let confirmed = if s.dry { asked } else { supported };
    if (!confirmed && !lone_lock) || !failed.is_empty() {
        s.skip(
            &support.display().to_string(),
            if s.dry {
                "aCaffeine stop would not be confirmed; its files would be kept"
            } else {
                "aCaffeine stop was not confirmed; its files are kept"
            },
        );
        return failed;
    }
    if s.dry {
        s.say(if asked {
            "aCaffeine file removal requires a helper above that stops aCaffeine and an acquired \
             idle lock; the lock itself stays"
        } else {
            "aCaffeine file removal requires an acquired idle lock; the lock itself stays"
        });
        if files.contains(&lock) {
            // A helper's quit would end the mug that holds it; with none, a held lock stays held.
            if !asked && !cleanup_lock_free(&lock, account) {
                s.skip(
                    &lock.display().to_string(),
                    "aCaffeine lock is busy or could not be acquired; its files would be kept",
                );
                return failed;
            }
        } else if !would_make_lock(s, &lock, account, "aCaffeine") {
            return failed;
        }
        for path in files {
            if path == lock {
                s.say(&format!(
                    "would leave aCaffeine lock rendezvous: {}",
                    path.display()
                ));
                s.kept += 1;
            } else if path == support.join(CAFFEINE_CODE) {
                sweep_acaffeine_code(s, &path, std::time::SystemTime::now());
            } else {
                s.rm_shared(&path, "aCaffeine file");
            }
        }
        return failed;
    }
    let Some(_hold) = acaffeine_cleanup_hold(&lock, account, make_as_account) else {
        s.skip(
            &lock.display().to_string(),
            "aCaffeine lock is busy or could not be acquired; its files are kept",
        );
        return failed;
    };
    let files = listed();
    for path in files {
        if path == lock {
            s.say(&format!(
                "left aCaffeine lock rendezvous: {}",
                path.display()
            ));
            s.kept += 1;
        } else if path == support.join(CAFFEINE_CODE) {
            sweep_acaffeine_code(s, &path, std::time::SystemTime::now());
        } else {
            s.rm_shared(&path, "aCaffeine file");
        }
    }
    failed
}

/// The files in `support` whose temporaries a window's launch sweeps
/// (`aterm_acaffeine::state::sweep_temps`): aCaffeine's mode and its update bridge's pid file,
/// under its name and the old one, and its trouble record, each with what a temporary of it is.
const WRITTEN_THROUGH_TEMPS: [(&str, &str); 5] = [
    ("acaffeine.state", "aCaffeine save cut short"),
    ("acaffeine.bridge", "aCaffeine save cut short"),
    ("acaffeine.trouble", "aCaffeine save cut short"),
    ("caffeine.state", "old Caffeine save cut short"),
    ("caffeine.bridge", "old Caffeine save cut short"),
];

/// The temporaries that no write will rename
/// ([`aterm_uds::atomic_file::abandoned_temps_beside`]) of [`WRITTEN_THROUGH_TEMPS`] in
/// `support`, and of the login items in `agents` named for a label [`acaffeine_label`] or
/// [`old_caffeine_label`] takes ([`aterm_uds::atomic_file::temp_target`]), each with what it is:
/// an ordinary window's launch removes them too, but an uninstall may come first. Their writers
/// are gone, so no mug's stop or lock is needed for them; the session plists' folders go whole
/// with aCaffeine's other files. A `support` that is no folder holds none, and one that cannot
/// be listed is one skip, after which none of them is removed. `agents` is read as the rest of
/// the uninstall reads it ([`sorted_entries`]): one that is no folder, or that cannot be listed
/// (a person may lock it against login items), holds none.
fn cut_short_saves(
    s: &mut Sweep<'_>,
    support: &Path,
    agents: &Path,
) -> Vec<(PathBuf, &'static str)> {
    let after = aterm_uds::atomic_file::ABANDONED_AFTER;
    let mut found = Vec::new();
    if support.is_dir() {
        for (name, what) in WRITTEN_THROUGH_TEMPS {
            match aterm_uds::atomic_file::abandoned_temps_beside(&support.join(name), after) {
                Ok(temps) => found.extend(temps.into_iter().map(|temp| (temp, what))),
                Err(e) => {
                    s.skip(
                        &support.display().to_string(),
                        &format!("its aCaffeine saves cut short could not be listed: {e}"),
                    );
                    return Vec::new();
                }
            }
        }
    }
    found.extend(login_items_cut_short(agents, after));
    found
}

/// [`cut_short_saves`]'s temporaries of the login items in `agents`.
fn login_items_cut_short(
    agents: &Path,
    after: std::time::Duration,
) -> Vec<(PathBuf, &'static str)> {
    let mut plists = std::collections::BTreeMap::new();
    for entry in std::fs::read_dir(agents).into_iter().flatten().flatten() {
        let name = entry.file_name();
        let Some(plist) = aterm_uds::atomic_file::temp_target(&name) else {
            continue;
        };
        let what = match plist.strip_suffix(".plist") {
            Some(label) if acaffeine_label(label) => "aCaffeine login item cut short",
            Some(label) if old_caffeine_label(label) => "old Caffeine login item cut short",
            _ => continue,
        };
        plists.insert(agents.join(plist), what);
    }
    let mut found = Vec::new();
    for (plist, what) in plists {
        let temps = aterm_uds::atomic_file::abandoned_temps_beside(&plist, after);
        found.extend(temps.into_iter().flatten().map(|temp| (temp, what)));
    }
    found
}

fn sweep_acaffeine_code(s: &mut Sweep<'_>, code: &Path, now: std::time::SystemTime) {
    sweep_acaffeine_code_with(
        s,
        code,
        now,
        &|path| std::fs::symlink_metadata(path),
        &acaffeine_code_entries,
    );
}

#[derive(Default)]
struct ACaffeineCodePlan {
    remove: Vec<(PathBuf, &'static str)>,
    keep: Vec<(PathBuf, Duration)>,
}

fn acaffeine_code_plan_with(
    code: &Path,
    now: std::time::SystemTime,
    metadata: &impl Fn(&Path) -> std::io::Result<std::fs::Metadata>,
    entries: &impl Fn(&Path) -> std::io::Result<Vec<PathBuf>>,
) -> std::io::Result<ACaffeineCodePlan> {
    let at = |path: &Path, error: std::io::Error| {
        std::io::Error::new(error.kind(), format!("{}: {error}", path.display()))
    };
    let inspect = |path: &Path| -> std::io::Result<Option<std::fs::Metadata>> {
        match metadata(path) {
            Ok(meta) => Ok(Some(meta)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(at(path, error)),
        }
    };
    let list = |path: &Path| entries(path).map_err(|error| at(path, error));
    let mut plan = ACaffeineCodePlan::default();
    let Some(root) = inspect(code)? else {
        return Ok(plan);
    };
    if !root.is_dir() {
        plan.remove.push((code.to_path_buf(), "aCaffeine file"));
        return Ok(plan);
    }
    let partial = |path: &Path| path.extension().is_some_and(|e| e == "partial");
    for label in list(code)? {
        let Some(meta) = inspect(&label)? else {
            continue;
        };
        if !meta.is_dir() || partial(&label) {
            plan.remove.push((label, "aCaffeine file"));
            continue;
        }
        let mut copies = Vec::new();
        let mut kept = Vec::new();
        for copy in list(&label)? {
            let Some(meta) = inspect(&copy)? else {
                continue;
            };
            let left = (meta.is_dir() && !partial(&copy))
                .then(|| aterm_uds::deadline::copy_wait_left(&copy, now))
                .flatten();
            if let Some(left) = left {
                kept.push((copy, left));
            } else {
                copies.push((copy, "aCaffeine copy"));
            }
        }
        if kept.is_empty() {
            plan.remove.push((label, "aCaffeine file"));
        } else {
            plan.remove.extend(copies);
            plan.keep.extend(kept);
        }
    }
    if plan.keep.is_empty() {
        plan.remove.clear();
        plan.remove.push((code.to_path_buf(), "aCaffeine file"));
    }
    Ok(plan)
}

fn acaffeine_code_entries(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    collect_acaffeine_code_entries(std::fs::read_dir(dir)?.map(|entry| entry.map(|e| e.path())))
}

fn collect_acaffeine_code_entries(
    entries: impl IntoIterator<Item = std::io::Result<PathBuf>>,
) -> std::io::Result<Vec<PathBuf>> {
    let mut paths = entries.into_iter().collect::<std::io::Result<Vec<_>>>()?;
    paths.sort_by_key(|path| {
        let name = path.file_name().unwrap_or_default();
        (name.to_string_lossy().starts_with('.'), name.to_os_string())
    });
    Ok(paths)
}

fn sweep_acaffeine_code_with(
    s: &mut Sweep<'_>,
    code: &Path,
    now: std::time::SystemTime,
    metadata: &impl Fn(&Path) -> std::io::Result<std::fs::Metadata>,
    entries: &impl Fn(&Path) -> std::io::Result<Vec<PathBuf>>,
) {
    let plan = match acaffeine_code_plan_with(code, now, metadata, entries) {
        Ok(plan) => plan,
        Err(error) => {
            s.skip(
                &code.display().to_string(),
                &format!("cannot inspect every aCaffeine copy; all kept: {error}"),
            );
            return;
        }
    };
    for (path, label) in plan.remove {
        if path == code {
            s.rm_shared(&path, label);
        } else {
            s.rm(&path, label);
        }
    }
    for (path, left) in plan.keep {
        s.skip(
            &path.display().to_string(),
            &format!(
                "no aCaffeine mug said it ran from this copy, so macOS may still be assessing \
                 its first exec, and removing it now can crash syspolicyd; remove it by hand \
                 once the Mac has been awake {} more minute(s)",
                left.as_secs().div_ceil(60).max(1)
            ),
        );
    }
}

fn acaffeine_cleanup_hold(
    path: &Path,
    account: Option<(u32, u32)>,
    make: fn(&Path, u32) -> bool,
) -> Option<crate::lock::Flock> {
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
    let owner = account.map_or_else(aterm_uds::peer::our_uid, |(uid, _)| uid);
    let made = account.is_some()
        && std::fs::symlink_metadata(path).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound);
    if made && !make(path, owner) {
        return None;
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(account.is_none() && aterm_uds::peer::our_uid() != 0)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .ok()?;
    let opened = file.metadata().ok()?;
    if !opened.is_file() || opened.uid() != owner || opened.nlink() != 1 {
        return None;
    }
    if made && opened.mode() & 0o077 != 0 {
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .ok()?;
    }
    let held = crate::lock::Flock::try_lock(file).ok()?;
    let named = std::fs::symlink_metadata(path).ok()?;
    if !named.is_file() || (named.dev(), named.ino()) != (opened.dev(), opened.ino()) {
        return None;
    }
    Some(held)
}

fn acaffeine_account(our_uid: u32, home: &Path) -> Option<(u32, u32)> {
    if our_uid != 0 {
        return None;
    }
    let owner = std::fs::metadata(home).ok()?;
    let own = aterm_uds::account::home_of(owner.uid())?;
    (owner.uid() != 0 && same_file(&own, home)).then(|| (owner.uid(), owner.gid()))
}

fn home_is_the_accounts(our_uid: u32, home: &Path) -> bool {
    acaffeine_account(our_uid, home).is_some()
        || aterm_uds::account::home_of(our_uid).is_some_and(|acct| same_file(&acct, home))
}

const SUDO: &str = "/usr/bin/sudo";
const TRUE: &str = "/usr/bin/true";
/// The shell a run through sudo asks a program by ([`ENDED`]), with its flags: bash, whose words
/// for an exec it could not make [`says_never_execs`] reads (macOS's `/bin/sh` may be zsh, which
/// words a kernel refusal otherwise), reading no startup file (`--norc`: sshd's `SSH_CLIENT` has
/// bash read the account's `.bashrc`) and none the environment names (`-p`: `BASH_ENV`), so only
/// the program and the script write what is read as the run.
#[cfg(target_vendor = "apple")]
const SH: [&str; 3] = ["/bin/bash", "--norc", "-p"];
/// See the Apple twin.
#[cfg(not(target_vendor = "apple"))]
const SH: [&str; 1] = ["/bin/sh"];
/// The script a run through sudo asks a program by: `ended <status>` once the shell's run of
/// it ends, which sudo's own refusal never writes ([`shell_end_unproven`]).
const ENDED: &str = "\"$0\" \"$@\"; echo \"ended $?\"";
const TOUCH: &str = "/usr/bin/touch";
const RM: &str = "/bin/rm";
const RMDIR: &str = "/bin/rmdir";

fn make_as_account(path: &Path, uid: u32) -> bool {
    let mut argv = as_account_argv(uid, Path::new(TOUCH), &[]);
    argv.push(path.into());
    run_quietly(Path::new(SUDO), &argv) == 0
}

fn as_account_argv(uid: u32, aterm: &Path, args: &[&str]) -> Vec<OsString> {
    let mut argv: Vec<OsString> = ["-n", "-H", "-u"].into_iter().map(OsString::from).collect();
    argv.push(format!("#{uid}").into());
    argv.push("--".into());
    argv.push(aterm.into());
    argv.extend(args.iter().map(OsString::from));
    argv
}

fn run_helper(
    aterm: &Path,
    args: &[&str],
    account: Option<(u32, u32)>,
    run: &mut impl FnMut(&Path, &[OsString]) -> i32,
) -> i32 {
    match account {
        None => run(aterm, &args.iter().map(OsString::from).collect::<Vec<_>>()),
        Some((uid, _)) => run(Path::new(SUDO), &as_account_argv(uid, aterm, args)),
    }
}

fn helper_line(aterm: &Path, args: &[&str], account: Option<(u32, u32)>) -> String {
    let step = format!("{} {}", aterm.display(), args.join(" "));
    match account {
        None => step,
        Some((uid, _)) => format!("{SUDO} -n -H -u #{uid} -- {step}"),
    }
}

fn run_quietly(program: &Path, args: &[impl AsRef<OsStr>]) -> i32 {
    aterm_uds::spawnfd::command(program)
        .args(args)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .status()
        .map_or(127, |st| st.code().unwrap_or(128))
}

/// Remove every artifact `install.sh` placed under `p`.
pub fn self_uninstall(
    p: &Places,
    man_src: Option<&Path>,
    dry: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> bool {
    self_uninstall_with_acaffeine(p, man_src, dry, out, err, sweep_acaffeine, CHILD_HANG)
}

/// [`self_uninstall`] with its aCaffeine sweep, and `hang`, how long a bundle's program is given
/// to answer before its bundle is kept.
fn self_uninstall_with_acaffeine(
    p: &Places,
    man_src: Option<&Path>,
    dry: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
    mut acaffeine: impl FnMut(&mut Sweep<'_>, &Places, &[PathBuf]) -> Vec<PathBuf>,
    hang: Duration,
) -> bool {
    let outsider = p.install_dir.as_ref().and_then(|_| scope_outsider(p));
    let mut s = Sweep {
        dry,
        as_root: if aterm_uds::peer::our_uid() == 0 {
            Some(run_quietly)
        } else {
            None
        },
        removed: 0,
        skipped: 0,
        kept: 0,
        shared_kept: 0,
        outsider,
        out,
        err,
    };
    let root = s.as_root.is_some();
    let support = p.support();
    let decline = support.join("pkg/declined");
    let decline_tagged =
        read_checked(&decline).is_ok_and(|text| text.lines().any(|line| line == SWEPT_DECLINE_TAG));
    let decline_before = present(&decline) && !decline_tagged;
    let app_dirs = p.app_dirs();

    let atpkg_helper = uninstall_helper(p);
    let mut swept = false;
    let mut atpkg_kept: Option<PathBuf> = None;
    // The atpkg whose run this sweep saw exit: past macOS's check of it.
    let mut atpkg_proven: Option<PathBuf> = None;
    match (&atpkg_helper, s.outsider.is_some()) {
        (Some(_), true) => {
            s.say("left in place: the ALab toolchain store");
            s.kept += 1;
            s.shared_kept += 1;
        }
        (Some(helper), false) => {
            let app = &helper.root;
            let atpkg = helper.program.clone();
            let invocation = format!("{} {}", atpkg.display(), helper.args.join(" "));
            if dry {
                let act = s.act();
                s.say(&format!("{act} the ALab toolchain store: {invocation}"));
                s.removed += 1;
                swept = true;
            } else {
                let run_as = runs_as(aterm_uds::peer::our_uid(), &p.home, &atpkg);
                let refused = match run_as {
                    RunAs::Me => None,
                    RunAs::Account(uid) => sudo_refuses(uid)
                        .map(|rc| format!("sudo could not run it as uid {uid} (exit {rc})")),
                    RunAs::Nobody => Some(NOT_AS_ROOT.to_string()),
                };
                let mut cmd = match run_as {
                    RunAs::Account(uid) => {
                        let mut cmd = aterm_uds::spawnfd::command(Path::new(SUDO));
                        cmd.env("LC_ALL", "C")
                            .args(as_account_argv(uid, &atpkg, helper.args));
                        cmd
                    }
                    RunAs::Me | RunAs::Nobody => {
                        let mut cmd = aterm_uds::spawnfd::command(&atpkg);
                        cmd.args(helper.args);
                        cmd
                    }
                };
                // Through sudo, its words say whether the kernel refused atpkg: kept in a file,
                // which nothing atpkg starts can hold open as it could a pipe.
                let said = matches!(run_as, RunAs::Account(_))
                    .then(aterm_tempfile::tempdir)
                    .and_then(Result::ok)
                    .map(|dir| {
                        let file = dir.path().join("stderr");
                        (dir, file)
                    });
                let stderr = said
                    .as_ref()
                    .and_then(|(_, file)| {
                        let mut opts = std::fs::OpenOptions::new();
                        opts.write(true).create_new(true);
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::OpenOptionsExt as _;
                            opts.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
                        }
                        opts.open(file).ok()
                    })
                    .map_or_else(
                        aterm_uds::spawnfd::Stdio::null,
                        aterm_uds::spawnfd::Stdio::from,
                    );
                let status = match refused {
                    Some(why) => Err(why),
                    None => Ok(cmd
                        .stdin(aterm_uds::spawnfd::Stdio::null())
                        .stdout(aterm_uds::spawnfd::Stdio::null())
                        .stderr(stderr)
                        .status()
                        .ok()),
                };
                // Only an exit shows the run got past macOS's check of atpkg: it shares this
                // process group and has no deadline, so a signal that ended it may be anyone's.
                let exited = matches!(status, Ok(Some(st)) if st.code().is_some());
                let code = match &status {
                    Ok(Some(st)) => st.code(),
                    _ => None,
                };
                let exec_refused = said
                    .as_ref()
                    .and_then(|(_, file)| {
                        crate::metadata_io::read_bounded_regular(file, 64 * 1024).ok()
                    })
                    .is_some_and(|err| sudo_refused_exec(run_as, code, &err, &atpkg));
                if exec_refused || (exited && !sudo_may_not_have_run(run_as, code)) {
                    atpkg_proven = Some(atpkg.clone());
                }
                // A refusal sudo names is no failure of atpkg's: it could not run.
                let ran = status.map(|st| {
                    st.map_or(127, |st| {
                        if exec_refused {
                            return 127;
                        }
                        st.code().unwrap_or_else(|| {
                            128 + std::os::unix::process::ExitStatusExt::signal(&st).unwrap_or(0)
                        })
                    })
                });
                if ran == Ok(0) {
                    s.say("removed the ALab toolchain store");
                    swept = true;
                    s.removed += 1;
                } else if let Ok(rc @ 126..) = ran {
                    s.skip(
                        "the ALab toolchain store",
                        &format!(
                            "`{invocation}` could not run (exit {rc}) — reinstall aterm, \
                             then uninstall again"
                        ),
                    );
                    if helper.stored {
                        atpkg_kept = Some(app.clone());
                    }
                } else {
                    let why = match ran {
                        Err(why) => why,
                        Ok(rc) if rc == i32::from(crate::lock::CONTENDED_EXIT) => {
                            "another aterm pkg run holds the store — quit aterm, or let its \
                             update pass finish"
                                .to_string()
                        }
                        Ok(rc) => format!("it failed (exit {rc}) — run it by hand to see why"),
                    };
                    s.skip(
                        "the ALab toolchain store",
                        &format!(
                            "`{invocation}`: {why}; {} is kept for it — then uninstall \
                             again",
                            app.display()
                        ),
                    );
                    atpkg_kept = Some(app.clone());
                }
            }
        }
        (None, _) => {}
    }

    let mut acaffeine_kept = acaffeine(&mut s, p, &app_dirs);
    if let Some(kept) = &atpkg_kept {
        acaffeine_kept.push(kept.clone());
    }
    sweep_setup_job(&mut s, p);

    for dir in &app_dirs {
        for name in ["aterm.app", "aterm.app.rollback"] {
            let app = dir.join(name);
            if !app.is_dir() {
                continue;
            }
            if atpkg_kept.as_ref() == Some(&app) {
                s.skip(
                    &app.display().to_string(),
                    "kept: its atpkg is what removes the ALab toolchain store, and that removal \
                     did not finish (above)",
                );
            } else if acaffeine_kept.contains(&app) {
                s.skip(
                    &app.display().to_string(),
                    "kept: its `aterm acaffeine` is what stops the aCaffeine mug, and that did \
                     not finish (above)",
                );
            } else if bundle_is_ours(&app) {
                let proven = atpkg_proven
                    .as_deref()
                    .filter(|atpkg| atpkg.starts_with(&app));
                match bundle_unproven(&mut s, p, &app, proven, hang) {
                    Some(why) => s.skip(&app.display().to_string(), &format!("kept: {why}")),
                    None => {
                        let command = app.join("Contents/MacOS/aterm");
                        let had_command = std::fs::symlink_metadata(&command).is_ok();
                        if s.rm(&app, "app bundle") && had_command {
                            remove_command_declaration(&mut s, p, &command);
                        }
                    }
                }
            } else {
                let why = bundle_identifier(&app).map_or_else(
                    || {
                        "not proven an aterm bundle (no readable CFBundleIdentifier): if it is \
                         a damaged aterm, remove it by hand"
                            .to_string()
                    },
                    |id| format!("not an aterm bundle (CFBundleIdentifier={id})"),
                );
                s.skip(&app.display().to_string(), &why);
            }
        }
    }

    if (swept || decline_tagged) && !dry && !decline_before && decline.is_file() {
        let app_left = app_dirs.iter().any(|d| {
            let app = d.join("aterm.app");
            app.is_dir() && bundle_may_be_aterm(&app)
        });
        if !app_left {
            let _ = s.unlink(&decline, true);
        } else if !decline_tagged {
            let tagged = open_checked(&decline, std::fs::OpenOptions::new().append(true), root)
                .and_then(|mut file| writeln!(file, "{SWEPT_DECLINE_TAG}"));
            if let Err(e) = tagged {
                s.say(&format!(
                    "could not mark {} as this uninstall's: {e}",
                    decline.display()
                ));
            }
        }
    }

    for bd in p.bin_dirs() {
        // The bundle's alias links the hooks pass lays beside `aterm` (`hooks::COMMAND_LINKS`):
        // each goes with the bundle, and a link elsewhere is someone's own.
        for (name, what) in [
            ("atpkg", "atpkg command"),
            ("amacs", "amacs command"),
            ("agh", "agh command"),
        ] {
            let alias_link = bd.join(name);
            if let Some(t) = link_target(&alias_link) {
                if t.contains("/aterm.app/Contents/MacOS/") {
                    rm_link(&mut s, p, &alias_link, what);
                } else {
                    s.skip(
                        &alias_link.display().to_string(),
                        "symlink points outside an aterm bundle",
                    );
                }
            }
        }
        let bin = bd.join("aterm");
        if bin_entry_is_ours(&bin, &p.store_dir) {
            if rm_link(&mut s, p, &bin, "aterm command") {
                remove_command_declaration(&mut s, p, &bin);
            }
        } else if let Some(t) = link_target(&bin) {
            s.skip(
                &bin.display().to_string(),
                &format!("symlink points outside an aterm bundle or store ({t})"),
            );
        } else if present(&bin) {
            s.skip(
                &bin.display().to_string(),
                "not a symlink — a hand-built binary this installer did not place",
            );
        }
    }

    sweep_store(&mut s, p, &acaffeine_kept);

    for section in 1..=9 {
        let dir = p.man_root.join(format!("man{section}"));
        for page in sorted_entries(&dir) {
            let Some(base) = page
                .file_name()
                .and_then(|n| n.to_str())
                .map(str::to_string)
            else {
                continue;
            };
            let man_page = base.starts_with("aterm")
                && base
                    .rsplit_once('.')
                    .is_some_and(|(_, ext)| matches!(ext.as_bytes(), [b'1'..=b'9']));
            if !man_page || !page.exists() {
                continue;
            }
            if SHIPPED_MAN_PAGES.contains(&base.as_str()) {
                s.rm_shared(&page, "man page");
            } else if !man_src.is_some_and(Path::is_dir) {
                s.skip(
                    &page.display().to_string(),
                    "not a man page this installer ships (run from a checkout to sweep newer \
                     pages)",
                );
            } else if !man_src.is_some_and(|src| src.join(&base).exists()) {
                s.skip(
                    &page.display().to_string(),
                    "not a man page this repo ships",
                );
            } else {
                s.rm_shared(&page, "man page");
            }
        }
    }

    for comp in completion_files(p) {
        if comp.exists() {
            s.rm_shared(&comp, "shell completion");
        }
    }

    let desktop = p.xdg_data.join("applications/aterm.desktop");
    let mut desktop_owned = false;
    if crate::metadata_io::read_bounded_regular_utf8(&desktop, 64 * 1024)
        .is_ok_and(|text| text.lines().any(|l| l == DESKTOP_MARKER))
    {
        desktop_owned = true;
        s.rm(&desktop, "desktop entry");
        if !dry {
            refresh_desktop_database(&ShellEnv::of_process(), &p.xdg_data, |update| {
                runs_as(aterm_uds::peer::our_uid(), &p.home, update)
            });
        }
    } else if std::fs::symlink_metadata(&desktop)
        .is_ok_and(|m| m.is_file() || m.file_type().is_symlink())
        || desktop.is_file()
    {
        s.skip(
            &desktop.display().to_string(),
            "not a desktop entry this installer wrote (missing its marker line)",
        );
    }
    for size in ICON_SIZES {
        let icon = p
            .xdg_data
            .join(format!("icons/hicolor/{size}/apps/aterm.png"));
        if !icon.exists() {
            continue;
        }
        if desktop_owned {
            s.rm(&icon, "app icon");
        } else {
            s.skip(
                &icon.display().to_string(),
                "no marker-owned aterm.desktop vouches for it",
            );
        }
    }

    if support.is_dir() {
        if dry {
            if std::fs::read_dir(&support).is_ok_and(|mut rd| rd.next().is_none()) {
                let act = s.act();
                s.say(&format!("{act} empty support dir: {}", support.display()));
                s.removed += 1;
            }
        } else if s.unlink(&support, false).is_ok() {
            s.say(&format!("removed empty support dir: {}", support.display()));
            s.removed += 1;
        }
    }

    sweep_path_blocks(&mut s, p);

    if let Some(other) = s.outsider.clone()
        && s.shared_kept > 0
    {
        let n = s.shared_kept;
        s.say(&format!(
            "left in place: {n} account-wide item(s) above — {}, another aterm on this \
             account, uses them",
            other.display()
        ));
    }
    if s.removed == 0 && s.skipped == 0 && s.kept == 0 {
        s.say("nothing installed by install.sh was found — nothing to do");
        return true;
    }
    let verb = if dry { "dry run" } else { "uninstall" };
    let tail = if s.skipped > 0 {
        format!(", {} skipped", s.skipped)
    } else {
        String::new()
    };
    let removed = s.removed;
    s.say(&format!("{verb} complete — {removed} item(s){tail}"));
    let settings = p.xdg_config.join("aterm");
    if swept || (atpkg_helper.is_some() && s.outsider.is_some()) {
        s.say(&format!(
            "left in place: your settings and themes in {}",
            settings.display()
        ));
    } else {
        s.say(&format!(
            "left in place: your settings and themes in {}, and any atpkg toolchain",
            settings.display()
        ));
    }
    if decline.is_file() {
        s.say(&format!(
            "left in place: the ALab toolset decline ({}) — installing aterm again installs \
             no toolset until `aterm pkg install --default-set`",
            decline.display()
        ));
    }
    s.skipped == 0
}

fn rm_link(s: &mut Sweep<'_>, p: &Places, link: &Path, label: &str) -> bool {
    match link_bundle(link) {
        Some(b) if other_aterm(p, &b) => {
            s.say(&format!(
                "left in place: {} — it leads into {}, another aterm than ATERM_INSTALL_DIR's",
                link.display(),
                b.display()
            ));
            s.kept += 1;
            false
        }
        _ => s.rm(link, label),
    }
}

fn remove_command_declaration(s: &mut Sweep<'_>, p: &Places, command: &Path) {
    let our_uid = aterm_uds::peer::our_uid();
    let owner = match home_as(our_uid, &p.home) {
        RunAs::Me => Ok(our_uid),
        RunAs::Account(uid) => Ok(uid),
        RunAs::Nobody => Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            NOT_AS_ROOT,
        )),
    };
    match owner
        .and_then(|uid| aterm_types::installed_command::remove_if_matches(&p.home, command, uid))
    {
        Ok(true) => {
            s.say("removed the matching installed command declaration");
            s.removed += 1;
        }
        Ok(false) => {}
        Err(error) => {
            let _ = writeln!(
                s.err,
                "atpkg: SKIPPED installed command declaration after removing {}: {error}",
                command.display()
            );
            s.skipped += 1;
        }
    }
}

fn sorted_entries(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<OsString> = rd.flatten().map(|e| e.file_name()).collect();
    names.sort_by_key(|n| (n.to_string_lossy().starts_with('.'), n.clone()));
    names.into_iter().map(|n| dir.join(n)).collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StoreLeft {
    Absent,
    PathDir,
    Unlisted,
}

fn store_left(p: &Places) -> Option<StoreLeft> {
    let store = &p.store_dir;
    if !store.is_dir() {
        Some(StoreLeft::Absent)
    } else if p.bin_dirs().iter().any(|bd| same_file(store, bd)) {
        Some(StoreLeft::PathDir)
    } else if std::fs::read_dir(store).is_err() {
        Some(StoreLeft::Unlisted)
    } else {
        None
    }
}

fn sweep_store(s: &mut Sweep<'_>, p: &Places, acaffeine_kept: &[PathBuf]) {
    let store = &p.store_dir;
    match store_left(p) {
        None => {}
        Some(StoreLeft::Absent) => return,
        Some(StoreLeft::PathDir) => {
            s.skip(
                &store.display().to_string(),
                "it is the PATH directory itself, not a store of its own — nothing in it was \
                 removed as the store's (ATERM_STORE_DIR needs a directory of its own)",
            );
            return;
        }
        Some(StoreLeft::Unlisted) => {
            s.skip(
                &store.display().to_string(),
                "cannot be listed, so what it holds cannot be judged — left in place",
            );
            return;
        }
    }
    let aterm = store.join("aterm");
    let run_as = runs_as(aterm_uds::peer::our_uid(), &p.home, &aterm);
    let unasked = match run_as {
        _ if !is_executable_file(&aterm) || acaffeine_kept.contains(&aterm) => None,
        RunAs::Nobody => Some(NOT_AS_ROOT.to_string()),
        RunAs::Account(uid) => {
            sudo_refuses(uid).map(|rc| format!("sudo could not run it as uid {uid} (exit {rc})"))
        }
        RunAs::Me => None,
    };
    if let Some(why) = unasked {
        s.skip(
            &store.display().to_string(),
            &format!("its aterm was not asked what it is: {why} — left in place"),
        );
        return;
    }
    let (kept, rest): (Vec<PathBuf>, Vec<PathBuf>) = sorted_entries(store)
        .into_iter()
        .partition(|entry| acaffeine_kept.contains(entry));
    let (own, foreign): (Vec<PathBuf>, Vec<PathBuf>) = rest
        .into_iter()
        .partition(|entry| store_entry_is_ours(entry, run_as));
    let is_link = std::fs::symlink_metadata(store).is_ok_and(|m| m.file_type().is_symlink());
    if foreign.is_empty() && kept.is_empty() && !is_link {
        let had_command = own.contains(&aterm);
        if s.rm(store, "source-built toolset") && had_command {
            remove_command_declaration(s, p, &aterm);
        }
        return;
    }
    for entry in &kept {
        s.skip(
            &entry.display().to_string(),
            "kept: its `aterm acaffeine` is what stops the aCaffeine mug, and that did not \
             finish (above)",
        );
    }
    for entry in &own {
        if s.rm(entry, "source-built toolset entry") && entry == &aterm {
            remove_command_declaration(s, p, &aterm);
        }
    }
    if !foreign.is_empty() {
        let mut shown = foreign
            .iter()
            .take(3)
            .filter_map(|e| e.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect::<Vec<_>>()
            .join(", ");
        if foreign.len() > 3 {
            shown.push_str(&format!(" and {} more", foreign.len() - 3));
        }
        s.skip(
            &store.display().to_string(),
            &format!(
                "holds files this installer did not place ({shown}) — left in place with them"
            ),
        );
    }
}

fn sweep_path_blocks(s: &mut Sweep<'_>, p: &Places) {
    let mut rcs: Vec<PathBuf> = Vec::new();
    for rc in [
        p.zshrc().unwrap_or_else(|| p.home.join(".zshrc")),
        p.home.join(".zshrc"),
        p.home.join(".bashrc"),
        p.home.join(".bash_profile"),
        p.home.join(".bash_login"),
        p.home.join(".profile"),
        p.xdg_config.join("fish/config.fish"),
        p.home.join(".config/fish/config.fish"),
    ] {
        if !rcs.contains(&rc) {
            rcs.push(rc);
        }
    }
    let root = s.as_root.is_some();
    for rc in rcs {
        if !rc.is_file() {
            continue;
        }
        let Ok(text) = read_checked(&rc) else {
            continue;
        };
        if !text.contains(PATH_BLOCK_START) {
            continue;
        }
        if s.outsider.is_some() {
            s.rm_shared(&rc, "PATH block");
            continue;
        }
        if !text.contains(PATH_BLOCK_END) {
            s.skip(
                &rc.display().to_string(),
                "start marker without its end marker — delete the aterm block by hand",
            );
            continue;
        }
        if s.dry {
            let act = s.act();
            s.say(&format!("{act} PATH block: {}", rc.display()));
            s.removed += 1;
            continue;
        }
        match without_block(&text) {
            Some(kept)
                if open_checked(&rc, std::fs::OpenOptions::new().write(true), root)
                    .and_then(|mut f| f.set_len(0).and_then(|()| f.write_all(kept.as_bytes())))
                    .is_ok() =>
            {
                s.say(&format!("removed PATH block: {}", rc.display()));
                s.removed += 1;
            }
            _ => s.skip(
                &rc.display().to_string(),
                "could not rewrite the profile — delete the aterm block by hand",
            ),
        }
    }
}

fn without_block(text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut dropping = false;
    for line in text.split_inclusive('\n') {
        let bare = line.strip_suffix('\n').unwrap_or(line);
        if bare == PATH_BLOCK_START {
            dropping = true;
        } else if bare == PATH_BLOCK_END {
            dropping = false;
        } else if !dropping {
            out.push_str(line);
        }
    }
    (!dropping).then_some(out)
}

/// The profile the managed PATH block lands in for `env`'s shell, zsh's in [`Places::zdotdir`]
/// (none while that is unknown); [`crate::hooks::login_rc`] asks it too.
pub(crate) fn rc_target(p: &Places, env: &ShellEnv) -> Option<PathBuf> {
    match env.shell_name() {
        "zsh" => p.zshrc(),
        "bash" if cfg!(target_os = "macos") => Some(
            [".bash_profile", ".bash_login", ".profile"]
                .iter()
                .map(|f| p.home.join(f))
                .find(|f| f.is_file())
                .unwrap_or_else(|| p.home.join(".bash_profile")),
        ),
        "bash" => Some(p.home.join(".bashrc")),
        "fish" => Some(p.xdg_config.join("fish/config.fish")),
        _ => None,
    }
}

fn hook_of(p: &Places, shell: &str) -> PathBuf {
    p.home.join(format!(
        ".aterm/shell.d/{}.{shell}",
        crate::hooks::HOOK_BASENAME
    ))
}

/// Options for [`shell_setup`].
#[derive(Clone, Debug, Default)]
pub struct Setup {
    /// Explicitly record this placed command, only when it resolves to this
    /// running executable. A declaration alone changes no shell integration.
    pub installed_command: Option<PathBuf>,
    /// Wire the managed PATH block.
    pub path: bool,
    /// Write the shell completions.
    pub completions: bool,
    /// Install the man pages of this directory (a checkout's `man/`).
    pub man: Option<PathBuf>,
    /// Install the man pages this binary carries ([`Setup::man`]).
    pub own_man: bool,
    /// Write the Linux desktop entry for `<bin dir>/aterm`.
    pub desktop: bool,
    /// The hicolor icon tree to install beside the desktop entry.
    pub icons: Option<PathBuf>,
    /// The toolset hook is coming (a first launch lays it).
    pub toolset_coming: bool,
    /// Say what would be done, and do nothing.
    pub dry_run: bool,
}

/// Execute `aterm pkg shell-setup`.
pub fn shell_setup(
    p: &Places,
    env: &ShellEnv,
    exe: &Path,
    setup: &Setup,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> bool {
    let mut ok = true;
    if setup.path {
        ok = if setup.dry_run {
            plan_path(p, env, setup.toolset_coming, out);
            true
        } else {
            wire_path(p, env, setup.toolset_coming, out, err)
        };
    }
    if let Some(src) = &setup.man {
        let from = src.display().to_string();
        install_man_pages(p, env, &checkout_pages(src), &from, setup.dry_run, out, err);
    } else if setup.own_man {
        install_man_pages(
            p,
            env,
            &carried_pages(),
            "this binary",
            setup.dry_run,
            out,
            err,
        );
    }
    if setup.completions {
        install_completions(p, env, exe, setup.dry_run, out, err);
    }
    if setup.desktop {
        install_desktop_entry(p, env, setup.icons.as_deref(), setup.dry_run, out, err);
    }
    if let Some(command) = &setup.installed_command {
        if setup.dry_run {
            let _ = writeln!(
                out,
                "atpkg: would record installed command: {}",
                command.display()
            );
        } else {
            match aterm_types::installed_command::record(&p.home, command, exe) {
                Ok(()) => {
                    let _ = writeln!(
                        out,
                        "atpkg: recorded installed command: {}",
                        command.display()
                    );
                }
                Err(error) => {
                    let _ = writeln!(err, "atpkg: installed command was not recorded: {error}");
                    ok = false;
                }
            }
        }
    }
    ok
}

fn plan_path(p: &Places, env: &ShellEnv, coming: bool, out: &mut dyn Write) {
    let line = match rc_target(p, env) {
        None if let Some(why) = zsh_folder_unknown(p, env) => {
            format!("PATH: no edit — {why}")
        }
        None => format!(
            "PATH: unrecognised shell ({}) — no profile is edited; a source hint prints \
             instead",
            env.shell.as_deref().unwrap_or("<unset>")
        ),
        Some(rc) if read_checked(&rc).is_ok_and(|t| t.contains(&path_block(p, env))) => {
            format!(
                "PATH: already wired — {} carries the managed block; no edit",
                rc.display()
            )
        }
        Some(_) if !(coming || hook_of(p, env.shell_name()).is_file() || !bin_on_path(p, env)) => {
            format!(
                "PATH: no edit — {} is on PATH and no ALab toolset is installed",
                p.bin_dir.display()
            )
        }
        Some(rc) => {
            let verb = if read_checked(&rc).is_ok_and(|t| t.contains(PATH_BLOCK_START)) {
                "updates the"
            } else {
                "appends ONE"
            };
            let hook = if coming || hook_of(p, env.shell_name()).is_file() {
                "the toolset hook + "
            } else {
                ""
            };
            format!(
                "PATH: {verb} marker-fenced block to {} — {hook}{}",
                rc.display(),
                p.bin_dir.display()
            )
        }
    };
    let _ = writeln!(out, "atpkg: {line}");
}

/// Why a zsh account's `.zshrc` is not elected: which folder zsh reads it from is unknown
/// ([`crate::zdotdir::ZshDir::Unknown`]); `None` for any other shell or a known folder.
fn zsh_folder_unknown(p: &Places, env: &ShellEnv) -> Option<String> {
    match &p.zdotdir {
        crate::zdotdir::ZshDir::Unknown(why) if env.shell_name() == "zsh" => Some(format!(
            "which folder zsh reads .zshrc from is unknown ({why})"
        )),
        _ => None,
    }
}

/// Whether `env`'s PATH already names the bin dir (the managed block's one line besides the
/// toolset hook).
fn bin_on_path(p: &Places, env: &ShellEnv) -> bool {
    env.path
        .as_ref()
        .is_some_and(|path| std::env::split_paths(path).any(|dir| dir == p.bin_dir))
}

fn path_block(p: &Places, env: &ShellEnv) -> String {
    let hook = hook_of(p, env.shell_name());
    let body = if env.shell_name() == "fish" {
        let bin = crate::hooks::fish_quote(&p.bin_dir);
        let hook = crate::hooks::fish_quote(&hook);
        format!(
            "set -l __aterm_cli_bin \"{bin}\"\n\
             set -l __aterm_cli_rest\n\
             for __aterm_cli_entry in $PATH; if test \"$__aterm_cli_entry\" != \"$__aterm_cli_bin\"; set __aterm_cli_rest $__aterm_cli_rest \"$__aterm_cli_entry\"; end; end\n\
             set -gx PATH \"$__aterm_cli_bin\" $__aterm_cli_rest\n\
             if test -f \"{hook}\"; source \"{hook}\"; end\n"
        )
    } else {
        let bin = crate::hooks::sh_quote(&p.bin_dir);
        let hook = crate::hooks::sh_quote(&hook);
        format!(
            "__aterm_cli_bin=\"{bin}\"\n\
             __aterm_cli_path=\":$PATH:\"\n\
             while :; do __aterm_cli_next=\"${{__aterm_cli_path//\":$__aterm_cli_bin:\"/:}}\"; [ \"$__aterm_cli_next\" = \"$__aterm_cli_path\" ] && break; __aterm_cli_path=\"$__aterm_cli_next\"; done\n\
             case \"$__aterm_cli_path\" in :) export PATH=\"$__aterm_cli_bin\" ;; *) __aterm_cli_path=\"${{__aterm_cli_path#:}}\"; __aterm_cli_path=\"${{__aterm_cli_path%:}}\"; export PATH=\"$__aterm_cli_bin:$__aterm_cli_path\" ;; esac\n\
             unset __aterm_cli_bin __aterm_cli_path __aterm_cli_next\n\
             if [ -f \"{hook}\" ]; then . \"{hook}\"; fi\n"
        )
    };
    format!("{PATH_BLOCK_START}\n{body}{PATH_BLOCK_END}\n")
}

fn profile_with_path_block(text: &str, block: &str) -> Result<String, &'static str> {
    let (mut start, mut end, mut offset) = (None, None, 0);
    for line in text.split_inclusive('\n') {
        match line.trim_end_matches(['\r', '\n']) {
            PATH_BLOCK_START if start.is_none() && end.is_none() => start = Some(offset),
            PATH_BLOCK_END if start.is_some() && end.is_none() => end = Some(offset + line.len()),
            PATH_BLOCK_START | PATH_BLOCK_END => {
                return Err("the profile has ambiguous PATH block markers");
            }
            _ => {}
        }
        offset += line.len();
    }
    match (start, end) {
        (Some(start), Some(end)) => Ok(format!("{}{block}{}", &text[..start], &text[end..])),
        (None, None) => {
            let separator = if text.is_empty() || text.ends_with('\n') {
                "\n"
            } else {
                "\n\n"
            };
            Ok(format!("{text}{separator}{block}"))
        }
        _ => Err("the profile has an incomplete PATH block"),
    }
}

fn wire_path(
    p: &Places,
    env: &ShellEnv,
    coming: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> bool {
    let shell = env.shell_name();
    let Some(rc) = rc_target(p, env) else {
        if let Some(why) = zsh_folder_unknown(p, env) {
            let _ = writeln!(
                err,
                "atpkg: PATH: no edit — {why}; put {} on PATH from it yourself",
                p.bin_dir.display()
            );
            return false;
        }
        if hook_of(p, "bash").is_file() {
            let _ = writeln!(
                err,
                "atpkg: NOTE: source ~/.aterm/shell.d/{}.* from your {} profile to get the ALab \
                 toolset on PATH",
                crate::hooks::HOOK_BASENAME,
                if shell.is_empty() { "shell" } else { shell }
            );
        }
        return false;
    };
    let hook = hook_of(p, shell);
    if !hook.is_file() && !coming && bin_on_path(p, env) {
        let _ = writeln!(
            out,
            "atpkg: PATH: no edit — {} is on PATH and no ALab toolset is installed",
            p.bin_dir.display()
        );
        return false;
    }
    let text = match read_checked(&rc) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            let _ = writeln!(err, "atpkg: cannot read {}: {e}", rc.display());
            return false;
        }
    };
    let block = path_block(p, env);
    let updated = match profile_with_path_block(&text, &block) {
        Ok(updated) => updated,
        Err(why) => {
            let _ = writeln!(err, "atpkg: {}: {why}; no edit", rc.display());
            return false;
        }
    };
    let bin = p.bin_dir.display();
    if updated == text {
        // What the standing block puts on PATH, as the write below says it: the toolset only
        // when its hook is laid (round 12: a --no-toolchain rerun said "ALab toolset already on
        // PATH" with no toolset installed or coming).
        let rc_s = rc.display();
        let said = if hook.is_file() {
            format!("ALab toolset already on PATH via {rc_s}")
        } else if coming {
            format!(
                "{bin} already on PATH via {rc_s}; the ALab toolset joins it after aterm's first \
                 launch"
            )
        } else {
            format!("{bin} already on PATH via {rc_s}")
        };
        let _ = writeln!(out, "atpkg: {said}");
        return true;
    }
    let destination = if rc.is_symlink() {
        match rc.canonicalize() {
            Ok(path) => path,
            Err(e) => {
                let _ = writeln!(err, "atpkg: cannot resolve {}: {e}", rc.display());
                return false;
            }
        }
    } else {
        rc.clone()
    };
    if let Some(parent) = destination.parent() {
        let _ = made_dir(parent);
    }
    let tmp = destination.with_file_name(format!(".aterm-profile.{}", std::process::id()));
    let wrote = (|| {
        let mode = destination
            .metadata()
            .map(|m| m.mode() & 0o777)
            .unwrap_or(0o600);
        let mut file = crate::platform::open_create_new_write(&tmp, mode)?;
        let written = file
            .write_all(updated.as_bytes())
            .and_then(|()| file.set_permissions(std::fs::Permissions::from_mode(mode)));
        drop(file);
        written
            .and_then(|()| std::fs::rename(&tmp, &destination))
            .inspect_err(|_| {
                let _ = std::fs::remove_file(&tmp);
            })
    })();
    if wrote.is_err() {
        let _ = writeln!(
            err,
            "atpkg: NOTE: could not write {} — add this line yourself for the ALab toolset on \
             PATH:\n{block}",
            rc.display()
        );
        return false;
    }
    let rc_s = rc.display();
    let said = if hook.is_file() {
        format!("added the ALab toolset and {bin} to PATH in {rc_s} — open a new shell")
    } else if coming {
        format!(
            "added {bin} to PATH in {rc_s} — open a new shell; the ALab toolset joins it after \
             aterm's first launch"
        )
    } else {
        format!("added {bin} to PATH in {rc_s} — open a new shell")
    };
    let _ = writeln!(out, "atpkg: {said}");
    true
}

type Page = (String, OsString, Cow<'static, [u8]>);

fn checkout_pages(src: &Path) -> Vec<Page> {
    sorted_entries(src)
        .into_iter()
        .filter_map(|page| {
            let section = page
                .extension()
                .and_then(|e| e.to_str())
                .filter(|e| matches!(e.as_bytes(), [b'1'..=b'9']))?
                .to_string();
            let name = page.file_name()?.to_os_string();
            let bytes = crate::metadata_io::read_bounded_regular(&page, 1024 * 1024).ok()?;
            Some((section, name, Cow::Owned(bytes)))
        })
        .collect()
}

fn carried_pages() -> Vec<Page> {
    CARRIED_MAN_PAGES
        .iter()
        .map(|&(section, name, page)| (section.to_string(), name.into(), Cow::Borrowed(page)))
        .collect()
}

fn made_dir(dir: &Path) -> std::io::Result<()> {
    aterm_types::fs_restricted::create_dir_all_unshared(dir, 0o755)
}

fn write_644(dir: &Path, name: &OsStr, bytes: &[u8]) -> std::io::Result<()> {
    made_dir(dir)?;
    let dest = dir.join(name);
    let (tmp, mut file) = crate::platform::create_temp_beside(&dest, 0o644)?;
    let written = file
        .write_all(bytes)
        .and_then(|()| file.set_permissions(std::fs::Permissions::from_mode(0o644)));
    drop(file);
    written
        .and_then(|()| std::fs::rename(&tmp, &dest))
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
}

fn install_man_pages(
    p: &Places,
    env: &ShellEnv,
    pages: &[Page],
    from: &str,
    dry: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) {
    if dry {
        if !pages.is_empty() {
            let _ = writeln!(
                out,
                "atpkg: man pages: would install {} page(s) from {from} -> {}",
                pages.len(),
                p.man_root.display()
            );
        }
        return;
    }
    let mut n = 0usize;
    for (section, name, bytes) in pages {
        let dir = p.man_root.join(format!("man{section}"));
        if write_644(&dir, name, bytes).is_ok() {
            n += 1;
        }
    }
    if n == 0 {
        return;
    }
    let _ = writeln!(
        out,
        "atpkg: installed {n} man page(s) -> {} (e.g. man aterm)",
        p.man_root.display()
    );
    if let Some(manpath) = env.manpath.as_deref().filter(|m| !m.is_empty())
        && !manpath
            .split(':')
            .any(|entry| Path::new(entry) == p.man_root)
    {
        let _ = writeln!(
            err,
            "atpkg: NOTE: {} is not in $MANPATH — add it to run `man aterm`",
            p.man_root.display()
        );
    }
}

fn completion_entries(p: &Places) -> [(&'static str, bool, PathBuf); 6] {
    let zsh = p.xdg_data.join("zsh/site-functions");
    let bash = p.xdg_data.join("bash-completion/completions");
    let fish = p.xdg_config.join("fish/completions");
    [
        ("bash", false, bash.join("aterm-ctl")),
        ("zsh", false, zsh.join("_aterm-ctl")),
        ("fish", false, fish.join("aterm-ctl.fish")),
        ("bash", true, bash.join("aterm")),
        ("zsh", true, zsh.join("_aterm")),
        ("fish", true, fish.join("aterm.fish")),
    ]
}

fn completion_files(p: &Places) -> Vec<PathBuf> {
    completion_entries(p)
        .into_iter()
        .map(|(_, _, f)| f)
        .collect()
}

fn write_setup_file(tmp: &Path, dest: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = crate::platform::open_create_new_write(tmp, 0o666)?;
    let written = file.write_all(bytes);
    drop(file);
    written
        .and_then(|()| std::fs::rename(tmp, dest))
        .inspect_err(|_| {
            let _ = std::fs::remove_file(tmp);
        })
}

fn install_completions(
    p: &Places,
    env: &ShellEnv,
    exe: &Path,
    dry: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) {
    let zsh_dir = p.xdg_data.join("zsh/site-functions");
    let bash_loader = [
        "/opt/homebrew/etc/profile.d/bash_completion.sh",
        "/usr/local/etc/profile.d/bash_completion.sh",
        "/etc/profile.d/bash_completion.sh",
        "/usr/share/bash-completion/bash_completion",
    ]
    .iter()
    .any(|f| Path::new(f).is_file());
    let tilde = zsh_dir
        .strip_prefix(&p.home)
        .map(|rest| format!("~/{}", rest.display()))
        .unwrap_or_default();
    let zshrc = p.zshrc().unwrap_or_else(|| p.home.join(".zshrc"));
    let zsh_loader = read_checked(&zshrc).is_ok_and(|rc| {
        rc.contains(&zsh_dir.display().to_string()) || (!tilde.is_empty() && rc.contains(&tilde))
    });
    let fish_loader = env.on_path("fish").is_some();
    let front_door =
        first_line_bounded(exe, &["--completions", "zsh"], CHILD_HANG).contains("#compdef aterm");
    let mut wrote = (false, false, false);
    for (shell, front, dest) in completion_entries(p) {
        if front && !front_door {
            continue;
        }
        if dry {
            let _ = writeln!(out, "atpkg: completions: {shell} -> {}", dest.display());
            continue;
        }
        let Some(dir) = dest.parent() else {
            continue;
        };
        if made_dir(dir).is_err() {
            continue;
        }
        let args: &[&str] = if front {
            &["--completions", shell]
        } else {
            &["ctl", "--completions", shell]
        };
        let mut cmd = aterm_uds::spawnfd::command(exe);
        cmd.args(args);
        let Ok(run) = aterm_uds::deadline::output_until(&mut cmd, Instant::now() + CHILD_HANG)
        else {
            continue;
        };
        if run.stdout.is_empty() {
            continue;
        }
        let tmp = dir.join(format!(
            ".{}.tmp.{}",
            dest.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            std::process::id()
        ));
        if write_setup_file(&tmp, &dest, &run.stdout).is_err() {
            continue;
        }
        match shell {
            "bash" => wrote.0 = true,
            "zsh" => wrote.1 = true,
            _ => wrote.2 = true,
        }
    }
    if dry {
        return;
    }
    let claimed: Vec<&str> = [
        ("bash", wrote.0 && bash_loader),
        ("zsh", wrote.1 && zsh_loader),
        ("fish", wrote.2 && fish_loader),
    ]
    .iter()
    .filter(|(_, yes)| *yes)
    .map(|(name, _)| *name)
    .collect();
    if !claimed.is_empty() {
        let _ = writeln!(
            out,
            "atpkg: installed shell completions ({}) — restart your shell to load them",
            claimed.join(", ")
        );
    }
    if wrote.1 && !zsh_loader {
        let z = zsh_dir.display();
        let rc = zshrc.display();
        let _ = writeln!(
            err,
            "atpkg: NOTE: zsh completions landed in {z}, which {rc} does not put on $fpath — \
             add:\n  fpath=(\"{z}\" $fpath)\n  autoload -Uz compinit && compinit"
        );
    }
}

fn desktop_exec_path_ok(path: &Path) -> bool {
    path.to_str().is_some_and(|p| {
        p.starts_with('/')
            && p.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"/._+-".contains(&b))
    })
}

fn refresh_desktop_database(env: &ShellEnv, xdg_data: &Path, run_as: impl Fn(&Path) -> RunAs) {
    if let Some(update) = env.on_path("update-desktop-database")
        && run_as(&update) == RunAs::Me
    {
        let mut cmd = aterm_uds::spawnfd::command(update);
        cmd.arg(xdg_data.join("applications"));
        let _ = aterm_uds::deadline::output_until(&mut cmd, Instant::now() + CHILD_HANG);
    }
}

type Icon = (String, Cow<'static, [u8]>);

fn checkout_icons(dir: &Path) -> Vec<Icon> {
    sorted_entries(dir)
        .into_iter()
        .filter_map(|size| {
            let png = size.join("apps/aterm.png");
            let name = size.file_name()?.to_str()?.to_string();
            let bytes = crate::metadata_io::read_bounded_regular(&png, 1024 * 1024).ok()?;
            Some((name, Cow::Owned(bytes)))
        })
        .collect()
}

fn carried_icons() -> Vec<Icon> {
    #[cfg(target_os = "linux")]
    let carried = HICOLOR_ICONS
        .iter()
        .map(|&(size, png)| (size.to_string(), Cow::Borrowed(png)))
        .collect();
    #[cfg(not(target_os = "linux"))]
    let carried = Vec::new();
    carried
}

fn install_desktop_entry(
    p: &Places,
    env: &ShellEnv,
    icons: Option<&Path>,
    dry: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) {
    let bin = p.bin_dir.join("aterm");
    if !std::fs::metadata(&bin).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0) {
        return;
    }
    if !desktop_exec_path_ok(&bin) {
        let _ = writeln!(
            err,
            "atpkg: SKIPPED the desktop entry: {} contains characters an unquoted desktop \
             Exec= line cannot carry safely (set ATERM_BIN_DIR to a plain path)",
            bin.display()
        );
        return;
    }
    let app_dir = p.xdg_data.join("applications");
    let entry = app_dir.join("aterm.desktop");
    let sources = icons.map_or_else(carried_icons, checkout_icons);
    if dry {
        let _ = writeln!(
            out,
            "atpkg: desktop entry: would write {} (+ the aterm icon at {} size(s))",
            entry.display(),
            sources.len()
        );
        return;
    }
    if entry.is_file()
        && !read_checked(&entry).is_ok_and(|text| text.lines().any(|l| l == DESKTOP_MARKER))
    {
        let _ = writeln!(
            out,
            "atpkg: NOTE: replacing a pre-existing {} this installer did not write (no backup \
             is kept)",
            entry.display()
        );
    }
    let b = bin.display();
    let body = format!(
        "{DESKTOP_MARKER}\n[Desktop Entry]\nType=Application\nName=aterm\nGenericName=Terminal\n\
         Comment=The batteries-included terminal for AI\nTryExec={b}\nExec={b} --window\n\
         Icon=aterm\nTerminal=false\nCategories=System;TerminalEmulator;\n\
         Keywords=terminal;shell;console;command line;\nStartupNotify=true\n\
         StartupWMClass=aterm\n"
    );
    let tmp = app_dir.join(format!(".aterm.desktop.tmp.{}", std::process::id()));
    if made_dir(&app_dir)
        .and_then(|()| write_setup_file(&tmp, &entry, body.as_bytes()))
        .is_err()
    {
        let _ = writeln!(
            err,
            "atpkg: SKIPPED the desktop entry: could not write {}",
            entry.display()
        );
        return;
    }
    let mut placed = 0usize;
    for (size, png) in &sources {
        let dir = p.xdg_data.join(format!("icons/hicolor/{size}/apps"));
        if write_644(&dir, OsStr::new("aterm.png"), png).is_ok() {
            placed += 1;
        }
    }
    refresh_desktop_database(env, &p.xdg_data, |_| RunAs::Me);
    if placed > 0 {
        let _ = writeln!(
            out,
            "atpkg: installed the desktop entry -> {} (+ the aterm icon at {placed} size(s) \
             under {})",
            entry.display(),
            p.xdg_data.join("icons/hicolor").display()
        );
    } else {
        let _ = writeln!(
            out,
            "atpkg: installed the desktop entry -> {}",
            entry.display()
        );
        let _ = writeln!(
            err,
            "atpkg: NOTE: no aterm icon was written under {} — the launcher entry works, \
             with a generic icon",
            p.xdg_data.join("icons/hicolor").display()
        );
    }
}

/// [`Places`] of this process: its environment's ([`places_from`]), with zsh's folder as
/// [`crate::zdotdir::of_process`] learns it, so a `ZDOTDIR` this shell did not export is
/// zsh's own answer, not `$HOME`.
fn process_places() -> Option<Places> {
    let mut places = process_places_from_env()?;
    places.zdotdir = crate::zdotdir::of_process(&places.home);
    Some(places)
}

fn process_places_from_env() -> Option<Places> {
    let home = aterm_types::dirs::home_dir()?;
    let account = crate::platform::account_home();
    Some(places_from(&home, account.as_deref(), |name| {
        std::env::var_os(name)
    }))
}

/// [`Places`] under `home` as the environment `var` reads them: an empty variable is unset,
/// and `ZDOTDIR` counts only as an absolute path at or under `home` (or anywhere when `home`
/// is `account`, the account's own home: [`crate::zdotdir::from_env`]).
fn places_from(
    home: &Path,
    account: Option<&Path>,
    var: impl Fn(&str) -> Option<OsString>,
) -> Places {
    let env_dir = |name: &str| var(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    let mut places = Places::defaults(
        home,
        env_dir("XDG_DATA_HOME").as_deref(),
        env_dir("XDG_CONFIG_HOME").as_deref(),
    );
    if let Some(zdotdir) = crate::zdotdir::from_env(home, account, var("ZDOTDIR")) {
        places.zdotdir = crate::zdotdir::ZshDir::At(zdotdir);
    }
    places
}

fn take_dir(
    flag: &str,
    it: &mut std::slice::Iter<'_, String>,
    verb: &str,
) -> Result<PathBuf, ExitCode> {
    match it.next() {
        Some(v) if !v.is_empty() && !v.starts_with('-') => Ok(PathBuf::from(v)),
        _ => Err(crate::cli::usage_error(
            verb,
            Some(&format!("{flag} needs a directory")),
        )),
    }
}

/// CLI entry point for `aterm pkg self-uninstall`.
pub fn self_uninstall_verb(rest: &[String]) -> ExitCode {
    const VERB: &str = "self-uninstall";
    let Some(mut places) = process_places() else {
        eprintln!("atpkg {VERB}: HOME is unset — nothing to find what install.sh placed under");
        return ExitCode::from(2);
    };
    let (mut dry, mut man) = (false, None);
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        let parsed = match arg.as_str() {
            "--dry-run" => {
                dry = true;
                Ok(())
            }
            "--install-dir" => take_dir(arg, &mut it, VERB).map(|d| places.install_dir = Some(d)),
            "--bin-dir" => take_dir(arg, &mut it, VERB).map(|d| places.bin_dir = d),
            "--store-dir" => take_dir(arg, &mut it, VERB).map(|d| places.store_dir = d),
            "--man-root" => take_dir(arg, &mut it, VERB).map(|d| places.man_root = d),
            "--man" => take_dir(arg, &mut it, VERB).map(|d| man = Some(d)),
            other => Err(crate::cli::usage_error(
                VERB,
                Some(&format!("unknown argument {other:?}")),
            )),
        };
        if let Err(code) = parsed {
            return code;
        }
    }
    let ok = self_uninstall(
        &places,
        man.as_deref(),
        dry,
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    );
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// CLI entry point for `aterm pkg shell-setup`.
pub fn shell_setup_verb(rest: &[String]) -> ExitCode {
    const VERB: &str = "shell-setup";
    let Some(mut places) = process_places_from_env() else {
        eprintln!("atpkg {VERB}: HOME is unset — no profile to wire");
        return ExitCode::from(2);
    };
    if let Some(why) = setup_refusal(aterm_uds::peer::our_uid(), &places.home, rest) {
        eprintln!("atpkg {VERB}: {why}");
        return ExitCode::FAILURE;
    }
    let mut setup = Setup::default();
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        let parsed = match arg.as_str() {
            "--path" => {
                setup.path = true;
                Ok(())
            }
            "--completions" => {
                setup.completions = true;
                Ok(())
            }
            "--toolset-coming" => {
                setup.toolset_coming = true;
                Ok(())
            }
            "--dry-run" => {
                setup.dry_run = true;
                Ok(())
            }
            "--desktop" => {
                setup.desktop = true;
                Ok(())
            }
            "--man" => take_dir(arg, &mut it, VERB).map(|d| setup.man = Some(d)),
            "--own-man" => {
                setup.own_man = true;
                Ok(())
            }
            "--icons" => take_dir(arg, &mut it, VERB).map(|d| setup.icons = Some(d)),
            "--installed-command" => {
                let path = it
                    .next()
                    .filter(|path| !path.is_empty() && !path.starts_with('-'));
                match path {
                    Some(path) => {
                        setup.installed_command = Some(PathBuf::from(path));
                        Ok(())
                    }
                    None => Err(crate::cli::usage_error(
                        VERB,
                        Some("--installed-command needs an absolute command path"),
                    )),
                }
            }
            "--bin-dir" => take_dir(arg, &mut it, VERB).map(|d| places.bin_dir = d),
            "--man-root" => take_dir(arg, &mut it, VERB).map(|d| places.man_root = d),
            other => Err(crate::cli::usage_error(
                VERB,
                Some(&format!("unknown argument {other:?}")),
            )),
        };
        if let Err(code) = parsed {
            return code;
        }
    }
    if !setup.path
        && !setup.completions
        && setup.man.is_none()
        && !setup.own_man
        && !setup.desktop
        && setup.installed_command.is_none()
    {
        setup.path = true;
        setup.completions = true;
    }
    if setup.path {
        places.zdotdir = crate::zdotdir::of_process(&places.home);
    }
    if setup.desktop && !cfg!(target_os = "linux") {
        println!("atpkg: desktop entry: skipped — an XDG desktop entry is a Linux desktop's");
        setup.desktop = false;
    }
    let Some(exe) = std::env::current_exe()
        .ok()
        .and_then(|p| std::fs::canonicalize(p).ok())
    else {
        eprintln!("atpkg {VERB}: this binary's own path cannot be read");
        return ExitCode::from(1);
    };
    let ok = shell_setup(
        &places,
        &ShellEnv::of_process(),
        &exe,
        &setup,
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    );
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

#[cfg(test)]
#[path = "selfsetup_tests.rs"]
mod tests;
