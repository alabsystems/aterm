// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A durable obligation to activate desktop preferences after their write.
//!
//! A process interrupted between writing preferences and relaunching their consumer
//! must not forget that work on the next invocation. The marker precedes every write
//! and survives a failed relaunch. A failed marker removal merely repeats activation.

use crate::model::User;
use crate::port::{Cmd, System};
use std::io;
use std::path::{Path, PathBuf};

const RECORD: &[u8] = b"aterm init desktop activation pending v1\n";
const DIRECTORY: &str = "Library/Application Support/aterm/init-activation";

fn path(user: &User, family: &str) -> Result<PathBuf, String> {
    if !Path::new(&user.home).is_absolute() || !matches!(family, "dock" | "widgets" | "desktop") {
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
///
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
///
/// # Errors
/// The private marker cannot be safely written.
pub fn begin(sys: &dyn System, user: &User, family: &str) -> Result<(), String> {
    let marker = path(user, family)?;
    parents(sys, user, &marker)?;
    // A malformed existing obligation must not be silently replaced.
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
    // Publish every ancestor entry as well as the marker's rename. Do not stop at
    // an existing directory: an interrupted earlier attempt may have created it
    // without completing its parent flush. HOME itself need not have existed yet.
    for directory in marker.parent().into_iter().flat_map(Path::ancestors) {
        sys.durable_dir(directory)
            .map_err(|_| "cannot persist the desktop activation directory entries".to_owned())?;
    }
    Ok(())
}

/// Clear the obligation only after activation has succeeded.
///
/// # Errors
/// The marker is malformed or cannot be removed; rerunning safely repeats activation.
pub fn finish(sys: &dyn System, user: &User, family: &str) -> Result<(), String> {
    if pending(sys, user, family)? {
        sys.remove(&path(user, family)?)
            .map_err(|_| "desktop activated, but its pending marker could not be cleared")?;
    }
    Ok(())
}

/// Relaunch a changed desktop consumer once and establish that every old process
/// actually exited. Stored preferences alone cannot discharge this obligation.
/// No running consumer means its next start will read the already-written values.
///
/// # Errors
/// An incomplete process census, failed relaunch, interrupted wait or surviving
/// old consumer. The caller must retain the marker on every error.
pub fn activate(sys: &dyn System, user: &User, family: &str) -> Result<(), String> {
    let processes: &[&str] = match family {
        "dock" => &["Dock"],
        "widgets" => &["NotificationCenter"],
        "desktop" => &["Dock", "Finder"],
        _ => return Err("invalid desktop activation family".into()),
    };
    for process in processes {
        activate_process(sys, user, process)?;
    }
    Ok(())
}

fn activate_process(sys: &dyn System, user: &User, process: &str) -> Result<(), String> {
    let census = Cmd::new("/usr/bin/pgrep")
        .args(["-u", &user.uid.to_string(), "-x", process])
        .as_user(user);
    let ran = sys
        .run_complete_stdout(&census)
        .map_err(|_| format!("cannot read every {process} process; activation remains pending"))?;
    let text = ran.out();
    let pids = match ran.status {
        Some(1) if text.trim().is_empty() => Vec::new(),
        Some(0) => {
            let mut pids = Vec::new();
            for line in text.lines() {
                let pid = line
                    .trim()
                    .parse::<u32>()
                    .ok()
                    .filter(|pid| *pid > 1 && *pid <= i32::MAX as u32)
                    .ok_or_else(|| {
                        format!("unrecognized {process} process list; activation remains pending")
                    })?;
                if pids.contains(&pid) {
                    return Err(format!(
                        "duplicate {process} process; activation remains pending"
                    ));
                }
                pids.push(pid);
            }
            if pids.is_empty() {
                return Err(format!(
                    "empty successful {process} process list; activation remains pending"
                ));
            }
            pids
        }
        _ => {
            return Err(format!(
                "cannot list {process} processes; activation remains pending"
            ));
        }
    };
    if pids.is_empty() {
        return Ok(());
    }
    crate::items::defaults::relaunch(sys, user, process)?;
    for attempt in 0..60 {
        if pids.iter().all(|pid| !sys.pid_alive(*pid)) {
            return Ok(());
        }
        if attempt != 59 {
            let sleep = Cmd::new("/bin/sleep").arg("1").as_user(user);
            let ran = sys.run(&sleep).map_err(|_| {
                format!("{process} exit wait was interrupted; activation remains pending")
            })?;
            if !ran.success() {
                return Err(format!(
                    "{process} exit wait was interrupted; activation remains pending"
                ));
            }
        }
    }
    Err(format!(
        "the previous {process} process has not exited; activation remains pending"
    ))
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
            assert!(activate(&sys, &who, "dock").is_err());
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
            let result = activate(&sys, &who, "widgets");
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
        assert!(activate(&sys, &who, "dock").is_err());
        assert!(pending(&sys, &who, "dock").unwrap());
        assert!(sys.calls().iter().all(|call| !call.contains("killall")));
    }
}
