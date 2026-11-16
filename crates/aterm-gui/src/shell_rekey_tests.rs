// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_core::terminal::{ShellIntegrationPosture, Terminal};
use std::sync::Mutex;

fn scratch(tag: &str) -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix(tag)
        .tempdir()
        .expect("scratch")
}

/// The key lands 0600 with its newline, replacing a stale key, and a planted
/// symlink at the path is replaced without writing through it.
#[test]
fn delivery_is_private_exclusive_and_never_through_a_symlink() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = scratch("rk-deliver");
    let names = || {
        let mut names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    };
    let path = dir.path().join("s-ab");
    std::fs::write(&path, "stale\n").unwrap();
    deliver(&path, "aa").unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "aa\n");
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    assert_eq!(names(), ["s-ab"]);

    let victim = dir.path().join("victim");
    std::fs::write(&victim, "original").unwrap();
    std::fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(&victim, &path).unwrap();
    deliver(&path, "bb").unwrap();
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "original");
    assert!(
        std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_file()
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "bb\n");

    let occupied = dir.path().join("s-cd");
    std::fs::create_dir(&occupied).unwrap();
    std::fs::write(occupied.join("x"), "").unwrap();
    assert!(deliver(&occupied, "cc").is_err());
    assert_eq!(names(), ["s-ab", "s-cd", "victim"]);
}

/// A command cycle (prompt, command line, start) signed with `id` (`";id=<hex>"`, or `""` for
/// none).
fn cycle(id: &str) -> String {
    format!(
        "\x1b]133;A{id}\x07$ \x1b]633;E;claude --resume{id}\x07\x1b]133;B{id}\x07\
         claude --resume\n\x1b]133;C{id}\x07"
    )
}

/// The carry of a parent that lost the shell's nonce: the requirement
/// crosses in `modes`, the nonce does not.
fn carry_without_nonce() -> aterm_core::terminal::TerminalCheckpoint {
    let mut parent = Terminal::new(24, 80);
    parent.set_require_shell_integration_nonce(true);
    let carry = parent.checkpoint_carry(0).expect("Ground");
    assert_eq!(carry.shell_integration_nonce, None);
    carry
}

/// The key the adoption wrote, as the shell's `__aterm_rekey_check` takes it.
fn key_in(path: &Path) -> String {
    let key = std::fs::read_to_string(path).expect("the key file");
    let key = key.trim_end().to_string();
    assert_eq!(key.len(), 64, "{key}");
    key
}

/// A session with the channel, adopted with its nonce requirement and no nonce (or no
/// screen), gets a key in its file from `spawn::hydrate_adopted_engine`.
#[test]
fn the_adoption_rekeys_a_degraded_shell_that_has_the_channel() {
    let dir = scratch("rk-heal");
    for (label, carry) in [
        ("carried screen", Some(carry_without_nonce())),
        ("no screen", None),
    ] {
        let path = dir.path().join("s-ab");
        let term = std::sync::Mutex::new(Terminal::new(24, 80));
        crate::spawn::hydrate_adopted_engine(
            &term,
            carry.as_ref(),
            None,
            Some(&path),
            None,
            1,
            &mut crate::handoff_history::AdoptedHistory::default(),
        );
        let key = key_in(&path);
        let mut engine = term.lock().unwrap();
        assert_eq!(
            engine.shell_integration_posture(),
            ShellIntegrationPosture::Degraded,
            "{label}: handed over, not yet taken"
        );
        engine.process(cycle("").as_bytes());
        engine.process(cycle(&format!(";id={}", "1".repeat(64))).as_bytes());
        assert_eq!(
            crate::session_status::executing_detail(
                &engine,
                &crate::command_ran::Ran::running(&["claude"])
            ),
            None,
            "{label}: unsigned and old-key marks stay dropped"
        );
        assert_eq!(
            engine.shell_integration_posture(),
            ShellIntegrationPosture::Degraded,
            "{label}"
        );
        engine.process(cycle(&format!(";id={key}")).as_bytes());
        assert_eq!(
            crate::session_status::executing_detail(
                &engine,
                &crate::command_ran::Ran::running(&["claude"])
            )
            .as_deref(),
            Some("claude"),
            "{label}"
        );
        assert_eq!(
            engine.shell_integration_posture(),
            ShellIntegrationPosture::On,
            "{label}: the first signed mark is the shell taking the key"
        );
        drop(engine);
        let _ = std::fs::remove_file(&path);
    }
}

/// Under an overlap handoff the old key stays in the file through an abandoned
/// attempt, and the Commit writes the new one, which the engine (authorized at
/// adoption) verifies.
#[test]
fn an_uncommitted_adoption_holds_its_key_until_the_commit() {
    let dir = scratch("rk-deferred");
    let path = dir.path().join("s-ab");
    let old_key = "7".repeat(64);
    deliver(&path, &old_key).expect("the old process's own waiting key");
    let term = std::sync::Mutex::new(Terminal::new(24, 80));
    let queue = Deferred::holding();
    crate::spawn::hydrate_adopted_engine(
        &term,
        Some(&carry_without_nonce()),
        None,
        Some(&path),
        Some(&queue),
        1,
        &mut crate::handoff_history::AdoptedHistory::default(),
    );
    assert_eq!(key_in(&path), old_key, "held: the old key still waits");
    drop(queue);
    assert_eq!(key_in(&path), old_key, "abandoned: nothing was written");
    let term = std::sync::Mutex::new(Terminal::new(24, 80));
    let queue = Deferred::holding();
    crate::spawn::hydrate_adopted_engine(
        &term,
        Some(&carry_without_nonce()),
        None,
        Some(&path),
        Some(&queue),
        1,
        &mut crate::handoff_history::AdoptedHistory::default(),
    );
    assert_eq!(key_in(&path), old_key, "held until the Commit");
    queue.release();
    let key = key_in(&path);
    assert_ne!(key, old_key, "the Commit wrote the successor's key");
    let mut engine = term.lock().unwrap();
    engine.process(cycle(&format!(";id={key}")).as_bytes());
    assert_eq!(
        crate::session_status::executing_detail(
            &engine,
            &crate::command_ran::Ran::running(&["claude"])
        )
        .as_deref(),
        Some("claude"),
        "authorized at adoption, taken after the Commit"
    );
    drop(engine);
    let later = dir.path().join("s-cd");
    queue.hold(mint(&later, 2).0);
    assert!(later.exists(), "released: written at once");
}

/// Negative controls: no channel or an unwritable path stays `degraded` without authorizing a
/// key, and a carried nonce stays `on` without writing a file.
#[test]
fn the_adoption_writes_no_key_where_none_is_wanted_or_deliverable() {
    let dir = scratch("rk-controls");
    let path = dir.path().join("s-ab");

    let term = std::sync::Mutex::new(Terminal::new(24, 80));
    crate::spawn::hydrate_adopted_engine(
        &term,
        Some(&carry_without_nonce()),
        None,
        None,
        None,
        1,
        &mut crate::handoff_history::AdoptedHistory::default(),
    );
    assert!(!path.exists());
    assert_eq!(
        term.lock().unwrap().shell_integration_posture(),
        ShellIntegrationPosture::Degraded,
        "no channel: degraded, and says so"
    );

    let unreachable = dir.path().join("missing-dir").join("s-ab");
    let term = std::sync::Mutex::new(Terminal::new(24, 80));
    crate::spawn::hydrate_adopted_engine(
        &term,
        Some(&carry_without_nonce()),
        None,
        Some(&unreachable),
        None,
        1,
        &mut crate::handoff_history::AdoptedHistory::default(),
    );
    assert_eq!(
        term.lock().unwrap().shell_integration_posture(),
        ShellIntegrationPosture::Degraded,
        "no key delivered, so none authorized"
    );

    let mut parent = Terminal::new(24, 80);
    parent.set_require_shell_integration_nonce(true);
    parent.authorize_shell_integration([7; 32]);
    let carried = parent.checkpoint_carry(0).expect("Ground");
    let term = std::sync::Mutex::new(Terminal::new(24, 80));
    crate::spawn::hydrate_adopted_engine(
        &term,
        Some(&carried),
        None,
        Some(&path),
        None,
        1,
        &mut crate::handoff_history::AdoptedHistory::default(),
    );
    assert!(!path.exists(), "a carried nonce needs no re-key");
    assert_eq!(
        term.lock().unwrap().shell_integration_posture(),
        ShellIntegrationPosture::On
    );
}

/// An adopted shell whose nonce did not cross the update, from a build with
/// no re-key channel: `integration=degraded`.
fn lost_nonce_engine() -> Mutex<Terminal> {
    let mut t = Terminal::new(24, 80);
    t.restore_checkpoint(&carry_without_nonce());
    assert_eq!(
        t.shell_integration_posture(),
        ShellIntegrationPosture::Degraded
    );
    Mutex::new(t)
}

/// Test session `n`'s tab ID (the `t` form), in a range of its own so no other test's typed
/// re-key shares its entry.
fn sid(n: u32) -> SessionId {
    SessionId::new(aterm_types::tab_id::TabId::from_bits(0x7e_0000 + u64::from(n)).as_str())
}

/// A degraded tab gets a private one-use `<sid>.<n>` file beside the channel's `<sid>`,
/// holding a fresh key.
#[test]
fn a_typed_rekey_heals_a_degraded_tab_from_its_first_signed_mark() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = scratch("rk-typed");
    let (term, sid) = (lost_nonce_engine(), sid(1));
    let (path, serial) = typed::issue_in(dir.path(), &term, &sid).expect("issued");
    assert_eq!(path, dir.path().join(format!("{}.{serial}", sid.as_str())));
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let key = key_in(&path);
    let mut engine = term.lock().unwrap();
    assert_eq!(
        engine.shell_integration_posture(),
        ShellIntegrationPosture::Degraded,
        "issued, not yet taken"
    );
    engine.process(b"\x1b]133;D;0\x07");
    engine.process(cycle("").as_bytes());
    engine.process(cycle(&format!(";id={}", "1".repeat(64))).as_bytes());
    assert_eq!(
        crate::session_status::executing_detail(
            &engine,
            &crate::command_ran::Ran::running(&["claude"])
        ),
        None
    );
    assert_eq!(
        engine.shell_integration_posture(),
        ShellIntegrationPosture::Degraded
    );
    engine.process(cycle(&format!(";id={key}")).as_bytes());
    assert_eq!(
        crate::session_status::executing_detail(
            &engine,
            &crate::command_ran::Ran::running(&["claude"])
        )
        .as_deref(),
        Some("claude")
    );
    assert_eq!(
        engine.shell_integration_posture(),
        ShellIntegrationPosture::On
    );
    drop(engine);
    std::fs::remove_file(&path).unwrap();
    assert_eq!(
        typed::settle_issued(&term, &sid, None),
        typed::Settled::Taken
    );
}

/// A healthy tab, a tab with integration off, or a tab with a pending channel key is refused
/// with nothing written.
#[test]
fn a_typed_rekey_is_refused_where_nothing_is_lost() {
    let dir = scratch("rk-typed-refused");
    let written = || std::fs::read_dir(dir.path()).unwrap().count();

    let mut healthy = Terminal::new(24, 80);
    healthy.set_require_shell_integration_nonce(true);
    healthy.authorize_shell_integration([9; 32]);
    let healthy = Mutex::new(healthy);
    assert_eq!(
        typed::issue_in(dir.path(), &healthy, &sid(2)).err(),
        Some(typed::Refusal::Healthy)
    );
    let off = Mutex::new(Terminal::new(24, 80));
    assert_eq!(
        typed::issue_in(dir.path(), &off, &sid(2)).err(),
        Some(typed::Refusal::Off)
    );
    assert_eq!(written(), 0, "nothing written for either");

    let term = lost_nonce_engine();
    deliver(&dir.path().join(sid(2).as_str()), &"5".repeat(64)).unwrap();
    assert_eq!(
        typed::issue_in(dir.path(), &term, &sid(2)).err(),
        Some(typed::Refusal::Pending)
    );
    assert_eq!(written(), 1, "the channel's key alone");
    assert_eq!(
        term.lock().unwrap().shell_integration_posture(),
        ShellIntegrationPosture::Degraded
    );
    assert_eq!(
        typed::issue_in(dir.path(), &term, &SessionId::new("../x")).err(),
        Some(typed::Refusal::NoDir),
        "a sid naming no file of ours"
    );
}

/// A healthy shell that is behind (signing no revision, or an older build's) is issued
/// the key it already signs with plus this build's folder and pointer path; nothing on
/// the engine changes.
#[test]
fn a_shell_behind_this_build_is_issued_the_upgrade_healthy_or_not() {
    use aterm_core::shell_integration::hex_encode;
    let dir = scratch("rk-typed-upgrade");
    let up = typed::Upgrade {
        folder: PathBuf::from("/c/shell-integration/0123456789abcdef"),
        pointer: PathBuf::from("/c/integration/s-04"),
    };
    let key = [9u8; 32];
    let healthy = |signed: Option<&str>| {
        let mut t = Terminal::new(24, 80);
        t.set_require_shell_integration_nonce(true);
        t.authorize_shell_integration(key);
        if let Some(rev) = signed {
            t.process(
                format!(
                    "\x1b]633;P;AtermIntegration={rev};id={}\x07",
                    hex_encode(&key)
                )
                .as_bytes(),
            );
        }
        Mutex::new(t)
    };
    let lines = |key: &str| format!("{key}\n{}\n{}\n", up.folder.display(), up.pointer.display());
    let in_use = |term: &Mutex<Terminal>| {
        let engine = term.lock().unwrap();
        (
            engine.shell_integration_posture(),
            engine.shell_integration_nonce_in_use(),
        )
    };

    for signed in [None, Some("fedcba9876543210")] {
        let term = healthy(signed);
        for read in [false, true] {
            let (path, _) = typed::issue_upgrading_in(dir.path(), &term, &sid(40), Some(&up))
                .unwrap_or_else(|no| panic!("{signed:?}: {no:?}"));
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                lines(&hex_encode(&key)),
                "{signed:?}"
            );
            assert_eq!(in_use(&term), (ShellIntegrationPosture::On, Some(key)));
            if read {
                std::fs::remove_file(&path).unwrap();
            }
            assert_eq!(
                typed::settle(&term, &sid(40)),
                if read {
                    typed::Settled::Taken
                } else {
                    typed::Settled::Withdrawn
                }
            );
            assert!(!path.exists());
            assert_eq!(
                in_use(&term),
                (ShellIntegrationPosture::On, Some(key)),
                "the key the shell signs with never moved"
            );
        }
    }

    let written = || std::fs::read_dir(dir.path()).unwrap().count();
    assert_eq!(
        typed::issue_upgrading_in(
            dir.path(),
            &healthy(Some(crate::shell_body::own_rev())),
            &sid(41),
            Some(&up)
        )
        .err(),
        Some(typed::Refusal::Healthy),
        "a shell that signs this build's revision runs this build's body"
    );
    assert_eq!(
        typed::issue_in(dir.path(), &healthy(None), &sid(41)).err(),
        Some(typed::Refusal::Healthy),
        "no upgrade to hand: the key-only form never touches a healthy tab"
    );
    assert_eq!(written(), 0, "nothing written for either");

    let term = lost_nonce_engine();
    let (path, _) =
        typed::issue_upgrading_in(dir.path(), &term, &sid(42), Some(&up)).expect("issued");
    let text = std::fs::read_to_string(&path).unwrap();
    let fresh = text.lines().next().unwrap().to_string();
    assert_eq!(fresh.len(), 64);
    assert_eq!(text, lines(&fresh), "a fresh key, then the two lines");
    term.lock()
        .unwrap()
        .process(cycle(&format!(";id={fresh}")).as_bytes());
    assert_eq!(
        term.lock().unwrap().shell_integration_posture(),
        ShellIntegrationPosture::On,
        "the fresh key verifies the shell's marks"
    );
    std::fs::remove_file(&path).unwrap();
    assert_eq!(typed::settle(&term, &sid(42)), typed::Settled::Taken);
}

/// A key whose line never ran is taken back with its file, so a mark signed with it is
/// dropped.
#[test]
fn a_typed_rekey_whose_line_never_ran_is_taken_back() {
    let dir = scratch("rk-typed-withdraw");
    let (term, sid) = (lost_nonce_engine(), sid(3));
    let (first, first_serial) = typed::issue_in(dir.path(), &term, &sid).expect("issued");
    let first_key = key_in(&first);
    let (second, second_serial) = typed::issue_in(dir.path(), &term, &sid).expect("issued");
    assert!(!first.exists(), "the unread first key's file went with it");
    assert_ne!(key_in(&second), first_key);
    assert_eq!(
        typed::settle_issued(&term, &sid, Some(first_serial)),
        typed::Settled::None
    );
    assert!(second.exists());
    let second_key = key_in(&second);
    assert_eq!(
        typed::settle_issued(&term, &sid, Some(second_serial)),
        typed::Settled::Withdrawn
    );
    assert!(!second.exists(), "the one-use file does not linger");
    let mut engine = term.lock().unwrap();
    for key in [&first_key, &second_key] {
        let dropped = engine.shell_integration_dropped_count();
        engine.process(format!("\x1b]133;D;0;id={key}\x07").as_bytes());
        assert_eq!(
            engine.shell_integration_dropped_count(),
            dropped + 1,
            "a withdrawn key verifies nothing"
        );
    }
    assert_eq!(
        engine.shell_integration_posture(),
        ShellIntegrationPosture::Degraded
    );
    drop(engine);
    assert_eq!(typed::settle(&term, &sid), typed::Settled::None, "once");

    let (read, _) = typed::issue_in(dir.path(), &term, &sid).expect("issued");
    let key = key_in(&read);
    std::fs::remove_file(&read).unwrap();
    assert_eq!(typed::settle(&term, &sid), typed::Settled::Taken);
    let mut engine = term.lock().unwrap();
    engine.process(cycle(&format!(";id={key}")).as_bytes());
    assert_eq!(
        engine.shell_integration_posture(),
        ShellIntegrationPosture::On
    );
    drop(engine);

    let term = lost_nonce_engine();
    let (waiting, _) = typed::issue_in(dir.path(), &term, &sid).expect("issued");
    forget_typed(&sid);
    assert!(!waiting.exists());
    assert_eq!(typed::settle(&term, &sid), typed::Settled::None);
}

/// Leftovers from an exited window (`<sid>.<n>` files or symlinks) are swept by the next typed
/// re-key of the same session, leaving the channel's own file and other sessions' files alone.
#[test]
fn a_typed_rekey_sweeps_what_a_dead_window_left_and_nothing_else() {
    let dir = scratch("rk-typed-sweep");
    let (term, sid) = (lost_nonce_engine(), sid(4));
    let stale = dir
        .path()
        .join(format!("{}.{}", sid.as_str(), u64::MAX - 1));
    std::fs::write(&stale, "stale\n").unwrap();
    let victim = dir.path().join("victim");
    std::fs::write(&victim, "original").unwrap();
    let planted = dir.path().join(format!("{}.{}", sid.as_str(), u64::MAX));
    std::os::unix::fs::symlink(&victim, &planted).unwrap();
    let other = dir.path().join(format!("{}.9", sid_of_other().as_str()));
    std::fs::write(&other, "theirs\n").unwrap();
    let (path, _) = typed::issue_in(dir.path(), &term, &sid).expect("issued");
    assert!(!stale.exists() && std::fs::symlink_metadata(&planted).is_err());
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "original");
    assert!(
        other.exists(),
        "another session's file is not ours to sweep"
    );
    assert!(path.exists());
    forget_typed(&sid);
}

fn sid_of_other() -> SessionId {
    sid(0xffff)
}

/// Two concurrent issues on one tab settle the first and leave no live key in a file once the
/// second is settled.
#[test]
fn two_issues_at_once_leave_no_key_in_a_file() {
    use std::sync::{Arc, Barrier};
    let dir = scratch("rk-typed-race");
    let sid = sid(5);
    let (mut left, mut live) = (0, 0);
    for _ in 0..50 {
        let term = Arc::new(lost_nonce_engine());
        let together = Arc::new(Barrier::new(2));
        let issue = || {
            let (term, together) = (Arc::clone(&term), Arc::clone(&together));
            let (dir, sid) = (dir.path().to_path_buf(), sid.clone());
            std::thread::spawn(move || {
                together.wait();
                typed::issue_in(&dir, &term, &sid).is_ok()
            })
        };
        let (a, b) = (issue(), issue());
        assert!(a.join().unwrap() && b.join().unwrap(), "both issued");
        assert_eq!(typed::settle(&term, &sid), typed::Settled::Withdrawn);
        for file in std::fs::read_dir(dir.path()).unwrap().flatten() {
            left += 1;
            let key = key_in(&file.path());
            let mut engine = term.lock().unwrap();
            let dropped = engine.shell_integration_dropped_count();
            engine.process(format!("\x1b]133;A;id={key}\x07").as_bytes());
            if engine.shell_integration_dropped_count() == dropped {
                live += 1;
            }
            drop(engine);
            std::fs::remove_file(file.path()).unwrap();
        }
    }
    assert_eq!((left, live), (0, 0), "(files left, live keys in them)");
}

/// A SESSION'S FILES GO AGAIN ONCE ITS SHELL HAS (unit BASH32H): a shell outlives its session for
/// as long as its hang-up takes, and a bash hook that runs then makes `<sid>.out` anew, so the
/// session's drop hands a second removal to the reaper ([`discard_once_gone`]), which asks
/// whether the shell is gone ([`aterm_pty::ShellIdentity::verify`], a stand-in child here): the
/// re-key file and both companions stay through a look that found it alive, and all three go once
/// it is reaped. NEGATIVE CONTROL, scratch: with [`remove_once`] removing at its first look
/// whatever `gone` answers, the files are gone after a look that found the shell alive.
#[test]
fn a_sessions_files_go_again_once_its_shell_has() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    const GAP: std::time::Duration = std::time::Duration::from_millis(10);
    let dir = scratch("rk-gone");
    let path = dir.path().join("s-7e0001");
    let files: Vec<PathBuf> = [
        "",
        aterm_core::shell_integration::DEBUG_TRAP_SUFFIX,
        aterm_core::shell_integration::OUTPUT_SUFFIX,
    ]
    .into_iter()
    .map(|suffix| {
        let mut file = path.as_os_str().to_owned();
        file.push(suffix);
        PathBuf::from(file)
    })
    .collect();
    for file in &files {
        std::fs::write(file, "").expect("a session file");
    }
    let mut command = aterm_uds::spawnfd::command("/bin/sleep");
    command.arg("60");
    let mut shell = command.spawn().expect("a stand-in shell");
    let pid = i32::try_from(shell.id()).expect("a pid");
    let identity = aterm_pty::ShellIdentity::from_parts(pid, aterm_pty::process_birth(pid), true);
    let alive_looks = Arc::new(AtomicUsize::new(0));
    let looks = Arc::clone(&alive_looks);
    remove_once(path.clone(), move || {
        let alive = identity.verify();
        if alive {
            looks.fetch_add(1, Ordering::AcqRel);
        }
        !alive
    });
    let looked = aterm_spec::wait::within(aterm_spec::wait::MUST_HAPPEN, GAP, || {
        (alive_looks.load(Ordering::Acquire) > 0).then_some(())
    });
    assert!(looked.is_some(), "the reaper never looked at the shell");
    for file in &files {
        assert!(
            file.exists(),
            "{}: removed while the shell lived",
            file.display()
        );
    }
    // SAFETY: `kill` takes no pointer; `pid` is this test's own child, unreaped until the wait.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
    shell.wait().expect("the stand-in reaped");
    let gone = aterm_spec::wait::within(aterm_spec::wait::MUST_HAPPEN, GAP, || {
        files.iter().all(|file| !file.exists()).then_some(())
    });
    assert!(
        gone.is_some(),
        "left after the shell went: {:?}",
        files
            .iter()
            .filter(|file| file.exists())
            .collect::<Vec<_>>()
    );
}

/// The re-key path of a tab an earlier build opened is `<control dir>/rekey/<its s- id>`, the
/// very path that build's shell captured as `ATERM_REKEY_PATH`, so a degraded adoption's re-key
/// (`spawn::hydrate_adopted_engine`, handed the path `path_for` gives the adopted ID) writes the
/// key where the shell reads it. A `t` tab's path is its own file, which that shell never reads;
/// a malformed ID of either form names no file at all.
#[test]
fn an_earlier_tabs_rekey_lands_where_its_shell_reads() {
    let Some(_env) = crate::seamless::tests::env_alone() else {
        return;
    };
    let _scratch = crate::seamless::tests::ScratchEnv::runtime("rk-earlier");
    let earlier = SessionId::parse("s-1a2b3c4d5e6f70819203").expect("an earlier tab's id");
    let control = crate::control_auth::socket_dir().expect("scratch control dir");
    let captured = control.join("rekey").join("s-1a2b3c4d5e6f70819203");
    let path = path_for(&earlier).expect("the rekey dir");
    assert_eq!(path, captured, "the path the earlier shell captured");
    let t = SessionId::parse("tqn5r6e0148").expect("a t tab's id");
    let minted = path_for(&t).expect("the rekey dir");
    assert_eq!(minted, control.join("rekey").join("tqn5r6e0148"));
    assert_ne!(minted, captured, "a t ID names another file");
    for malformed in ["s-../x", "s-", "t000000000", "T0000000000", "tqn5r6e014u"] {
        assert_eq!(
            path_for(&SessionId::new(malformed)),
            None,
            "{malformed} names no file"
        );
    }

    let term = Mutex::new(Terminal::new(24, 80));
    crate::spawn::hydrate_adopted_engine(
        &term,
        Some(&carry_without_nonce()),
        None,
        Some(&path),
        None,
        1,
        &mut crate::handoff_history::AdoptedHistory::default(),
    );
    let key = key_in(&captured);
    assert!(!minted.exists(), "nothing is written under another ID");
    let mut engine = term.lock().unwrap();
    engine.process(cycle(&format!(";id={key}")).as_bytes());
    assert_eq!(
        engine.shell_integration_posture(),
        ShellIntegrationPosture::On,
        "the shell that read its captured path is healed"
    );
    drop(engine);
    discard(&earlier);
    assert!(!captured.exists(), "discard finds the s- file too");
}
