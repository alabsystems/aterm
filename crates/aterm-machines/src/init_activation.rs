// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A durable obligation to activate desktop preferences after their write.

use crate::items::defaults::{Session, Signalled, Target};
use crate::model::{PValue, User};
use crate::plist::Entries;
use crate::port::{Cmd, System};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const RECORD: &[u8] = b"aterm init desktop activation pending v1\n";
const DIRECTORY: &str = "Library/Application Support/aterm/init-activation";

/// The marker of a write to another app's sandbox container ([`crate::items::defaults::CONTAINED`]: Notification Center's widget list), kept apart from its family's: only a pass that holds Full Disk Access reaches that container, so only such a pass begins, discharges or reports it, and a pass without the grant, which relaunches the family's other readers, cannot clear what Notification Center still owes.
pub const CONTAINED_MARKER: &str = "notificationcenter";

/// The marker of a write to the Window Manager's widget keys (on the desktop and in Stage Manager), whose relaunch is the Dock's.
pub const WIDGETS_MARKER: &str = "desktopwidgets";

/// The marker an older aterm (0.116.0 and before) wrote before every widgets write, Notification Center's widget list with the Window Manager's keys, and cleared once the Dock and Notification Center were relaunched.
pub const LEGACY_WIDGETS_MARKER: &str = "widgets";

fn path(user: &User, family: &str) -> Result<PathBuf, String> {
    if !Path::new(&user.home).is_absolute()
        || !matches!(
            family,
            "dock" | "desktop" | WIDGETS_MARKER | LEGACY_WIDGETS_MARKER | CONTAINED_MARKER
        )
    {
        return Err("invalid local initialization activation scope".into());
    }
    Ok(Path::new(&user.home)
        .join(DIRECTORY)
        .join(format!("{family}.pending")))
}

/// Check every directory from the home down to `marker`'s: each that exists belongs to the
/// user, is no link and is writable by no one else. Return those that do not exist yet, which
/// [`begin`] creates, from the home down.
fn parents(sys: &dyn System, user: &User, marker: &Path) -> Result<Vec<PathBuf>, String> {
    let home = Path::new(&user.home);
    let mut current = home.to_path_buf();
    let relative = marker
        .parent()
        .ok_or("activation marker has no directory")?
        .strip_prefix(home)
        .map_err(|_| "activation marker is outside the account home")?;
    let mut missing = Vec::new();
    for component in std::iter::once(None).chain(relative.components().map(Some)) {
        if let Some(component) = component {
            current.push(component);
        }
        match sys.stat(&current) {
            Ok(stat)
                if stat.dir && !stat.symlink && stat.uid == user.uid && stat.mode & 0o022 == 0 => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => missing.push(current.clone()),
            _ => {
                return Err(
                    "activation state directory must belong to this user and contain no links"
                        .into(),
                );
            }
        }
    }
    Ok(missing)
}

/// Whether a previous write still needs its consumer relaunched.
/// # Errors
/// Unreadable, foreign, linked or malformed activation state.
pub fn pending(sys: &dyn System, user: &User, family: &str) -> Result<bool, String> {
    let marker = path(user, family)?;
    match sys.stat(&marker) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Ok(stat)
            if !stat.dir
                && !stat.symlink
                && stat.uid == user.uid
                && stat.mode & 0o077 == 0
                && stat.len == RECORD.len() as u64 => {}
        _ => return Err("cannot trust this user's desktop activation marker".into()),
    }
    parents(sys, user, &marker)?;
    let bytes = sys
        .read_file(&marker)
        .map_err(|_| "cannot read the desktop activation marker")?;
    if bytes != RECORD {
        return Err("unrecognized desktop activation marker".into());
    }
    Ok(true)
}

/// Persist activation work before the first preference write: the marker, then a barrier
/// ([`System::ordered_dir`]) on each directory whose entries this call changed (the marker's
/// own, and the parent of each directory it created), so the marker reaches the disk before
/// any write after it. A marker already there was left by an earlier call whose barriers may
/// not all have run (one failed, or the process died), so the whole chain is ordered again.
/// # Errors
/// The private marker cannot be safely written.
pub fn begin(sys: &dyn System, user: &User, family: &str) -> Result<(), String> {
    let marker = path(user, family)?;
    let created = parents(sys, user, &marker)?;
    let earlier = pending(sys, user, family)?;
    let directory = marker
        .parent()
        .ok_or("activation marker has no directory")?;
    sys.create_dir_all(directory, 0o700)
        .map_err(|_| "cannot create the desktop activation state directory")?;
    parents(sys, user, &marker)?;
    sys.write_user_file(user, &marker, RECORD, 0o600)
        .map_err(|_| "cannot persist pending desktop activation".to_owned())?;
    for changed in directory.ancestors().filter(|changed| {
        earlier
            || *changed == directory
            || created.iter().any(|made| made.parent() == Some(*changed))
    }) {
        sys.ordered_dir(changed)
            .map_err(|_| "cannot persist the desktop activation directory entries".to_owned())?;
    }
    Ok(())
}

/// Clear the obligation only after activation has succeeded.
/// # Errors
/// The marker is malformed or cannot be removed; rerunning safely repeats activation.
pub fn finish(sys: &dyn System, user: &User, family: &str) -> Result<(), String> {
    if pending(sys, user, family)? {
        sys.remove(&path(user, family)?)
            .map_err(|_| "desktop activated, but its pending marker could not be cleared")?;
    }
    Ok(())
}

/// The desktop processes a pass relaunches, in the order it relaunches them.
pub const RELAUNCHED: [&str; 3] = ["Dock", "Finder", "NotificationCenter"];

/// The processes that read `domain`'s preferences and relaunch to read a write to it, in [`RELAUNCHED`] order: the one table the init pass (each family's readers are those of the domains its keys are in) and the doctor's relaunch after its own writes both read.
#[must_use]
pub fn readers(domain: &str) -> &'static [&'static str] {
    match domain {
        "com.apple.dock" | "com.apple.WindowManager" => &["Dock"],
        "com.apple.finder" => &["Finder"],
        "com.apple.notificationcenterui" => &["NotificationCenter"],
        _ => &[],
    }
}

/// How long [`relaunch_settled`] waits on what it relaunched: the one-minute hang detector,
/// never a latency budget (a consumer that settles is done at once).
const PATIENCE: Duration = Duration::from_secs(60);

/// How often [`relaunch_settled`] looks whether an old process exited: in this process
/// ([`System::pid_alive`], [`System::sleep`]), with no child.
const TICK: Duration = Duration::from_millis(20);

/// The ticks between two looks at a consumer whose old processes are gone (its census and
/// what it reads, which spawn): two equal looks this far apart (240 ms) under the same new
/// process are what counts as still.
const LOOK_EVERY: u32 = 12;

/// The ticks [`PATIENCE`] holds.
const TICKS: u32 = 3000;

/// Where [`relaunch_settled`] is with one consumer.
enum State<W> {
    /// Its answer.
    Done(Result<(), String>),
    /// Relaunched and not yet up: its old processes, what the last look saw under which
    /// new ones, what it waits for, and the tick of its next look.
    Waiting {
        old: Vec<u32>,
        last: Option<(Vec<u32>, W)>,
        why: String,
        next: u32,
    },
}

/// Relaunch each of `processes` that runs, once, in turn, in `session` ([`Session`]), and return once each is up again and past its start or the minute is out: for each, `Ok`, or why not, which says the activation remains pending (its durable marker, [`begin`], is cleared only after this).
pub fn relaunch_settled<W: PartialEq>(
    sys: &dyn System,
    user: &User,
    session: Session,
    processes: &[&str],
    look: impl Fn(&str) -> Result<W, String>,
) -> Vec<Result<(), String>> {
    relaunched(sys, user, session, processes, look)
        .into_iter()
        .map(|answer| answer.map_err(|why| format!("{why}; activation remains pending")))
        .collect()
}

/// Relaunch `user`'s `process` once and return once it is up again and what it reads of
/// `keys` is still ([`reading`]), for a caller that keeps no durable obligation: a fleet
/// row's relaunch ([`crate::items::defaults::Relaunch`]) and the doctor's own after it puts
/// a value back. `Ok` too when the process does not run or `user` has no login session:
/// nothing then holds what it read before.
///
/// # Errors
/// Why the process was not relaunched, or is not yet up and still a minute after.
pub fn relaunch_reading(
    sys: &dyn System,
    user: &User,
    session: Session,
    process: &str,
    keys: &[Target],
) -> Result<(), String> {
    relaunched(sys, user, session, &[process], |_| reading(sys, keys))
        .pop()
        .unwrap_or(Ok(()))
}

/// The Dock's keys whose tiles it gives a GUID as it loads them (aterm's own pins carry none).
const TILE_KEYS: [&str; 2] = ["persistent-apps", "persistent-others"];

/// What the process that reads `keys` holds of them now, each of their domains read once,
/// a read's failure kept as what it read; `Err` while it is the Dock and has not yet loaded
/// the tiles it was handed: a Dock gives each tile it loads a GUID, and a write of aterm's
/// pins carries none.
///
/// # Errors
/// The Dock has not yet loaded a tile of `keys`.
pub fn reading(sys: &dyn System, keys: &[Target]) -> Result<Vec<Result<Entries, String>>, String> {
    let mut domains: Vec<&Target> = Vec::new();
    for key in keys {
        if !domains
            .iter()
            .any(|seen| seen.domain == key.domain && seen.scope == key.scope)
        {
            domains.push(key);
        }
    }
    let mut seen = Vec::with_capacity(domains.len());
    for domain in domains {
        let entries = domain.entries(sys).map(|entries| -> Entries {
            entries
                .into_iter()
                .filter(|(name, _)| {
                    keys.iter().any(|key| {
                        key.domain == domain.domain && key.scope == domain.scope && key.key == *name
                    })
                })
                .collect()
        });
        if domain.domain == "com.apple.dock"
            && let Ok(entries) = &entries
            && entries.iter().any(|(key, value)| {
                TILE_KEYS.contains(&key.as_str())
                    && value.as_ref().is_ok_and(|tiles| !loaded(tiles))
            })
        {
            return Err("the Dock has not yet loaded the tiles it was given".into());
        }
        seen.push(entries);
    }
    Ok(seen)
}

/// Whether every tile of `tiles` carries the GUID a Dock gives a tile it loads.
fn loaded(tiles: &PValue) -> bool {
    let PValue::Array(tiles) = tiles else {
        return true;
    };
    tiles.iter().all(
        |tile| matches!(tile, PValue::Dict(fields) if fields.iter().any(|(key, _)| key == "GUID")),
    )
}

/// [`relaunch_settled`], each failure saying only why.
fn relaunched<W: PartialEq>(
    sys: &dyn System,
    user: &User,
    session: Session,
    processes: &[&str],
    look: impl Fn(&str) -> Result<W, String>,
) -> Vec<Result<(), String>> {
    let mut states: Vec<State<W>> = processes
        .iter()
        .map(|&process| match census(sys, user, process) {
            Err(why) => State::Done(Err(why)),
            Ok(old) if old.is_empty() => State::Done(Ok(())),
            Ok(old) => match crate::items::defaults::relaunch(sys, user, session, process) {
                Err(why) => State::Done(Err(why)),
                // No login session: nothing was signalled and nothing starts again.
                Ok(Signalled::NoSession) => State::Done(Ok(())),
                // A `killall` that signalled not every one (a refused kill, or a census
                // the process outran) proves nothing: the old processes are waited on.
                Ok(Signalled::Every | Signalled::NotEvery) => State::Waiting {
                    old,
                    last: None,
                    why: format!("{process} has not been looked at"),
                    next: 0,
                },
            },
        })
        .collect();
    let deadline = Instant::now() + PATIENCE;
    for tick in 0..TICKS {
        for (state, &process) in states.iter_mut().zip(processes) {
            let State::Waiting {
                old,
                last,
                why,
                next,
            } = state
            else {
                continue;
            };
            // An old process still running is looked at in this process, every tick.
            if old.iter().any(|pid| sys.pid_alive(*pid)) {
                *last = None;
                *why = format!("the previous {process} process has not exited");
                continue;
            }
            if tick < *next {
                continue;
            }
            *next = tick + LOOK_EVERY;
            let now =
                up(sys, user, process, old).and_then(|pids| look(process).map(|seen| (pids, seen)));
            let settled = match now {
                Err(not_yet) => {
                    *last = None;
                    *why = not_yet;
                    false
                }
                Ok(now) if last.as_ref() == Some(&now) => true,
                Ok(now) => {
                    *last = Some(now);
                    *why = format!("{process} still rewrites what it reads");
                    false
                }
            };
            if settled {
                *state = State::Done(Ok(()));
            }
        }
        let waiting = states
            .iter()
            .any(|state| matches!(state, State::Waiting { .. }));
        if !waiting || tick + 1 == TICKS || Instant::now() >= deadline {
            break;
        }
        sys.sleep(TICK);
        if let Some(signal) = sys.stopped() {
            for (state, process) in states.iter_mut().zip(processes) {
                if matches!(state, State::Waiting { .. }) {
                    *state = State::Done(Err(format!(
                        "the wait for the relaunched {process} was interrupted (signal {signal})"
                    )));
                }
            }
            break;
        }
    }
    states
        .into_iter()
        .map(|state| match state {
            State::Done(answer) => answer,
            State::Waiting { why, .. } => Err(format!("{why} a minute after its relaunch")),
        })
        .collect()
}

/// The new processes of a relaunched `process`, once every `old` one has exited; else
/// what is still awaited.
fn up(sys: &dyn System, user: &User, process: &str, old: &[u32]) -> Result<Vec<u32>, String> {
    if old.iter().any(|pid| sys.pid_alive(*pid)) {
        return Err(format!("the previous {process} process has not exited"));
    }
    let new: Vec<u32> = census(sys, user, process)?
        .into_iter()
        .filter(|pid| !old.contains(pid))
        .collect();
    if new.is_empty() {
        return Err(format!("{process} has not started again"));
    }
    Ok(new)
}

/// Every running `process` of `user`, from a complete `pgrep` record.
fn census(sys: &dyn System, user: &User, process: &str) -> Result<Vec<u32>, String> {
    let census = Cmd::new("/usr/bin/pgrep")
        .args(["-u", &user.uid.to_string(), "-x", process])
        .as_user(user);
    let ran = sys
        .run_complete_stdout(&census)
        .map_err(|_| format!("cannot read every {process} process"))?;
    let text = ran.out();
    match ran.status {
        Some(1) if text.trim().is_empty() => Ok(Vec::new()),
        Some(0) => {
            let mut pids = Vec::new();
            for line in text.lines() {
                let pid = crate::parse_uint::<u32>(line.trim())
                    .filter(|pid| *pid > 1 && *pid <= i32::MAX as u32)
                    .ok_or_else(|| format!("unrecognized {process} process list"))?;
                if pids.contains(&pid) {
                    return Err(format!("duplicate {process} process"));
                }
                pids.push(pid);
            }
            if pids.is_empty() {
                return Err(format!("empty successful {process} process list"));
            }
            Ok(pids)
        }
        _ => Err(format!("cannot list {process} processes")),
    }
}

#[cfg(test)]
#[path = "init_activation_tests.rs"]
mod tests;
