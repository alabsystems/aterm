// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};

#[test]
fn a_forked_successor_adopts_the_carried_launcher_limit() {
    assert_eq!(carried_launcher_limit(Some("256"), 4096), Some(256));
    assert_eq!(carried_launcher_limit(Some("4096"), 4096), Some(4096));
    assert_eq!(
        carried_launcher_limit(Some("8192"), 4096),
        None,
        "never raised by it"
    );
    assert_eq!(carried_launcher_limit(Some("0"), 4096), None);
    assert_eq!(carried_launcher_limit(Some("00"), 4096), None);
    assert_eq!(carried_launcher_limit(Some("0256"), 4096), None);
    assert_eq!(carried_launcher_limit(Some("+256"), 4096), None);
    assert_eq!(carried_launcher_limit(Some("-1"), 4096), None);
    assert_eq!(carried_launcher_limit(Some("lots"), 4096), None);
    assert_eq!(carried_launcher_limit(None, 4096), None, "an older parent");
}

#[test]
fn a_cold_lane_exec_carries_the_launchers_limit_to_the_new_images_shells() {
    const NEW_IMAGE: &str = "ATERM_TEST_COLD_EXEC_NEW_IMAGE";
    if std::env::var_os(NEW_IMAGE).is_some() {
        let env = crate::handoff_env::HandoffEnv::capture();
        let carried =
            adopt_carried_launcher_limit(&env).expect("the new image adopts the carried limit");
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: A valid resource id and a writable out-parameter.
        assert_eq!(
            unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) },
            0
        );
        assert!(
            carried < limit.rlim_cur,
            "the raise crossed the exec: {carried} < {}",
            limit.rlim_cur
        );
        a_shell_spawned_after_the_raise_gets_the_launchers_limit(carried, limit.rlim_cur);
        return;
    }

    if !crate::control_auth::enter_low_nofile_test_child(64) {
        return;
    }
    let (from, _) = raise_descriptor_limit_for_claim()
        .expect("the raise is permitted")
        .expect("a 64-spare limit is below the floor");
    let mut image = crate::test_alone::StandIn::this_test();
    image.command().env(NEW_IMAGE, "1");
    image
        .command()
        .envs(launcher_limit_environment(TAKES_PARK_RAISED_FROM_BUILD));
    assert!(
        image
            .command()
            .get_envs()
            .any(|(key, value)| key == ENV_LAUNCHER_NOFILE.name()
                && value == Some(std::ffi::OsStr::new(&from.to_string()))),
        "the command names the launcher's {from}"
    );
    image.spawn().passed();
}

fn soft_nofile() -> u64 {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: A valid resource id and a writable out-parameter.
    assert_eq!(
        unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) },
        0
    );
    limit.rlim_cur
}

fn mark_close_on_exec_at_or_above(floor: u64) {
    let floor = i32::try_from(floor).expect("a descriptor number");
    for fd in floor..aterm_uds::spawnfd::descriptor_bound() {
        // SAFETY: F_GETFD/F_SETFD on any number only reads or sets its close-on-exec flag; a
        // number that is not open answers -1.
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags >= 0 {
                libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
            }
        }
    }
}

fn successor_soft_nofile(target_build: u64, build: u64, inherited: &[std::os::fd::RawFd]) -> u64 {
    let mut successor = std::process::Command::new("/bin/sh");
    successor.args(["-c", "ulimit -n"]);
    start_older_image_at_its_limit(&mut successor, target_build, build, inherited);
    let out = successor.output().expect("run the successor's stand-in");
    assert!(out.status.success(), "{out:?}");
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .expect("a limit")
}

#[test]
fn an_older_successor_starts_at_the_limit_held_before_a_parks_raise() {
    const NEW_IMAGE: &str = "ATERM_TEST_PARK_RAISE_NEW_IMAGE";
    const BUILD: u64 = 7;

    assert_eq!(older_image_soft_limit(6, 7, Some(256), 4096), Some(256));
    assert_eq!(
        older_image_soft_limit(7, 7, Some(256), 4096),
        None,
        "the same build reads the told limit"
    );
    assert_eq!(
        older_image_soft_limit(8, 7, Some(256), 4096),
        None,
        "a newer one does too"
    );
    assert_eq!(
        older_image_soft_limit(6, 7, None, 4096),
        None,
        "no park's raise: inherited as before round eight"
    );
    assert_eq!(
        older_image_soft_limit(6, 7, Some(4096), 4096),
        None,
        "nothing to lower"
    );

    if std::env::var_os(NEW_IMAGE).is_some() {
        let env = crate::handoff_env::HandoffEnv::capture();
        let launcher = adopt_carried_launcher_limit(&env).expect("the launcher's limit is carried");
        assert_eq!(
            park_raised_from(),
            Some(launcher),
            "the park's raise is adopted"
        );
        assert!(launcher < soft_nofile(), "the raise crossed");
        mark_close_on_exec_at_or_above(launcher);
        assert_eq!(successor_soft_nofile(BUILD - 1, BUILD, &[]), launcher);
        return;
    }

    if !crate::control_auth::enter_low_nofile_test_child(64) {
        return;
    }
    let launchd = soft_nofile();
    mark_close_on_exec_at_or_above(launchd);
    assert_eq!(OlderImageLimit::toward(BUILD - 1, BUILD), None);
    assert_eq!(successor_soft_nofile(BUILD - 1, BUILD, &[]), launchd);
    raise_descriptor_limit_for_park(BUILD - 1, BUILD);
    assert_eq!(soft_nofile(), launchd, "no raise toward an older target");
    raise_descriptor_limit_for_park(BUILD, BUILD);
    let raised = soft_nofile();
    assert!(raised > launchd, "the park's raise is made: {raised}");
    assert_eq!(
        park_raised_from(),
        Some(launchd),
        "…and recorded as a park's"
    );

    assert_eq!(
        successor_soft_nofile(BUILD - 1, BUILD, &[]),
        launchd,
        "the fork lane starts the older successor at launchd's limit, not {raised}"
    );
    assert_eq!(
        successor_soft_nofile(BUILD, BUILD, &[]),
        raised,
        "a same-or-newer successor inherits the room, and reads the told limit"
    );

    let (read, _write) = aterm_uds::pipe().expect("pipe");
    let launchd_fd = i32::try_from(launchd).expect("a descriptor number");
    // SAFETY: F_DUPFD duplicates a live descriptor at the first free number at or above
    // `launchd_fd`, with close-on-exec clear.
    let high = unsafe { libc::fcntl(read.as_raw_fd(), libc::F_DUPFD, launchd_fd) };
    assert!(
        high >= launchd_fd,
        "PRECONDITION: room above launchd's limit"
    );
    // SAFETY: A fresh duplicate, owned here from now.
    let high = unsafe { OwnedFd::from_raw_fd(high) };
    assert_eq!(
        successor_soft_nofile(BUILD - 1, BUILD, &[high.as_raw_fd()]),
        raised,
        "handed above the limit"
    );
    assert_eq!(
        successor_soft_nofile(BUILD - 1, BUILD, &[]),
        raised,
        "a stray the exec would keep"
    );
    assert!(
        lower_for_older_image(BUILD - 1, BUILD).is_none(),
        "the cold lane's exec would keep it too"
    );
    assert_eq!(soft_nofile(), raised);
    drop(high);

    let lowered = lower_for_older_image(BUILD - 1, BUILD).expect("lowered for the exec");
    assert_eq!(
        soft_nofile(),
        launchd,
        "the older image starts at launchd's limit"
    );
    drop(lowered);
    assert_eq!(soft_nofile(), raised, "raised back when the exec returned");
    assert!(
        lower_for_older_image(BUILD, BUILD).is_none(),
        "never toward this build"
    );

    let mut image = crate::test_alone::StandIn::this_test();
    image.command().env(NEW_IMAGE, "1");
    image
        .command()
        .envs(launcher_limit_environment(TAKES_PARK_RAISED_FROM_BUILD));
    assert!(
        image
            .command()
            .get_envs()
            .any(|(key, value)| key == ENV_PARK_RAISED_FROM.name()
                && value == Some(std::ffi::OsStr::new(&launchd.to_string()))),
        "the command names the park's raise from {launchd}"
    );
    image.spawn().passed();

    raise_descriptor_limit_for_recovery();
    assert_eq!(park_raised_from(), None, "the raise is a recovery's now");
    assert_eq!(successor_soft_nofile(BUILD - 1, BUILD, &[]), raised);
}

/// …AND THE RAISE STAYS THE SUCCESSOR'S.
pub(crate) fn a_shell_spawned_after_the_raise_gets_the_launchers_limit(from: u64, to: u64) {
    use std::os::unix::process::CommandExt as _;
    assert_eq!(
        launcher_soft_descriptor_limit(),
        Some(from),
        "the raise was recorded"
    );
    let limits = crate::spawn::daily_driver_limits();
    assert_eq!(limits.open_files, Some(from));
    // SAFETY: Test-only root authority, used for this one child's cap.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    let cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
    let mut shell = std::process::Command::new("/bin/sh");
    shell.args(["-c", "ulimit -n"]);
    // SAFETY: `apply` is a bare `getrlimit`/`setrlimit` loop with a valid cap —
    // async-signal-safe, as the PTY's own post-fork child relies on.
    unsafe {
        shell.pre_exec(move || limits.apply(&cap));
    }
    let out = shell.output().expect("run the shell");
    assert!(out.status.success(), "{out:?}");
    let seen = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert_eq!(
        seen,
        from.to_string(),
        "the shell sees the launcher's limit, not {to}"
    );
}

#[test]
fn every_lane_settles_its_descriptor_limit_in_one_place() {
    fn soft_limit() -> u64 {
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: A valid resource id and a writable out-parameter.
        assert_eq!(
            unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) },
            0
        );
        limit.rlim_cur
    }
    if !crate::control_auth::enter_low_nofile_test_child(64) {
        return;
    }
    let low = soft_limit();
    PARK_RAISED_FROM.store(low, std::sync::atomic::Ordering::Relaxed);
    let mut launched = crate::handoff_env::HandoffEnv::default();
    launched.set(crate::seamless::ENV_RENDEZVOUS, "/nonexistent/rendezvous");
    settle_incoming_descriptor_limit(&launched);
    assert!(
        soft_limit() > low,
        "a launched successor makes room before it dials"
    );
    assert_eq!(launcher_soft_descriptor_limit(), Some(low));
    assert_eq!(
        park_raised_from(),
        None,
        "a launched successor's raise is a claim's"
    );

    let raised = soft_limit();
    let mut forked = crate::handoff_env::HandoffEnv::default();
    forked.set(crate::seamless::ENV_FDS, "3,4");
    settle_incoming_descriptor_limit(&forked);
    assert_eq!(
        soft_limit(),
        raised,
        "room already held is not raised again"
    );
    assert_eq!(park_raised_from(), None, "and the record stays the claim's");
}

#[test]
fn a_cold_launch_makes_room_for_its_tabs_and_its_shells_keep_the_launchers_limit() {
    if !crate::control_auth::enter_low_nofile_test_child(64) {
        return;
    }
    let low = soft_nofile();
    settle_incoming_descriptor_limit(&crate::handoff_env::HandoffEnv::default());
    let raised = soft_nofile();
    assert!(
        raised > low,
        "a cold launch makes room for the tabs it opens"
    );
    assert_eq!(
        launcher_soft_descriptor_limit(),
        Some(low),
        "its shells get the launcher's limit"
    );
    assert_eq!(
        park_raised_from(),
        Some(low),
        "and an older successor starts at it"
    );

    PARK_RAISED_FROM.store(0, std::sync::atomic::Ordering::Relaxed);
    settle_incoming_descriptor_limit(&crate::handoff_env::HandoffEnv::default());
    assert_eq!(
        soft_nofile(),
        raised,
        "room already held is not raised again"
    );
    assert_eq!(park_raised_from(), None, "nor recorded again");
}

#[test]
fn only_an_updates_launch_adopts_a_carried_launcher_limit() {
    const ROLE: &str = "ATERM_TEST_STRAY_LIMIT_ROLE";
    const BUILD: u64 = 7;
    fn launch(strays: bool) -> crate::handoff_env::HandoffEnv {
        let mut env = crate::handoff_env::HandoffEnv::capture();
        let forgot =
            forget_stray_limits(&mut env, std::env::var_os("ATERM_UPDATED_FROM").is_some());
        assert_eq!(forgot, strays, "whether this launch forgot a stray");
        settle_incoming_descriptor_limit(&env);
        env
    }
    match std::env::var(ROLE).as_deref() {
        Ok("older successor") => {
            let mut window = crate::test_alone::StandIn::this_test();
            window
                .command()
                .env(ROLE, "window")
                .env_remove("ATERM_UPDATED_FROM");
            window.spawn().passed();
            launch(false);
            let launcher = launcher_soft_descriptor_limit().expect("the update's word");
            assert!(launcher < soft_nofile(), "below the raise it inherited");
            assert_eq!(park_raised_from(), Some(launcher));
            return;
        }
        Ok("window") => {
            let env = launch(true);
            assert_eq!(
                launcher_soft_descriptor_limit(),
                None,
                "a window started from a shell takes no stray launcher limit"
            );
            assert_eq!(park_raised_from(), None, "nor a stray park's raise");
            let mut reexec = crate::test_alone::StandIn::this_test();
            reexec
                .command()
                .env(ROLE, "boot re-exec")
                .env("ATERM_UPDATED_FROM", BUILD.to_string())
                .envs(env.exec_env());
            reexec.spawn().passed();
            return;
        }
        Ok("boot re-exec") => {
            launch(false);
            assert_eq!(
                launcher_soft_descriptor_limit(),
                None,
                "the boot re-exec carries no stray on"
            );
            assert_eq!(park_raised_from(), None);
            return;
        }
        _ => {}
    }
    let mut plain = crate::handoff_env::HandoffEnv::default();
    assert!(
        !forget_stray_limits(&mut plain, false),
        "a plain launch forgets none"
    );
    for stray in [ENV_LAUNCHER_NOFILE, ENV_PARK_RAISED_FROM] {
        let mut one = crate::handoff_env::HandoffEnv::default();
        one.set(stray, "256");
        assert!(
            forget_stray_limits(&mut one, false),
            "one stray name is a stray"
        );
        assert!(!one.present(stray), "and it goes");
    }
    if !crate::control_auth::enter_low_nofile_test_child(64) {
        return;
    }
    raise_descriptor_limit_for_park(BUILD, BUILD);
    assert!(park_raised_from().is_some(), "PRECONDITION: a park's raise");
    let mut successor = crate::test_alone::StandIn::this_test();
    successor
        .command()
        .env(ROLE, "older successor")
        .env("ATERM_UPDATED_FROM", BUILD.to_string());
    successor
        .command()
        .envs(launcher_limit_environment(TAKES_PARK_RAISED_FROM_BUILD));
    successor.spawn().passed();
}

#[test]
fn only_a_target_that_takes_the_names_is_told_them() {
    const V0_98_0: u64 = 1_790_607_299;
    if !crate::control_auth::enter_low_nofile_test_child(64) {
        return;
    }
    raise_descriptor_limit_for_park(7, 7);
    let launchd = park_raised_from().expect("PRECONDITION: a park's raise");
    assert_eq!(launcher_soft_descriptor_limit(), Some(launchd));
    let told = |target_build: u64| -> Vec<&'static str> {
        let mut command = std::process::Command::new("/usr/bin/true");
        carry_launcher_limit(&mut command, target_build);
        [ENV_LAUNCHER_NOFILE, ENV_PARK_RAISED_FROM]
            .into_iter()
            .filter(|name| {
                command.get_envs().any(|(key, value)| {
                    key == name.name() && value == Some(std::ffi::OsStr::new(&launchd.to_string()))
                })
            })
            .map(HandoffName::name)
            .collect()
    };
    assert_eq!(told(V0_98_0), Vec::<&str>::new(), "v0.98.0 takes neither");
    assert_eq!(
        told(TAKES_LAUNCHER_NOFILE_FROM_BUILD - 1),
        Vec::<&str>::new()
    );
    assert_eq!(
        told(TAKES_LAUNCHER_NOFILE_FROM_BUILD),
        vec![ENV_LAUNCHER_NOFILE.name()],
        "v0.100.0 takes the launcher's limit alone"
    );
    assert_eq!(
        told(TAKES_PARK_RAISED_FROM_BUILD),
        vec![ENV_LAUNCHER_NOFILE.name(), ENV_PARK_RAISED_FROM.name()],
        "v0.101.0 takes both"
    );
}
