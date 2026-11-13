// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::mpsc;

use super::*;
use crate::model::Mode;

const REASON: &str = "aterm acaffeine bridge test";

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
        local_volume: |_| true,
    }
}

fn held_types() -> Vec<String> {
    let mut types = super::super::awake::tests::assertions_named(REASON);
    types.sort();
    types
}

const SAYS: Duration = Duration::from_secs(60);

/// A bridge's doors in a test: `begin`, and a machine with its lid open in a full wake.
fn doors(begin: fn(&str) -> Option<Awake>) -> Doors {
    Doors {
        begin,
        machine: crate::model::Machine::default,
    }
}

/// A bridge over a closed lid with Keep Awake with Lid Closed off holds nothing and says
/// nothing, the mode staying on; with the option on it holds.
/// NEGATIVE CONTROL: the same bridge over an open lid holds.
#[test]
fn a_bridge_over_a_closed_lid_holds_only_with_the_opt_in() {
    let dir = crate::test_scratch_dir("bridge");
    let paths = scratch(dir.path());
    state::save(&paths.state_file(), Mode::Forever).expect("saved on");
    let closed = || crate::model::Machine {
        lid_closed: true,
        ..crate::model::Machine::default()
    };
    assert_eq!(wanted(&paths, closed), None);
    let (tx, rx) = mpsc::channel();
    let mut out = Said(tx, Vec::new());
    let doors = Doors {
        begin: |_| panic!("nothing to hold with the lid closed"),
        machine: closed,
    };
    bridge(&paths, 4242, &mut out, doors, POLL, MAX).expect("paused");
    assert!(rx.try_recv().is_err());
    assert!(!paths.bridge_file().exists());
    assert_eq!(
        wanted(&paths, crate::model::Machine::default),
        Some(crate::model::Hold::Idle),
        "CONTROL"
    );
    state::save_lid(&paths.lid_file(), true).expect("lid on");
    assert_eq!(
        wanted(&paths, closed),
        Some(crate::model::Hold::LidClosedToo)
    );
}

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
    let dir = crate::test_scratch_dir("bridge");
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
    bridge(
        &paths,
        4242,
        &mut out,
        doors(|_| Awake::begin(REASON)),
        poll,
        SAYS,
    )
    .expect("bridge");
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
        let dir = crate::test_scratch_dir("bridge");
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
                let begin = |_: &str| Awake::begin("aterm acaffeine bridge let-go test");
                let poll = Duration::from_millis(20);
                let _ = done.send(bridge(&paths, 4242, &mut out, doors(begin), poll, max));
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
    let dir = crate::test_scratch_dir("bridge");
    let paths = scratch(dir.path());
    state::save(&paths.state_file(), Mode::Off).expect("saved off");
    let (tx, rx) = mpsc::channel();
    let mut out = Said(tx, Vec::new());
    bridge(
        &paths,
        4242,
        &mut out,
        doors(|_| panic!("nothing to hold")),
        POLL,
        MAX,
    )
    .expect("off");
    assert!(rx.try_recv().is_err());
    assert!(!paths.bridge_file().exists());
    state::save(&paths.state_file(), Mode::Forever).expect("saved on");
    let (tx, rx) = mpsc::channel();
    let mut out = Said(tx, Vec::new());
    assert!(bridge(&paths, 4242, &mut out, doors(|_| None), POLL, MAX).is_err());
    assert!(rx.try_recv().is_err());
    assert!(!paths.bridge_file().exists());
}

#[test]
fn the_mug_waits_for_held_alone() {
    assert_eq!(hear(&b"held\n"[..], SAYS), Heard::Held);
    assert_eq!(hear(&b"held\r\n"[..], SAYS), Heard::Held);
    assert_eq!(hear(&b"held"[..], SAYS), Heard::Not, "unterminated");
    assert_eq!(hear(&[b'x'; 256][..], SAYS), Heard::Not, "past cap");
    assert_eq!(hear(&b"error\n"[..], SAYS), Heard::Not);
    assert_eq!(hear(&b""[..], SAYS), Heard::Ended);
    let (read, _write) = aterm_uds::pipe().expect("pipe");
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
    let dir = crate::test_scratch_dir("bridge");
    let file = dir.path().join("nested").join("acaffeine.bridge");
    assert_eq!(read_pid(&file), None);
    write_pid(&file, 4242).expect("write");
    assert_eq!(read_pid(&file), Some(4242));
    forget(&file, 4243);
    assert_eq!(read_pid(&file), Some(4242), "another bridge's file stays");
    forget(&file, 4242);
    assert!(!file.exists());
    std::fs::write(&file, b"0\n").expect("write");
    assert_eq!(read_pid(&file), None, "zero is not a pid");
    std::fs::write(&file, b"not a pid").expect("write");
    assert_eq!(read_pid(&file), None);
    let long_digits = format!(
        "{}4242\n",
        "0".repeat(usize::try_from(PID_CAP).expect("small"))
    );
    std::fs::write(&file, long_digits).expect("write");
    assert_eq!(read_pid(&file), None, "digits past the cap never truncate");
    let past = format!(
        "{}4242\n",
        " ".repeat(usize::try_from(PID_CAP).expect("small"))
    );
    std::fs::write(&file, past).expect("write");
    assert_eq!(read_pid(&file), None, "past the cap");
    let target = dir.path().join("real");
    std::fs::write(&target, b"4242\n").expect("real");
    let link = dir.path().join("link.bridge");
    std::os::unix::fs::symlink(&target, &link).expect("symlink");
    assert_eq!(read_pid(&link), None, "symlinks are not followed");
}
