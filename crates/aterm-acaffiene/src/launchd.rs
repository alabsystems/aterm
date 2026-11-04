// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Launchd job plist ([`crate::paths::Paths::session_plist`], [`EXIT_RESTART`], [`bootout`]).

use std::io;
use std::path::Path;

use crate::paths::Paths;

/// The installed app's label.
pub const INSTALLED_LABEL: &str = "com.aterm.aterm.acaffiene";
/// The prefix of any other build's label.
pub const DEV_LABEL_PREFIX: &str = "com.aterm.aterm.acaffiene.dev";
/// The installed app's executable in `/Applications` ([`installed_executables`] has
/// `~/Applications` too).
pub const INSTALLED_EXECUTABLE: &str = "/Applications/aterm.app/Contents/MacOS/aterm";

/// Where aterm installs itself, as the executable in each, in the order aterm-update's first
/// launch picks: `/Applications` ([`INSTALLED_EXECUTABLE`]) for a person who can write it, else
/// `~/Applications` under `home`.
#[must_use]
pub fn installed_executables(home: &Path) -> [std::path::PathBuf; 2] {
    [
        std::path::PathBuf::from(INSTALLED_EXECUTABLE),
        home.join("Applications")
            .join("aterm.app")
            .join("Contents")
            .join("MacOS")
            .join("aterm"),
    ]
}

/// The [`installed_executables`] under `home` that are there.
#[must_use]
pub fn installed_present(home: &Path) -> Vec<std::path::PathBuf> {
    installed_executables(home)
        .into_iter()
        .filter(|exe| exe.is_file())
        .collect()
}

/// Whether `exe` (canonical) is an installed app's executable ([`installed_executables`]), as
/// spelled or resolved.
#[must_use]
pub fn is_installed(exe: &Path, home: &Path) -> bool {
    installed_executables(home)
        .iter()
        .any(|installed| same_exe(installed, exe))
}

/// Whether `a` names `b`: spelled alike, or `a` resolves to `b`.
pub(crate) fn same_exe(a: &Path, b: &Path) -> bool {
    a == b || std::fs::canonicalize(a).is_ok_and(|a| a == b)
}

/// The installed app whose mug a pass with no window keeps ([`serving_install`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Installed {
    /// Its executable, which has the `acaffiene` verb.
    At(std::path::PathBuf),
    /// None is installed, or each one installed is an aterm with no `acaffiene` verb, whose own
    /// Caffeine, if any, is its own: why.
    Absent(String),
    /// One did not say whether it has the verb (its first exec macOS still assesses, say): why.
    /// The next pass asks again.
    Unanswered(String),
}

/// The first of `present` ([`installed_present`]) that has the `acaffiene` verb: `own`, this
/// process's executable, without asking, and any other as `serves` answers
/// ([`crate::code::serves_acaffiene`]). An installed aterm older than the verb is never started
/// as an aCaffiene job, which its KeepAlive would start again every ten seconds.
#[must_use]
pub fn serving_install(
    present: &[std::path::PathBuf],
    own: Option<&Path>,
    serves: impl Fn(&Path) -> Option<bool>,
) -> Installed {
    if present.is_empty() {
        return Installed::Absent(
            "aterm.app is not installed in /Applications or ~/Applications".to_string(),
        );
    }
    let mut older = Vec::new();
    let mut unanswered = Vec::new();
    for exe in present {
        if own.is_some_and(|own| same_exe(exe, own)) {
            return Installed::At(exe.clone());
        }
        match serves(exe) {
            Some(true) => return Installed::At(exe.clone()),
            Some(false) => older.push(format!(
                "{} has no `acaffiene` verb (an aterm older than aCaffiene)",
                exe.display()
            )),
            None => unanswered.push(format!("{} did not answer `help acaffiene`", exe.display())),
        }
    }
    if unanswered.is_empty() {
        Installed::Absent(older.join("; "))
    } else {
        Installed::Unanswered(
            unanswered
                .into_iter()
                .chain(older)
                .collect::<Vec<_>>()
                .join("; "),
        )
    }
}

/// What the job runs after its program.
pub const SERVE_ARGS: [&str; 2] = ["acaffiene", "serve"];
/// The exit status `serve` ends with when the code it was started from changed (an update).
pub const EXIT_RESTART: u8 = 75;
/// The exit status of a `serve` that could not keep the Mac awake for a reason that can clear
/// up (a full disk, a copy or a save that failed, a lock another process holds a moment):
/// launchd's KeepAlive starts it again ([`crate::trouble`] records why). A refusal no start
/// again cures exits 0 with its trouble recorded, for `aterm doctor` and the window to report.
pub const EXIT_RETRY: u8 = 69;

/// How long a `serve` run as its job waits before it exits [`EXIT_RETRY`]: launchd starts a job
/// that ran this long again at once, so a trouble that lasts costs a start a minute rather than
/// one every ten seconds (launchd's throttle).
pub const RETRY_PAUSE: std::time::Duration = std::time::Duration::from_secs(50);

/// Wait [`RETRY_PAUSE`] when this process runs as a mug's launchd job (`XPC_SERVICE_NAME` names
/// one); a `serve` run by hand, or a test, goes on at once.
pub fn pause_before_retry() {
    if std::env::var("XPC_SERVICE_NAME").is_ok_and(|name| is_label(&name)) {
        std::thread::sleep(RETRY_PAUSE);
    }
}

/// The launchctl binary.
#[cfg(unix)]
const LAUNCHCTL: &str = aterm_uds::os_path!("launchctl");

/// How long one `launchctl` call may take before it is called hung, killed and refused.
#[cfg(unix)]
const LAUNCHCTL_HANG: std::time::Duration = std::time::Duration::from_secs(30);

/// The label of the job for executable `exe` ([`INSTALLED_LABEL`], [`INSTALLED_EXECUTABLE`]).
#[must_use]
pub fn label_for(exe: &Path) -> String {
    if exe == Path::new(INSTALLED_EXECUTABLE) {
        return INSTALLED_LABEL.to_string();
    }
    let hash = aterm_hash::fnv1a_64(exe.as_os_str().as_encoded_bytes());
    format!(
        "{DEV_LABEL_PREFIX}.{:08x}",
        (hash >> 32) as u32 ^ hash as u32
    )
}

/// Whether `text` is a label [`label_for`] makes ([`INSTALLED_LABEL`]).
#[must_use]
pub fn is_label(text: &str) -> bool {
    label_of(text, INSTALLED_LABEL, DEV_LABEL_PREFIX)
}

/// Whether `text` is `installed`, or `dev_prefix`, a dot and eight lowercase hex digits: the
/// grammar of [`label_for`]'s labels, under the mug's name now or before ([`crate::legacy`]).
pub(crate) fn label_of(text: &str, installed: &str, dev_prefix: &str) -> bool {
    text == installed
        || text
            .strip_prefix(dev_prefix)
            .and_then(|rest| rest.strip_prefix('.'))
            .is_some_and(|hash| {
                hash.len() == 8
                    && hash
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
}

/// Render the launchd plist for `label` running `program acaffiene serve` ([`EXIT_RESTART`]).
#[must_use]
pub fn plist_text(label: &str, program: &Path, log: &Path, bundle_id: Option<&str>) -> String {
    let string = |s: &str| format!("<string>{}</string>", aterm_codec::xml::escape(s));
    let mut args = String::new();
    for arg in std::iter::once(program.to_string_lossy().as_ref()).chain(SERVE_ARGS) {
        args.push_str(&format!("\t\t{}\n", string(arg)));
    }
    let associated = bundle_id.map_or_else(String::new, |id| {
        format!(
            "\t<key>AssociatedBundleIdentifiers</key>\n\t<array>\n\t\t{}\n\t</array>\n",
            string(id)
        )
    });
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \t<key>Label</key>\n\
         \t{label}\n\
         \t<key>ProgramArguments</key>\n\
         \t<array>\n\
         {args}\
         \t</array>\n\
         \t<key>RunAtLoad</key>\n\
         \t<true/>\n\
         \t<key>KeepAlive</key>\n\
         \t<dict>\n\
         \t\t<key>SuccessfulExit</key>\n\
         \t\t<false/>\n\
         \t</dict>\n\
         \t<key>ProcessType</key>\n\
         \t<string>Interactive</string>\n\
         \t<key>LimitLoadToSessionType</key>\n\
         \t<string>Aqua</string>\n\
         \t<key>StandardErrorPath</key>\n\
         \t{log}\n\
         {associated}\
         </dict>\n\
         </plist>\n",
        label = string(label),
        log = string(&log.to_string_lossy()),
    )
}

/// The bundle identifier of the app `program` is the executable of.
#[cfg(unix)]
fn bundle_id_of(program: &Path) -> Option<String> {
    crate::code::own_bundle(program).and_then(|b| crate::code::bundle_identifier(&b))
}

fn is_regular_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_file())
}

fn validate_label(label: &str) -> io::Result<()> {
    if crate::code::plain_name(label) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{label:?} is not a plain launchd label"),
        ))
    }
}

fn check_existing_tree_from(base: &Path, leaf: &Path) -> io::Result<bool> {
    let Ok(rel) = leaf.strip_prefix(base) else {
        return crate::paths::existing_real_dir(leaf);
    };
    if !crate::paths::existing_real_dir(base)? {
        return Ok(false);
    }
    let mut cur = base.to_path_buf();
    for part in rel.components() {
        cur.push(part);
        if !crate::paths::existing_real_dir(&cur)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn check_existing_launch_agents_dir(paths: &Paths) -> io::Result<bool> {
    check_existing_tree_from(&paths.home, &paths.launch_agents())
}

#[cfg(unix)]
fn ensure_tree_from(base: &Path, leaf: &Path) -> io::Result<()> {
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
    if check_existing_tree_from(base, leaf)? {
        return Ok(());
    }
    std::fs::create_dir_all(leaf)?;
    let opened = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(leaf)
        .map_err(|err| match err.raw_os_error() {
            Some(code) if code == libc::ELOOP || code == libc::ENOTDIR => io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is not a directory", leaf.display()),
            ),
            _ => err,
        })?;
    let meta = opened.metadata()?;
    if !meta.is_dir() || meta.uid() != aterm_uds::peer::our_uid() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is not this user's directory", leaf.display()),
        ));
    }
    if !check_existing_tree_from(base, leaf)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a directory", leaf.display()),
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn ensure_launch_agents_dir(paths: &Paths) -> io::Result<()> {
    ensure_tree_from(&paths.home, &paths.launch_agents())
}

#[cfg(unix)]
fn ensure_logs_dir(paths: &Paths) -> io::Result<()> {
    ensure_tree_from(&paths.home, &paths.logs_dir)
}

/// Whether Enable at Startup is on for `label` ([`startup`]).
#[must_use]
pub fn startup_enabled(paths: &Paths, label: &str) -> bool {
    crate::code::plain_name(label)
        && matches!(check_existing_launch_agents_dir(paths), Ok(true))
        && is_regular_file(&paths.startup_plist(label))
}

/// What Enable at Startup does at the next login.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Startup {
    /// No login plist: no login starts the mug.
    Off,
    /// The login plist is there, and every login starts the mug.
    On,
    /// The login plist is there, but aterm's switch under Background App Activity is off: no
    /// login starts the mug.
    Blocked,
    /// The login plist is there, but launchd has its label disabled (`launchctl disable`): no
    /// login loads it.
    Disabled,
}

/// The doors to what decides, besides the login plist, whether a login loads it.
#[derive(Clone, Copy)]
pub struct StartupDoors {
    /// Whether Login Items' switch keeps the plist from loading (macOS: `SMAppService`).
    pub blocks: fn(&Path) -> bool,
    /// Whether launchd has the label disabled ([`print_disabled_lists`]).
    pub disabled: fn(&str) -> bool,
    /// Lift launchd's disable of the label (`launchctl enable`).
    pub enable: fn(&str) -> io::Result<()>,
}

impl Startup {
    /// The word `status` and `startup` answer after `startup=`.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::On => "on",
            Self::Blocked => "blocked",
            Self::Disabled => "disabled",
        }
    }
}

/// Startup state ([`Startup::Off`], [`Startup::Disabled`], [`Startup::Blocked`], [`Startup::On`]).
#[must_use]
pub fn startup(paths: &Paths, label: &str, doors: &StartupDoors) -> Startup {
    if !crate::code::plain_name(label)
        || !matches!(check_existing_launch_agents_dir(paths), Ok(true))
    {
        return Startup::Off;
    }
    let plist = paths.startup_plist(label);
    if !is_regular_file(&plist) {
        Startup::Off
    } else if (doors.disabled)(label) {
        Startup::Disabled
    } else if (doors.blocks)(&plist) {
        Startup::Blocked
    } else {
        Startup::On
    }
}

/// Toggle Enable at Startup for `label` and `program` ([`prune_orphans`]).
/// # Errors
/// `label` is not a plain name, `paths`'s `LaunchAgents` or `logs_dir` root is not this user's
/// plain directory, the plist cannot be written or removed, or `program` is not a UTF-8 path.
#[cfg(unix)]
pub fn set_startup(paths: &Paths, label: &str, program: &Path, on: bool) -> io::Result<()> {
    validate_label(label)?;
    let plist = paths.startup_plist(label);
    if !on {
        let agents_exist = check_existing_launch_agents_dir(paths)?;
        let removed = if agents_exist {
            match std::fs::remove_file(&plist) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            }
        } else {
            Ok(())
        };
        prune_orphans(paths);
        return removed;
    }
    utf8(program)?;
    ensure_logs_dir(paths)?;
    ensure_launch_agents_dir(paths)?;
    let text = plist_text(
        label,
        program,
        &paths.log_file(),
        bundle_id_of(program).as_deref(),
    );
    aterm_uds::atomic_file::write(&plist, text.as_bytes(), 0o644)
}

fn check_existing_session_dir(dir: &Path) -> io::Result<bool> {
    if let Some(parent) = dir.parent()
        && !parent.as_os_str().is_empty()
        && !crate::paths::existing_real_dir(parent)?
    {
        return Ok(false);
    }
    crate::paths::existing_real_dir(dir)
}

/// The person's own Enable at Startup for `label`: [`set_startup`], and a record of an off
/// ([`Paths::startup_off_file`]) that no launch overrides ([`startup_turned_off`]); an on
/// removes it.
/// # Errors
/// As [`set_startup`], or the record cannot be written or removed.
pub fn choose_startup(paths: &Paths, label: &str, program: &Path, on: bool) -> io::Result<()> {
    validate_label(label)?;
    let record = paths.startup_off_file(label);
    if on {
        let session_exists = match record.parent() {
            Some(dir) => check_existing_session_dir(dir)?,
            None => true,
        };
        set_startup(paths, label, program, true)?;
        if !session_exists {
            return Ok(());
        }
        return match std::fs::remove_file(&record) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    const RECORD_TEXT: &[u8] = b"Enable at Startup turned off by the person\n";
    #[cfg(unix)]
    {
        if let Some(dir) = record.parent() {
            ensure_session_dir(dir)?;
        }
        set_startup(paths, label, program, false)?;
        aterm_uds::atomic_file::write(&record, RECORD_TEXT, 0o600)
    }
    #[cfg(not(unix))]
    {
        if let Some(dir) = record.parent()
            && !dir.as_os_str().is_empty()
        {
            crate::paths::ensure_real_dir(dir)?;
        }
        set_startup(paths, label, program, false)?;
        std::fs::write(&record, RECORD_TEXT)
    }
}

/// Whether the person turned Enable at Startup off for `label` ([`choose_startup`]).
#[must_use]
pub fn startup_turned_off(paths: &Paths, label: &str) -> bool {
    let record = paths.startup_off_file(label);
    crate::code::plain_name(label)
        && crate::paths::parent_is_real_dir(&paths.session_dir())
        && crate::paths::parent_is_real_dir(&record)
        && is_regular_file(&record)
}

/// Enable at Startup.
/// # Errors
/// Always: launchd is macOS.
#[cfg(not(unix))]
pub fn set_startup(paths: &Paths, label: &str, program: &Path, on: bool) -> io::Result<()> {
    let _ = (paths, label, program, on);
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "launchd is macOS only",
    ))
}

/// `Ok` when `path` is UTF-8 (what a plist can name).
#[cfg(unix)]
fn utf8(path: &Path) -> io::Result<()> {
    if path.to_str().is_some() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a UTF-8 path", path.display()),
        ))
    }
}

/// Orphaned login plists in `~/Library/LaunchAgents` whose executable is gone ([`is_label`]),
/// of those this Mac may judge ([`judged`], with [`Paths::launch_agents_local`]). One it may not
/// judge whose executable is gone here stays, and is only unloaded here, while it has no process
/// ([`unload_stranded`]).
#[must_use]
pub fn orphans(paths: &Paths) -> Vec<(String, std::path::PathBuf)> {
    let local = paths.launch_agents_local();
    login_jobs(paths, is_label, &SERVE_ARGS, |program| {
        judged(program, &paths.home, local) && program_gone(program)
    })
}

/// A login job this Mac may not judge ([`judged`]), which stays for another Mac that shares the
/// home ([`unload_stranded`]).
#[cfg(unix)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Unjudged {
    /// Its label.
    pub(crate) label: String,
    /// Its plist in `~/Library/LaunchAgents`.
    pub(crate) plist: std::path::PathBuf,
    /// The program its ProgramArguments run.
    pub(crate) program: std::path::PathBuf,
    /// What the unload did with it where that program is gone here ([`program_gone`]); `None`
    /// where it is not, and nothing was asked of launchd.
    pub(crate) unload: Option<Unload>,
}

/// Unload with `unload` ([`unload_idle`] live, which boots a job out of this Mac's own domain
/// only while it has no process) each login job `is_label` and `serve_args` name
/// ([`login_job_programs`]) that this Mac may not judge ([`judged`], with
/// [`Paths::launch_agents_local`]) and whose program is gone here ([`program_gone`]). Its plist
/// stays, as another Mac that shares the home may run that program from there, and asking
/// whether it is gone runs no program. This Mac cannot start it. Where its program was already
/// gone when launchd here loaded it at login, its start failed, and macOS 27.0's launchd then
/// keeps it loaded with no process, waiting for the program to appear rather than retrying
/// (measured 2026-11-03). The bootout of such a job leaves this Mac's launchd with no job it
/// cannot run, whatever its policy for a failed start, for this login alone; the next login loads
/// it again, and nothing another Mac reads changes. Where its start succeeded and its program
/// went later, the job may still have a process here (an old mug that re-execs into its own copy
/// of aterm, which keeps launchd's pid, before the app is removed, say): such a job is left
/// loaded while that process runs, as a bootout would end it ([`Unload::Running`]). Every login
/// job this Mac may not judge, sorted, each with what `unload` said it did where its program is
/// gone here.
#[cfg(unix)]
pub(crate) fn unload_stranded(
    paths: &Paths,
    is_label: fn(&str) -> bool,
    serve_args: &[&str],
    unload: impl Fn(&str) -> Unload,
) -> Vec<Unjudged> {
    let local = paths.launch_agents_local();
    login_job_programs(paths, is_label, serve_args)
        .into_iter()
        .filter(|(_, _, program)| !judged(program, &paths.home, local))
        .map(|(label, plist, program)| {
            let unload = program_gone(&program).then(|| unload(&label));
            Unjudged {
                label,
                plist,
                program,
                unload,
            }
        })
        .collect()
}

/// Whether this Mac may judge a login job by its `program`: always where the plists are on a
/// volume local to it (`local`), and on a network home, which another Mac may read at the same
/// time, only a program inside `home`. One outside it, in this Mac's `/Applications` say, may be
/// another Mac's, there, whose Enable at Startup a removal here would turn off.
pub(crate) fn judged(program: &Path, home: &Path, local: bool) -> bool {
    local || program.starts_with(home)
}

/// Whether `path` is on a volume local to this Mac (`statfs`'s `MNT_LOCAL`): `false` where it
/// cannot be read.
#[cfg(target_os = "macos")]
pub(crate) fn on_local_volume(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    const MNT_LOCAL: u32 = 0x0000_1000;
    let Ok(name) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `statfs` is integers and arrays: all-zero is a value.
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: a NUL-terminated path the call reads and does not retain, and a writable `statfs`.
    if unsafe { libc::statfs(name.as_ptr(), &raw mut st) } != 0 {
        return false;
    }
    st.f_flags & MNT_LOCAL != 0
}

/// Off macOS no home is shared with a Mac.
#[cfg(not(target_os = "macos"))]
pub(crate) fn on_local_volume(_path: &Path) -> bool {
    true
}

/// The [`login_job_programs`] with `pick` true of their program: `(label, plist)`, sorted.
pub(crate) fn login_jobs(
    paths: &Paths,
    is_label: fn(&str) -> bool,
    serve_args: &[&str],
    pick: impl Fn(&Path) -> bool,
) -> Vec<(String, std::path::PathBuf)> {
    login_job_programs(paths, is_label, serve_args)
        .into_iter()
        .filter(|(_, _, program)| pick(program))
        .map(|(label, plist, _)| (label, plist))
        .collect()
}

/// The login plists in `~/Library/LaunchAgents` named for a label `is_label` takes, each a
/// regular file whose ProgramArguments are a program and `serve_args`: `(label, plist,
/// program)`, sorted.
pub(crate) fn login_job_programs(
    paths: &Paths,
    is_label: fn(&str) -> bool,
    serve_args: &[&str],
) -> Vec<(String, std::path::PathBuf, std::path::PathBuf)> {
    if !matches!(check_existing_launch_agents_dir(paths), Ok(true)) {
        return Vec::new();
    }
    let mut found = Vec::new();
    for entry in std::fs::read_dir(paths.launch_agents())
        .into_iter()
        .flatten()
    {
        let Ok(entry) = entry else { continue };
        let plist = entry.path();
        let Some(label) = plist
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".plist"))
            .filter(|label| is_label(label))
            .map(str::to_string)
        else {
            continue;
        };
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        let Some(args) = crate::code::read_plist(&plist)
            .and_then(|text| crate::code::plist_strings(&text, "ProgramArguments"))
        else {
            continue;
        };
        let [program, rest @ ..] = args.as_slice() else {
            continue;
        };
        if rest == serve_args {
            found.push((label, plist, std::path::PathBuf::from(program)));
        }
    }
    found.sort();
    found
}

/// Whether a job's `program` is gone: an absolute path, off `/Volumes` (a volume that is not
/// mounted now may come back), with nothing there.
pub(crate) fn program_gone(program: &Path) -> bool {
    program.is_absolute()
        && !program.starts_with("/Volumes")
        && std::fs::symlink_metadata(program).is_err_and(|e| e.kind() == io::ErrorKind::NotFound)
}

/// Remove the temporaries that no write will rename beside the job plists, under this name and
/// the old one ([`crate::legacy`]): in the session plists' folders and in
/// `~/Library/LaunchAgents`, each beside a plist named for a label [`is_label`] or
/// [`crate::legacy::is_label`] takes ([`aterm_uds::atomic_file::temp_target`]), with
/// [`aterm_uds::atomic_file::sweep_temps_beside`] past
/// [`aterm_uds::atomic_file::ABANDONED_AFTER`]. 0.145.0's window autostart, cut short by a
/// refused command line, left one beside a session plist. Each plist that has one, with how many
/// went, or its sweep's first error once the rest were tried, after which some may have gone;
/// a folder that cannot be listed, with why, and none of its temporaries go.
#[cfg(unix)]
#[must_use]
pub fn sweep_plist_temps(paths: &Paths) -> Vec<(std::path::PathBuf, io::Result<usize>)> {
    let session = paths.session_dir();
    let old = crate::legacy::session_dir(paths);
    let agents = paths.launch_agents();
    let mut swept = Vec::new();
    for (dir, exists) in [
        (session.clone(), check_existing_session_dir(&session)),
        (old.clone(), check_existing_session_dir(&old)),
        (agents, check_existing_launch_agents_dir(paths)),
    ] {
        let listed = exists.and_then(|present| {
            if present {
                plists_with_temps(&dir)
            } else {
                Ok(Vec::new())
            }
        });
        match listed {
            Ok(plists) => swept.extend(plists.into_iter().map(|plist| {
                let after = aterm_uds::atomic_file::ABANDONED_AFTER;
                let gone = aterm_uds::atomic_file::sweep_temps_beside(&plist, after);
                (plist, gone)
            })),
            Err(e) => swept.push((dir, Err(e))),
        }
    }
    swept
}

/// The job plists in `dir` with a temporary beside them ([`sweep_plist_temps`]), sorted; none
/// for a missing `dir`.
#[cfg(unix)]
fn plists_with_temps(dir: &Path) -> io::Result<Vec<std::path::PathBuf>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut plists = std::collections::BTreeSet::new();
    for entry in entries {
        let name = entry?.file_name();
        let Some(plist) = aterm_uds::atomic_file::temp_target(&name) else {
            continue;
        };
        let ours = plist
            .strip_suffix(".plist")
            .is_some_and(|label| is_label(label) || crate::legacy::is_label(label));
        if ours {
            plists.insert(dir.join(plist));
        }
    }
    Ok(plists.into_iter().collect())
}

/// Remove every [`orphans`] plist, and unload each job kept on a network home whose program is
/// gone here and that has no process ([`unload_stranded`]).
#[cfg(unix)]
pub fn prune_orphans(paths: &Paths) {
    prune_orphans_with(paths, unload_idle);
}

/// [`prune_orphans`], unloading each job with `unload`.
#[cfg(unix)]
fn prune_orphans_with(paths: &Paths, unload: impl Fn(&str) -> Unload) {
    let _ = retire_with(orphans(paths), &unload);
    let _ = unload_stranded(paths, is_label, &SERVE_ARGS, unload);
}

/// Remove each job's plist, then unload its label with `unload` whether or not the plist went
/// ([`unload_idle`] live, which unloads a job launchd has loaded but not running, so it is not
/// started again this login). What could not be removed, one line each.
#[cfg(unix)]
pub(crate) fn retire_with(
    jobs: Vec<(String, std::path::PathBuf)>,
    unload: impl Fn(&str) -> Unload,
) -> Vec<String> {
    let mut failed = Vec::new();
    for (label, plist) in jobs {
        if let Err(e) = std::fs::remove_file(&plist)
            && e.kind() != io::ErrorKind::NotFound
        {
            failed.push(format!("{}: {e}", plist.display()));
        }
        let _ = unload(&label);
    }
    failed
}

/// What [`unload_idle`] did with a job in this Mac's launchd.
#[cfg(unix)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Unload {
    /// launchd had it loaded with no process, and booted it out for this login.
    Unloaded,
    /// launchd has no such job loaded: it had none, or another pass booted it out first.
    NotLoaded,
    /// It had a process, this pid, so it stays loaded: a bootout would end that process.
    Running(u32),
    /// `launchctl` could not tell its state, or refused the bootout: why.
    Failed(String),
}

/// Boot out the job `label` when launchd has it loaded but not running, and say what was done
/// ([`unload_idle_with`]).
#[cfg(unix)]
pub(crate) fn unload_idle(label: &str) -> Unload {
    unload_idle_with(|| job_state(label), || bootout(label))
}

/// [`unload_idle`] for a job whose state in launchd `state` reads, booting it out with `bootout`
/// only when it is loaded with no process. A refused bootout is judged by the state read again
/// after it: another pass that unloads the same job (a window's tidy, the old `caffeine serve`
/// or the converge pass's retire, none of which takes a lock) may have booted it out between this
/// one's read and its bootout, and `launchctl` refuses the bootout of a job it no longer has. A job gone by then is
/// [`Unload::NotLoaded`]; one still loaded, or whose state cannot be read again, is
/// [`Unload::Failed`] with the bootout's refusal.
#[cfg(unix)]
fn unload_idle_with(
    mut state: impl FnMut() -> io::Result<JobState>,
    bootout: impl FnOnce() -> io::Result<()>,
) -> Unload {
    match state() {
        Ok(JobState::Loaded { pid: None }) => match bootout() {
            Ok(()) => Unload::Unloaded,
            Err(e) => match state() {
                Ok(JobState::Absent) => Unload::NotLoaded,
                Ok(JobState::Loaded { .. }) | Err(_) => Unload::Failed(e.to_string()),
            },
        },
        Ok(JobState::Loaded { pid: Some(pid) }) => Unload::Running(pid),
        Ok(JobState::Absent) => Unload::NotLoaded,
        Err(e) => Unload::Failed(e.to_string()),
    }
}

/// Orphaned login plists.
#[cfg(not(unix))]
pub fn prune_orphans(_paths: &Paths) {}

/// The user's GUI domain target for `uid`, `gui/<uid>`.
#[must_use]
pub fn gui_domain(uid: u32) -> String {
    format!("gui/{uid}")
}

/// `launchctl print <domain>/<label>`.
#[must_use]
pub fn print_argv(domain: &str, label: &str) -> Vec<String> {
    vec!["print".to_string(), format!("{domain}/{label}")]
}

/// `launchctl bootstrap <domain> <plist>`.
#[must_use]
pub fn bootstrap_argv(domain: &str, plist: &Path) -> Vec<String> {
    vec![
        "bootstrap".to_string(),
        domain.to_string(),
        plist.to_string_lossy().into_owned(),
    ]
}

/// `launchctl kickstart <domain>/<label>`.
#[must_use]
pub fn kickstart_argv(domain: &str, label: &str) -> Vec<String> {
    vec!["kickstart".to_string(), format!("{domain}/{label}")]
}

/// `launchctl bootout <domain>/<label>`.
#[must_use]
pub fn bootout_argv(domain: &str, label: &str) -> Vec<String> {
    vec!["bootout".to_string(), format!("{domain}/{label}")]
}

/// `launchctl enable <domain>/<label>`.
#[must_use]
pub fn enable_argv(domain: &str, label: &str) -> Vec<String> {
    vec!["enable".to_string(), format!("{domain}/{label}")]
}

/// `launchctl print-disabled <domain>`.
#[must_use]
pub fn print_disabled_argv(domain: &str) -> Vec<String> {
    vec!["print-disabled".to_string(), domain.to_string()]
}

/// Whether `print-disabled`'s `text` lists `label` disabled.
#[must_use]
pub fn print_disabled_lists(text: &str, label: &str) -> bool {
    text.lines().any(|line| {
        line.trim()
            .strip_prefix('"')
            .and_then(|rest| rest.strip_prefix(label))
            .and_then(|rest| rest.strip_prefix('"'))
            .and_then(|rest| rest.trim().strip_prefix("=>"))
            .is_some_and(|word| matches!(word.trim(), "disabled" | "true"))
    })
}

/// What launchd says of a job.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobState {
    /// launchd knows no such job in the GUI domain.
    Absent,
    /// It is loaded; `pid` when it is running now.
    Loaded {
        /// The job's process, when one runs.
        pid: Option<u32>,
    },
}

/// The running pid `launchctl print`'s text names.
#[must_use]
pub fn print_pid(text: &str) -> Option<u32> {
    text.lines().find_map(|line| {
        let top = line.strip_prefix('\t')?;
        let s = top.strip_prefix("pid = ")?.trim_end();
        (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            .then(|| s.parse().ok())
            .flatten()
            .filter(|pid| *pid > 0)
    })
}

/// How [`ensure_running`] got the mug running.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Started {
    /// The job already ran.
    AlreadyRunning {
        /// Its process.
        pid: u32,
    },
    /// The job was loaded but not running.
    Kicked,
    /// The job was not loaded.
    Bootstrapped,
}

/// Run one `launchctl` command with `/dev/null` stdin under [`LAUNCHCTL_HANG`].
#[cfg(unix)]
fn launchctl(argv: &[String]) -> io::Result<std::process::Output> {
    let mut cmd = aterm_uds::spawnfd::command(LAUNCHCTL);
    cmd.args(argv);
    aterm_uds::deadline::output_within(&mut cmd, LAUNCHCTL_HANG)
        .map_err(|e| io::Error::other(format!("launchctl {}: {e}", argv.join(" "))))
}

/// One `launchctl` call that must succeed: a refusal is an error carrying what it said.
#[cfg(unix)]
fn launchctl_ok(argv: &[String]) -> io::Result<()> {
    let out = launchctl(argv)?;
    if out.status.success() {
        return Ok(());
    }
    let said = [&out.stderr, &out.stdout]
        .iter()
        .map(|b| String::from_utf8_lossy(b).trim().to_string())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("; ");
    Err(io::Error::other(format!(
        "launchctl {} ({}): {said}",
        argv.join(" "),
        out.status
    )))
}

/// This user's GUI domain.
#[cfg(unix)]
fn own_domain() -> String {
    gui_domain(aterm_uds::peer::our_uid())
}

/// Read the job `label`'s state with `launchctl print` (read-only).
/// # Errors
/// `launchctl` cannot be run, or hangs.
#[cfg(unix)]
pub fn job_state(label: &str) -> io::Result<JobState> {
    let out = launchctl(&print_argv(&own_domain(), label))?;
    if !out.status.success() {
        return Ok(JobState::Absent);
    }
    Ok(JobState::Loaded {
        pid: print_pid(&String::from_utf8_lossy(&out.stdout)),
    })
}

/// Load (and, with `RunAtLoad`, start) the job `plist` defines in this user's GUI domain.
/// # Errors
/// `launchctl` cannot be run, or refuses.
#[cfg(unix)]
pub fn bootstrap(plist: &Path) -> io::Result<()> {
    launchctl_ok(&bootstrap_argv(&own_domain(), plist))
}

/// Start the loaded job `label` (a running one is left as it is).
/// # Errors
/// `launchctl` cannot be run, or refuses.
#[cfg(unix)]
pub fn kickstart(label: &str) -> io::Result<()> {
    launchctl_ok(&kickstart_argv(&own_domain(), label))
}

/// Lift launchd's disable of `label` in this user's GUI domain.
/// # Errors
/// `launchctl` cannot be run, or refuses.
#[cfg(unix)]
pub fn enable(label: &str) -> io::Result<()> {
    launchctl_ok(&enable_argv(&own_domain(), label))
}

/// End the job `label` and unload it for this login (launchd's SIGTERM; no restart).
/// # Errors
/// `launchctl` cannot be run, or refuses (a label it does not know).
#[cfg(unix)]
pub fn bootout(label: &str) -> io::Result<()> {
    launchctl_ok(&bootout_argv(&own_domain(), label))
}

/// Ensure `label` is running `program`'s mug via kickstart or bootstrap ([`Paths::session_plist`]).
/// # Errors
/// What failed, as one line: `launchctl`, or a plist that cannot be written.
#[cfg(unix)]
pub fn ensure_running(paths: &Paths, label: &str, program: &Path) -> Result<Started, String> {
    match job_state(label).map_err(|e| e.to_string())? {
        JobState::Loaded { pid: Some(pid) } => return Ok(Started::AlreadyRunning { pid }),
        JobState::Loaded { pid: None } => {
            return kickstart(label)
                .map(|()| Started::Kicked)
                .map_err(|e| e.to_string());
        }
        JobState::Absent => {}
    }
    ensure_logs_dir(paths).map_err(|e| format!("{}: {e}", paths.logs_dir.display()))?;
    let plist = if startup_enabled(paths, label) {
        paths.startup_plist(label)
    } else {
        let plist = paths.session_plist(label);
        write_session_plist(
            paths,
            &plist,
            label,
            program,
            bundle_id_of(program).as_deref(),
        )
        .map_err(|e| format!("{}: {e}", plist.display()))?;
        plist
    };
    match bootstrap(&plist) {
        Ok(()) => Ok(Started::Bootstrapped),
        Err(refused) => match job_state(label) {
            Ok(JobState::Loaded { pid: Some(pid) }) => Ok(Started::AlreadyRunning { pid }),
            Ok(JobState::Loaded { pid: None }) => kickstart(label)
                .map(|()| Started::Kicked)
                .map_err(|e| e.to_string()),
            Ok(JobState::Absent) | Err(_) if disabled(label) => Err(format!(
                "launchd refused to load {}: it has {label} disabled for this user, past \
                 logout and restart (`launchctl enable {}/{label}` lifts it)",
                plist.display(),
                own_domain()
            )),
            Ok(JobState::Absent) | Err(_) => Err(refused.to_string()),
        },
    }
}

/// Whether launchd has `label` disabled in this user's GUI domain ([`print_disabled_lists`]).
#[cfg(unix)]
pub fn disabled(label: &str) -> bool {
    launchctl(&print_disabled_argv(&own_domain())).is_ok_and(|out| {
        out.status.success() && print_disabled_lists(&String::from_utf8_lossy(&out.stdout), label)
    })
}

#[cfg(unix)]
fn ensure_session_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::{
        DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _,
    };
    if let Some(parent) = dir.parent()
        && !parent.as_os_str().is_empty()
    {
        let _ = crate::paths::existing_real_dir(parent)?;
    }
    let _ = crate::paths::existing_real_dir(dir)?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let directory = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_DIRECTORY)
        .open(dir)?;
    let meta = directory.metadata()?;
    if !meta.is_dir() || meta.uid() != aterm_uds::peer::our_uid() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is not this user's directory", dir.display()),
        ));
    }
    directory.set_permissions(std::fs::Permissions::from_mode(0o700))
}

/// Write the session plist at `plist` (`0644` in a `0700` directory).
#[cfg(unix)]
fn write_session_plist(
    paths: &Paths,
    plist: &Path,
    label: &str,
    program: &Path,
    bundle_id: Option<&str>,
) -> io::Result<()> {
    validate_label(label)?;
    utf8(program)?;
    if let Some(dir) = plist.parent() {
        ensure_session_dir(dir)?;
    }
    let text = plist_text(label, program, &paths.log_file(), bundle_id);
    aterm_uds::atomic_file::write(plist, text.as_bytes(), 0o644)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_installed_app_and_dev_builds_never_share_a_label() {
        assert_eq!(label_for(Path::new(INSTALLED_EXECUTABLE)), INSTALLED_LABEL);
        let dev = label_for(Path::new("/Users//x/aterm/target/debug/aterm"));
        let short = dev
            .strip_prefix(&format!("{DEV_LABEL_PREFIX}."))
            .expect("the dev prefix");
        assert_eq!(short.len(), 8);
        assert!(
            short
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        );
        assert_eq!(
            dev,
            label_for(Path::new("/Users//x/aterm/target/debug/aterm"))
        );
        assert_ne!(
            dev,
            label_for(Path::new("/Users//x/aterm2/target/debug/aterm"))
        );
        for other in [
            "/Users//x/Applications/aterm.app/Contents/MacOS/aterm",
            "/Applications/aterm (dev).app/Contents/MacOS/aterm",
        ] {
            assert_ne!(label_for(Path::new(other)), INSTALLED_LABEL, "{other}");
        }
    }

    /// aterm installs itself in `/Applications`, or in `~/Applications` for a person who cannot
    /// write it (aterm-update's first launch): either is the installed app, and a dev build or
    /// another bundle is not.
    #[test]
    fn an_install_in_either_applications_folder_is_installed() {
        let home = Path::new("/Users//x");
        let [system, own] = installed_executables(home);
        assert_eq!(system, Path::new(INSTALLED_EXECUTABLE));
        assert_eq!(
            own,
            Path::new("/Users//x/Applications/aterm.app/Contents/MacOS/aterm")
        );
        assert!(is_installed(&system, home));
        assert!(is_installed(&own, home), "a non-admin's install");
        for not in [
            "/Users//x/aterm/target/debug/aterm",
            "/Applications/aterm (dev).app/Contents/MacOS/aterm",
            "/Users//y/Applications/aterm.app/Contents/MacOS/aterm",
        ] {
            assert!(!is_installed(Path::new(not), home), "{not}");
        }
    }

    /// The pass keeps the first installed app that has the verb: its own executable without
    /// asking; an older aterm is never started as the job; one that did not answer is asked
    /// again by the next pass.
    #[test]
    fn the_pass_keeps_an_installed_app_that_serves_acaffiene() {
        let system = std::path::PathBuf::from(INSTALLED_EXECUTABLE);
        let own = std::path::PathBuf::from("/Users//x/Applications/aterm.app/Contents/MacOS/aterm");
        assert!(matches!(
            serving_install(&[], None, |_| panic!("nothing to ask")),
            Installed::Absent(_)
        ));
        assert_eq!(
            serving_install(std::slice::from_ref(&own), Some(&own), |_| panic!(
                "its own verb"
            )),
            Installed::At(own.clone())
        );
        assert_eq!(
            serving_install(&[system.clone(), own.clone()], None, |exe| Some(exe == own)),
            Installed::At(own.clone()),
            "0.144 in /Applications, a newer aterm in ~/Applications"
        );
        let older = serving_install(std::slice::from_ref(&system), None, |_| Some(false));
        assert!(
            matches!(&older, Installed::Absent(why) if why.contains("no `acaffiene` verb")),
            "{older:?}"
        );
        assert!(matches!(
            serving_install(&[system.clone(), own], None, |exe| (exe != system)
                .then_some(false)),
            Installed::Unanswered(_)
        ));
        assert_eq!(
            serving_install(std::slice::from_ref(&system), None, |_| Some(true)),
            Installed::At(system)
        );
    }

    #[test]
    fn only_a_label_this_crate_makes_is_one() {
        assert!(is_label(INSTALLED_LABEL));
        assert!(is_label(&label_for(Path::new("/w/target/debug/aterm"))));
        assert!(is_label("com.aterm.aterm.acaffiene.dev.0123abcd"));
        for bad in [
            "",
            "com.aterm.aterm",
            "com.aterm.aterm.acaffiene.dev",
            "com.aterm.aterm.acaffiene.dev.",
            "com.aterm.aterm.acaffiene.dev.0123ABCD",
            "com.aterm.aterm.acaffiene.dev.0123abc",
            "com.aterm.aterm.acaffiene.dev.0123abcde",
            "com.aterm.aterm.acaffiene.dev.0123abcg",
            "com.aterm.aterm.acaffiene.x",
            "com.aterm.aterm.acaffiene\n",
        ] {
            assert!(!is_label(bad), "{bad:?}");
        }
    }

    #[test]
    #[cfg(unix)]
    fn an_orphan_is_a_mugs_login_plist_whose_program_is_gone() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let paths = Paths {
            state_dir: root.path().join("state"),
            home: root.path().join("home"),
            logs_dir: root.path().join("logs"),
            local_volume: |_| true,
        };
        assert_eq!(orphans(&paths), [], "no LaunchAgents at all");
        let present = root.path().join("aterm");
        std::fs::write(&present, "exe").expect("exe");
        let gone = root.path().join("removed-worktree").join("aterm");
        let agents = paths.launch_agents();
        std::fs::create_dir_all(&agents).expect("agents");
        let plant = |label: &str, program: &Path| {
            let text = plist_text(label, program, &paths.log_file(), None);
            std::fs::write(agents.join(format!("{label}.plist")), text).expect("plist");
        };
        let orphan = "com.aterm.aterm.acaffiene.dev.0000000a";
        plant(orphan, &gone);
        plant(INSTALLED_LABEL, &gone);
        plant("com.aterm.aterm.acaffiene.dev.0000000b", &present);
        plant(
            "com.aterm.aterm.acaffiene.dev.0000000c",
            Path::new("/Volumes/Ejected/aterm.app/Contents/MacOS/aterm"),
        );
        plant("com.example.other", &gone);
        let uncertain = present.join("aterm");
        assert!(
            std::fs::symlink_metadata(&uncertain)
                .is_err_and(|e| e.kind() != io::ErrorKind::NotFound)
        );
        plant("com.aterm.aterm.acaffiene.dev.0000000e", &uncertain);
        std::fs::write(
            agents.join("com.aterm.aterm.acaffiene.dev.0000000d.plist"),
            plist_text("x", &gone, &paths.log_file(), None)
                .replace("<string>serve</string>", "<string>status</string>"),
        )
        .expect("plist");
        assert_eq!(
            orphans(&paths),
            [
                (
                    INSTALLED_LABEL.to_string(),
                    paths.startup_plist(INSTALLED_LABEL)
                ),
                (orphan.to_string(), paths.startup_plist(orphan)),
            ]
        );
        assert!(
            on_local_volume(&agents),
            "a temporary folder is on this Mac"
        );
        #[cfg(target_os = "macos")]
        assert!(
            !on_local_volume(&agents.join("missing")),
            "a path statfs cannot read is not judged local"
        );

        assert_eq!(
            unload_stranded(&paths, is_label, &SERVE_ARGS, |label| {
                panic!(
                    "{label} is this Mac's to judge, and so its orphan to retire, never stranded"
                )
            }),
            [],
            "a home on this Mac leaves no job unjudged"
        );

        let in_home = "com.aterm.aterm.acaffiene.dev.0000000f";
        plant(in_home, &paths.home.join("worktree/aterm"));
        let network = Paths {
            local_volume: |_| false,
            ..paths.clone()
        };
        assert_eq!(
            orphans(&network),
            [(in_home.to_string(), paths.startup_plist(in_home))],
            "on a network home only a program inside it is judged gone: one outside may be \
             another Mac's"
        );

        let unloaded = std::cell::RefCell::new(Vec::new());
        prune_orphans_with(&network, |label| {
            unloaded.borrow_mut().push(label.to_string());
            Unload::Unloaded
        });
        assert!(!paths.startup_plist(in_home).exists(), "the orphan goes");
        assert!(
            paths.startup_plist(INSTALLED_LABEL).is_file() && paths.startup_plist(orphan).is_file(),
            "a job whose program outside the home is gone here stays for another Mac"
        );
        assert_eq!(
            unloaded.into_inner(),
            [in_home, INSTALLED_LABEL, orphan],
            "the orphan is unloaded, and so is each job kept whose program is gone here, which \
             this Mac cannot run; none whose program is here, on a volume or unreadable"
        );

        let kept = |label: &str, program: &Path, unload: Option<Unload>| Unjudged {
            label: label.to_string(),
            plist: paths.startup_plist(label),
            program: program.to_path_buf(),
            unload,
        };
        assert_eq!(
            unload_stranded(&network, is_label, &SERVE_ARGS, |label| {
                if label == INSTALLED_LABEL {
                    Unload::Running(4242)
                } else {
                    Unload::Unloaded
                }
            }),
            [
                kept(INSTALLED_LABEL, &gone, Some(Unload::Running(4242))),
                kept(orphan, &gone, Some(Unload::Unloaded)),
                kept("com.aterm.aterm.acaffiene.dev.0000000b", &present, None),
                kept(
                    "com.aterm.aterm.acaffiene.dev.0000000c",
                    Path::new("/Volumes/Ejected/aterm.app/Contents/MacOS/aterm"),
                    None
                ),
                kept("com.aterm.aterm.acaffiene.dev.0000000e", &uncertain, None),
            ],
            "each job kept carries what its unload did, and only one whose program is gone here \
             has one"
        );

        let unloaded = std::cell::RefCell::new(Vec::new());
        prune_orphans_with(&paths, |label| {
            unloaded.borrow_mut().push(label.to_string());
            Unload::Unloaded
        });
        assert!(
            !paths.startup_plist(INSTALLED_LABEL).exists() && !paths.startup_plist(orphan).exists()
        );
        assert_eq!(
            unloaded.into_inner(),
            [INSTALLED_LABEL, orphan],
            "on a home on this Mac each orphan goes and is unloaded once, and nothing else is"
        );
    }

    /// An idle job is booted out; one with a process is left loaded and booted out never, as a
    /// bootout would end that process; one launchd has not loaded, or whose state cannot be read,
    /// is not booted out either. A refused bootout of a job launchd no longer has, as when another
    /// pass booted it out first, finds it not loaded; one of a job still loaded, or whose state
    /// cannot be read again, is a failure that says why.
    #[test]
    #[cfg(unix)]
    fn only_a_job_with_no_process_is_unloaded() {
        let booted = std::cell::Cell::new(0);
        let boot = |answer: io::Result<()>| {
            let booted = &booted;
            move || {
                booted.set(booted.get() + 1);
                answer
            }
        };
        let reads = |states: Vec<io::Result<JobState>>| {
            let mut states = states.into_iter();
            move || {
                states
                    .next()
                    .expect("launchd's state is read no more often than planned")
            }
        };
        let idle = || Ok(JobState::Loaded { pid: None });
        assert_eq!(
            unload_idle_with(reads(vec![idle()]), boot(Ok(()))),
            Unload::Unloaded
        );
        assert_eq!(booted.replace(0), 1, "an idle job is booted out");
        for (again, unload) in [
            (idle(), Unload::Failed("refused".to_string())),
            (
                Ok(JobState::Loaded { pid: Some(4242) }),
                Unload::Failed("refused".to_string()),
            ),
            (
                Err(io::Error::other("hung")),
                Unload::Failed("refused".to_string()),
            ),
            (Ok(JobState::Absent), Unload::NotLoaded),
        ] {
            assert_eq!(
                unload_idle_with(
                    reads(vec![idle(), again]),
                    boot(Err(io::Error::other("refused")))
                ),
                unload
            );
            assert_eq!(booted.replace(0), 1, "{unload:?} after one bootout");
        }
        for (state, unload) in [
            (
                Ok(JobState::Loaded { pid: Some(4242) }),
                Unload::Running(4242),
            ),
            (Ok(JobState::Absent), Unload::NotLoaded),
            (
                Err(io::Error::other("hung")),
                Unload::Failed("hung".to_string()),
            ),
        ] {
            assert_eq!(unload_idle_with(reads(vec![state]), boot(Ok(()))), unload);
            assert_eq!(booted.get(), 0, "{unload:?} is never booted out");
        }
    }

    #[test]
    fn only_a_program_inside_a_network_home_is_judged_here() {
        let home = Path::new("/Network/Users//x");
        let inside = Path::new("/Network/Users//x/Applications/aterm.app/Contents/MacOS/aterm");
        let outside = Path::new("/Applications/aterm.app/Contents/MacOS/aterm");
        assert!(judged(inside, home, true) && judged(outside, home, true));
        assert!(judged(inside, home, false));
        assert!(!judged(outside, home, false));
    }

    #[test]
    fn the_plist_names_the_job_and_escapes_every_string() {
        let text = plist_text(
            "com.aterm.aterm.acaffiene.dev.0123abcd",
            Path::new("/Users//x/A&B <\"q'>.app/Contents/MacOS/aterm"),
            Path::new("/Users//x/Library/Logs/aterm/acaffiene.log"),
            Some("com.aterm.aterm"),
        );
        assert!(text.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist"));
        assert!(text.ends_with("</dict>\n</plist>\n"));
        assert!(text.contains(
            "\t<key>Label</key>\n\t<string>com.aterm.aterm.acaffiene.dev.0123abcd</string>\n"
        ));
        assert!(text.contains(
            "\t<array>\n\
             \t\t<string>/Users//x/A&amp;B &lt;&quot;q&apos;&gt;.app/Contents/MacOS/aterm</string>\n\
             \t\t<string>acaffiene</string>\n\
             \t\t<string>serve</string>\n\
             \t</array>\n"
        ));
        assert!(!text.contains("A&B"), "an unescaped ampersand");
        assert!(text.contains("\t<key>RunAtLoad</key>\n\t<true/>\n"));
        assert!(text.contains(
            "\t<key>KeepAlive</key>\n\t<dict>\n\t\t<key>SuccessfulExit</key>\n\t\t<false/>\n\t</dict>\n"
        ));
        assert!(
            !text.contains("<key>KeepAlive</key>\n\t<true/>"),
            "never KeepAlive true: Quit must end the job"
        );
        assert!(text.contains("\t<key>ProcessType</key>\n\t<string>Interactive</string>\n"));
        assert!(text.contains("\t<key>LimitLoadToSessionType</key>\n\t<string>Aqua</string>\n"));
        assert!(text.contains(
            "\t<key>StandardErrorPath</key>\n\t<string>/Users//x/Library/Logs/aterm/acaffiene.log</string>\n"
        ));
        assert!(text.contains(
            "\t<key>AssociatedBundleIdentifiers</key>\n\t<array>\n\t\t<string>com.aterm.aterm</string>\n\t</array>\n"
        ));
        let bare = plist_text("l", Path::new("/t/aterm"), Path::new("/l.log"), None);
        assert!(!bare.contains("AssociatedBundleIdentifiers"));
    }

    /// `plutil -lint` reads the plist as launchd will: well formed, with and without the
    /// bundle row and with every character escaping covers.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_plist_is_well_formed_by_plutil() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let lint = |name: &str, text: &str| {
            let path = dir.path().join(name);
            std::fs::write(&path, text).expect("write");
            let out = aterm_uds::deadline::output_until(
                aterm_uds::spawnfd::command("/usr/bin/plutil")
                    .arg("-lint")
                    .arg(&path),
                std::time::Instant::now() + std::time::Duration::from_secs(60),
            )
            .expect("plutil");
            (
                out.status.success(),
                String::from_utf8_lossy(&out.stdout).into_owned(),
            )
        };
        let full = plist_text(
            "com.aterm.aterm.acaffiene",
            Path::new("/A&B <\"q'>/x.app/Contents/MacOS/aterm"),
            Path::new("/l/acaffiene & log.log"),
            Some("com.aterm.<aterm>"),
        );
        let (ok, said) = lint("full.plist", &full);
        assert!(ok, "{said}\n{full}");
        let bare = plist_text("l", Path::new("/t/aterm"), Path::new("/l.log"), None);
        let (ok, said) = lint("bare.plist", &bare);
        assert!(ok, "{said}\n{bare}");
        let broken = full.replace("A&amp;B", "A&B");
        let (ok, _) = lint("broken.plist", &broken);
        assert!(!ok, "the control lints");
    }

    #[test]
    fn the_launchctl_argv_is_plain() {
        let domain = gui_domain(501);
        assert_eq!(domain, "gui/501");
        assert_eq!(print_argv(&domain, "l.x"), ["print", "gui/501/l.x"]);
        assert_eq!(
            bootstrap_argv(&domain, Path::new("/h/Library/LaunchAgents/l.x.plist")),
            ["bootstrap", "gui/501", "/h/Library/LaunchAgents/l.x.plist"]
        );
        assert_eq!(kickstart_argv(&domain, "l.x"), ["kickstart", "gui/501/l.x"]);
        assert_eq!(bootout_argv(&domain, "l.x"), ["bootout", "gui/501/l.x"]);
        assert_eq!(print_disabled_argv(&domain), ["print-disabled", "gui/501"]);
        assert_eq!(enable_argv(&domain, "l.x"), ["enable", "gui/501/l.x"]);
    }

    #[test]
    fn a_disabled_label_is_its_own_row() {
        let text = "\tdisabled services = {\n\
                    \t\t\"com.aterm.aterm.acaffiene.dev.0000000d\" => enabled\n\
                    \t\t\"com.aterm.aterm.acaffiene\" => disabled\n\
                    \t\t\"com.apple.Siri.agent\" => disabled\n\
                    \t}\n";
        assert!(print_disabled_lists(text, "com.aterm.aterm.acaffiene"));
        assert!(!print_disabled_lists(
            text,
            "com.aterm.aterm.acaffiene.dev.0000000d"
        ));
        assert!(
            !print_disabled_lists(text, "com.aterm.aterm"),
            "a prefix of a listed label is not listed"
        );
        assert!(print_disabled_lists("\"l.x\" => true\n", "l.x"));
        assert!(!print_disabled_lists("\"l.x\" => false\n", "l.x"));
        assert!(!print_disabled_lists("", "l.x"));
    }

    #[test]
    fn the_pid_is_the_services_own_line() {
        let running = "gui/501/com.aterm.aterm.acaffiene = {\n\
                       \tactive count = 1\n\
                       \tpath = /Users//x/Library/LaunchAgents/com.aterm.aterm.acaffiene.plist\n\
                       \tstate = running\n\
                       \n\
                       \tenvironment = {\n\
                       \t\tXPC_SERVICE_NAME => com.aterm.aterm.acaffiene\n\
                       \t}\n\
                       \n\
                       \truns = 1\n\
                       \tpid = 1688\n\
                       \timmediate reason = non-ipc demand\n\
                       }\n";
        assert_eq!(print_pid(running), Some(1688));
        let stopped = "gui/501/l = {\n\tstate = not running\n\truns = 1\n\tlast exit code = 0\n}\n";
        assert_eq!(print_pid(stopped), None);
        let nested = "gui/501/l = {\n\tsection = {\n\t\tpid = 9\n\t}\n\tstate = not running\n}\n";
        assert_eq!(
            print_pid(nested),
            None,
            "a nested section's pid is not the job's"
        );
        assert_eq!(print_pid("\tpid = 0\n"), None);
        assert_eq!(print_pid("\tpid = x\n"), None);
        assert_eq!(print_pid("\tpid = 77  \n"), Some(77));
    }

    #[cfg(unix)]
    #[test]
    fn startup_is_the_login_plist_on_disk() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let paths = Paths {
            state_dir: root.path().join("state"),
            home: root.path().join("home"),
            logs_dir: root.path().join("logs"),
            local_volume: |_| true,
        };
        let label = "com.aterm.aterm.acaffiene.dev.0123abcd";
        let program = std::path::PathBuf::from("/t/target/debug/aterm");
        assert!(!startup_enabled(&paths, label));
        {
            set_startup(&paths, label, &program, false).expect("off");
            set_startup(&paths, label, &program, true).expect("on");
            assert!(startup_enabled(&paths, label));
            let plist = paths.startup_plist(label);
            assert_eq!(
                std::fs::read_to_string(&plist).expect("read"),
                plist_text(label, &program, &paths.log_file(), None)
            );
            let mode = std::fs::metadata(&plist)
                .expect("meta")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o644);
            assert!(paths.logs_dir.is_dir());
            set_startup(&paths, label, &program, true).expect("on");
            let names: Vec<_> = std::fs::read_dir(plist.parent().expect("dir"))
                .expect("list")
                .map(|e| e.expect("entry").file_name())
                .collect();
            assert_eq!(names, [std::ffi::OsString::from(format!("{label}.plist"))]);
            set_startup(&paths, label, &program, false).expect("off");
            assert!(!startup_enabled(&paths, label));
            assert!(!plist.exists());
        }
    }

    /// Login Items' and launchd's say count only over a plist that is there: with none,
    /// startup is off whatever they say. launchd's disable reads first: it is lifted first.
    #[cfg(unix)]
    #[test]
    fn a_login_plist_login_items_blocks_is_blocked() {
        let doors = |blocks: fn(&Path) -> bool, disabled: fn(&str) -> bool| StartupDoors {
            blocks,
            disabled,
            enable: |_| panic!("reading startup lifts nothing"),
        };
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let paths = Paths {
            state_dir: root.path().join("state"),
            home: root.path().join("home"),
            logs_dir: root.path().join("logs"),
            local_volume: |_| true,
        };
        let label = "com.aterm.aterm.acaffiene.dev.0123abcd";
        let program = std::path::PathBuf::from("/t/target/debug/aterm");
        let off = startup(&paths, label, &doors(|_| true, |_| true));
        assert_eq!(off, Startup::Off);
        set_startup(&paths, label, &program, true).expect("on");
        let on = startup(&paths, label, &doors(|_| false, |_| false));
        assert_eq!(on, Startup::On);
        let blocked = startup(&paths, label, &doors(|_| true, |_| false));
        assert_eq!(blocked, Startup::Blocked);
        let disabled = startup(&paths, label, &doors(|_| true, |_| true));
        assert_eq!(disabled, Startup::Disabled);
        let asked = startup(
            &paths,
            label,
            &doors(
                |plist| {
                    plist.ends_with(
                        "Library/LaunchAgents/com.aterm.aterm.acaffiene.dev.0123abcd.plist",
                    )
                },
                |label| label != "com.aterm.aterm.acaffiene.dev.0123abcd",
            ),
        );
        assert_eq!(
            asked,
            Startup::Blocked,
            "asked about the plist and its label"
        );
        let words = [
            Startup::Off,
            Startup::On,
            Startup::Blocked,
            Startup::Disabled,
        ]
        .map(Startup::word);
        assert_eq!(words, ["off", "on", "blocked", "disabled"]);
    }

    #[cfg(unix)]
    #[test]
    fn a_startup_plist_names_the_programs_bundle() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let app = root.path().join("X.app");
        std::fs::create_dir_all(app.join("Contents").join("MacOS")).expect("bundle");
        std::fs::write(
            app.join("Contents").join("Info.plist"),
            "<plist><dict><key>CFBundleIdentifier</key><string>com.x.y</string></dict></plist>",
        )
        .expect("plist");
        let paths = Paths {
            state_dir: root.path().join("state"),
            home: root.path().join("home"),
            logs_dir: root.path().join("logs"),
            local_volume: |_| true,
        };
        let program = app.join("Contents").join("MacOS").join("aterm");
        set_startup(&paths, "l", &program, true).expect("on");
        let text = std::fs::read_to_string(paths.startup_plist("l")).expect("read");
        assert!(text.contains("<string>com.x.y</string>"), "{text}");
    }

    #[cfg(unix)]
    #[test]
    fn a_session_plist_sits_in_a_private_directory() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let paths = Paths {
            state_dir: root.path().join("state"),
            home: root.path().join("home"),
            logs_dir: root.path().join("logs"),
            local_volume: |_| true,
        };
        let plist = paths.session_plist("l");
        write_session_plist(&paths, &plist, "l", Path::new("/t/aterm"), None).expect("write");
        let mode = |p: &Path| std::fs::metadata(p).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode(plist.parent().expect("dir")), 0o700);
        assert_eq!(mode(&plist), 0o644);
        assert_eq!(
            std::fs::read_to_string(&plist).expect("read"),
            plist_text("l", Path::new("/t/aterm"), &paths.log_file(), None)
        );

        let aliased = Paths {
            state_dir: root.path().join("alias-state"),
            ..paths.clone()
        };
        std::fs::create_dir_all(&aliased.state_dir).expect("alias state");
        let outside = root.path().join("outside");
        std::fs::create_dir(&outside).expect("outside");
        std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o750))
            .expect("outside mode");
        let marker = outside.join("keep");
        std::fs::write(&marker, b"unrelated bytes").expect("marker");
        let alias = aliased.state_dir.join("acaffiene");
        std::os::unix::fs::symlink(&outside, &alias).expect("directory alias");
        assert!(
            write_session_plist(
                &aliased,
                &aliased.session_plist("l"),
                "l",
                Path::new("/t/aterm"),
                None,
            )
            .is_err()
        );
        assert_eq!(mode(&outside), 0o750);
        assert_eq!(std::fs::read(&marker).expect("marker"), b"unrelated bytes");
        assert!(!outside.join("l.plist").exists());
        assert!(
            std::fs::symlink_metadata(&alias)
                .expect("alias retained")
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    #[cfg(unix)]
    fn a_sweep_takes_what_cut_short_plist_writes_left_beside_the_mugs_jobs() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let paths = Paths {
            state_dir: root.path().join("state"),
            home: root.path().join("home"),
            logs_dir: root.path().join("logs"),
            local_volume: |_| true,
        };
        assert!(sweep_plist_temps(&paths).is_empty(), "no folders at all");
        let ago = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 60 * 60);
        let plant = |dir: &Path, plist: &str, salt: &str, dated: bool| {
            std::fs::create_dir_all(dir).expect("dir");
            let temp = dir.join(format!(".{plist}.tmp.4294967295.{salt}"));
            let file = std::fs::File::create(&temp).expect("plant");
            if dated {
                file.set_modified(ago).expect("date");
            }
            temp
        };
        let (session, old, agents) = (
            paths.session_dir(),
            crate::legacy::session_dir(&paths),
            paths.launch_agents(),
        );
        let cut_short = [
            plant(
                &session,
                "com.aterm.aterm.acaffiene.dev.0123abcd.plist",
                "0123abcd",
                true,
            ),
            plant(&old, "com.aterm.aterm.caffeine.plist", "0123abcd", true),
            plant(&agents, "com.aterm.aterm.acaffiene.plist", "0123abcd", true),
            plant(&agents, "com.aterm.aterm.acaffiene.plist", "89abcdef", true),
            plant(
                &agents,
                "com.aterm.aterm.caffeine.dev.0123abcd.plist",
                "0123abcd",
                true,
            ),
        ];
        let kept = [
            plant(
                &session,
                "com.aterm.aterm.acaffiene.dev.0123abcd.plist",
                "fedcba98",
                false,
            ),
            plant(&agents, "com.example.other.plist", "0123abcd", true),
            plant(
                &agents,
                "com.aterm.aterm.acaffiene.dev.0123ABCD.plist",
                "0123abcd",
                true,
            ),
            plant(&agents, "com.aterm.aterm.acaffiene", "0123abcd", true),
        ];
        let swept = sweep_plist_temps(&paths);
        let answered: Vec<_> = swept
            .iter()
            .map(|(plist, gone)| (plist.clone(), *gone.as_ref().expect("swept")))
            .collect();
        assert_eq!(
            answered,
            [
                (
                    session.join("com.aterm.aterm.acaffiene.dev.0123abcd.plist"),
                    1
                ),
                (old.join("com.aterm.aterm.caffeine.plist"), 1),
                (agents.join("com.aterm.aterm.acaffiene.plist"), 2),
                (
                    agents.join("com.aterm.aterm.caffeine.dev.0123abcd.plist"),
                    1
                ),
            ]
        );
        for temp in &cut_short {
            assert!(!temp.exists(), "{}", temp.display());
        }
        for temp in &kept {
            assert!(temp.exists(), "{}", temp.display());
        }

        use std::os::unix::fs::PermissionsExt as _;
        let stuck = plant(
            &session,
            "com.aterm.aterm.acaffiene.plist",
            "0123abcd",
            true,
        );
        let mode = |bits| {
            std::fs::set_permissions(&session, std::fs::Permissions::from_mode(bits))
                .expect("chmod");
        };
        mode(0o300);
        let swept = sweep_plist_temps(&paths);
        mode(0o700);
        let answered: Vec<_> = swept
            .iter()
            .map(|(dir, gone)| (dir.clone(), gone.as_ref().map_err(io::Error::kind).copied()))
            .collect();
        assert_eq!(answered, [(session, Err(io::ErrorKind::PermissionDenied))]);
        assert!(stuck.exists(), "an unlisted folder's stay");
    }

    #[test]
    fn print_pid_rejects_signed_numbers() {
        assert_eq!(print_pid("\tpid = 42\n"), Some(42));
        assert_eq!(print_pid("\tpid = +42\n"), None);
        assert_eq!(print_pid("\tpid = -1\n"), None);
    }

    /// STARTUP-OFF AND LOGIN PLIST CHECKS REFUSE DIRECTORY AND FILE SYMLINKS: `choose_startup`
    /// writes `.startup-off` atomically (`0600`) inside a validated `0700` session directory
    /// without following symlinks, and `startup_enabled`, `startup`, and `startup_turned_off`
    /// inspect `symlink_metadata` rather than following symlinks.
    /// NEGATIVE CONTROL: a regular `.startup-off` and regular login plist round-trip cleanly.
    #[cfg(unix)]
    #[test]
    fn choose_startup_and_startup_readers_refuse_symlinks() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let paths = Paths {
            state_dir: root.path().join("state"),
            home: root.path().join("home"),
            logs_dir: root.path().join("logs"),
            local_volume: |_| true,
        };
        let label = "com.aterm.aterm.acaffiene.dev.0123abcd";
        let program = Path::new("/t/aterm");
        let doors = StartupDoors {
            blocks: |_| false,
            disabled: |_| false,
            enable: |_| Ok(()),
        };

        choose_startup(&paths, label, program, false).expect("record off");
        assert!(startup_turned_off(&paths, label));
        let record = paths.startup_off_file(label);
        let mode = |p: &Path| {
            std::fs::symlink_metadata(p)
                .expect("meta")
                .permissions()
                .mode()
                & 0o777
        };
        assert_eq!(mode(record.parent().expect("session dir")), 0o700);
        assert_eq!(mode(&record), 0o600);

        let keep = root.path().join("keep");
        std::fs::write(&keep, b"preserve").expect("seed keep");
        std::fs::remove_file(&record).expect("remove regular record");
        std::os::unix::fs::symlink(&keep, &record).expect("symlink record");
        assert!(
            !startup_turned_off(&paths, label),
            "symlinked startup-off record must not count as regular"
        );
        choose_startup(&paths, label, program, false).expect("atomic replace over symlink");
        assert_eq!(std::fs::read(&keep).expect("keep intact"), b"preserve");
        assert!(startup_turned_off(&paths, label));

        choose_startup(&paths, label, program, true).expect("record on");
        assert!(!startup_turned_off(&paths, label));
        assert!(startup_enabled(&paths, label));
        assert_eq!(startup(&paths, label, &doors), Startup::On);

        let plist = paths.startup_plist(label);
        let fake_plist = root.path().join("fake.plist");
        std::fs::write(&fake_plist, b"<plist/>").expect("fake plist");
        std::fs::remove_file(&plist).expect("remove regular plist");
        std::os::unix::fs::symlink(&fake_plist, &plist).expect("symlink plist");
        assert!(!startup_enabled(&paths, label));
        assert_eq!(startup(&paths, label, &doors), Startup::Off);

        let aliased = Paths {
            state_dir: root.path().join("alias-state"),
            ..paths.clone()
        };
        std::fs::create_dir_all(&aliased.state_dir).expect("alias state");
        let outside = root.path().join("outside");
        std::fs::create_dir(&outside).expect("outside");
        std::os::unix::fs::symlink(&outside, aliased.session_dir()).expect("symlink session dir");
        let outside_record = outside.join(format!("{label}.startup-off"));
        std::fs::write(&outside_record, b"do-not-delete").expect("seed outside record");
        assert!(
            choose_startup(&aliased, label, program, false).is_err(),
            "choose_startup(off) must refuse symlinked session_dir"
        );
        assert!(
            choose_startup(&aliased, label, program, true).is_err(),
            "choose_startup(on) must refuse symlinked session_dir without unlinking outside record"
        );
        assert!(
            outside_record.exists(),
            "outside record must remain untouched"
        );

        let outside_agents = root.path().join("outside-agents");
        std::fs::create_dir(&outside_agents).expect("outside agents");
        let outside_plist = outside_agents.join(format!("{label}.plist"));
        let gone_program = root.path().join("missing-aterm");
        std::fs::write(
            &outside_plist,
            plist_text(label, &gone_program, &paths.log_file(), None),
        )
        .expect("seed outside login plist");
        let outside_temp = outside_agents.join(format!(".{label}.plist.tmp.4294967295.0123abcd"));
        let temp_file = std::fs::File::create(&outside_temp).expect("seed outside temp");
        temp_file
            .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(7200))
            .expect("age outside temp");

        for (case, make_home) in [
            ("symlinked home", {
                let target_home = root.path().join("target-home");
                std::fs::create_dir_all(target_home.join("Library")).expect("target Library");
                std::os::unix::fs::symlink(
                    &outside_agents,
                    target_home.join("Library/LaunchAgents"),
                )
                .expect("link target LaunchAgents");
                let h = root.path().join("symlink-home");
                std::os::unix::fs::symlink(&target_home, &h).expect("symlink home");
                h
            }),
            ("symlinked Library", {
                let target_lib = root.path().join("target-lib");
                std::fs::create_dir(&target_lib).expect("target lib");
                std::os::unix::fs::symlink(&outside_agents, target_lib.join("LaunchAgents"))
                    .expect("link LaunchAgents");
                let h = root.path().join("home-with-symlink-lib");
                std::fs::create_dir(&h).expect("home");
                std::os::unix::fs::symlink(&target_lib, h.join("Library"))
                    .expect("symlink Library");
                h
            }),
            ("symlinked LaunchAgents", {
                let h = root.path().join("home-with-symlink-agents");
                std::fs::create_dir_all(h.join("Library")).expect("home Library");
                std::os::unix::fs::symlink(&outside_agents, h.join("Library/LaunchAgents"))
                    .expect("symlink LaunchAgents");
                h
            }),
        ] {
            let aliased_home = Paths {
                state_dir: root
                    .path()
                    .join(format!("state-{}", case.replace(' ', "-"))),
                home: make_home,
                logs_dir: root.path().join("logs"),
                local_volume: |_| true,
            };
            assert!(!startup_enabled(&aliased_home, label), "{case}");
            assert_eq!(
                startup(&aliased_home, label, &doors),
                Startup::Off,
                "{case}"
            );
            assert!(orphans(&aliased_home).is_empty(), "{case}");
            assert!(
                set_startup(&aliased_home, label, program, true).is_err(),
                "{case}: set_startup(on) must refuse symlinked LaunchAgents ancestry"
            );
            assert!(
                set_startup(&aliased_home, label, program, false).is_err(),
                "{case}: set_startup(off) must refuse symlinked LaunchAgents ancestry"
            );
            assert!(
                choose_startup(&aliased_home, label, program, true).is_err(),
                "{case}"
            );
            assert!(
                choose_startup(&aliased_home, label, program, false).is_err(),
                "{case}"
            );
            let swept = sweep_plist_temps(&aliased_home);
            assert_eq!(swept.len(), 1, "{case}");
            assert!(swept[0].1.is_err(), "{case}");
            assert!(
                outside_plist.exists() && outside_temp.exists(),
                "{case}: outside plist and temp must remain untouched"
            );
        }

        let outside_logs = root.path().join("outside-logs");
        std::fs::create_dir(&outside_logs).expect("outside logs");
        let symlink_logs = root.path().join("symlink-logs");
        std::os::unix::fs::symlink(&outside_logs, &symlink_logs).expect("symlink logs");
        let aliased_logs = Paths {
            state_dir: root.path().join("state-aliased-logs"),
            home: root.path().join("home-aliased-logs"),
            logs_dir: symlink_logs,
            local_volume: |_| true,
        };
        assert!(
            set_startup(&aliased_logs, label, program, true).is_err(),
            "set_startup(on) must refuse symlinked logs_dir"
        );
        assert!(
            !aliased_logs.startup_plist(label).exists(),
            "no login plist written when logs_dir is a symlink"
        );

        for bad_label in ["", ".", "..", "../escape", "a/b", "a\\b", "x.partial"] {
            assert!(set_startup(&paths, bad_label, program, true).is_err());
            assert!(choose_startup(&paths, bad_label, program, true).is_err());
            assert!(choose_startup(&paths, bad_label, program, false).is_err());
            assert!(!startup_enabled(&paths, bad_label));
            assert!(!startup_turned_off(&paths, bad_label));
            assert_eq!(startup(&paths, bad_label, &doors), Startup::Off);
        }
    }
}
