// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn live_exclusive_guards_serialize_independent_opens_and_preserve_the_inode() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("claim.lock");
    let live = Live::new();
    let held = live.exclusive(&path, Duration::ZERO).unwrap();
    let identity = live.file_id(&path).unwrap();
    let Err(error) = Live::new().exclusive(&path, Duration::ZERO) else {
        panic!("independent open must not acquire the held inode");
    };
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    drop(held);
    let _reclaimed = Live::new().exclusive(&path, Duration::ZERO).unwrap();
    assert_eq!(live.file_id(&path).unwrap(), identity);
}

#[test]
fn a_fake_exclusive_guard_excludes_until_drop_without_blocking_other_paths() {
    let sys = Fake::default();
    let path = Path::new("/state/claim.lock");
    let held = sys.exclusive(path, Duration::ZERO).unwrap();
    let Err(refused) = sys.exclusive(path, Duration::ZERO) else {
        panic!("the path is held");
    };
    assert_eq!(refused.kind(), io::ErrorKind::TimedOut);
    let other = sys
        .exclusive(Path::new("/state/other.lock"), Duration::ZERO)
        .unwrap();
    drop(held);
    let reclaimed = sys.exclusive(path, Duration::ZERO).unwrap();
    assert!(sys.exclusive(path, Duration::ZERO).is_err());
    drop((reclaimed, other));
    assert!(sys.exclusive(path, Duration::ZERO).is_ok());
}

/// A user file is written only at a plain path strictly inside the user's home with
/// permission bits alone: `..` out of the home, the home itself, a `/` home and a setuid
/// mode are refused before anything is written. NEGATIVE CONTROL: a file under a
/// symbolically linked `~/.config` (a dotfile manager's) is written with its mode.
#[test]
fn a_user_file_outside_the_home_or_with_more_than_permission_bits_is_refused() {
    let fake = Fake::root();
    let alice = User {
        name: "alice".into(),
        uid: 502,
        gid: 20,
        home: "/Users//alice".into(),
        shell: "/bin/zsh".into(),
        admin: false,
    };
    let root_home = User {
        home: "/".into(),
        ..alice.clone()
    };
    for (user, path, mode, kind) in [
        (
            &alice,
            "/Users//alice/../bob/x",
            0o600,
            io::ErrorKind::PermissionDenied,
        ),
        (
            &alice,
            "/Users//alice",
            0o600,
            io::ErrorKind::PermissionDenied,
        ),
        (
            &alice,
            "Users/alice/x",
            0o600,
            io::ErrorKind::PermissionDenied,
        ),
        (
            &alice,
            "/Users//alice2/x",
            0o600,
            io::ErrorKind::PermissionDenied,
        ),
        (
            &root_home,
            "/etc/sudoers.d/x",
            0o600,
            io::ErrorKind::PermissionDenied,
        ),
        (
            &alice,
            "/Users//alice/x",
            0o4755,
            io::ErrorKind::InvalidInput,
        ),
        (
            &alice,
            "/Users//alice/x",
            0o1777,
            io::ErrorKind::InvalidInput,
        ),
    ] {
        let refused = fake
            .write_user_file(user, Path::new(path), b"x", mode)
            .unwrap_err();
        assert_eq!(refused.kind(), kind, "{path} {mode:o}: {refused}");
    }
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    fake.link("/Users//alice/.config", "/Users//alice/dotfiles/config");
    fake.write_user_file(&alice, Path::new("/Users//alice/.config/x"), b"x", 0o644)
        .unwrap();
    assert_eq!(fake.calls(), ["alice$ write /Users//alice/.config/x 644"]);
    assert_eq!(fake.mode_of("/Users//alice/.config/x"), Some(0o644));
}

#[test]
fn child_failures_preserve_spawn_post_start_and_timeout_phases() {
    use aterm_uds::deadline::{CompleteOutputError, RunError};
    let budget = Duration::from_secs(60);
    assert!(matches!(
        RunFailed::from_child(RunError::Spawn(io::ErrorKind::NotFound.into()), budget),
        RunFailed::Spawn(_)
    ));
    for error in [
        RunError::Capture(io::ErrorKind::NotFound.into()),
        RunError::Wait(io::ErrorKind::NotFound.into()),
    ] {
        let failed = RunFailed::from_child(error, budget);
        assert!(matches!(failed, RunFailed::AfterStart(_)), "{failed:?}");
        assert!(failed.to_string().starts_with("failed after start:"));
    }
    assert_eq!(
        RunFailed::from_child(RunError::TimedOut, budget),
        RunFailed::TimedOut(budget)
    );
    for error in [
        CompleteOutputError::StdoutRead(io::ErrorKind::UnexpectedEof.into()),
        CompleteOutputError::StdoutTooLarge { limit: 3 },
        CompleteOutputError::StdoutIncomplete,
    ] {
        assert!(matches!(
            RunFailed::from_complete_output(error, budget),
            RunFailed::AfterStart(_)
        ));
    }
    assert!(matches!(
        RunFailed::from_complete_output(
            CompleteOutputError::Run(RunError::Spawn(io::ErrorKind::NotFound.into())),
            budget
        ),
        RunFailed::Spawn(_)
    ));
    assert_eq!(
        RunFailed::from_complete_output(CompleteOutputError::Run(RunError::TimedOut), budget),
        RunFailed::TimedOut(budget)
    );
}

/// The window's walk takes a sign-out's Accessibility ask only when its answer names that
/// page; `OK declined` or a `not-now:accessibility` word is the owner's Not now on it; a walk
/// under way over other pages (or one within the last minute, `OK recent`) has its page up,
/// beside which macOS's own dialog would be a second question. NEGATIVE CONTROL: `OK
/// granted` and an answer from a window older than the walk leave the ask to macOS.
#[test]
fn the_windows_answer_tells_a_walk_that_takes_the_ask_from_a_not_now_and_other_pages() {
    use AccessibilityWalk::{NotNow, NotTaken, OtherPages, Taken};
    for (reply, want) in [
        ("OK walking accessibility\n", Taken),
        ("OK walking fda accessibility", Taken),
        ("OK walking fda not-now:accessibility\n", NotNow),
        ("OK declined\n", NotNow),
        ("OK walking fda devtools\n", OtherPages),
        ("OK walking devtools not-now:fda\n", OtherPages),
        ("OK recent\n", OtherPages),
        ("OK granted\n", NotTaken),
        ("ERR unknown verb (try: help)\n", NotTaken),
        (
            "ERR headless: a headless instance brings no System Settings page up\n",
            NotTaken,
        ),
    ] {
        assert_eq!(walk_answer(reply, "accessibility"), want, "{reply}");
    }
}

#[cfg(unix)]
#[test]
fn complete_output_refuses_a_diagnostic_tail_that_would_change_root_admission() {
    let sh = aterm_uds::deadline::stand_in(Path::new("/bin/sh"));
    let dd = aterm_uds::deadline::stand_in(Path::new("/bin/dd"));
    let script = "printf 'Developer mode is currently enabled.\\n' >&2; \
        \"$1\" if=/dev/zero bs=\"$2\" count=1 1>&2 2>/dev/null; \
        printf '\\nDeveloper mode is currently disabled.\\n' >&2";
    let command = Cmd::new(sh)
        .args(["-c", script, "status-stand-in"])
        .arg(dd)
        .arg(aterm_uds::deadline::OUTPUT_KEEP.to_string())
        .budget(Duration::from_secs(60));
    let live = Live::new();
    let tail = live.run_complete_stdout(&command).unwrap();
    assert_eq!(tail.status, Some(0));
    assert_eq!(
        crate::root_steps::developer_mode(&tail),
        Some(false),
        "negative control: the diagnostic tail lost the contradictory earlier status"
    );
    assert!(
        matches!(live.run_complete_output(&command), Err(RunFailed::AfterStart(why)) if why.contains("stderr exceeds"))
    );
}

#[test]
fn complete_stdout_admits_scripted_whole_records_and_refuses_unproven_capture() {
    let sys = Fake::root();
    let cmd = Cmd::new("/synthetic-record");
    let record = "header\nfirst row\nlast row\n";
    sys.on(&cmd.display(), Ran::ok(record));
    assert_eq!(
        sys.run_complete_stdout(&cmd).unwrap().stdout,
        record.as_bytes()
    );
    sys.refuse_complete_stdout(&cmd.display(), "synthetic stdout did not reach EOF");
    let why = sys.run_complete_stdout(&cmd).unwrap_err().to_string();
    assert!(why.contains("did not reach EOF"), "{why}");
    assert_eq!(
        sys.run(&cmd).unwrap().stdout,
        record.as_bytes(),
        "diagnostic lane is unchanged"
    );
    let kept = cmd.clone().until_deadline();
    let before = sys.calls().len();
    assert!(
        sys.run_complete_stdout(&kept)
            .unwrap_err()
            .to_string()
            .contains("partial output")
    );
    assert_eq!(
        sys.calls().len(),
        before,
        "strict refusal precedes execution"
    );
}

#[test]
fn complete_stdout_accepts_its_exact_bound_and_refuses_one_byte_more() {
    let sys = Fake::root();
    let cmd = Cmd::new("/synthetic-bound");
    let mut ran = Ran {
        status: Some(0),
        stdout: vec![b'x'; aterm_uds::deadline::OUTPUT_KEEP],
        stderr: b"diagnostic".to_vec(),
    };
    sys.on(&cmd.display(), ran.clone());
    ran.stdout.push(b'y');
    sys.on(&cmd.display(), ran);
    let exact = sys.run_complete_stdout(&cmd).unwrap();
    assert_eq!(exact.stdout.len(), aterm_uds::deadline::OUTPUT_KEEP);
    assert_eq!(exact.stderr, b"diagnostic");
    let why = sys.run_complete_stdout(&cmd).unwrap_err().to_string();
    assert!(why.contains("complete stdout exceeds"), "{why}");
}

#[test]
fn a_word_is_quoted_only_when_a_shell_would_read_it_specially() {
    assert_eq!(shell_word("/usr/bin/defaults"), "/usr/bin/defaults");
    assert_eq!(shell_word("com.apple.dock"), "com.apple.dock");
    assert_eq!(shell_word("Assistant Enabled"), "'Assistant Enabled'");
    assert_eq!(shell_word("it's"), r"'it'\''s'");
    assert_eq!(shell_word(""), "''");
    assert_eq!(shell_word("$HOME"), "'$HOME'");
}

fn alice() -> User {
    User {
        name: "alice".into(),
        uid: 501,
        gid: 20,
        home: "/Users//alice".into(),
        shell: "/bin/zsh".into(),
        admin: false,
    }
}

#[test]
fn fake_recursive_creation_recreates_removed_directories_and_preserves_user_ownership() {
    let who = alice();
    let mut sys = Fake::root();
    sys.euid = who.uid;
    let home = Path::new(&who.home);
    let home_stat = Stat {
        uid: who.uid,
        gid: who.gid,
        mode: 0o755,
        dir: true,
        symlink: false,
        len: 0,
    };
    sys.stat_of(home.to_str().unwrap(), home_stat);
    let state = home.join("Library/Application Support/aterm/init-activation");
    sys.create_dir_all(&state, 0o700).unwrap();
    assert_eq!(
        sys.stat(home).unwrap(),
        home_stat,
        "mkdir must preserve existing directory metadata"
    );
    let directory = sys.stat(&state).unwrap();
    assert!(directory.dir && !directory.symlink);
    assert_eq!(
        (directory.uid, directory.gid, directory.mode),
        (who.uid, who.gid, 0o700)
    );
    sys.remove(&state).unwrap();
    assert_eq!(
        sys.stat(&state).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
    sys.create_dir_all(&state, 0o700).unwrap();
    assert_eq!(sys.stat(&state).unwrap(), directory);
    let marker = state.join("dock.pending");
    sys.write_user_file(&who, &marker, b"pending", 0o600)
        .unwrap();
    let stat = sys.stat(&marker).unwrap();
    assert_eq!(
        (stat.uid, stat.gid, stat.mode, stat.len),
        (who.uid, who.gid, 0o600, 7)
    );
    sys.remove(&marker).unwrap();
    assert_eq!(
        sys.stat(&marker).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );

    let conflict = home.join("conflict");
    sys.file(
        conflict.to_str().unwrap(),
        b"a file cannot contain a child directory",
    );
    assert!(sys.create_dir_all(&conflict.join("child"), 0o700).is_err());
    assert!(sys.stat(&conflict.join("child")).is_err());
}

#[test]
fn display_names_the_user_and_quotes_each_word() {
    let cmd = Cmd::new("/usr/bin/defaults")
        .args([
            "write",
            "com.apple.assistant.support",
            "Assistant Enabled",
            "-bool",
            "false",
        ])
        .as_user(&alice());
    assert_eq!(
        cmd.display(),
        "alice$ /usr/bin/defaults write com.apple.assistant.support 'Assistant Enabled' -bool false"
    );
}

#[test]
fn from_root_a_user_run_scrubs_the_environment_and_a_session_run_goes_through_launchctl() {
    let (program, args) = Live::argv_from(
        0,
        &Cmd::new("/usr/bin/open")
            .arg("u")
            .as_user(&alice())
            .in_session(),
    )
    .unwrap();
    assert_eq!(program, PathBuf::from("/bin/launchctl"));
    let args: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        args,
        [
            "asuser",
            "501",
            "/usr/bin/sudo",
            "-n",
            "-u",
            "alice",
            "-H",
            "--",
            "/usr/bin/env",
            "-i",
            "HOME=/Users//alice",
            "USER=alice",
            "LOGNAME=alice",
            "SHELL=/bin/zsh",
            "PATH=/usr/bin:/bin:/usr/sbin:/sbin",
            "LANG=en_US.UTF-8",
            "/usr/bin/open",
            "u"
        ]
    );
}

#[test]
fn without_root_a_run_as_another_user_is_refused_before_anything_starts() {
    let euid = 502;
    let mut other = alice();
    other.uid = euid + 1;
    let err = Live::argv_from(euid, &Cmd::new("/usr/bin/true").as_user(&other)).unwrap_err();
    assert!(matches!(err, RunFailed::Refused(_)), "{err:?}");
    let (program, args) = Live::argv_from(
        euid,
        &Cmd::new("/usr/bin/true").as_user(&User {
            uid: euid,
            ..alice()
        }),
    )
    .unwrap();
    assert_eq!(program, PathBuf::from("/usr/bin/true"));
    assert!(args.is_empty(), "{args:?}");
}

#[test]
fn a_run_as_this_processs_own_account_gets_that_accounts_home_and_names() {
    let live = Live::new();
    if live.euid() == 0 {
        aterm_uds::host::could_not_run(
            "as root a run as this account is wrapped; the unwrapped run needs a non-root uid",
        );
    }
    let me = User {
        name: "lab-person".into(),
        uid: live.euid(),
        gid: 20,
        home: "/nonexistent/lab-person".into(),
        shell: String::new(),
        admin: false,
    };
    let cmd = Cmd::new("/usr/bin/env").as_user(&me).env("GIT_X", "1");
    let (program, args) = live.argv(&cmd).unwrap();
    assert_eq!(program, PathBuf::from("/usr/bin/env"));
    assert!(args.is_empty(), "{args:?}");
    for cmd in [cmd.clone(), cmd.until_deadline()] {
        let ran = live.run(&cmd).unwrap();
        assert!(ran.success(), "{ran:?}");
        let out = ran.out();
        let mut lines: Vec<&str> = out.lines().collect();
        lines.sort_unstable();
        assert_eq!(
            lines,
            [
                "GIT_X=1",
                "HOME=/nonexistent/lab-person",
                "LANG=en_US.UTF-8",
                "LOGNAME=lab-person",
                "PATH=/usr/bin:/bin:/usr/sbin:/sbin",
                "SHELL=/bin/zsh",
                "USER=lab-person",
            ],
            "{}",
            cmd.display()
        );
    }
}

#[test]
fn display_names_each_added_variable_and_masks_its_value() {
    let cmd = Cmd::new("/usr/bin/ssh-add")
        .secret_env("SSH_AUTH_SOCK", "/private/agent-secret.sock")
        .args(["-l"]);
    assert_eq!(cmd.display(), "SSH_AUTH_SOCK=\u{2026} /usr/bin/ssh-add -l");
    assert!(!cmd.display().contains("agent-secret"));
    assert!(!format!("{cmd:?}").contains("agent-secret"), "{cmd:?}");
}

/// The live system reads this machine in process: its logical cores, and the volume holding
/// the home it is given, never smaller than what is free on it. CONTROL: asked for no home,
/// it reads no volume.
#[test]
fn the_live_system_reads_this_machine_and_the_homes_volume() {
    let home = aterm_tempfile::tempdir().unwrap();
    let live = Live::new();
    let reading = live
        .machine_reading(Some(home.path()))
        .expect("the live system reads the machine");
    assert!(reading.cores.is_some_and(|n| n > 0), "{reading:?}");
    let volume = reading.volume.expect("the home's volume");
    assert!(
        volume.total > 0 && volume.total >= volume.free,
        "{volume:?}"
    );
    assert_eq!(live.machine_reading(None).and_then(|r| r.volume), None);
}

#[test]
fn a_new_file_is_ordered_and_a_copy_durable() {
    use aterm_durable::{Flush, seam::Recording};
    let dir = aterm_tempfile::tempdir().unwrap();
    let live = Live::new();
    let recording = Recording::start();
    live.create_new(&dir.path().join("run.lock"), b"1 2\n", 0o600)
        .unwrap();
    assert_eq!(recording.take(), [Flush::Ordered], "a new file");
    live.copy_file(
        &dir.path().join("run.lock"),
        &dir.path().join("copy"),
        0o600,
    )
    .unwrap();
    assert_eq!(recording.take(), [Flush::Durable], "a copy");
}

#[test]
fn a_relative_program_is_refused() {
    let live = Live::new();
    let err = live.run(&Cmd::new("defaults")).unwrap_err();
    assert!(matches!(err, RunFailed::Refused(_)), "{err:?}");
}

#[test]
fn a_terminal_prelude_runs_only_its_final_validated_command() {
    let live = Live::new();
    let dir = aterm_tempfile::tempdir().unwrap();
    let earlier = dir.path().join("earlier");
    let current = dir.path().join("current");
    std::fs::create_dir(&earlier).unwrap();
    std::fs::create_dir(&current).unwrap();
    let mut initial = Cmd::new("/usr/bin/false")
        .arg("old argument")
        .env("ATERM_TTY_TEST_VALUE", "old value");
    initial.cwd = Some(earlier.clone());
    let unchanged = initial.clone();
    let mut final_command = Cmd::new("/bin/sh")
        .args([
            "-c",
            "{ printf '%s\\n%s\\n' \"$1\" \"$ATERM_TTY_TEST_VALUE\"; pwd -P; } > \"$2\"",
            "terminal-prelude",
            "new argument",
            "result.txt",
        ])
        .env("ATERM_TTY_TEST_VALUE", "new value");
    final_command.cwd = Some(current.clone());
    let spawns = super::spawns::Spawns::new(&live);
    let result = spawns.run_tty(&initial, &mut |command| {
        assert!(terminal_free().is_none(), "the prelude owns stdin");
        assert_eq!(*command, initial);
        *command = final_command.clone();
        Ok(())
    });
    assert_eq!(result.unwrap(), Some(0));
    assert_eq!(initial, unchanged, "the caller's command is immutable");
    assert_eq!(spawns.record(), [(final_command, None)]);
    assert_eq!(
        std::fs::read_to_string(current.join("result.txt")).unwrap(),
        format!(
            "new argument\nnew value\n{}\n",
            std::fs::canonicalize(&current).unwrap().display()
        )
    );
    assert!(!earlier.join("result.txt").exists());

    let mut would_write = Cmd::new("/bin/sh").args(["-c", "printf child > child.txt"]);
    would_write.cwd = Some(current.clone());
    for refused in [true, false] {
        let result = live.run_tty(&would_write, &mut |command| {
            assert!(terminal_free().is_none());
            if refused {
                return Err(RunFailed::Refused("the work is no longer owed".into()));
            }
            command.program = "relative-program".into();
            Ok(())
        });
        assert!(matches!(result, Err(RunFailed::Refused(_))), "{result:?}");
        assert!(!current.join("child.txt").exists());
    }
    if live.euid() != 0 {
        let mut other = alice();
        other.uid = live.euid() ^ 1;
        let result = live.run_tty(&would_write, &mut |command| {
            command.user = Some(other.clone());
            Ok(())
        });
        assert!(matches!(result, Err(RunFailed::Refused(_))), "{result:?}");
        assert!(!current.join("child.txt").exists());
    }
}

/// A `System` that never says where GitHub is reaches none of it: its agh client's transport
/// sends nothing, its endpoints are 127.0.0.1's port 1, and it finds no agh.toml. The
/// defaults are read through `Spawns`, a double that overrides neither method; a `Fake`
/// never given `with_github` or `with_agh_dirs` answers the same, and is checked beside it.
/// NEGATIVE CONTROL: `Live` sends to github.com.
#[test]
fn a_system_that_names_no_github_sends_nowhere_and_reads_no_agh_toml() {
    // A double that names no GitHub: its transport answers a round trip as not sent.
    fn sends_nowhere(sys: &dyn System) {
        let (transport, endpoints) = sys.github_transport();
        assert_eq!(endpoints, agh::wire::Endpoints::loopback(1, 1, 1, 1));
        let sent = transport.round_trip(agh::wire::WireRequest::new(
            agh::wire::Method::Get,
            endpoints.url(agh::wire::OriginRole::Api, "/user"),
            Vec::new(),
            1024,
            agh::wire::Deadlines::JSON,
        ));
        assert!(
            matches!(&sent, Err(e) if e.phase == agh::wire::Phase::NotSent),
            "{:?}",
            sent.err()
        );
        assert!(sys.agh_dirs().is_none());
    }
    let live = Live::new();
    sends_nowhere(&super::spawns::Spawns::new(&live));
    sends_nowhere(&Fake::default());
    assert_eq!(
        Live::new().github_transport().1,
        agh::wire::Endpoints::github_com()
    );
}

#[test]
fn a_run_kept_to_its_deadline_ends_what_its_child_started_too() {
    let live = Live::new();
    let ran = live
        .run(
            &Cmd::new("/bin/sh")
                .args(["-c", "/bin/sleep 120 & echo $!; wait"])
                .budget(Duration::from_secs(2))
                .until_deadline(),
        )
        .unwrap();
    assert_eq!(ran.status, None, "{ran:?}");
    let pid: u32 = ran
        .out()
        .trim()
        .parse()
        .expect("the shell printed its child's pid");
    let since = std::time::Instant::now();
    while aterm_uds::process::pid_alive(pid) {
        assert!(
            since.elapsed() < Duration::from_secs(60),
            "hang detector: {pid} outlived its run's deadline"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn owner_and_mode_are_set_through_the_file_and_never_through_a_link_or_a_second_name() {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    let live = Live::new();
    let dir = aterm_tempfile::tempdir().unwrap();
    let file = dir.path().join("prefs.plist");
    std::fs::write(&file, b"x").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    let meta = std::fs::metadata(&file).unwrap();
    live.set_owner_and_mode(&file, meta.uid(), meta.gid(), 0o644)
        .unwrap();
    assert_eq!(std::fs::metadata(&file).unwrap().mode() & 0o7777, 0o644);

    let link = dir.path().join("link.plist");
    std::os::unix::fs::symlink(&file, &link).unwrap();
    assert!(
        live.set_owner_and_mode(&link, meta.uid(), meta.gid(), 0o600)
            .is_err()
    );

    let second = dir.path().join("second.plist");
    std::fs::hard_link(&file, &second).unwrap();
    let err = live
        .set_owner_and_mode(&file, meta.uid(), meta.gid(), 0o600)
        .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{err}");
    assert_eq!(std::fs::metadata(&file).unwrap().mode() & 0o7777, 0o644);
}

#[test]
fn a_variable_set_on_a_run_shows_in_its_display_and_reaches_the_tool() {
    let cmd = Cmd::new("/usr/bin/env").env("HOMEBREW_NO_ANALYTICS", "1");
    assert_eq!(cmd.display(), "HOMEBREW_NO_ANALYTICS=1 /usr/bin/env");
    let ran = Live::new().run(&cmd).unwrap();
    assert!(ran.out().lines().any(|l| l == "HOMEBREW_NO_ANALYTICS=1"));
}

#[test]
fn write_atomic_replaces_a_link_rather_than_following_it() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    std::fs::write(&target, b"untouched").unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    write_atomic(&link, b"new", 0o600).unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"untouched");
    assert_eq!(std::fs::read(&link).unwrap(), b"new");
    assert!(
        !std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn the_running_app_is_the_bundle_the_resolved_executable_sits_in() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let app = root.join("Applications/aterm.app");
    let macos = app.join("Contents/MacOS");
    std::fs::create_dir_all(&macos).unwrap();
    std::fs::write(macos.join("aterm"), b"").unwrap();
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::os::unix::fs::symlink(macos.join("aterm"), root.join("bin/aterm")).unwrap();
    std::fs::write(root.join("bin/built"), b"").unwrap();
    let live = |exe: &str| Live {
        agent_exe: root.join(exe),
        ..Live::new()
    };
    assert_eq!(live("bin/aterm").running_app(), Some(app.clone()));
    assert_eq!(
        live("Applications/aterm.app/Contents/MacOS/aterm").running_app(),
        Some(app)
    );
    assert_eq!(live("bin/built").running_app(), None, "a build is no app");
    assert_eq!(live("bin/gone").running_app(), None);
}

/// [`Live::read_head`] reads at most `max` bytes of a regular file, and refuses a link at the
/// end of the path or on its way (a program linked out of its bundle, a bundle under a linked
/// folder), so it reads nothing outside the folders the path names, and refuses a FIFO at
/// once, never waiting for a writer, by this host's refusals (macOS's `ELOOP` and `EFTYPE`;
/// elsewhere `ELOOP` at the end, `ENOTDIR` on the way, and no regular file). NEGATIVE
/// CONTROLS (scratch): on macOS, a plain `std::fs::File::open` in place of `open_reader` reads
/// through both links and waits on the FIFO until the one-minute detector fails, and
/// `O_NONBLOCK` alone in its place reads the FIFO as an empty file; on Linux, a plain
/// `std::fs::File::open` in place of `open_components` reads through the link at the end;
/// reading with no `take` fails the 512-byte assertion.
#[test]
fn read_head_reads_a_bounded_head_of_a_regular_file_alone() {
    use std::os::unix::ffi::OsStrExt as _;
    #[cfg(target_os = "macos")]
    let refused_link = |_at_the_end: bool, error: &io::Error| aterm_uds::nofollow::is_link(error);
    #[cfg(target_os = "macos")]
    let not_regular = aterm_uds::nofollow::not_regular;
    #[cfg(not(target_os = "macos"))]
    let refused_link = |at_the_end: bool, error: &io::Error| {
        error.raw_os_error()
            == Some(if at_the_end {
                libc::ELOOP
            } else {
                libc::ENOTDIR
            })
    };
    #[cfg(not(target_os = "macos"))]
    let not_regular = |error: &io::Error| error.kind() == io::ErrorKind::InvalidInput;
    let temp = aterm_tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let live = Live::new();
    let program = root.join("program");
    std::fs::write(&program, [b'#'; 2000]).unwrap();
    assert_eq!(live.read_head(&program, 512).unwrap(), [b'#'; 512]);
    std::fs::write(root.join("outside"), b"#!/bin/zsh\n").unwrap();
    std::os::unix::fs::symlink(root.join("outside"), root.join("linked")).unwrap();
    std::fs::create_dir(root.join("real")).unwrap();
    std::fs::write(root.join("real/program"), b"#!/bin/zsh\n").unwrap();
    std::os::unix::fs::symlink(root.join("real"), root.join("folder")).unwrap();
    for (linked, at_the_end) in [("linked", true), ("folder/program", false)] {
        let read = live.read_head(&root.join(linked), 512);
        assert!(
            read.as_ref()
                .is_err_and(|error| refused_link(at_the_end, error)),
            "{linked}: {read:?}"
        );
    }
    let fifo = root.join("fifo");
    let c_fifo = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: a live owned C string names an absent file in this private fixture.
    assert_eq!(unsafe { libc::mkfifo(c_fifo.as_ptr(), 0o600) }, 0);
    let (send, receive) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let read = Live::new().read_head(&fifo, 512);
        let _ = send.send(read.map_err(|e| not_regular(&e)));
    });
    assert_eq!(
        receive
            .recv_timeout(std::time::Duration::from_secs(60))
            .expect("a FIFO's read must not wait for a writer"),
        Err(true)
    );
}

/// A live port whose [`System::download`] reads `fake`'s loopback origins.
fn live_reading(fake: &agh_fake::FakeGithub) -> Live {
    let ports = fake.ports();
    Live {
        release_origins: Some(aterm_update_core::github_rest::Origins::loopback(
            ports.api,
            ports.uploads,
            ports.web,
            ports.cdn,
        )),
        ..Live::new()
    }
}

fn api_reads(fake: &agh_fake::FakeGithub) -> usize {
    fake.seen()
        .iter()
        .filter(|s| s.origin == agh_fake::FakeOrigin::Api)
        .count()
}

#[test]
fn a_live_download_reads_a_release_asset_on_the_api_route_while_the_download_host_fails() {
    use agh_fake::{AssetSpec, FakeGithub, Fault, Match, ReleaseSpec, RepoSpec};
    const ZIP: &[u8] = b"the release zip";
    let fake = FakeGithub::start();
    fake.add_repo(
        "fixture-dev/app",
        RepoSpec::public().push("fixture-dev-bot"),
    );
    let id = fake.add_release(
        "fixture-dev/app",
        ReleaseSpec::published("v1", "2026-10-01T00:00:00Z"),
    );
    fake.add_asset(id, AssetSpec::uploaded("app.zip", ZIP));
    let live = live_reading(&fake);
    let url = "https://github.com/fixture-dev/app/releases/download/v1/app.zip";
    let dir = aterm_tempfile::tempdir().unwrap();
    let dest = dir.path().join("payload");

    // Negative control: while the download host serves, the API route is never asked.
    live.download(url, &dest, 1 << 20).unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), ZIP);
    assert_eq!(api_reads(&fake), 0);

    fake.fault_always(
        Match::web("/fixture-dev/app/releases/"),
        Fault::Status {
            status: 504,
            body: String::new(),
            headers: Vec::new(),
        },
    );
    std::fs::remove_file(&dest).unwrap();
    live.download(url, &dest, 1 << 20).unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), ZIP);
    assert!(api_reads(&fake) >= 2, "the release, then the asset by id");
    assert_eq!(live.download_bytes(url, 1 << 20).unwrap(), ZIP);

    // An asset the release lacks is absent, not a stalled read of both routes.
    let missing = "https://github.com/fixture-dev/app/releases/download/v1/other.zip";
    let why = live
        .download(missing, &dir.path().join("other"), 1 << 20)
        .unwrap_err();
    assert!(
        why.ends_with("the API's release lists no asset of that name"),
        "{why}"
    );
    assert!(!dir.path().join("other").exists());
}
