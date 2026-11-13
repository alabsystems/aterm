// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn layout(label: &str) -> crate::testdir::ScratchLayout {
    crate::testdir::layout(&format!("atpkg-progress-{label}-"))
}

fn read_file(path: &std::path::Path) -> ProgressFile {
    aterm_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn create_truncates_and_leaves_no_temp() {
    let l = layout("create");
    let path = l.progress_file();
    std::fs::write(&path, r#"{"v":1,"pass":"stale","pid":1}"#).unwrap();
    let sink = ProgressSink::create(&path, "net").expect("sink creates");
    let file = read_file(&path);
    assert_eq!(file.pass, "net", "pass start rewrites the stale snapshot");
    assert_eq!(file.pid, Some(std::process::id()));
    assert!(file.ended_unix.is_none());
    let leftovers: Vec<_> = std::fs::read_dir(&l.prefix)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
        .collect();
    assert!(leftovers.is_empty(), "temp+rename leaves no temp behind");
    drop(sink);
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn finish_writes_the_terminal_snapshot() {
    let l = layout("finish");
    let path = l.progress_file();
    let sink = ProgressSink::create(&path, "net").unwrap();
    let generation = read_file(&path).generation.expect("this pass is tagged");
    sink.plan(&[("ty".into(), 7)]);
    sink.finished("ty", Phase::Done, None);
    sink.finish();
    let file = read_file(&path);
    assert_eq!(file.pid, None, "a clean end clears the pid");
    assert!(file.ended_unix.is_some());
    assert_eq!(file.generation.as_deref(), Some(generation.as_str()));
    assert_eq!(file.programs["ty"].phase, Phase::Done);
    assert_eq!(file.overall.programs_done, 1);
    assert!(file.queue.is_empty(), "a finished program leaves the queue");
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// The writer too refuses a symlink planted at the progress path: the sink is disabled.
#[cfg(unix)]
#[test]
fn writer_refuses_a_symlinked_destination() {
    let l = layout("symlink");
    let victim = l.prefix.join("victim");
    std::fs::write(&victim, "precious").unwrap();
    let path = l.progress_file();
    std::os::unix::fs::symlink(&victim, &path).unwrap();
    assert!(
        ProgressSink::create(&path, "net").is_none(),
        "a non-regular destination must disable the sink"
    );
    assert_eq!(
        std::fs::read_to_string(&victim).unwrap(),
        "precious",
        "the symlink target was never written through"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn writer_refuses_a_destination_the_first_write_cannot_reach() {
    let l = layout("unreachable");
    let unreachable = l
        .prefix
        .join("prefix-not-created-yet")
        .join("progress.json");
    assert!(
        ProgressSink::create(&unreachable, "net").is_none(),
        "a first write that cannot land must not yield a live sink"
    );
    let _slot = own_pass_slot();
    let began = begin_pass(&unreachable, "net");
    if began {
        end_pass();
    }
    assert!(
        !began,
        "an unwritable progress file must leave the lane with NO pass"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn snapshot_running_applies_the_heartbeat_window() {
    let now = 1_000_000u64;
    let mut file: ProgressFile = aterm_json::from_str(r#"{"v":1}"#).unwrap();
    file.pid = Some(std::process::id());
    file.heartbeat_unix = now;
    assert!(snapshot_running(&file, now));
    file.heartbeat_unix = now - HEARTBEAT_STALE_SECS - 1;
    assert!(
        !snapshot_running(&file, now),
        "stale heartbeat = not running"
    );
    file.heartbeat_unix = now + HEARTBEAT_STALE_SECS + 1;
    assert!(
        !snapshot_running(&file, now),
        "future heartbeat = not running"
    );
    file.heartbeat_unix = now;
    file.pid = None;
    assert!(!snapshot_running(&file, now), "no pid = not running");
}

#[test]
fn pass_running_reads_a_live_writer_off_the_progress_file() {
    let l = layout("pass-running");
    let now = 1_000_000u64;
    assert!(!pass_running(&l, now), "no progress file");
    let write = |pid: Option<u32>, heartbeat: u64| {
        let mut file: ProgressFile = aterm_json::from_str(r#"{"v":1}"#).unwrap();
        file.pid = pid;
        file.heartbeat_unix = heartbeat;
        std::fs::write(l.progress_file(), aterm_json::to_string(&file).unwrap()).unwrap();
    };
    write(Some(std::process::id()), now);
    assert!(pass_running(&l, now), "a live writer");
    write(None, now);
    assert!(!pass_running(&l, now), "the pass ended");
    write(Some(std::process::id()), now - HEARTBEAT_STALE_SECS - 1);
    assert!(!pass_running(&l, now), "a stale heartbeat");
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// A pid we may not signal (`EPERM`; pid 1 stands in for another user's installer) still
/// exists, so its fresh heartbeat reads as running.
#[cfg(unix)]
#[test]
fn pid_alive_reads_an_unsignalable_foreign_pid_as_alive() {
    assert!(pid_alive(std::process::id()), "this process is alive");
    assert!(
        pid_alive(1),
        "pid 1 exists — EPERM is not ESRCH, and a foreign pass is not a dead one"
    );
    let mut file: ProgressFile = aterm_json::from_str(r#"{"v":1}"#).unwrap();
    file.pid = Some(1);
    file.heartbeat_unix = 1_000_000;
    assert!(
        snapshot_running(&file, 1_000_000),
        "a fresh heartbeat from a live pid we cannot signal is a RUNNING installer"
    );
    assert!(!pid_alive(u32::MAX), "an impossible pid is not alive");
    let mut child = std::process::Command::new("/usr/bin/true")
        .spawn()
        .expect("spawn /usr/bin/true");
    let dead = child.id();
    child.wait().expect("reap it");
    assert!(!pid_alive(dead), "a reaped pid must not read as alive");
    file.pid = Some(dead);
    assert!(!snapshot_running(&file, 1_000_000));
}

#[test]
fn sanitize_for_tty_strips_and_caps() {
    assert_eq!(sanitize_for_tty("plain error", 64), "plain error");
    assert_eq!(
        sanitize_for_tty("evil\u{1b}[2Jwipe\r\nmore", 64),
        "evil[2Jwipemore",
        "ESC/CR/LF are control characters and must not reach the TTY"
    );
    assert_eq!(sanitize_for_tty("abcdef", 3), "abc…");
}

#[test]
fn bump_round_trips_and_refuses_garbage() {
    let l = layout("bump");
    let trust = crate::store::ToolName::new("trust").unwrap();
    let ty = crate::store::ToolName::new("ty").unwrap();
    append_bump(&l, &trust, Asker::Person).unwrap();
    append_bump(&l, &ty, Asker::Person).unwrap();
    append_bump(&l, &trust, Asker::Person).unwrap();
    assert_eq!(read_bump(&l), vec!["trust".to_string(), "ty".to_string()]);
    std::fs::write(l.bump_file(), "sudo\n../evil\na/b\ntrust\n\n").unwrap();
    assert_eq!(
        read_bump(&l),
        vec!["trust".to_string()],
        "shim-refused and malformed names never come back"
    );
    std::fs::write(l.bump_file(), "x".repeat(BUMP_READ_CAP as usize + 1)).unwrap();
    assert!(read_bump(&l).is_empty());
    clear_bump(&l);
    assert!(!l.bump_file().exists());
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn a_bump_says_whether_a_person_asked() {
    let l = layout("bump-asker");
    let codex = crate::store::ToolName::new("codex").unwrap();
    let claude = crate::store::ToolName::new("claude").unwrap();
    append_bump(&l, &codex, Asker::Script).unwrap();
    append_bump(&l, &claude, Asker::Person).unwrap();
    assert_eq!(bump_contents(&l), "codex unattended\nclaude\n");
    assert_eq!(read_bump(&l), ["codex", "claude"]);
    let bump = |program: &str, person| Bump {
        program: program.to_string(),
        person,
    };
    assert_eq!(read_bumps(&l), [bump("codex", false), bump("claude", true)]);
    append_bump(&l, &codex, Asker::Person).unwrap();
    append_bump(&l, &claude, Asker::Script).unwrap();
    assert_eq!(
        read_bumps(&l),
        [bump("codex", true), bump("claude", true)],
        "a person's line makes it a person's, in first-mention order"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

fn bump_contents(l: &crate::store::Layout) -> String {
    std::fs::read_to_string(l.bump_file()).unwrap_or_default()
}

/// Symlink refusal on the bump paths (unix): append refuses, read refuses.
#[cfg(unix)]
#[test]
fn bump_refuses_symlinks() {
    let l = layout("bump-sym");
    let victim = l.prefix.join("victim");
    std::fs::write(&victim, "trust\n").unwrap();
    std::os::unix::fs::symlink(&victim, l.bump_file()).unwrap();
    let trust = crate::store::ToolName::new("trust").unwrap();
    assert!(
        append_bump(&l, &trust, Asker::Person).is_err(),
        "append must not follow a link"
    );
    assert!(read_bump(&l).is_empty(), "read must not follow a link");
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "trust\n");
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn stopping_a_pass_poller_does_not_wait_out_its_tick() {
    let l = layout("prompt-stop");
    let progress = l.progress_file();
    let asset = l.prefix.join("trust-1.tar.zst");
    std::fs::write(l.prefix.join("trust-1.tar.zst.part"), [0u8; 7]).unwrap();
    let reported = || live_snapshot().and_then(|f| f.programs.get("trust").map(|r| r.bytes_done));
    let (ended_tx, ended_rx) = std::sync::mpsc::channel();
    let pass = std::thread::spawn(move || {
        let _slot = own_pass_slot();
        TICK_OVERRIDE.set(Some(Duration::from_secs(3600)));
        assert!(begin_pass(&progress, "net"));
        for _ in 0..4 {
            let watch = watch_download("trust", &asset, 1000);
            assert!(watch.handle.is_some(), "a net pass gets the live poller");
            while reported() != Some(7) {
                std::thread::sleep(Duration::from_millis(1));
            }
            drop(watch);
        }
        end_pass();
        let _ = ended_tx.send(read_file(&progress));
    });
    let ended = match ended_rx.recv_timeout(Duration::from_secs(60)) {
        Ok(ended) => ended,
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            std::panic::resume_unwind(pass.join().expect_err("the pass sent nothing"))
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => panic!(
            "the pass did not end within 60 s of hour-long ticks: a stop waited out \
             its tick (a poller or the heartbeat sleeps instead of parking)"
        ),
    };
    pass.join().unwrap();
    assert_eq!(ended.pid, None, "the terminal snapshot landed");
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// A LAN PEER'S TRANSFER MOVES THE FILE. After the release host fails, a peer may go on sending
/// a build for over an hour at its pace's floor, landing only in `.peer.part`, and what judges
/// an install reads `progress.json`. CONTROLS: the release host's partial moves it as ever, the
/// further of the two shows, and the release host's shows again once a failed peer's has gone.
#[test]
fn a_download_watch_writes_what_a_lan_peers_partial_lands() {
    let _slot = own_pass_slot();
    let l = layout("peer-partial");
    let asset = l.prefix.join("muse-1.tar.zst");
    let [part, peer_part, _] = crate::peers::arrival_paths(&asset);
    let landed = |want: u64| {
        let by = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        loop {
            let read = read_progress(&l)
                .and_then(|file| file.programs.get("muse").map(|row| row.bytes_done));
            if read == Some(want) {
                return;
            }
            assert!(
                Instant::now() < by,
                "progress.json says {read:?}, not {want}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    assert!(begin_pass(&l.progress_file(), "install"));
    let watch = watch_download("muse", &asset, 1000);

    std::fs::write(&peer_part, [0u8; 7]).unwrap();
    landed(7);
    std::fs::write(&peer_part, [0u8; 12]).unwrap();
    landed(12);

    std::fs::write(&part, [0u8; 40]).unwrap();
    landed(40);
    std::fs::write(&peer_part, [0u8; 90]).unwrap();
    landed(90);
    std::fs::remove_file(&peer_part).unwrap();
    std::fs::write(&part, [0u8; 41]).unwrap();
    landed(41);

    drop(watch);
    end_pass();
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// A PARTIAL LEFT BEHIND HIDES NO TRANSFER. A release host that failed keeps its `.part` for
/// the next call, and a peer's transfer behind it would land hundreds of MB unseen: past
/// [`PARTIAL_QUIET`] the meter follows what moves, each byte moving it. CONTROLS: within that,
/// and while both grow, the meter shows the furthest and does not flicker; the other partial
/// moving again takes it back; and with nothing moving it stands at the furthest.
#[test]
fn a_partial_that_went_quiet_leaves_the_meter_to_the_one_that_moves() {
    let t0 = Instant::now();
    let mut partials = Partials::new(t0);
    assert_eq!(partials.shown([0, 0], t0), 0);
    assert_eq!(partials.shown([300, 10], t0), 300);
    let quiet = t0 + PARTIAL_QUIET;
    let short = quiet - Duration::from_secs(1);
    assert_eq!(partials.shown([300, 20], short), 300, "not yet quiet");
    assert_eq!(partials.shown([300, 30], quiet), 30, "the peer's bytes");
    let on = quiet + Duration::from_secs(1);
    assert_eq!(partials.shown([300, 31], on), 31, "each moves the meter");
    assert_eq!(partials.shown([301, 32], on), 301, "both move: the further");
    let still = on + PARTIAL_QUIET;
    assert_eq!(partials.shown([301, 32], still), 301, "nothing moves");
    assert_eq!(partials.shown([0, 0], still), 0, "the partials went");
}

#[test]
fn a_silent_phase_keeps_the_pass_heartbeat_live() {
    let _slot = own_pass_slot();
    let clock = ManualPassClock::new();
    let _clock = clock.drive_passes();
    let l = layout("silent-heartbeat");
    TICK_OVERRIDE.set(Some(Duration::from_millis(1)));
    assert!(begin_pass(&l.progress_file(), "net"));
    TICK_OVERRIDE.set(None);
    let h0 = read_file(&l.progress_file()).heartbeat_unix;
    clock.advance(Duration::from_secs(HEARTBEAT_MIN_INTERVAL_SECS));
    let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let file = loop {
        let file = read_file(&l.progress_file());
        if file.heartbeat_unix >= h0 + HEARTBEAT_MIN_INTERVAL_SECS {
            break file;
        }
        assert!(
            Instant::now() < deadline,
            "the pass heartbeat must advance with no flow calls ({h0} → {})",
            file.heartbeat_unix
        );
        std::thread::sleep(Duration::from_millis(1));
    };
    assert!(
        snapshot_running(&file, clock.unix_now()),
        "a silent-but-live pass reads as running"
    );
    end_pass();
    let ended = read_file(&l.progress_file());
    assert_eq!(ended.pid, None);
    assert!(!snapshot_running(&ended, clock.unix_now()));
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn a_quiet_sink_refreshes_its_heartbeat_at_the_interval_and_not_before() {
    let clock = ManualPassClock::new();
    let _clock = clock.drive_passes();
    let l = layout("heartbeat-interval");
    let path = l.progress_file();
    let sink = ProgressSink::create(&path, "net").expect("sink creates");
    let h0 = read_file(&path).heartbeat_unix;
    let interval = Duration::from_secs(HEARTBEAT_MIN_INTERVAL_SECS);
    clock.advance(interval - Duration::from_millis(1));
    sink.heartbeat();
    assert_eq!(
        read_file(&path).heartbeat_unix,
        h0,
        "a tick inside the interval writes nothing"
    );
    clock.advance(Duration::from_millis(1));
    sink.heartbeat();
    assert_eq!(
        read_file(&path).heartbeat_unix,
        h0 + HEARTBEAT_MIN_INTERVAL_SECS,
        "the tick at the interval refreshes the stamp"
    );
    drop(sink);
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn a_lent_pass_credits_its_lane_and_each_lane_extracts_into_its_own_row() {
    let _slot = own_pass_slot();
    let l = layout("lent");
    assert!(begin_pass(&l.progress_file(), "net"));
    active()
        .expect("the owner")
        .plan(&[("codex".to_string(), 10), ("trust".to_string(), 10)]);
    let both_open = std::sync::Barrier::new(2);
    let both_ticked = std::sync::Barrier::new(2);
    std::thread::scope(|s| {
        assert!(
            s.spawn(|| active().is_none()).join().unwrap(),
            "a thread nobody lent the pass credits nothing"
        );
        let lent = lend();
        let (both_open, both_ticked) = (&both_open, &both_ticked);
        let lane = s.spawn(move || {
            let _adopted = lent.adopt();
            let _codex = extract_scope("codex", 100);
            both_open.wait();
            extract_tick(7);
            both_ticked.wait();
        });
        let _trust = extract_scope("trust", 100);
        both_open.wait();
        extract_tick(3);
        both_ticked.wait();
        lane.join().unwrap();
    });
    let file = live_snapshot().expect("the pass is live");
    end_pass();
    let credited = |p: &str| (file.programs[p].phase, file.programs[p].bytes_done);
    assert_eq!(credited("codex"), (Phase::Extract, 7));
    assert_eq!(credited("trust"), (Phase::Extract, 3));
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn a_promise_holds_only_while_its_owner_lives_and_its_deadline_is_ahead() {
    let l = layout("promise");
    let now = unix_now();
    let me = std::process::id();
    assert!(!pass_owed(&l, now), "no promise");
    promise_pass(&l, me, SEED_PROMISE_SECS);
    let written: PassPromise =
        aterm_json::from_str(&std::fs::read_to_string(l.pass_owed_file()).unwrap()).unwrap();
    assert_eq!(written.owner, me);
    assert!(
        (now + SEED_PROMISE_SECS..=unix_now() + SEED_PROMISE_SECS).contains(&written.until_unix),
        "{written:?}"
    );
    assert!(pass_owed(&l, now), "a live owner, a deadline ahead");
    assert!(pass_owed(&l, written.until_unix), "up to the deadline");
    assert!(!pass_owed(&l, written.until_unix + 1), "not past it");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(l.pass_owed_file())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    retire_promise(&l);
    assert!(!pass_owed(&l, now), "retired");
    let write = |body: String| std::fs::write(l.pass_owed_file(), body).unwrap();
    write(format!(
        "{{\"v\":1,\"owner\":{me},\"until_unix\":{}}}",
        now + PASS_PROMISE_SECS + 60
    ));
    assert!(!pass_owed(&l, now), "a deadline no writer promises");
    write(format!(
        "{{\"v\":2,\"owner\":{me},\"until_unix\":{}}}",
        now + 5
    ));
    assert!(!pass_owed(&l, now), "another version");
    write("{not json".to_string());
    assert!(!pass_owed(&l, now), "malformed");
    write(" ".repeat(PROMISE_READ_CAP + 1));
    assert!(!pass_owed(&l, now), "oversized");
    #[cfg(unix)]
    {
        let victim = l.prefix.join("victim.json");
        let honest = format!("{{\"v\":1,\"owner\":{me},\"until_unix\":{}}}", now + 5);
        std::fs::write(&victim, &honest).unwrap();
        let _ = std::fs::remove_file(l.pass_owed_file());
        std::os::unix::fs::symlink(&victim, l.pass_owed_file()).unwrap();
        assert!(!pass_owed(&l, now), "a symlink is never followed");
        promise_pass(&l, me, SEED_PROMISE_SECS);
        assert_eq!(
            std::fs::read_to_string(&victim).unwrap(),
            honest,
            "nor written through"
        );
    }
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn a_promise_ends_when_its_owners_pid_is_another_process() {
    let l = layout("promise-born");
    let now = unix_now();
    let me = std::process::id();
    promise_pass(&l, me, SEED_PROMISE_SECS);
    let written: PassPromise =
        aterm_json::from_str(&std::fs::read_to_string(l.pass_owed_file()).unwrap()).unwrap();
    if cfg!(any(target_os = "macos", target_os = "linux", windows)) {
        assert_eq!(written.born, crate::platform::process_birth(me).at());
        assert!(written.born.is_some(), "{written:?}");
    }
    assert!(pass_owed(&l, now), "its writer's own process");
    if let Some(born) = written.born {
        let reused = PassPromise {
            born: Some(born + 1),
            ..written.clone()
        };
        std::fs::write(l.pass_owed_file(), aterm_json::to_string(&reused).unwrap()).unwrap();
        assert!(!pass_owed(&l, now), "the same pid, another process");
    }
    std::fs::write(
        l.pass_owed_file(),
        format!("{{\"v\":1,\"owner\":{me},\"until_unix\":{}}}", now + 5),
    )
    .unwrap();
    assert!(pass_owed(&l, now), "no start time: the pid alone");
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn an_owner_start_time_the_reader_cannot_read_leaves_the_pid_to_answer() {
    use crate::platform::Birth;
    assert!(same_owner(Some(7), Birth::At(7)), "the owner it recorded");
    assert!(!same_owner(Some(7), Birth::At(8)), "another process");
    assert!(!same_owner(Some(7), Birth::Gone), "none: a zombie");
    assert!(
        same_owner(Some(7), Birth::Unreadable),
        "the reader cannot say"
    );
    for now in [Birth::At(8), Birth::Gone, Birth::Unreadable] {
        assert!(same_owner(None, now), "no start time recorded: {now:?}");
    }
}

/// The platform's start-time reader as [`same_owner`] judges it, on every platform that
/// has one: this process, read twice, is the owner it recorded; a child that has exited
/// is no owner — reaped on Unix (`ESRCH`; no `/proc/<pid>`), and on Windows still held by
/// its `Child`'s handle, so the pid names that exited process and no other
/// (`GetExitCodeProcess` no longer `STILL_ACTIVE`).
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
#[test]
fn the_platform_reader_tells_a_live_owner_from_one_that_exited() {
    use crate::platform::{Birth, process_birth};
    let me = std::process::id();
    let born = process_birth(me);
    assert!(
        matches!(born, Birth::At(_)),
        "this process's start time: {born:?}"
    );
    assert!(
        same_owner(born.at(), process_birth(me)),
        "read again: the same owner"
    );
    #[cfg(unix)]
    let mut child = std::process::Command::new("/usr/bin/true")
        .spawn()
        .expect("spawn /usr/bin/true");
    #[cfg(windows)]
    let mut child = std::process::Command::new("cmd")
        .args(["/C", "exit 0"])
        .spawn()
        .expect("spawn cmd /C exit 0");
    let exited = child.id();
    child.wait().expect("it exits");
    assert_eq!(
        process_birth(exited),
        Birth::Gone,
        "an exited child is no owner"
    );
    drop(child);
}

#[test]
fn a_promise_is_retired_only_by_its_holders() {
    let l = layout("promise-holders");
    let me = std::process::id();
    promise_pass(&l, me, SEED_PROMISE_SECS);
    retire_promise_of(&l, &[me.wrapping_add(1)]);
    assert!(
        pass_owed(&l, unix_now()),
        "another holder's ending leaves it"
    );
    retire_promise_of(&l, &[me.wrapping_add(1), me]);
    assert!(
        !l.pass_owed_file().exists(),
        "its holder's ending retires it"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[cfg(unix)]
#[test]
fn a_promise_does_not_publish_a_preexisting_fifo_temp() {
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::{FileTypeExt as _, OpenOptionsExt as _};

    let l = layout("promise-fifo-temp");
    promise_pass(&l, std::process::id(), SEED_PROMISE_SECS);
    let path = l.pass_owed_file();
    let before = std::fs::read(&path).unwrap();
    let tmp = path.with_file_name(format!("pass-owed.json.tmp-{}", std::process::id()));
    let fifo = std::ffi::CString::new(tmp.as_os_str().as_bytes()).unwrap();
    // SAFETY: the path is NUL-terminated and belongs to this private fixture.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    let _fifo = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(&tmp)
        .unwrap();

    promise_pass(&l, std::process::id(), PASS_PROMISE_SECS);

    assert!(
        std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_file(),
        "a colliding FIFO must not replace the regular promise"
    );
    let refreshed = read_promise(&l).expect("a colliding FIFO cannot suppress refresh");
    assert!(
        refreshed.until_unix
            > aterm_json::from_slice::<PassPromise>(&before)
                .unwrap()
                .until_unix
    );
    assert!(
        pass_owed(&l, unix_now()),
        "the refreshed promise remains live"
    );
    assert!(
        std::fs::symlink_metadata(&tmp)
            .unwrap()
            .file_type()
            .is_fifo()
    );
}

#[test]
fn an_occupied_staging_path_cannot_suppress_the_first_pass_promise() {
    let l = layout("promise-occupied-first");
    let path = l.pass_owed_file();
    let occupied = path.with_file_name(format!("pass-owed.json.tmp-{}", std::process::id()));
    std::fs::write(&occupied, b"a previous writer's staged bytes").unwrap();
    let owner = std::process::id();

    promise_pass(&l, owner, PASS_PROMISE_SECS);

    let promise = read_promise(&l).expect("the current pass publishes its promise");
    assert_eq!(promise.owner, owner);
    assert!(pass_owed(&l, unix_now()));
    assert_eq!(
        std::fs::read(occupied).unwrap(),
        b"a previous writer's staged bytes"
    );
}

#[cfg(unix)]
#[test]
fn a_promise_does_not_truncate_a_preexisting_hardlinked_temp() {
    use std::os::unix::fs::PermissionsExt as _;

    let l = layout("promise-hardlink-temp");
    promise_pass(&l, std::process::id(), SEED_PROMISE_SECS);
    let path = l.pass_owed_file();
    let before = std::fs::read(&path).unwrap();
    let victim = l.prefix.join("other-metadata");
    std::fs::write(&victim, b"preserve this metadata").unwrap();
    std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o644)).unwrap();
    let tmp = path.with_file_name(format!("pass-owed.json.tmp-{}", std::process::id()));
    std::fs::hard_link(&victim, &tmp).unwrap();

    promise_pass(&l, std::process::id(), PASS_PROMISE_SECS);

    assert_eq!(std::fs::read(&victim).unwrap(), b"preserve this metadata");
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(tmp.exists(), "an unowned colliding entry is left untouched");

    let refreshed: PassPromise =
        aterm_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(
        refreshed.until_unix
            > aterm_json::from_slice::<PassPromise>(&before)
                .unwrap()
                .until_unix
    );
    assert!(pass_owed(&l, unix_now()));
}

#[test]
fn read_progress_is_bounded_and_symlink_refusing() {
    let l = layout("read");
    assert!(read_progress(&l).is_none(), "absent reads as None");
    std::fs::write(l.progress_file(), r#"{"v":1,"pass":"net"}"#).unwrap();
    assert_eq!(read_progress(&l).unwrap().pass, "net");
    std::fs::write(l.progress_file(), "not json at all {{{").unwrap();
    assert!(read_progress(&l).is_none(), "malformed reads as None");
    let mut big = String::from(r#"{"v":1,"pass":""#);
    big.push_str(&"x".repeat(PROGRESS_READ_CAP as usize));
    big.push_str("\"}");
    std::fs::write(l.progress_file(), big).unwrap();
    assert!(read_progress(&l).is_none(), "oversize reads as None");
    #[cfg(unix)]
    {
        let victim = l.prefix.join("victim.json");
        std::fs::write(&victim, r#"{"v":1}"#).unwrap();
        let _ = std::fs::remove_file(l.progress_file());
        std::os::unix::fs::symlink(&victim, l.progress_file()).unwrap();
        assert!(read_progress(&l).is_none(), "a symlink is never followed");
    }
    let _ = std::fs::remove_dir_all(&l.prefix);
}
