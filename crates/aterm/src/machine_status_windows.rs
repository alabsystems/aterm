// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm status` on Windows: what this machine can truthfully say of itself there. The areas
//! are this build and where it runs from, the tools, git, the AI CLIs, the disk and the aterm
//! instances that have published a control socket, each read as its own verb reads it, from
//! local state alone. An area whose subsystem is Unix-only (the macOS first-launch setup, ALab
//! membership, the secrets vault, the vault's fleet, the machine name) is one `off` line that
//! says so, so the verb answers instead of refusing.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aterm_agent::harness::disk::{Level as DiskLevel, Thresholds};
use aterm_sysprobe::VolumeReading;

use super::{
    Area, Clt, Level, READ_WITHIN, Who, agents_area_at, bounded, read_agents, read_toolset,
    tools_area,
};

/// The areas whose subsystem does not run on Windows, in the order the Unix read lists them, and
/// why each is absent.
const UNPORTED: [(&str, &str); 5] = [
    (
        "setup",
        "not available on Windows yet: the first-launch setup is macOS's",
    ),
    (
        "membership",
        "not available on Windows yet: ALab membership rests on the secrets vault",
    ),
    (
        "vault",
        "not available on Windows yet: the secrets vault keeps its files private with POSIX \
         file modes",
    ),
    (
        "fleet",
        "not available on Windows yet: the fleet's sync and presence row come from the vault",
    ),
    (
        "name",
        "not available on Windows yet: aterm names Macs only",
    ),
];

/// The area of a subsystem Windows does not have: `off`, with no step, since nobody can take one.
pub(crate) fn unported_areas() -> impl Iterator<Item = Area> {
    UNPORTED
        .into_iter()
        .map(|(name, says)| Area::off(name, says))
}

/// This build and where it runs from: its `<version>+g<commit>`, its cargo profile and the
/// running executable's path.
pub(crate) fn about_area(build_id: &str, profile: &str, exe: Option<&Path>) -> Area {
    const NAME: &str = "aterm";
    match exe {
        Some(exe) => Area::ok(
            NAME,
            format!("{build_id} ({profile} build) at {}", exe.display()),
        ),
        None => Area::ok(
            NAME,
            format!("{build_id} ({profile} build); its own path did not read"),
        ),
    }
}

/// The repositories area: the vault's inventory is what Unix judges and Windows has none, so the
/// line says that and where `git` is.
pub(crate) fn repos_area(git: Option<&Path>) -> Area {
    const NAME: &str = "repos";
    const ABSENT: &str = "the vault's repository clone is not available on Windows yet";
    match git {
        Some(git) => Area::off(NAME, format!("{ABSENT}; git is {}", git.display())),
        None => Area::off(NAME, format!("{ABSENT}; no git on PATH")),
    }
}

/// `bytes` in binary gigabytes to one decimal place, the unit Explorer shows a drive in.
fn gigabytes(bytes: u64) -> String {
    const GIB: u64 = 1 << 30;
    format!("{}.{} GB", bytes / GIB, (bytes % GIB) * 10 / GIB)
}

/// A volume's reading with the free-space lines in force on it: the `[disk]` knobs of
/// aterm.toml, each one not written raised to the volume's share and its kept room, as the
/// window's disk watch holds the volume to them.
pub(crate) struct DiskRead {
    pub(crate) volume: VolumeReading,
    pub(crate) lines: Thresholds,
}

/// The disk area: the volume holding `at` (the account's home), its free space and size, judged
/// against the lines the disk watch holds it to. Under any of them a step is due: `ok` is only
/// a volume with room.
pub(crate) fn disk_area(at: &Path, reading: Option<&DiskRead>) -> Area {
    const NAME: &str = "disk";
    const FREES: &str = "`aterm ctl @self await sys 'free>=room'` in an aterm tab waits while \
                         aterm frees build caches nobody uses";
    let Some(DiskRead { volume, lines }) = reading else {
        return Area::of(
            NAME,
            Level::Unknown,
            format!(
                "not read: the volume holding {} is not a local drive, or did not answer",
                at.display()
            ),
            Some((
                Who::Agent,
                "`aterm ctl @self status` in an aterm tab says `disk=` and `disk_free_mb=`"
                    .to_owned(),
            )),
        );
    };
    let line = format!(
        "{} ({}): {} free of {}",
        volume.mount.display(),
        volume.fs,
        gigabytes(volume.free),
        gigabytes(volume.total)
    );
    let short = match lines.level(Some(volume.free), None) {
        DiskLevel::Ok | DiskLevel::Unknown => return Area::ok(NAME, line),
        DiskLevel::Low => format!("low: under the {} warning line", gigabytes(lines.warn)),
        DiskLevel::Short => format!(
            "short: under the {} floor aterm frees build caches at",
            gigabytes(lines.floor)
        ),
        DiskLevel::Emergency => format!(
            "emergency: under {}, writes are about to fail",
            gigabytes(lines.emergency)
        ),
    };
    Area::of(
        NAME,
        Level::Due,
        format!("{line}; {short}"),
        Some((Who::Agent, FREES.to_owned())),
    )
}

/// The aterm instances the control directory names, read without dialling any.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Instances {
    /// The pids whose process runs and published the socket file naming it.
    pub(crate) running: Vec<u32>,
    /// How many socket files name a process that is gone (killed, crashed, or the PC lost
    /// power: `%TMP%` outlives a restart), or a pid a later process now holds.
    pub(crate) stale: usize,
    /// The other elevation's control directory, when it exists: this read did not list it.
    pub(crate) other: Option<OtherElevation>,
}

/// The control directory of the elevation this process does not run at.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct OtherElevation {
    /// Whether this process is elevated (so the directory is the unelevated instances').
    pub(crate) elevated: bool,
    pub(crate) dir: PathBuf,
}

/// The instances area: the aterm processes that run and have published a control socket here,
/// by pid, and how many socket files are left of processes that are gone. A published socket
/// is not an answer, so the line points at the verb that asks each.
pub(crate) fn instances_area(published: &Result<Instances, String>) -> Area {
    const NAME: &str = "instances";
    let found = match published {
        Err(why) => return Area::unread(NAME, why.as_str(), "aterm ctl ls"),
        Ok(found) => found,
    };
    let stale = match found.stale {
        0 => None,
        1 => Some("1 socket file of a process that is gone".to_owned()),
        n => Some(format!("{n} socket files of processes that are gone")),
    };
    let mut says = match (found.running.as_slice(), stale) {
        ([], None) => "none has published a control socket here".to_owned(),
        ([], Some(stale)) => format!("none running; {stale}"),
        (pids, stale) => {
            let listed: Vec<String> = pids.iter().map(u32::to_string).collect();
            format!(
                "{} running with a published control socket (pid {}){}; `aterm ctl ls` lists \
                 the sessions of each that answers",
                pids.len(),
                listed.join(", "),
                stale.map_or_else(String::new, |stale| format!(", and {stale}"))
            )
        }
    };
    match &found.other {
        Some(OtherElevation {
            elevated: true,
            dir,
        }) => says.push_str(&format!(
            "; this shell is elevated, so only elevated instances were read: unelevated ones \
             publish in {}",
            dir.display()
        )),
        Some(OtherElevation {
            elevated: false,
            dir,
        }) => says.push_str(&format!(
            "; elevated instances publish in {}, which only an elevated shell can list",
            dir.display()
        )),
        None => {}
    }
    Area::ok(NAME, says)
}

/// How far after a socket file's last write the process holding its pid may have started and
/// still be read as its publisher: the two clocks' grain, as a graph entry's writer is judged
/// (`aterm-ctl`'s `graph_hygiene`).
const PUBLISH_SLACK: Duration = Duration::from_secs(2);

/// Whether the process holding a socket file's pid is the instance that published it: it runs
/// (`alive`), and it did not start (`born`) after the file was last written (`published`), as a
/// process that took the pid after its publisher died did. A time that did not read decides
/// nothing.
pub(crate) fn publishes(
    alive: bool,
    born: Option<SystemTime>,
    published: Option<SystemTime>,
) -> bool {
    alive
        && !matches!(
            (born, published),
            (Some(born), Some(published))
                if published.checked_add(PUBLISH_SLACK).is_some_and(|latest| born > latest)
        )
}

/// When process `pid` started, on the wall clock.
fn born(pid: u32) -> Option<SystemTime> {
    let birth = aterm_uds::process::read_process_birth(i32::try_from(pid).ok()?)?;
    UNIX_EPOCH
        .checked_add(Duration::from_secs(birth.seconds))?
        .checked_add(Duration::from_micros(birth.microseconds))
}

/// The first `git.exe` on `PATH`.
fn read_git() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    aterm_agent::harness::login_drive::on_path(Path::new("git.exe"), &path)
}

/// The folder whose volume the disk area reads, and that volume with the lines in force on it:
/// the account's home, else the folder this executable is in.
fn read_disk() -> Option<(PathBuf, Option<DiskRead>)> {
    let at = aterm_types::dirs::home_dir().or_else(|| {
        std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(Path::to_path_buf))
    })?;
    let reading = aterm_sysprobe::volume(&at).map(|volume| DiskRead {
        lines: read_lines(&at, volume.total),
        volume,
    });
    Some((at, reading))
}

/// The free-space lines in force on the volume of `total` bytes holding `at`, as
/// `aterm harness disk` reads them: aterm.toml's `[disk]` knobs and the room the disk watch has
/// kept for the volume. Reads files only.
fn read_lines(at: &Path, total: u64) -> Thresholds {
    use aterm_agent::harness::{cli, disk};
    let knobs = cli::disk_config_read(aterm_types::dirs::aterm_config_path().as_deref());
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|since| i64::try_from(since.as_secs()).ok());
    let room = disk::volume_of(at, &aterm_sysprobe::volumes::table())
        .ok()
        .zip(cli::harness_state_dir().ok())
        .zip(now)
        .map(|((volume, state), now)| disk::room::kept_room(&state, &volume, now));
    knobs.thresholds_with(Some(total), room)
}

/// The instances that have published a control socket, as `aterm ctl ls` finds them, without
/// dialling any: each socket file's pid is counted as running only while its process runs and
/// is the one that published it ([`publishes`]). A control directory nobody has made yet holds
/// none.
fn read_instances() -> Result<Instances, String> {
    let dir = aterm_uds::control_socket_dir();
    let mut found = Instances {
        other: dir.as_deref().and_then(read_other_elevation),
        ..Instances::default()
    };
    if dir.is_some_and(|dir| matches!(dir.try_exists(), Ok(false))) {
        return Ok(found);
    }
    let mut published = aterm_ctl::local_instances().map_err(|error| error.reason)?;
    published.sort_unstable();
    published.dedup_by_key(|(pid, _sock)| *pid);
    for (pid, sock) in published {
        let written = std::fs::symlink_metadata(&sock)
            .and_then(|meta| meta.modified())
            .ok();
        if publishes(aterm_uds::process::pid_alive(pid), born(pid), written) {
            found.running.push(pid);
        } else {
            found.stale += 1;
        }
    }
    Ok(found)
}

/// The control directory of the other elevation beside `dir`, when one exists: an elevated
/// process publishes and lists in one folder and an unelevated one in its sibling, so each
/// read misses the other's instances (`aterm ctl ls` notes the same).
fn read_other_elevation(dir: &Path) -> Option<OtherElevation> {
    let elevated = aterm_uds::elevation::is_elevated();
    let other = dir.with_file_name(if elevated {
        aterm_uds::CONTROL_DIR_NAME
    } else {
        aterm_uds::ELEVATED_CONTROL_DIR_NAME
    });
    other.is_dir().then_some(OtherElevation {
        elevated,
        dir: other,
    })
}

/// Every area, each read on its own thread under one [`READ_WITHIN`] bound.
pub(super) fn read_areas() -> Vec<Area> {
    let deadline = std::time::Instant::now() + READ_WITHIN;
    let late = |name: &'static str, reader: &str| {
        Area::unread(
            name,
            format!(
                "its reader did not answer within {} s",
                READ_WITHIN.as_secs()
            ),
            reader,
        )
    };
    let toolset = bounded(read_toolset);
    let git = bounded(read_git);
    let agents = bounded(read_agents);
    let disk = bounded(read_disk);
    let instances = bounded(read_instances);

    let stamp = aterm_stamp::current();
    let mut areas = Vec::with_capacity(11);
    areas.push(about_area(
        &stamp.build_id(),
        stamp.build_profile,
        std::env::current_exe().ok().as_deref(),
    ));
    areas.extend(unported_areas());
    areas.push(match toolset(deadline) {
        Some(toolset) => tools_area(Clt::NotHere, &toolset),
        None => late("tools", "aterm pkg doctor"),
    });
    areas.push(git(deadline).map_or_else(
        || late("repos", "aterm agh --help"),
        |git| repos_area(git.as_deref()),
    ));
    areas.push(match agents(deadline) {
        Some(agents) => agents_area_at(&agents, "this PC"),
        None => late("agents", "aterm ai status"),
    });
    areas.push(match disk(deadline) {
        Some(Some((at, reading))) => disk_area(&at, reading.as_ref()),
        Some(None) => Area::unread(
            "disk",
            "%USERPROFILE% is not set and this executable's folder did not read",
            "aterm ctl @self status",
        ),
        None => late("disk", "aterm ctl @self status"),
    });
    areas.push(match instances(deadline) {
        Some(published) => instances_area(&published),
        None => late("instances", "aterm ctl ls"),
    });
    areas
}

#[cfg(test)]
#[path = "machine_status_windows_tests.rs"]
mod tests;
