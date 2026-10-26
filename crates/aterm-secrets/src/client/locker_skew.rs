// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A publish that moves the admin locker to a newer format first checks that the approver this
//! Mac runs can read it. An approver that cannot open the locker lets no machine join, and the
//! locker never returns to its old format, so the check comes before anything is published or
//! retired. It reads this Mac alone: an approver another admin Mac runs is not checked, so every
//! admin Mac's subscriber is updated before any admin publishes with a newer build.

use super::{Error, Paths};
use crate::service::{self, Job};
use std::path::{Path, PathBuf};
use std::time::Duration;

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
    let mut parts = core.split('.').map(|part| part.parse::<u64>().ok());
    let version = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(version)
}

/// The release a `--version` answer names on its first line, `aterm <version>`.
pub(super) fn version_answer(stdout: &str) -> Option<Version> {
    version(stdout.lines().next()?.strip_prefix("aterm ")?.trim())
}

fn dotted((major, minor, patch): Version) -> String {
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
/// differs. Refuses when the job's state is unknown, or an installed job's program cannot be
/// named at all: an unchecked approver is the one this check exists for.
pub(super) fn programs(
    state: crate::Result<service::State>,
    running_program: &dyn Fn(u32) -> Option<PathBuf>,
    unit_program: &dyn Fn(&Path) -> Option<PathBuf>,
) -> crate::Result<Vec<(Option<u32>, PathBuf)>> {
    let refuse = |why: String| {
        Error::new(format!(
            "refusing to publish: it would rewrite the vault's admin locker into a newer format, \
             which an older approver cannot read, and {why}"
        ))
    };
    let (pid, unit) = match state {
        Ok(service::State::Absent) => return Ok(Vec::new()),
        Ok(service::State::Installed { pid, unit }) => (pid, unit),
        Err(error) => {
            return Err(refuse(format!(
                "whether this Mac's subscriber runs is unknown ({error})"
            )));
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
            return Err(refuse(format!(
                "the program {} starts cannot be named",
                unit.display()
            )));
        }
        _ => {}
    }
    Ok(programs)
}

#[cfg(test)]
thread_local! {
    /// The approvers [`check`] reads on this thread, in place of the service's.
    pub(super) static PLAYED: std::cell::RefCell<Option<Vec<Running>>> =
        const { std::cell::RefCell::new(None) };
}

/// This build, as [`judge`] compares it.
fn own() -> crate::Result<Own> {
    Ok(Own {
        version: version(env!("CARGO_PKG_VERSION"))
            .ok_or_else(|| Error::new("this aterm's release number is unreadable"))?,
        release: aterm_types::version::installed().is_some_and(|stamp| stamp.release_build),
        program: std::env::current_exe().ok().map(canonical),
    })
}

/// [`judge`] for this Mac: refuse a publish that moves the admin locker from `from` to `to`
/// while this Mac's subscriber runs, or would start, an older program than this one.
pub(super) fn check(paths: &Paths, from: &str, to: &str) -> crate::Result<()> {
    let own = own()?;
    #[cfg(test)]
    if let Some(played) = PLAYED.with(|played| played.borrow().clone()) {
        return verdict(from, to, &own, &played);
    }
    let state = service::state(&paths.home, Job::Subscriber);
    let found = programs(state, &|pid| program_of(pid).map(canonical), &|unit| {
        service::unit_program(unit).map(canonical)
    })?;
    let running: Vec<Running> = found
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
        .collect();
    verdict(from, to, &own, &running)
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
fn version_of(program: &Path) -> Result<Version, String> {
    let mut command = aterm_uds::spawnfd::command(program);
    command.arg("--version");
    let out = aterm_uds::deadline::output_within(&mut command, Duration::from_secs(30))
        .map_err(|error| format!("{} --version: {error}", program.display()))?;
    if !out.status.success() {
        return Err(format!("{} --version: {}", program.display(), out.status));
    }
    version_answer(&String::from_utf8_lossy(&out.stdout))
        .ok_or_else(|| format!("{} --version named no aterm release", program.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_release_number_reads_without_its_build_metadata() {
        assert_eq!(version("0.140.0"), Some((0, 140, 0)));
        assert_eq!(version("0.141.0+g8a5d5c1510"), Some((0, 141, 0)));
        assert_eq!(version("1.2.3-rc1"), Some((1, 2, 3)));
        for bad in ["", "0.140", "0.140.0.1", "0.x.0", "v0.140.0"] {
            assert_eq!(version(bad), None, "{bad}");
        }
        let answer = "aterm 0.125.0\nby Andrew Yates · ALab\nbuild: 1\n";
        assert_eq!(version_answer(answer), Some((0, 125, 0)));
        assert_eq!(version_answer("atermx 0.125.0\n"), None);
        assert_eq!(version_answer(""), None);
    }

    const FROM: &str = "alab-vault-admin/1";
    const TO: &str = "alab-vault-admin/3";
    /// A format this build would introduce, which no release before it reads.
    const NEWEST: &str = "alab-vault-admin/9";
    const HERE: &str = "/Applications/aterm.app/Contents/MacOS/aterm";
    const SNAPSHOT: &str = "/Users//a/.local/share/aterm-secrets/aterm";

    fn own(release: bool) -> Own {
        Own {
            version: (0, 141, 0),
            release,
            program: Some(PathBuf::from(HERE)),
        }
    }

    fn running(pid: Option<u32>, program: &str, version: Result<Version, String>) -> Running {
        Running {
            pid,
            program: PathBuf::from(program),
            version,
        }
    }

    #[test]
    fn an_older_approver_refuses_the_rewrite_and_names_the_program_to_use() {
        let old = running(Some(8000), SNAPSHOT, Ok((0, 125, 0)));
        let refused = judge(FROM, TO, &own(true), &old)
            .expect_err("an older approver refuses")
            .to_string();
        for part in [
            "from alab-vault-admin/1 to alab-vault-admin/3",
            "this aterm (0.141.0)",
            "(pid 8000, /Users//a/.local/share/aterm-secrets/aterm, aterm 0.125.0)",
            "lets no machine join",
            "(/Users//a/.local/share/aterm-secrets/aterm secrets …)",
        ] {
            assert!(refused.contains(part), "{part}: {refused}");
        }
        let next = running(None, SNAPSHOT, Ok((0, 125, 0)));
        let refused = judge(FROM, TO, &own(true), &next)
            .expect_err("a job that would start an older program refuses")
            .to_string();
        assert!(
            refused.contains("(the program its job starts next, "),
            "{refused}"
        );
        let unknown = running(Some(8000), SNAPSHOT, Err("timed out".into()));
        let refused = judge(FROM, TO, &own(true), &unknown)
            .expect_err("an approver of unknown release refuses")
            .to_string();
        assert!(
            refused.contains("release is unknown: timed out"),
            "{refused}"
        );
    }

    #[test]
    fn this_program_or_a_release_that_has_this_one_lets_the_rewrite_through() {
        let same = running(Some(1), HERE, Err("never asked".into()));
        assert_eq!(judge(FROM, TO, &own(false), &same), Ok(()));
        for version in [(0, 142, 0), (1, 0, 0)] {
            let newer = running(Some(1), "/opt/aterm", Ok(version));
            assert_eq!(judge(FROM, TO, &own(false), &newer), Ok(()));
        }
        // A format a release before this build introduced passes any release that reads it.
        let reads = running(Some(1), "/opt/aterm", Ok((0, 140, 9)));
        assert_eq!(judge(FROM, TO, &own(false), &reads), Ok(()));
        assert_eq!(
            judge(
                FROM,
                TO,
                &own(false),
                &running(Some(1), "/o", Ok((0, 138, 0)))
            ),
            Ok(())
        );
        let before = running(Some(1), "/opt/aterm", Ok((0, 137, 9)));
        assert!(judge(FROM, TO, &own(true), &before).is_err());
        // A format this build introduces: the same number is as new only when this build is
        // that release, and an older one never is.
        let equal = running(Some(1), "/opt/aterm", Ok((0, 141, 0)));
        assert_eq!(judge(FROM, NEWEST, &own(true), &equal), Ok(()));
        let refused = judge(FROM, NEWEST, &own(false), &equal)
            .expect_err("a development build may write past its own number's release")
            .to_string();
        assert!(
            refused.contains("a release this development build may write past"),
            "{refused}"
        );
        assert!(judge(FROM, NEWEST, &own(true), &reads).is_err());
        let nowhere = Own {
            program: None,
            ..own(true)
        };
        assert!(judge(FROM, NEWEST, &nowhere, &reads).is_err());
    }

    #[test]
    fn a_subscriber_about_to_restart_into_a_newer_program_says_to_publish_again() {
        let old = running(
            Some(8000),
            "/Applications/aterm.app.old/Contents/MacOS/aterm",
            Ok((0, 137, 0)),
        );
        let next = running(None, HERE, Ok((0, 141, 0)));
        let refused = verdict(FROM, TO, &own(true), &[old.clone(), next])
            .expect_err("the running program is still older")
            .to_string();
        assert!(
            refused.starts_with("refusing to publish for now:"),
            "{refused}"
        );
        assert!(
            refused.contains("(pid 8000) still runs /Applications/aterm.app.old"),
            "{refused}"
        );
        assert!(
            refused.contains("publish again once it has restarted"),
            "{refused}"
        );
        // CONTROL: a job that starts the old program again gets the plain refusal.
        let stale = running(None, SNAPSHOT, Ok((0, 125, 0)));
        let refused = verdict(FROM, TO, &own(true), &[old, stale])
            .expect_err("both are older")
            .to_string();
        assert!(refused.starts_with("refusing to publish: "), "{refused}");
        assert!(
            refused.contains("publish with that program instead"),
            "{refused}"
        );
        let fine = running(Some(1), HERE, Ok((0, 141, 0)));
        assert_eq!(verdict(FROM, TO, &own(true), &[fine]), Ok(()));
        assert_eq!(verdict(FROM, TO, &own(true), &[]), Ok(()));
    }

    #[test]
    fn every_build_reads_version_one_and_a_format_this_build_does_not_know_needs_it() {
        assert_eq!(first_reader("alab-vault-admin/1", (0, 141, 0)), None);
        assert_eq!(
            first_reader("alab-vault-admin/2", (0, 141, 0)),
            Some((0, 138, 0))
        );
        assert_eq!(
            first_reader("alab-vault-admin/3", (0, 141, 0)),
            Some((0, 138, 0))
        );
        assert_eq!(
            first_reader("alab-vault-admin/9", (0, 141, 0)),
            Some((0, 141, 0))
        );
    }

    #[test]
    fn the_programs_checked_are_the_running_one_and_the_one_its_job_starts_next() {
        let unit = PathBuf::from("/u/systems.alab.aterm-secrets.plist");
        let installed = |pid| {
            Ok(service::State::Installed {
                pid,
                unit: unit.clone(),
            })
        };
        let at = |path: &str| Some(PathBuf::from(path));
        let snapshot = |_: &Path| at(SNAPSHOT);
        let nothing = |_: &Path| None;
        assert_eq!(
            programs(Ok(service::State::Absent), &|_| at(HERE), &snapshot),
            Ok(Vec::new())
        );
        assert_eq!(
            programs(installed(Some(8000)), &|_| at(SNAPSHOT), &snapshot),
            Ok(vec![(Some(8000), PathBuf::from(SNAPSHOT))])
        );
        assert_eq!(
            programs(installed(Some(8000)), &|_| at(HERE), &snapshot),
            Ok(vec![
                (Some(8000), PathBuf::from(HERE)),
                (None, PathBuf::from(SNAPSHOT))
            ])
        );
        // Between two runs the unit names what launchd starts next.
        assert_eq!(
            programs(installed(None), &|_| at(HERE), &snapshot),
            Ok(vec![(None, PathBuf::from(SNAPSHOT))])
        );
        assert_eq!(
            programs(installed(Some(8000)), &|_| None, &snapshot),
            Ok(vec![(None, PathBuf::from(SNAPSHOT))])
        );
        assert_eq!(
            programs(installed(Some(8000)), &|_| at(SNAPSHOT), &nothing),
            Ok(vec![(Some(8000), PathBuf::from(SNAPSHOT))])
        );
        // Nothing nameable, or an unreadable state, refuses rather than passes.
        let refused = programs(installed(None), &|_| None, &nothing)
            .expect_err("an installed job of no nameable program refuses")
            .to_string();
        assert!(
            refused.contains("systems.alab.aterm-secrets.plist starts cannot be named"),
            "{refused}"
        );
        let refused = programs(Err(Error::new("launchctl hung")), &|_| at(HERE), &snapshot)
            .expect_err("an unknown state refuses")
            .to_string();
        assert!(
            refused.contains("runs is unknown (launchctl hung)"),
            "{refused}"
        );
    }
}
