// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Keep-awake bridge process across aterm update restarts.

use std::io::{self, BufRead as _, Read as _, Write};
use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use super::awake::{AWAKE_REASON, Awake};
use crate::paths::Paths;
use crate::state;

/// The line the bridge prints on its stdout once its activity has begun.
const HELD: &str = "held";

/// The longest a bridge holds, on the awake clock: the first execs of the new build and of its
/// copy, each assessed for up to the first-exec hang, and a minute more for launchd and the new
/// mug's start.
const MAX: Duration = Duration::from_secs(2 * aterm_uds::deadline::FIRST_EXEC_HANG.as_secs() + 60);

/// How often the bridge looks whether to let go.
const POLL: Duration = Duration::from_secs(1);

/// How long a restarting mug waits for its bridge's [`HELD`]: the exec of the mug's own copy
/// (assessed already, as the mug runs from it) and the activity's begin take milliseconds. A
/// bare build the update replaced may still be assessed then, and misses it ([`start`]).
const READY_WAIT: Duration = Duration::from_secs(10);

/// The most of [`Paths::bridge_file`] read: a pid is a few digits.
const PID_CAP: u64 = 32;

/// Starts a bridge process from the current executable and waits until it holds the activity:
/// whether it does, or found keep-awake off and so had nothing to hold.
pub(super) fn start() -> bool {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            aterm_log::warn!("caffeine: no bridge: this executable's path cannot be read: {e}");
            return false;
        }
    };
    let mut cmd = aterm_uds::spawnfd::command(&exe);
    cmd.args(["caffeine", "bridge"])
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .setsid(true);
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            aterm_log::warn!("caffeine: no bridge: {}: {e}", exe.display());
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
            "caffeine: the bridge (pid {}) ended without holding the Mac awake",
            child.id()
        );
    } else {
        aterm_log::warn!(
            "caffeine: the bridge (pid {}) did not hold the Mac awake within {} s; ended",
            child.id(),
            READY_WAIT.as_secs()
        );
        let _ = child.kill();
        let _ = child.wait();
    }
    false
}

/// Whether a bridge that said `heard` took over: it holds the Mac awake, or it ended with 0
/// (`ended` waits for its exit) because it found keep-awake saved off (turned off while this
/// mug was leaving) and so had nothing to hold.
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

/// What `out`'s first line is within `within`. The line is read on a thread of its own, which
/// ends with the pipe: at the line, or once the child is killed.
fn hear(out: impl io::Read + Send + 'static, within: Duration) -> Heard {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let read = io::BufReader::new(out).read_line(&mut line);
        let _ = tx.send(read.map(|_| line));
    });
    match rx.recv_timeout(within) {
        Ok(Ok(line)) if line.is_empty() => Heard::Ended,
        Ok(Ok(line)) if line.trim_end() == HELD => Heard::Held,
        Ok(_) | Err(_) => Heard::Not,
    }
}

/// Runs `aterm caffeine bridge` and holds the keep-awake activity until released.
pub(crate) fn run() -> ExitCode {
    let Some(paths) = Paths::from_env() else {
        eprintln!("aterm caffeine: bridge: no home directory");
        return ExitCode::FAILURE;
    };
    let me = std::process::id();
    match bridge(&paths, me, &mut io::stdout(), Awake::begin, POLL, MAX) {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("aterm caffeine: bridge (pid {me}): {why}");
            ExitCode::FAILURE
        }
    }
}

/// [`run`]'s work for the bridge `me`, the activity begun by `begin`, [`HELD`] said on `out`,
/// looking again every `poll` for up to `max`.
fn bridge(
    paths: &Paths,
    me: u32,
    out: &mut impl Write,
    begin: fn(&str) -> Option<Awake>,
    poll: Duration,
    max: Duration,
) -> Result<(), String> {
    if !mode_on(paths) {
        return Ok(());
    }
    let file = paths.bridge_file();
    write_pid(&file, me).map_err(|e| format!("{}: {e}", file.display()))?;
    let Some(awake) = begin(AWAKE_REASON) else {
        forget(&file, me);
        return Err("the keep-awake activity did not begin".into());
    };
    let _ = writeln!(out, "{HELD}").and_then(|()| out.flush());
    eprintln!("aterm caffeine: bridge (pid {me}) keeps the Mac awake while the mug restarts");
    let since = Instant::now();
    while holds(read_pid(&file), me, mode_on(paths), since.elapsed(), max) {
        std::thread::sleep(poll);
    }
    drop(awake);
    forget(&file, me);
    eprintln!(
        "aterm caffeine: bridge (pid {me}) let go after {} s",
        since.elapsed().as_secs()
    );
    Ok(())
}

/// Whether the bridge `me`, holding for `held` so far, keeps holding: [`Paths::bridge_file`]
/// names it (`named`; the new mug has not taken over), the saved mode reads `on`, and `held`
/// is short of `max`.
fn holds(named: Option<u32>, me: u32, on: bool, held: Duration, max: Duration) -> bool {
    named == Some(me) && on && held < max
}

/// Whether the saved mode reads on now.
fn mode_on(paths: &Paths) -> bool {
    let now = aterm_time::unix_secs().unwrap_or(0);
    state::load(&paths.state_file(), now).is_on(now)
}

/// Releases any running update-restart bridge by removing the bridge PID file.
pub(super) fn release(paths: &Paths) {
    let file = paths.bridge_file();
    match std::fs::remove_file(&file) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => aterm_log::warn!("caffeine: could not remove {}: {e}", file.display()),
    }
}

/// Remove `file` if it still names `me`.
fn forget(file: &Path, me: u32) {
    if read_pid(file) == Some(me) {
        let _ = std::fs::remove_file(file);
    }
}

/// The pid `file` names, if it names one.
fn read_pid(file: &Path) -> Option<u32> {
    let mut text = String::new();
    std::fs::File::open(file)
        .ok()?
        .take(PID_CAP)
        .read_to_string(&mut text)
        .ok()?;
    text.trim().parse().ok()
}

/// Write `pid` to `file` whole: a private temporary file beside it, renamed over it.
fn write_pid(file: &Path, pid: u32) -> io::Result<()> {
    let (tmp, mut out) = aterm_uds::atomic_file::create_temp_beside(file, 0o600)?;
    let written = out
        .write_all(format!("{pid}\n").as_bytes())
        .and_then(|()| std::fs::rename(&tmp, file));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;
    use crate::model::Mode;

    /// The activity name of the test that counts assertions, apart from every other test's in
    /// this process.
    const REASON: &str = "aterm caffeine bridge test";

    /// What [`bridge`] says, sent on a channel line by line.
    struct Said(mpsc::Sender<String>, Vec<u8>);

    impl Write for Said {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.1.extend_from_slice(buf);
            while let Some(end) = self.1.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = self.1.drain(..=end).collect();
                let _ = self
                    .0
                    .send(String::from_utf8_lossy(&line).trim_end().to_string());
            }
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn scratch(dir: &Path) -> Paths {
        Paths {
            state_dir: dir.join("state"),
            home: dir.join("home"),
            logs_dir: dir.join("logs"),
        }
    }

    fn held_types() -> Vec<String> {
        let mut types = super::super::awake::tests::assertions_named(REASON);
        types.sort();
        types
    }

    /// The hang detector for a line [`bridge`] must say.
    const SAYS: Duration = Duration::from_secs(60);

    #[test]
    fn a_bridge_holds_while_it_is_named_on_and_short_of_its_bound() {
        let max = Duration::from_secs(60);
        let short = Duration::from_secs(1);
        assert!(holds(Some(7), 7, true, short, max));
        assert!(!holds(None, 7, true, short, max));
        assert!(!holds(Some(8), 7, true, short, max));
        assert!(!holds(Some(7), 7, false, short, max));
        assert!(holds(Some(7), 7, true, max - Duration::from_millis(1), max));
        assert!(!holds(Some(7), 7, true, max, max));
        assert!(MAX > 2 * aterm_uds::deadline::FIRST_EXEC_HANG);
    }

    /// The bridge's whole life: it names itself, holds both idle-sleep assertions while it says
    /// it does, and once the new mug removes its file, lets go of them at its next look.
    #[test]
    fn a_bridge_holds_the_mac_awake_until_the_new_mug_takes_over() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let paths = scratch(dir.path());
        state::save(&paths.state_file(), Mode::Forever).expect("saved on");
        let (tx, rx) = mpsc::channel();
        let mug = {
            let paths = paths.clone();
            std::thread::spawn(move || {
                let line = rx.recv_timeout(SAYS).expect("the bridge says it holds");
                assert_eq!(line, HELD);
                assert_eq!(read_pid(&paths.bridge_file()), Some(4242));
                let types = held_types();
                release(&paths);
                types
            })
        };
        let mut out = Said(tx, Vec::new());
        let poll = Duration::from_millis(20);
        let begun = Instant::now();
        bridge(&paths, 4242, &mut out, |_| Awake::begin(REASON), poll, SAYS).expect("bridge");
        assert!(
            begun.elapsed() < SAYS,
            "it let go when its file went, not at its bound"
        );
        assert_eq!(
            mug.join().expect("mug"),
            ["PreventUserIdleDisplaySleep", "PreventUserIdleSystemSleep"]
        );
        assert_eq!(held_types(), Vec::<String>::new());
        assert!(!paths.bridge_file().exists());
    }

    /// Off saved while no mug runs (`aterm caffeine off` or `quit`) ends a bridge at its next
    /// look, and so does its bound; either way it forgets itself. The bridge runs on a thread
    /// of its own, so one that never lets go fails the test at the hang detector.
    #[test]
    fn a_bridge_lets_go_once_the_mode_reads_off_or_at_its_bound() {
        for off in [true, false] {
            let dir = aterm_tempfile::tempdir().expect("tempdir");
            let paths = scratch(dir.path());
            state::save(&paths.state_file(), Mode::Forever).expect("saved on");
            let max = if off {
                SAYS
            } else {
                Duration::from_millis(200)
            };
            let (said, heard) = mpsc::channel();
            let (done, ended) = mpsc::channel();
            let begun = Instant::now();
            {
                let paths = paths.clone();
                std::thread::spawn(move || {
                    let mut out = Said(said, Vec::new());
                    let begin = |_: &str| Awake::begin("aterm caffeine bridge let-go test");
                    let poll = Duration::from_millis(20);
                    let _ = done.send(bridge(&paths, 4242, &mut out, begin, poll, max));
                });
            }
            assert_eq!(heard.recv_timeout(SAYS).as_deref(), Ok(HELD));
            if off {
                state::save(&paths.state_file(), Mode::Off).expect("saved off");
            }
            let result = ended.recv_timeout(SAYS).expect("the bridge lets go");
            assert_eq!(result, Ok(()));
            if off {
                assert!(
                    begun.elapsed() < max,
                    "it let go when off was saved, not at its bound"
                );
            }
            assert!(
                !paths.bridge_file().exists(),
                "a bridge that lets go forgets itself"
            );
        }
    }

    #[test]
    fn a_bridge_with_nothing_to_hold_ends_at_once_and_says_nothing() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let paths = scratch(dir.path());
        let (tx, rx) = mpsc::channel();
        let mut out = Said(tx, Vec::new());
        bridge(
            &paths,
            4242,
            &mut out,
            |_| panic!("nothing to hold"),
            POLL,
            MAX,
        )
        .expect("off");
        assert!(rx.try_recv().is_err());
        assert!(!paths.bridge_file().exists());
        state::save(&paths.state_file(), Mode::Forever).expect("saved on");
        let (tx, rx) = mpsc::channel();
        let mut out = Said(tx, Vec::new());
        assert!(bridge(&paths, 4242, &mut out, |_| None, POLL, MAX).is_err());
        assert!(rx.try_recv().is_err());
        assert!(!paths.bridge_file().exists());
    }

    /// The mug waits for [`HELD`] alone, and tells a bridge that ended without a word (one that
    /// found keep-awake off) from one that said something else or nothing in time.
    #[test]
    fn the_mug_waits_for_held_alone() {
        assert_eq!(hear(&b"held\n"[..], SAYS), Heard::Held);
        assert_eq!(hear(&b"error\n"[..], SAYS), Heard::Not);
        assert_eq!(hear(&b""[..], SAYS), Heard::Ended);
        let (read, _write) = std::io::pipe().expect("pipe");
        assert_eq!(hear(read, Duration::from_millis(50)), Heard::Not);
        assert_eq!(hear(&b"\xff\n"[..], SAYS), Heard::Not, "not a line of text");
    }

    /// A bridge took over when it holds, or ended 0 with nothing to hold; one that ended
    /// otherwise, or said anything else, did not, and is not waited on as an ended one.
    #[test]
    fn a_bridge_takes_over_by_holding_or_by_a_clean_end() {
        use std::os::unix::process::ExitStatusExt as _;
        use std::process::ExitStatus;
        let unasked = || -> io::Result<ExitStatus> { panic!("an exit read where none ended") };
        assert!(bridged(&Heard::Held, unasked));
        assert!(!bridged(&Heard::Not, unasked));
        assert!(bridged(&Heard::Ended, || Ok(ExitStatus::from_raw(0))));
        assert!(
            !bridged(&Heard::Ended, || Ok(ExitStatus::from_raw(1 << 8))),
            "exit 1"
        );
        assert!(
            !bridged(&Heard::Ended, || Ok(ExitStatus::from_raw(9))),
            "SIGKILL"
        );
        assert!(!bridged(&Heard::Ended, || Err(io::Error::other(
            "no child"
        ))));
    }

    #[test]
    fn a_pid_reads_back_and_anything_else_names_none() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("caffeine.bridge");
        assert_eq!(read_pid(&file), None);
        write_pid(&file, 4242).expect("write");
        assert_eq!(read_pid(&file), Some(4242));
        forget(&file, 4243);
        assert_eq!(read_pid(&file), Some(4242), "another bridge's file stays");
        forget(&file, 4242);
        assert!(!file.exists());
        std::fs::write(&file, b"not a pid").expect("write");
        assert_eq!(read_pid(&file), None);
        let past = format!(
            "{}4242\n",
            " ".repeat(usize::try_from(PID_CAP).expect("small"))
        );
        std::fs::write(&file, past).expect("write");
        assert_eq!(read_pid(&file), None, "past the cap");
    }
}
