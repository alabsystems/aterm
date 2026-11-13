// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{LaunchError, TextFault, validate_request};
use std::ffi::OsString;
use std::path::PathBuf;

fn scratch_bundle(name: &str) -> (aterm_tempfile::TempDir, PathBuf) {
    let root = aterm_tempfile::Builder::new()
        .prefix(&format!("aterm-launch-successor-{name}-"))
        .tempdir()
        .expect("create the scratch root");
    let bundle = root.path().join("Successor.app");
    std::fs::create_dir_all(&bundle).expect("create the scratch bundle directory");
    (root, bundle)
}

fn env(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
    pairs
        .iter()
        .map(|(key, value)| (OsString::from(*key), OsString::from(*value)))
        .collect()
}

#[test]
fn a_malformed_bundle_path_is_refused() {
    for (path, want) in [
        (
            "Successor.app",
            LaunchError::BundlePathNotAbsolute("Successor.app".to_string()),
        ),
        (
            "/nowhere/successor.tar.gz",
            LaunchError::BundlePathNotAnAppBundle("/nowhere/successor.tar.gz".to_string()),
        ),
        (
            "/nowhere/aterm-does-not-exist.app",
            LaunchError::BundleMissing("/nowhere/aterm-does-not-exist.app".to_string()),
        ),
    ] {
        assert_eq!(
            validate_request(&PathBuf::from(path), &[], &[]),
            Err(want),
            "{path}"
        );
    }
}

#[test]
fn a_well_formed_request_carries_exactly_the_keys_it_was_given() {
    let (_root, bundle) = scratch_bundle("well-formed");
    let request = validate_request(
        &bundle,
        &[OsString::from("--headless"), OsString::from("-e")],
        &env(&[
            ("ATERM_SEAMLESS_NONCE", "0123456789abcdef"),
            ("ATERM_UPDATED_FROM", "1785910394"),
        ]),
    )
    .expect("a real .app directory with sane argv and env validates");

    assert_eq!(request.bundle, bundle.to_str().unwrap());
    assert_eq!(request.arguments, vec!["--headless", "-e"]);
    assert_eq!(
        request.environment,
        vec![
            (
                "ATERM_SEAMLESS_NONCE".to_string(),
                "0123456789abcdef".to_string()
            ),
            ("ATERM_UPDATED_FROM".to_string(), "1785910394".to_string()),
        ],
        "only the caller's keys ride the launch; the rest MERGES from this process"
    );
}

#[test]
fn a_malformed_environment_is_refused() {
    let (_root, bundle) = scratch_bundle("bad-env");
    for (pairs, expected) in [
        (
            env(&[("", "value")]),
            LaunchError::EnvKey {
                index: 0,
                fault: TextFault::Empty,
            },
        ),
        (
            env(&[("ATERM_A", "a"), ("ATERM_B=C", "b")]),
            LaunchError::EnvKey {
                index: 1,
                fault: TextFault::Separator,
            },
        ),
        (
            env(&[("ATERM\0B", "b")]),
            LaunchError::EnvKey {
                index: 0,
                fault: TextFault::InteriorNul,
            },
        ),
        (
            env(&[("ATERM_SEAMLESS_NONCE", "ab\0cd")]),
            LaunchError::EnvValue {
                key: "ATERM_SEAMLESS_NONCE".to_string(),
                fault: TextFault::InteriorNul,
            },
        ),
        (
            env(&[
                ("ATERM_SEAMLESS_NONCE", "first"),
                ("ATERM_OTHER", "x"),
                ("ATERM_SEAMLESS_NONCE", "second"),
            ]),
            LaunchError::EnvKeyDuplicated("ATERM_SEAMLESS_NONCE".to_string()),
        ),
    ] {
        assert_eq!(
            validate_request(&bundle, &[], &pairs),
            Err(expected),
            "{pairs:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn text_that_is_not_utf8_is_refused_wherever_it_appears() {
    use std::os::unix::ffi::OsStringExt;

    let (_root, bundle) = scratch_bundle("not-utf8");
    let invalid = || OsString::from_vec(vec![0x61, 0xff, 0x62]);

    assert_eq!(
        validate_request(&bundle, &[OsString::from("--ok"), invalid()], &[]),
        Err(LaunchError::Argument {
            index: 1,
            fault: TextFault::NotUtf8,
        }),
    );
    assert_eq!(
        validate_request(&bundle, &[], &[(invalid(), OsString::from("v"))]),
        Err(LaunchError::EnvKey {
            index: 0,
            fault: TextFault::NotUtf8,
        }),
    );
    assert_eq!(
        validate_request(&bundle, &[], &[(OsString::from("ATERM_K"), invalid())]),
        Err(LaunchError::EnvValue {
            key: "ATERM_K".to_string(),
            fault: TextFault::NotUtf8,
        }),
    );
    assert_eq!(
        validate_request(
            &PathBuf::from(OsString::from_vec(b"/\xff.app".to_vec())),
            &[],
            &[]
        ),
        Err(LaunchError::BundlePath {
            path: "/\u{fffd}.app".to_string(),
            fault: TextFault::NotUtf8,
        }),
    );
}

/// Off macOS the request is still validated — same refusals, same tests —
/// and only then is the platform refused.
#[cfg(not(target_os = "macos"))]
#[test]
fn off_macos_the_launcher_refuses_the_platform_and_not_the_request() {
    use super::launch_app_bundle;
    use std::time::Duration;

    let (_root, bundle) = scratch_bundle("unsupported");
    assert_eq!(
        launch_app_bundle(&bundle, &[], &[], Duration::from_millis(10)),
        Err(LaunchError::Unsupported),
        "a platform with no LaunchServices says so, and launches nothing"
    );
    assert_eq!(
        launch_app_bundle(
            &PathBuf::from("relative.app"),
            &[],
            &[],
            Duration::from_millis(10)
        ),
        Err(LaunchError::BundlePathNotAbsolute(
            "relative.app".to_string()
        )),
        "a malformed request is caught on every platform, not only where it can launch"
    );
}

#[cfg(target_os = "macos")]
mod macos {
    use super::super::{
        CONDVAR_PARKS, LaunchError, LaunchWatch, LaunchedSuccessor, admit_launch_answer,
        admit_launched_pid, launch_answer_proves_exit, remaining_budget,
    };
    use std::cell::Cell;
    use std::time::{Duration, Instant};

    #[test]
    fn a_spent_budget_never_hands_back_a_zero_length_wait() {
        let budget = Duration::from_millis(100);
        assert_eq!(
            remaining_budget(budget, Duration::from_millis(40)),
            Some(Duration::from_millis(60)),
            "an unspent budget hands back what is left"
        );
        assert_eq!(
            remaining_budget(budget, budget),
            None,
            "an exactly-spent budget is spent, not a zero-length wait to spin on"
        );
        assert_eq!(
            remaining_budget(budget, Duration::from_secs(9)),
            None,
            "an overspent budget saturates instead of panicking in the worker"
        );
        assert_eq!(
            remaining_budget(Duration::ZERO, Duration::ZERO),
            None,
            "a zero budget is already spent"
        );
    }

    #[test]
    fn activation_refuses_an_impossible_pid_without_touching_appkit() {
        assert!(!crate::app_launch_successor::activate_running(0));
        assert!(!crate::app_launch_successor::activate_running(-1));
        assert!(!crate::app_launch_successor::activate_running(1));
    }

    #[test]
    fn a_zero_budget_wait_is_a_poll_that_never_sleeps() {
        let parks = CONDVAR_PARKS.with(Cell::get);
        let started = Instant::now();
        assert_eq!(
            crate::app_launch_successor::LaunchInFlight::scripted(None).wait(Duration::ZERO),
            Err(LaunchError::Timeout(Duration::ZERO))
        );
        assert_eq!(
            CONDVAR_PARKS.with(Cell::get),
            parks,
            "a zero budget parked on the condvar instead of polling"
        );
        assert!(
            started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
            "a zero budget waited on (a hang detector): {:?}",
            started.elapsed()
        );
        assert_eq!(
            crate::app_launch_successor::LaunchInFlight::scripted(Some(7))
                .wait(Duration::ZERO)
                .map(crate::app_launch_successor::LaunchedSuccessor::pid),
            Ok(7)
        );
        assert_eq!(
            CONDVAR_PARKS.with(Cell::get),
            parks,
            "a delivered answer is read without parking"
        );
        let live = [1, 10, 100, 1_000].into_iter().any(|ms| {
            let budget = Duration::from_millis(ms);
            assert_eq!(
                crate::app_launch_successor::LaunchInFlight::scripted(None).wait(budget),
                Err(LaunchError::Timeout(budget))
            );
            CONDVAR_PARKS.with(Cell::get) > parks
        });
        assert!(live, "an unanswered budget never parked: the count is dead");
    }

    #[test]
    fn an_answer_from_another_thread_ends_the_wait() {
        let watch = LaunchWatch::pending();
        let slot = std::sync::Arc::clone(&watch.slot);
        let completion = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
            slot.deliver(admit_launched_pid(4321, 4322));
        });
        let answer = watch.wait(aterm_spec::wait::MUST_HAPPEN);
        completion.join().expect("the completion thread finishes");
        assert_eq!(answer.map(LaunchedSuccessor::pid), Ok(4321));
    }

    #[test]
    fn only_the_first_answer_is_kept() {
        let watch = LaunchWatch::pending();
        watch.slot.deliver(admit_launched_pid(11, 12));
        watch.slot.deliver(Err(LaunchError::NoApplication));
        assert_eq!(
            watch
                .wait(Duration::from_millis(10))
                .map(LaunchedSuccessor::pid),
            Ok(11),
            "a second completion must not overwrite the answer already acted on"
        );
    }

    #[test]
    fn an_unusable_pid_is_refused_instead_of_being_waited_on() {
        assert_eq!(
            admit_launched_pid(-1, 900),
            Err(LaunchError::PidUnknown(-1)),
            "NSRunningApplication answers -1 when it does not know a pid"
        );
        assert_eq!(
            admit_launched_pid(0, 900),
            Err(LaunchError::PidUnknown(0)),
            "0 is not a pid this launch could have produced"
        );
        assert_eq!(
            admit_launched_pid(900, 900),
            Err(LaunchError::SubstitutedRunningInstance(900)),
            "our own pid means LaunchServices substituted the running instance"
        );
        assert_eq!(
            admit_launched_pid(901, 900).map(LaunchedSuccessor::pid),
            Ok(901),
        );
    }

    #[test]
    fn only_a_terminated_application_with_no_pid_reads_as_death() {
        assert_eq!(
            admit_launch_answer(-1, true, 900),
            Err(LaunchError::ExitedBeforeCheckIn)
        );
        assert_eq!(
            admit_launch_answer(-1, false, 900),
            Err(LaunchError::PidUnknown(-1)),
            "an unknown pid on a live application is not death"
        );
        assert_eq!(
            admit_launch_answer(901, true, 900).map(LaunchedSuccessor::pid),
            Ok(901),
            "a known pid is admitted as before; the corroboration compares it"
        );
        assert_eq!(
            admit_launch_answer(900, false, 900),
            Err(LaunchError::SubstitutedRunningInstance(900))
        );
        assert!(launch_answer_proves_exit(Some(&Err(
            LaunchError::ExitedBeforeCheckIn
        ))));
        for alive in [
            None,
            Some(admit_launch_answer(901, false, 900)),
            Some(admit_launch_answer(901, true, 900)),
            Some(Err(LaunchError::PidUnknown(-1))),
            Some(Err(LaunchError::NoApplication)),
            Some(Err(LaunchError::Timeout(Duration::ZERO))),
            Some(Err(LaunchError::Rejected {
                domain: "NSCocoaErrorDomain".to_string(),
                code: 256,
                message: "a miscellaneous error occurred".to_string(),
            })),
        ] {
            assert!(!launch_answer_proves_exit(alive.as_ref()), "{alive:?}");
        }
    }

    #[test]
    fn the_exit_peek_leaves_the_answer_for_the_corroboration() {
        let in_flight = crate::app_launch_successor::LaunchInFlight::scripted(None);
        assert!(!in_flight.exited_before_check_in(), "no answer is no death");
        in_flight.answer_for_test(Err(LaunchError::ExitedBeforeCheckIn));
        for _ in 0..3 {
            assert!(in_flight.exited_before_check_in());
        }
        assert_eq!(
            in_flight.wait(Duration::ZERO),
            Err(LaunchError::ExitedBeforeCheckIn),
            "the peeks took nothing"
        );
        let live = crate::app_launch_successor::LaunchInFlight::scripted(Some(4321));
        assert!(!live.exited_before_check_in());
        assert_eq!(
            live.wait(Duration::ZERO).map(LaunchedSuccessor::pid),
            Ok(4321)
        );
    }
}
