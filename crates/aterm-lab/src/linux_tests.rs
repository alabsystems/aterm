// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

/// LR-3's race: a VM lima still reads running when the cut command returns is down once it
/// reads it so within the wait; one that stays running, or that lima cannot read, is not.
#[test]
fn a_cut_command_waits_for_lima_to_read_the_vm_down() {
    use std::time::Duration;
    let pace = Duration::from_millis(1);
    let mut readings = [Some(true), Some(true), Some(false)].into_iter();
    let mut asked = 0;
    let down = super::left_running(
        || {
            asked += 1;
            readings.next().flatten()
        },
        Duration::from_secs(60),
        pace,
    );
    assert!(down);
    assert_eq!(asked, 3, "it asked until lima read the VM down");
    assert!(!super::left_running(|| Some(true), Duration::ZERO, pace));
    assert!(!super::left_running(|| None, Duration::from_secs(60), pace));
}

/// A stream past the limit stops there, keeps only a prefix, and says it was cut.
#[test]
fn a_stream_past_the_limit_stops_and_says_it_was_cut() {
    let input = "abcdefg\n".repeat(10);
    let read = super::read_lines(input.as_bytes(), 20, |_| {});
    assert!(read.cut);
    assert_eq!(
        read.text,
        "abcdefg\nabcdefg\n[output passed 20 bytes: the read stopped here]\n"
    );
}

/// Bytes that are not UTF-8 neither end the read nor lose the lines around them.
#[test]
fn a_line_that_is_not_utf8_does_not_end_the_read() {
    let read = super::read_lines(&b"one\n\xff\ntwo\n"[..], 1024, |_| {});
    assert!(!read.cut);
    assert_eq!(read.text, "one\n\u{fffd}\ntwo\n");
}

use super::*;

#[test]
fn unowned_lima_never_reaches_the_mutating_or_guest_command() {
    use std::os::unix::fs::DirBuilderExt as _;
    let t = aterm_tempfile::tempdir().unwrap();
    let owned = owned::Namespace::open(&t.path().join("lima"), VM).unwrap();
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(owned.vm_dir())
        .unwrap();
    std::fs::write(owned.vm_dir().join("notes"), "keep").unwrap();
    let lima = Lima {
        bin: t.path().join("must-not-start"),
        vm: VM,
        env: Vec::new(),
        owned,
        lost: OnceLock::new(),
    };
    for verb in ["list", "create", "stop", "start", "edit", "delete", "copy"] {
        let out = lima.run(&[verb.into(), VM.into()]);
        assert_eq!(out.code, Some(NOT_RUN), "{verb}: {}", out.text);
        assert!(out.not_started.is_some(), "{verb}: {}", out.text);
    }
    for out in [
        lima.shell("/", &["true".into()], None),
        lima.shell_streamed("/", &["true".into()], ""),
    ] {
        assert_eq!(out.code, Some(NOT_RUN));
        assert!(out.not_started.is_some());
    }
    assert_eq!(
        std::fs::read_to_string(lima.owned.vm_dir().join("notes")).unwrap(),
        "keep"
    );
}

#[test]
fn a_signal_that_ended_limactl_is_named() {
    let run = |script: &str| {
        aterm_uds::spawnfd::command("/bin/sh")
            .args(["-c", script])
            .status()
            .expect("/bin/sh runs")
    };
    assert_eq!(
        ended_on_signal(run("kill -9 $$")),
        "\n(limactl ended on signal 9)\n"
    );
    assert_eq!(ended_on_signal(run("exit 255")), "");
}

#[test]
fn a_failure_names_how_it_ended() {
    let out = |code: Option<i32>, text: &str| Out {
        code,
        text: text.into(),
        secs: 0.0,
        signal: None,
        not_started: None,
        lost: None,
    };
    assert_eq!(
        out(Some(255), " \n").status(),
        "exit 255, nothing said: a guest command that a signal ended, or one that exited 255"
    );
    assert_eq!(
        out(Some(255), "ssh: connect refused\n").status(),
        "exit 255"
    );
    assert_eq!(
        out(None, "\n(limactl ended on signal 9)\n").status(),
        "no exit status"
    );
    assert_eq!(out(Some(1), "").status(), "exit 1");
}

const LIST: &str = r#"{"name":"other","status":"Running","dir":"/x","cpus":2,"memory":1,"disk":2}
{"name":"aterm-linux","status":"Stopped","dir":"/Users//u/.lima/aterm-linux","vmType":"vz","cpus":8,"memory":17179869184,"disk":64424509440,"config":{"mounts":[{"location":"/Users//u/aterm","mountPoint":"/Users//u/aterm","writable":false}]}}
"#;

#[test]
fn the_lane_vm_is_read_out_of_limas_list() {
    let vm = parse_list(LIST, VM).expect("the lane's VM");
    assert_eq!(vm.status, "Stopped");
    assert_eq!(vm.dir, PathBuf::from("/Users//u/.lima/aterm-linux"));
    assert_eq!(vm.cpus, 8);
    assert_eq!(vm.memory, 16 << 30);
    assert_eq!(vm.disk, 60 << 30);
    let alone = ["/Users//u/aterm".to_string()];
    assert!(mounted_exactly(&vm, &alone));
    assert!(!mounted_exactly(&vm, &["/Users//u/other-worktree".into()]));
    assert!(!mounted_exactly(
        &vm,
        &["/Users//u/aterm".into(), "/Users//u/main/.git".into()]
    ));
    assert_eq!(parse_list(LIST, "absent"), None);
    assert_eq!(parse_list("", VM), None);
    let writable = LIST.replace("\"writable\":false", "\"writable\":true");
    assert!(!mounted_exactly(
        &parse_list(&writable, VM).expect("vm"),
        &alone
    ));
}

#[test]
fn a_run_builds_only_on_its_own_checkouts_output() {
    let me = "/Users//u/aterm-dv-a";
    assert_eq!(build_output(false, None, me), BuildOutput::Absent);
    assert_eq!(
        build_output(false, Some("/Users//u/aterm-dv-b"), me),
        BuildOutput::Absent
    );
    assert_eq!(build_output(true, Some(me), me), BuildOutput::Own);
    assert_eq!(
        build_output(true, Some("/Users//u/aterm-dv-b"), me),
        BuildOutput::Foreign
    );
    assert_eq!(build_output(true, None, me), BuildOutput::Foreign);
    assert_eq!(
        build_output(true, Some("/Users//u/aterm-dv-a-base"), me),
        BuildOutput::Foreign
    );
}

#[test]
fn a_linked_worktree_mounts_its_repositorys_git_directory() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    let root = crate::paths::physical(root.path());
    let main_git = root.join("main/.git");
    let gitdir = main_git.join("worktrees/wt");
    std::fs::create_dir_all(&gitdir).unwrap();
    std::fs::write(gitdir.join("commondir"), "../..\n").unwrap();
    let wt = root.join("wt");
    std::fs::create_dir_all(&wt).unwrap();
    std::fs::write(wt.join(".git"), format!("gitdir: {}\n", gitdir.display())).unwrap();
    assert_eq!(outside_git_dir(&wt), Some(main_git.clone()));
    let wt_path = wt.to_string_lossy().into_owned();
    assert_eq!(
        lane_mounts(&wt_path),
        [wt_path.clone(), main_git.to_string_lossy().into_owned()]
    );
    let main = root.join("main").to_string_lossy().into_owned();
    assert_eq!(outside_git_dir(Path::new(&main)), None);
    assert_eq!(lane_mounts(&main), std::slice::from_ref(&main));
    let inner = root.join("inner");
    std::fs::create_dir_all(inner.join("gitdir")).unwrap();
    std::fs::write(inner.join(".git"), "gitdir: gitdir\n").unwrap();
    assert_eq!(outside_git_dir(&inner), None);
}

/// A wait no interrupt ends.
fn go_on() -> Result<(), crate::stop::Stopped> {
    Ok(())
}

/// An interrupt that ends a wait for a held lane is NOT RUN, naming the holder and carrying
/// the signal: the lane ran nothing. Negative control: the holder's own take, with no
/// interrupt, has the lane.
#[test]
fn an_interrupted_wait_is_not_run_naming_the_holder() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("lab-lane-stop-")
        .tempdir()
        .expect("a scratch dir");
    let path = dir.path().join(".aterm-linux.lane.lock");
    let holder = named_lock::Holder::me("lab linux proof", Path::new("/checkouts/holder"));
    let held = LaneLock::at_as(&path, "linux", &holder, Wait::No, &mut |_| {}, &go_on)
        .unwrap_or_else(|f| panic!("the lane is free: {:?}", f.msg));
    assert_eq!(held.waited(), std::time::Duration::ZERO);
    let me = named_lock::Holder::me("lab linux exec", Path::new("/checkouts/waiter"));
    let refused = LaneLock::at_as(&path, "linux", &me, Wait::Block, &mut |_| {}, &|| {
        Err(crate::stop::Stopped(libc::SIGTERM))
    })
    .err()
    .expect("an interrupt ends the wait");
    let why = refused.msg.unwrap_or_default();
    assert_eq!(
        (refused.code, refused.stopped),
        (NOT_RUN, Some(libc::SIGTERM)),
        "{why}"
    );
    assert!(
        why.starts_with(&format!(
            "NOT RUN: interrupted while waiting for pid {} (lab linux proof, /checkouts/holder, since ",
            holder.pid
        )),
        "{why}"
    );
    drop(held);
}

/// [`a_held_lane_is_waited_for_until_its_holder_dies_by_its_own_hand`]'s holder.
const STAND_IN: &str = "linux::tests::a_lane_holder_stand_in";
/// The lock file the stand-in takes.
const STAND_IN_ENV: &str = "ATERM_LAB_TEST_LANE_HOLDER";
/// What the stand-in says once it holds the lane (libtest's own words share its line).
const STAND_IN_HOLDS: &str = "the stand-in holds the lane";

/// Not a test alone: as [`STAND_IN`] it takes the lane at [`STAND_IN_ENV`]'s path, says
/// [`STAND_IN_HOLDS`], and at its first line on stdin dies by its own `SIGKILL`.
#[test]
fn a_lane_holder_stand_in() {
    let Some(path) = std::env::var_os(STAND_IN_ENV) else {
        return;
    };
    let me = named_lock::Holder::me("lab linux proof", Path::new("/checkouts/holder"));
    let _held = LaneLock::at_as(
        Path::new(&path),
        "linux",
        &me,
        Wait::No,
        &mut |_| {},
        &go_on,
    )
    .unwrap_or_else(|f| panic!("the lane is free: {:?}", f.msg));
    println!("{STAND_IN_HOLDS}");
    let _ = std::io::stdout().flush();
    let _ = std::io::stdin().read_line(&mut String::new());
    aterm_uds::process::kill_self_now()
}

/// A held lane is waited for with its holder named, and is the waiter's once the holder is
/// gone, though the holder died by its own `SIGKILL` and nobody signalled anyone; the waiter
/// says how soon it had the lane. Negative control: `--no-wait` is NOT RUN at once, naming
/// the same holder.
#[test]
fn a_held_lane_is_waited_for_until_its_holder_dies_by_its_own_hand() {
    use std::os::unix::process::ExitStatusExt as _;
    let dir = aterm_tempfile::Builder::new()
        .prefix("lab-lane-wait-")
        .tempdir()
        .expect("a scratch dir");
    let path = dir.path().join(".aterm-linux.lane.lock");
    let mut holder = aterm_uds::spawnfd::command(std::env::current_exe().expect("this test"))
        .args(["--exact", STAND_IN, "--nocapture", "--test-threads=1"])
        .env(STAND_IN_ENV, &path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("the stand-in starts");
    let pid = holder.id();
    let said = BufReader::new(holder.stdout.take().expect("its stdout"));
    let (holds, held_now) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in said.lines().map_while(Result::ok) {
            if line.contains(STAND_IN_HOLDS) {
                let _ = holds.send(());
            }
        }
    });
    // The one-minute hang detector: the stand-in takes a free lane at once.
    held_now
        .recv_timeout(std::time::Duration::from_secs(60))
        .expect("the stand-in holds the lane");
    let me = named_lock::Holder::me("lab linux proof", Path::new("/checkouts/waiter"));
    let refused = LaneLock::at_as(&path, "linux", &me, Wait::No, &mut |_| {}, &go_on)
        .err()
        .expect("--no-wait does not wait");
    let why = refused.msg.unwrap_or_default();
    assert_eq!(refused.code, NOT_RUN, "{why}");
    assert!(
        why.contains(&format!(
            "pid {pid} (lab linux proof, /checkouts/holder, since "
        )),
        "{why}"
    );

    let mut stdin = holder.stdin.take().expect("its stdin");
    let (died, death) = std::sync::mpsc::channel();
    let reaper = std::thread::spawn(move || {
        let status = holder.wait().expect("the stand-in is reaped");
        let _ = died.send(Instant::now());
        status
    });
    let (took, taken) = std::sync::mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let mut heard = Vec::new();
        let held = LaneLock::at_as(
            &path,
            "linux",
            &me,
            Wait::Block,
            &mut |line| {
                heard.push(line.to_string());
                let _ = stdin.write_all(b"die\n");
            },
            &go_on,
        );
        let _ = took.send(Instant::now());
        (held.map(|_| ()).map_err(|f| f.msg), heard)
    });
    // The one-minute hang detector: the lane frees itself as the holder dies.
    let had = taken
        .recv_timeout(std::time::Duration::from_secs(60))
        .expect("the waiter has the lane once its holder is gone");
    let (held, heard) = waiter.join().expect("the waiter");
    held.expect("the lane");
    let status = reaper.join().expect("the reaper");
    assert_eq!(status.signal(), Some(libc::SIGKILL), "{status}");
    let gone = death.recv().expect("the death's instant");
    assert!(
        heard.first().is_some_and(|l| l.starts_with(&format!(
            "linux: waiting for pid {pid} (lab linux proof, /checkouts/holder, since "
        ))),
        "{heard:?}"
    );
    eprintln!(
        "lane free {} ms after its holder died",
        had.saturating_duration_since(gone).as_millis()
    );
}

#[test]
fn every_checkout_takes_the_one_lock_beside_the_vm() {
    let home = Path::new("/private/lima");
    let mine = lane_lock_path(VM, home);
    assert_eq!(mine, PathBuf::from("/private/lima/.aterm-linux.lane.lock"));
    assert_eq!(
        lane_lock_path(VM, Path::new("/v/lima")),
        PathBuf::from("/v/lima/.aterm-linux.lane.lock")
    );
    assert_eq!(
        lane_lock_path(crate::linux_gpu::VM, home),
        PathBuf::from("/private/lima/.aterm-linux-gpu.lane.lock"),
        "the GPU instance's runs never wait on the lane VM's"
    );
}

#[test]
fn the_mount_expression_quotes_its_paths() {
    assert_eq!(
        mount_expr(&["/Users//u/aterm".into()]),
        r#".mounts = [{"location": "/Users//u/aterm", "writable": false}]"#
    );
    assert_eq!(
        mount_expr(&["/Users//u/wt".into(), "/Users//u/aterm/.git".into()]),
        r#".mounts = [{"location": "/Users//u/wt", "writable": false}, {"location": "/Users//u/aterm/.git", "writable": false}]"#
    );
    assert!(mount_expr(&["/a\"b".into()]).contains(r#""/a\"b""#));
}

#[test]
fn exec_reads_its_switches_then_the_command() {
    let s = |v: &[&str]| v.iter().map(|x| (*x).to_string()).collect::<Vec<_>>();
    let e = parse_exec(&s(&[
        "--arch",
        "x86_64",
        "--display",
        "weston",
        "--",
        "cargo",
        "test",
    ]))
    .expect("parses");
    assert_eq!(e.arch, Arch::X86_64);
    assert_eq!(e.display, Some(Display::Weston));
    assert_eq!(e.argv, ["cargo", "test"]);
    let bare = parse_exec(&s(&["uname", "-m"])).expect("parses");
    assert_eq!((bare.arch, bare.display), (Arch::Aarch64, None));
    assert_eq!(bare.argv, ["uname", "-m"]);
    assert_eq!(
        parse_exec(&s(&["--arch", "mips", "x"]))
            .map_err(|f| f.code)
            .err(),
        Some(2)
    );
    assert_eq!(parse_exec(&s(&["--"])).map_err(|f| f.code).err(), Some(2));
}

#[test]
fn df_and_sizes_read_as_shown() {
    assert_eq!(
        parse_df("    Used     1B-blocks\n 6940000000 61285326848\n"),
        Some((6_940_000_000, 61_285_326_848))
    );
    assert_eq!(parse_df("Used 1B-blocks\n"), None);
    assert_eq!(gib(3 << 29), "1.5 GiB");
}

#[test]
fn a_named_limactl_is_the_only_one_tried() {
    let code = |r: Result<String, Fail>| r.map_err(|f| f.code);
    let none = |_: &str| false;
    let brew = |p: &str| p == "/opt/homebrew/bin/limactl";
    let cache = "/c/tooling.noindex/lima-2.2.1/bin/limactl";
    assert_eq!(
        code(pick_limactl(None, Some(cache), brew)),
        Ok("/opt/homebrew/bin/limactl".into())
    );
    assert_eq!(
        code(pick_limactl(None, Some(cache), |_| true)),
        Ok(cache.into())
    );
    assert_eq!(code(pick_limactl(None, Some(cache), none)), Err(NOT_RUN));
    assert_eq!(
        code(pick_limactl(Some("/x/limactl"), Some(cache), brew)),
        Err(NOT_RUN),
        "no fallback"
    );
    assert_eq!(
        code(pick_limactl(Some("/x/limactl"), None, |_| true)),
        Ok("/x/limactl".into())
    );
    assert_eq!(code(pick_limactl(Some("limactl"), None, |_| true)), Err(2));
}

#[test]
fn a_lane_that_cannot_run_exits_three_and_says_so() {
    let f = not_run("no limactl");
    assert_eq!(f.code, NOT_RUN);
    assert_eq!(f.msg.as_deref(), Some("NOT RUN: no limactl"));
}

/// A boot takes the VM's turn, then claims its disk, then takes the lane, so a run waiting
/// for disk holds no lane that a run holding disk would wait for. With the lane held by
/// another run, the turn and the claim are taken first and only then is the lane found held.
/// Negative control: a claim the volume can never hold is NOT RUN, and the lane is never
/// asked for.
#[test]
fn a_boot_claims_its_disk_before_it_takes_the_lane() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("lab-lane-boot-")
        .tempdir()
        .expect("a scratch dir");
    let path = dir.path().join(".aterm-linux.lane.lock");
    let turn = boot_turn_path(&path);
    let holder = named_lock::Holder::me("lab linux proof", Path::new("/checkouts/holder"));
    let _held = LaneLock::at_as(&path, "linux", &holder, Wait::No, &mut |_| {}, &go_on)
        .unwrap_or_else(|f| panic!("the lane is free: {:?}", f.msg));
    let me = named_lock::Holder::me("lab linux exec", Path::new("/checkouts/waiter"));
    let order = std::cell::RefCell::new(Vec::new());
    let take_turn = || {
        order.borrow_mut().push("turn");
        LaneLock::named(&turn, "linux", TURN, &me, Wait::No, &mut |_| {}, &go_on)
    };
    let refused = LaneLock::boot_with(
        "linux",
        take_turn,
        || {
            order.borrow_mut().push("claim");
            Ok(None)
        },
        || {
            order.borrow_mut().push("lane");
            LaneLock::at_as(&path, "linux", &me, Wait::No, &mut |_| {}, &go_on)
        },
    )
    .err()
    .expect("the lane is held");
    assert_eq!(*order.borrow(), ["turn", "claim", "lane"]);
    assert_eq!(refused.code, NOT_RUN, "{:?}", refused.msg);
    order.borrow_mut().clear();
    let never = LaneLock::boot_with(
        "linux",
        take_turn,
        || {
            order.borrow_mut().push("claim");
            Err(not_run(
                "lab linux boot: needs 10 GiB; the VM can never boot there",
            ))
        },
        || {
            order.borrow_mut().push("lane");
            LaneLock::at_as(&path, "linux", &me, Wait::No, &mut |_| {}, &go_on)
        },
    )
    .err()
    .expect("never fits");
    assert_eq!(*order.borrow(), ["turn", "claim"], "no lane asked for");
    assert!(
        never.msg.is_some_and(|m| m.contains("can never boot")),
        "the claim's NOT RUN"
    );
}

/// F2 (2026-10-30: two queued runs of the Windows lane held 35 and 32.5 GiB for its one
/// 33.8 GiB disk): a run queued behind another run's boot of the same VM claims no disk until
/// that run is over. It waits for the VM's turn, naming its holder, with its claim not yet
/// made, and makes it once the first run lets its lock go. `--no-wait` is NOT RUN at once,
/// naming the holder, with nothing claimed. Negative control: a boot of another VM, whose
/// turn is its own, claims at once beside it.
#[test]
fn queued_boots_of_one_vm_claim_its_disk_one_at_a_time() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("lab-lane-turn-")
        .tempdir()
        .expect("a scratch dir");
    let lane = dir.path().join(".aterm-linux.lane.lock");
    assert_eq!(
        boot_turn_path(&lane),
        dir.path().join(".aterm-linux.boot.lock")
    );
    assert_eq!(
        boot_turn_path(Path::new("/h/windows-lane/.lane.lock")),
        Path::new("/h/windows-lane/.boot.lock")
    );
    let claims = std::sync::Arc::new(std::sync::Mutex::new(Vec::<&str>::new()));
    let boot = |lane: &Path, who: &'static str, wait: Wait, say: &mut dyn FnMut(&str)| {
        let me = named_lock::Holder::me(&format!("lab linux {who}"), Path::new("/checkouts"));
        let claims = std::sync::Arc::clone(&claims);
        LaneLock::boot_with(
            "linux",
            || LaneLock::named(&boot_turn_path(lane), "linux", TURN, &me, wait, say, &go_on),
            move || {
                claims.lock().expect("the claims").push(who);
                Ok(None)
            },
            || LaneLock::at_as(lane, "linux", &me, wait, &mut |_| {}, &go_on),
        )
    };
    let first = boot(&lane, "first", Wait::No, &mut |_| {})
        .unwrap_or_else(|f| panic!("the VM is free: {:?}", f.msg));
    let refused = boot(&lane, "now", Wait::No, &mut |_| {})
        .err()
        .expect("--no-wait does not wait for the VM's turn");
    let why = refused.msg.unwrap_or_default();
    assert_eq!(refused.code, NOT_RUN, "{why}");
    assert!(
        why.contains("(lab linux first, /checkouts, since ")
            && why.contains("holds the VM's turn to boot"),
        "{why}"
    );
    let other = boot(
        &dir.path().join(".aterm-linux-gpu.lane.lock"),
        "other",
        Wait::No,
        &mut |_| {},
    )
    .unwrap_or_else(|f| panic!("another VM's turn is its own: {:?}", f.msg));
    assert_eq!(*claims.lock().expect("the claims"), ["first", "other"]);
    drop(other);

    let (waiting, heard) = std::sync::mpsc::channel();
    let (booted, done) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let held = boot(&lane, "queued", Wait::Block, &mut |line| {
                let _ = waiting.send(line.to_string());
            });
            let _ = booted.send(held.map(drop).map_err(|f| f.msg));
        });
        // The one-minute hang detectors: a held turn is named as its wait starts, and is
        // the waiter's once its holder lets it go.
        let line = heard
            .recv_timeout(std::time::Duration::from_secs(60))
            .expect("the queued run waits for the VM's turn");
        assert!(
            line.starts_with("linux: waiting for pid ")
                && line.contains("(lab linux first, /checkouts, since ")
                && line.contains("which holds the VM's turn to boot"),
            "{line}"
        );
        assert_eq!(
            *claims.lock().expect("the claims"),
            ["first", "other"],
            "the queued run claims nothing while the first holds the VM"
        );
        drop(first);
        done.recv_timeout(std::time::Duration::from_secs(60))
            .expect("the queued run boots once the first is over")
            .expect("its turn, its claim and the lane");
    });
    assert_eq!(
        *claims.lock().expect("the claims"),
        ["first", "other", "queued"]
    );
}

/// A boot claims the VM's directory on its volume, its record beside it; a need the volume
/// can never hold is NOT RUN naming the need, and a ledger that cannot be used boots without
/// a claim.
#[test]
fn a_boot_claims_its_vms_directory_or_cannot_run() {
    let base = Path::new("/Users//u/.lima");
    let run = boot_run(
        "linux",
        base,
        base.join(VM),
        base.join(".aterm-linux.boot-footprint.json"),
        BOOT_COLD_DISK,
    );
    assert_eq!(run.what, "lab linux boot");
    assert_eq!(run.dirs, [base.join(VM)]);
    assert_eq!(run.on, base);
    let mut asked = Vec::new();
    let refused = claim_boot_disk_with("linux", run.clone(), &mut |r, _| {
        asked.push(r);
        Err(atpkg::admit_disk::Error::Never(
            "lab linux boot: needs 10 GiB on /Volumes/s, which holds 6 GiB above its floor".into(),
        ))
    });
    let f = refused.expect_err("never fits");
    assert_eq!(f.code, NOT_RUN);
    let msg = f.msg.unwrap_or_default();
    assert!(
        msg.contains("needs 10 GiB on /Volumes/s") && msg.contains("can never boot"),
        "{msg}"
    );
    assert_eq!(asked, std::slice::from_ref(&run));
    let unclaimed = claim_boot_disk_with("linux", run, &mut |_, _| {
        Err(atpkg::admit_disk::Error::Ledger(std::io::Error::other(
            "read-only",
        )))
    });
    assert!(matches!(unclaimed, Ok(None)));
}

/// A run whose VM was never admitted lets its boot's disk claim go and keeps nothing it took,
/// so the next boot still claims the cold budget less what the VM's directory holds; its
/// negative control, a run whose VM was admitted, keeps what it took, and the next boot
/// claims that. Either way the claim leaves the ledger.
#[test]
fn a_run_that_booted_nothing_keeps_no_boot_footprint() {
    use aterm_capacity::floor::GIB;
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let ledger = aterm_capacity::Ledger::at(dir.path().join("ledger"));
    let mut reading = aterm_capacity::Reading::of_gib(48, 40, 14);
    reading.volume = Some(aterm_capacity::Volume {
        mount: dir.path().to_path_buf(),
        total: 400 * GIB,
        free: 300 * GIB,
        floor: 4 * GIB,
        room: 2 * GIB,
    });
    let machine = aterm_capacity::Fixed::new(reading);
    let vm = dir.path().join(VM);
    std::fs::create_dir_all(&vm).expect("the VM's directory");
    let record = dir.path().join(".aterm-linux.boot-footprint.json");
    let run = boot_run(
        "linux",
        dir.path(),
        vm.clone(),
        record.clone(),
        BOOT_COLD_DISK,
    );
    let admit = || {
        let mut relief = atpkg::admit_disk::Reclaim::of(
            Box::new(|_, _| panic!("a volume with room is relieved of nothing")),
            Box::new(|_, _, _| panic!("a volume with room asks the window nothing")),
            Box::new(|_, _| Vec::new()),
        );
        atpkg::admit_disk::admit_on(
            run.clone(),
            &ledger,
            &machine,
            Admit::Wait,
            aterm_capacity::Pace::default(),
            &mut relief,
            &mut |_| {},
        )
        .expect("room on the volume")
    };
    let claims = || ledger.list().expect("the ledger").len();
    let mut wrote = 0u32;
    let mut write = || {
        wrote += 1;
        std::fs::write(vm.join(format!("disk-{wrote}")), vec![1u8; 1 << 20]).expect("write");
    };

    let first = admit();
    assert_eq!(
        (first.estimate.runs, claims()),
        (0, 1),
        "the cold budget, claimed"
    );
    write();
    boot_disk_done("linux", first, false);
    assert_eq!(claims(), 0, "the claim was let go");
    assert!(
        !record.exists(),
        "a run that booted nothing keeps no footprint"
    );
    let second = admit();
    assert_eq!(
        second.estimate.runs, 0,
        "the next boot claims the cold budget again"
    );
    assert!(
        second.estimate.bytes > BOOT_COLD_DISK - GIB,
        "{}",
        second.estimate
    );

    write();
    boot_disk_done("linux", second, true);
    assert_eq!(claims(), 0, "the claim was let go");
    let third = admit();
    assert_eq!(
        third.estimate.runs, 1,
        "a run whose VM was admitted keeps what it took"
    );
    assert!(third.estimate.bytes < GIB, "{}", third.estimate);
    boot_disk_done("linux", third, true);
}

/// Set, the test binary is [`standin_boot_disk_claim_interrupted`].
const BOOT_STANDIN: &str = "ATERM_LAB_BOOT_DISK_INTERRUPT_STANDIN";

/// Run by [`an_interrupted_boot_disk_claim_ends_the_run_not_run`] in a process of its own,
/// since the interrupt becomes the run's: a boot's disk claim that an interrupt (SIGTERM)
/// ended is NOT RUN carrying it, the run's own interrupt path has it, and a later boot's
/// claim is refused without being asked for; it exits as the lab's main would.
#[test]
fn standin_boot_disk_claim_interrupted() {
    if std::env::var_os(BOOT_STANDIN).is_none() {
        return;
    }
    let base = Path::new("/Users//u/.lima");
    let run = boot_run(
        "linux",
        base,
        base.join(VM),
        base.join(".aterm-linux.boot-footprint.json"),
        BOOT_COLD_DISK,
    );
    let f = claim_boot_disk_with("linux", run.clone(), &mut |_, _| {
        Err(atpkg::admit_disk::Error::Stopped {
            signal: Some(libc::SIGTERM),
            short: None,
        })
    })
    .expect_err("the interrupt ended the claim");
    assert_eq!((f.code, f.stopped), (NOT_RUN, Some(libc::SIGTERM)));
    assert!(
        crate::stop::ended_a_wait(),
        "the claim handed its interrupt on"
    );
    assert_eq!(
        crate::stop::check(),
        Err(crate::stop::Stopped(libc::SIGTERM))
    );
    let again = claim_boot_disk_with("linux", run, &mut |_, _| {
        panic!("a boot's disk claim asked for once the run is ending")
    })
    .expect_err("no claim once the run is ending");
    println!("{}", f.msg.unwrap_or_default());
    println!("{}", again.msg.unwrap_or_default());
    std::process::exit(crate::stop::status(f.code, false));
}

/// An interrupt that ended a boot's disk claim ends the run NOT RUN (exit 3), naming it,
/// and no later boot of the run asks for a claim.
#[test]
fn an_interrupted_boot_disk_claim_ends_the_run_not_run() {
    let mut cmd = aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"));
    cmd.args([
        "--exact",
        "linux::tests::standin_boot_disk_claim_interrupted",
        "--test-threads=1",
        "--nocapture",
    ])
    .env(BOOT_STANDIN, "1");
    let out = aterm_uds::deadline::output_within(&mut cmd, aterm_spec::wait::MUST_HAPPEN)
        .expect("the stand-in ends within the hang detector");
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(NOT_RUN), "{}\n{text}", out.status);
    assert!(
        text.contains("NOT RUN: linux: the boot's disk claim was interrupted (SIGTERM)"),
        "{text}"
    );
    assert!(
        text.contains("NOT RUN: linux: the boot's disk claim was not asked for"),
        "{text}"
    );
}

use crate::linux_driver::fake::{Answer, Call, Guest};

fn up_on(guest: &Guest, booted_here: bool) -> Up {
    super::fake_up(guest, booted_here)
}

/// The guest's `cargo` runs as many jobs as cores this run was admitted, on every arch, on
/// top of the arch's own environment.
#[test]
fn the_guests_cargo_runs_the_jobs_admitted() {
    let up = up_on(&Guest::new(), false);
    for arch in [Arch::Aarch64, Arch::X86_64] {
        let env = up.cargo_env(arch, "/t", false);
        assert!(
            env.contains(&("CARGO_BUILD_JOBS".into(), "3".into())),
            "{env:?}"
        );
        assert!(
            env.starts_with(&guest::cargo_env(arch, "/t", false)),
            "{env:?}"
        );
    }
}

fn ends(guest: &Guest) -> Vec<Call> {
    guest
        .calls()
        .into_iter()
        .filter(|c| matches!(c, Call::End { .. }))
        .collect()
}

#[test]
fn a_runs_end_stops_its_units_then_lets_the_driver_end_or_keep_the_vm() {
    let guest = Guest::new();
    up_on(&guest, true).release();
    let calls = guest.calls();
    let stops: Vec<_> = guest::stop_units().into_iter().collect();
    assert!(
        matches!(&calls[..], [Call::Shell { argv: a, .. }, Call::Shell { argv: b, .. }, Call::End { booted_here: true, keep: false }] if *a == stops[0] && *b == stops[1]),
        "{calls:?}"
    );
    // --keep-up and `up`: kept, whoever booted it.
    let guest = Guest::new();
    up_on(&guest, false).end(true);
    assert_eq!(
        ends(&guest),
        [Call::End {
            booted_here: false,
            keep: true
        }]
    );
}

#[test]
fn provisioning_runs_in_place_only_when_the_guests_stamp_is_another_plans() {
    let steps = guest::provision_steps();
    let want = guest::fingerprint(&steps);
    let current = Guest::new().answer(
        &["cat", guest::STAMP],
        Answer::Exited(0, format!("{want}\n")),
    );
    assert_eq!(provision(&up_on(&current, true)).ok(), Some(None));
    assert_eq!(current.calls().len(), 1, "only the stamp was read");
    // A stale stamp: every step, in order, through the driver, then the new stamp.
    let stale = Guest::new().answer(&["cat", guest::STAMP], Answer::Exited(0, "0123\n".into()));
    assert!(matches!(provision(&up_on(&stale, true)), Ok(Some(_))));
    let ran: Vec<Vec<String>> = stale
        .calls()
        .into_iter()
        .filter_map(|c| match c {
            Call::Shell { argv, .. } => Some(argv),
            _ => None,
        })
        .collect();
    for (i, s) in steps.iter().enumerate() {
        assert_eq!(ran[1 + i], s.argv, "step {i}, {}", s.label);
    }
    assert!(
        stale.calls().iter().any(|c| matches!(c, Call::Shell { argv, stdin: Some(t), .. } if argv.last().map(String::as_str) == Some(guest::STAMP) && *t == format!("{want}\n"))),
        "the stamp is the plan's"
    );
}

#[test]
fn a_failed_provisioning_step_keeps_the_error_before_its_progress_tail() {
    let diagnostic = "dpkg: actionable package failure before later progress";
    let output = format!(
        "{diagnostic}\n{}",
        (0..64)
            .map(|n| format!("later package progress {n}\n"))
            .collect::<String>()
    );
    let guest = Guest::new().answer(&["apt-get", "install"], Answer::Exited(100, output));
    let fail = provision(&up_on(&guest, true)).unwrap_err();
    assert_eq!(fail.code, NOT_RUN);
    let message = fail.msg.unwrap_or_default();
    assert!(message.contains("provisioning step 'apt-get install' failed (exit 100)"));
    assert!(message.contains(diagnostic), "{message}");
    assert!(message.contains("later package progress 63"), "{message}");
    assert!(!guest.calls().iter().any(|call| matches!(
        call,
        Call::Shell { argv, .. }
            if argv.first().is_some_and(|word| word == "rustup")
                || (argv[0] == "sudo" && argv.iter().any(|word| word == guest::STAMP))
    )));
}

#[test]
fn a_step_that_a_signal_ended_never_started_or_was_lost_names_why_and_stops_provisioning() {
    for (answer, words) in [
        (
            Answer::Signaled(9),
            [
                "(ended on signal 9)",
                "(the guest command ended on signal 9)",
            ],
        ),
        (
            Answer::NotStarted("rustup: No such file or directory (os error 2)".into()),
            [
                "(not started: rustup: No such file or directory (os error 2))",
                "",
            ],
        ),
        (
            Answer::Lost("the guest stopped answering (no heartbeat for 60 s); VM stopped".into()),
            [
                "(lost: the guest stopped answering (no heartbeat for 60 s); VM stopped)",
                "",
            ],
        ),
    ] {
        // `rustup` alone is also one of apt's packages.
        let guest = Guest::new().answer(&["rustup", "toolchain"], answer);
        let fail = provision(&up_on(&guest, true)).unwrap_err();
        assert_eq!(fail.code, NOT_RUN);
        let msg = fail.msg.unwrap_or_default();
        assert!(
            msg.contains("provisioning step 'rust toolchain' failed")
                && words.iter().all(|w| msg.contains(w)),
            "{msg}"
        );
        // No later step ran, and no stamp was written.
        assert!(!guest.calls().iter().any(
            |c| matches!(c, Call::Shell { argv, .. } if argv.iter().any(|a| a == guest::STAMP) && argv[0] == "sudo")
        ));
    }
}

#[test]
fn up_keeps_the_vm_and_runs_nothing_more_in_the_guest() {
    let guest = Guest::new();
    up_on(&guest, true).keep();
    assert_eq!(
        guest.calls(),
        [Call::End {
            booted_here: true,
            keep: true
        }],
        "no unit is stopped again: the bring-up did that, as lima's up"
    );
    // The negative control: a run's end with --keep-up stops its units first.
    let guest = Guest::new();
    up_on(&guest, true).end(true);
    assert_eq!(guest.calls().len(), 1 + guest::stop_units().len());
}

fn out(code: Option<i32>, signal: Option<i32>) -> Out {
    Out {
        code,
        text: "before\n".into(),
        secs: 0.1,
        signal,
        not_started: None,
        lost: None,
    }
}

/// A start that failed on a VM lima reads running: a guest agent systemd gave up on (as
/// after a krunkit abort, measured 2026-10-29) is reset and restarted and the boot goes on;
/// anything else stops the VM, so the failed start leaves nothing up.
#[test]
fn a_failed_start_revives_a_failed_guest_agent_or_stops_the_vm() {
    let run = |running: bool, failed: bool, restarts: bool| {
        let (mut scripts, mut stops) = (Vec::new(), 0);
        let result = after_failed_start(
            running,
            &mut |script| {
                scripts.push(script.to_string());
                let ok = if script.starts_with("systemctl is-failed") {
                    failed
                } else {
                    restarts
                };
                out(Some(i32::from(!ok)), None)
            },
            &mut || {
                stops += 1;
                out(Some(0), None)
            },
        );
        (result, scripts.len(), stops)
    };
    let (revived, scripts, stops) = run(true, true, true);
    assert!(
        revived
            .as_ref()
            .is_ok_and(|l| l.contains("reset and restarted")),
        "{revived:?}"
    );
    assert_eq!((scripts, stops), (2, 0), "revived, the VM stays up");
    // The negative controls: an agent that did not fail, or would not restart, and a VM
    // that is not running.
    let (stopped, scripts, stops) = run(true, false, true);
    assert!(
        stopped.as_ref().is_err_and(|e| e.contains("is stopped")),
        "{stopped:?}"
    );
    assert_eq!((scripts, stops), (1, 1));
    let (stopped, scripts, stops) = run(true, true, false);
    assert!(stopped.is_err());
    assert_eq!((scripts, stops), (2, 1));
    assert_eq!(run(false, true, true), (Err(String::new()), 0, 0));
}

#[test]
fn exec_names_each_end_that_was_not_an_exit_and_exits_apart() {
    let run = |o: &Out| {
        let mut said = Vec::new();
        let code = exec_exit(o, &mut |w| said.push(w.to_string()));
        (code.map_err(|f| (f.code, f.msg.unwrap_or_default())), said)
    };
    // The negative control: an exit is its own code, and nothing more is said.
    assert_eq!(run(&out(Some(7), None)), (Ok(7), vec![]));
    assert_eq!(run(&out(Some(0), None)), (Ok(0), vec![]));
    assert_eq!(
        run(&out(None, Some(9))),
        (
            Ok(137),
            vec!["linux exec: the command ended on signal 9".to_string()]
        )
    );
    let mut missing = out(None, None);
    missing.not_started = Some("/nonexistent: No such file or directory (os error 2)".into());
    assert_eq!(
        run(&missing),
        (
            Ok(127),
            vec![
                "linux exec: the command was not started: /nonexistent: No such file or \
                 directory (os error 2)"
                    .to_string()
            ]
        )
    );
    let mut lost = out(None, None);
    lost.lost = Some("the guest stopped answering (no heartbeat for 60 s); VM stopped".into());
    let (code, said) = run(&lost);
    let (exit, msg) = code.unwrap_err();
    assert_eq!(exit, NOT_RUN);
    assert!(
        msg.contains("lost under the command: the guest stopped answering"),
        "{msg}"
    );
    assert!(said.is_empty());
    let mut limactl = out(None, None);
    limactl.text = "half\n(limactl ended on signal 15)\n".into();
    assert_eq!(
        run(&limactl),
        (
            Ok(1),
            vec!["linux exec: no exit status: (limactl ended on signal 15)".to_string()]
        )
    );
}

#[test]
fn a_failures_tail_names_an_end_its_text_cannot() {
    let mut o = out(None, None);
    o.text = "a\nb\nc\n".into();
    // The negative control: an exit's tail is its text alone.
    assert_eq!(o.tail(2), "b\nc");
    o.not_started = Some("no cwd".into());
    assert_eq!(o.tail(2), "b\nc\n(not started: no cwd)");
    assert_eq!(o.status(), "not started: no cwd");
    o.lost = Some("gone".into());
    assert_eq!(o.tail(1), "c\n(lost: gone)");
    o.text.clear();
    assert_eq!(o.tail(5), "(lost: gone)");
    assert_eq!(out(None, Some(15)).status(), "ended on signal 15");
}

#[test]
fn the_host_disk_and_version_lines_come_from_the_driver() {
    let mut guest = Guest::new();
    guest.host_disk = Some(crate::linux_driver::HostDisk {
        bytes: 5 << 30,
        path: "/c/vz.noindex/live/aterm-linux.img".into(),
    });
    let up = up_on(&guest, false);
    assert_eq!(
        up.guest.host_disk().map(|d| gib(d.bytes)),
        Some("5.0 GiB".into())
    );
    assert_eq!(up.guest.version(), "fake guest");
}
