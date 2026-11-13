// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[cfg(target_vendor = "apple")]
#[test]
fn a_completed_native_trust_probe_is_not_silent() {
    super::require_trust_service();
    static GATE: super::super::Gate = super::super::Gate::new("native-probe-test");
    let now = std::time::Instant::now();
    assert!(!super::probe_silent_at(
        now - std::time::Duration::from_secs(31),
        now,
        |within| GATE.run(within, within, super::probe),
    ));
}

#[test]
fn an_expired_probe_does_not_start_and_a_failed_probe_is_not_silence() {
    let now = std::time::Instant::now();
    assert!(super::probe_silent_at(
        now - std::time::Duration::from_secs(61),
        now,
        |_| panic!("an expired probe must not start"),
    ));
    assert!(!super::probe_silent_at(now, now, |_| {
        Err(super::Refused::Failed("native failure".to_owned()))
    }));
    assert!(super::probe_silent_at(now, now, |_| {
        Err(super::Refused::Unanswered)
    }));
}
use std::os::unix::process::ExitStatusExt as _;
use std::process::{ExitStatus, Output};

fn run(code: i32, stderr: &str) -> Output {
    Output {
        status: ExitStatus::from_raw(code << 8),
        stdout: Vec::new(),
        stderr: stderr.as_bytes().to_vec(),
    }
}

/// Only a failed run that timed out is the service's silence.
#[test]
fn only_a_timed_out_failure_is_a_silent_disk_image_service() {
    use super::timed_out as silent;
    assert!(silent(&run(
        1,
        "hdiutil: create failed - Operation timed out\n"
    )));
    assert!(!silent(&run(
        1,
        "hdiutil: create failed - No space left on device\n"
    )));
    assert!(!silent(&run(1, "")));
    assert!(!silent(&run(0, "Operation timed out\n")));
}

#[cfg(target_vendor = "apple")]
#[test]
fn script_stand_in_executes_with_its_sibling_from_another_working_directory() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("script package with spaces");
    for name in ["bin", "libexec", "elsewhere"] {
        std::fs::create_dir_all(root.join(name)).expect("fixture directory");
    }
    for (name, bytes) in [
        (
            "bin/launcher",
            b"#!/bin/sh\ncase $0 in */*) base=${0%/*} ;; *) base=. ;; esac\n\
              exec \"$base/../libexec/companion\" \"$@\"\n"
                .as_slice(),
        ),
        (
            "libexec/companion",
            b"#!/bin/sh\nprintf 'sibling retained:%s\\n' \"$1\"\nexit 23\n".as_slice(),
        ),
    ] {
        let path = root.join(name);
        std::fs::write(&path, bytes).expect("script fixture");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
            .expect("executable script");
    }
    for selection in ["absolute", "relative"] {
        let mut child = crate::spawnfd::command(std::env::current_exe().expect("test binary"));
        child
            .args([
                "--exact",
                "deadline::silent::tests::script_stand_in_actor",
                "--test-threads=1",
                "--nocapture",
            ])
            .current_dir(&root)
            .env("ATERM_UDS_SCRIPT_STAND_IN_ROOT", &root)
            .env("ATERM_UDS_SCRIPT_STAND_IN_SELECTION", selection);
        let output = super::super::output_within(&mut child, aterm_spec::wait::MUST_HAPPEN)
            .expect("script actor completes");
        assert!(
            output.status.success(),
            "{selection}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[cfg(target_vendor = "apple")]
#[test]
fn script_stand_in_actor() {
    let Some(root) = std::env::var_os("ATERM_UDS_SCRIPT_STAND_IN_ROOT") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let from = match std::env::var("ATERM_UDS_SCRIPT_STAND_IN_SELECTION")
        .expect("selection")
        .as_str()
    {
        "absolute" => root.join("bin/launcher"),
        "relative" => std::path::PathBuf::from("bin/launcher"),
        _ => panic!("unknown selection"),
    };
    let selected = super::stand_in(&from);
    let mut command = crate::spawnfd::command(&selected);
    command
        .current_dir(root.join("elsewhere"))
        .env_clear()
        .env("PATH", "")
        .arg("payload with spaces");
    let output = super::super::output_within(&mut command, aterm_spec::wait::MUST_HAPPEN)
        .expect("selected script executes");
    assert_eq!(
        output.status.code(),
        Some(23),
        "{}: {}",
        selected.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"sibling retained:payload with spaces\n");
    assert!(output.stderr.is_empty(), "{output:?}");
}

/// A stand-in is signed once per version of its binary: a second ask for the same
/// binary copies the signed one, and a changed binary is signed again.
#[test]
fn a_stand_in_is_signed_once_per_version_of_its_binary() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let cache = dir.path().join("cache");
    let binary = dir.path().join("tool");
    std::fs::write(&binary, b"version one").expect("the binary");
    let mut signed = Vec::new();
    let mut sign = |fresh: &std::path::Path| {
        signed.push(fresh.to_path_buf());
        let mut bytes = std::fs::read(fresh).expect("the fresh copy");
        bytes.extend_from_slice(b" signed");
        std::fs::write(fresh, bytes).expect("sign the fresh copy");
    };
    let first = super::signed_stand_in(&cache, &binary, &mut sign);
    let again = super::signed_stand_in(&cache, &binary, &mut sign);
    assert_eq!(
        first.file_name(),
        binary.file_name(),
        "argv[0] keeps the program's name"
    );
    assert_eq!(first, again, "one stand-in per version");
    assert_eq!(std::fs::read(&first).unwrap(), b"version one signed");
    std::fs::write(&binary, b"version two, longer").expect("the binary changes");
    let changed = super::signed_stand_in(&cache, &binary, &mut sign);
    assert_ne!(changed, first, "a new version is a new stand-in");
    assert_eq!(
        std::fs::read(&changed).unwrap(),
        b"version two, longer signed"
    );
    assert_eq!(signed.len(), 2, "signed once per version: {signed:?}");
    let left: Vec<_> = std::fs::read_dir(&cache)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| name.to_string_lossy().starts_with('.'))
        .collect();
    assert!(left.is_empty(), "no unplaced copy left: {left:?}");
}

/// A stand-in once placed is never replaced: a caller that finds one placed while it
/// signed its own (another test process's, which may be running it while macOS assesses
/// it by path) keeps the placed one, file and all, and leaves no copy of its own behind.
#[test]
fn a_placed_stand_in_is_never_replaced() {
    use std::os::unix::fs::MetadataExt as _;
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let cache = dir.path().join("cache");
    let binary = dir.path().join("tool");
    std::fs::write(&binary, b"bytes").expect("the binary");
    let kept = super::kept_at(&cache, &binary);
    let mut placed = None;
    let got = super::signed_stand_in(&cache, &binary, &mut |fresh| {
        std::fs::write(fresh, b"ours").expect("sign ours");
        std::fs::create_dir_all(kept.parent().unwrap()).unwrap();
        std::fs::write(&kept, b"theirs").expect("another process places its own");
        placed = Some(std::fs::metadata(&kept).expect("theirs").ino());
    });
    assert_eq!(got, kept);
    assert_eq!(
        std::fs::read(&kept).unwrap(),
        b"theirs",
        "the placed one stays"
    );
    assert_eq!(
        Some(std::fs::metadata(&kept).unwrap().ino()),
        placed,
        "the same file, not a replacement"
    );
    assert_eq!(
        std::fs::read_dir(&cache).unwrap().count(),
        1,
        "no copy of our own left"
    );
    let other = dir.path().join("other");
    std::fs::write(&other, b"other bytes").expect("another binary");
    let own = super::signed_stand_in(&cache, &other, &mut |fresh| {
        std::fs::write(fresh, b"ours").expect("sign ours");
    });
    assert_eq!(
        std::fs::read(&own).unwrap(),
        b"ours",
        "nothing placed: ours is"
    );
}

/// The cache keeps one stand-in per source name: a source rebuilt twice leaves one entry, the
/// older ones removed once their source no longer names them and no lookup asked for them within
/// the idle bound. CONTROLS: an entry a process holds open, one a lookup asked for within the
/// bound, one whose source (another file of the same name) still names it, and one whose source
/// was never recorded are each kept, as is an entry of another name.
#[cfg(target_vendor = "apple")]
#[test]
fn a_rebuilt_source_leaves_one_stand_in() {
    use std::time::{Duration, SystemTime};
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let cache = dir.path().join("cache");
    let named = cache.join("named");
    let binary = dir.path().join("tool");
    let mut sign = |fresh: &std::path::Path| {
        let mut bytes = std::fs::read(fresh).expect("the fresh copy");
        bytes.extend_from_slice(b" signed");
        std::fs::write(fresh, bytes).expect("sign the fresh copy");
    };
    let idle = |kept: &std::path::Path| {
        std::fs::File::open(kept.parent().expect("its version directory"))
            .expect("open the version directory")
            .set_modified(SystemTime::now() - Duration::from_secs(2 * 60 * 60))
            .expect("age the version directory");
    };
    let entries = || {
        let mut names: Vec<String> = std::fs::read_dir(&named)
            .expect("the cache")
            .map(|entry| entry.expect("an entry").path())
            .filter(|path| path.is_dir())
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    };
    let version = |kept: &std::path::Path| {
        kept.parent()
            .and_then(|parent| parent.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .expect("a version directory")
    };
    let mut build = |bytes: &[u8]| {
        std::fs::write(&binary, bytes).expect("the binary is rebuilt");
        super::signed_stand_in(&cache, &binary, &mut sign)
    };

    let other = dir.path().join("other");
    std::fs::write(&other, b"another program").expect("another binary");
    let unrelated = super::signed_stand_in(&cache, &other, &mut |_| {});
    idle(&unrelated);

    let first = build(b"version one");
    idle(&first);
    let second = build(b"version two, longer");
    idle(&second);
    let third = build(b"version three, longer still");
    assert_eq!(
        entries(),
        [version(&unrelated), version(&third)],
        "rebuilt twice, one stand-in of it is left"
    );
    assert_eq!(
        std::fs::read(&third).unwrap(),
        b"version three, longer still signed"
    );

    let held = std::fs::File::open(&third).expect("a process holds it open");
    idle(&third);
    let fourth = build(b"version four, the longest yet");
    assert!(third.is_file(), "an entry held open is kept");
    drop(held);

    let fifth = build(b"version five, longer than four");
    assert!(
        fourth.is_file(),
        "an entry asked for within the bound is kept"
    );
    assert!(
        !third.exists(),
        "once nothing holds it, the superseded entry goes"
    );

    let twin_dir = dir.path().join("twin");
    std::fs::create_dir(&twin_dir).expect("another directory");
    let twin_source = twin_dir.join("tool");
    std::fs::write(&twin_source, b"a twin of the same name").expect("the twin");
    let twin = super::signed_stand_in(&cache, &twin_source, &mut |_| {});
    idle(&twin);
    idle(&fourth);
    idle(&fifth);
    let unrecorded = named.join("tool-1-2.3-4");
    std::fs::create_dir(&unrecorded).expect("an entry no source was recorded for");
    std::fs::write(unrecorded.join("tool"), b"old").expect("its stand-in");
    idle(&unrecorded.join("tool"));
    let sixth = build(b"version six, longer than the fifth");
    assert!(
        twin.is_file(),
        "a source that still names its entry keeps it"
    );
    assert!(
        unrecorded.join("tool").is_file(),
        "an entry whose source is unknown is kept"
    );
    assert!(
        !fourth.exists() && !fifth.exists(),
        "idle, unheld, superseded entries go"
    );
    assert!(sixth.is_file());
    assert!(unrelated.is_file(), "an entry of another name is kept");
}

/// A stand-in a lookup returns is never removed by an eviction that judged it idle before the
/// lookup stamped it: the source is a hard link of the recorded one under the same name, so the
/// eviction never reads the name the lookup found it by. CONTROL: once idle again with no lookup,
/// the same eviction removes it.
#[test]
fn a_stand_in_found_while_its_eviction_runs_is_kept() {
    use std::time::{Duration, SystemTime};
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let cache = dir.path().join("cache");
    let (a, b) = (dir.path().join("a"), dir.path().join("b"));
    std::fs::create_dir(&a).expect("one directory");
    std::fs::create_dir(&b).expect("another directory");
    let recorded = a.join("tool");
    let twin = b.join("tool");
    std::fs::write(&recorded, b"version one").expect("the binary");
    std::fs::hard_link(&recorded, &twin).expect("another name for it");
    let old = super::signed_stand_in(&cache, &recorded, &mut |_| {});
    let age = |kept: &std::path::Path| {
        std::fs::File::open(kept.parent().expect("its version directory"))
            .expect("open the version directory")
            .set_modified(SystemTime::now() - Duration::from_secs(2 * 60 * 60))
            .expect("age the version directory");
    };
    age(&old);
    let rebuilt = a.join(".tool.new");
    std::fs::write(&rebuilt, b"version two, longer").expect("the rebuild");
    std::fs::rename(&rebuilt, &recorded).expect("the rebuild replaces the recorded name");
    let new = super::kept_at(&cache, &recorded);
    assert_ne!(new, old, "the rebuild is a new version");
    super::place_stand_in(&cache, &recorded, &new, &mut |_| {});
    let new_version = new.parent().expect("its version directory");

    let found = std::cell::RefCell::new(None);
    super::remove_superseded(new_version, "tool", super::STAND_IN_IDLE, &|_| {
        // The lookup runs after the eviction read the stamp and before it removes the entry.
        *found.borrow_mut() = Some(super::signed_stand_in(&cache, &twin, &mut |_| {
            panic!("the old version is placed already")
        }));
        false
    });
    let found = found
        .into_inner()
        .expect("the eviction asked whether it was in use");
    assert_eq!(found, old, "the twin finds the old version");
    assert!(found.is_file(), "the stand-in the lookup returned is kept");

    age(&old);
    super::remove_superseded(new_version, "tool", super::STAND_IN_IDLE, &|_| false);
    assert!(!old.exists(), "idle with no lookup, it goes");
    assert!(new.is_file());
}

/// A stand-in a process runs, holding no descriptor of it, is in use; a file nothing opened is not.
#[cfg(target_vendor = "apple")]
#[test]
fn a_running_stand_in_is_in_use() {
    let cat = super::stand_in(std::path::Path::new("/bin/cat"));
    let mut child = crate::spawnfd::command(&cat)
        .stdin(crate::spawnfd::Stdio::piped())
        .stdout(crate::spawnfd::Stdio::piped())
        .spawn()
        .expect("spawn the stand-in");
    let stdin = super::cat_echoes(&mut child);
    assert!(super::in_use(&cat), "{} runs", cat.display());
    drop(stdin);
    assert!(child.wait().expect("reap").success());
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let idle = dir.path().join("idle");
    std::fs::write(&idle, b"nobody opens this").expect("a file");
    assert!(!super::in_use(&idle), "nothing holds {}", idle.display());
}

/// OFF APPLE A COPY'S MODE IS SET ONLY ON THE FILE ITS CHILD MADE. A link to a FIFO put at
/// `to` after the copy cleared the name (here by the child's own line, ahead of its open) is
/// opened by `set -C`, and the child writes into the FIFO; the copy then refuses `to`
/// (`InvalidData`) and leaves the link, and the FIFO's mode, as they were. NEGATIVE CONTROL:
/// the bytes reached the FIFO, so the child did write through the link; and a plain copy
/// holds `from`'s bytes, with `from`'s mode.
#[cfg(not(target_vendor = "apple"))]
#[test]
fn a_copys_mode_is_never_set_through_a_link() {
    use std::io::Read as _;
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
    const SOURCE: &[u8] = b"#!/bin/sh\nexit 0\n";
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let mode = |path: &std::path::Path| {
        std::fs::symlink_metadata(path)
            .map(|m| m.mode() & 0o7777)
            .ok()
    };
    let from = dir.path().join("from");
    std::fs::write(&from, SOURCE).expect("the source");
    std::fs::set_permissions(&from, std::fs::Permissions::from_mode(0o750)).expect("chmod");
    let fifo = dir.path().join("fifo");
    let made = super::ends_within(
        crate::spawnfd::command("mkfifo")
            .args(["-m", "600"])
            .arg(&fifo),
        super::ECHO_HANG,
    );
    assert!(made.status.success(), "mkfifo: {made:?}");
    assert_eq!(mode(&fifo), Some(0o600));
    let mut reader = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(&fifo)
        .expect("the FIFO's reader");
    let to = dir.path().join("to");
    let steered = format!("ln -s fifo \"$1\"; {}", super::WRITES_ITS_INPUT);
    let refused = super::copy_regular_fresh_by(&from, &to, &steered);
    let mut got = Vec::new();
    reader.read_to_end(&mut got).expect("what the child wrote");
    assert_eq!(got, SOURCE, "the child wrote through the link");
    assert_eq!(mode(&fifo), Some(0o600), "the FIFO's mode was set");
    assert!(
        std::fs::symlink_metadata(&to).is_ok_and(|m| m.file_type().is_symlink()),
        "the link is left"
    );
    assert_eq!(
        refused.map_err(|e| e.kind()),
        Err(std::io::ErrorKind::InvalidData)
    );
    let plain = dir.path().join("plain");
    let copied = super::copy_regular_fresh(&from, &plain).ok();
    assert_eq!(copied, Some(SOURCE.len() as u64));
    assert_eq!(std::fs::read(&plain).ok().as_deref(), Some(SOURCE));
    assert_eq!(mode(&plain), Some(0o750));
}

/// Only an echo of its line is a `cat`'s proof that it runs: `cat` itself passes and runs
/// on until its stdin closes.
#[test]
fn only_an_echo_proves_a_cat_runs() {
    use crate::spawnfd::{Stdio, command};
    let piped = |program: &str, args: &[&str]| {
        command(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn")
    };
    let mut cat = piped("/bin/cat", &[]);
    let stdin = super::cat_echoes(&mut cat);
    assert_eq!(cat.try_wait().expect("poll"), None, "it runs on");
    drop(stdin);
    assert!(cat.wait().expect("reap").success(), "and ends at its EOF");
    for (program, args) in [
        ("/bin/sh", &["-c", "read line; echo other"][..]),
        ("/usr/bin/true", &[][..]),
    ] {
        let mut child = piped(program, args);
        let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            super::cat_echoes(&mut child)
        }));
        assert!(refused.is_err(), "{program} {args:?} is no proof");
        assert!(
            child.try_wait().expect("poll").is_some(),
            "{program}: killed and reaped"
        );
    }
    let mut unpiped = command("/bin/cat")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .spawn()
        .expect("spawn");
    let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        super::cat_echoes(&mut unpiped)
    }));
    assert!(refused.is_err(), "no pipes, no proof");
    assert!(
        unpiped.try_wait().expect("poll").is_some(),
        "killed and reaped"
    );
}

/// A copied program whose output ends before its line is named with how it ended, its own exit
/// status or the signal that ended it, so a failure says whether something else killed it.
#[test]
fn a_copied_program_that_ends_first_is_named_with_how_it_ended() {
    use crate::spawnfd::{Stdio, command};
    for (script, said) in [
        ("exit 3", "(exit status: 3)"),
        ("kill -9 $$", "(signal: 9 (SIGKILL))"),
    ] {
        let mut child = command("/bin/sh")
            .args(["-c", script])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn");
        let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            super::prints(&mut child, "parked", super::ECHO_HANG);
        }))
        .expect_err("no proof");
        let words = refused
            .downcast_ref::<String>()
            .map(String::as_str)
            .unwrap_or_default();
        assert!(
            words.ends_with(&format!("its output ended first {said}")),
            "{script}: {words}"
        );
    }
}

/// Only the line itself is a copied program's proof that it runs: a child that writes other
/// lines first and then the line passes, and its output after it is read on, so a child
/// that writes a megabyte more still ends.
#[test]
fn only_its_line_proves_a_copied_program_runs() {
    use crate::spawnfd::{Stdio, command};
    let piped = |script: &str| {
        command("/bin/sh")
            .args(["-c", script])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn")
    };
    let mut child = piped("echo running 1 test; echo parked; head -c 1048576 /dev/zero");
    super::prints(&mut child, "parked", super::ECHO_HANG);
    let hang = std::time::Instant::now() + super::ECHO_HANG;
    while child.try_wait().expect("poll").is_none() {
        assert!(
            std::time::Instant::now() < hang,
            "its output after the line was never read"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    for (script, hang) in [
        ("echo parked-not; echo other", super::ECHO_HANG),
        ("exec sleep 60", std::time::Duration::from_millis(200)),
    ] {
        let mut child = piped(script);
        let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            super::prints(&mut child, "parked", hang);
        }));
        assert!(refused.is_err(), "{script} is no proof");
        assert!(
            child.try_wait().expect("poll").is_some(),
            "{script}: killed and reaped"
        );
    }
    let mut unpiped = command("/bin/sh")
        .args(["-c", "echo parked"])
        .stdout(Stdio::null())
        .spawn()
        .expect("spawn");
    let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        super::prints(&mut unpiped, "parked", super::ECHO_HANG);
    }));
    assert!(refused.is_err(), "no pipe, no proof");
    assert!(
        unpiped.try_wait().expect("poll").is_some(),
        "killed and reaped"
    );
}

/// A directory of copies goes when dropped only once they ran: dropped by a failing test's
/// unwind it stays, and dropped by a test that never said so it stays and fails that test.
#[test]
fn a_directory_of_copies_goes_only_once_they_ran() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let laid = |name: &str| {
        let dir = tmp.path().join(name);
        std::fs::create_dir_all(dir.join("bin")).expect("the directory");
        std::fs::write(dir.join("bin").join("copy"), b"copied").expect("the copy");
        dir
    };
    let proven = laid("proven");
    let mut kept = super::KeptUntilRan::new(&proven);
    assert_eq!(kept.path(), proven);
    kept.ran();
    drop(kept);
    assert!(!proven.exists(), "its copies ran: it goes");

    let failing = laid("failing");
    let unwound = std::panic::catch_unwind(|| {
        let _kept = super::KeptUntilRan::new(&failing);
        panic!("a hang detector gave up");
    });
    assert!(unwound.is_err());
    assert!(
        failing.join("bin").join("copy").is_file(),
        "a failing test's unwind keeps it"
    );

    let forgot = laid("forgot");
    let refused = std::panic::catch_unwind(|| drop(super::KeptUntilRan::new(&forgot)))
        .expect_err("a test that never said its copies ran fails");
    let why = refused
        .downcast_ref::<String>()
        .expect("the guard panics with its words");
    assert!(why.contains(&forgot.display().to_string()), "{why}");
    assert!(
        forgot.join("bin").join("copy").is_file(),
        "and keeps the directory"
    );
}

/// A signing that panics leaves no stand-in, so the next ask signs again.
#[test]
fn a_refused_signing_leaves_no_stand_in() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let cache = dir.path().join("cache");
    let binary = dir.path().join("tool");
    std::fs::write(&binary, b"bytes").expect("the binary");
    let refused = std::panic::catch_unwind(|| {
        super::signed_stand_in(&cache, &binary, &mut |_| panic!("codesign refused"));
    });
    assert!(refused.is_err());
    assert_eq!(
        std::fs::read_dir(&cache).unwrap().count(),
        0,
        "nothing left"
    );
    let mut asked = 0;
    super::signed_stand_in(&cache, &binary, &mut |_| asked += 1);
    assert_eq!(asked, 1, "the next ask signs");
}

/// Whether `pid` is this test's own child and has not exited: a `WNOWAIT` `waitid`, which
/// reads alike on every Unix and reaps nothing.
fn runs_as_ours(pid: u32) -> bool {
    // SAFETY: every field of `siginfo_t` is an integer or a pointer, so all-zero is valid.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY: a non-blocking `WNOWAIT` `waitid` of one pid into the live local `info`.
    let rc = unsafe {
        libc::waitid(
            libc::P_PID,
            libc::id_t::from(pid),
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    // SAFETY: `info` was zeroed, then filled by `waitid` or, with no exit to report under
    // `WNOHANG`, left with `si_pid` 0.
    rc == 0 && unsafe { info.si_pid() } == 0
}

/// Whether `pid` is no child of this test's any more: reaped, so `waitid` answers `ECHILD`.
fn reaped(pid: u32) -> bool {
    // SAFETY: as in `runs_as_ours`.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY: as in `runs_as_ours`.
    let rc = unsafe {
        libc::waitid(
            libc::P_PID,
            libc::id_t::from(pid),
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    rc == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD)
}

/// THE PROOF LINE AT ONE TEST THREAD: a test binary run again at `--test-threads=1` writes
/// `test <name> ... ` unended before its test's first line, so the proof follows it on that
/// line; the line alone is proof too. NEGATIVE CONTROLS: a line that only ends with the proof,
/// one where the proof is followed by more, and the bare prefix with no name are not.
#[test]
fn a_proof_line_counts_alone_and_after_libtests_one_thread_prefix() {
    let proof = b"appended";
    assert!(super::is_proof(b"appended", proof));
    assert!(super::is_proof(
        b"test tests::tables::child_append ... appended",
        proof
    ));
    for not in [
        &b"not appended"[..],
        b"xappended",
        b"test tests::tables::child_append ... appended ok",
        b"test  ... appended",
        b"test a ...appended",
        b"",
    ] {
        assert!(
            !super::is_proof(not, proof),
            "{}",
            String::from_utf8_lossy(not)
        );
    }
}

/// The pid a run wrote to `mark` (`echo $$ > mark`), once it has written all of it, within
/// the one-minute hang detector.
fn wrote_pid(mark: &std::path::Path) -> Option<u32> {
    let by = std::time::Instant::now() + super::ECHO_HANG;
    loop {
        let read = std::fs::read_to_string(mark).unwrap_or_default();
        if let Some(pid) = read.strip_suffix('\n') {
            return pid.parse().ok();
        }
        if std::time::Instant::now() >= by {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// A child the program check holds past the hang detector ends the test COULD NOT RUN,
/// naming it, and is killed and reaped; one that is not held, or unread, is still the
/// test's own failure. (`sleep` writes nothing, so the 100 ms runs out by construction.)
/// The reader is asked about the child itself while it still runs: the fake answers only
/// for the child's pid while it is this test's unexited child (`runs_as_ours`).
#[test]
fn a_child_held_at_the_program_check_ends_could_not_run_and_others_still_hang() {
    use crate::spawnfd::{Stdio, command};
    let ends = |held: Option<bool>| {
        let mut child = command("/bin/sleep")
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn");
        let pid = child.id();
        let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            super::prints_judged(
                &mut child,
                "ran",
                std::time::Duration::from_millis(100),
                &|asked| {
                    (asked == pid && runs_as_ours(asked))
                        .then_some(held)
                        .flatten()
                },
            );
        }))
        .expect_err("a silent child ends the test");
        let why = refused
            .downcast_ref::<String>()
            .expect("the wait panics with its words")
            .clone();
        assert!(
            child.try_wait().expect("poll").is_some(),
            "{held:?}: killed and reaped"
        );
        (pid, why)
    };
    let (pid, why) = ends(Some(true));
    assert!(
        why.starts_with(crate::host::COULD_NOT_RUN),
        "expected COULD NOT RUN, got: {why}"
    );
    assert!(why.contains(&format!("(pid {pid})")), "{why}");
    for held in [Some(false), None] {
        let (pid, why) = ends(held);
        assert!(
            why.starts_with(&format!("the copied program (pid {pid}) never wrote")),
            "{held:?}: {why}"
        );
    }
}

/// A run that outlives its hang detector ends the test: COULD NOT RUN, naming it, while the
/// program check held it then, and otherwise the test's own failure, naming it with what it
/// wrote; either way it is killed and reaped. A run that ends hands back its status and
/// output. Both the `spawnfd` run and a test's own `std` launch are judged so. The run
/// writes a line, then its pid, then sleeps past the 100 ms by construction. The fake
/// reads the pid the run wrote (within the one-minute hang detector) and answers only while
/// the pid it was given is this test's unexited child, so it is asked about the run itself,
/// while it still runs.
#[test]
fn a_run_past_its_hang_detector_ends_the_test_and_one_held_could_not_run() {
    const RUN: &str = "echo ran; echo $$ > \"$0\"; exec /bin/sleep 30";
    let hang = std::time::Duration::from_millis(100);
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let ends = |held: Option<bool>, by_std: bool| {
        let mark = dir.path().join(format!("pid-{held:?}-{by_std}"));
        let asked = std::cell::Cell::new(None);
        let fake = |pid| {
            asked.set(Some((pid, wrote_pid(&mark))));
            runs_as_ours(pid).then_some(held).flatten()
        };
        let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if by_std {
                let how = super::Overrun {
                    held: &fake,
                    kill: super::kill_child,
                    reap: super::ECHO_HANG,
                };
                super::std_ends_within_by(
                    std::process::Command::new("/bin/sh")
                        .args(["-c", RUN])
                        .arg(&mark),
                    hang,
                    &how,
                );
            } else {
                let how = super::Overrun {
                    held: &fake,
                    kill: crate::deadline::children::kill_group,
                    reap: super::ECHO_HANG,
                };
                super::ends_within_by(
                    crate::spawnfd::command("/bin/sh")
                        .args(["-c", RUN])
                        .arg(&mark),
                    hang,
                    &how,
                );
            }
        }))
        .expect_err("a run past its hang detector ends the test");
        let why = refused
            .downcast_ref::<String>()
            .expect("the wait panics with its words")
            .clone();
        let (pid, its) = asked
            .get()
            .expect("the run was asked about at its hang detector");
        assert_eq!(
            Some(pid),
            its,
            "{held:?}, std {by_std}: the run itself was asked"
        );
        assert!(reaped(pid), "{held:?}, std {by_std}: killed and reaped");
        (pid, why)
    };
    for by_std in [false, true] {
        let (pid, why) = ends(Some(true), by_std);
        assert!(
            why.starts_with(crate::host::COULD_NOT_RUN) && why.contains(&format!("(pid {pid})")),
            "std {by_std}: expected COULD NOT RUN naming it, got: {why}"
        );
        for held in [Some(false), None] {
            let (pid, why) = ends(held, by_std);
            assert!(
                why.starts_with(&format!("/bin/sh (pid {pid}) did not finish in time"))
                    && why.contains("\nstdout: ran\n"),
                "{held:?}, std {by_std}: {why}"
            );
        }
    }
    let shell = "echo out; echo err >&2; exit 3";
    for out in [
        super::ends_within(
            crate::spawnfd::command("/bin/sh").args(["-c", shell]),
            super::ECHO_HANG,
        ),
        super::std_ends_within(
            std::process::Command::new("/bin/sh").args(["-c", shell]),
            super::ECHO_HANG,
        ),
    ] {
        assert_eq!(
            (out.status.code(), &out.stdout[..], &out.stderr[..]),
            (Some(3), &b"out\n"[..], &b"err\n"[..])
        );
    }
}

/// A run its kill does not end (whether a program macOS's program check holds takes its
/// `SIGKILL` is unmeasured) still ends the test, once its reap has been waited on for its
/// bound: the kill here sends nothing, and the outer wait is the one-minute hang detector.
/// The run, which nothing signalled, is then killed by its pid: this test's own unexited
/// child, which the fake was asked about. NEGATIVE CONTROL: with the reap after the kill
/// the unbounded wait it was, the run outlives the hang detector and the test fails.
#[test]
fn a_run_its_kill_does_not_end_still_ends_the_test_at_its_hang_detector() {
    let (tell, asked) = std::sync::mpsc::channel();
    let (tx, ended) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let held = |pid| {
            let _ = tell.send(pid);
            runs_as_ours(pid).then_some(true)
        };
        let waited = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            super::ends_within_by(
                crate::spawnfd::command("/bin/sleep").arg("120"),
                std::time::Duration::from_millis(100),
                &super::Overrun {
                    held: &held,
                    kill: |_| {},
                    reap: std::time::Duration::from_millis(100),
                },
            );
        }));
        let _ = tx.send(waited.map_err(|why| why.downcast_ref::<String>().cloned()));
    });
    let pid = asked
        .recv_timeout(super::ECHO_HANG)
        .expect("the run was asked about at its hang detector");
    let waited = ended.recv_timeout(super::ECHO_HANG);
    if runs_as_ours(pid) {
        let _ = crate::process::kill_group_until_empty(i32::try_from(pid).expect("a pid"));
    }
    let why = waited
        .expect("the wait on a run its kill does not end ended within the one-minute hang detector")
        .expect_err("a run past its hang detector ends the test")
        .expect("the wait panics with its words");
    assert!(
        why.starts_with(crate::host::COULD_NOT_RUN) && why.contains(&format!("(pid {pid})")),
        "{why}"
    );
}

/// A run that cannot start is the test's own failure, naming the program and why, never a
/// COULD NOT RUN, for the `spawnfd` run and a test's own `std` launch alike.
#[test]
fn a_run_that_cannot_start_fails_its_test_naming_it() {
    const MISSING: &str = "/nonexistent/aterm-uds-no-such-program";
    let refused = |run: &dyn Fn()| {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(run))
            .expect_err("a run that cannot start ends the test")
            .downcast_ref::<String>()
            .expect("the wait panics with its words")
            .clone()
    };
    let mut refusals = vec![refused(&|| {
        super::ends_within(&mut crate::spawnfd::command(MISSING), super::ECHO_HANG);
    })];
    // Under a translator std's glibc posix_spawn loses the exec error (the store to
    // its CLONE_VM-shared args is dropped, measured on the x86_64 cell under
    // Rosetta): the spawn reports Ok and the child exits 127, so a std run cannot
    // fail to start there. The transparent arm above still must, and does.
    if crate::process::translator().is_none() {
        refusals.push(refused(&|| {
            super::std_ends_within(&mut std::process::Command::new(MISSING), super::ECHO_HANG);
        }));
    }
    for why in refusals {
        assert!(
            why.starts_with(&format!("{MISSING} could not start: ")),
            "{why}"
        );
    }
}

/// The trustd skip counts this user's held programs beside the silence when there are
/// any, and says nothing of them when there are none or they could not be counted.
#[test]
fn the_trust_skip_counts_the_wait_without_blaming_it() {
    use crate::process::HeldNow;
    let plain = super::trust_silent_words(None);
    assert!(!plain.contains("program check"), "{plain}");
    assert_eq!(super::trust_silent_words(Some(HeldNow::default())), plain);
    let two = HeldNow {
        count: 2,
        oldest_ms: 35_940_000,
    };
    assert_eq!(
        super::trust_silent_words(Some(two)),
        format!("{plain}; 2 programs of this user wait for macOS's program check now")
    );
    assert_eq!(
        super::trust_silent_words(Some(HeldNow { count: 1, ..two })),
        format!("{plain}; 1 program of this user waits for macOS's program check now")
    );
}

/// The silence ends the test COULD NOT RUN; an answer returns.
#[test]
fn a_silent_disk_image_service_ends_the_test_could_not_run() {
    super::require_disk_image_service(&run(1, "hdiutil: No space left on device\n"));
    let refused = std::panic::catch_unwind(|| {
        super::require_disk_image_service(&run(1, "hdiutil: Operation timed out\n"));
    })
    .expect_err("a timed-out run ends the test");
    let why = refused
        .downcast_ref::<String>()
        .expect("could_not_run panics with its words");
    assert!(why.starts_with(crate::host::COULD_NOT_RUN), "{why}");
}
