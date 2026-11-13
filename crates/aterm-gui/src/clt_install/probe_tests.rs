// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn an_unread_table_is_no_idle_table_and_live_evidence_still_counts() {
    use super::super::{Activity, Reading};
    for placeholder in [false, true] {
        let unread = reading_from(Some(placeholder), None::<std::iter::Empty<Option<&Path>>>);
        assert_eq!(super::super::activity(unread), Activity::Unread);
        let empty = reading_from(Some(placeholder), Some(std::iter::empty::<Option<&Path>>()));
        assert_eq!(super::super::activity(empty), Activity::Idle);
        assert_eq!(
            super::super::activity(Reading {
                live_run: true,
                ..unread
            }),
            if placeholder {
                Activity::Installing
            } else {
                Activity::Unread
            }
        );
    }
}

#[test]
fn a_failed_placeholder_stat_is_unread_not_absent() {
    use super::super::Activity;
    let dir = aterm_tempfile::tempdir().unwrap();
    let absent = dir.path().join("absent");
    assert_eq!(placeholder_at(&absent), Some(false));
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&absent, &link).unwrap();
    assert_eq!(placeholder_at(&link), Some(true));
    let parent = dir.path().join("parent");
    std::fs::write(&parent, b"a file, not a directory").unwrap();
    assert_eq!(placeholder_at(&parent), Some(true));
    let unread = placeholder_at(&parent.join("placeholder"));
    assert_eq!(unread, None);
    let reading = reading_from(unread, Some(std::iter::empty::<Option<&Path>>()));
    assert_eq!(super::super::activity(reading), Activity::Unread);
    let window = reading_from(unread, Some([Some(INSTALLER)]));
    assert_eq!(super::super::activity(window), Activity::AppleWindow);
}

/// [`super::super::activity`] over `paths`, each a running process's executable, beside the
/// placeholder when `placeholder`.
fn activity(
    placeholder: bool,
    paths: impl IntoIterator<Item = impl AsRef<Path>>,
) -> super::super::Activity {
    super::super::activity(reading_from(
        Some(placeholder),
        Some(paths.into_iter().map(Some)),
    ))
}

const INSTALLER: &str = "/System/Library/CoreServices/Install Command Line Developer Tools.app/Contents/MacOS/Install Command Line Developer Tools";

/// The page reads an install, and Apple's window with or without the placeholder, exactly
/// where `aterm init` and the root half do (`aterm_machines::items::developer::holder`): the
/// one rule ([`aterm_types::macos_setup::tools_held_by`]) over the same processes, a process
/// the kernel names no executable for among them, then a placeholder a live run's stamp names
/// ([`aterm_types::macos_setup::placeholder_held_by_a_live_run`], the one reader all share:
/// a run between its steps, or waiting on Apple's installer window, with no installer process
/// running) ahead of the window. An install outranks the window beside it; the window with no
/// placeholder still asks the person to finish there, and a command-line installer with no
/// placeholder installs nothing of theirs.
#[test]
fn the_page_reads_an_install_where_init_and_the_root_half_do() {
    use super::super::Activity;
    use aterm_types::macos_setup::ToolsInstaller;
    for placeholder in [false, true] {
        for live_run in [false, true] {
            for paths in [
                &[][..],
                &[None],
                &[Some("/usr/sbin/softwareupdate")],
                &[None, Some("/usr/sbin/softwareupdate")],
                &[Some("/usr/sbin/installer")],
                &[Some(INSTALLER)],
                &[None, Some(INSTALLER)],
                &[Some("/bin/zsh"), Some("/usr/sbin/installer")],
                &[Some("/tmp/softwareupdate"), Some(INSTALLER)],
                &[Some(INSTALLER), Some("/usr/sbin/softwareupdate")],
            ] {
                let mut reading = reading_from(Some(placeholder), Some(paths.iter().copied()));
                reading.live_run = live_run;
                let held = aterm_types::macos_setup::tools_held_by(
                    placeholder,
                    paths.iter().copied(),
                    is_install_process,
                );
                let holder = match held {
                    Some(ToolsInstaller::CommandLine) => Activity::Installing,
                    _ if placeholder && live_run => Activity::Installing,
                    Some(ToolsInstaller::AppleWindow) => Activity::AppleWindow,
                    None => Activity::Idle,
                };
                assert_eq!(
                    super::super::activity(reading),
                    holder,
                    "{placeholder} {live_run} {paths:?}"
                );
            }
        }
    }
}

/// The page reads a run's stamp as `aterm init` does: a regular file of the placeholder's
/// owner that no group or other may write, no longer than a stamp, never through a link;
/// an empty placeholder (Apple's own) has none.
#[test]
fn the_page_reads_a_runs_stamp_as_init_does() {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("placeholder");
    let stamp = aterm_types::macos_setup::placeholder_stamp(4242, 1000);
    std::fs::write(&path, &stamp).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let me = path.metadata().unwrap().uid();
    assert_eq!(stamp_of(&path, me).as_deref(), Some(stamp.as_bytes()));
    assert_eq!(stamp_of(&path, me.wrapping_add(1)), None, "another owner's");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
    assert_eq!(stamp_of(&path, me), None, "one anyone may write");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&path, &link).unwrap();
    assert_eq!(stamp_of(&link, me), None, "never through a link");
    std::fs::write(&path, b"").unwrap();
    assert_eq!(stamp_of(&path, me), None, "Apple's own, empty");
    std::fs::write(
        &path,
        vec![b'a'; aterm_types::macos_setup::PLACEHOLDER_STAMP_MAX + 1],
    )
    .unwrap();
    assert_eq!(stamp_of(&path, me), None, "longer than a stamp");
}

/// A live run's chain is the run and its parents, nearest first, by the process table: it
/// ends at a process the table does not list, at launchd or the kernel, or where it holds no
/// more, and a cycle (a table read across a pid's reuse) only fills it.
#[test]
fn a_runs_chain_is_its_parents_nearest_first() {
    let table = [(900, 800), (800, 700), (700, 1), (1, 0)];
    assert_eq!(run_chain(900, &table), [900, 800, 700, 0, 0, 0]);
    assert_eq!(
        run_chain(901, &table),
        [901, 0, 0, 0, 0, 0],
        "a run the table does not list is its own chain"
    );
    assert_eq!(run_chain(1, &table), [0; super::super::RUN_CHAIN]);
    let deep: Vec<(u32, u32)> = (10..20).map(|pid| (pid, pid - 1)).collect();
    assert_eq!(run_chain(19, &deep), [19, 18, 17, 16, 15, 14]);
    assert_eq!(
        run_chain(5, &[(5, 6), (6, 5)]),
        [5, 6, 5, 6, 5, 6],
        "bounded"
    );
}

#[test]
fn a_leftover_placeholder_without_a_live_installer_is_not_an_install() {
    let idle = super::super::Activity::Idle;
    assert_eq!(activity(true, std::iter::empty::<&str>()), idle);
    assert_eq!(
        activity(
            true,
            ["/System/Library/CoreServices/Software Update.app/Contents/Resources/softwareupdated"]
        ),
        idle
    );
    assert_eq!(activity(true, ["/tmp/softwareupdate"]), idle);
}

/// The card's Go to installer finds Apple's installer window alone, by the pid of the first
/// process whose executable is that app's own, as the page reads the window: never one of the
/// command-line installers, a program merely named like the app, the app's path when it is
/// not absolute, or a process the kernel names no executable for.
#[test]
fn go_to_installer_finds_apples_installer_window_alone() {
    let others = [
        (11, Some("/usr/sbin/softwareupdate")),
        (12, Some("/usr/sbin/installer")),
        (13, Some("/tmp/Install Command Line Developer Tools")),
        (
            14,
            Some(
                "Install Command Line Developer Tools.app/Contents/MacOS/Install Command Line Developer Tools",
            ),
        ),
        (15, None),
    ];
    assert_eq!(apple_window_pid(others), None);
    let open = others
        .into_iter()
        .chain([(16, Some(INSTALLER)), (17, Some(INSTALLER))]);
    assert_eq!(apple_window_pid(open), Some(16), "the first window's");
}

/// Only the tools' command-line installers' live evidence beside the placeholder makes the
/// wait calm, at any age. Apple's installer window, beside the placeholder it made as it
/// opened (pressed or not, minutes or days ago: nothing this Mac shows tells which) or with
/// none, is where the person finishes. Another program named like an installer is none.
#[test]
fn only_the_tools_installers_live_evidence_makes_the_wait_calm() {
    use super::super::Activity::{AppleWindow, Idle, Installing};
    assert_eq!(activity(true, ["/usr/sbin/softwareupdate"]), Installing);
    assert_eq!(activity(true, ["/usr/sbin/installer"]), Installing);
    assert_eq!(activity(false, ["/usr/sbin/installer"]), Idle);
    assert_eq!(activity(true, ["/usr/local/bin/installer"]), Idle);
    assert_eq!(activity(true, ["/tmp/installer"]), Idle);
    assert_eq!(activity(true, ["usr/sbin/installer"]), Idle);
    assert_eq!(activity(true, ["/usr/local/bin/softwareupdate"]), Idle);
    assert_eq!(
        activity(true, [INSTALLER]),
        AppleWindow,
        "beside the placeholder it made, at any age"
    );
    assert_eq!(
        activity(true, [INSTALLER, "/usr/sbin/softwareupdate"]),
        Installing,
        "an install under way beside the window"
    );
    assert_eq!(activity(false, [INSTALLER]), AppleWindow);
    assert_eq!(activity(false, ["/usr/sbin/softwareupdate"]), Idle);
    assert_eq!(
        activity(
            false,
            ["/tmp/Install Command Line Developer Tools.app/Contents/MacOS/unrelated"]
        ),
        Idle
    );
    assert_eq!(
        activity(
            false,
            [
                "Install Command Line Developer Tools.app/Contents/MacOS/Install Command Line Developer Tools"
            ]
        ),
        Idle
    );
}
