// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const CLEANUP_FIXTURE: &str = "ATERM_VERIFY_ARENA_CLEANUP_FIXTURE";
const CLEANUP_ARMED: &str = "ATERM_VERIFY_ARENA_CLEANUP_ARMED";
const FIXTURE_TEST: &str = "perf::arena::live::tests::native_interrupt_cleanup_fixture";

/// Run only as this test's owned subprocess. Re-exec after blocking signals so every
/// libtest thread inherits the mask and the production guard's sigwait owns delivery.
#[test]
fn native_interrupt_cleanup_fixture() {
    let Some(root) = std::env::var_os(CLEANUP_FIXTURE).map(PathBuf::from) else {
        return;
    };
    if std::env::var_os(CLEANUP_ARMED).is_none() {
        let set = interrupts();
        // SAFETY: changes this owned fixture thread's mask using a live initialized set;
        // transparent exec removes the other threads and preserves this mask. The parent
        // sends no signal before the second image has reported its registered child.
        assert_eq!(
            unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()) },
            0
        );
        let error = aterm_uds::spawnfd::transparent(std::env::current_exe().unwrap())
            .args(["--exact", FIXTURE_TEST, "--test-threads=1", "--nocapture"])
            .env(CLEANUP_ARMED, "1")
            .exec();
        panic!("fixture re-exec failed: {error}");
    }
    guard();
    let run_root = make_dir(&root, "run-").unwrap();
    let home = run_root.join("sample-1/home");
    std::fs::create_dir_all(&home).unwrap();
    let socket_dir = make_dir(&root, "s-").unwrap();
    let _socket = std::os::unix::net::UnixListener::bind(socket_dir.join("sock")).unwrap();
    let cat = aterm_uds::deadline::stand_in(Path::new("/bin/cat"));
    let group = Group::spawn(
        command(&cat)
            .current_dir(&home)
            .env("HOME", &home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )
    .unwrap();
    let _input = {
        let mut live = live();
        let child = live.groups.get_mut(&group.pgid).unwrap().as_mut().unwrap();
        aterm_uds::deadline::cat_echoes(child)
    };
    let identity = aterm_uds::process::read_identity(group.pgid as i32).unwrap();
    let ready = aterm_json::json!({"child":identity.token(),"run_root":run_root.to_str().unwrap(),"socket_dir":socket_dir.to_str().unwrap()});
    std::fs::write(root.join("ready"), ready.to_string()).unwrap();
    loop {
        std::thread::park();
    }
}

struct FixtureChild {
    child: Child,
    terminal: Option<aterm_uds::process::Identity>,
}
impl Drop for FixtureChild {
    fn drop(&mut self) {
        if let Some(identity) = self.terminal
            && aterm_uds::process::read_identity(identity.pid) == Some(identity)
        {
            killpg(identity.pid as u32, libc::SIGKILL);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The real interrupt guard retires its registered group, sandbox and socket directory
/// on all three signals. No benchmark, live CPU wait, CLI override or window is involved.
#[test]
fn an_interrupted_arena_run_retires_its_terminal_and_its_scratch() {
    use aterm_spec::wait::{MUST_HAPPEN, until_within};
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        let root = aterm_tempfile::Builder::new()
            .prefix("atvl-")
            .suffix(".noindex")
            .tempdir_in("/tmp")
            .unwrap();
        let log = root.path().join("fixture.log");
        let mut fixture = FixtureChild {
            child: command(std::env::current_exe().unwrap())
                .args(["--exact", FIXTURE_TEST, "--test-threads=1", "--nocapture"])
                .env_remove(CLEANUP_ARMED)
                .env(CLEANUP_FIXTURE, root.path())
                .stdin(Stdio::null())
                .stdout(std::fs::File::create(&log).unwrap())
                .stderr(std::fs::File::options().append(true).open(&log).unwrap())
                .spawn()
                .unwrap(),
            terminal: None,
        };
        let ready = until_within(
            MUST_HAPPEN,
            "owned arena fixture stdout proof and registration",
            || {
                if let Ok(text) = std::fs::read_to_string(root.path().join("ready")) {
                    return aterm_json::from_str::<aterm_json::Value>(&text).ok();
                }
                assert!(
                    fixture.child.try_wait().unwrap().is_none(),
                    "fixture exited before readiness: {}",
                    std::fs::read_to_string(&log).unwrap()
                );
                None
            },
        );
        let identity =
            aterm_uds::process::Identity::parse(ready["child"].as_str().unwrap()).unwrap();
        let lineage = aterm_uds::process::read_lineage(identity.pid).unwrap();
        assert_eq!(lineage.identity, identity);
        assert_eq!(lineage.pgid, identity.pid as u32);
        assert_eq!(
            lineage.parent,
            fixture.child.id(),
            "reported group belongs to the owned fixture"
        );
        fixture.terminal = Some(identity);
        let run_root = PathBuf::from(ready["run_root"].as_str().unwrap());
        let socket_dir = PathBuf::from(ready["socket_dir"].as_str().unwrap());
        assert!(run_root.starts_with(root.path()) && socket_dir.starts_with(root.path()));
        assert!(run_root.join("sample-1/home").is_dir() && socket_dir.join("sock").exists());
        assert_eq!(
            aterm_uds::process::read_identity(identity.pid),
            Some(identity)
        );
        // SAFETY: the unreaped Child owns this pid, which cannot be reused before wait;
        // only that fixture is signalled, never the test runner or another session.
        assert_eq!(unsafe { libc::kill(fixture.child.id() as i32, signal) }, 0);
        let status = until_within(MUST_HAPPEN, "actual arena interrupt cleanup", || {
            fixture.child.try_wait().unwrap()
        });
        assert_eq!(
            status.code(),
            Some(128 + signal),
            "{}",
            std::fs::read_to_string(&log).unwrap()
        );
        assert!(!aterm_uds::process::pid_alive(identity.pid as u32));
        assert!(
            !run_root.exists() && !socket_dir.exists(),
            "registered private directories retired"
        );
    }
}

/// A retired group is gone: the leader's exit is what the retire waits on,
/// so the test needs no sleep and no poll.
#[test]
fn retiring_a_group_ends_its_leader() {
    let mut g = Group::spawn(command("/bin/sleep").arg("600")).expect("spawn");
    assert!(g.exited().is_none());
    let pid = g.pgid;
    g.retire().expect("retired");
    assert!(
        !aterm_uds::process::pid_alive(pid),
        "{pid} outlived its retire"
    );
    assert!(!live().groups.contains_key(&pid), "the table forgets it");
}

/// A scratch directory a started copy has not shown it ran from is kept;
/// once it ran, the same directory goes. NEGATIVE CONTROL: the second half.
#[test]
fn a_directory_a_started_copy_never_ran_from_is_kept() {
    let unproven = make_dir(&std::env::temp_dir(), "atv-unproven-").expect("scratch");
    std::fs::write(unproven.join("aterm"), b"copied").expect("the copy");
    starting_from(&unproven);
    remove_dir(&unproven);
    let kept = unproven.join("aterm").is_file();
    let forgotten = !live().dirs.contains(&unproven);
    let unmarked = !live().unproven.contains(&unproven);
    let _ = std::fs::remove_dir_all(&unproven);
    assert!(kept, "kept while unproven");
    assert!(forgotten && unmarked, "and forgotten, both marks");

    let proven = make_dir(&std::env::temp_dir(), "atv-proven-").expect("scratch");
    std::fs::write(proven.join("aterm"), b"copied").expect("the copy");
    starting_from(&proven);
    ran_from(&proven);
    remove_dir(&proven);
    assert!(!proven.exists(), "it ran: removed");
}
