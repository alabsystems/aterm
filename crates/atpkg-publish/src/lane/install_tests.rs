// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::os::unix::process::ExitStatusExt;

use super::*;

fn reply(success: bool, stdout: &[u8]) -> std::process::Output {
    std::process::Output {
        status: std::process::ExitStatus::from_raw(if success { 0 } else { 1 << 8 }),
        stdout: stdout.to_vec(),
        stderr: Vec::new(),
    }
}

struct Observation {
    stop: Option<std::process::Output>,
    uid: Option<std::process::Output>,
    manager: Option<std::process::Output>,
    jobs: Option<std::process::Output>,
}

impl Default for Observation {
    fn default() -> Self {
        Self {
            stop: Some(reply(false, b"")),
            uid: Some(reply(true, b"501\n")),
            manager: Some(reply(true, b"Aqua\n")),
            jobs: Some(reply(true, b"PID\tStatus\tLabel\n-\t0\tother.job\n")),
        }
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn legacy_retirement_needs_a_confirmed_stop_or_gui_absence_and_no_reloadable_plist() {
    let present = format!("PID\tStatus\tLabel\n-\t0\t{}\n", units::RETIRED_LABEL);
    let duplicate = b"PID\tStatus\tLabel\n-\t0\tother\n42\t0\tother\n";
    let cases = [
        (
            Observation {
                stop: Some(reply(true, b"")),
                ..Observation::default()
            },
            true,
        ),
        (Observation::default(), true),
        (
            Observation {
                jobs: Some(reply(true, b"PID\tStatus\tLabel\n")),
                ..Observation::default()
            },
            true,
        ),
        (
            Observation {
                stop: None,
                ..Observation::default()
            },
            true,
        ),
        (
            Observation {
                jobs: Some(reply(true, present.as_bytes())),
                ..Observation::default()
            },
            false,
        ),
        (
            Observation {
                jobs: Some(reply(true, b"PID\tStatus\tLabel\n-\t0\tother")),
                ..Observation::default()
            },
            false,
        ),
        (
            Observation {
                jobs: Some(reply(true, duplicate)),
                ..Observation::default()
            },
            false,
        ),
        (
            Observation {
                jobs: Some(reply(true, b"not a job table\n")),
                ..Observation::default()
            },
            false,
        ),
        (
            Observation {
                jobs: Some(reply(true, b"PID\tStatus\tLabel\n?\t0\tother\n")),
                ..Observation::default()
            },
            false,
        ),
        (
            Observation {
                jobs: Some(reply(true, b"PID\tStatus\tLabel\n-\t?\tother\n")),
                ..Observation::default()
            },
            false,
        ),
        (
            Observation {
                jobs: Some(reply(true, b"PID\tStatus\tLabel\n-\t+0\tother\n")),
                ..Observation::default()
            },
            false,
        ),
        (
            Observation {
                jobs: Some(reply(true, b"PID\tStatus\tLabel\n-\t0\t\xff\n")),
                ..Observation::default()
            },
            false,
        ),
        (
            Observation {
                jobs: Some(reply(false, b"PID\tStatus\tLabel\n")),
                ..Observation::default()
            },
            false,
        ),
        (
            Observation {
                jobs: None,
                ..Observation::default()
            },
            false,
        ),
        (
            Observation {
                uid: Some(reply(true, b"502\n")),
                ..Observation::default()
            },
            false,
        ),
        (
            Observation {
                uid: None,
                ..Observation::default()
            },
            false,
        ),
        (
            Observation {
                manager: Some(reply(true, b"Background\n")),
                ..Observation::default()
            },
            false,
        ),
    ];
    for (observation, confirmed) in cases {
        let scratch = aterm_tempfile::tempdir().expect("private backend fixture");
        let manager = Launchd {
            agents_dir: scratch.path().join("agents"),
            state_dir: scratch.path().join("state"),
        };
        std::fs::create_dir_all(&manager.agents_dir).expect("agents");
        std::fs::create_dir_all(&manager.state_dir).expect("state");
        let plist = manager
            .agents_dir
            .join(format!("{}.plist", units::RETIRED_LABEL));
        std::fs::write(&plist, b"legacy definition").expect("legacy plist");
        let mut calls = Vec::new();
        let result = manager
            .arm_with("501", &[scratch.path().join("current.plist")], |args| {
                calls.push(args.join(" "));
                let answer = match args {
                    ["bootout", target] if target.ends_with(units::RETIRED_LABEL) => {
                        &observation.stop
                    }
                    ["manageruid"] => &observation.uid,
                    ["managername"] => &observation.manager,
                    ["list"] => &observation.jobs,
                    ["bootout" | "bootstrap" | "enable", ..] => return Ok(reply(true, b"")),
                    _ => panic!("unexpected launchctl arguments: {args:?}"),
                };
                answer
                    .clone()
                    .ok_or_else(|| Error::cannot_run("fixture command unavailable"))
            })
            .expect("current bootstrap succeeds");
        assert_eq!(result == Retirement::Confirmed, confirmed, "{calls:?}");
        assert!(calls.iter().any(|s| s.starts_with("bootstrap gui/501 ")));
        if confirmed {
            assert!(!plist.exists());
        } else {
            assert_eq!(
                std::fs::read(&plist).expect("unknown keeps legacy definition"),
                b"legacy definition"
            );
        }
    }

    let scratch = aterm_tempfile::tempdir().expect("private removal fixture");
    let manager = Launchd {
        agents_dir: scratch.path().to_path_buf(),
        state_dir: scratch.path().to_path_buf(),
    };
    let plist = scratch
        .path()
        .join(format!("{}.plist", units::RETIRED_LABEL));
    let mut stopped = |_: &[&str]| Ok(reply(true, b""));
    assert_eq!(
        manager.retire_vendor("501", &mut stopped),
        Retirement::Confirmed
    );
    std::fs::create_dir(&plist).expect("obstruct plist removal");
    std::fs::write(plist.join("held"), b"legacy contents").expect("held contents");
    let result = manager
        .arm_with("501", &[scratch.path().join("current.plist")], stopped)
        .expect("current bootstrap succeeds despite removal uncertainty");
    assert!(matches!(result, Retirement::Unknown(_)));
    assert_eq!(
        std::fs::read(plist.join("held")).expect("contents retained"),
        b"legacy contents"
    );
    let mut queried = false;
    assert!(!bootout_or_absent("0", units::RETIRED_LABEL, &mut |args| {
        queried |= args != ["bootout", &format!("gui/0/{}", units::RETIRED_LABEL)];
        Ok(reply(false, b""))
    }));
    assert!(!queried, "root's legacy list would name the system domain");
}

#[test]
fn arming_one_lanes_jobs_retires_every_other_lanes_job_and_definition() {
    let scratch = aterm_tempfile::tempdir().expect("private backend fixture");
    let manager = Launchd {
        agents_dir: scratch.path().join("agents"),
        state_dir: scratch.path().join("state"),
    };
    std::fs::create_dir_all(&manager.agents_dir).expect("agents");
    std::fs::create_dir_all(&manager.state_dir).expect("state");
    let plist = |label: &str| manager.agents_dir.join(format!("{label}.plist"));
    for label in units::APPLE_LABELS {
        std::fs::write(plist(label), b"a definition").expect("a plist");
    }
    // The tip lane's jobs are armed: the source-release lane's job would start a snapshot this
    // install may prune, so it is stopped and its definition removed. A job being armed is
    // stopped only to be started again.
    let armed: &[&str] = &[units::TIP_LABEL, units::TIP_READY_LABEL];
    let arming: Vec<PathBuf> = armed.iter().map(|label| plist(label)).collect();
    let mut calls = Vec::new();
    let result = manager
        .arm_with("501", &arming, |args| {
            calls.push(args.join(" "));
            Ok(reply(true, b""))
        })
        .expect("armed");
    assert_eq!(result, Retirement::Confirmed);
    assert!(
        !plist(units::LAUNCHD_LABEL).exists(),
        "the other lane's definition is gone"
    );
    for label in armed {
        assert!(plist(label).exists(), "{label} keeps its definition");
        let domain = format!("gui/501/{label}");
        let at = |verb: &str| {
            calls
                .iter()
                .position(|c| *c == format!("{verb} {domain}"))
                .unwrap_or_else(|| panic!("no `{verb} {domain}` in {calls:?}"))
        };
        let started = calls
            .iter()
            .position(|c| *c == format!("bootstrap gui/501 {}", plist(label).display()))
            .expect("bootstrapped");
        assert!(
            at("bootout") < started && started < at("enable"),
            "{calls:?}"
        );
    }
    assert!(
        calls.contains(&format!("bootout gui/501/{}", units::LAUNCHD_LABEL)),
        "{calls:?}"
    );
    assert!(manager.state_dir.join("tip.launchd.log").is_file());
    assert!(manager.state_dir.join("tip-ready.launchd.log").is_file());

    // A sibling job that cannot be shown stopped keeps its definition, and the install is told
    // its retirement is unknown, so it prunes no snapshot that job could still start.
    std::fs::write(plist(units::LAUNCHD_LABEL), b"a definition").expect("a plist");
    let alab = format!("gui/501/{}", units::LAUNCHD_LABEL);
    let result = manager
        .arm_with("501", &arming, |args| match args {
            ["bootout", target] if *target == alab => Ok(reply(false, b"")),
            ["manageruid"] => Ok(reply(true, b"502\n")),
            _ => Ok(reply(true, b"")),
        })
        .expect("armed");
    assert!(matches!(result, Retirement::Unknown(why) if why.contains(units::LAUNCHD_LABEL)));
    assert!(plist(units::LAUNCHD_LABEL).exists());
}
