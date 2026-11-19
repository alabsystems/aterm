// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! W-kin's law on real processes: a test binary run under [`run`] with a run's record starts two
//! `/bin/sh` loops that leave its group, one for a group of its own (`process_group(0)`) and one
//! for a session of its own (`setsid`), itself or through a process it starts in its own group,
//! and each loop is gone once the binary has left its run, whether the run's ceiling ended the
//! binary or the binary `SIGKILL`ed itself. Each loop runs while a holder process this test
//! starts lives ([`aterm_uds::stand_in_loop::while_lives`]), so no loop can end by itself while
//! the test reads it; the test never signals a loop, and ending the holder ends any loop left.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_uds::runs::Kin;
use aterm_uds::spawnfd::Stdio;

use super::super::{CaseWatch, Cmd, ExecEnv, Host, KinRow, LIVE_HOST, Run, TreeMember, run};

/// The fixture's mode: `hold` blocks until its run ends it, `die` `SIGKILL`s itself once
/// released; `middle` is the process such a binary starts in its group to start the loops.
const FIXTURE: &str = "ATERM_VERIFY_KIN_FIXTURE";
/// Set where the binary starts its loops through a `middle` process in its own group.
const MIDDLE: &str = "ATERM_VERIFY_KIN_MIDDLE";
/// The fixture's test, by its full name.
const FIXTURE_TEST: &str = "exec::kin::tests::kin_fixture";
/// Where the fixture writes its loops' pids and binds its release socket.
const DIR: &str = "ATERM_VERIFY_KIN_DIR";
/// The pid each loop runs while.
const HOLDER: &str = "ATERM_VERIFY_KIN_HOLDER";
/// The loops, by the file each writes its pid to.
const LOOPS: [&str; 2] = ["group", "session"];
/// The work ceiling the `hold` run ends its binary at, which only the host's jump reaches.
const CEILING: Duration = Duration::from_secs(30 * 60);

/// Start the two loops, each writing its pid into `dir`, each running while `holder` lives.
fn start_loops(dir: &Path, holder: &str) -> Vec<aterm_uds::spawnfd::Child> {
    let body = aterm_uds::stand_in_loop::while_lives(holder, "sleep 1");
    let mut loops = Vec::new();
    for name in LOOPS {
        let mut sh = aterm_uds::spawnfd::command("/bin/sh");
        sh.args([
            "-c",
            &format!("echo $$ > '{}'; {body}", dir.join(name).display()),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
        if name == "group" {
            sh.process_group(0);
        } else {
            sh.setsid(true);
        }
        loops.push(sh.spawn().expect("a loop starts"));
    }
    loops
}

/// The stand-in test binary: it binds its release socket, starts its two loops (itself, or
/// through a `middle` process it starts in its own group) and waits on the socket. A `middle`
/// process starts the loops and reads its input to its end, which comes when the binary that
/// holds the other end is gone. There is no time-based fixture behavior.
#[test]
fn kin_fixture() {
    let Some(mode) = std::env::var_os(FIXTURE) else {
        return;
    };
    let dir = PathBuf::from(std::env::var_os(DIR).expect("the fixture's directory"));
    let holder = std::env::var(HOLDER).expect("the holder's pid");
    if mode == "middle" {
        let _loops = start_loops(&dir, &holder);
        let _ = std::io::copy(&mut std::io::stdin(), &mut std::io::sink());
        return;
    }
    let socket = std::os::unix::net::UnixDatagram::bind(dir.join("release.sock"))
        .expect("bind the release socket");
    let (_loops, _middle) = if std::env::var_os(MIDDLE).is_some() {
        let mut middle =
            aterm_uds::spawnfd::command(std::env::current_exe().expect("this test binary"));
        middle
            .args(["--exact", FIXTURE_TEST, "--test-threads=1"])
            .env(FIXTURE, "middle")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        (
            Vec::new(),
            Some(middle.spawn().expect("the middle process starts")),
        )
    } else {
        (start_loops(&dir, &holder), None)
    };
    let mut byte = [0_u8];
    socket.recv(&mut byte).expect("the release");
    match mode.to_str() {
        Some("die") => aterm_uds::process::kill_self_now(),
        other => panic!("{other:?} is released by no test"),
    }
}

/// The pid `dir`'s loop `name` wrote, once whole.
fn written(dir: &Path, name: &str) -> Option<i32> {
    let text = std::fs::read_to_string(dir.join(name)).ok()?;
    text.strip_suffix('\n')?.trim().parse().ok()
}

/// Both loops' pids, once both are written.
fn loops(dir: &Path) -> Option<[i32; 2]> {
    Some([written(dir, LOOPS[0])?, written(dir, LOOPS[1])?])
}

/// Which of the host's kin reads are cut off ([`KinHost`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Blind {
    /// Neither.
    No,
    /// The reads at the clock's cadence ([`Host::tree_kin`]): only a kill path's read books.
    Cadence,
    /// Both: only the binary's end books.
    All,
}

/// The live host, its kin reads cut off as `blind` says, and its clock jumped an hour once both
/// loops are written: past [`CEILING`] and short of the case watch's two hours.
#[derive(Debug)]
struct KinHost {
    dir: PathBuf,
    blind: Blind,
    jump: bool,
}

impl Host for KinHost {
    fn monotonic(&self) -> Instant {
        let now = Instant::now();
        if self.jump && loops(&self.dir).is_some() {
            now + Duration::from_secs(60 * 60)
        } else {
            now
        }
    }
    fn group_usage(&self, pgid: u32) -> Option<Vec<aterm_sysprobe::MemberUsage>> {
        LIVE_HOST.group_usage(pgid)
    }
    fn tree_usage(&self, root: u32) -> Option<Vec<TreeMember>> {
        LIVE_HOST.tree_usage(root)
    }
    fn fresh_tree_usage(&self, root: u32) -> Option<Vec<TreeMember>> {
        LIVE_HOST.fresh_tree_usage(root)
    }
    fn tree_kin(&self, root: u32) -> Option<Vec<KinRow>> {
        (self.blind == Blind::No)
            .then(|| LIVE_HOST.tree_kin(root))
            .flatten()
    }
    fn fresh_tree_kin(&self, root: u32) -> Option<Vec<KinRow>> {
        (self.blind != Blind::All)
            .then(|| LIVE_HOST.fresh_tree_kin(root))
            .flatten()
    }
    fn idle(&self) -> Option<Duration> {
        None
    }
    fn cores(&self) -> u32 {
        LIVE_HOST.cores()
    }
}

/// The process each loop runs while: a `cat` this test starts, ended by closing its input.
struct Holder(aterm_uds::spawnfd::Child);

impl Holder {
    fn start() -> Self {
        let mut cat = aterm_uds::spawnfd::command("/bin/cat");
        cat.stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        Self(cat.spawn().expect("the holder starts"))
    }
}

impl Drop for Holder {
    fn drop(&mut self) {
        drop(self.0.stdin.take());
        let _ = self.0.wait();
    }
}

/// What one run of the fixture left.
struct Ran {
    run: Run,
    loops: [i32; 2],
    booked: BTreeSet<Kin>,
}

/// Release the fixture in `dir`: a `die` one `SIGKILL`s itself, a `hold` one panics.
fn release(dir: &Path) {
    std::os::unix::net::UnixDatagram::unbound()
        .and_then(|socket| socket.send_to(&[1], dir.join("release.sock")))
        .expect("release the fixture");
}

/// Wait for `done` while the stage thread runs, at most the wall minute; `false` past it.
fn waited(stage: &std::thread::ScopedJoinHandle<'_, Run>, mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while Instant::now() < deadline {
        if done() || stage.is_finished() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

/// Run the fixture in `mode` under a run's record on a [`KinHost`] (`blind`; its clock jumped in
/// `hold` mode), its loops started through a `middle` process where `middle`, the ender spared
/// where `spare`, and return once the ender has ended what the binary's end handed it. A `die`
/// binary is released once both loops are written, and where no read is cut off only once the
/// kin book holds both.
///
/// The fixture's start is waited for while this process and its descendants work
/// ([`aterm_spec::wait::until_while_working`]), and ends with the stage, which a binary that
/// never starts ends. The other waits are this process's own work, its stage thread's bounded
/// reads and its kill path, each a wall minute: past one the fixture is released, so a `hold`
/// binary panics and the stage returns, and the test fails.
fn ran(mode: &str, blind: Blind, spare: bool, middle: bool, holder: &Holder, dir: &Path) -> Ran {
    let records = dir.join("records");
    let ctx = crate::Ctx::new(
        dir.to_path_buf(),
        crate::Scope::workspace(),
        crate::EnvSnapshot::default(),
        dir.join("scratch"),
    );
    let runs = crate::runs::Runs::open(&ctx, &records, |_, _| Run {
        case_observations: None,
        stopped_at: None,
        hung: None,
        ok: false,
        output: String::new(),
        code: None,
        spawn_error: None,
        usage: None,
    })
    .expect("the run's record");
    if spare {
        runs.sparing_kin();
    }
    let host = KinHost {
        dir: dir.to_path_buf(),
        blind,
        jump: mode == "hold",
    };
    let binary = std::env::current_exe().expect("this test binary");
    let mut cmd = Cmd::new(&binary)
        .args([
            "-Z",
            "unstable-options",
            "--format",
            "json",
            "--exact",
            FIXTURE_TEST,
            "--test-threads=1",
        ])
        .env(FIXTURE, mode)
        .env(DIR, dir)
        .env(HOLDER, holder.0.id().to_string());
    if middle {
        cmd = cmd.env(MIDDLE, "1");
    }
    cmd.test_cases = Some(CaseWatch {
        spec: "-p aterm-verify --lib".into(),
        binary: binary.display().to_string(),
        json: true,
        stop_at_red: false,
        hang_after: Duration::from_secs(2 * 60 * 60),
    });
    let env = ExecEnv {
        events: None,
        cwd: dir,
        path: std::ffi::OsStr::new("/usr/bin:/bin"),
        scratch: dir,
        child_ceiling: Some(CEILING),
        remove_env: &[],
        add_env: &[],
        stage_groups: Some(&runs),
        host: &host,
        hang_samples: None,
    };
    std::thread::scope(|scope| {
        let stage = scope.spawn(|| run(&cmd, env));
        let me = std::process::id();
        aterm_spec::wait::until_while_working(me, "both loops write their pids", || {
            (loops(dir).is_some() || stage.is_finished()).then_some(())
        });
        let Some(pids) = loops(dir) else {
            let run = stage.join().expect("the stage thread");
            panic!("the fixture ended before both loops started: {run:?}");
        };
        if mode == "die" {
            let booked = || {
                let booked = runs.kin_booked();
                pids.iter().all(|&pid| booked.iter().any(|k| k.pid == pid))
            };
            if blind == Blind::No && !waited(&stage, booked) {
                release(dir);
                let _ = stage.join();
                panic!(
                    "the kin book never held both loops: {:?}",
                    runs.kin_booked()
                );
            }
            release(dir);
        }
        if !waited(&stage, || false) {
            release(dir);
            let _ = stage.join();
            panic!("the run never ended its binary");
        }
        let run = stage.join().expect("the stage thread");
        runs.drain_kin();
        Ran {
            run,
            loops: pids,
            booked: runs.kin_booked(),
        }
    })
}

/// Whether `pid` still reads as the booked process `kin` names.
fn lives_as(pid: i32, booked: &BTreeSet<Kin>) -> bool {
    aterm_uds::process::read_lineage(pid).is_some_and(|lin| booked.contains(&Kin::of(&lin)))
}

/// Each loop is booked by its identity and gone once its binary has left the run. Gone is read
/// after the ender finished: what is left is launchd's or init's reap of an ended loop, another
/// process whose progress this test cannot read, so its wait is the wall minute.
fn each_loop_gone(ran: &Ran) {
    for pid in ran.loops {
        assert!(
            ran.booked.iter().any(|kin| kin.pid == pid),
            "loop {pid} was booked: {:?}",
            ran.booked
        );
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            &format!("loop {pid} is gone"),
            || (!lives_as(pid, &ran.booked)).then_some(()),
        );
    }
}

/// The run's ceiling ends a binary whose loops left its group and its session: a tree read books
/// them (at the clock's cadence, or the kill path's before its group signal), and each is gone
/// once the binary is reaped. NEGATIVE CONTROL:
/// [`without_the_ender_both_loops_outlive_their_binary`].
#[test]
fn the_ceiling_ends_a_binary_and_the_loops_it_left() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let holder = Holder::start();
    let ran = ran("hold", Blind::No, false, false, &holder, dir.path());
    assert!(
        !ran.run.ok && ran.run.output.contains(crate::findings::CEILING_KILL),
        "the ceiling ended it: {:?}",
        ran.run
    );
    each_loop_gone(&ran);
    let sidecar = std::fs::read_dir(dir.path().join("records"))
        .expect("the records")
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "kin"))
        .expect("a kin sidecar");
    let kept = aterm_uds::runs::read_kin(&sidecar)
        .expect("the sidecar reads")
        .expect("a whole sidecar");
    for pid in ran.loops {
        assert!(kept.kin.iter().any(|k| k.pid == pid), "{kept:?}");
    }
}

/// A binary that `SIGKILL`s itself leaves its loops; each is gone once the binary is reaped. On
/// Apple no tree read books them (the host is blind): the binary's end finds them by the parent
/// unique ids they name. On Linux no kernel record ties an orphan to the binary, so the test
/// releases the binary only once the tree reads booked both.
#[test]
fn a_binary_that_kills_itself_leaves_no_loop() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let holder = Holder::start();
    let blind = if cfg!(target_vendor = "apple") {
        Blind::All
    } else {
        Blind::No
    };
    let ran = ran("die", blind, false, false, &holder, dir.path());
    assert!(
        !ran.run.ok && !ran.run.output.contains(crate::findings::CEILING_KILL),
        "it ended by its own signal: {:?}",
        ran.run
    );
    each_loop_gone(&ran);
}

/// NEGATIVE CONTROL for both: with the ender spared (`Runs::sparing_kin`), both booked loops
/// still live after their binary was reaped, so neither law's loop ends by itself; the holder's
/// end then ends them.
#[test]
fn without_the_ender_both_loops_outlive_their_binary() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let holder = Holder::start();
    let ran = ran("hold", Blind::No, true, false, &holder, dir.path());
    for pid in ran.loops {
        assert!(lives_as(pid, &ran.booked), "loop {pid} outlives its binary");
    }
    drop(holder);
    // Each loop must notice that its holder is gone: its own work, waited for while it and its
    // `sleep` make progress.
    for pid in ran.loops {
        aterm_spec::wait::until_while_working(
            pid.unsigned_abs(),
            &format!("loop {pid} ends once its holder is gone"),
            || (!lives_as(pid, &ran.booked)).then_some(()),
        );
    }
}

/// A kill path's read of the tree before its group signal books what no other read can: the
/// binary's loops were started by a process in the binary's own group, which the ceiling's group
/// signal ends with the binary, and the host's reads at the clock's cadence are cut off. On Linux
/// nothing is booked at the binary's end but the groups of what was booked; on Apple the chain
/// of unique ids the closure follows runs through that ended process, which no listing holds
/// once it is gone. So only the read the ceiling makes before its signal binds each loop,
/// through that process while it lives, and each is gone once the binary is reaped. NEGATIVE
/// CONTROL: let a kill path read nothing into the kin book (`book_kin(true)` a no-op), and
/// neither loop is booked.
#[test]
fn a_kill_paths_read_books_what_a_process_of_the_binarys_group_left() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let holder = Holder::start();
    let ran = ran("hold", Blind::Cadence, false, true, &holder, dir.path());
    assert!(
        !ran.run.ok && ran.run.output.contains(crate::findings::CEILING_KILL),
        "the ceiling ended it: {:?}",
        ran.run
    );
    each_loop_gone(&ran);
}
