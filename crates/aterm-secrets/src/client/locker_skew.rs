// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A publish that moves the admin locker to a newer format first checks that the approver this
//! Mac runs can read it. An approver that cannot open the locker lets no machine join, and the
//! locker never returns to its old format, so the check comes before anything is published or
//! retired. It reads this Mac alone: an approver another admin Mac runs is not checked, so every
//! admin Mac's subscriber is updated before any admin publishes with a newer build. A founding of
//! Commons by default reads the same approvers ([`approvers`]) before it publishes.

use super::{Error, Paths};
use crate::service::{self, Job};
use std::path::{Path, PathBuf};

/// A release number: `MAJOR.MINOR.PATCH`.
pub(super) type Version = (u64, u64, u64);

/// This build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Own {
    /// Its release number.
    pub(super) version: Version,
    /// Whether the release lane built it. A development build shares its number with the
    /// release before it, which may not read a format the build writes; the number moves only
    /// in a release's own commit, so any build naming a release has everything that release has.
    pub(super) release: bool,
    /// Its program, where the kernel names it.
    pub(super) program: Option<PathBuf>,
}

/// A program this Mac's subscriber approves with: the one running now, or the one its job
/// starts next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Running {
    /// The subscriber's pid, when this is the program running now.
    pub(super) pid: Option<u32>,
    /// The program.
    pub(super) program: PathBuf,
    /// The release `program --version` names, or why it named none.
    pub(super) version: Result<Version, String>,
}

/// The `MAJOR.MINOR.PATCH` that `text` starts with: `0.140.0+g1a2b3c` reads as `0.140.0`.
pub(super) fn version(text: &str) -> Option<Version> {
    let core = text.split(['+', '-']).next()?;
    let mut parts = core.split('.').map(|part| {
        (!part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
            .then(|| part.parse::<u64>().ok())
            .flatten()
    });
    let version = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(version)
}

/// The release a `--version` answer names on its first line, `aterm <version>`.
#[cfg(test)]
pub(super) fn version_answer(stdout: &str) -> Option<Version> {
    version(stdout.lines().next()?.strip_prefix("aterm ")?.trim())
}

pub(super) fn dotted((major, minor, patch): Version) -> String {
    format!("{major}.{minor}.{patch}")
}

/// Refuse moving the admin locker from `from` to `to` unless `running` is this program, a
/// release that reads `to` where a release before this build introduced it ([`first_reader`]),
/// a newer release, or this release when this build is that release.
pub(super) fn judge(from: &str, to: &str, own: &Own, running: &Running) -> crate::Result<()> {
    if own.program.as_ref() == Some(&running.program) {
        return Ok(());
    }
    if let (Ok(version), Some(need)) = (&running.version, first_reader(to, own.version))
        && need < own.version
        && *version >= need
    {
        return Ok(());
    }
    let what = match &running.version {
        Ok(version) if *version > own.version || (*version == own.version && own.release) => {
            return Ok(());
        }
        Ok(version) if *version == own.version => format!(
            "aterm {}, a release this development build may write past",
            dotted(*version)
        ),
        Ok(version) => format!("aterm {}", dotted(*version)),
        Err(why) => format!("a program whose release is unknown: {why}"),
    };
    let who = match running.pid {
        Some(pid) => format!("pid {pid}"),
        None => "the program its job starts next".to_string(),
    };
    let program = running.program.display();
    Err(Error::new(format!(
        "refusing to publish: this aterm ({}) would rewrite the vault's admin locker from {from} \
         to {to}, and the approver this Mac runs ({who}, {program}, {what}) may not read it; an \
         approver that cannot read the locker lets no machine join, and the locker never returns \
         to {from}; publish with that program instead ({program} secrets …), or first move this \
         Mac's subscriber to this aterm or a newer one",
        dotted(own.version),
    )))
}

/// The programs a subscriber job in `state` approves with: the one running now
/// (`running_program` of its pid), and the one its unit starts next (`unit_program`) where that
/// differs. Why none are named where the job's state is unknown, or an installed job's program
/// cannot be named at all: an unchecked approver is the one each reader of this exists for.
pub(super) fn programs(
    state: crate::Result<service::State>,
    running_program: &dyn Fn(u32) -> Option<PathBuf>,
    unit_program: &dyn Fn(&Path) -> Option<PathBuf>,
) -> Result<Vec<(Option<u32>, PathBuf)>, String> {
    let (pid, unit) = match state {
        Ok(service::State::Absent) => return Ok(Vec::new()),
        Ok(service::State::Installed { pid, unit }) => (pid, unit),
        Err(error) => {
            return Err(format!(
                "whether this Mac's subscriber runs is unknown ({error})"
            ));
        }
    };
    let mut programs = Vec::new();
    if let Some((pid, program)) = pid.and_then(|pid| Some((pid, running_program(pid)?))) {
        programs.push((Some(pid), program));
    }
    match unit_program(&unit) {
        Some(next) if programs.iter().all(|(_, program)| *program != next) => {
            programs.push((None, next));
        }
        None if programs.is_empty() => {
            return Err(format!(
                "the program {} starts cannot be named",
                unit.display()
            ));
        }
        _ => {}
    }
    Ok(programs)
}

#[cfg(test)]
thread_local! {
    /// The approvers [`approvers`] reads on this thread, in place of the service's.
    pub(super) static PLAYED: std::cell::RefCell<Option<Vec<Running>>> =
        const { std::cell::RefCell::new(None) };
}

/// The release this crate's test binary runs as: it installs no identity (`aterm_stamp`), so
/// [`own`] takes this fixed release, the one current when the product version left the library
/// graph, and no bump is an input of the tests.
#[cfg(test)]
pub(super) const TEST_RELEASE: Version = (0, 156, 0);

/// This build, as [`judge`] compares it: the release its installed identity names.
///
/// # Errors
/// A process that installed no identity, or whose version does not read as a release: what it
/// may publish depends on which release it is, so it publishes nothing.
pub(super) fn own() -> crate::Result<Own> {
    #[cfg(test)]
    let unstamped = Some(TEST_RELEASE);
    #[cfg(not(test))]
    let unstamped = None;
    let release = aterm_stamp::installed().map_or(unstamped, |stamp| version(stamp.app_version));
    Ok(Own {
        version: release.ok_or_else(|| Error::new("this aterm's release number is unreadable"))?,
        release: aterm_stamp::installed().is_some_and(|stamp| stamp.release_build),
        program: std::env::current_exe().ok().map(canonical),
    })
}

/// [`judge`] for this Mac: refuse a publish that moves the admin locker from `from` to `to`
/// while this Mac's subscriber runs, or would start, an older program than this one.
pub(super) fn check(paths: &Paths, from: &str, to: &str) -> crate::Result<()> {
    let own = own()?;
    let running = approvers(paths, &own).map_err(|why| {
        Error::new(format!(
            "refusing to publish: it would rewrite the vault's admin locker into a newer format, \
             which an older approver cannot read, and {why}"
        ))
    })?;
    verdict(from, to, &own, &running)
}

/// This Mac's approvers: each program its subscriber runs or starts next ([`programs`]), with
/// the release its `--version` names, `own`'s where it is this program. Why they cannot be read
/// where [`programs`] says.
pub(super) fn approvers(paths: &Paths, own: &Own) -> Result<Vec<Running>, String> {
    #[cfg(test)]
    if let Some(played) = PLAYED.with(|played| played.borrow().clone()) {
        return Ok(played);
    }
    let state = service::state(&paths.home, Job::Subscriber);
    let found = programs(state, &|pid| program_of(pid).map(canonical), &|unit| {
        service::unit_program(unit).map(canonical)
    })?;
    Ok(found
        .into_iter()
        .map(|(pid, program)| {
            let version = if own.program.as_ref() == Some(&program) {
                Ok(own.version)
            } else {
                version_of(&program)
            };
            Running {
                pid,
                program,
                version,
            }
        })
        .collect())
}

/// [`judge`] over each of the subscriber's programs. A running program refused while the one its
/// job starts next passes is a subscriber about to restart (an update moved its program aside):
/// that refusal says to publish again once it has.
pub(super) fn verdict(from: &str, to: &str, own: &Own, running: &[Running]) -> crate::Result<()> {
    let next = running
        .iter()
        .find(|program| program.pid.is_none())
        .filter(|next| judge(from, to, own, next).is_ok());
    for program in running {
        if let Err(refused) = judge(from, to, own, program) {
            return Err(match (program.pid, next) {
                (Some(pid), Some(next)) => Error::new(format!(
                    "refusing to publish for now: this aterm would rewrite the vault's admin \
                     locker from {from} to {to}, and this Mac's subscriber (pid {pid}) still \
                     runs {}, which may not read it, though its job restarts it into {}, which \
                     does; publish again once it has restarted",
                    program.program.display(),
                    next.program.display()
                )),
                _ => refused,
            });
        }
    }
    Ok(())
}

/// The first release that reads the admin locker format `custody`, or `None` where every build
/// does: versions 2 and 3 came in 0.138.0, and a later one needs, for all this build knows, a
/// build as new as `own`.
pub(super) fn first_reader(custody: &str, own: Version) -> Option<Version> {
    match custody {
        "alab-vault-admin/1" => None,
        "alab-vault-admin/2" | "alab-vault-admin/3" => Some((0, 138, 0)),
        _ => Some(own),
    }
}

/// Why the approver in process `pid` cannot read an admin locker in format `custody`: its
/// program is an older release than [`first_reader`]. `None` where it can, it is this program,
/// or its program or release cannot be read.
pub(super) fn unreadable_by(pid: u32, custody: &str) -> Option<String> {
    let own = own().ok()?;
    let need = first_reader(custody, own.version)?;
    let program = canonical(program_of(pid)?);
    if own.program.as_ref() == Some(&program) {
        return None;
    }
    let version = version_of(&program).ok()?;
    (version < need).then(|| {
        format!(
            "its program {} is aterm {}, which cannot read the admin locker ({custody}, read \
             from aterm {} on)",
            program.display(),
            dotted(version),
            dotted(need)
        )
    })
}

fn canonical(path: PathBuf) -> PathBuf {
    std::fs::canonicalize(&path).unwrap_or(path)
}

/// The executable the kernel records for `pid`.
fn program_of(pid: u32) -> Option<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        let link = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
        // A program replaced on disk reads `<path> (deleted)`; the job restarts into the path.
        let text = link.to_str()?;
        Some(PathBuf::from(
            text.strip_suffix(" (deleted)").unwrap_or(text),
        ))
    }
    #[cfg(not(target_os = "linux"))]
    {
        aterm_uds::sudo::exe_path(i32::try_from(pid).ok()?)
    }
}

/// The release `program --version` names.
pub(super) fn version_of(program: &Path) -> Result<Version, String> {
    crate::service::program_release(program).map_err(|error| error.to_string())
}

#[cfg(test)]
#[path = "locker_skew_tests.rs"]
mod tests;
