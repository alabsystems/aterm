// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn target() -> Identity {
    Identity {
        pid: 4663,
        birth: Birth(123),
        idversion: aterm_uds::process::KEEPS_PID_VERSIONS.then_some(7),
    }
}

#[test]
fn a_reused_pid_is_gone_but_an_unreadable_birth_is_not() {
    let expected = Birth(123);
    assert!(original_gone(
        expected,
        ProcessPresence::Present,
        Some(Birth(124))
    ));
    assert!(!original_gone(
        expected,
        ProcessPresence::Present,
        Some(expected)
    ));
    for presence in [ProcessPresence::Present, ProcessPresence::Unknown] {
        assert!(!original_gone(expected, presence, None));
    }
}

#[test]
fn only_system_settings_quit_is_a_quit_and_reopen() {
    for sender in REOPEN_SENDERS {
        assert!(sender_reopens(Path::new(sender)), "{sender}");
    }
    assert_eq!(
        REOPEN_SENDERS[0],
        "/System/Library/ExtensionKit/Extensions/SecurityPrivacyExtension.appex/Contents/\
         MacOS/SecurityPrivacyExtension"
    );
    for other in [
        "/System/Library/CoreServices/Dock.app/Contents/MacOS/Dock",
        "/usr/bin/osascript",
        "/System/Library/ExtensionKit/Extensions/SecurityPrivacyExtension.appex",
        "/Applications/System Settings.app/Contents/MacOS/System Settings",
        "/tmp/System/Applications/System Settings.app/Contents/MacOS/System Settings",
        "System/Applications/System Settings.app/Contents/MacOS/System Settings",
        "",
    ] {
        assert!(!sender_reopens(Path::new(other)), "{other}");
    }
    assert_eq!(KEY_SENDER_PID, 0x7370_6964);
}

/// An absolute path to a `.app` on the host the test runs on.
fn host_bundle() -> &'static Path {
    Path::new(if cfg!(windows) {
        r"C:\Applications\aterm.app"
    } else {
        "/Applications/aterm.app"
    })
}

#[test]
fn the_helper_reads_back_only_what_the_quit_wrote() {
    let bundle = host_bundle();
    let target = target();
    let args = reopen_args(target, bundle);
    assert_eq!(args[0], REOPEN_VERB);
    assert_eq!(
        parse_reopen_args(&args[1..]),
        Some((target, bundle.to_path_buf()))
    );
    let app = bundle.to_str().expect("a UTF-8 path");
    let exe = bundle.join("Contents/MacOS/aterm");
    let exe = exe.to_str().expect("a UTF-8 path");
    let token = target.token();
    let token = token.as_str();
    let refused: [&[&str]; 6] = [
        &[],
        &[token],
        &[token, app, "more"],
        &["4663", app],
        &[token, "Applications/aterm.app"],
        &[token, exe],
    ];
    for words in refused {
        let args: Vec<OsString> = words.iter().map(OsString::from).collect();
        assert_eq!(parse_reopen_args(&args), None, "{words:?}");
    }
}

#[test]
fn the_helper_runs_the_bundles_front_door_whatever_name_this_aterm_ran_under() {
    let bundle = host_bundle();
    let target = target();
    let command = reopen_command(target, bundle);
    let program = Path::new(command.get_program());
    assert_eq!(program, bundle.join("Contents/MacOS/aterm"));
    assert_eq!(program.file_name(), Some(std::ffi::OsStr::new("aterm")));
    let args: Vec<&std::ffi::OsStr> = command.get_args().collect();
    let wrote = reopen_args(target, bundle);
    assert_eq!(
        args,
        wrote.iter().map(OsString::as_os_str).collect::<Vec<_>>()
    );
}

/// The helper opens aterm only once the quitting one is gone, and tries `open` again on
/// [`OPEN_RETRY`]'s schedule while it fails: the second try a second after the first
/// failure, doubling, [`OPEN_TRIES`] tries in all. NEGATIVE CONTROL: an aterm still running
/// at the ceiling is opened and waited for not at all.
#[test]
fn the_helper_opens_once_the_quitting_aterm_is_gone_and_again_while_open_fails() {
    use std::cell::{Cell, RefCell};
    let steps = RefCell::new(Vec::new());
    let run = |gone: bool, fails: u32| {
        steps.borrow_mut().clear();
        let tries = Cell::new(0);
        let reopened = reopen_once_gone(
            || {
                steps.borrow_mut().push("gone".to_owned());
                gone
            },
            || steps.borrow_mut().push("forgotten".to_owned()),
            || {
                tries.set(tries.get() + 1);
                steps.borrow_mut().push("open".to_owned());
                tries.get() > fails
            },
            |pause| steps.borrow_mut().push(format!("{}s", pause.as_secs())),
        );
        (reopened, steps.borrow().clone())
    };
    let words = |words: &[&str]| words.iter().map(|word| (*word).to_owned()).collect();
    assert_eq!(run(false, 0), (Reopened::StillRunning, words(&["gone"])));
    assert_eq!(
        run(true, 0),
        (
            Reopened::Opened { tries: 1 },
            words(&["gone", "forgotten", "0s", "open"])
        )
    );
    assert_eq!(
        run(true, 2),
        (
            Reopened::Opened { tries: 3 },
            words(&[
                "gone",
                "forgotten",
                "0s",
                "open",
                "1s",
                "open",
                "2s",
                "open"
            ])
        ),
        "a failed open is tried again, a second and then two after"
    );
    let (reopened, steps) = run(true, u32::MAX);
    assert_eq!(reopened, Reopened::OpenFailed);
    let opens = steps.iter().filter(|step| *step == "open").count();
    assert_eq!(opens, usize::try_from(OPEN_TRIES).expect("a few tries"));
    assert_eq!(
        steps.last().map(String::as_str),
        Some("open"),
        "no pause after the last"
    );
    assert!(
        steps.contains(&format!("{}s", OPEN_RETRY.longest().as_secs())),
        "the pauses reach the schedule's longest: {steps:?}"
    );
}

/// What the helper did reaches the next aterm's journal once: its record reads back each
/// outcome with its tries, the next launch takes it and it goes, and a record that does not
/// read goes too, journaling nothing. NEGATIVE CONTROL: no record, no line.
#[test]
fn the_helpers_outcome_reaches_the_next_journal_once() {
    let dir = aterm_tempfile::tempdir().expect("a scratch <state>");
    let state = dir.path();
    assert_eq!(take_outcome_in(state), None, "no record");
    for reopened in [
        Reopened::Opened { tries: 1 },
        Reopened::Opened { tries: OPEN_TRIES },
        Reopened::OpenFailed,
        Reopened::StillRunning,
    ] {
        note_outcome_in(state, 1_800_000_000, reopened).expect("the record");
        assert_eq!(
            take_outcome_in(state),
            Some((1_800_000_000, reopened)),
            "{reopened:?}"
        );
        assert_eq!(take_outcome_in(state), None, "{reopened:?}: taken once");
    }
    for junk in [
        "",
        "1800000000 opened 1",
        "1800000000 opened 0\n",
        "1800000000 opened 9\n",
        "1800000000 open-failed 3\n",
        "1800000000 still-running 1\n",
        "1800000000 reopened 1\n",
        "x opened 1\n",
        "1800000000 opened 1 more\n",
    ] {
        std::fs::write(state.join(OUTCOME_RECORD), junk).expect("junk");
        assert_eq!(take_outcome_in(state), None, "{junk:?}");
        assert!(!state.join(OUTCOME_RECORD).exists(), "{junk:?} goes");
    }
}

#[cfg(target_os = "macos")]
#[test]
fn the_wait_ends_at_the_exit_and_not_before() {
    let cat = aterm_uds::deadline::stand_in(Path::new("/bin/cat"));
    let mut child = aterm_uds::spawnfd::command(&cat)
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .spawn()
        .expect("spawn cat with held input");
    let target = i32::try_from(child.id())
        .ok()
        .and_then(aterm_uds::process::read_identity)
        .expect("the child's exact identity");
    assert!(
        !wait_gone(target, Duration::ZERO),
        "a running process read as gone"
    );
    child.kill().expect("kill cat");
    assert!(
        wait_gone(target, Duration::from_secs(60)),
        "the exit of an unreaped child was missed"
    );
    aterm_uds::deadline::kill_within(&mut child, Duration::from_secs(60));
}
