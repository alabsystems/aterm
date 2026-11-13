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
            "/bin/launchctl enable system/systems.alab.aterm-lan",
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
        at("/bin/launchctl enable"),
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
            "/bin/launchctl enable system/systems.alab.aterm-lan",
            Ran::ok(""),
        )
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
        "/bin/launchctl enable system/systems.alab.aterm-lan",
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
