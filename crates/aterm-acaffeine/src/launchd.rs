// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Launchd job plist ([`crate::paths::Paths::session_plist`], [`EXIT_RESTART`], [`bootout`]).

use std::io;
use std::path::Path;

use crate::paths::Paths;

/// The installed app's label.
pub const INSTALLED_LABEL: &str = "com.aterm.aterm.acaffeine";
/// The prefix of any other build's label.
pub const DEV_LABEL_PREFIX: &str = "com.aterm.aterm.acaffeine.dev";
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
    /// Its executable, which has the `acaffeine` verb.
    At(std::path::PathBuf),
    /// None is installed, or each one installed is an aterm with no `acaffeine` verb, whose own
    /// Caffeine, if any, is its own: why.
    Absent(String),
    /// One did not say whether it has the verb (its first exec macOS still assesses, say): why.
    /// The next pass asks again.
    Unanswered(String),
}

/// The first of `present` ([`installed_present`]) that has the `acaffeine` verb: `own`, this
/// process's executable, without asking, and any other as `serves` answers
/// ([`crate::code::serves_acaffeine`]). An installed aterm older than the verb is never started
/// as an aCaffeine job, which its KeepAlive would start again every ten seconds.
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
                "{} has no `acaffeine` verb (an aterm older than aCaffeine)",
                exe.display()
            )),
            None => unanswered.push(format!("{} did not answer `help acaffeine`", exe.display())),
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
pub const SERVE_ARGS: [&str; 2] = ["acaffeine", "serve"];
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
const LAUNCHCTL_HANG: std::time::Duration = std::time::Duration::from_secs(60);

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

/// Render the launchd plist for `label` running `program acaffeine serve` ([`EXIT_RESTART`]).
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
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        let file_name = entry.file_name();
        let Some(label) = file_name
            .to_str()
            .and_then(|name| name.strip_suffix(".plist"))
            .filter(|label| is_label(label))
            .map(str::to_string)
        else {
            continue;
        };
        let plist = entry.path();
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
    launchctl_after(Path::new(LAUNCHCTL), argv, std::time::Duration::ZERO)
}

#[cfg(unix)]
fn launchctl_after(
    program: &Path,
    argv: &[String],
    elapsed: std::time::Duration,
) -> io::Result<std::process::Output> {
    let remaining = LAUNCHCTL_HANG
        .checked_sub(elapsed)
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::TimedOut, "launchctl hang detector elapsed")
        })?;
    let mut cmd = aterm_uds::spawnfd::command(program);
    cmd.args(argv);
    aterm_uds::deadline::output_within(&mut cmd, remaining)
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
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_DIRECTORY | libc::O_CLOEXEC)
        .open(dir)?;
    let meta = directory.metadata()?;
    if !meta.is_dir() || meta.uid() != aterm_uds::peer::our_uid() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is not this user's directory", dir.display()),
        ));
    }
    if meta.permissions().mode() & 0o7777 != 0o700 {
        directory.set_permissions(std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
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
#[path = "launchd_tests.rs"]
mod tests;
