// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read-only evidence of Apple's Command Line Tools installer on this Mac
//! ([`super::Activity`]).

#[cfg(any(target_os = "macos", all(test, unix)))]
use std::path::Path;

/// The [`super::Reading`] with the placeholder there or not (`placeholder`), last changed at
/// `placeholder_changed` (Unix seconds; `None` where unread), over the processes running, each a
/// pid and its executable's path, read as `aterm init` and the root half read them
/// ([`aterm_types::macos_setup::tools_installer`]) at `now` (Unix seconds): the exact paths of
/// Apple's `softwareupdate` and `installer` ([`is_install_process`]) hold the on-demand
/// placeholder while installing; an absolute path ending in Apple's installer app's own
/// executable identifies its window, which installs only while it, from the start `started`
/// reads of it, or the placeholder is younger than
/// [`aterm_types::macos_setup::APPLE_WINDOW_INSTALL_BOUND`]
/// ([`aterm_types::macos_setup::ToolsInstaller::installs`]). So [`super::activity`] reads an
/// install exactly where [`aterm_types::macos_setup::tools_install_running`] does.
#[cfg(any(target_os = "macos", all(test, unix)))]
fn reading_from(
    placeholder: bool,
    placeholder_changed: Option<u64>,
    processes: impl IntoIterator<Item = (u32, impl AsRef<Path>)>,
    started: impl Fn(u32) -> Option<u64>,
    now: u64,
) -> super::Reading {
    use aterm_types::macos_setup::{ToolsInstaller, tools_installer};
    let mut reading = super::Reading {
        placeholder,
        ..super::Reading::default()
    };
    for (pid, path) in processes {
        match tools_installer(path.as_ref(), is_install_process) {
            Some(ToolsInstaller::CommandLine) => reading.install_process = true,
            Some(window @ ToolsInstaller::AppleWindow) => {
                reading.apple_window = true;
                reading.apple_window_young |=
                    window.installs(started(pid), placeholder_changed, now);
            }
            None => {}
        }
    }
    reading
}

/// Whether `path` is one of Apple's command-line installers: compared, never run.
#[cfg(any(target_os = "macos", all(test, unix)))]
fn is_install_process(path: &::std::path::Path) -> bool {
    path == ::std::path::Path::new("/usr/sbin/softwareupdate")
        || path == ::std::path::Path::new("/usr/sbin/installer")
}

/// This Mac's [`super::Reading`] now: the placeholder as one `lstat` finds it, which follows no
/// link, as `aterm init` reads it (there, and its status change time, `st_ctime`), each
/// process's start as the kernel recorded it, and the clock (a clock that cannot be read is `0`,
/// before every start and change, so each window reads young).
#[cfg(target_os = "macos")]
pub(super) fn installing_now() -> super::Reading {
    use std::os::unix::fs::MetadataExt as _;
    let path = std::path::Path::new(aterm_types::macos_setup::COMMAND_LINE_TOOLS_INSTALLING);
    let placeholder = std::fs::symlink_metadata(path).ok();
    let changed = placeholder
        .as_ref()
        .and_then(|meta| u64::try_from(meta.ctime()).ok());
    let started = |pid| aterm_sysprobe::kproc::record(pid).map(|record| record.start_s);
    let now = aterm_time::unix_secs().unwrap_or(0);
    let mut reading = reading_from(placeholder.is_some(), changed, processes(), started, now);
    reading.live_run = stamp_of(path, 0).is_some_and(|stamp| {
        aterm_types::macos_setup::placeholder_held_by_a_live_run(&stamp, started)
    });
    reading
}

/// The stamp in the placeholder at `path`, as `aterm init` reads it: a regular file `owner`
/// (root, whose root half alone writes a run's stamp) owns and no group or other may write, no
/// longer than a stamp ([`aterm_types::macos_setup::PLACEHOLDER_STAMP_MAX`]), read without
/// following a link or waiting on a pipe; `None` for an empty one (Apple's own) or any other.
#[cfg(any(target_os = "macos", all(test, unix)))]
fn stamp_of(path: &Path, owner: u32) -> Option<Vec<u8>> {
    use std::io::Read as _;
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
    let max = aterm_types::macos_setup::PLACEHOLDER_STAMP_MAX;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.uid() != owner || meta.mode() & 0o6022 != 0 {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(max as u64 + 1).read_to_end(&mut bytes).ok()?;
    (!bytes.is_empty() && bytes.len() <= max).then_some(bytes)
}

#[cfg(not(target_os = "macos"))]
pub(super) fn installing_now() -> super::Reading {
    super::Reading::default()
}

/// Every process running now with the executable the kernel names for it, as a pid and a path.
#[cfg(target_os = "macos")]
fn processes() -> impl Iterator<Item = (u32, std::path::PathBuf)> {
    process_ids()
        .into_iter()
        .filter_map(|pid| match aterm_uds::holders::exe_of(pid) {
            aterm_uds::holders::Exe::At(path) => Some((pid, path)),
            _ => None,
        })
}

#[cfg(target_os = "macos")]
fn process_ids() -> Vec<u32> {
    // SAFETY: a null buffer asks for the count only.
    let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    let Ok(count) = usize::try_from(count) else {
        return Vec::new();
    };
    let mut pids = vec![0_i32; count + 64];
    let Ok(bytes) = libc::c_int::try_from(pids.len() * std::mem::size_of::<i32>()) else {
        return Vec::new();
    };
    // SAFETY: `pids` is `bytes` writable bytes.
    let found = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
    pids.truncate(usize::try_from(found).unwrap_or(0).min(pids.len()));
    pids.into_iter()
        .filter_map(|pid| u32::try_from(pid).ok().filter(|pid| *pid > 0))
        .collect()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// The clock these readings are taken at.
    const NOW: u64 = 1_800_000_000;

    /// How long Apple's window beside the placeholder counts as an install.
    const BOUND: u64 = aterm_types::macos_setup::APPLE_WINDOW_INSTALL_BOUND.as_secs();

    /// [`super::super::activity`] at [`NOW`] over `paths`, each a process that started `age`
    /// seconds before, beside the placeholder (when `placeholder`) last changed `changed`
    /// seconds before (`None`: an age that cannot be read).
    fn activity_aged(
        placeholder: bool,
        paths: impl IntoIterator<Item = impl AsRef<Path>>,
        age: Option<u64>,
        changed: Option<u64>,
    ) -> super::super::Activity {
        super::super::activity(reading_from(
            placeholder,
            changed.map(|changed| NOW - changed),
            (1..).zip(paths),
            |_| age.map(|age| NOW - age),
            NOW,
        ))
    }

    /// [`activity_aged`] of processes that started a minute ago, beside a placeholder changed as
    /// long ago.
    fn activity(
        placeholder: bool,
        paths: impl IntoIterator<Item = impl AsRef<Path>>,
    ) -> super::super::Activity {
        activity_aged(placeholder, paths, Some(60), Some(60))
    }

    const INSTALLER: &str = "/System/Library/CoreServices/Install Command Line Developer Tools.app/Contents/MacOS/Install Command Line Developer Tools";

    /// The page reads an install exactly where `aterm init` does: the one predicate
    /// ([`aterm_types::macos_setup::tools_install_running`]) over the same processes and their
    /// ages and the placeholder's, or a placeholder a live run's stamp names
    /// ([`aterm_types::macos_setup::placeholder_held_by_a_live_run`], the one reader both share):
    /// a run between its steps, or waiting on Apple's installer window a person closed, with no
    /// installer process running.
    #[test]
    fn the_page_reads_an_install_where_init_and_the_root_half_do() {
        for placeholder in [false, true] {
            for live_run in [false, true] {
                let ages = [Some(60), Some(BOUND - 1), Some(BOUND), None];
                for (age, changed) in ages.into_iter().flat_map(|a| ages.map(|c| (a, c))) {
                    for paths in [
                        &[][..],
                        &["/usr/sbin/softwareupdate"],
                        &["/usr/sbin/installer"],
                        &[INSTALLER],
                        &["/bin/zsh", "/usr/sbin/installer"],
                        &["/tmp/softwareupdate", INSTALLER],
                    ] {
                        let started = |_| age.map(|age| NOW - age);
                        let changed_at = changed.map(|changed| NOW - changed);
                        let processes = || (1..).zip(paths.iter().copied());
                        let mut reading =
                            reading_from(placeholder, changed_at, processes(), started, NOW);
                        reading.live_run = live_run;
                        let init = placeholder
                            && (aterm_types::macos_setup::tools_install_running(
                                true,
                                changed_at,
                                processes(),
                                is_install_process,
                                started,
                                NOW,
                            ) || live_run);
                        assert_eq!(
                            super::super::activity(reading) == super::super::Activity::Installing,
                            init,
                            "{placeholder} {live_run} {age:?} {changed:?} {paths:?}"
                        );
                    }
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

    #[test]
    fn a_leftover_placeholder_without_a_live_installer_is_not_an_install() {
        let idle = super::super::Activity::Idle;
        assert_eq!(activity(true, std::iter::empty::<&str>()), idle);
        assert_eq!(
            activity(
                true,
                [
                    "/System/Library/CoreServices/Software Update.app/Contents/Resources/softwareupdated"
                ]
            ),
            idle
        );
        assert_eq!(activity(true, ["/tmp/softwareupdate"]), idle);
    }

    /// Only the tools installers' live evidence makes the wait calm, Apple's installer window
    /// beside the placeholder among it while the window or the placeholder is younger than its
    /// bound (or of an age nothing can read): a person pressed Install there, perhaps hours
    /// after the window opened, and the press created the placeholder. That window with no
    /// placeholder yet, before its install starts, or left open past the bound beside a
    /// placeholder an ended install left as long ago, waits for the person's press. Another
    /// program named like an installer is none.
    #[test]
    fn only_the_tools_installers_live_evidence_makes_the_wait_calm() {
        use super::super::Activity::{AppleWindow, Idle, Installing};
        assert_eq!(activity(true, ["/usr/sbin/softwareupdate"]), Installing);
        assert_eq!(activity(true, ["/usr/sbin/installer"]), Installing);
        assert_eq!(
            activity_aged(true, ["/usr/sbin/installer"], Some(BOUND), Some(BOUND)),
            Installing,
            "at any age"
        );
        assert_eq!(activity(false, ["/usr/sbin/installer"]), Idle);
        assert_eq!(activity(true, ["/usr/local/bin/installer"]), Idle);
        assert_eq!(activity(true, ["/tmp/installer"]), Idle);
        assert_eq!(activity(true, ["usr/sbin/installer"]), Idle);
        assert_eq!(activity(true, ["/usr/local/bin/softwareupdate"]), Idle);
        assert_eq!(activity(true, [INSTALLER]), Installing, "a press's install");
        assert_eq!(
            activity_aged(true, [INSTALLER], Some(BOUND - 1), Some(BOUND)),
            Installing
        );
        assert_eq!(
            activity_aged(true, [INSTALLER], None, Some(BOUND)),
            Installing
        );
        assert_eq!(
            activity_aged(true, [INSTALLER], Some(BOUND), Some(60)),
            Installing,
            "a press hours after the window opened: the placeholder it created is young"
        );
        assert_eq!(
            activity_aged(true, [INSTALLER], Some(BOUND), Some(BOUND - 1)),
            Installing
        );
        assert_eq!(
            activity_aged(true, [INSTALLER], Some(BOUND), None),
            Installing
        );
        assert_eq!(
            activity_aged(true, [INSTALLER], Some(BOUND), Some(BOUND)),
            AppleWindow,
            "a window left open beside a leftover placeholder as old"
        );
        assert_eq!(activity(false, [INSTALLER]), AppleWindow);
        assert_eq!(
            activity_aged(false, [INSTALLER], Some(BOUND), None),
            AppleWindow
        );
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
}
