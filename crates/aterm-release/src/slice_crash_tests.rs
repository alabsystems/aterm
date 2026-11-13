// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Real-process interruption of the slice admission and driver boundaries.

use super::*;
use aterm_capacity::{Fixed, Ledger, Need, Reading};
use aterm_uds::spawnfd::{Child, Stdio};
use std::time::Duration;

const ROLE: &str = "ATERM_RELEASE_SLICE_CRASH_ROLE";
const ROOT: &str = "ATERM_RELEASE_SLICE_CRASH_ROOT";
const HANG: Duration = Duration::from_secs(60);

fn tether_exe() -> PathBuf {
    std::env::var_os("ATERM_CRASH_TETHER")
        .map(PathBuf::from)
        .unwrap_or_else(|| aterm_tempfile::bin_exe("aterm-release"))
}

fn await_file(path: &Path) -> String {
    aterm_spec::wait::within(HANG, Duration::from_millis(10), || {
        std::fs::read_to_string(path).ok().filter(|s| !s.is_empty())
    })
    .unwrap_or_else(|| panic!("no handshake at {}", path.display()))
}

fn actor(role: &str, root: &Path) -> Child {
    let mut cmd = command(std::env::current_exe().unwrap());
    cmd.args([
        "--exact",
        "buildplan::crash_tests::slice_crash_actor",
        "--nocapture",
    ])
    .env(ROLE, role)
    .env(ROOT, root)
    .default_signals()
    .env("ATERM_CRASH_TETHER", tether_exe())
    .stdout(Stdio::null())
    .stderr(Stdio::inherit());
    cmd.spawn().unwrap()
}

#[test]
fn slice_crash_actor() {
    let Ok(role) = std::env::var(ROLE) else {
        return;
    };
    let root = PathBuf::from(std::env::var_os(ROOT).unwrap());
    if role == "cutter" {
        let ledger = Ledger::at(root.join("ledger"));
        let machine = Fixed::new(Reading::of_gib(128, 96, 8));
        let _claim = aterm_capacity::admit::try_admit_with(
            &ledger,
            &machine,
            Need::new("release slice memory budget")
                .mem_gib(4)
                .margin(1),
        )
        .unwrap()
        .unwrap();
        let pool = aterm_capacity::Jobserver::new(&root.join("jobs"), 3).unwrap();
        let mut cmd = driver_command(&root);
        pool.hand_to(&mut cmd);
        let _implicit = pool.acquire(&|| false).unwrap();
        let child = crate::interrupt::GroupChild::spawn(&mut cmd).unwrap();
        await_file(&root.join("ready"));
        std::fs::write(root.join("cutter.ready"), child.id().to_string()).unwrap();
        wait_with_peak(child).unwrap();
        return;
    }
    if role == "cargo" {
        aterm_uds::tether::through(PathBuf::from(
            std::env::var_os("ATERM_CRASH_TETHER").unwrap(),
        ));
        let pool = aterm_capacity::Jobserver::inherited().unwrap().unwrap();
        let _borrowed = pool.acquire_many(2, &|| false).unwrap();
        let args = vec![
            "/bin/sh".into(),
            "-c".into(),
            "echo $$ > \"$0\"; exec /bin/sleep 600".into(),
            root.join("compiler.pid").into_os_string(),
        ];
        let mut compiler = crate::slice_units::compiler(&args)
            .unwrap()
            .spawn()
            .unwrap();
        // Build scripts do not acquire a wrapper tether. This member is the negative
        // control for mistaking Cargo's death (or its compiler's tether) for group drain.
        let _build_script = command("/bin/sh")
            .args(["-c", "echo $$ > \"$0\"; exec /bin/sleep 600"])
            .arg(root.join("build-script.pid"))
            .spawn()
            .unwrap();
        await_file(&root.join("compiler.pid"));
        await_file(&root.join("build-script.pid"));
        std::fs::write(root.join("ready"), b"cargo holds two GNU tokens").unwrap();
        compiler.wait().unwrap();
        return;
    }
    crate::interrupt::install();
    let ledger = Ledger::at(root.join("ledger"));
    let mut reading = Reading::of_gib(96, 35, 8);
    reading.pressure = Some(aterm_sysprobe::MemoryLevel::Normal);
    let machine = Fixed::new(reading);
    let elapsed = std::cell::Cell::new(Duration::ZERO);
    let result = admit_slice_parent_in(
        &ledger,
        &machine,
        Need::new("release slice memory budget")
            .mem_gib(44)
            .margin(1),
        Need::new(crate::slice_units::SERIAL_PARENT)
            .mem_gib(40)
            .margin(1),
        || elapsed.get(),
        || {
            elapsed.set(if role == "serial" {
                Duration::from_secs(60)
            } else {
                Duration::ZERO
            });
            let waiting = ledger.list().unwrap();
            let ready = waiting.iter().any(|r| {
                r.need.what
                    == if role == "serial" {
                        crate::slice_units::SERIAL_PARENT
                    } else {
                        "release slice memory budget"
                    }
            });
            if ready {
                std::fs::write(root.join("ready"), role.as_bytes()).unwrap();
            }
            std::thread::sleep(Duration::from_millis(10));
            crate::interrupt::caught().is_none()
        },
        |_| {},
    );
    assert!(result.unwrap_err().contains("interrupted"));
    assert!(ledger.list().unwrap().is_empty());
}

/// SIGINT in each actual admission queue drops its waiter. A new cut can claim the same
/// ledger; the negative control before the interrupt observes the queued reservation.
#[test]
fn sigint_shared_and_serial_admission_leave_no_phantom_waiter() {
    for role in ["shared", "serial"] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let mut child = actor(role, dir.path());
        assert_eq!(await_file(&dir.path().join("ready")), role);
        let ledger = Ledger::at(dir.path().join("ledger"));
        assert_eq!(ledger.list().unwrap().len(), 1);
        crate::interrupt::send(child.id().cast_signed(), crate::interrupt::SIGINT);
        assert!(child.wait().unwrap().success());
        assert!(ledger.list().unwrap().is_empty());
        let machine = Fixed::new(Reading::of_gib(128, 96, 8));
        let claim = aterm_capacity::admit::try_admit_with(
            &ledger,
            &machine,
            Need::new("next release").mem_gib(44).margin(1),
        )
        .unwrap()
        .unwrap();
        drop(claim);
        assert!(ledger.list().unwrap().is_empty());
    }
}

/// Kill the driver while two GNU tokens are outside its FIFO. Production group draining
/// must precede share recovery: recovery then yields exactly four tokens, once only.
#[test]
fn sigkill_cargo_drains_compiler_before_recovering_exactly_its_gnu_tokens() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path();
    let shares = aterm_capacity::BranchShares::new(&root.join("jobs"), 4, 0, [true, true]).unwrap();
    let pool = shares.pool(false);
    let mut cmd = driver_command(root);
    pool.hand_to(&mut cmd);
    // This libtest stand-in does not parse Cargo's -j, but consumes the exact production
    // implicit token; its two workers draw directly from the same GNU FIFO.
    let implicit = pool.acquire(&|| false).unwrap();
    let mut child = crate::interrupt::GroupChild::spawn(&mut cmd).unwrap();
    await_file(&root.join("ready"));
    let compiler = await_file(&root.join("compiler.pid"))
        .trim()
        .parse::<i32>()
        .unwrap();
    let build_script = await_file(&root.join("build-script.pid"))
        .trim()
        .parse::<i32>()
        .unwrap();
    assert!(
        pool.try_acquire().unwrap().is_none(),
        "negative control: all three build tokens are charged"
    );
    child.kill().unwrap();
    assert!(!wait_with_peak(child).unwrap().0.success());
    assert_gone(compiler);
    assert!(
        matches!(
            aterm_uds::process::process_presence(build_script),
            aterm_uds::process::ProcessPresence::Exited
                | aterm_uds::process::ProcessPresence::Absent
        ),
        "Cargo's untethered build script must drain before returning tokens"
    );
    drop(implicit);
    shares.finish(0);
    shares.finish(0);
    let free = std::iter::from_fn(|| shares.pool(true).try_acquire().unwrap()).collect::<Vec<_>>();
    assert_eq!(free.len(), 4, "all tokens returned, none double-counted");
}

fn driver_command(root: &Path) -> Command {
    let exe = std::env::current_exe().unwrap();
    let env = ReleaseEnv {
        path: "/usr/bin:/bin".into(),
        home: root.as_os_str(),
        tmp: root,
        cargo_home: root.join("cargo"),
        build_number: 1,
        short_commit: "0123456789ab",
        commit: "0123456789abcdef0123456789abcdef01234567",
        target_dir: root.join("target"),
        rustup_home: None,
        index_hint: None,
        units: None,
    };
    aterm_uds::tether::through(tether_exe());
    let mut cmd = release_build_command(
        &exe,
        [
            "--exact",
            "buildplan::crash_tests::slice_crash_actor",
            "--nocapture",
        ]
        .map(OsString::from)
        .into(),
        &env,
    );
    cmd.env(ROLE, "cargo")
        .env(ROOT, root)
        .env("ATERM_CRASH_TETHER", tether_exe())
        .stdout(Stdio::null());
    cmd
}

fn assert_gone(compiler: i32) {
    let gone = aterm_spec::wait::within(HANG, Duration::from_millis(10), || {
        matches!(
            aterm_uds::process::process_presence(compiler),
            aterm_uds::process::ProcessPresence::Exited
                | aterm_uds::process::ProcessPresence::Absent
        )
        .then_some(())
    });
    if gone.is_none() {
        crate::interrupt::send(compiler, crate::interrupt::SIGKILL);
    }
    assert!(gone.is_some(), "compiler outlived killed driver");
}

/// A cutter killed while the link stand-in is running cannot execute any destructor.
/// Parent-death watchers close both descendant levels; the next run uses the same ledger.
#[test]
fn sigkill_cutter_mid_link_leaves_no_holder_for_the_next_cut() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path();
    let mut cutter = actor("cutter", root);
    let cargo = await_file(&root.join("cutter.ready"))
        .parse::<i32>()
        .unwrap();
    let compiler = await_file(&root.join("compiler.pid"))
        .trim()
        .parse::<i32>()
        .unwrap();
    let build_script = await_file(&root.join("build-script.pid"))
        .trim()
        .parse::<i32>()
        .unwrap();
    let ledger = Ledger::at(root.join("ledger"));
    assert_eq!(ledger.list().unwrap().len(), 1);
    cutter.kill().unwrap();
    assert!(!cutter.wait().unwrap().success());
    assert_gone(cargo);
    assert_gone(compiler);
    assert_gone(build_script);
    assert!(ledger.list().unwrap().is_empty());
    let machine = Fixed::new(Reading::of_gib(128, 96, 8));
    let next = aterm_capacity::admit::try_admit_with(
        &ledger,
        &machine,
        Need::new("next release slice").mem_gib(4).margin(1),
    )
    .unwrap()
    .unwrap();
    assert_eq!(ledger.list().unwrap().len(), 1);
    drop(next);
    let next_pool = aterm_capacity::Jobserver::new(&root.join("next-jobs"), 3).unwrap();
    let tokens = std::iter::from_fn(|| next_pool.try_acquire().unwrap()).collect::<Vec<_>>();
    assert_eq!(tokens.len(), 3, "a fresh cut owns exactly its own new pool");
}
