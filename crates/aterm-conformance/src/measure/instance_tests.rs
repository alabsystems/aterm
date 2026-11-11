// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_reaped_instance_never_lists_helpers_under_its_released_pid() {
    let program = aterm_uds::deadline::stand_in(Path::new("/usr/bin/true"));
    let mut child = spawnfd::command(program)
        .spawn()
        .expect("start the stand-in");
    aterm_uds::deadline::kill_within(&mut child, MUST_HAPPEN);
    assert!(child.try_wait().expect("the exit status").is_some());
    end_with_helpers_using(&mut child, |_| panic!("the pid no longer proves ownership"));
}

#[test]
fn a_log_tail_keeps_recent_lines_and_malformed_bytes() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let path = dir.path().join("instance.log");
    let mut bytes = (0..40)
        .map(|n| format!("line {n}\n"))
        .collect::<String>()
        .into_bytes();
    bytes.extend_from_slice(b"failure: \xff\n");
    std::fs::write(&path, bytes).expect("write log");
    let tail = read_log_tail(&path);
    assert_eq!(tail.lines().count(), 30);
    assert!(tail.starts_with("line 11\n"), "{tail}");
    assert!(tail.ends_with("failure: \u{fffd}"), "{tail}");
}

#[test]
fn a_log_tail_bounds_a_single_oversized_line() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let path = dir.path().join("instance.log");
    let mut bytes = vec![b'x'; 128 * 1024];
    bytes.extend_from_slice(b"failure at the end");
    std::fs::write(&path, bytes).expect("write log");
    let tail = read_log_tail(&path);
    assert!(tail.ends_with("failure at the end"));
    assert!(
        tail.len() < 65 * 1024,
        "a long line must not escape the bound"
    );
}

#[test]
fn an_overdue_command_service_refuses_new_work() {
    static GATE: aterm_uds::deadline::Gate =
        aterm_uds::deadline::Gate::new("measure-command-overdue-test");
    let (release, held) = std::sync::mpsc::channel::<()>();
    let pending = GATE
        .start(Duration::ZERO, move || held.recv())
        .expect("start a held operation");
    let program = aterm_uds::deadline::stand_in(Path::new("/usr/bin/true"));
    let result = command_until(
        spawnfd::command(program),
        Instant::now() + MUST_HAPPEN,
        &GATE,
    );
    drop(release);
    assert!(pending.wait(MUST_HAPPEN).is_ok(), "release the helper");
    assert!(matches!(result, Err(CouldNotRun::Refused(_))), "{result:?}");
}

#[test]
fn first_exec_rejects_an_expired_warmup_deadline() {
    let program = aterm_uds::deadline::stand_in(Path::new("/bin/echo"));
    let command = spawnfd::command(program);
    assert!(matches!(
        first_exec_until(command, Instant::now()),
        Err(CouldNotRun::Refused(_))
    ));
}

#[test]
fn first_exec_accepts_completion_and_reports_a_missing_program() {
    let program = aterm_uds::deadline::stand_in(Path::new("/usr/bin/false"));
    assert!(first_exec(&program).is_ok(), "warm-up accepts completion");
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    assert!(matches!(
        first_exec(&scratch.path().join("missing")),
        Err(CouldNotRun::Refused(_))
    ));
}

#[test]
fn a_spent_budget_gives_up_at_once_but_still_asks_the_probe_once() {
    let spent = Budget::from_now(0);
    assert_eq!(spent.left(), None);
    assert_eq!(spent.await_event("x", || Some(7)), Ok(7));
    let mut asked = 0;
    let got = spent.await_event("x", || {
        asked += 1;
        None::<()>
    });
    assert!(got.is_err());
    assert_eq!(asked, 1);
}

#[test]
fn a_live_budget_leaves_the_moment_the_event_lands() {
    let budget = Budget::from_now(60);
    assert!(budget.left().is_some());
    let mut asked = 0;
    let got = budget.await_event("x", || {
        asked += 1;
        (asked == 3).then_some(asked)
    });
    assert_eq!(got, Ok(3));
}

/// The measure world's `aterm.toml` sets the audible bell off once, ahead
/// of every table, even when the head it is given opens one (the cold
/// streamer's `[sparkle_words]`): a `bell_sound` line after a table header
/// would be that table's key, not the root one aterm reads.
#[test]
fn a_measure_world_starts_with_the_audible_bell_off() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let head = "cursor_trail_style = \"off\"\n[sparkle_words]\nenabled = false";
    let world = World::prepare(dir.path(), head).expect("world");
    let text = std::fs::read_to_string(world.root.join("cfg/aterm/aterm.toml")).expect("written");
    let keys = |key: &str| {
        text.lines()
            .filter(|l| l.split('=').next().map(str::trim) == Some(key))
            .count()
    };
    assert_eq!(text.lines().next(), Some("bell_sound = false"), "{text}");
    assert_eq!(keys("bell_sound"), 1, "one bell key: {text}");
    assert!(text.contains(head), "the head follows: {text}");
    assert!(
        text.ends_with(aterm_types::world::LANES_OFF),
        "the lanes-off tables close it: {text}"
    );
}

/// Each entry of [`aterm_types::world::LANES_OFF`] that `doc` does not hold with the same
/// value, as `table.key`.
fn lanes_left_on(doc: &aterm_toml::Value) -> Vec<String> {
    let off = aterm_toml::from_str::<aterm_toml::Value>(aterm_types::world::LANES_OFF)
        .expect("the lanes-off tables are one document");
    let mut on = Vec::new();
    for (table, keys) in off.as_table().expect("a document is a table") {
        for (key, value) in keys.as_table().expect("each entry is a table") {
            if doc.get(table).and_then(|held| held.get(key)) != Some(value) {
                on.push(format!("{table}.{key}"));
            }
        }
    }
    on
}

/// A measured instance of the handed bundle (`/Applications/aterm (dev).app`: a release
/// build, no development seams) starts, unless its world says otherwise, the lanes that act
/// for the login's own account: its window's `_alab._tcp` browse and converge runner, a use
/// of the person's real LAN, and a keeper job in the
/// login's launchd domain. The world says otherwise in both places those lanes read: its
/// `aterm.toml`, read back as a document, holds every lanes-off entry after a head that opens
/// a table of its own, and its environment carries the scratch world's marker at the state
/// folder under its HOME.
#[test]
fn a_measure_world_starts_no_lane_of_the_logins_account() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let head = "cursor_trail_style = \"off\"\n[sparkle_words]\nenabled = false";
    let world = World::prepare(dir.path(), head).expect("world");
    let text = std::fs::read_to_string(world.root.join("cfg/aterm/aterm.toml")).expect("written");
    let doc = aterm_toml::from_str::<aterm_toml::Value>(&text).expect("one document");
    assert_eq!(lanes_left_on(&doc), Vec::<String>::new(), "{text}");
    let state = aterm_types::world::state_home(&world.home).expect("a state folder");
    let marker = world
        .env()
        .into_iter()
        .find(|(name, _)| *name == aterm_types::world::MARKER)
        .map(|(_, value)| value);
    assert_eq!(marker, Some(state.into_os_string()));
}

#[test]
fn a_verb_the_subject_lacks_is_told_from_other_refusals() {
    let reply = |stdout: &str, stderr: &str| Reply {
        code: Some(1),
        stdout: stdout.into(),
        stderr: stderr.into(),
    };
    let unknown = reply("ERR unknown verb (try: help)\n", "");
    assert_eq!(
        unknown.refuses_verb("rewind"),
        Some("ERR unknown verb (try: help)")
    );
    let unknown = reply("", "aterm-ctl: ERR unknown verb (try: help)\n");
    assert_eq!(
        unknown.refuses_verb("rewind"),
        Some("ERR unknown verb (try: help)")
    );
    assert!(
        reply("ERR aos needs a debug build\n", "")
            .refuses_verb("aos")
            .is_some()
    );
    for other in [
        reply("ERR aos bundle needs a debug build\n", ""),
        reply("ERR aos attach refused: no window\n", ""),
        reply("", "aterm-ctl: connect: No such file or directory\n"),
    ] {
        assert_eq!(other.refuses_verb("aos"), None, "{other:?}");
    }
}

/// A refused verb is unanswered; any other failure is refused.
#[test]
fn a_request_the_subject_has_no_verb_for_is_unanswered() {
    let client = |bin: &str| Ctl {
        bin: PathBuf::from(bin),
        sock: PathBuf::from("/nonexistent.sock"),
        home: PathBuf::from("/"),
        budget: Budget::from_now(60),
    };
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let stand_in = dir.path().join("refuses");
    std::fs::write(
        &stand_in,
        "#!/bin/sh\necho 'aterm-ctl: ERR unknown verb (try: help)' >&2\nexit 1\n",
    )
    .expect("write");
    std::fs::set_permissions(
        &stand_in,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .expect("chmod");
    let refused = client(stand_in.to_str().expect("utf-8")).ok(&["rewind", "open"]);
    assert!(
        matches!(&refused, Err(CouldNotRun::Unanswered(why)) if why.contains("ctl rewind open")),
        "{refused:?}"
    );
    let failed = client("/usr/bin/false").ok(&["rewind", "open"]);
    assert!(matches!(failed, Err(CouldNotRun::Refused(_))), "{failed:?}");
}

#[test]
fn a_client_whose_budget_is_spent_runs_nothing() {
    let client = |secs| Ctl {
        bin: PathBuf::from("/usr/bin/true"),
        sock: PathBuf::from("/nonexistent.sock"),
        home: PathBuf::from("/"),
        budget: Budget::from_now(secs),
    };
    assert_eq!(client(60).call(&["status"]).code, Some(0));
    assert!(client(60).ok(&["status"]).is_ok());
    assert_eq!(client(0).call(&["status"]).code, None);
    assert!(client(0).ok(&["status"]).is_err());
    assert!(Budget::from_now(u64::MAX).left().is_some());
}

#[test]
fn replay_checks_judges_before_reading_status() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let log = dir.path().join("calls.log");
    let stand_in = dir.path().join("temporal-ctl");
    std::fs::write(
        &stand_in,
        format!(
            "#!/bin/sh\nshift 5\necho \"$*\" >> '{}'\nif [ \"$*\" = 'temporal status' ]; then\n  echo 'OK recording=on witnessed=7 diverged=0 pending=0 unjudged_evicted=2'\nelse\n  echo 'OK judged=7 diverged=0 pending=0 unjudged_evicted=2'\nfi\n",
            log.display()
        ),
    )
    .expect("write stand-in");
    std::fs::set_permissions(
        &stand_in,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .expect("chmod");
    let ctl = Ctl {
        bin: stand_in,
        sock: PathBuf::from("/nonexistent.sock"),
        home: PathBuf::from("/"),
        budget: Budget::from_now(60),
    };
    assert_eq!(
        replay_checks(&ctl),
        "witnessed=7 diverged=0 pending=0 unjudged_evicted=2"
    );
    assert_eq!(
        std::fs::read_to_string(&log).expect("calls"),
        "temporal judge\ntemporal status\n"
    );
}

/// A child starts with no role (the band the subject ran in before), and
/// [`Band::take`] moves it to `UI_FOCAL`, or names the kernel's refusal
/// with the child still role-less.
#[cfg(target_os = "macos")]
#[test]
fn a_child_takes_the_app_band_or_names_the_refusal() {
    // SAFETY: `getpriority` takes no pointers.
    let role = |pid: u32| unsafe { libc::getpriority(PRIO_DARWIN_ROLE, pid) };
    let mut child = spawnfd::command("/bin/sleep")
        .arg("60")
        .stdin(spawnfd::Stdio::null())
        .stdout(spawnfd::Stdio::null())
        .stderr(spawnfd::Stdio::null())
        .spawn()
        .expect("spawn a sleep");
    let pid = child.id();
    let before = role(pid);
    let band = Band::take(pid);
    let after = role(pid);
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(before, 0, "a spawned child holds no role");
    match band {
        Band::Focal => assert_eq!(after, ROLE_UI_FOCAL, "the child holds UI_FOCAL"),
        Band::Refused(errno) => {
            assert!(
                errno == libc::EPERM || errno == libc::ENOTSUP,
                "the kernel's refusal, not a wrong request: {}",
                Band::Refused(errno)
            );
            assert_eq!(after, 0, "a refused child keeps no role");
        }
        Band::NoRoles => panic!("macOS has darwin roles"),
    }
}

/// A window's consent sentinel outlived the window in the bench's group. The stand-in's helper
/// is a subshell that never execs (so the identity listed is the one it keeps) blocked reading
/// a pipe this test holds: only the drop can end it.
#[cfg(target_vendor = "apple")]
#[test]
fn an_instance_ends_with_the_helper_it_started() {
    use std::io::BufRead as _;
    let mut child = spawnfd::command("/bin/sh")
        .args([
            "-c",
            "exec 3<&0; (read held <&3) & echo $!; exec /bin/sleep 600 3<&-",
        ])
        .stdin(spawnfd::Stdio::piped())
        .stdout(spawnfd::Stdio::piped())
        .stderr(spawnfd::Stdio::null())
        .spawn()
        .expect("spawn the stand-in instance");
    let held = child.stdin.take().expect("the helper's held input");
    let mut line = String::new();
    std::io::BufReader::new(child.stdout.take().expect("piped stdout"))
        .read_line(&mut line)
        .expect("the helper's pid");
    let helper: i32 = line.trim().parse().expect("a pid");
    end_with_helpers(&mut child);
    // The helper's end must happen once its signals are sent, but a loaded machine can take
    // longer than the two graces to show it: wait on the hang detector, with the pipe still
    // held so only the drop's signals can end it.
    let ended = Budget::from_now(MUST_HAPPEN.as_secs()).await_event("the helper's end", || {
        let presence = aterm_uds::process::process_presence(helper);
        matches!(
            presence,
            aterm_uds::process::ProcessPresence::Absent
                | aterm_uds::process::ProcessPresence::Exited
        )
        .then_some(())
    });
    drop(held);
    assert!(
        ended.is_ok(),
        "the helper (pid {helper}) outlived its instance: {ended:?}"
    );
}

/// The bench takes aterm.app's band itself ([`Band::take_own`], the first thing
/// [`super::super::paint_probe::measure`] does): this process then holds `UI_FOCAL`, or the
/// kernel's refusal is named with the process still in the role it had.
#[cfg(target_os = "macos")]
#[test]
fn the_bench_takes_the_app_band_itself() {
    // SAFETY: `getpriority` takes no pointers.
    let role = || unsafe { libc::getpriority(PRIO_DARWIN_ROLE, 0) };
    let before = role();
    let band = Band::take_own();
    let after = role();
    match band {
        Band::Focal => assert_eq!(after, ROLE_UI_FOCAL, "the bench holds UI_FOCAL"),
        Band::Refused(errno) => {
            assert!(
                errno == libc::EPERM || errno == libc::ENOTSUP,
                "the kernel's refusal, not a wrong request: {}",
                Band::Refused(errno)
            );
            assert_eq!(after, before, "a refused bench keeps its role");
        }
        Band::NoRoles => panic!("macOS has darwin roles"),
    }
}
