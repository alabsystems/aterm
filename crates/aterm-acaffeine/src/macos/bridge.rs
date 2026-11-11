// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Keep-awake bridge process across aterm update restarts. It holds what the mug would
//! ([`crate::model::hold`]): nothing while the lid is closed without Keep Awake with Lid Closed,
//! and that option's `PreventSystemSleep` besides the activity when it is on.

use std::io::{self, BufRead as _, Read as _, Write};
use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use super::awake::{AWAKE_REASON, Awake};
use crate::paths::Paths;
use crate::state;

const HELD: &str = "held";

const MAX: Duration = Duration::from_secs(2 * aterm_uds::deadline::FIRST_EXEC_HANG.as_secs() + 60);

const POLL: Duration = Duration::from_secs(1);

const READY_WAIT: Duration = Duration::from_secs(10);

const PID_CAP: u64 = 32;

const LINE_CAP: u64 = 64;

/// Starts a bridge process from the current executable and waits until it holds the activity:
/// whether it does, or found keep-awake off and so had nothing to hold.
pub(super) fn start() -> bool {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            aterm_log::warn!("acaffeine: no bridge: this executable's path cannot be read: {e}");
            return false;
        }
    };
    let mut cmd = aterm_uds::spawnfd::command(&exe);
    cmd.args(["acaffeine", "bridge"])
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .setsid(true);
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            aterm_log::warn!("acaffeine: no bridge: {}: {e}", exe.display());
            return false;
        }
    };
    let heard = child
        .stdout
        .take()
        .map_or(Heard::Not, |out| hear(out, READY_WAIT));
    if bridged(&heard, || child.wait()) {
        return true;
    }
    if heard == Heard::Ended {
        aterm_log::warn!(
            "acaffeine: the bridge (pid {}) ended without holding the Mac awake",
            child.id()
        );
    } else {
        aterm_log::warn!(
            "acaffeine: the bridge (pid {}) did not hold the Mac awake within {} s; ended",
            child.id(),
            READY_WAIT.as_secs()
        );
        let _ = child.kill();
        let _ = child.wait();
    }
    false
}

fn bridged(heard: &Heard, ended: impl FnOnce() -> io::Result<std::process::ExitStatus>) -> bool {
    match heard {
        Heard::Held => true,
        Heard::Ended => ended().is_ok_and(|status| status.success()),
        Heard::Not => false,
    }
}

/// What a bridge's stdout said first ([`hear`]).
#[derive(Debug, PartialEq, Eq)]
enum Heard {
    /// [`HELD`]: its activity has begun.
    Held,
    /// Nothing before the pipe closed: the bridge ended without holding.
    Ended,
    /// Another line, or none in time.
    Not,
}

fn hear(out: impl io::Read + Send + 'static, within: Duration) -> Heard {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let read = io::BufReader::new(out.take(LINE_CAP)).read_line(&mut line);
        let _ = tx.send(read.map(|_| line));
    });
    match rx.recv_timeout(within) {
        Ok(Ok(line)) if line.is_empty() => Heard::Ended,
        Ok(Ok(line))
            if line
                .strip_suffix('\n')
                .map(|s| s.strip_suffix('\r').unwrap_or(s))
                == Some(HELD) =>
        {
            Heard::Held
        }
        Ok(_) | Err(_) => Heard::Not,
    }
}

/// Runs `aterm acaffeine bridge` and holds the keep-awake activity until released.
pub(crate) fn run() -> ExitCode {
    let Some(paths) = Paths::from_env() else {
        eprintln!("aterm acaffeine: bridge: no home directory");
        return ExitCode::FAILURE;
    };
    let me = std::process::id();
    let doors = Doors {
        begin: Awake::begin,
        machine: crate::held::machine_now,
    };
    match bridge(&paths, me, &mut io::stdout(), doors, POLL, MAX) {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("aterm acaffeine: bridge (pid {me}): {why}");
            ExitCode::FAILURE
        }
    }
}

/// What a bridge holds its activity with (`begin`) and reads the machine with (`machine`,
/// [`crate::held::machine_now`]; a test's arm reads an open lid).
#[derive(Clone, Copy)]
struct Doors {
    begin: fn(&str) -> Option<Awake>,
    machine: fn() -> crate::model::Machine,
}

fn bridge(
    paths: &Paths,
    me: u32,
    out: &mut impl Write,
    doors: Doors,
    poll: Duration,
    max: Duration,
) -> Result<(), String> {
    let wanted = || wanted(paths, doors.machine);
    let Some(hold) = wanted() else {
        return Ok(());
    };
    let file = paths.bridge_file();
    write_pid(&file, me).map_err(|e| format!("{}: {e}", file.display()))?;
    let Some(awake) = (doors.begin)(AWAKE_REASON) else {
        forget(&file, me);
        return Err("the keep-awake activity did not begin".into());
    };
    let lid = hold
        .system()
        .then(|| aterm_sysprobe::power::hold(crate::held::LID_ASSERTION, AWAKE_REASON))
        .flatten();
    let _ = writeln!(out, "{HELD}").and_then(|()| out.flush());
    eprintln!("aterm acaffeine: bridge (pid {me}) keeps the Mac awake while the mug restarts");
    let since = Instant::now();
    while holds(
        read_pid(&file),
        me,
        wanted().is_some(),
        since.elapsed(),
        max,
    ) {
        std::thread::sleep(poll);
    }
    drop(lid);
    drop(awake);
    forget(&file, me);
    eprintln!(
        "aterm acaffeine: bridge (pid {me}) let go after {} s",
        since.elapsed().as_secs()
    );
    Ok(())
}

fn holds(named: Option<u32>, me: u32, on: bool, held: Duration, max: Duration) -> bool {
    named == Some(me) && on && held < max
}

/// What the mug would hold now ([`crate::model::hold`]) when that is the activity, with or
/// without the lid's assertion: `None` while the mode is off or paused.
fn wanted(paths: &Paths, machine: fn() -> crate::model::Machine) -> Option<crate::model::Hold> {
    let now = state::clocks().unwrap_or_default();
    let on = state::load(&paths.state_file(), now).is_on(now);
    let hold = crate::model::hold(on, state::load_lid(&paths.lid_file()), machine());
    hold.activity().then_some(hold)
}

/// Releases any running update-restart bridge by removing the bridge PID file.
pub(super) fn release(paths: &Paths) {
    let file = paths.bridge_file();
    match crate::paths::existing_real_dir(&paths.state_dir) {
        Ok(true) => {}
        Ok(false) => return,
        Err(e) => {
            aterm_log::warn!("acaffeine: could not remove {}: {e}", file.display());
            return;
        }
    }
    match std::fs::remove_file(&file) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => aterm_log::warn!("acaffeine: could not remove {}: {e}", file.display()),
    }
}

fn forget(file: &Path, me: u32) {
    if read_pid(file) == Some(me) {
        let _ = std::fs::remove_file(file);
    }
}

fn read_pid(file: &Path) -> Option<u32> {
    use std::os::unix::fs::OpenOptionsExt as _;
    if !crate::paths::parent_is_real_dir(file) {
        return None;
    }
    let opened = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(file)
        .ok()?;
    let meta = opened.metadata().ok()?;
    if !meta.is_file() || meta.len() > PID_CAP {
        return None;
    }
    let mut text =
        String::with_capacity(usize::try_from(meta.len().saturating_add(1)).unwrap_or(0));
    opened.take(PID_CAP + 1).read_to_string(&mut text).ok()?;
    if text.len() as u64 > PID_CAP {
        return None;
    }
    let digits = text.trim();
    (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
        .then(|| digits.parse().ok())
        .flatten()
        .filter(|pid| *pid > 0)
}

fn write_pid(file: &Path, pid: u32) -> io::Result<()> {
    if let Some(parent) = file.parent()
        && !parent.as_os_str().is_empty()
    {
        crate::paths::ensure_real_dir(parent)?;
    }
    let (tmp, mut out) = aterm_uds::atomic_file::create_temp_beside(file, 0o600)?;
    let written = out.write_all(format!("{pid}\n").as_bytes()).and_then(|()| {
        if let Some(parent) = file.parent()
            && !parent.as_os_str().is_empty()
            && !crate::paths::existing_real_dir(parent)?
        {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("{} does not exist", parent.display()),
            ));
        }
        std::fs::rename(&tmp, file)
    });
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

#[cfg(test)]
#[path = "bridge_tests.rs"]
mod tests;
