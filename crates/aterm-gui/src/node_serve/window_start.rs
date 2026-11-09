// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! How the node job starts a window on Linux: in a systemd scope of its own, as a desktop
//! launcher starts an app (systemd's desktop-environment convention puts each app in an
//! `app-*.scope` of the user manager's `app.slice`), so a stop or restart of the node's unit
//! (`systems.alab.aterm-node.service`, whose `KillMode=control-group` ends every process in its
//! cgroup) never ends the window or its shells. Where no systemd user manager runs the node, or
//! `systemd-run` cannot start the scope, the window is the node's own child, and the log says so.

use std::ffi::OsStr;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_uds::spawnfd::{Child, Command, Stdio};

/// The program that asks the user manager for the scope and then execs the window in place, at
/// the path the OS keeps it (never a `PATH` lookup: the node's `PATH` leads with folders a
/// package manager fills).
const SYSTEMD_RUN: &str = aterm_uds::os_path!("systemd-run");

/// How long `systemd-run` may take to register the scope and exec the window before it is killed
/// and the window starts as the node's own child, so a wedged user manager never holds the fleet
/// connection that asked for a window.
const SCOPE_WITHIN: Duration = Duration::from_secs(30);

/// How often [`await_exec`] looks at the launcher again.
const LOOK_EVERY: Duration = Duration::from_millis(5);

/// The variables systemd's service manager sets for a unit's own processes that name that unit or
/// hand it something of its own (`systemd.exec(5)`, "Environment Variables in Spawned
/// Processes"): its invocation, its journal stream, its main and manager pids, its sockets,
/// notification and watchdog, its directories and credentials, its memory-pressure file, its
/// trigger and its result. A window the node starts is not the node's unit, so none of these is
/// the window's, and its shells would carry them otherwise. The session's variables the same page
/// lists (`PATH`, `LANG`, `USER`, `LOGNAME`, `HOME`, `SHELL`, `TERM`, `XDG_RUNTIME_DIR`) stay, as
/// does every other variable the node has (`DISPLAY`, `WAYLAND_DISPLAY`, `XAUTHORITY`,
/// `DBUS_SESSION_BUS_ADDRESS`, the rest of `XDG_*`, the launcher's descriptor limit).
pub(super) const UNIT_VARIABLES: &[&str] = &[
    "INVOCATION_ID",
    "JOURNAL_STREAM",
    "SYSTEMD_EXEC_PID",
    "MAINPID",
    "MANAGERPID",
    "NOTIFY_SOCKET",
    "LISTEN_PID",
    "LISTEN_FDS",
    "LISTEN_FDNAMES",
    "WATCHDOG_PID",
    "WATCHDOG_USEC",
    "FDSTORE",
    "PIDFILE",
    "RUNTIME_DIRECTORY",
    "STATE_DIRECTORY",
    "CACHE_DIRECTORY",
    "LOGS_DIRECTORY",
    "CONFIGURATION_DIRECTORY",
    "CREDENTIALS_DIRECTORY",
    "LOG_NAMESPACE",
    "MEMORY_PRESSURE_WATCH",
    "MEMORY_PRESSURE_WRITE",
    "REMOTE_ADDR",
    "REMOTE_PORT",
    "TRIGGER_UNIT",
    "TRIGGER_PATH",
    "TRIGGER_TIMER_REALTIME_USEC",
    "TRIGGER_TIMER_MONOTONIC_USEC",
    "SERVICE_RESULT",
    "EXIT_CODE",
    "EXIT_STATUS",
    "MONITOR_SERVICE_RESULT",
    "MONITOR_EXIT_CODE",
    "MONITOR_EXIT_STATUS",
    "MONITOR_INVOCATION_ID",
    "MONITOR_UNIT",
];

/// How the node starts a window ([`decide`]).
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Start {
    /// `systemd-run --user --scope`: the user manager registers the scope `unit` for
    /// `systemd-run`'s own pid, then `systemd-run` execs the window in place, so that pid is the
    /// window's and the window lives in its own cgroup.
    Scope { systemd_run: PathBuf, unit: String },
    /// The node's own child, in the node unit's cgroup, for the reason given.
    Child(&'static str),
}

/// A window the node started ([`start`]).
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Started {
    /// The window's pid.
    pub(super) pid: u32,
    /// Where it lives.
    pub(super) home: Home,
}

/// Where a started window lives.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Home {
    /// In its own scope, this unit.
    Scope(String),
    /// In the node unit's cgroup, as the node's own child, for the reason given.
    Child(&'static str),
}

impl std::fmt::Display for Started {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let pid = self.pid;
        match &self.home {
            Home::Scope(unit) => write!(
                f,
                "a window (pid {pid}) in its own scope {unit}, so a stop of the node's unit leaves \
                 it running"
            ),
            Home::Child(why) => write!(
                f,
                "a window (pid {pid}) as the node's own child ({why}); a stop of the node's unit \
                 ends it"
            ),
        }
    }
}

/// Start `program args` as a window the node launches: in a scope of its own where [`decide`]
/// says so, else, or where `systemd-run` could not start the scope ([`await_exec`]), as the
/// node's own child. Each outcome is logged.
///
/// # Errors
/// The node's own child could not be spawned.
pub(super) fn start(program: &Path, args: &[&OsStr]) -> Result<Started, String> {
    let start = decide_here();
    let why = match &start {
        Start::Scope { systemd_run, unit } => {
            match spawn_in_scope(&start, systemd_run, program, args) {
                Ok(pid) => {
                    let started = Started {
                        pid,
                        home: Home::Scope(unit.clone()),
                    };
                    aterm_log::info!("node: started {started}");
                    return Ok(started);
                }
                Err(why) => {
                    aterm_log::warn!(
                        "node: systemd-run could not start the window in its own scope {unit} \
                         ({why}); it starts as the node's own child instead"
                    );
                    "systemd-run could not start its scope"
                }
            }
        }
        Start::Child(why) => *why,
    };
    let child = command(&Start::Child(why), program, args)
        .spawn()
        .map_err(|e| format!("{}: {e}", program.display()))?;
    let started = Started {
        pid: child.id(),
        home: Home::Child(why),
    };
    aterm_log::info!("node: started {started}");
    Ok(started)
}

/// [`decide`] on this process: its own cgroup, its uid, [`SYSTEMD_RUN`] where it is a program,
/// and a random scope name.
fn decide_here() -> Start {
    let cgroup = std::fs::read_to_string("/proc/self/cgroup").ok();
    let systemd_run = program_at(Path::new(SYSTEMD_RUN));
    let random = aterm_uds::rand::hex_token::<8>().ok();
    decide(
        cgroup.as_deref(),
        aterm_uds::peer::our_uid(),
        systemd_run,
        random,
    )
}

/// How a node whose `/proc/self/cgroup` reads `cgroup`, running as `uid`, with `systemd_run`
/// found and `random` drawn, starts a window: in a scope of its own when the user manager
/// `user@<uid>.service` runs it (that manager is the one `systemd-run --user` asks) and the
/// other two are at hand, else as its own child.
pub(super) fn decide(
    cgroup: Option<&str>,
    uid: u32,
    systemd_run: Option<PathBuf>,
    random: Option<String>,
) -> Start {
    if !cgroup.is_some_and(|text| under_user_manager(text, uid)) {
        return Start::Child("no systemd user manager runs the node");
    }
    let Some(systemd_run) = systemd_run else {
        return Start::Child("no systemd-run where the OS keeps it");
    };
    let Some(random) = random else {
        return Start::Child("no random scope name could be drawn");
    };
    Start::Scope {
        systemd_run,
        unit: scope_unit(&random),
    }
}

/// Whether a `/proc/<pid>/cgroup` text places the process under `uid`'s systemd user manager:
/// some line's path (`<id>:<controllers>:<path>`, the unified `0::` line or a legacy
/// `name=systemd` one) has the component `user@<uid>.service`.
fn under_user_manager(cgroup: &str, uid: u32) -> bool {
    let manager = format!("user@{uid}.service");
    cgroup.lines().any(|line| {
        line.splitn(3, ':')
            .nth(2)
            .is_some_and(|path| path.split('/').any(|component| component == manager))
    })
}

/// The window's scope: `app-aterm-<random>.scope`, systemd's desktop-environment name for an
/// application's scope (`app[-<launcher>]-<ApplicationID>-<RANDOM>.scope`), `aterm` being the ID
/// of the desktop entry `aterm.desktop`.
pub(super) fn scope_unit(random: &str) -> String {
    format!("app-aterm-{random}.scope")
}

/// `file`, where it is a regular file (a link followed) with an execute bit.
pub(super) fn program_at(file: &Path) -> Option<PathBuf> {
    std::fs::metadata(file)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .then(|| file.to_path_buf())
}

/// The command that starts `program args` the way `start` says, its standard streams on
/// `/dev/null`, without the node unit's [`UNIT_VARIABLES`], and with the launcher's descriptor
/// limit ([`crate::descriptor_limit::window_launcher_limit`]): every other variable of the node
/// reaches the window. A scope's `systemd-run` keeps the environment, limits and working
/// directory it was given when it execs the window, adding only the scope's own
/// `INVOCATION_ID`.
pub(super) fn command(start: &Start, program: &Path, args: &[&OsStr]) -> Command {
    let mut command = match start {
        Start::Scope { systemd_run, unit } => {
            let mut command = aterm_uds::spawnfd::command(systemd_run);
            command
                .args([
                    "--user",
                    "--scope",
                    "--collect",
                    "--quiet",
                    "--slice=app.slice",
                    "--description=aterm",
                ])
                .arg(format!("--unit={unit}"))
                .arg("--")
                .arg(program);
            command
        }
        Start::Child(_) => aterm_uds::spawnfd::command(program),
    };
    command.args(args);
    for name in UNIT_VARIABLES {
        command.env_remove(name);
    }
    command
        .envs(crate::descriptor_limit::window_launcher_limit())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

/// Spawn [`command`] for the scope `start`, and wait until `systemd_run` has execed the window
/// in it ([`await_exec`]): the window's pid, or why it did not. The launcher's image is read just
/// before the spawn, so only a `systemd-run` replaced on disk between that read and the child's
/// exec of it could be taken for the window.
fn spawn_in_scope(
    start: &Start,
    systemd_run: &Path,
    program: &Path,
    args: &[&OsStr],
) -> Result<u32, String> {
    let launcher = Launcher::of(systemd_run)?;
    let mut child = command(start, program, args)
        .spawn()
        .map_err(|e| format!("{}: {e}", systemd_run.display()))?;
    let now = Instant::now();
    let deadline = now.checked_add(SCOPE_WITHIN).unwrap_or(now);
    await_exec(&mut child, &launcher, deadline)?;
    Ok(child.id())
}

/// A file's identity: the device and inode `stat` gives it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Image {
    dev: u64,
    ino: u64,
}

impl Image {
    /// The identity of the file at `path`, a symbolic link followed: for `/proc/<pid>/exe`, the
    /// image that process runs.
    ///
    /// # Errors
    /// `stat`'s.
    pub(super) fn of(path: &Path) -> std::io::Result<Self> {
        let meta = std::fs::metadata(path)?;
        Ok(Self {
            dev: meta.dev(),
            ino: meta.ino(),
        })
    }
}

/// The launcher a window's start waits on ([`await_exec`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Launcher {
    /// The name the kernel gives a process it runs ([`process_name`]).
    pub(super) name: String,
    /// Its program file.
    pub(super) image: Image,
}

impl Launcher {
    /// The launcher `program`.
    ///
    /// # Errors
    /// Its file cannot be read.
    pub(super) fn of(program: &Path) -> Result<Self, String> {
        let image = Image::of(program).map_err(|e| format!("{}: {e}", program.display()))?;
        Ok(Self {
            name: process_name(program),
            image,
        })
    }
}

/// The name the kernel gives a process `program` runs (`/proc/<pid>/comm`): its file name, cut
/// to the 15 bytes `TASK_COMM_LEN` keeps.
pub(super) fn process_name(program: &Path) -> String {
    let name = program
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut end = name.len().min(15);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    name[..end].to_owned()
}

/// One look at a launcher [`await_exec`] waits on.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Look {
    /// It runs, this image (`/proc/<pid>/exe`), `None` where that could not be read (it may have
    /// ended since it was seen running).
    Running(Option<Image>),
    /// It ended, unreaped, this its last process name (`/proc/<pid>/comm`), `None` where that
    /// could not be read.
    Ended(Option<String>),
}

/// What a [`Look`] says of the launcher ([`judge`]).
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Verdict {
    /// It execed its command in place, which runs now or ran and ended.
    Execed,
    /// It still runs its own image, or it could not be told.
    Launching,
    /// It ended without execing its command.
    Ended,
}

/// What `look` says of `launcher`, a child whose spawn has returned. The spawn returns once the
/// child's exec of the launcher has closed its close-on-exec descriptors, which the kernel does
/// after it points the child's `/proc/<pid>/exe` at the launcher's image but before it sets the
/// child's process name (`begin_new_exec`), so a running child's name may still be the one it
/// was forked with, any name at all, while its image is already the launcher's. A running child
/// is therefore judged by its image alone: another image than the launcher's is its command's.
/// An ended child's name is final (the exec sets it before the child can run, so before it can
/// end), and is the launcher's unless the launcher execed its command. A command whose image is
/// the launcher's own file is not seen as execed while it runs.
pub(super) fn judge(launcher: &Launcher, look: &Look) -> Verdict {
    match look {
        Look::Running(Some(image)) if *image != launcher.image => Verdict::Execed,
        Look::Running(_) => Verdict::Launching,
        Look::Ended(Some(name)) if *name != launcher.name => Verdict::Execed,
        Look::Ended(_) => Verdict::Ended,
    }
}

/// Wait until `child`, a spawned `launcher`, has execed its command in place (`Ok`, whether that
/// command runs now or ran and ended), has ended without doing so (what it exited with), or
/// `deadline` passes (it is then killed and reaped), each look [`judge`]d. Each look asks whether
/// the child ended before it reads anything of it, and an ended child this handle has not reaped
/// keeps its pid and its last name, so the name read of an ended child is its last one.
///
/// # Errors
/// The launcher ended without execing its command, or was killed because it had not by
/// `deadline` or because its exit could not be watched.
pub(super) fn await_exec(
    child: &mut Child,
    launcher: &Launcher,
    deadline: Instant,
) -> Result<(), String> {
    let pid = child.id();
    let name = &launcher.name;
    loop {
        let ended = match child.exited() {
            Ok(ended) => ended,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "cannot watch {name} (pid {pid}), so it was killed: {e}"
                ));
            }
        };
        let look = if ended {
            Look::Ended(
                std::fs::read_to_string(format!("/proc/{pid}/comm"))
                    .ok()
                    .map(|name| name.trim_end_matches('\n').to_owned()),
            )
        } else {
            Look::Running(Image::of(Path::new(&format!("/proc/{pid}/exe"))).ok())
        };
        match judge(launcher, &look) {
            Verdict::Execed => return Ok(()),
            Verdict::Ended => {
                let status = child
                    .wait()
                    .map_err(|e| format!("cannot reap {name} (pid {pid}): {e}"))?;
                return Err(format!("{name} exited with {status}"));
            }
            Verdict::Launching => {}
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "{name} had not execed the window by its deadline, so it was killed"
            ));
        }
        std::thread::sleep(LOOK_EVERY);
    }
}

#[cfg(test)]
#[path = "window_start_tests.rs"]
mod tests;
