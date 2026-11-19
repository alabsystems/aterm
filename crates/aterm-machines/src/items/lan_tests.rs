// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::items::defaults::tests::ctx;
use crate::port::{Fake, Ran};
use std::path::PathBuf;

const SOURCE: &str = "/Applications/aterm.app";
const SOURCE_EXE: &str = "/Applications/aterm.app/Contents/MacOS/aterm";
const SOURCE_INFO: &str = "/Applications/aterm.app/Contents/Info.plist";
/// The team this build's release names.
const TEAM: &str = "ABCDE12345";
/// The build this release's `Info.plist` states.
const OURS: u64 = 1_790_120_495;
const PRINT: &str = "/bin/launchctl print system/systems.alab.aterm-lan";

#[cfg(unix)]
#[test]
fn reverting_disables_before_cleanup_and_preserves_files_when_disable_fails() {
    let revert = LanBroker::default()
        .revert()
        .unwrap()
        .replace(super::super::sharing::LAUNCHCTL, "record_launchctl")
        .replace("/bin/rm", "record_remove");
    // Execute the journal's shell control flow with inert functions: no launchd operation
    // or file removal runs, including when the job has already disappeared.
    let script = format!(
        r#"
record_launchctl() {{
    printf 'launchctl:%s\n' "$1"
    case "$1" in
        disable) return "$DISABLE_EXIT" ;;
        bootout) return "$BOOTOUT_EXIT" ;;
        *) return 99 ;;
    esac
}}
record_remove() {{ printf 'remove:%s\n' "$1"; }}
{revert}
"#
    );
    for (disable, bootout, exit, calls) in [
        (
            "0",
            "0",
            0,
            "launchctl:disable\nlaunchctl:bootout\nremove:-f\nremove:-rf\n",
        ),
        ("7", "0", 7, "launchctl:disable\n"),
        (
            "0",
            "113",
            0,
            "launchctl:disable\nlaunchctl:bootout\nremove:-f\nremove:-rf\n",
        ),
    ] {
        let mut command = aterm_uds::spawnfd::command("/bin/sh");
        command
            .args(["-c", &script])
            .env("DISABLE_EXIT", disable)
            .env("BOOTOUT_EXIT", bootout);
        let output = aterm_uds::deadline::output_while_process_progressing(
            &mut command,
            aterm_uds::deadline::ECHO_HANG,
            "the LAN broker revert's inert shell fixture",
        )
        .unwrap();
        assert_eq!(output.status.code(), Some(exit), "{output:?}");
        assert_eq!(String::from_utf8_lossy(&output.stdout), calls);
        assert!(output.stderr.is_empty(), "{output:?}");
    }
}

/// This build's executable: a release of [`TEAM`] sealing its `Info.plist`, which states [`OURS`].
fn this_build() -> Vec<u8> {
    install::test_release(TEAM, 0, &install::test_info(OURS))
}

fn from_app() -> Fake {
    let mut sys = Fake::root();
    as_app(&mut sys);
    sys
}

/// `sys` running from this build's bundle ([`SOURCE`]).
fn as_app(sys: &mut Fake) {
    sys.born(std::process::id(), 123);
    sys.running_app = Some(PathBuf::from(SOURCE));
    sys.file(SOURCE_EXE, &this_build())
        .file(SOURCE_INFO, &install::test_info(OURS));
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
}

/// `sys` running from this build's bundle, with this build's broker installed and launchd
/// running it: a root LAN broker of this build serves
/// ([`crate::init_spotlight::broker_serves`]).
pub(crate) fn serving(sys: &mut Fake) {
    as_app(sys);
    install_on(sys);
    sys.on(
        PRINT,
        Ran::ok("system/systems.alab.aterm-lan = {\n\tpid = 77\n}\n"),
    );
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

/// The image's `Info.plist`.
fn image_info() -> String {
    format!("{}/{}", lan::IMAGE_BUNDLE, install::BUNDLE_INFO)
}

/// launchd's listing of the broker's job: with a pid where `pid`, else none.
fn listed(pid: bool) -> Ran {
    if pid {
        Ran::ok("system/systems.alab.aterm-lan = {\n\tpid = 77\n}\n")
    } else {
        Ran::exit(113, "Could not find service")
    }
}

/// A Mac has one broker, and its image only moves up. A copy of this release reads the image
/// another bundle of it made in place (any bundle's plist is this build's), owing nothing and
/// ready, though the bundle the plist names is gone. A development build owes nothing outside a
/// VM guest and is ready while a team image serves, and in a guest it owes the broker as before.
/// An older build beside a newer image keeps it while launchd lists its pid, whatever its hello
/// says, and asks for a change only where launchd lists none.
/// NEGATIVE CONTROLS (run): without the probe's Development arm the development build asks for a
/// change; without the Kept pid rule the older build asks to replace the newer image.
#[test]
fn a_mac_has_one_broker_and_its_image_only_moves_up() {
    const COPY: &str = "/Users//alice/aterm/dist/cut-app/aterm.app";
    let copy_exe = format!("{COPY}/{}", install::BUNDLE_EXECUTABLE);
    let copy_info = format!("{COPY}/{}", install::BUNDLE_INFO);
    let ctx = ctx(&[]);
    // The install the bundle `holder` made, its image of build `image`, read from a copy whose
    // executable is `copy`, of build `ours`.
    let from_copy = |holder: &str, copy: &[u8], ours: u64, image: u64, pid: bool| {
        let mut sys = from_app();
        sys.running_app = Some(PathBuf::from(COPY));
        sys.file(&copy_exe, copy)
            .file(&copy_info, &install::test_info(ours))
            .file(
                lan::IMAGE_EXECUTABLE,
                &install::test_release(TEAM, 1, &install::test_info(image)),
            )
            .file(&image_info(), &install::test_info(image))
            .file(
                lan::PLIST_PATH,
                install::plist_text(Path::new(holder)).as_bytes(),
            );
        sys.on(PRINT, listed(pid))
            .on(PRINT_DISABLED, disabled_list("enabled"));
        sys.on_lan_broker(Ok(lan::BrokerHello {
            protocol: lan::PROTOCOL,
            build: aterm_stamp::build_id(),
            direct: None,
        }));
        sys
    };
    let release =
        |page: u8, build: u64| install::test_release(TEAM, page, &install::test_info(build));

    // The image a copy of this very build made, from a bundle since deleted.
    let copied = install::test_release(TEAM, 1, &install::test_info(OURS));
    let copy = from_copy("/Users//alice/gone/aterm.app", &copied, OURS, OURS, true);
    let reads = reading(&copy).unwrap().unwrap();
    assert!(reads.current() && !reads.owed(), "{reads:?}");
    assert_eq!(ready(&copy), Ok(true));
    assert!(matches!(
        LanBroker::default().probe(&copy, &ctx),
        Probe::Ok { .. }
    ));

    // A development build: nothing owed and ready on a real Mac, though a team image serves.
    let development = install::test_image(0x0100_000c, &[b"a development build"]);
    for guest in [None, Some(false)] {
        let mut sys = from_copy(SOURCE, &development, OURS, OURS, true);
        sys.vm_guest = guest;
        let reads = reading(&sys).unwrap().unwrap();
        assert_eq!(reads.image, install::Part::Development, "{guest:?}");
        assert!(!reads.owed(), "{guest:?}");
        assert_eq!(ready(&sys), Ok(true), "{guest:?}");
        assert!(
            matches!(LanBroker::default().probe(&sys, &ctx), Probe::NotApplicable { ref why } if why == DEVELOPMENT),
            "{guest:?}"
        );
        assert!(
            !crate::root_steps::owed(&sys).contains(&crate::root_steps::Owed::LanBroker),
            "{guest:?}"
        );
    }
    let mut guest = from_copy(SOURCE, &development, OURS, OURS, true);
    guest.vm_guest = Some(true);
    assert!(reading(&guest).unwrap().unwrap().owed());
    assert!(matches!(
        LanBroker::default().probe(&guest, &ctx),
        Probe::Change { .. }
    ));
    assert!(crate::root_steps::owed(&guest).contains(&crate::root_steps::Owed::LanBroker));

    // An older build beside a newer image keeps it while launchd lists its pid, whatever the
    // hello says.
    let kept = from_copy(SOURCE, &release(2, OURS - 20), OURS - 20, OURS, true);
    kept.lan_broker_now(Err(lan::Unavailable::Unchecked(
        "trustd did not answer".into(),
    )
    .to_string()));
    let reads = reading(&kept).unwrap().unwrap();
    assert!(
        matches!(reads.image, install::Part::Kept { image, ours, .. } if image == OURS && ours == OURS - 20),
        "{reads:?}"
    );
    assert!(!reads.owed() && reads.in_place());
    assert!(
        matches!(LanBroker::default().probe(&kept, &ctx), Probe::Ok { ref now } if now.contains("pid 77")),
    );
    assert_eq!(ready(&kept), Ok(true));
    let stopped = from_copy(SOURCE, &release(2, OURS - 20), OURS - 20, OURS, false);
    assert!(matches!(
        LanBroker::default().probe(&stopped, &ctx),
        Probe::Change { .. }
    ));
    assert_eq!(ready(&stopped), Ok(false));
}

/// This build beside a newer image of its signer (build `OURS + 20`) that wrote the plist
/// `plist` (none where `None`), launchd's `launchctl print` of the job answering `listing`.
fn beside_a_newer_image(plist: Option<&[u8]>, listing: Ran) -> Fake {
    let sys = from_app();
    sys.file(
        lan::IMAGE_EXECUTABLE,
        &install::test_release(TEAM, 3, &install::test_info(OURS + 20)),
    )
    .file(&image_info(), &install::test_info(OURS + 20));
    if let Some(plist) = plist {
        sys.file(lan::PLIST_PATH, plist);
    }
    sys.on(PRINT, listing)
        .on(PRINT_DISABLED, disabled_list("enabled"));
    sys.on_lan_broker(Ok(lan::BrokerHello {
        protocol: lan::PROTOCOL,
        build: "0.300.0+gfeedbee".into(),
        direct: None,
    }));
    sys
}

/// An older build never replaces a newer image on what launchd cannot say: where `launchctl
/// print` fails the probe measures nothing and nothing is owed, and where launchd holds the job
/// with no pid (between two of its starts) the image is kept and ready. Whatever plist the newer
/// build wrote (one this build would not write, or none) owes this build nothing: the reading,
/// the root steps, the probe and readiness agree, so no Mac password is asked at every launch.
/// At the effect, an install round that finds such an image held by launchd publishes and changes
/// nothing. NEGATIVE CONTROLS (run): with the probe reading a failed or pid-less listing as none,
/// it asks to replace the newer image; without the effect's own reading the round publishes this
/// build's older image over it.
#[test]
fn an_older_build_never_replaces_a_newer_image_on_what_launchd_cannot_say() {
    let ctx = ctx(&[]);
    let current = install::plist_text(Path::new(SOURCE)).into_bytes();
    let owes = |sys: &Fake| {
        let owed = crate::root_steps::owed(sys);
        owed.contains(&crate::root_steps::Owed::LanBroker)
            || owed.contains(&crate::root_steps::Owed::LanBrokerAdoption)
    };

    let unsaid = beside_a_newer_image(Some(&current), Ran::exit(5, "Operation not permitted"));
    assert!(
        matches!(LanBroker::default().probe(&unsaid, &ctx), Probe::Unusable { ref why } if why.contains("could not say")),
    );
    assert!(!owes(&unsaid));

    let restarting = beside_a_newer_image(
        Some(&current),
        Ran::ok("system/systems.alab.aterm-lan = {\n\tstate = waiting\n}\n"),
    );
    assert!(
        matches!(LanBroker::default().probe(&restarting, &ctx), Probe::Ok { ref now } if now.contains("loaded")),
    );
    assert_eq!(ready(&restarting), Ok(true));
    assert!(!owes(&restarting));

    for plist in [
        Some(b"<plist>the newer build's own</plist>".as_slice()),
        None,
    ] {
        let sys = beside_a_newer_image(plist, listed(true));
        let reads = reading(&sys).unwrap().unwrap();
        assert!(
            matches!(reads.image, install::Part::Kept { .. }) && !reads.owed() && reads.in_place(),
            "{reads:?}"
        );
        assert!(!owes(&sys), "{reads:?}");
        assert_eq!(ready(&sys), Ok(true), "{reads:?}");
        assert!(matches!(
            LanBroker::default().probe(&sys, &ctx),
            Probe::Ok { .. }
        ));
    }

    let held = beside_a_newer_image(Some(&current), listed(true));
    let newer = held.written(lan::IMAGE_EXECUTABLE);
    let applied = LanBroker::default().apply(&held, &ctx);
    let calls = held.calls();
    assert!(
        !calls
            .iter()
            .any(|call| call.starts_with("exchange ") || call.starts_with("rename ")),
        "{calls:?}"
    );
    applied.unwrap();
    assert!(job_changes(&held).is_empty(), "{calls:?}");
    assert_eq!(held.written(lan::IMAGE_EXECUTABLE), newer);
}

/// A newer image launchd holds no job for (none loaded, or one a newer build's own restart booted
/// out) is loaded in place: the probe asks to load it, never to replace it, and the effect
/// publishes nothing over it, keeps the plist a build wrote (the newer build's, whatever it
/// says), writes this build's only where none is, and loads and starts the job with no
/// `bootout`, so a newer build's own restart is never taken down. NEGATIVE CONTROLS (run):
/// without the effect's load in place the round clones this build's bundle for nothing and then
/// refuses, loading nothing; with the effect's earlier reading, which let a job launchd holds none
/// of be replaced, this build's older image is published over the newer one.
#[test]
fn a_newer_image_launchd_holds_no_job_for_is_loaded_in_place() {
    let ctx = ctx(&[]);
    let theirs = b"<plist>the newer build's own</plist>".to_vec();
    for plist in [Some(theirs.clone()), None] {
        let sys = beside_a_newer_image(plist.as_deref(), listed(false));
        sys.on(PRINT, listed(false))
            .on(PRINT, listed(true))
            .on(BOOTSTRAP, Ran::ok(""))
            .on(
                "/bin/launchctl kickstart system/systems.alab.aterm-lan",
                Ran::ok(""),
            );
        let newer = sys.written(lan::IMAGE_EXECUTABLE);
        assert!(
            matches!(LanBroker::default().probe(&sys, &ctx), Probe::Change { ref to, .. } if to.contains("loaded in place")),
            "{plist:?}"
        );
        let applied = LanBroker::default().apply(&sys, &ctx);
        let calls = sys.calls();
        assert!(
            !calls.iter().any(|call| call.starts_with("exchange ")
                || call.starts_with("rename ")
                || call.starts_with("clone ")),
            "{calls:?}"
        );
        applied.unwrap();
        assert_eq!(
            job_changes(&sys),
            vec![
                BOOTSTRAP.to_owned(),
                "/bin/launchctl kickstart system/systems.alab.aterm-lan".to_owned()
            ],
            "{calls:?}"
        );
        assert_eq!(sys.written(lan::IMAGE_EXECUTABLE), newer);
        assert_eq!(
            sys.written(lan::PLIST_PATH),
            Some(plist.unwrap_or_else(|| install::plist_text(Path::new(SOURCE)).into_bytes())),
            "a build's plist stays; this build's is written only where none is"
        );
        assert!(
            !crate::root_steps::owed(&sys).contains(&crate::root_steps::Owed::LanBroker),
            "a kept image owes this build nothing"
        );
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
    install_on(&sys);
    sys
}

/// This build's broker's files, written on `sys`.
fn install_on(sys: &Fake) {
    sys.file(lan::IMAGE_EXECUTABLE, &this_build()).file(
        lan::PLIST_PATH,
        install::plist_text(Path::new(SOURCE)).as_bytes(),
    );
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
/// refused launch does. A check unanswered or not run may be the Mac's state or the bundle's,
/// which one run cannot tell apart: the run's no-verdict is pending and asks again; one
/// [`adoption::STALL`] later in the same boot stalls the step for that boot; one after a restart
/// is the build's own. MUTANT: a retried staging error that counts no round retries until the
/// test's hang detector stops the run. Run: a no-verdict taken as the run's own failure
/// (`unserved == rounds` alone) records the key at once and fails it.
#[test]
fn a_staging_step_that_keeps_failing_ends_the_root_run_with_its_reason() {
    use crate::root_steps::{Owed, refused};
    use aterm_types::macos_setup::refused::LAN_BROKER;
    const SESSION: &str = "/usr/sbin/sysctl -n kern.bootsessionuuid";
    const BOOT: &str = "01234567-89AB-CDEF-0123-456789ABCDEF\n";
    const BOOT_WORD: &str = "0123456789abcdef0123456789abcdef";
    const LATER_BOOT: &str = "11111111-2222-3333-4444-555555555555\n";
    for failed in [
        CheckError::Unanswered("the trust service did not answer".into()),
        CheckError::Unchecked("trust service unavailable".into()),
    ] {
        let mut sys = from_app();
        let check = failed.clone();
        sys.app_check(move |_, _| Err(check.clone()));
        let boot = std::sync::Arc::new(std::sync::Mutex::new(BOOT));
        let reads = boot.clone();
        sys.simulate(move |_, command| {
            (command.display() == SESSION).then(|| Ran::ok(*reads.lock().unwrap()))
        });
        stop_after_many_sleeps(&sys);
        let item = LanBroker {
            retained_root_run: true,
            ..LanBroker::default()
        };
        let key = refusal_key(&sys).unwrap();
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
        let pending = format!("no-verdict {} {BOOT_WORD} {key}\n", sys.now);
        assert_eq!(
            sys.written(LAN_BROKER).as_deref(),
            Some(pending.as_bytes()),
            "{failed:?}"
        );
        assert!(!refused(&sys, Owed::LanBroker), "{failed:?}");
        sys.now += adoption::STALL.as_secs() - 1;
        item.apply(&sys, &ctx(&[])).unwrap_err();
        assert_eq!(
            sys.written(LAN_BROKER).as_deref(),
            Some(pending.as_bytes()),
            "a second just short of the stall is still pending: {failed:?}"
        );
        assert!(!refused(&sys, Owed::LanBroker), "{failed:?}");
        sys.now += 1;
        item.apply(&sys, &ctx(&[])).unwrap_err();
        let stalled = format!("stalled {BOOT_WORD} {key}\n");
        assert_eq!(
            sys.written(LAN_BROKER).as_deref(),
            Some(stalled.as_bytes()),
            "{failed:?}"
        );
        assert!(refused(&sys, Owed::LanBroker), "{failed:?}");
        *boot.lock().unwrap() = LATER_BOOT;
        assert!(!refused(&sys, Owed::LanBroker), "another boot asks again");
        item.apply(&sys, &ctx(&[])).unwrap_err();
        assert_eq!(
            sys.written(LAN_BROKER),
            Some(format!("{key}\n").into_bytes()),
            "{failed:?}"
        );
        assert!(refused(&sys, Owed::LanBroker), "{failed:?}");
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
/// record, its pending and stalled lines with it. NEGATIVE CONTROLS: nothing stands before any
/// apply; a stopped run and a job switched off record nothing (an unanswered check and a
/// refused launch are held by the two tests above); a run whose job did not serve only in its
/// last round, the rounds before it lost to an unanswered check, gave no verdict and stands for
/// nothing; the record does not stand for another build of the bundle (of the same length) or
/// for the same build in another bundle.
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
    // refuses once: one launch of a bundle the Mac was too busy to check gives no verdict.
    let late = from_app();
    late.on(
        "/usr/sbin/sysctl -n kern.bootsessionuuid",
        Ran::ok("01234567-89AB-CDEF-0123-456789ABCDEF\n"),
    );
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
    assert_eq!(
        late.written(LAN_BROKER),
        Some(
            format!(
                "no-verdict {} 0123456789abcdef0123456789abcdef {key}\n",
                late.now
            )
            .into_bytes()
        )
    );
    assert!(!refused(&late, Owed::LanBroker));

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

    // The install that succeeds removes the record, whatever lines it held.
    let sys = from_app();
    sys.file(
        LAN_BROKER,
        format!("{key}\nan older build\nno-verdict 1 b1 {key}\nstalled b1 {key}\n").as_bytes(),
    )
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

/// A broker of this build that serves (its plist and image current, launchd running its job)
/// and would take every folder owed takes Spotlight's scope of a home setup's password never
/// scoped off the automatic ask: it excludes them with no password. Here the receipt names every
/// folder beyond the home's top level, so the home's own top-level folders are all that is
/// owed; a home with none of the scope excluded still owes ~/Library's and the data volume's
/// folders, which the broker never takes, so the password is asked with the broker serving. A
/// broker launchd does not run, one whose plist this build does not write, and one that would refuse
/// the home (a sticky home) leave the ask to the password, and so does one that already left
/// the home's folders as they are (this account's record of them), which `sync.spotlight` then
/// reads as no work of its own for a day, and asks the broker again after it while the password
/// stays owed.
/// NEGATIVE CONTROLS: an automatic reading that does not read the broker asks the password on
/// the served Mac too; a reading that leaves out the record asks the broker for them again; an
/// automatic reading that drops a record past its day owes the password nothing.
#[test]
fn a_serving_broker_of_this_build_takes_spotlight_off_the_automatic_ask() {
    use aterm_types::macos_setup::spotlight_scope::{self, Leave};
    let folder = |uid: u32, mode: u32| crate::port::Stat {
        uid,
        gid: 0,
        mode,
        dir: true,
        symlink: false,
        len: 0,
    };
    let never_scoped = |print: Ran| {
        let mut sys = installed();
        sys.euid = 501;
        crate::init_spotlight::tests::home(&sys);
        sys.stat_of("/", folder(0, 0o755))
            .stat_of("/Users", folder(0, 0o755))
            .stat_of("/Users//alice", folder(501, 0o750));
        sys.logged_in(&crate::items::defaults::tests::user("alice", 501))
            .stat_of(
                "/dev/console",
                crate::port::Stat {
                    uid: 501,
                    gid: 0,
                    mode: 0o600,
                    dir: false,
                    symlink: false,
                    len: 0,
                },
            )
            .on(
                "/usr/bin/mdutil -s /System/Volumes/Data",
                Ran::ok("/System/Volumes/Data:\n\tIndexing enabled.\n"),
            )
            .on(PRINT, print);
        sys
    };
    let account = |print: Ran| {
        let sys = never_scoped(print);
        sys.file(
            crate::init_spotlight::SCOPE_RECEIPT,
            crate::init_spotlight::tests::BEYOND_THE_HOME,
        );
        sys
    };
    let running = || Ran::ok("system/systems.alab.aterm-lan = {\n\tpid = 77\n}\n");
    let never = never_scoped(running());
    assert!(crate::init_spotlight::broker_serves(&never));
    assert!(crate::init_spotlight::automatic_scope_owed(&never));
    let served = account(running());
    assert!(crate::init_spotlight::broker_serves(&served));
    assert!(!crate::init_spotlight::automatic_scope_owed(&served));
    assert!(
        crate::init_spotlight::scope_owed(&served),
        "still work, the broker's"
    );

    let stopped = account(Ran::exit(113, "Could not find service"));
    assert!(!crate::init_spotlight::broker_serves(&stopped));
    assert!(crate::init_spotlight::automatic_scope_owed(&stopped));

    let elsewhere = account(running());
    elsewhere.file(lan::PLIST_PATH, b"<plist>another build's</plist>\n");
    assert!(!crate::init_spotlight::broker_serves(&elsewhere));
    assert!(crate::init_spotlight::automatic_scope_owed(&elsewhere));

    let sticky = account(running());
    sticky.stat_of("/Users//alice", folder(501, 0o1750));
    assert!(crate::init_spotlight::broker_serves(&sticky));
    assert!(crate::init_spotlight::automatic_scope_owed(&sticky));

    let Some(Ok(owed)) = crate::init_spotlight::broker_reading(&served) else {
        panic!("the served home is the broker's to exclude");
    };
    assert!(!owed.is_empty());
    let mut left = account(running());
    let state = PathBuf::from("/Users//alice/Library/Application Support/aterm/state");
    left.account_state = Some(state.clone());
    let record = state.join(spotlight_scope::BROKER_LEFT);
    left.file(
        &record.to_string_lossy(),
        spotlight_scope::broker_left_text(&owed, left.now).as_bytes(),
    );
    assert!(crate::init_spotlight::broker_serves(&left));
    assert_eq!(
        crate::init_spotlight::broker_reading(&left),
        Some(Err(Leave::Stands(spotlight_scope::LEFT.to_owned())))
    );
    assert!(crate::init_spotlight::automatic_scope_owed(&left));
    // A day on, `sync.spotlight` asks the broker again, and the password stays owed until the
    // broker's exclusion removes the record.
    left.advance(spotlight_scope::LEFT_KEPT.as_secs());
    assert_eq!(crate::init_spotlight::broker_reading(&left), Some(Ok(owed)));
    assert!(crate::init_spotlight::automatic_scope_owed(&left));
}
