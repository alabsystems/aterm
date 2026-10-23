// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A durable obligation to activate desktop preferences after their write.

use crate::model::User;
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

/// The marker an older aterm (0.114.0 and before) wrote before every widgets write, Notification Center's widget list with the Window Manager's keys, and cleared once the Dock and Notification Center were relaunched.
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

fn parents(sys: &dyn System, user: &User, marker: &Path) -> Result<(), String> {
    let home = Path::new(&user.home);
    let mut current = home.to_path_buf();
    let relative = marker
        .parent()
        .ok_or("activation marker has no directory")?
        .strip_prefix(home)
        .map_err(|_| "activation marker is outside the account home")?;
    for component in std::iter::once(None).chain(relative.components().map(Some)) {
        if let Some(component) = component {
            current.push(component);
        }
        match sys.stat(&current) {
            Ok(stat)
                if stat.dir && !stat.symlink && stat.uid == user.uid && stat.mode & 0o022 == 0 => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            _ => {
                return Err(
                    "activation state directory must belong to this user and contain no links"
                        .into(),
                );
            }
        }
    }
    Ok(())
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

/// Persist activation work before the first preference write.
/// # Errors
/// The private marker cannot be safely written.
pub fn begin(sys: &dyn System, user: &User, family: &str) -> Result<(), String> {
    let marker = path(user, family)?;
    parents(sys, user, &marker)?;
    pending(sys, user, family)?;
    sys.create_dir_all(
        marker
            .parent()
            .ok_or("activation marker has no directory")?,
        0o700,
    )
    .map_err(|_| "cannot create the desktop activation state directory")?;
    parents(sys, user, &marker)?;
    sys.write_user_file(user, &marker, RECORD, 0o600)
        .map_err(|_| "cannot persist pending desktop activation".to_owned())?;
    for directory in marker.parent().into_iter().flat_map(Path::ancestors) {
        sys.durable_dir(directory)
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

/// How long [`relaunch_settled`] waits on what it relaunched, looking once a second: the
/// one-minute hang detector, never a latency budget (a consumer that settles is done at
/// once).
const PATIENCE: Duration = Duration::from_secs(60);
const POLLS: u32 = 60;

/// Where [`relaunch_settled`] is with one consumer.
enum State<W> {
    /// Its answer.
    Done(Result<(), String>),
    /// Relaunched and not yet up: its old processes, what the last look saw under which
    /// new ones, and what it waits for.
    Waiting {
        old: Vec<u32>,
        last: Option<(Vec<u32>, W)>,
        why: String,
    },
}

/// Relaunch each of `processes` that runs, once, in turn, and return once each is up again and past its start or the minute is out: for each, `Ok`, or why not.
pub fn relaunch_settled<W: PartialEq>(
    sys: &dyn System,
    user: &User,
    processes: &[&str],
    look: impl Fn(&str) -> Result<W, String>,
) -> Vec<Result<(), String>> {
    let pending = |why: String| format!("{why}; activation remains pending");
    let mut states: Vec<State<W>> = processes
        .iter()
        .map(|&process| match census(sys, user, process) {
            Err(why) => State::Done(Err(pending(why))),
            Ok(old) if old.is_empty() => State::Done(Ok(())),
            Ok(old) => match crate::items::defaults::relaunch(sys, user, process) {
                Err(why) => State::Done(Err(pending(why))),
                Ok(()) => State::Waiting {
                    old,
                    last: None,
                    why: format!("{process} has not been looked at"),
                },
            },
        })
        .collect();
    let deadline = Instant::now() + PATIENCE;
    for poll in 0..POLLS {
        for (state, &process) in states.iter_mut().zip(processes) {
            let State::Waiting { old, last, why } = state else {
                continue;
            };
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
        if !waiting || poll + 1 == POLLS || Instant::now() >= deadline {
            break;
        }
        let sleep = Cmd::new("/bin/sleep").arg("1").as_user(user);
        if !sys.run(&sleep).is_ok_and(|ran| ran.success()) {
            for (state, process) in states.iter_mut().zip(processes) {
                if matches!(state, State::Waiting { .. }) {
                    *state = State::Done(Err(pending(format!(
                        "the wait for the relaunched {process} was interrupted"
                    ))));
                }
            }
            break;
        }
    }
    states
        .into_iter()
        .map(|state| match state {
            State::Done(answer) => answer,
            State::Waiting { why, .. } => {
                Err(pending(format!("{why} a minute after its relaunch")))
            }
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
                let pid = line
                    .trim()
                    .parse::<u32>()
                    .ok()
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
mod tests {
    use super::*;
    use crate::items::defaults::tests::user;
    use crate::port::{Fake, Ran, Stat};

    #[test]
    fn activation_obligation_survives_a_failed_or_interrupted_write_until_finished() {
        let who = user("alice", 501);
        let mut sys = Fake::root();
        sys.euid = who.uid;
        assert!(!pending(&sys, &who, "dock").unwrap());
        begin(&sys, &who, "dock").unwrap();
        assert!(pending(&sys, &who, "dock").unwrap());
        assert!(!pending(&sys, &who, "widgets").unwrap());
        begin(&sys, &who, "dock").unwrap();
        finish(&sys, &who, "dock").unwrap();
        assert!(!pending(&sys, &who, "dock").unwrap());
    }

    #[test]
    fn linked_foreign_or_malformed_state_never_turns_into_success_or_an_overwrite() {
        let who = user("alice", 501);
        for (uid, symlink, mode) in [(0, false, 0o600), (501, true, 0o600), (501, false, 0o644)] {
            let sys = Fake::root();
            let marker = path(&who, "dock").unwrap();
            sys.stat_of(
                marker.to_str().unwrap(),
                Stat {
                    uid,
                    gid: 20,
                    mode,
                    dir: false,
                    symlink,
                    len: RECORD.len() as u64,
                },
            );
            assert!(pending(&sys, &who, "dock").is_err());
            assert!(begin(&sys, &who, "dock").is_err());
            assert!(
                sys.calls()
                    .iter()
                    .all(|call| !call.contains(" write ") && !call.starts_with("mkdir "))
            );
        }
        let mut sys = Fake::root();
        sys.euid = who.uid;
        begin(&sys, &who, "dock").unwrap();
        sys.write_user_file(&who, &path(&who, "dock").unwrap(), b"unknown marker", 0o600)
            .unwrap();
        assert!(pending(&sys, &who, "dock").is_err());
        assert!(finish(&sys, &who, "dock").is_err());
    }

    #[test]
    fn linked_foreign_or_shared_parent_cannot_receive_an_activation_marker() {
        let who = user("alice", 501);
        for (uid, symlink, mode) in [(0, false, 0o700), (501, true, 0o700), (501, false, 0o777)] {
            let sys = Fake::root();
            sys.stat_of(
                "/Users//alice/Library/Application Support/aterm",
                Stat {
                    uid,
                    gid: 20,
                    mode,
                    dir: true,
                    symlink,
                    len: 0,
                },
            );
            assert!(begin(&sys, &who, "dock").is_err());
            assert!(sys.calls().is_empty());
        }
    }

    #[test]
    fn a_successful_kill_request_cannot_hide_a_surviving_old_consumer_or_interrupted_wait() {
        let who = user("alice", 501);
        for interrupted in [false, true] {
            let mut sys = Fake::root();
            sys.euid = who.uid;
            begin(&sys, &who, "dock").unwrap();
            sys.on("alice$ /usr/bin/pgrep -u 501 -x Dock", Ran::ok("123\n"));
            sys.alive(123);
            sys.on("/bin/launchctl print gui/501", Ran::ok(""));
            sys.on("alice@gui$ /usr/bin/killall -u alice Dock", Ran::ok(""));
            sys.on(
                "alice$ /bin/sleep 1",
                if interrupted {
                    Ran {
                        status: Some(130),
                        stdout: Vec::new(),
                        stderr: Vec::new(),
                    }
                } else {
                    Ran::ok("")
                },
            );
            let answers = relaunch_settled(&sys, &who, &["Dock"], |_| Ok::<_, String>(()));
            assert!(answers[0].is_err(), "{answers:?}");
            assert!(pending(&sys, &who, "dock").unwrap());
            assert_eq!(
                sys.calls()
                    .iter()
                    .filter(|call| call.contains("killall"))
                    .count(),
                1
            );
            assert_eq!(
                sys.calls()
                    .iter()
                    .filter(|call| call.contains("/bin/sleep"))
                    .count(),
                if interrupted { 1 } else { 59 }
            );
        }
    }

    #[test]
    fn missing_consumer_needs_no_launch_and_unproven_census_cannot_clear_pending_work() {
        let who = user("alice", 501);
        for output in [None, Some("not-a-pid"), Some(""), Some("123\n123\n")] {
            let mut sys = Fake::root();
            sys.euid = who.uid;
            begin(&sys, &who, "widgets").unwrap();
            let census = "alice$ /usr/bin/pgrep -u 501 -x NotificationCenter";
            sys.on(
                census,
                output.map_or_else(
                    || Ran {
                        status: Some(1),
                        stdout: Vec::new(),
                        stderr: Vec::new(),
                    },
                    Ran::ok,
                ),
            );
            let mut answers =
                relaunch_settled(&sys, &who, &["NotificationCenter"], |_| Ok::<_, String>(()));
            let result = answers.remove(0);
            if output.is_none() {
                result.unwrap();
                finish(&sys, &who, "widgets").unwrap();
                assert!(!pending(&sys, &who, "widgets").unwrap());
            } else {
                assert!(result.is_err());
                assert!(pending(&sys, &who, "widgets").unwrap());
            }
            assert!(
                sys.calls()
                    .iter()
                    .all(|call| !call.contains("launchctl") && !call.contains("killall"))
            );
        }
        let mut sys = Fake::root();
        sys.euid = who.uid;
        begin(&sys, &who, "dock").unwrap();
        let census = "alice$ /usr/bin/pgrep -u 501 -x Dock";
        sys.on(census, Ran::ok("123\n"));
        sys.refuse_complete_stdout(census, "incomplete capture");
        let answers = relaunch_settled(&sys, &who, &["Dock"], |_| Ok::<_, String>(()));
        assert!(answers[0].is_err(), "{answers:?}");
        assert!(pending(&sys, &who, "dock").unwrap());
        assert!(sys.calls().iter().all(|call| !call.contains("killall")));
    }

    /// A relaunched consumer counts as up only once its old process exited, a new one
    /// runs, and what it reads stopped changing across a poll under that same process: a
    /// Dock still starting can write what it read back.
    #[test]
    fn a_relaunched_consumer_is_up_once_it_runs_again_and_what_it_reads_is_still() {
        let who = user("alice", 501);
        let census = "alice$ /usr/bin/pgrep -u 501 -x Dock";
        let script = |sys: &Fake, after: Ran| {
            sys.on(census, Ran::ok("123\n"));
            sys.on(census, after);
            sys.on("/bin/launchctl print gui/501", Ran::ok(""));
            sys.on("alice@gui$ /usr/bin/killall -u alice Dock", Ran::ok(""));
            sys.on("alice$ /bin/sleep 1", Ran::ok(""));
        };
        let sys = Fake::root();
        script(&sys, Ran::ok("124\n"));
        let reads = std::sync::Mutex::new(vec![1, 2, 2, 2]);
        let answers = relaunch_settled(&sys, &who, &["Dock"], |_| {
            Ok::<_, String>(reads.lock().unwrap().remove(0))
        });
        assert_eq!(answers, [Ok(())]);
        assert_eq!(sleeps(&sys), 2);
        let sys = Fake::root();
        script(
            &sys,
            Ran {
                status: Some(1),
                stdout: Vec::new(),
                stderr: Vec::new(),
            },
        );
        let why = relaunch_settled(&sys, &who, &["Dock"], |_| Ok::<_, String>(()))
            .remove(0)
            .unwrap_err();
        assert!(why.contains("has not started again"), "{why}");
        assert_eq!(sleeps(&sys), 59);
        let sys = Fake::root();
        script(&sys, Ran::ok("124\n"));
        let count = std::sync::atomic::AtomicU32::new(0);
        let why = relaunch_settled(&sys, &who, &["Dock"], |_| {
            Ok::<_, String>(count.fetch_add(1, std::sync::atomic::Ordering::SeqCst))
        })
        .remove(0)
        .unwrap_err();
        assert!(why.contains("Dock still rewrites what it reads"), "{why}");
        assert_eq!(sleeps(&sys), 59);
        assert_eq!(kills(&sys), 1);
        let sys = Fake::root();
        script(&sys, Ran::ok("124\n"));
        let looks = std::sync::Mutex::new(vec![Err("tiles"), Err("tiles"), Ok(()), Ok(())]);
        let answers = relaunch_settled(&sys, &who, &["Dock"], |_| {
            looks.lock().unwrap().remove(0).map_err(str::to_owned)
        });
        assert_eq!(answers, [Ok(())]);
        assert_eq!(sleeps(&sys), 3);
    }

    /// Two equal reads count only under the same new processes: a consumer that started
    /// again between them (a crash right after its start) is looked at again.
    #[test]
    fn two_equal_reads_settle_a_consumer_only_under_the_same_new_process() {
        let who = user("alice", 501);
        let census = "alice$ /usr/bin/pgrep -u 501 -x Dock";
        let sys = Fake::root();
        sys.on(census, Ran::ok("123\n"));
        sys.on(census, Ran::ok("124\n"));
        sys.on(census, Ran::ok("125\n"));
        sys.on("/bin/launchctl print gui/501", Ran::ok(""));
        sys.on("alice@gui$ /usr/bin/killall -u alice Dock", Ran::ok(""));
        sys.on("alice$ /bin/sleep 1", Ran::ok(""));
        let answers = relaunch_settled(&sys, &who, &["Dock"], |_| Ok::<_, String>("pins"));
        assert_eq!(answers, [Ok(())]);
        assert_eq!(sleeps(&sys), 2);
    }

    /// Each consumer gets its own answer, and every one that was relaunched is waited on
    /// whatever another does: a census that fails or a relaunch that cannot be made (it
    /// signalled nothing, so nothing of it is awaited) does not cut short the wait for one
    /// already killed, which a rollback would otherwise relaunch while it starts.
    #[test]
    fn every_relaunched_consumer_is_waited_on_whatever_another_does() {
        let who = user("alice", 501);
        let sys = Fake::root();
        sys.on("alice$ /usr/bin/pgrep -u 501 -x Dock", Ran::ok("123\n"));
        sys.on("alice$ /usr/bin/pgrep -u 501 -x Dock", Ran::ok("124\n"));
        sys.on("alice$ /usr/bin/pgrep -u 501 -x Finder", Ran::ok("200\n"));
        sys.refuse_complete_stdout("alice$ /usr/bin/pgrep -u 501 -x Finder", "incomplete");
        sys.on(
            "alice$ /usr/bin/pgrep -u 501 -x NotificationCenter",
            Ran::ok("300\n"),
        );
        sys.on("/bin/launchctl print gui/501", Ran::ok(""));
        sys.on("alice@gui$ /usr/bin/killall -u alice Dock", Ran::ok(""));
        sys.on(
            "alice@gui$ /usr/bin/killall -u alice NotificationCenter",
            Ran::exit(2, "injected"),
        );
        sys.on("alice$ /bin/sleep 1", Ran::ok(""));
        let looked = std::sync::Mutex::new(Vec::new());
        let answers = relaunch_settled(&sys, &who, &RELAUNCHED, |process| {
            looked.lock().unwrap().push(process.to_owned());
            Ok::<_, String>(())
        });
        assert_eq!(answers[0], Ok(()));
        let finder = answers[1].clone().unwrap_err();
        assert!(
            finder.starts_with("cannot read every Finder process"),
            "{finder}"
        );
        let center = answers[2].clone().unwrap_err();
        assert!(
            center.contains("killall -u alice NotificationCenter"),
            "{center}"
        );
        assert_eq!(*looked.lock().unwrap(), ["Dock", "Dock"]);
        assert_eq!(sleeps(&sys), 1);
    }

    fn sleeps(sys: &Fake) -> usize {
        sys.calls()
            .iter()
            .filter(|call| call.contains("/bin/sleep"))
            .count()
    }

    fn kills(sys: &Fake) -> usize {
        sys.calls()
            .iter()
            .filter(|call| call.contains("killall"))
            .count()
    }
}
