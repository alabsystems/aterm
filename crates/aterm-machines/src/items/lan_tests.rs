// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::items::defaults::tests::ctx;
use crate::port::{Fake, Ran};
use std::path::PathBuf;

const SOURCE: &str = "/Applications/aterm.app";
const SOURCE_EXE: &str = "/Applications/aterm.app/Contents/MacOS/aterm";
const PRINT: &str = "/bin/launchctl print system/systems.alab.aterm-lan";

fn this_build() -> Vec<u8> {
    install::test_image(0x0100_000c, &[b"this build"])
}

fn from_app() -> Fake {
    let mut sys = Fake::root();
    sys.born(std::process::id(), 123);
    sys.running_app = Some(PathBuf::from(SOURCE));
    sys.file(SOURCE_EXE, &this_build());
    sys.stat_of(
        SOURCE,
        crate::port::Stat {
            uid: 501,
            gid: 20,
            mode: 0o755,
            dir: true,
            symlink: false,
            len: 0,
        },
    );
    sys.app_check(|_, _| Ok(()));
    for (path, ino) in [(lan::IMAGE_BUNDLE, 1), (lan::PLIST_PATH, 2)] {
        sys.file_id_of(
            Path::new(path),
            FileId {
                dev: 1,
                ino,
                born_ns: None,
            },
        );
    }
    sys
}

/// As root the bundle is cloned into the root-only staging folder, sealed, swapped in, and
/// only then is the plist written and the job (re)started, each launchctl step in launchd's
/// order; the probe then reads it in place and running. No aterm process is started.
/// NEGATIVE CONTROL: a development build outside a bundle is not applicable and its apply
/// touches nothing.
#[test]
fn the_broker_is_cloned_sealed_swapped_then_started() {
    let sys = from_app();
    // The missing files need no launchctl read; the new job has a PID before
    // its socket is ready. Only the final strict hello completes the apply.
    sys.on(PRINT, Ran::exit(113, "Could not find service"))
        .on(
            PRINT,
            Ran::ok("system/systems.alab.aterm-lan = {\n\tpid = 77\n}\n"),
        )
        .on(
            "/bin/launchctl bootstrap system /Library/LaunchDaemons/systems.alab.aterm-lan.plist",
            Ran::ok(""),
        )
        .on(
            "/bin/launchctl kickstart system/systems.alab.aterm-lan",
            Ran::ok(""),
        );
    sys.on_lan_broker(Err("launchd has a PID, but its socket is not bound".into()))
        .on_lan_broker(Ok(lan::BrokerHello {
            protocol: lan::PROTOCOL,
            build: aterm_stamp::build_id(),
            direct: Some("an allowed direct path is not the installed helper".into()),
        }))
        .on_lan_broker(Ok(lan::BrokerHello {
            protocol: lan::PROTOCOL + 1,
            build: aterm_stamp::build_id(),
            direct: None,
        }))
        .on_lan_broker(Ok(lan::BrokerHello {
            protocol: lan::PROTOCOL,
            build: aterm_stamp::build_id(),
            direct: None,
        }));
    let ctx = ctx(&[]);
    assert!(matches!(
        LanBroker::default().probe(&sys, &ctx),
        Probe::Change { .. }
    ));
    let item = LanBroker::default();
    item.apply(&sys, &ctx).unwrap();
    let calls = sys.calls();
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.as_str() == "installed LAN broker hello")
            .count(),
        4,
        "ordinary apply returned before the installed broker answered its strict protocol"
    );
    for prefix in [
        "clone ",
        "root-own -R ",
        "check staged app ",
        "exchange ",
        "/bin/launchctl bootstrap ",
        "/bin/launchctl kickstart ",
    ] {
        assert_eq!(
            calls.iter().filter(|call| call.starts_with(prefix)).count(),
            1,
            "readiness retains one publication and initial launch: {prefix}: {calls:?}"
        );
    }
    assert!(
        calls.iter().any(|call| call.starts_with("sleep ")),
        "unready strict hellos yield through the owned retry budget"
    );
    let at = |prefix: &str| {
        calls
            .iter()
            .position(|c| c.starts_with(prefix))
            .unwrap_or_else(|| panic!("{prefix}: {calls:?}"))
    };
    let parent = Path::new(lan::IMAGE_DIR);
    let scratch = Fake::scratch_path(Some(parent), "lan-image", 0);
    let staged = scratch.join("aterm.app");
    let staged = staged.display();
    let order = [
        at("root-own /Library/Application Support/aterm/lan"),
        at(&format!("scratch allocated {}", scratch.display())),
        at(&format!("clone {SOURCE} {staged}")),
        at(&format!("root-own -R {staged}")),
        at(&format!("check staged app {staged}")),
        at(&format!("exchange {staged} {}", lan::IMAGE_BUNDLE)),
        at(&format!("rename {staged} {}", lan::IMAGE_BUNDLE)),
        at(&format!("write {} 644", lan::PLIST_PATH)),
        at(&format!("chown 0:0 {}", lan::PLIST_PATH)),
        at("/bin/launchctl bootstrap"),
        at("/bin/launchctl kickstart"),
    ];
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?} {calls:?}");
    assert!(
        calls
            .iter()
            .filter(|c| c.starts_with('/'))
            .all(|c| c.starts_with("/bin/launchctl ")),
        "launchctl alone runs, no aterm: {calls:?}"
    );
    assert_eq!(sys.written(lan::IMAGE_EXECUTABLE), Some(this_build()));
    assert_eq!(sys.mode_of(lan::PLIST_PATH), Some(0o644));
    assert_eq!(item.detail(&sys, &ctx), None);

    assert_eq!(
        LanBroker::default().probe(&sys, &ctx),
        Probe::Ok {
            now: "systems.alab.aterm-lan running in system (pid 77)".to_owned()
        }
    );

    let dev = Fake::root();
    assert!(matches!(
        LanBroker::default().probe(&dev, &ctx),
        Probe::NotApplicable { .. }
    ));
    assert!(LanBroker::default().apply(&dev, &ctx).is_err());
    assert!(dev.calls().is_empty(), "{:?}", dev.calls());

    let refused = from_app();
    refused.app_check(|_, _| Err(CheckError::Refused("another designated image".into())));
    assert!(LanBroker::default().apply(&refused, &ctx).is_err());
    assert!(refused.written(lan::IMAGE_EXECUTABLE).is_none());
    assert!(
        !refused
            .calls()
            .iter()
            .any(|call| call.starts_with("exchange ") || call.starts_with("/bin/launchctl"))
    );

    // Ordinary baselines return an operational staging failure without
    // retrying a new snapshot or publishing an unchecked image.
    let unchecked = from_app();
    unchecked.app_check(|_, _| Err(CheckError::Unchecked("trust service unavailable".into())));
    let why = LanBroker::default().apply(&unchecked, &ctx).unwrap_err();
    assert!(why.contains("trust service unavailable"), "{why}");
    let calls = unchecked.calls();
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.starts_with("clone "))
            .count(),
        1
    );
    assert!(
        !calls.iter().any(|call| {
            call.starts_with("exchange ")
                || call.starts_with("/bin/launchctl")
                || call.starts_with("sleep ")
        }),
        "{calls:?}"
    );

    // Published files are not a running broker. Stop during a launch failure
    // retains the publication but cannot turn that failure into success.
    let failed = from_app();
    failed
        .on(PRINT, Ran::exit(113, "Could not find service"))
        .on(
            "/bin/launchctl bootstrap system /Library/LaunchDaemons/systems.alab.aterm-lan.plist",
            Ran::exit(5, "transient bootstrap failure"),
        );
    failed.on_sleep(|sys, _| {
        sys.stop_now();
    });
    let why = LanBroker::default().apply(&failed, &ctx).unwrap_err();
    assert!(why.contains("stopped"), "{why}");
    let calls = failed.calls();
    for prefix in ["clone ", "exchange ", "/bin/launchctl bootstrap "] {
        assert_eq!(
            calls.iter().filter(|call| call.starts_with(prefix)).count(),
            1,
            "{calls:?}"
        );
    }
    assert!(calls.iter().any(|call| call.starts_with("sleep ")));
    let published = reading(&failed).unwrap().unwrap();
    assert!(published.current());
    assert!(
        !published.owed(),
        "file currency alone sees no remaining work"
    );
    assert!(matches!(
        LanBroker::default().probe(&failed, &ctx),
        Probe::Change { .. }
    ));
}

/// The elevation's own baseline installs the broker before the Command Line Tools, whose
/// download can take minutes while the vault look waits on the broker.
#[test]
fn the_elevation_installs_the_broker_before_the_tools() {
    assert!(
        crate::baseline::parse("schema = 1\n[lan]\nbroker = true\nretained_root_run = true\n")
            .is_err(),
        "a user baseline cannot claim accepted root custody"
    );
    let baseline = crate::baseline::parse(&crate::init_clt::CommandLineTools::baseline()).unwrap();
    let ids: Vec<String> = crate::items::expand(&baseline, &ctx(&[]))
        .unwrap()
        .iter()
        .map(|item| item.id())
        .collect();
    let at = |id: &str| {
        ids.iter()
            .position(|i| i == id)
            .unwrap_or_else(|| panic!("{ids:?}"))
    };
    assert!(
        at("lan.broker") < at(crate::items::github::CLT_ITEM),
        "{ids:?}"
    );
}

/// A broker already running from another build's image is booted out before the new job
/// loads, and the image it ran from is exchanged for the clone, never renamed over.
#[test]
fn a_running_broker_is_replaced_by_exchange_and_restarted() {
    let sys = from_app();
    sys.on_lan_broker(Ok(lan::BrokerHello {
        protocol: lan::PROTOCOL,
        build: aterm_stamp::build_id(),
        direct: None,
    }));
    sys.stat_of(
        lan::IMAGE_BUNDLE,
        crate::port::Stat {
            uid: 0,
            gid: 0,
            mode: 0o755,
            dir: true,
            symlink: false,
            len: 0,
        },
    );
    sys.file(
        lan::IMAGE_EXECUTABLE,
        &install::test_image(0x0100_000c, &[b"old"]),
    )
    .on(
        PRINT,
        Ran::ok("system/systems.alab.aterm-lan = {\n\tpid = 9\n}\n"),
    )
    .on(
        "/bin/launchctl bootout system/systems.alab.aterm-lan",
        Ran::ok(""),
    )
    .on(
        "/bin/launchctl bootstrap system /Library/LaunchDaemons/systems.alab.aterm-lan.plist",
        Ran::ok(""),
    )
    .on(
        "/bin/launchctl kickstart system/systems.alab.aterm-lan",
        Ran::ok(""),
    );
    let ctx = ctx(&[]);
    match LanBroker::default().probe(&sys, &ctx) {
        Probe::Change { from, .. } => {
            assert_eq!(from, "image another build's, plist missing");
        }
        other => panic!("{other:?}"),
    }
    LanBroker::default().apply(&sys, &ctx).unwrap();
    let calls = sys.calls();
    assert!(
        calls.iter().any(|c| c.starts_with("exchange ")),
        "{calls:?}"
    );
    assert!(!calls.iter().any(|c| c.starts_with("rename ")), "{calls:?}");
    let bootout = calls.iter().position(|c| c.contains("bootout")).unwrap();
    let bootstrap = calls.iter().position(|c| c.contains("bootstrap")).unwrap();
    assert!(bootout < bootstrap, "{calls:?}");
    assert_eq!(sys.written(lan::IMAGE_EXECUTABLE), Some(this_build()));

    // A later socket outage leaves the installed files and PID current. The
    // ordinary engine must still enter apply and wait for a strict hello.
    sys.on_lan_broker(Err(
        "the current broker socket is temporarily unavailable".into()
    ));
    // Retire the previous successful observation; the engine's first hello is the outage.
    assert!(matches!(
        LanBroker::default().probe(&sys, &ctx),
        Probe::Ok { .. }
    ));
    let available = std::sync::Once::new();
    sys.on_sleep(move |sys, _| {
        available.call_once(|| {
            sys.on_lan_broker(Ok(lan::BrokerHello {
                protocol: lan::PROTOCOL,
                build: aterm_stamp::build_id(),
                direct: None,
            }));
        });
    });
    let items: Vec<Box<dyn Item>> = vec![Box::new(LanBroker::default())];
    let rows = crate::engine::run(
        &items,
        crate::model::Mode::Apply,
        &sys,
        &ctx,
        &mut crate::engine::NoJournal,
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].outcome, crate::model::Outcome::Changed);
    let calls = sys.calls();
    for prefix in ["clone ", "/bin/launchctl bootstrap "] {
        assert_eq!(
            calls.iter().filter(|call| call.starts_with(prefix)).count(),
            2,
            "the engine repairs the unready current install once: {calls:?}"
        );
    }
}

/// A release executable of `team` whose code is `page`: one CodeDirectory that names
/// [`install::BUNDLE_ID`] and the team ([`install::CodeDirectories::signer`]).
fn release(team: &str, page: u8) -> Vec<u8> {
    let identifier = install::BUNDLE_ID;
    let ident_at = 52_u32;
    let team_at = ident_at + identifier.len() as u32 + 1;
    let mut body = Vec::new();
    for word in [0x20400_u32, 0, 0, ident_at, 0, 0, 0, 0, 0, 0, team_at] {
        body.extend_from_slice(&word.to_be_bytes());
    }
    for text in [identifier, team] {
        body.extend_from_slice(text.as_bytes());
        body.push(0);
    }
    body.push(page);
    install::test_image(0x0100_000c, &[&body])
}

/// A Mac has one broker: a copy of a release (a cut's bundle) reads the serving broker the
/// installed app holds in place, owing nothing and waiting on nothing, so it asks no password
/// to move the job between two bundles of one signer. An elevation that runs anyway installs
/// the bundle it runs from, as every elevation does.
/// NEGATIVE CONTROLS: where the bundle the job names is gone the copy owes the broker, and so
/// does a copy with no team signature (a development build).
#[test]
fn a_serving_broker_another_bundle_of_this_signer_holds_owes_nothing() {
    const COPY: &str = "/Users//alice/aterm/dist/cut-app/aterm.app";
    let copy_exe = format!("{COPY}/{}", install::BUNDLE_EXECUTABLE);
    let team = "ABCDE12345";
    // The install of the bundle at `holder`, read from the copy.
    let from_copy = |holder: &str, copy: &[u8]| {
        let mut sys = from_app();
        sys.running_app = Some(PathBuf::from(COPY));
        sys.file(SOURCE_EXE, &release(team, 1))
            .file(&copy_exe, copy)
            .file(lan::IMAGE_EXECUTABLE, &release(team, 1))
            .file(
                lan::PLIST_PATH,
                install::plist_text(Path::new(holder)).as_bytes(),
            );
        sys.on(
            PRINT,
            Ran::ok("system/systems.alab.aterm-lan = {\n\tpid = 77\n}\n"),
        )
        .on(PRINT_DISABLED, disabled_list("enabled"));
        sys.on_lan_broker(Ok(lan::BrokerHello {
            protocol: lan::PROTOCOL,
            build: aterm_stamp::build_id(),
            direct: None,
        }));
        sys
    };
    let ctx = ctx(&[]);
    let takes_it = |sys: &Fake| {
        let Probe::Change { to, .. } = LanBroker::default().probe(sys, &ctx) else {
            panic!("an elevation installs its own bundle");
        };
        assert!(to.ends_with(&format!("clone of {COPY}")), "{to}");
    };

    let held = from_copy(SOURCE, &release(team, 2));
    let reads = reading(&held).unwrap().unwrap();
    assert_eq!(reads.held_by.as_deref(), Some(Path::new(SOURCE)));
    assert!(reads.current() && !reads.owed());
    assert_eq!(ready(&held), Ok(true));
    takes_it(&held);

    for (why, sys) in [
        (
            "the bundle the job names is gone",
            from_copy("/Users//alice/gone/aterm.app", &release(team, 2)),
        ),
        (
            "a copy with no team signature",
            from_copy(SOURCE, &this_build()),
        ),
    ] {
        let reads = reading(&sys).unwrap().unwrap();
        assert!(reads.owed() && reads.held_by.is_none(), "{why}: {reads:?}");
        assert_eq!(ready(&sys), Ok(false), "{why}");
        takes_it(&sys);
    }
}

const PRINT_DISABLED: &str = "/bin/launchctl print-disabled system";
const BOOTSTRAP: &str =
    "/bin/launchctl bootstrap system /Library/LaunchDaemons/systems.alab.aterm-lan.plist";

/// `launchctl print-disabled system`'s list with the broker's row saying `state`.
fn disabled_list(state: &str) -> Ran {
    Ran::ok(&format!(
        "disabled services = {{\n\t\"com.openssh.sshd\" => disabled\n\t\"{}\" => {state}\n}}\n\
         login item associations = {{\n}}\n",
        lan::LABEL
    ))
}

/// Every launchd call that would change the broker's job.
fn job_changes(sys: &Fake) -> Vec<String> {
    sys.calls()
        .into_iter()
        .filter(|call| {
            ["bootout", "enable", "bootstrap", "kickstart"]
                .iter()
                .any(|verb| call.starts_with(&format!("/bin/launchctl {verb} ")))
        })
        .collect()
}

/// A broker's files in place, this build's.
fn installed() -> Fake {
    let sys = from_app();
    sys.file(lan::IMAGE_EXECUTABLE, &this_build()).file(
        lan::PLIST_PATH,
        install::plist_text(Path::new(SOURCE)).as_bytes(),
    );
    sys
}

/// A hang detector, never a budget: far past any bound, a stop ends a retry loop that has none.
fn stop_after_many_sleeps(sys: &Fake) {
    let sleeps = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    sys.on_sleep(move |sys, _| {
        if sleeps.fetch_add(1, std::sync::atomic::Ordering::SeqCst) > 1000 {
            sys.stop_now();
        }
    });
}

/// A broker job a person switched off (aterm's background item in Login Items, or `launchctl
/// disable`), stopped with its files current or not yet installed, in macOS 13's and earlier
/// spellings, is the person's: the item does not apply, and the root run clones, writes and
/// launches nothing, in words that say how to put it back. NEGATIVE CONTROL: the same stopped
/// job launchd lists enabled is a change. MUTANT: a probe that reads no switch restarts the job.
#[test]
fn a_broker_the_person_switched_off_is_left_off() {
    let ctx = ctx(&[]);
    for (files, state) in [(true, "disabled"), (false, "disabled"), (true, "true")] {
        let sys = if files { installed() } else { from_app() };
        sys.on(PRINT, Ran::exit(113, "Could not find service"))
            .on(PRINT_DISABLED, disabled_list(state))
            .on(BOOTSTRAP, Ran::ok(""))
            .on(
                "/bin/launchctl kickstart system/systems.alab.aterm-lan",
                Ran::ok(""),
            );
        match LanBroker::default().probe(&sys, &ctx) {
            Probe::NotApplicable { why } => {
                assert!(why.contains("switched off"), "{why}");
                assert!(
                    why.contains("sudo launchctl enable system/systems.alab.aterm-lan"),
                    "{why}"
                );
            }
            other => panic!("files={files} {state}: {other:?}"),
        }
        let items: Vec<Box<dyn Item>> = vec![Box::new(LanBroker::default())];
        let rows = crate::engine::run(
            &items,
            crate::model::Mode::Apply,
            &sys,
            &ctx,
            &mut crate::engine::NoJournal,
        );
        assert_eq!(rows[0].outcome, crate::model::Outcome::NotApplicable);
        assert!(job_changes(&sys).is_empty(), "{:?}", sys.calls());
        assert!(
            !sys.calls().iter().any(|call| call.starts_with("clone ")),
            "{:?}",
            sys.calls()
        );
    }
    let enabled = installed();
    enabled
        .on(PRINT, Ran::exit(113, "Could not find service"))
        .on(PRINT_DISABLED, disabled_list("enabled"));
    assert!(matches!(
        LanBroker::default().probe(&enabled, &ctx),
        Probe::Change { .. }
    ));
}

/// The root result waits on nothing for a job the person switched off, installed or not.
/// NEGATIVE CONTROL: the same stopped job launchd lists enabled is not ready. MUTANT: a `ready`
/// that reads no switch holds the result short of a broker that will never run.
#[test]
fn a_switched_off_broker_leaves_the_root_result_nothing_to_wait_on() {
    for files in [true, false] {
        for (state, wanted) in [("disabled", true), ("enabled", false)] {
            let sys = if files { installed() } else { from_app() };
            sys.on(PRINT, Ran::exit(113, "Could not find service"))
                .on(PRINT_DISABLED, disabled_list(state));
            assert_eq!(ready(&sys), Ok(wanted), "files={files} {state}");
        }
    }
}

/// A launch starts a stopped job launchd lists enabled with `bootstrap` and `kickstart` alone:
/// no run of this item ever runs `launchctl enable` on the broker's job, so none undoes a
/// person's switch. MUTANT: a launch that enables the job first.
#[test]
fn a_launch_never_enables_the_job() {
    let enabled = installed();
    enabled
        .on(PRINT, Ran::exit(113, "Could not find service"))
        .on(PRINT, Ran::exit(113, "Could not find service"))
        .on(
            PRINT,
            Ran::ok("system/systems.alab.aterm-lan = {\n\tpid = 77\n}\n"),
        )
        .on(PRINT_DISABLED, disabled_list("enabled"))
        // Answered, so a launch that ran it would be seen running it.
        .on(
            "/bin/launchctl enable system/systems.alab.aterm-lan",
            Ran::ok(""),
        )
        .on(BOOTSTRAP, Ran::ok(""))
        .on(
            "/bin/launchctl kickstart system/systems.alab.aterm-lan",
            Ran::ok(""),
        )
        .on_lan_broker(Ok(lan::BrokerHello {
            protocol: lan::PROTOCOL,
            build: aterm_stamp::build_id(),
            direct: None,
        }));
    let ctx = ctx(&[]);
    assert_eq!(
        LanBroker::default().probe(&enabled, &ctx),
        Probe::Change {
            from: "installed, not serving".into(),
            to: format!(
                "{} running in system from a root-owned clone of {SOURCE}",
                lan::LABEL
            ),
        }
    );
    LanBroker::default().apply(&enabled, &ctx).unwrap();
    assert_eq!(
        job_changes(&enabled),
        vec![
            BOOTSTRAP.to_owned(),
            "/bin/launchctl kickstart system/systems.alab.aterm-lan".to_owned()
        ],
    );
}

/// A person who switches the job off while the root run retries a refused launch ends that run
/// at its next round, in words that say how to put it back, with no further launch and no
/// further pause. MUTANT: a round that reads no switch before its launch launches again; one
/// that retries a switched-off job pauses and reads the switch again, round after round.
#[test]
fn a_switch_off_during_the_root_run_ends_it() {
    let ctx = ctx(&[]);
    let during = from_app();
    during
        .on(PRINT, Ran::exit(113, "Could not find service"))
        .on(PRINT_DISABLED, disabled_list("enabled"))
        .on(PRINT_DISABLED, disabled_list("enabled"))
        .on(PRINT_DISABLED, disabled_list("disabled"))
        .on(
            "/bin/launchctl enable system/systems.alab.aterm-lan",
            Ran::ok(""),
        )
        .on(
            BOOTSTRAP,
            Ran::exit(5, "Bootstrap failed: 5: Input/output error"),
        );
    stop_after_many_sleeps(&during);
    let item = LanBroker {
        retained_root_run: true,
        ..LanBroker::default()
    };
    assert!(matches!(item.probe(&during, &ctx), Probe::Change { .. }));
    let why = item.apply(&during, &ctx).unwrap_err();
    assert!(why.contains("switched off"), "{why}");
    assert_eq!(job_changes(&during), vec![BOOTSTRAP.to_owned()]);
    // The probe's read, the first round's and the second's, which reads it switched off: the run
    // ends there.
    let calls = during.calls();
    let reads: Vec<usize> = calls
        .iter()
        .enumerate()
        .filter_map(|(at, call)| (call == PRINT_DISABLED).then_some(at))
        .collect();
    assert_eq!(reads.len(), 3, "{calls:?}");
    assert!(
        !calls[reads[2]..]
            .iter()
            .any(|call| call.starts_with("sleep ")),
        "{calls:?}"
    );
}

/// A staging step that keeps failing in a way a retained root run retries (the app-copy check
/// unanswered or not run) ends the run after [`ROUNDS`] rounds with that step's reason, as a
/// refused launch does. MUTANT: a retried staging error that counts no round retries until the
/// test's hang detector stops the run.
#[test]
fn a_staging_step_that_keeps_failing_ends_the_root_run_with_its_reason() {
    for failed in [
        CheckError::Unanswered("the trust service did not answer".into()),
        CheckError::Unchecked("trust service unavailable".into()),
    ] {
        let sys = from_app();
        let check = failed.clone();
        sys.app_check(move |_, _| Err(check.clone()));
        stop_after_many_sleeps(&sys);
        let item = LanBroker {
            retained_root_run: true,
            ..LanBroker::default()
        };
        let why = item.apply(&sys, &ctx(&[])).unwrap_err();
        assert!(why.contains(&failed.to_string()), "{failed:?}: {why}");
        assert!(!why.contains("stopped"), "{why}");
        let calls = sys.calls();
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.starts_with("check staged app "))
                .count(),
            ROUNDS as usize,
            "{calls:?}"
        );
        assert!(job_changes(&sys).is_empty(), "{calls:?}");
        // A check unanswered or not run may be the Mac's state or the bundle's, which this
        // run cannot tell apart: nothing is recorded, and the step is asked for again.
        assert!(
            sys.written(aterm_types::macos_setup::refused::LAN_BROKER)
                .is_none()
        );
    }
}

/// A launch launchd keeps refusing ends the root run after [`ROUNDS`] rounds, at the capped
/// retry rate, with launchd's own reason, retained root custody or not: the Mac Setup tab's root
/// run ends. MUTANT: the unbounded retry loop goes on until the test's hang detector stops it, so
/// the run ends `stopped`, with more launches than the bound.
#[test]
fn a_launch_launchd_keeps_refusing_ends_the_root_run_with_its_reason() {
    for retained_root_run in [false, true] {
        let sys = from_app();
        sys.on(PRINT, Ran::exit(113, "Could not find service"))
            .on(PRINT_DISABLED, disabled_list("enabled"))
            .on(
                "/bin/launchctl enable system/systems.alab.aterm-lan",
                Ran::ok(""),
            )
            .on(
                BOOTSTRAP,
                Ran::exit(5, "Bootstrap failed: 5: Input/output error"),
            );
        stop_after_many_sleeps(&sys);
        let item = LanBroker {
            retained_root_run,
            ..LanBroker::default()
        };
        let why = item.apply(&sys, &ctx(&[])).unwrap_err();
        assert!(
            why.contains("Bootstrap failed: 5: Input/output error"),
            "launchd's reason: {why}"
        );
        assert!(!why.contains("stopped"), "{why}");
        let calls = sys.calls();
        assert_eq!(
            calls.iter().filter(|call| *call == BOOTSTRAP).count(),
            ROUNDS as usize,
            "{calls:?}"
        );
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.starts_with("clone "))
                .count(),
            1,
            "{calls:?}"
        );
        let slept: u128 = calls
            .iter()
            .filter_map(|call| {
                call.strip_prefix("sleep ")?
                    .strip_suffix("ms")?
                    .parse::<u128>()
                    .ok()
            })
            .sum();
        assert!(
            (5_000..=10 * 60 * 1000).contains(&slept),
            "the rounds pause at the capped rate, in total {slept} ms"
        );
        // The job this bundle published did not serve in any round: recorded for this build.
        assert_eq!(
            sys.written(aterm_types::macos_setup::refused::LAN_BROKER),
            Some(format!("{}\n", refusal_key(&sys).unwrap()).into_bytes())
        );
    }
}

/// A broker install that fails for good (the strict check refuses this bundle's clone) is
/// recorded with the bundle and build it failed for, so a run nobody asked for does not ask the
/// Mac password for it again (`root_steps::refused`); the apply that succeeds removes the
/// record. NEGATIVE CONTROLS: nothing stands before any apply; a stopped run and a job
/// switched off record nothing (an unanswered check and a refused launch are held by the two
/// tests above), and neither does a run whose job did not serve only in its last round, the
/// rounds before it lost to an unanswered check; the record does not stand for another build
/// of the bundle (of the same length) or for the same build in another bundle.
#[test]
fn a_broker_root_could_not_install_is_recorded_until_an_apply_succeeds() {
    use crate::root_steps::{Owed, refused};
    use aterm_types::macos_setup::refused::LAN_BROKER;
    let ctx = ctx(&[]);
    let key = refusal_key(&from_app()).unwrap();
    assert!(key.ends_with(&format!(" {SOURCE}")), "{key}");
    assert!(!refused(&from_app(), Owed::LanBroker));

    for retained_root_run in [false, true] {
        let sys = from_app();
        sys.app_check(|_, _| Err(CheckError::Refused("not this team's code".into())));
        stop_after_many_sleeps(&sys);
        let item = LanBroker {
            retained_root_run,
            ..LanBroker::default()
        };
        let why = item.apply(&sys, &ctx).unwrap_err();
        assert!(why.contains("not this team's code"), "{why}");
        assert_eq!(
            sys.written(LAN_BROKER),
            Some(format!("{key}\n").into_bytes())
        );
        assert!(refused(&sys, Owed::LanBroker));
        assert!(!refused(&sys, Owed::Debugger));
        // Another build at the same path, and the same build in another bundle, are asked for.
        sys.file(
            SOURCE_EXE,
            &install::test_image(0x0100_000c, &[b"that build"]),
        );
        assert!(!refused(&sys, Owed::LanBroker));
        let mut moved = from_app();
        moved.running_app = Some(PathBuf::from("/Users//alice/Applications/aterm.app"));
        moved
            .file(
                "/Users//alice/Applications/aterm.app/Contents/MacOS/aterm",
                &this_build(),
            )
            .file(LAN_BROKER, format!("{key}\n").as_bytes());
        assert!(!refused(&moved, Owed::LanBroker));
    }

    // The check goes unanswered in every round but the last, whose published job launchd
    // refuses once: one launch of a bundle the Mac was too busy to check says nothing of it.
    let late = from_app();
    let checks = std::sync::atomic::AtomicU32::new(0);
    late.app_check(move |_, _| {
        if checks.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1 < ROUNDS {
            Err(CheckError::Unanswered(
                "the trust service did not answer".into(),
            ))
        } else {
            Ok(())
        }
    });
    late.on(PRINT, Ran::exit(113, "Could not find service"))
        .on(PRINT_DISABLED, disabled_list("enabled"))
        .on(
            "/bin/launchctl enable system/systems.alab.aterm-lan",
            Ran::ok(""),
        )
        .on(
            BOOTSTRAP,
            Ran::exit(5, "Bootstrap failed: 5: Input/output error"),
        );
    stop_after_many_sleeps(&late);
    let item = LanBroker {
        retained_root_run: true,
        ..LanBroker::default()
    };
    let why = item.apply(&late, &ctx).unwrap_err();
    assert!(why.contains("Bootstrap failed"), "{why}");
    assert_eq!(
        late.calls()
            .iter()
            .filter(|call| *call == BOOTSTRAP)
            .count(),
        1
    );
    assert!(late.written(LAN_BROKER).is_none());

    // A stop ends the run with nothing said of the bundle.
    let stopped = from_app();
    stopped.stop_now();
    assert!(LanBroker::default().apply(&stopped, &ctx).is_err());
    assert!(stopped.written(LAN_BROKER).is_none());
    // So does a job the person switched off while the run held root.
    let off = from_app();
    off.on(PRINT, Ran::exit(113, "Could not find service"))
        .on(PRINT_DISABLED, disabled_list("disabled"));
    let why = LanBroker::default().apply(&off, &ctx).unwrap_err();
    assert!(why.contains("switched off"), "{why}");
    assert!(off.written(LAN_BROKER).is_none());

    // The install that succeeds removes the record, whatever keys it held.
    let sys = from_app();
    sys.file(LAN_BROKER, format!("{key}\nan older build\n").as_bytes())
        .on(PRINT, Ran::exit(113, "Could not find service"))
        .on(
            PRINT,
            Ran::ok("system/systems.alab.aterm-lan = {\n\tpid = 77\n}\n"),
        )
        .on(PRINT_DISABLED, disabled_list("enabled"))
        .on(BOOTSTRAP, Ran::ok(""))
        .on(
            "/bin/launchctl kickstart system/systems.alab.aterm-lan",
            Ran::ok(""),
        )
        .on_lan_broker(Ok(lan::BrokerHello {
            protocol: lan::PROTOCOL,
            build: aterm_stamp::build_id(),
            direct: None,
        }));
    assert!(refused(&sys, Owed::LanBroker));
    LanBroker::default().apply(&sys, &ctx).unwrap();
    assert!(sys.written(LAN_BROKER).is_none());
    assert!(!refused(&sys, Owed::LanBroker));
}
