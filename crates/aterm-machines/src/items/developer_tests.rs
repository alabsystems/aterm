// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::engine::{self, NoJournal};
use crate::model::{HostFacts, Mode, Outcome};
use crate::port::{Fake, Ran, Stat};

fn ctx() -> Ctx {
    Ctx {
        facts: HostFacts::default(),
        users: Vec::new(),
        console: None,
        host_name: "lab-07".into(),
        cache_dir: "/c".into(),
        disrupt: true,
    }
}

fn retained_system() -> Fake {
    let sys = Fake::root();
    sys.born(std::process::id(), 123)
        .running(&[])
        .on("/usr/bin/xcode-select -p", none_selected())
        .on("/usr/bin/xcode-select -p", none_selected())
        .on("/usr/bin/xcode-select -p", Ran::ok(&format!("{CLT_DIR}\n")))
        .on(RETAINED_INSTALL, installed_quietly())
        .on_stat(RETAINED_INSTALL, Path::new(CLT_GIT), git())
        .on_stat(RETAINED_INSTALL, Path::new(CLT_CLANG), git())
        .on(PKGUTIL, Ran::ok(RECEIPT))
        .on(
            "/usr/bin/xcode-select --switch /Library/Developer/CommandLineTools",
            Ran::ok(""),
        );
    sys
}

#[test]
fn a_remaining_git_does_not_hide_a_missing_compiler_from_repair() {
    for compiler in [
        None,
        Some(Stat { dir: true, ..git() }),
        Some(Stat {
            symlink: true,
            ..git()
        }),
        Some(Stat {
            mode: 0o644,
            ..git()
        }),
        Some(Stat { len: 0, ..git() }),
    ] {
        let sys = retained_system();
        sys.stat_of(CLT_GIT, git())
            .on(SOFTWAREUPDATE_LIST, Ran::ok(VM_LIST));
        if let Some(compiler) = compiler {
            sys.stat_of(CLT_CLANG, compiler);
        }
        let rows = engine::run(&retained_items(), Mode::Apply, &sys, &ctx(), &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
        assert!(sys.calls().iter().any(|call| call == RETAINED_INSTALL));
        assert!(installed(&sys).unwrap());
    }
}

const RETAINED_INSTALL: &str =
    "/usr/sbin/softwareupdate -i 'Command Line Tools for Xcode 27.0-27.0'";

fn retained_items() -> Vec<Box<dyn Item>> {
    expand(
        &Table {
            command_line_tools: Some(true),
            retained_root_run: true,
            ..Table::default()
        },
        &ctx(),
    )
    .unwrap()
}

fn slept_ms(sys: &Fake) -> u64 {
    sys.calls()
        .iter()
        .filter_map(|call| {
            call.strip_prefix("sleep ")?
                .strip_suffix("ms")?
                .parse::<u64>()
                .ok()
        })
        .sum()
}

#[test]
fn retained_install_recovers_after_a_prolonged_outage_in_one_custody() {
    let sys = retained_system();
    for _ in 0..7 {
        sys.on(
            SOFTWAREUPDATE_LIST,
            Ran::exit(1, "catalog network unavailable"),
        );
    }
    sys.on(SOFTWAREUPDATE_LIST, Ran::ok(VM_LIST));
    sys.on_sleep(|sys, _| {
        assert_eq!(stamped(sys), Some(stamp(sys)));
        assert_eq!(holder(sys, Some(true)), Holder::Install);
    });
    let items = retained_items();
    let rows = engine::run(&items, Mode::Apply, &sys, &ctx(), &mut NoJournal);
    assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
    assert!(slept_ms(&sys) > 10 * 60 * 1000);
    let calls = sys.calls();
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.as_str() == RETAINED_INSTALL)
            .count(),
        1
    );
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.starts_with("create "))
            .count(),
        1
    );
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.starts_with("remove "))
            .count(),
        1
    );
    assert!(
        !calls
            .iter()
            .any(|call| call.contains("sudo") || call.contains("xcode-select --install"))
    );
    assert!(sys.written(PLACEHOLDER).is_none());
    assert_eq!(items[0].unfinished(), None);
}

#[test]
fn a_network_transition_wakes_the_retained_retry_before_its_timer() {
    let sys = retained_system();
    sys.install_path(crate::port::InstallNetworkReading {
        revision: 1,
        available: false,
        constrained: false,
    });
    sys.on(
        SOFTWAREUPDATE_LIST,
        Ran::exit(1, "catalog network unavailable"),
    )
    .on(SOFTWAREUPDATE_LIST, Ran::ok(VM_LIST));
    sys.on_sleep(|sys, _| {
        sys.install_path(crate::port::InstallNetworkReading {
            revision: 2,
            available: true,
            constrained: false,
        });
    });
    recovery::install(&sys).unwrap();
    assert_eq!(slept_ms(&sys), 2000);
    assert!(installed(&sys).unwrap());
}

#[test]
fn low_data_mode_clears_in_the_same_authorized_run() {
    let mut sys = retained_system();
    sys.low_data = Some(true);
    sys.install_path(crate::port::InstallNetworkReading {
        revision: 1,
        available: true,
        constrained: true,
    });
    sys.on(SOFTWAREUPDATE_LIST, Ran::ok(VM_LIST));
    sys.on_sleep(|sys, _| {
        assert!(
            !sys.calls()
                .iter()
                .any(|call| call.starts_with(SOFTWAREUPDATE))
        );
        sys.install_path(crate::port::InstallNetworkReading {
            revision: 2,
            available: true,
            constrained: false,
        });
    });
    let items = retained_items();
    let rows = engine::run(&items, Mode::Apply, &sys, &ctx(), &mut NoJournal);
    assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
    assert_eq!(slept_ms(&sys), 2000);
}

#[test]
fn stop_during_network_wait_releases_only_the_original_custody() {
    let sys = retained_system();
    sys.on(
        SOFTWAREUPDATE_LIST,
        Ran::exit(1, "catalog network unavailable"),
    );
    sys.on_sleep(|sys, _| {
        sys.stop_now();
    });
    assert_eq!(
        recovery::install(&sys).unwrap_err().class,
        Unfinished::Stopped
    );
    assert_eq!(slept_ms(&sys), 2000);
    assert!(sys.written(PLACEHOLDER).is_none());
    assert!(
        !sys.calls()
            .iter()
            .any(|call| call.as_str() == RETAINED_INSTALL)
    );
}

#[test]
fn another_generation_keeps_its_placeholder_and_is_not_retried() {
    let sys = retained_system();
    sys.on(
        SOFTWAREUPDATE_LIST,
        Ran::exit(1, "catalog network unavailable"),
    );
    let other_pid = std::process::id().saturating_add(1);
    sys.on_sleep(move |sys, _| {
        sys.born(other_pid, 456).file(
            PLACEHOLDER,
            aterm_types::macos_setup::placeholder_stamp(other_pid, 456).as_bytes(),
        );
    });
    assert_eq!(recovery::install(&sys).unwrap_err().class, Unfinished::Busy);
    assert_eq!(slept_ms(&sys), 2000);
    assert_eq!(
        sys.written(PLACEHOLDER).unwrap(),
        aterm_types::macos_setup::placeholder_stamp(other_pid, 456).into_bytes()
    );
    assert!(
        !sys.calls()
            .iter()
            .any(|call| call.as_str() == RETAINED_INSTALL)
    );
}

#[test]
fn unread_process_custody_is_not_treated_as_a_network_outage() {
    let sys = Fake::root();
    assert_eq!(
        recovery::install(&sys).unwrap_err().class,
        Unfinished::Unread
    );
    assert_eq!(slept_ms(&sys), 0);
    assert!(
        !sys.calls()
            .iter()
            .any(|call| call.starts_with(SOFTWAREUPDATE))
    );
}

#[test]
fn a_baseline_file_cannot_claim_the_retained_root_context() {
    assert!(
        crate::baseline::parse(
            "schema = 1\n[developer]\ncommand_line_tools = true\nretained_root_run = true\n"
        )
        .is_err()
    );
}

#[test]
fn retained_recovery_waits_for_an_external_window_and_respects_its_cancellation() {
    let sys = retained_system();
    sys.on(
        SOFTWAREUPDATE_LIST,
        Ran::exit(1, "catalog network unavailable"),
    );
    let sleeps = std::sync::atomic::AtomicUsize::new(0);
    sys.on_sleep(move |sys, _| {
        if sleeps.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 0 {
            installers(sys, Some("apple"));
        } else {
            sys.remove(Path::new(PLACEHOLDER)).unwrap();
            installers(sys, None);
        }
    });
    assert_eq!(
        recovery::install(&sys).unwrap_err().class,
        Unfinished::WindowClosed
    );
    assert!(
        !sys.calls().iter().any(
            |call| call.as_str() == RETAINED_INSTALL || call.contains("xcode-select --install")
        )
    );
}

#[test]
fn an_installer_refusal_is_not_retried_as_a_network_outage() {
    let sys = Fake::root();
    sys.born(std::process::id(), 123)
        .running(&[])
        .on(SOFTWAREUPDATE_LIST, Ran::ok(VM_LIST))
        .on(
            RETAINED_INSTALL,
            Ran::exit(
                1,
                "Downloaded Command Line Tools\nInstalling Command Line Tools\nInstaller refused",
            ),
        );
    assert_eq!(
        recovery::install(&sys).unwrap_err().class,
        Unfinished::Installer
    );
    assert_eq!(slept_ms(&sys), 0);
    assert_eq!(
        sys.calls()
            .iter()
            .filter(|call| call.as_str() == RETAINED_INSTALL)
            .count(),
        1
    );
}

#[test]
fn retained_recovery_cannot_authorize_a_nonroot_process() {
    let mut sys = Fake::root();
    sys.euid = 501;
    assert_eq!(
        recovery::install(&sys).unwrap_err().class,
        Unfinished::Other
    );
    assert!(sys.calls().is_empty());
}

#[test]
fn an_existing_installer_finishes_before_the_authorized_run_claims_the_tools() {
    let sys = retained_system();
    sys.file(PLACEHOLDER, b"").running(&[SOFTWAREUPDATE]);
    sys.on(SOFTWAREUPDATE_LIST, Ran::ok(VM_LIST));
    sys.on_sleep(|sys, _| {
        assert_eq!(sys.written(PLACEHOLDER), Some(Vec::new()));
        assert!(
            !sys.calls()
                .iter()
                .any(|call| call.starts_with(SOFTWAREUPDATE))
        );
        sys.running(&[]);
    });
    recovery::install(&sys).unwrap();
    assert!(installed(&sys).unwrap());
    assert_eq!(slept_ms(&sys), 2000);
    assert_eq!(
        sys.calls()
            .iter()
            .filter(|call| call.as_str() == RETAINED_INSTALL)
            .count(),
        1
    );
}

#[test]
fn another_command_line_installer_holds_the_same_authorized_run() {
    let sys = retained_system();
    sys.on(
        SOFTWAREUPDATE_LIST,
        Ran::exit(1, "catalog network unavailable"),
    )
    .on(SOFTWAREUPDATE_LIST, Ran::ok(VM_LIST));
    let ticks = std::sync::atomic::AtomicUsize::new(0);
    sys.on_sleep(move |sys, _| {
        let tick = ticks.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(stamped(sys), Some(stamp(sys)));
        assert!(
            !sys.calls()
                .iter()
                .any(|call| call.as_str() == RETAINED_INSTALL)
        );
        if tick == 0 {
            sys.running(&[SOFTWAREUPDATE]);
        }
        if tick == 3 {
            sys.running(&[]);
        }
    });
    recovery::install(&sys).unwrap();
    assert_eq!(slept_ms(&sys), 7000);
    assert!(installed(&sys).unwrap());
    assert_eq!(
        sys.calls()
            .iter()
            .filter(|call| call.as_str() == RETAINED_INSTALL)
            .count(),
        1
    );
}

#[test]
fn stop_leaves_the_placeholder_needed_by_an_external_installer() {
    let sys = retained_system();
    sys.on(
        SOFTWAREUPDATE_LIST,
        Ran::exit(1, "catalog network unavailable"),
    );
    sys.on_sleep(|sys, _| {
        sys.running(&[SOFTWAREUPDATE]).stop_now();
    });
    assert_eq!(
        recovery::install(&sys).unwrap_err().class,
        Unfinished::Stopped
    );
    assert_eq!(sys.written(PLACEHOLDER), Some(stamp(&sys)));
    assert!(
        !sys.calls()
            .iter()
            .any(|call| call.as_str() == RETAINED_INSTALL)
    );
}

const VM_LIST: &str = "Software Update Tool\n\
\n\
Finding available software\n\
Software Update found the following new or updated software:\n\
* Label: Command Line Tools for Xcode 27.0-27.0\n\
\tTitle: Command Line Tools for Xcode 27.0, Version: 27.0, Size: 519460KiB, Recommended: YES, \n\
* Label: macOS 27.0.1-26A434\n\
\tTitle: macOS 27.0.1, Version: 27.0.1, Size: 3235867KiB, Recommended: YES, Action: restart, \n\
";

const OLD_LIST: &str = "Software Update Tool\n\
\n\
Finding available software\n\
Software Update found the following new or updated software:\n\
* Label: Command Line Tools for Xcode-15.3\n\
\tTitle: Command Line Tools for Xcode, Version: 15.3, Size: 707397KiB, Recommended: YES, \n\
* Label: Command Line Tools for Xcode-14.3\n\
\tTitle: Command Line Tools for Xcode, Version: 14.3, Size: 711657KiB, Recommended: YES, \n\
* Label: Command Line Tools beta 2 for Xcode-16.0\n\
\tTitle: Command Line Tools beta 2 for Xcode, Version: 16.0, Size: 721000KiB, Recommended: YES, \n\
";

fn none_selected() -> Ran {
    Ran::exit(
        2,
        "xcode-select: error: Unable to get active developer directory. Use `sudo xcode-select --switch path/to/Xcode.app` to set one (or see `man xcode-select`)",
    )
}

fn git() -> Stat {
    Stat {
        uid: 0,
        gid: 0,
        mode: 0o755,
        dir: false,
        symlink: false,
        len: 3_600_000,
    }
}

const RECEIPT: &str = "package-id: com.apple.pkg.CLTools_Executables\nversion: 27.0.0.0.1788430756\nvolume: /\nlocation: /\ninstall-time: 1792019691\n";

fn no_receipt() -> Ran {
    Ran::exit(
        1,
        "No receipt for 'com.apple.pkg.CLTools_Executables' found at '/'.",
    )
}

const PKGUTIL: &str = "/usr/sbin/pkgutil --pkg-info=com.apple.pkg.CLTools_Executables";

fn no_space() -> Ran {
    Ran {
        status: Some(0),
        stdout: b"Software Update Tool\n\nFinding available software\n".to_vec(),
        stderr: b"Not enough free disk space: a total of 9.52 GB is required.\n".to_vec(),
    }
}

fn installed_quietly() -> Ran {
    Ran::ok("Software Update Tool\n\nFinding available software\n")
}

const SOFTWAREUPDATE_LIST: &str = "/usr/sbin/softwareupdate -l";

#[test]
fn a_catalog_failure_recovers_in_the_same_authorized_run() {
    let sys = Fake::root();
    sys.running(&[]);
    sys.on(
        SOFTWAREUPDATE_LIST,
        Ran {
            status: Some(1),
            ..Ran::ok("")
        },
    )
    .on(SOFTWAREUPDATE_LIST, Ran::ok(VM_LIST))
    .on(
        "/usr/sbin/softwareupdate -i 'Command Line Tools for Xcode 27.0-27.0'",
        installed_quietly(),
    )
    .on_stat(RETAINED_INSTALL, Path::new(CLT_GIT), git())
    .on_stat(RETAINED_INSTALL, Path::new(CLT_CLANG), git())
    .on(PKGUTIL, Ran::ok(RECEIPT));
    CommandLineTools::install(&sys).unwrap();
    let calls = sys.calls();
    assert_eq!(
        calls
            .iter()
            .filter(|c| c.as_str() == SOFTWAREUPDATE_LIST)
            .count(),
        2
    );
    assert!(calls.iter().any(|c| c == "sleep 5000ms"));
    assert!(
        !calls
            .iter()
            .any(|c| c.contains("sudo") || c.contains("xcode-select --install"))
    );
    assert!(sys.written(PLACEHOLDER).is_none());
}

#[test]
fn a_persistent_catalog_failure_is_bounded_and_a_stop_prevents_recovery() {
    for stop_after in [None, Some(0), Some(1)] {
        let mut sys = Fake::root();
        sys.running(&[]);
        sys.stop_after = stop_after;
        sys.on(
            SOFTWAREUPDATE_LIST,
            Ran {
                status: Some(1),
                ..Ran::ok("")
            },
        );
        assert_eq!(
            CommandLineTools::install(&sys).unwrap_err().class,
            Unfinished::Catalog
        );
        let calls = sys.calls();
        assert_eq!(
            calls
                .iter()
                .filter(|c| c.as_str() == SOFTWAREUPDATE_LIST)
                .count(),
            if stop_after.is_some() { 1 } else { 3 }
        );
        assert_eq!(
            calls.iter().filter(|c| c.starts_with("sleep ")).count(),
            match stop_after {
                Some(0) => 0,
                Some(_) => 1,
                None => 2,
            }
        );
        assert!(sys.written(PLACEHOLDER).is_none());
    }
}

#[test]
fn recovery_rechecks_low_data_and_tools_installed_during_the_wait() {
    for low_data in [false, true] {
        let mut sys = Fake::root();
        sys.running(&[]);
        sys.low_data = Some(low_data);
        sys.on(
            SOFTWAREUPDATE_LIST,
            Ran {
                status: Some(1),
                ..Ran::ok("")
            },
        );
        if !low_data {
            sys.on_sleep(|sys, _| {
                sys.stat_of(CLT_GIT, git()).stat_of(CLT_CLANG, git());
            });
        }
        let result = CommandLineTools::install(&sys);
        if low_data {
            assert_eq!(result.unwrap_err().class, Unfinished::LowData);
        } else {
            result.unwrap();
        }
        assert_eq!(
            sys.calls()
                .iter()
                .filter(|c| c.as_str() == SOFTWAREUPDATE_LIST)
                .count(),
            1
        );
        assert!(sys.written(PLACEHOLDER).is_none());
    }
}

#[test]
fn a_download_failure_recovers_but_an_installer_failure_is_not_retried() {
    const INSTALL: &str = "/usr/sbin/softwareupdate -i 'Command Line Tools for Xcode 27.0-27.0'";
    for reached_installer in [false, true] {
        let sys = Fake::root();
        sys.running(&[]);
        sys.on(SOFTWAREUPDATE_LIST, Ran::ok(VM_LIST))
            .on(
                INSTALL,
                Ran {
                    status: Some(1),
                    ..Ran::ok(if reached_installer {
                        "Installing Command Line Tools\n"
                    } else {
                        "Download failed\n"
                    })
                },
            )
            .on(INSTALL, installed_quietly())
            .on(PKGUTIL, Ran::ok(RECEIPT));
        sys.on_sleep(|sys, _| {
            sys.on_stat(INSTALL, Path::new(CLT_GIT), git()).on_stat(
                INSTALL,
                Path::new(CLT_CLANG),
                git(),
            );
        });
        let result = CommandLineTools::install(&sys);
        if reached_installer {
            assert_eq!(result.unwrap_err().class, Unfinished::Installer);
        } else {
            result.unwrap();
        }
        let calls = sys.calls();
        assert_eq!(
            calls.iter().filter(|c| c.as_str() == INSTALL).count(),
            if reached_installer { 1 } else { 2 }
        );
        assert_eq!(
            calls.iter().filter(|c| c.starts_with("sleep ")).count(),
            usize::from(!reached_installer)
        );
        assert!(sys.written(PLACEHOLDER).is_none());
    }
}

#[test]
fn the_newest_label_is_picked_in_the_new_space_form_and_the_old_hyphen_form() {
    let labels = parse_labels(VM_LIST);
    assert_eq!(
        labels,
        vec![Label {
            label: "Command Line Tools for Xcode 27.0-27.0".into(),
            version: vec![27, 0],
            beta: false,
        }]
    );
    let old = parse_labels(OLD_LIST);
    assert_eq!(old.len(), 3);
    assert_eq!(
        pick(&old).unwrap().label,
        "Command Line Tools for Xcode-15.3"
    );
    let betas: Vec<Label> = old.into_iter().filter(|l| l.beta).collect();
    assert_eq!(pick(&betas).unwrap().version, vec![16, 0]);
    assert_eq!(
        version_after_xcode("Command Line Tools for Xcode 26.4-26.4.1"),
        vec![26, 4]
    );
    let without = VM_LIST.replace(
        "* Label: Command Line Tools for Xcode 27.0-27.0\n\tTitle: Command Line Tools for Xcode 27.0, Version: 27.0, Size: 519460KiB, Recommended: YES, \n",
        "",
    );
    assert_ne!(without, VM_LIST);
    assert!(pick(&parse_labels(&without)).is_none());
}

#[test]
fn a_title_version_outranks_the_labels_own_digits() {
    let text = "* Label: Command Line Tools for Xcode 26.4-26.4.1\n\tTitle: Command Line Tools for Xcode 26.4, Version: 26.4.1, Size: 1KiB, \n* Label: Command Line Tools for Xcode 26.4-26.4\n\tTitle: Command Line Tools for Xcode 26.4, Version: 26.4, Size: 1KiB, \n";
    assert_eq!(
        pick(&parse_labels(text)).unwrap().label,
        "Command Line Tools for Xcode 26.4-26.4.1"
    );
}

/// Developer mode, a moment's step, comes before the tools' minutes-long install, so the
/// elevation applies it at once whatever becomes of the download.
#[test]
fn developer_mode_is_applied_before_the_tools_install() {
    let items = expand(
        &Table {
            command_line_tools: Some(true),
            debugger: Some(true),
            ..Table::default()
        },
        &ctx(),
    )
    .unwrap();
    let ids: Vec<String> = items.iter().map(|item| item.id()).collect();
    assert_eq!(ids, ["developer.debugger", crate::items::github::CLT_ITEM]);
}

#[test]
fn installed_and_selected_tools_are_ok_with_their_receipt_version() {
    let sys = Fake::root();
    sys.stat_of(CLT_GIT, git())
        .stat_of(CLT_CLANG, git())
        .on(
            "/usr/bin/xcode-select -p",
            Ran::ok("/Library/Developer/CommandLineTools\n"),
        )
        .on(PKGUTIL, Ran::ok(RECEIPT));
    let items = expand(
        &Table {
            command_line_tools: Some(true),
            debugger: None,
            ..Table::default()
        },
        &ctx(),
    )
    .unwrap();
    assert_eq!(
        items[0].probe(&sys, &ctx()),
        Probe::Ok {
            now: "installed 27.0.0.0.1788430756; selected /Library/Developer/CommandLineTools"
                .into()
        }
    );
    assert!(sys.calls().iter().all(|c| !c.contains("softwareupdate")));
}

#[test]
fn a_fresh_mac_installs_the_listed_label_then_selects_it_and_the_placeholder_goes() {
    let sys = Fake::root();
    sys.running(&[]);
    sys.on("/usr/bin/xcode-select -p", none_selected())
        .on("/usr/bin/xcode-select -p", none_selected())
        .on(
            "/usr/bin/xcode-select -p",
            Ran::ok("/Library/Developer/CommandLineTools\n"),
        )
        .on("/usr/sbin/softwareupdate -l", Ran::ok(VM_LIST))
        .on(
            "/usr/sbin/softwareupdate -i 'Command Line Tools for Xcode 27.0-27.0'",
            installed_quietly(),
        )
        .on_stat(RETAINED_INSTALL, Path::new(CLT_GIT), git())
        .on_stat(RETAINED_INSTALL, Path::new(CLT_CLANG), git())
        .on(PKGUTIL, Ran::ok(RECEIPT))
        .on(
            "/usr/bin/xcode-select --switch /Library/Developer/CommandLineTools",
            Ran::ok(""),
        );
    let items = expand(
        &Table {
            command_line_tools: Some(true),
            debugger: None,
            ..Table::default()
        },
        &ctx(),
    )
    .unwrap();
    let rows = {
        let probe = items[0].probe(&sys, &ctx());
        assert_eq!(
            probe,
            Probe::Change {
                from: "not installed".into(),
                to: "installed; selected /Library/Developer/CommandLineTools".into()
            }
        );
        items[0].apply(&sys, &ctx()).unwrap();
        sys.stat_of(CLT_GIT, git()).stat_of(CLT_CLANG, git());
        items[0].probe(&sys, &ctx())
    };
    assert!(matches!(rows, Probe::Ok { .. }), "{rows:?}");
    let calls = sys.calls();
    let at = |needle: &str| calls.iter().position(|c| c.starts_with(needle)).unwrap();
    assert!(at("create /tmp/.com.apple.dt.CommandLineTools") < at("/usr/sbin/softwareupdate -l"));
    assert!(at("/usr/sbin/softwareupdate -i") < at("remove /tmp/.com.apple.dt.CommandLineTools"));
    assert!(at("remove /tmp/.com.apple.dt") < at("/usr/bin/xcode-select --switch"));
    assert_eq!(
        items[0].revert().as_deref(),
        Some(
            "/bin/rm -rf /Library/Developer/CommandLineTools; /usr/sbin/pkgutil --forget=com.apple.pkg.CLTools_Executables"
        )
    );
}

/// A closed tab or a Ctrl-C while `softwareupdate -l` reads the catalog installs nothing and
/// leaves no placeholder to refuse the next install: the stops are caught before the
/// placeholder exists.
#[test]
fn a_stop_while_the_catalog_is_read_installs_nothing_and_removes_the_placeholder() {
    let mut sys = Fake::root();
    sys.running(&[]);
    sys.stop_after = Some(0);
    sys.on("/usr/sbin/softwareupdate -l", Ran::ok(VM_LIST));
    let failure = CommandLineTools::install(&sys).unwrap_err();
    assert!(failure.why.starts_with("stopped by signal"), "{failure:?}");
    assert_eq!(failure.class, Unfinished::Stopped);
    let calls = sys.calls();
    let at = |needle: &str| calls.iter().position(|c| c.starts_with(needle));
    let caught = at("defer stops").expect("stops caught");
    let created = at("create /tmp/.com.apple.dt.CommandLineTools").expect("placeholder");
    assert!(
        caught < created,
        "caught before the placeholder exists: {calls:?}"
    );
    assert!(at("/usr/sbin/softwareupdate -i").is_none(), "{calls:?}");
    assert_stop_sent_after_the_placeholder_goes(&calls);
    assert!(sys.written(PLACEHOLDER).is_none());

    let unstopped = Fake::root();
    unstopped
        .running(&[])
        .on("/usr/sbin/softwareupdate -l", Ran::ok(VM_LIST))
        .on(
            "/usr/sbin/softwareupdate -i 'Command Line Tools for Xcode 27.0-27.0'",
            installed_quietly(),
        )
        .on_stat(RETAINED_INSTALL, Path::new(CLT_GIT), git())
        .on_stat(RETAINED_INSTALL, Path::new(CLT_CLANG), git())
        .on(PKGUTIL, Ran::ok(RECEIPT));
    CommandLineTools::install(&unstopped).unwrap();
    let calls = unstopped.calls();
    assert!(
        calls
            .iter()
            .any(|c| c.starts_with("/usr/sbin/softwareupdate -i")),
        "the negative control installs"
    );
    let at = |needle: &str| calls.iter().position(|c| c.starts_with(needle));
    let removed = at("remove /tmp/.com.apple.dt.CommandLineTools").expect("placeholder removed");
    let restored = at("stops restored").expect("stops restored");
    assert!(
        removed < restored,
        "the stops stay caught until the placeholder is gone: {calls:?}"
    );
    assert!(at("stop sent again").is_none(), "{calls:?}");
}

/// In Low Data Mode the probe plans nothing and reads nothing from Apple: the half-gigabyte
/// download waits, saying why; with Low Data Mode off (the negative control) it plans the
/// install.
#[test]
fn low_data_mode_holds_the_download_back_and_says_how_to_go_on() {
    let items = expand(
        &Table {
            command_line_tools: Some(true),
            debugger: None,
            ..Table::default()
        },
        &ctx(),
    )
    .unwrap();
    let mut sys = Fake::root();
    sys.low_data = Some(true);
    sys.on("/usr/bin/xcode-select -p", none_selected());
    assert_eq!(
        items[0].probe(&sys, &ctx()),
        Probe::Blocked {
            why: LOW_DATA.into()
        }
    );
    assert_eq!(items[0].unfinished(), Some(Unfinished::LowData));
    assert!(
        sys.calls()
            .iter()
            .all(|c| !c.contains("curl") && !c.contains("softwareupdate")),
        "{:?}",
        sys.calls()
    );
    for low_data in [Some(false), None] {
        let mut sys = Fake::root();
        sys.low_data = low_data;
        sys.on("/usr/bin/xcode-select -p", none_selected());
        assert!(
            matches!(items[0].probe(&sys, &ctx()), Probe::Change { .. }),
            "{low_data:?}"
        );
        assert_eq!(items[0].unfinished(), None, "{low_data:?}");
    }
}

#[test]
fn an_unlisted_catalog_recovers_without_launching_apples_installer() {
    let sys = Fake::root();
    sys.running(&[]);
    sys.on(SOFTWAREUPDATE_LIST, Ran::ok("No new software available.\n"))
        .on(SOFTWAREUPDATE_LIST, Ran::ok("No new software available.\n"))
        .on(SOFTWAREUPDATE_LIST, Ran::ok(VM_LIST))
        .on(
            "/usr/sbin/softwareupdate -i 'Command Line Tools for Xcode 27.0-27.0'",
            installed_quietly(),
        )
        .on_stat(RETAINED_INSTALL, Path::new(CLT_GIT), git())
        .on_stat(RETAINED_INSTALL, Path::new(CLT_CLANG), git())
        .on(PKGUTIL, Ran::ok(RECEIPT));
    CommandLineTools::install(&sys).unwrap();
    let calls = sys.calls();
    assert_eq!(
        calls
            .iter()
            .filter(|c| c.as_str() == SOFTWAREUPDATE_LIST)
            .count(),
        3
    );
    assert_eq!(
        calls
            .iter()
            .filter(|c| c.starts_with("/usr/sbin/softwareupdate -i"))
            .count(),
        1
    );
    assert!(calls.iter().any(|c| c == "sleep 5000ms"));
    assert!(calls.iter().any(|c| c == "sleep 15000ms"));
    assert!(
        !calls
            .iter()
            .any(|c| c.contains("xcode-select --install") || c.contains("sudo"))
    );
    assert!(sys.written(PLACEHOLDER).is_none());
}

#[test]
fn an_unlisted_catalog_reports_unavailability_without_a_gui_fallback() {
    for stop_after in [None, Some(0)] {
        let mut sys = Fake::root();
        sys.running(&[]);
        sys.stop_after = stop_after;
        sys.on(SOFTWAREUPDATE_LIST, Ran::ok("No new software available.\n"));
        let failure = CommandLineTools::install(&sys).unwrap_err();
        assert_eq!(
            failure.class,
            if stop_after.is_some() {
                Unfinished::Stopped
            } else {
                Unfinished::NoneOffered
            }
        );
        let calls = sys.calls();
        assert!(!calls.iter().any(|c| c.contains("xcode-select --install")
            || c.starts_with("/usr/sbin/softwareupdate -i")));
        assert!(sys.written(PLACEHOLDER).is_none());
        if stop_after.is_none() {
            assert_eq!(
                calls
                    .iter()
                    .filter(|c| c.as_str() == SOFTWAREUPDATE_LIST)
                    .count(),
                3
            );
        }
    }
}

#[test]
fn tools_arriving_during_an_unlisted_catalog_retry_end_the_wait() {
    let sys = Fake::root();
    sys.running(&[]);
    sys.on(SOFTWAREUPDATE_LIST, Ran::ok("No new software available.\n"));
    sys.on_sleep(|sys, _| {
        sys.stat_of(CLT_GIT, git()).stat_of(CLT_CLANG, git());
    });
    CommandLineTools::install(&sys).unwrap();
    let calls = sys.calls();
    assert_eq!(
        calls
            .iter()
            .filter(|c| c.as_str() == SOFTWAREUPDATE_LIST)
            .count(),
        1
    );
    assert!(
        !calls.iter().any(|c| c.contains("xcode-select --install")
            || c.starts_with("/usr/sbin/softwareupdate -i"))
    );
    assert!(sys.written(PLACEHOLDER).is_none());
}

/// Apple's window open with no placeholder beside it as the run begins (it opened beside
/// another install's, which has ended since) holds the tools all the same: the run makes no
/// placeholder, asks `softwareupdate` for nothing and `xcode-select --install` for no other
/// window, and waits on that one until the tools are there. The window is found among the
/// processes the kernel names, so one process whose executable it names none for (its file
/// deleted) hides no window. (Reading the window through [`System::process_paths`], which is
/// `None` once any executable is unread, misses it and fails this test.)
/// NEGATIVE CONTROL: with no window either, the run makes its placeholder, lists the tools
/// and installs them.
#[test]
fn apples_window_with_no_placeholder_is_waited_on_and_nothing_is_made_beside_it() {
    for unnamed_beside in [false, true] {
        let sys = Fake::root();
        sys.running(&["/sbin/launchd", APPLE_WINDOW])
            .on("/usr/sbin/softwareupdate -l", Ran::ok(VM_LIST));
        if unnamed_beside {
            sys.process(unnamed(), None);
        }
        assert_eq!(
            holder(&sys, Some(false)),
            Holder::AppleWindow,
            "{unnamed_beside}"
        );
        sys.on_sleep(|sys, _| {
            if sys
                .calls()
                .iter()
                .filter(|c| c.starts_with("sleep "))
                .count()
                == 2
            {
                sys.stat_of(CLT_GIT, git()).stat_of(CLT_CLANG, git());
            }
        });
        CommandLineTools::install(&sys).unwrap();
        let calls = sys.calls();
        assert!(
            !calls.iter().any(|c| c.starts_with(SOFTWAREUPDATE)
                || c.starts_with("create ")
                || c.starts_with("remove ")
                || c.contains("xcode-select --install")),
            "{unnamed_beside}: {calls:?}"
        );
        assert_eq!(
            calls.iter().filter(|c| c.starts_with("sleep ")).count(),
            2,
            "{unnamed_beside}: it waited on the window: {calls:?}"
        );
        assert!(sys.written(PLACEHOLDER).is_none(), "{unnamed_beside}");
    }

    let control = Fake::root();
    control
        .running(&["/sbin/launchd"])
        .on("/usr/sbin/softwareupdate -l", Ran::ok(VM_LIST))
        .on(
            "/usr/sbin/softwareupdate -i 'Command Line Tools for Xcode 27.0-27.0'",
            installed_quietly(),
        )
        .on_stat(RETAINED_INSTALL, Path::new(CLT_GIT), git())
        .on_stat(RETAINED_INSTALL, Path::new(CLT_CLANG), git())
        .on(PKGUTIL, Ran::ok(RECEIPT));
    assert_eq!(holder(&control, Some(false)), Holder::Nothing);
    CommandLineTools::install(&control).unwrap();
    let calls = control.calls();
    let at = |needle: &str| calls.iter().position(|c| c.starts_with(needle));
    let created = at(&format!("create {PLACEHOLDER}")).expect("this install's own");
    let listed = at("/usr/sbin/softwareupdate -l").expect("the install goes on");
    let installed = at("/usr/sbin/softwareupdate -i ").expect("and installs");
    assert!(created < listed && listed < installed, "{calls:?}");
}

/// Apple's window raised while the run read the catalog (a `git` stub's, beside this run's
/// placeholder, so it made none of its own) is the person's install: whether the catalog
/// lists the tools or none, the run downloads nothing beside it and asks `xcode-select
/// --install` for no other window, and waits on that one until the tools are there.
#[test]
fn a_window_raised_while_the_catalog_is_read_is_waited_on_and_nothing_installs_beside_it() {
    let unlisted =
        "Software Update Tool\n\nFinding available software\nNo new software available.\n";
    for catalog in [VM_LIST, unlisted] {
        let sys = Fake::root();
        sys.running(&["/sbin/launchd"])
            .on("/usr/sbin/softwareupdate -l", Ran::ok(catalog))
            .on_running(
                "/usr/sbin/softwareupdate -l",
                &["/sbin/launchd", APPLE_WINDOW],
            );
        sys.on_sleep(|sys, _| {
            sys.stat_of(CLT_GIT, git()).stat_of(CLT_CLANG, git());
        });
        CommandLineTools::install(&sys).unwrap();
        let calls = sys.calls();
        assert!(
            !calls
                .iter()
                .any(|c| c.starts_with("/usr/sbin/softwareupdate -i")
                    || c.contains("xcode-select --install")),
            "{calls:?}"
        );
        assert!(
            calls.iter().any(|c| c.starts_with("sleep ")),
            "it waited on the window: {calls:?}"
        );
    }
}

/// The run's placeholder guard leaves its placeholder in place while Apple's window is listed
/// as the run ends, when another program raises a window while the catalog is read,
/// and however the wait ends (its patience
/// spent, or a stop): that window made none of its own, its install needs the file to find
/// the tools in Apple's catalog, and it unlinks the file when its request ends. Every reader
/// then holds the tools for the window, so the next run waits on it, making, removing and
/// asking `softwareupdate` for nothing.
/// NEGATIVE CONTROL: the window gone before the run ends (quit, or killed outright: either
/// leaves this run's placeholder, which the window did not make) ends the wait as closed and
/// takes the placeholder with it.
#[test]
fn the_guard_keeps_its_placeholder_while_apples_window_is_listed_as_the_run_ends() {
    for (route, stop_after, class) in [
        ("raised", None, Unfinished::AppleWindow),
        ("stopped", Some(2), Unfinished::Stopped),
    ] {
        let mut sys = Fake::root();
        sys.stop_after = stop_after;
        window_comes_up(&sys);
        let failure = CommandLineTools::install(&sys).unwrap_err();
        assert_eq!(failure.class, class, "{route}: {failure:?}");
        let calls = sys.calls();
        assert!(
            !calls.iter().any(|c| c.starts_with("remove ")),
            "{route}: {calls:?}"
        );
        assert_eq!(
            sys.written(PLACEHOLDER),
            Some(stamp(&sys)),
            "{route}: kept for the window still open"
        );
        assert_eq!(holder(&sys, Some(true)), Holder::AppleWindow, "{route}");

        sys.stop_after = None;
        let next = CommandLineTools::install(&sys).unwrap_err();
        assert_eq!(next.class, Unfinished::AppleWindow, "{route}: {next:?}");
        let again = &sys.calls()[calls.len()..];
        assert!(
            !again.iter().any(|c| c.starts_with(SOFTWAREUPDATE)
                || c.starts_with("create ")
                || c.starts_with("remove ")
                || c.contains("xcode-select --install")),
            "{route}: {again:?}"
        );
    }

    {
        let route = "raised";
        let gone = Fake::root();
        window_comes_up(&gone);
        gone.on_sleep(|sys, _| {
            sys.running(&["/sbin/launchd"]);
        });
        let failure = CommandLineTools::install(&gone).unwrap_err();
        assert_eq!(
            failure.class,
            Unfinished::WindowClosed,
            "{route}: {failure:?}"
        );
        assert!(
            gone.written(PLACEHOLDER).is_none(),
            "{route}: {:?}",
            gone.calls()
        );
        assert!(
            gone.calls()
                .iter()
                .any(|c| c == &format!("remove {PLACEHOLDER}")),
            "{route}"
        );
    }
}

/// Another program raises Apple's window beside this run's placeholder during its catalog read.
fn window_comes_up(sys: &Fake) {
    sys.running(&["/sbin/launchd"])
        .on(SOFTWAREUPDATE_LIST, Ran::ok(VM_LIST))
        .on_running(SOFTWAREUPDATE_LIST, &["/sbin/launchd", APPLE_WINDOW]);
}

/// The placeholder is removed, then the stops are restored, then the one caught is sent again.
fn assert_stop_sent_after_the_placeholder_goes(calls: &[String]) {
    let at = |needle: &str| calls.iter().position(|c| c.starts_with(needle));
    let removed = at("remove /tmp/.com.apple.dt.CommandLineTools").expect("placeholder removed");
    let restored = at("stops restored").expect("stops restored");
    let sent = at("stop sent again: signal 2").expect("the stop is sent again");
    assert!(removed < restored && restored < sent, "{calls:?}");
}

/// A stop during the install itself lets it finish and verify, then ends the run: the item
/// is Ok, and the next item of the apply never starts.
#[test]
fn a_stop_during_the_install_finishes_it_then_ends_the_run() {
    let mut sys = Fake::root();
    sys.running(&[]);
    sys.stop_after = Some(1);
    sys.on("/usr/sbin/softwareupdate -l", Ran::ok(VM_LIST))
        .on(
            "/usr/sbin/softwareupdate -i 'Command Line Tools for Xcode 27.0-27.0'",
            installed_quietly(),
        )
        .on_stat(RETAINED_INSTALL, Path::new(CLT_GIT), git())
        .on_stat(RETAINED_INSTALL, Path::new(CLT_CLANG), git())
        .on(PKGUTIL, Ran::ok(RECEIPT));
    CommandLineTools::install(&sys).unwrap();
    let calls = sys.calls();
    assert!(
        calls
            .iter()
            .any(|c| c.starts_with("/usr/sbin/softwareupdate -i")),
        "{calls:?}"
    );
    assert_stop_sent_after_the_placeholder_goes(&calls);
}

#[test]
fn the_placeholder_is_removed_even_when_softwareupdate_lists_no_tools() {
    let sys = Fake::root();
    sys.running(&[]);
    sys.on("/usr/bin/xcode-select -p", none_selected()).on(
        "/usr/sbin/softwareupdate -l",
        Ran::ok(&VM_LIST.replace("Command Line Tools for Xcode 27.0", "Safari 27.0")),
    );
    let items = expand(
        &Table {
            command_line_tools: Some(true),
            debugger: None,
            ..Table::default()
        },
        &ctx(),
    )
    .unwrap();
    let rows = engine::run(&items, Mode::Apply, &sys, &ctx(), &mut NoJournal);
    assert_eq!(rows[0].outcome, Outcome::Failed);
    assert!(
        rows[0]
            .why
            .as_deref()
            .unwrap()
            .contains("lists no Command Line Tools")
    );
    assert!(
        sys.calls()
            .iter()
            .any(|c| c.starts_with("remove /tmp/.com.apple.dt.CommandLineTools"))
    );
    assert!(sys.written(PLACEHOLDER).is_none());
}

#[test]
fn an_install_refused_with_exit_0_fails_even_with_a_stale_receipt() {
    for stat in [
        None,
        Some(Stat { dir: true, ..git() }),
        Some(Stat {
            symlink: true,
            ..git()
        }),
        Some(Stat {
            mode: 0o644,
            ..git()
        }),
        Some(Stat { len: 0, ..git() }),
    ] {
        for receipt in [no_receipt(), Ran::ok(RECEIPT)] {
            let sys = Fake::root();
            sys.running(&[]);
            sys.stat_of(CLT_CLANG, git());
            if let Some(stat) = stat {
                sys.stat_of(CLT_GIT, stat);
            }
            sys.on("/usr/bin/xcode-select -p", none_selected())
                .on("/usr/sbin/softwareupdate -l", Ran::ok(VM_LIST))
                .on(
                    "/usr/sbin/softwareupdate -i 'Command Line Tools for Xcode 27.0-27.0'",
                    no_space(),
                )
                .on(PKGUTIL, receipt);
            let items = expand(
                &Table {
                    command_line_tools: Some(true),
                    debugger: None,
                    ..Table::default()
                },
                &ctx(),
            )
            .unwrap();
            let rows = engine::run(&items, Mode::Apply, &sys, &ctx(), &mut NoJournal);
            assert_eq!(rows[0].outcome, Outcome::Failed);
            let why = rows[0].why.as_deref().unwrap();
            assert!(why.contains("9.52 GB is required"), "{why}");
            let calls = sys.calls();
            assert!(calls.iter().all(|c| !c.contains("--switch")));
            assert!(
                calls
                    .iter()
                    .any(|c| c.starts_with("remove /tmp/.com.apple.dt"))
            );
            assert_eq!(items[0].unfinished(), Some(Unfinished::Installer));
        }
    }
}

#[test]
fn a_first_catalog_outage_reaches_recovery_through_the_apply_engine() {
    let sys = Fake::root();
    sys.running(&[]);
    let install = "/usr/sbin/softwareupdate -i 'Command Line Tools for Xcode 27.0-27.0'";
    sys.on("/usr/bin/xcode-select -p", none_selected())
        .on("/usr/bin/xcode-select -p", none_selected())
        .on(
            "/usr/bin/xcode-select -p",
            Ran::ok("/Library/Developer/CommandLineTools\n"),
        )
        .on(
            SOFTWAREUPDATE_LIST,
            Ran::exit(1, "catalog network unavailable"),
        )
        .on(SOFTWAREUPDATE_LIST, Ran::ok(VM_LIST))
        .on(install, installed_quietly())
        .on_stat(install, Path::new(CLT_GIT), git())
        .on_stat(install, Path::new(CLT_CLANG), git())
        .on(PKGUTIL, Ran::ok(RECEIPT))
        .on(
            "/usr/bin/xcode-select --switch /Library/Developer/CommandLineTools",
            Ran::ok(""),
        );
    let items = expand(
        &Table {
            command_line_tools: Some(true),
            debugger: None,
            ..Table::default()
        },
        &ctx(),
    )
    .unwrap();
    let rows = engine::run(&items, Mode::Apply, &sys, &ctx(), &mut NoJournal);
    assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
    assert!(matches!(items[0].probe(&sys, &ctx()), Probe::Ok { .. }));
    let calls = sys.calls();
    assert_eq!(
        calls
            .iter()
            .filter(|c| c.as_str() == SOFTWAREUPDATE_LIST)
            .count(),
        2
    );
    assert_eq!(calls.iter().filter(|c| c.as_str() == install).count(), 1);
    assert!(calls.iter().any(|c| c == "sleep 5000ms"));
    assert!(
        !calls.iter().any(|c| c.contains("curl")
            || c.contains("sudo")
            || c.contains("xcode-select --install"))
    );
    assert!(sys.written(PLACEHOLDER).is_none());
    assert_eq!(items[0].unfinished(), None);
}

#[test]
fn installed_tools_repair_missing_or_unusable_selection_without_downloading() {
    for stale in [
        None,
        Some("/Applications/Removed Xcode.app/Contents/Developer"),
    ] {
        let sys = Fake::root();
        let selection = || stale.map_or_else(none_selected, |dir| Ran::ok(&format!("{dir}\n")));
        sys.stat_of(CLT_GIT, git())
            .stat_of(CLT_CLANG, git())
            .on("/usr/bin/xcode-select -p", selection())
            .on("/usr/bin/xcode-select -p", selection())
            .on(
                "/usr/bin/xcode-select -p",
                Ran::ok("/Library/Developer/CommandLineTools\n"),
            )
            .on(
                "/usr/bin/xcode-select --switch /Library/Developer/CommandLineTools",
                Ran::ok(""),
            );
        let items = expand(
            &Table {
                command_line_tools: Some(true),
                debugger: None,
                ..Table::default()
            },
            &ctx(),
        )
        .unwrap();
        let rows = engine::run(&items, Mode::Apply, &sys, &ctx(), &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:#?}");
        assert_eq!(
            rows[0].revert.as_deref(),
            Some("/usr/bin/xcode-select --reset")
        );
        assert!(sys.calls().iter().all(|c| !c.contains("softwareupdate")));
    }
}

#[test]
fn selected_compiler_aliases_are_checked_and_unread_paths_stay_unknown() {
    let dir = "/Applications/Xcode.app/Contents/Developer";
    let compiler = format!("{dir}/usr/bin/clang");
    let sys = Fake::root();
    sys.on("/usr/bin/xcode-select -p", Ran::ok(dir))
        .link(&compiler, "/private/compiler")
        .stat_of("/private/compiler", git());
    assert_eq!(selected(&sys).unwrap().as_deref(), Some(dir));

    sys.link(&compiler, "/usr/bin/clang")
        .stat_of("/usr/bin/clang", git());
    assert_eq!(selected(&sys).unwrap(), None);

    sys.link(&compiler, "/private/compiler")
        .stat_fails("/private/compiler", std::io::ErrorKind::PermissionDenied);
    assert!(selected(&sys).is_err());
    assert!(sys.calls().iter().all(|c| !c.contains("--switch")));
}

#[test]
fn a_selected_xcode_is_left_selected_when_the_tools_are_added() {
    let sys = Fake::root();
    sys.stat_of("/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/bin/clang", git());
    sys.running(&[]);
    sys.on(
        "/usr/bin/xcode-select -p",
        Ran::ok("/Applications/Xcode.app/Contents/Developer\n"),
    )
    .on("/usr/sbin/softwareupdate -l", Ran::ok(VM_LIST))
    .on(
        "/usr/sbin/softwareupdate -i 'Command Line Tools for Xcode 27.0-27.0'",
        installed_quietly(),
    )
    .on_stat(RETAINED_INSTALL, Path::new(CLT_GIT), git())
    .on_stat(RETAINED_INSTALL, Path::new(CLT_CLANG), git())
    .on(PKGUTIL, Ran::ok(RECEIPT));
    let items = expand(
        &Table {
            command_line_tools: Some(true),
            debugger: None,
            ..Table::default()
        },
        &ctx(),
    )
    .unwrap();
    items[0].apply(&sys, &ctx()).unwrap();
    assert!(sys.calls().iter().all(|c| !c.contains("--switch")));
}

#[test]
fn false_or_absent_leaves_the_tools_alone() {
    assert!(
        expand(
            &Table {
                command_line_tools: Some(false),
                debugger: None,
                ..Table::default()
            },
            &ctx()
        )
        .unwrap()
        .is_empty()
    );
    assert!(expand(&Table::default(), &ctx()).unwrap().is_empty());
}

/// Apple's own installer app's executable.
const APPLE_WINDOW: &str = "/System/Library/CoreServices/Install Command Line Developer \
                            Tools.app/Contents/MacOS/Install Command Line Developer Tools";

/// The processes running beside the install: launchd and the one `running` names
/// (`softwareupdate`, Apple's package `installer`, Apple's window `apple`, or a program
/// merely called `installer`, `lookalike`), under pids from [`Fake::PIDS`] on, their starts
/// unread until a test gives one ([`Fake::born`]).
fn installers(sys: &Fake, running: Option<&str>) {
    let exe = match running {
        Some("softwareupdate") => Some(SOFTWAREUPDATE),
        Some("installer") => Some(INSTALLER),
        Some("apple") => Some(APPLE_WINDOW),
        Some("lookalike") => Some("/tmp/installer"),
        Some(other) => panic!("no process {other}"),
        None => None,
    };
    sys.running(&["/sbin/launchd"].into_iter().chain(exe).collect::<Vec<_>>());
}

#[test]
fn unread_placeholder_state_preserves_independent_custody_evidence() {
    for process in [
        None,
        Some("softwareupdate"),
        Some("installer"),
        Some("lookalike"),
    ] {
        let sys = Fake::root();
        installers(&sys, process);
        assert_eq!(holder(&sys, None), Holder::Unread, "{process:?}");
    }

    let window = Fake::root();
    installers(&window, Some("apple"));
    assert_eq!(holder(&window, None), Holder::AppleWindow);

    let live = Fake::root();
    live.file(PLACEHOLDER, b"aterm 4242 1000\n")
        .born(4242, 1000);
    assert_eq!(
        holder(&live, None),
        Holder::Install,
        "the verified stamp proves custody"
    );
    live.born(4242, 1001);
    assert_eq!(
        holder(&live, None),
        Holder::Unread,
        "a reused pid proves no custody"
    );
}

#[test]
fn root_admission_mutates_no_placeholder_while_custody_is_unread() {
    let unread_table = Fake::root();
    let unread_path = Fake::root();
    unread_path
        .running(&[])
        .stat_fails(PLACEHOLDER, std::io::ErrorKind::PermissionDenied);
    for sys in [&unread_table, &unread_path] {
        let Err(failure) = CommandLineTools::hold_placeholder(sys) else {
            panic!("unread custody admits no claim");
        };
        assert_eq!(failure.class, Unfinished::Unread, "{failure:?}");
        assert!(sys.written(PLACEHOLDER).is_none());
        assert!(
            sys.calls()
                .iter()
                .all(|call| { !call.starts_with("create ") && !call.starts_with("remove ") }),
            "{:?}",
            sys.calls()
        );
    }
    unread_table.running(&[]);
    let claim = CommandLineTools::hold_placeholder(&unread_table).unwrap();
    assert!(claim.is_some());
    assert!(unread_table.written(PLACEHOLDER).is_some());
    drop(claim);

    installers(&unread_path, Some("apple"));
    assert!(
        CommandLineTools::hold_placeholder(&unread_path)
            .unwrap()
            .is_none()
    );
    assert!(unread_path.written(PLACEHOLDER).is_none());
}

#[test]
fn a_clt_marker_an_install_holds_is_never_replaced_or_removed_by_an_install() {
    let ready = || {
        let sys = Fake::root();
        sys.on("/usr/sbin/softwareupdate -l", Ran::ok(VM_LIST))
            .on(
                "/usr/sbin/softwareupdate -i 'Command Line Tools for Xcode 27.0-27.0'",
                installed_quietly(),
            )
            .on_stat(RETAINED_INSTALL, Path::new(CLT_GIT), git())
            .on_stat(RETAINED_INSTALL, Path::new(CLT_CLANG), git())
            .on(PKGUTIL, Ran::ok(RECEIPT));
        sys
    };

    let fresh = ready();
    fresh.running(&[]);
    CommandLineTools::install(&fresh).unwrap();
    assert!(fresh.written(PLACEHOLDER).is_none());
    assert!(
        fresh
            .calls()
            .iter()
            .any(|c| c == "/usr/sbin/softwareupdate -l")
    );
    assert!(
        fresh
            .calls()
            .iter()
            .any(|c| c.starts_with("/usr/sbin/softwareupdate -i "))
    );
    assert!(
        fresh
            .calls()
            .iter()
            .any(|c| c == &format!("remove {PLACEHOLDER}"))
    );

    let no_tools = Fake::root();
    no_tools.running(&[]);
    no_tools.on("/usr/sbin/softwareupdate -l", Ran::ok("no tools listed\n"));
    assert!(CommandLineTools::install(&no_tools).is_err());
    assert!(no_tools.written(PLACEHOLDER).is_none());
    assert!(
        no_tools
            .calls()
            .iter()
            .any(|c| c == &format!("remove {PLACEHOLDER}"))
    );

    let saved = b"another install owns this marker\n";
    let target = "/unowned-clt-marker-target";
    let mut observed = Vec::new();
    for linked in [false, true] {
        let sys = ready();
        installers(&sys, Some("softwareupdate"));
        if linked {
            sys.file(target, saved).link(PLACEHOLDER, target);
            assert!(sys.stat(Path::new(PLACEHOLDER)).unwrap().symlink);
        } else {
            sys.file(PLACEHOLDER, saved);
            assert_eq!(sys.written(PLACEHOLDER).as_deref(), Some(saved.as_slice()));
        }
        let result = CommandLineTools::install(&sys);
        assert_eq!(
            result.as_ref().err().map(|failure| failure.class),
            Some(Unfinished::Busy)
        );
        let calls = sys.calls();
        let preserved = if linked {
            sys.stat(Path::new(PLACEHOLDER)).is_ok_and(|s| s.symlink)
                && sys.written(PLACEHOLDER).is_none()
                && sys.written(target).as_deref() == Some(saved.as_slice())
        } else {
            sys.written(PLACEHOLDER).as_deref() == Some(saved.as_slice())
        };
        observed.push((
            result.is_err(),
            preserved,
            !calls.iter().any(|c| c.starts_with(SOFTWAREUPDATE)),
            !calls.iter().any(|c| c == &format!("remove {PLACEHOLDER}")),
        ));
    }
    assert_eq!(
        observed,
        vec![(true, true, true, true); 2],
        "occupied CLT markers retain their owners"
    );
}

/// A second run (a second `aterm init` while the first waits on Apple's installer window,
/// with no installer process running) takes no placeholder a live run's stamp names: only
/// the run that started the install holds or replaces it. One whose
/// run is gone (no such process, or one started at another time) is stale and taken, as is
/// one whose stamp names no start (`0` matches no live process: the pid alone may be any
/// later process's), and a run's own guard removes the placeholder only while it is still
/// its own.
#[test]
fn only_the_run_that_started_the_install_holds_or_replaces_its_placeholder() {
    let ready = |stamp: &[u8]| {
        let sys = Fake::root();
        sys.file(PLACEHOLDER, stamp)
            .on("/usr/sbin/softwareupdate -l", Ran::ok(VM_LIST))
            .on(
                "/usr/sbin/softwareupdate -i 'Command Line Tools for Xcode 27.0-27.0'",
                installed_quietly(),
            )
            .on_stat(RETAINED_INSTALL, Path::new(CLT_GIT), git())
            .on_stat(RETAINED_INSTALL, Path::new(CLT_CLANG), git())
            .on(PKGUTIL, Ran::ok(RECEIPT));
        installers(&sys, None);
        sys
    };
    // The first run, alive, from the start its stamp names.
    {
        let stamp = &b"aterm 4242 1000\n"[..];
        let live = ready(stamp);
        live.born(4242, 1000);
        assert!(placeholder_held_by_a_live_run(&live));
        let failure = CommandLineTools::install(&live).unwrap_err();
        assert_eq!(failure.class, Unfinished::Busy, "{failure:?}");
        assert_eq!(
            live.written(PLACEHOLDER).as_deref(),
            Some(stamp),
            "its own stays"
        );
        let calls = live.calls();
        assert!(
            !calls
                .iter()
                .any(|c| c.starts_with(SOFTWAREUPDATE) || c.starts_with("remove ")),
            "{calls:?}"
        );
    }
    // The first run is gone, the pid is another process's now, or the stamp names no start:
    // stale, taken.
    for (stamp, born) in [
        (&b"aterm 4242 1000\n"[..], None),
        (b"aterm 4242 1000\n", Some(999)),
        (b"aterm 4242 0\n", Some(7)),
    ] {
        let gone = ready(stamp);
        if let Some(started) = born {
            gone.born(4242, started);
        }
        assert!(!placeholder_held_by_a_live_run(&gone));
        CommandLineTools::install(&gone).unwrap();
        let calls = gone.calls();
        let at = |needle: &str| calls.iter().position(|c| c.starts_with(needle));
        let removed = at(&format!("remove {PLACEHOLDER}")).expect("the stale one goes");
        let created = at(&format!("create {PLACEHOLDER}")).expect("this run's own");
        assert!(removed < created, "{calls:?}");
        assert!(gone.written(PLACEHOLDER).is_none(), "removed when it ends");
    }
    // A stamp a person's account wrote is no run's: root owns a run's placeholder.
    let forged = ready(b"aterm 4242 1000\n");
    forged.born(4242, 1000).stat_of(
        PLACEHOLDER,
        crate::port::Stat {
            uid: 501,
            gid: 20,
            mode: 0o644,
            dir: false,
            symlink: false,
            len: 16,
        },
    );
    assert!(!placeholder_held_by_a_live_run(&forged));

    // This run's guard leaves a placeholder that is no longer its own.
    let taken = Fake::root();
    let install = "/usr/sbin/softwareupdate -i 'Command Line Tools for Xcode 27.0-27.0'";
    taken
        .on("/usr/sbin/softwareupdate -l", Ran::ok(VM_LIST))
        .on(install, installed_quietly())
        .on_write(install, Path::new(PLACEHOLDER), b"aterm 4243 2000\n")
        .on_stat(install, Path::new(CLT_GIT), git())
        .on_stat(install, Path::new(CLT_CLANG), git())
        .on(PKGUTIL, Ran::ok(RECEIPT));
    installers(&taken, None);
    CommandLineTools::install(&taken).unwrap();
    assert_eq!(
        taken.written(PLACEHOLDER).as_deref(),
        Some(&b"aterm 4243 2000\n"[..]),
        "another run's placeholder stays: {:?}",
        taken.calls()
    );
    assert!(
        !taken
            .calls()
            .iter()
            .any(|c| c == &format!("remove {PLACEHOLDER}")),
        "{:?}",
        taken.calls()
    );
}

#[test]
fn placeholder_cleanup_requires_a_readable_empty_installer_table() {
    let sys = Fake::root();
    let saved = stamp(&sys);
    sys.file(PLACEHOLDER, &saved);
    drop(Placeholder {
        sys: &sys,
        stamp: saved.clone(),
        _claim: placeholder_lock(&sys).unwrap(),
    });
    assert_eq!(sys.written(PLACEHOLDER).as_deref(), Some(saved.as_slice()));
    assert!(!sys.calls().iter().any(|call| call.starts_with("remove ")));

    installers(&sys, Some("apple"));
    drop(Placeholder {
        sys: &sys,
        stamp: saved.clone(),
        _claim: placeholder_lock(&sys).unwrap(),
    });
    assert_eq!(sys.written(PLACEHOLDER).as_deref(), Some(saved.as_slice()));

    installers(&sys, None);
    drop(Placeholder {
        sys: &sys,
        stamp: saved,
        _claim: placeholder_lock(&sys).unwrap(),
    });
    assert!(
        sys.written(PLACEHOLDER).is_none(),
        "known idle permits cleanup"
    );
}

#[test]
fn a_placeholder_claim_excludes_a_second_root_without_a_readable_birth() {
    let sys = Fake::root();
    installers(&sys, None);
    let first = CommandLineTools::hold_placeholder(&sys).unwrap().unwrap();
    let saved = sys.written(PLACEHOLDER).unwrap();
    match CommandLineTools::hold_placeholder(&sys) {
        Err(failure) => assert_eq!(failure.class, Unfinished::Busy),
        Ok(_) => panic!("the first claim still owns the install"),
    }
    assert_eq!(sys.written(PLACEHOLDER).as_deref(), Some(saved.as_slice()));
    drop(first);
    assert!(CommandLineTools::hold_placeholder(&sys).unwrap().is_some());
}

#[test]
fn a_placeholder_claim_excludes_stale_cleanup_until_it_drops() {
    let sys = Fake::root();
    installers(&sys, None);
    let first = CommandLineTools::hold_placeholder(&sys).unwrap().unwrap();
    let saved = sys.written(PLACEHOLDER).unwrap();
    remove_a_gone_runs_placeholder(&sys);
    assert_eq!(sys.written(PLACEHOLDER).as_deref(), Some(saved.as_slice()));
    installers(&sys, Some("apple"));
    drop(first);
    assert_eq!(sys.written(PLACEHOLDER).as_deref(), Some(saved.as_slice()));
    installers(&sys, None);
    remove_a_gone_runs_placeholder(&sys);
    assert!(sys.written(PLACEHOLDER).is_none());
}

#[test]
fn a_refused_placeholder_lock_permits_no_claim_or_stale_cleanup() {
    for kind in [
        std::io::ErrorKind::PermissionDenied,
        std::io::ErrorKind::Unsupported,
    ] {
        let sys = Fake::root();
        installers(&sys, None);
        sys.file(PLACEHOLDER, b"aterm 4242 0\n")
            .exclusive_fails(Path::new(PLACEHOLDER_LOCK), kind);
        let Err(failure) = CommandLineTools::hold_placeholder(&sys) else {
            panic!("a refused lock cannot admit a claim");
        };
        assert_eq!(failure.class, Unfinished::Unread);
        remove_a_gone_runs_placeholder(&sys);
        assert_eq!(
            sys.written(PLACEHOLDER).as_deref(),
            Some(b"aterm 4242 0\n".as_slice())
        );
        assert!(
            sys.calls()
                .iter()
                .all(|call| { !call.starts_with("create ") && !call.starts_with("remove ") })
        );
    }
}

/// A placeholder no install holds (neither `softwareupdate` nor `installer` runs, nor Apple's
/// installer window, nor a live run of the root half) is stale, left by an install that ended
/// without removing it: the install takes its place instead of failing on it, and removes it
/// when it ends, beside a program merely called `installer` too. Apple's package installer
/// and a process table that cannot be read leave it alone: another install, or one nobody can
/// rule out.
#[test]
fn a_stale_clt_marker_nothing_holds_is_replaced_by_the_install() {
    let stale = stale_marker();
    installers(&stale, None);
    CommandLineTools::install(&stale).unwrap();
    let calls = stale.calls();
    let at = |needle: &str| calls.iter().position(|c| c.starts_with(needle));
    let removed = at(&format!("remove {PLACEHOLDER}")).expect("the stale marker goes");
    let created = at(&format!("create {PLACEHOLDER}")).expect("this install's own");
    let listed = at("/usr/sbin/softwareupdate -l").expect("the install goes on");
    assert!(removed < created && created < listed, "{calls:?}");
    assert!(stale.written(PLACEHOLDER).is_none(), "removed when it ends");

    // A program merely called `installer` holds nothing: as stale as with none.
    let beside = stale_marker();
    installers(&beside, Some("lookalike"));
    CommandLineTools::install(&beside).unwrap();
    assert!(
        beside
            .calls()
            .iter()
            .any(|c| c.starts_with("/usr/sbin/softwareupdate -i ")),
        "{:?}",
        beside.calls()
    );

    // Apple's package installer holds it: another install of the tools is running.
    let package = stale_marker();
    installers(&package, Some("installer"));
    let failure = CommandLineTools::install(&package).unwrap_err();
    assert_eq!(failure.class, Unfinished::Busy, "{failure:?}");

    // Nothing could tell whether one runs: it is left to whoever may hold it.
    let unread = stale_marker();
    assert_eq!(
        CommandLineTools::install(&unread).unwrap_err().class,
        Unfinished::Unread
    );
    assert!(unread.written(PLACEHOLDER).is_some());
}

/// A placeholder left by an install that ended, ready for an install that lists and installs
/// the tools.
fn stale_marker() -> Fake {
    let sys = Fake::root();
    sys.file(PLACEHOLDER, b"left by an install that ended\n")
        .on("/usr/sbin/softwareupdate -l", Ran::ok(VM_LIST))
        .on(
            "/usr/sbin/softwareupdate -i 'Command Line Tools for Xcode 27.0-27.0'",
            installed_quietly(),
        )
        .on_stat(RETAINED_INSTALL, Path::new(CLT_GIT), git())
        .on_stat(RETAINED_INSTALL, Path::new(CLT_CLANG), git())
        .on(PKGUTIL, Ran::ok(RECEIPT));
    sys
}

/// A process the kernel names no executable for (one whose file a replacement deleted while
/// it ran) is none of Apple's installers, which live on the sealed system volume: beside a
/// stale placeholder whose stamped run is gone, the install takes the placeholder's place
/// and goes on, as it would beside named programs alone.
/// NEGATIVE CONTROL: the same table with `softwareupdate` named in it is another install
/// running, which the install leaves alone.
#[test]
fn a_process_the_kernel_names_no_file_for_holds_no_placeholder() {
    let ready = |softwareupdate: bool| {
        let sys = stale_marker();
        sys.file(PLACEHOLDER, b"aterm 4242 1000\n");
        installers(&sys, softwareupdate.then_some("softwareupdate"));
        sys.process(unnamed(), None);
        sys
    };
    let unnamed_beside = ready(false);
    assert_eq!(holder(&unnamed_beside, Some(true)), Holder::Nothing);
    CommandLineTools::install(&unnamed_beside).unwrap();
    let calls = unnamed_beside.calls();
    let at = |needle: &str| calls.iter().position(|c| c.starts_with(needle));
    let removed = at(&format!("remove {PLACEHOLDER}")).expect("the stale marker goes");
    let created = at(&format!("create {PLACEHOLDER}")).expect("this install's own");
    let installed = at("/usr/sbin/softwareupdate -i ").expect("and installs");
    assert!(removed < created && created < installed, "{calls:?}");

    let control = ready(true);
    assert_eq!(holder(&control, Some(true)), Holder::Install);
    let failure = CommandLineTools::install(&control).unwrap_err();
    assert_eq!(failure.class, Unfinished::Busy, "{failure:?}");
    assert!(
        !control
            .calls()
            .iter()
            .any(|c| c.starts_with(SOFTWAREUPDATE) || c.starts_with("remove ")),
        "{:?}",
        control.calls()
    );
}

/// A process the kernel names no executable for.
fn unnamed() -> crate::port::Process {
    crate::port::Process {
        pid: Fake::PIDS + 7,
        ppid: crate::port::Process::LAUNCHD,
        terminal: false,
        exe: None,
        comm: "deleted".into(),
    }
}

/// Apple's installer window beside the placeholder is Apple's own flow at any age: it made
/// that placeholder as it opened (`O_EXCL`, before any press) and removes it when its request
/// ends, so a press hours later leaves no trace aterm reads. The install neither removes nor
/// replaces the placeholder, never asks `softwareupdate` for the tools (a second download of
/// them beside the window's own) nor `xcode-select --install` for another window, and waits
/// on that window until the tools are there. A command-line installer or a live run of the
/// root half beside the window is another install running.
/// NEGATIVE CONTROL: the same placeholder with no window beside it is stale, and the install
/// takes its place, lists the tools and installs them.
#[test]
fn apples_window_beside_the_placeholder_is_waited_on_at_any_age_and_never_replaced() {
    let window = Fake::PIDS + 1;
    let day = 24 * 60 * 60;
    for age in [Some(60), Some(3 * 60 * 60), Some(10 * day), None] {
        let sys = stale_marker();
        sys.file(PLACEHOLDER, b"");
        installers(&sys, Some("apple"));
        if let Some(age) = age {
            sys.born(window, sys.now - age);
        }
        assert_eq!(holder(&sys, Some(true)), Holder::AppleWindow, "{age:?}");
        sys.on_sleep(|sys, _| {
            if sys
                .calls()
                .iter()
                .filter(|c| c.starts_with("sleep "))
                .count()
                == 2
            {
                sys.stat_of(CLT_GIT, git()).stat_of(CLT_CLANG, git());
            }
        });
        CommandLineTools::install(&sys).unwrap();
        let calls = sys.calls();
        assert!(
            !calls.iter().any(|c| c.starts_with(SOFTWAREUPDATE)
                || c.starts_with("remove ")
                || c.starts_with("create ")
                || c.contains("xcode-select --install")),
            "{age:?}: {calls:?}"
        );
        assert_eq!(
            calls.iter().filter(|c| c.starts_with("sleep ")).count(),
            2,
            "{age:?}: it waited on the window: {calls:?}"
        );
        assert_eq!(
            sys.written(PLACEHOLDER).as_deref(),
            Some(&b""[..]),
            "{age:?}: the window's own stays"
        );
    }

    for beside in ["softwareupdate", "live run"] {
        let sys = stale_marker();
        sys.file(PLACEHOLDER, b"aterm 4242 1000\n");
        if beside == "live run" {
            installers(&sys, Some("apple"));
            sys.born(4242, 1000);
        } else {
            sys.running(&["/sbin/launchd", APPLE_WINDOW, SOFTWAREUPDATE]);
        }
        assert_eq!(holder(&sys, Some(true)), Holder::Install, "{beside}");
        let failure = CommandLineTools::install(&sys).unwrap_err();
        assert_eq!(failure.class, Unfinished::Busy, "{beside}: {failure:?}");
        assert!(
            !sys.calls().iter().any(|c| c.starts_with(SOFTWAREUPDATE)
                || c.starts_with("remove ")
                || c.starts_with("sleep ")),
            "{beside}: {:?}",
            sys.calls()
        );
    }

    let control = stale_marker();
    control.file(PLACEHOLDER, b"");
    installers(&control, None);
    CommandLineTools::install(&control).unwrap();
    let calls = control.calls();
    let at = |needle: &str| calls.iter().position(|c| c.starts_with(needle));
    let removed = at(&format!("remove {PLACEHOLDER}")).expect("the stale marker goes");
    let created = at(&format!("create {PLACEHOLDER}")).expect("this install's own");
    let listed = at("/usr/sbin/softwareupdate -l").expect("the install goes on");
    let installed = at("/usr/sbin/softwareupdate -i ").expect("and installs");
    assert!(
        removed < created && created < listed && listed < installed,
        "{calls:?}"
    );
}

/// Apple's installer window that closes without installing the tools ends the wait on it at
/// once, as its own class: a stub raised it while this run read the catalog, or it was open
/// already, beside its own
/// placeholder, beside one an earlier run kept for it, or with none. A person pressed Not
/// Now, Cancel, or Done on Apple's own failure there (the window unlinks the placeholder,
/// whoever made it, and quits), or quit the window before pressing Install, which leaves a
/// placeholder aterm stamped (the window unlinks at its exit only a file it made): this run's,
/// which its guard removes (the wait does as the window goes where its start was not read,
/// which reads its run gone), or an earlier run's, which the wait removes as the window goes.
/// NEGATIVE CONTROL: a window gone with Apple's own empty placeholder still there (killed
/// outright, so it removed nothing) is no end, and the wait runs to its patience.
#[test]
fn apples_window_closing_without_the_tools_ends_the_wait_at_once() {
    let sleeps = |sys: &Fake| {
        sys.calls()
            .iter()
            .filter(|c| c.starts_with("sleep "))
            .count()
    };
    // Already open beside its own placeholder when the run began.
    let found = Fake::root();
    found.file(PLACEHOLDER, b"");
    installers(&found, Some("apple"));
    found.on_sleep(|sys, _| {
        let _ = sys.remove(Path::new(PLACEHOLDER));
        sys.running(&["/sbin/launchd"]);
    });
    let failure = CommandLineTools::install(&found).unwrap_err();
    assert_eq!(failure.class, Unfinished::WindowClosed, "{failure:?}");
    assert_eq!(sleeps(&found), 1, "{:?}", found.calls());

    // Already open with no placeholder beside it: this run made none to hold the wait.
    let alone = Fake::root();
    installers(&alone, Some("apple"));
    alone.on_sleep(|sys, _| {
        sys.running(&["/sbin/launchd"]);
    });
    let failure = CommandLineTools::install(&alone).unwrap_err();
    assert_eq!(failure.class, Unfinished::WindowClosed, "{failure:?}");
    assert_eq!(sleeps(&alone), 1, "{:?}", alone.calls());

    // Raised while this run read the catalog, beside this run's placeholder, which the
    // window unlinks as it closes.
    let raised = Fake::root();
    raised
        .running(&["/sbin/launchd"])
        .on("/usr/sbin/softwareupdate -l", Ran::ok(VM_LIST))
        .on_running(
            "/usr/sbin/softwareupdate -l",
            &["/sbin/launchd", APPLE_WINDOW],
        );
    raised.on_sleep(|sys, _| {
        let _ = sys.remove(Path::new(PLACEHOLDER));
        sys.running(&["/sbin/launchd"]);
    });
    let failure = CommandLineTools::install(&raised).unwrap_err();
    assert_eq!(failure.class, Unfinished::WindowClosed, "{failure:?}");
    assert_eq!(sleeps(&raised), 1, "{:?}", raised.calls());

    // Quit before Install was pressed, beside a placeholder aterm stamped, which stays: this
    // a stub raised it during the catalog read, or an earlier run, gone now, kept its
    // placeholder for it. Whether
    // this run's own start is read (its stamp then a live run's, as on a Mac) or not (a start
    // of 0, which matches no process), the stamped placeholder is gone once the run has
    // ended, removed once.
    for (route, own_start) in ["raised", "kept"]
        .into_iter()
        .flat_map(|route| [(route, false), (route, true)])
    {
        let quit = Fake::root();
        if own_start {
            quit.born(std::process::id(), 1_000);
        }
        match route {
            "raised" => {
                quit.running(&["/sbin/launchd"])
                    .on("/usr/sbin/softwareupdate -l", Ran::ok(VM_LIST))
                    .on_running(
                        "/usr/sbin/softwareupdate -l",
                        &["/sbin/launchd", APPLE_WINDOW],
                    );
            }
            _ => {
                quit.file(PLACEHOLDER, b"aterm 4242 1000\n");
                installers(&quit, Some("apple"));
                assert_eq!(holder(&quit, Some(true)), Holder::AppleWindow);
            }
        }
        quit.on_sleep(|sys, _| {
            sys.running(&["/sbin/launchd"]);
        });
        let failure = CommandLineTools::install(&quit).unwrap_err();
        assert_eq!(
            failure.class,
            Unfinished::WindowClosed,
            "{route} {own_start}: {failure:?}"
        );
        let calls = quit.calls();
        assert_eq!(sleeps(&quit), 1, "{route} {own_start}: {calls:?}");
        assert!(
            quit.written(PLACEHOLDER).is_none(),
            "{route} {own_start}: the stamped placeholder goes with the window: {calls:?}"
        );
        assert_eq!(
            calls.iter().filter(|c| c.starts_with("remove ")).count(),
            1,
            "{route} {own_start}: {calls:?}"
        );
        assert!(
            !calls
                .iter()
                .any(|c| c.starts_with("/usr/sbin/softwareupdate -i")),
            "{route} {own_start}: {calls:?}"
        );
    }

    let killed = Fake::root();
    killed.file(PLACEHOLDER, b"");
    installers(&killed, Some("apple"));
    killed.on_sleep(|sys, _| {
        sys.running(&["/sbin/launchd"]);
    });
    let failure = CommandLineTools::install(&killed).unwrap_err();
    assert_eq!(failure.class, Unfinished::AppleWindow, "{failure:?}");
    assert_eq!(sleeps(&killed), 900, "every 2 s for 30 minutes");
}

/// A placeholder a live run of aterm's stamped stays as Apple's window goes: the wait that
/// ends `WindowClosed` removes only a stamp whose run is gone
/// ([`remove_a_gone_runs_placeholder`]). This run's own, its start read, is still there when
/// the wait decides the window closed (its guard removes it once the wait has returned); and
/// another run's, which took the stale stamp an earlier run kept in the tick after the window
/// went, is left, the wait removing nothing.
/// NEGATIVE CONTROL: that other run gone (its start matches no process), its stamp goes with
/// the window. The mutant `remove_a_gone_runs_placeholder` without its
/// `holder(sys, Some(true)) == Holder::Nothing` check removes both live stamps and fails this test.
#[test]
fn a_live_runs_placeholder_stays_as_apples_window_goes() {
    let own = Fake::root();
    own.born(std::process::id(), 1_000);
    let own_stamp = stamp(&own);
    own.file(PLACEHOLDER, &own_stamp);
    installers(&own, Some("apple"));
    assert_eq!(
        holder(&own, Some(true)),
        Holder::Install,
        "this run is alive"
    );
    own.on_sleep(|sys, _| {
        sys.running(&["/sbin/launchd"]);
    });
    let failure = CommandLineTools::wait_on_window(&own, "patience").unwrap_err();
    assert_eq!(failure.class, Unfinished::WindowClosed, "{failure:?}");
    assert_eq!(
        own.written(PLACEHOLDER).as_deref(),
        Some(own_stamp.as_slice()),
        "the wait leaves this run's own to its guard: {:?}",
        own.calls()
    );

    for live in [true, false] {
        let kept = Fake::root();
        kept.file(PLACEHOLDER, b"aterm 4242 1000\n");
        installers(&kept, Some("apple"));
        kept.on_sleep(move |sys, _| {
            sys.running(&["/sbin/launchd"]);
            sys.file(PLACEHOLDER, b"aterm 4243 2000\n");
            if live {
                sys.born(4243, 2_000);
            }
        });
        let failure = CommandLineTools::install(&kept).unwrap_err();
        assert_eq!(
            failure.class,
            Unfinished::WindowClosed,
            "{live}: {failure:?}"
        );
        let calls = kept.calls();
        assert_eq!(
            calls.iter().filter(|c| c.starts_with("remove ")).count(),
            usize::from(!live),
            "{live}: {calls:?}"
        );
        assert_eq!(
            kept.written(PLACEHOLDER).is_some(),
            live,
            "{live}: {calls:?}"
        );
    }
}

/// Each way the install falls short is named by its class ([`Item::unfinished`]), which
/// `machines here apply --plain` exits with and System State's tab says in plain words.
#[test]
fn each_way_the_install_falls_short_is_named_by_its_class() {
    let applied = |script: &dyn Fn(&Fake)| {
        let sys = Fake::root();
        sys.running(&[]);
        sys.on("/usr/bin/xcode-select -p", none_selected());
        script(&sys);
        let items = expand(
            &Table {
                command_line_tools: Some(true),
                debugger: None,
                ..Table::default()
            },
            &ctx(),
        )
        .unwrap();
        let rows = engine::run(&items, Mode::Apply, &sys, &ctx(), &mut NoJournal);
        assert_ne!(rows[0].outcome, Outcome::Changed, "{rows:?}");
        items[0].unfinished()
    };
    let install = "/usr/sbin/softwareupdate -i 'Command Line Tools for Xcode 27.0-27.0'";
    assert_eq!(
        applied(&|sys| {
            sys.on(
                "/usr/sbin/softwareupdate -l",
                Ran::exit(1, "Can't connect to the Apple Software Update server."),
            );
        }),
        Some(Unfinished::Catalog)
    );
    assert_eq!(
        applied(&|sys| {
            sys.on("/usr/sbin/softwareupdate -l", Ran::ok("no tools listed\n"));
        }),
        Some(Unfinished::NoneOffered)
    );
    assert_eq!(
        applied(&|sys| {
            sys.on("/usr/sbin/softwareupdate -l", Ran::ok(VM_LIST)).on(
                install,
                Ran::exit(
                    1,
                    "Error downloading Command Line Tools for Xcode: network lost",
                ),
            );
        }),
        Some(Unfinished::Download)
    );
    assert_eq!(
        applied(&|sys| {
            sys.on("/usr/sbin/softwareupdate -l", Ran::ok(VM_LIST)).on(
                install,
                Ran {
                    status: Some(1),
                    stdout: b"Downloading Command Line Tools\nDownloaded Command Line \
                              Tools\nInstalling Command Line Tools\n"
                        .to_vec(),
                    stderr: b"Failed to install Command Line Tools\n".to_vec(),
                },
            );
        }),
        Some(Unfinished::Installer)
    );
    assert_eq!(
        applied(&|sys| {
            sys.on("/usr/sbin/softwareupdate -l", Ran::ok(VM_LIST))
                .on(install, no_space())
                .on(PKGUTIL, no_receipt());
        }),
        Some(Unfinished::Installer),
        "exit 0 with nothing installed"
    );
    assert_eq!(
        applied(&|sys| {
            sys.file(PLACEHOLDER, b"");
            installers(sys, Some("softwareupdate"));
        }),
        Some(Unfinished::Busy)
    );
    assert_eq!(
        applied(&|sys| {
            sys.file(PLACEHOLDER, b"");
            installers(sys, Some("apple"));
            sys.on_sleep(|sys, _| {
                let _ = sys.remove(Path::new(PLACEHOLDER));
                sys.running(&["/sbin/launchd"]);
            });
        }),
        Some(Unfinished::WindowClosed)
    );
}
