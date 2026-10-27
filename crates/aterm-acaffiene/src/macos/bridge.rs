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

const HELD: &str = "held";

const MAX: Duration = Duration::from_secs(2 * aterm_uds::deadline::FIRST_EXEC_HANG.as_secs() + 60);

const POLL: Duration = Duration::from_secs(1);

const READY_WAIT: Duration = Duration::from_secs(10);

const PID_CAP: u64 = 32;

/// Starts a bridge process from the current executable and waits until it holds the activity:
/// whether it does, or found keep-awake off and so had nothing to hold.
pub(super) fn start() -> bool {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            aterm_log::warn!("acaffiene: no bridge: this executable's path cannot be read: {e}");
            return false;
        }
    };
    let mut cmd = aterm_uds::spawnfd::command(&exe);
    cmd.args(["acaffiene", "bridge"])
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .setsid(true);
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            aterm_log::warn!("acaffiene: no bridge: {}: {e}", exe.display());
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
            "acaffiene: the bridge (pid {}) ended without holding the Mac awake",
            child.id()
        );
    } else {
        aterm_log::warn!(
            "acaffiene: the bridge (pid {}) did not hold the Mac awake within {} s; ended",
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
        let read = io::BufReader::new(out).read_line(&mut line);
        let _ = tx.send(read.map(|_| line));
    });
    match rx.recv_timeout(within) {
        Ok(Ok(line)) if line.is_empty() => Heard::Ended,
        Ok(Ok(line)) if line.trim_end() == HELD => Heard::Held,
        Ok(_) | Err(_) => Heard::Not,
    }
}

/// Runs `aterm acaffiene bridge` and holds the keep-awake activity until released.
pub(crate) fn run() -> ExitCode {
    let Some(paths) = Paths::from_env() else {
        eprintln!("aterm acaffiene: bridge: no home directory");
        return ExitCode::FAILURE;
    };
    let me = std::process::id();
    match bridge(&paths, me, &mut io::stdout(), Awake::begin, POLL, MAX) {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("aterm acaffiene: bridge (pid {me}): {why}");
            ExitCode::FAILURE
        }
    }
}

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
    eprintln!("aterm acaffiene: bridge (pid {me}) keeps the Mac awake while the mug restarts");
    let since = Instant::now();
    while holds(read_pid(&file), me, mode_on(paths), since.elapsed(), max) {
        std::thread::sleep(poll);
    }
    drop(awake);
    forget(&file, me);
    eprintln!(
        "aterm acaffiene: bridge (pid {me}) let go after {} s",
        since.elapsed().as_secs()
    );
    Ok(())
}

fn holds(named: Option<u32>, me: u32, on: bool, held: Duration, max: Duration) -> bool {
    named == Some(me) && on && held < max
}

fn mode_on(paths: &Paths) -> bool {
    let now = state::clocks().unwrap_or_default();
    state::load(&paths.state_file(), now).is_on(now)
}

/// Releases any running update-restart bridge by removing the bridge PID file.
pub(super) fn release(paths: &Paths) {
    let file = paths.bridge_file();
    match std::fs::remove_file(&file) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => aterm_log::warn!("acaffiene: could not remove {}: {e}", file.display()),
    }
}

fn forget(file: &Path, me: u32) {
    if read_pid(file) == Some(me) {
        let _ = std::fs::remove_file(file);
    }
}

fn read_pid(file: &Path) -> Option<u32> {
    let mut text = String::new();
    std::fs::File::open(file)
        .ok()?
        .take(PID_CAP)
        .read_to_string(&mut text)
        .ok()?;
    text.trim().parse().ok()
}

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

    const REASON: &str = "aterm acaffiene bridge test";

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
                    let begin = |_: &str| Awake::begin("aterm acaffiene bridge let-go test");
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
        state::save(&paths.state_file(), Mode::Off).expect("saved off");
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

    #[test]
    fn the_mug_waits_for_held_alone() {
        assert_eq!(hear(&b"held\n"[..], SAYS), Heard::Held);
        assert_eq!(hear(&b"error\n"[..], SAYS), Heard::Not);
        assert_eq!(hear(&b""[..], SAYS), Heard::Ended);
        let (read, _write) = std::io::pipe().expect("pipe");
        assert_eq!(hear(read, Duration::from_millis(50)), Heard::Not);
        assert_eq!(hear(&b"\xff\n"[..], SAYS), Heard::Not, "not a line of text");
    }

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
        let file = dir.path().join("acaffiene.bridge");
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
