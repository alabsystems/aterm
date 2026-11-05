// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read-only evidence of Apple's Command Line Tools installer on this Mac
//! ([`super::Activity`]).

#[cfg(any(target_os = "macos", all(test, unix)))]
use std::path::Path;

/// The [`super::Reading`] with the placeholder there or not (`placeholder`), over each running
/// process's executable path (`None` where the kernel names none), read as `aterm init` and the
/// root half read them ([`aterm_types::macos_setup::tools_installer`]): the exact paths of
/// Apple's `softwareupdate` and `installer` ([`is_install_process`]) hold the on-demand
/// placeholder while installing; an absolute path ending in Apple's installer app's own
/// executable identifies its window, whatever its age, with the placeholder it made as it opened
/// beside it or none; a process the kernel names no executable for is neither. So
/// [`super::activity`] reads an install, and Apple's window with or without the placeholder,
/// exactly where [`aterm_types::macos_setup::tools_held_by`] does.
#[cfg(any(target_os = "macos", all(test, unix)))]
fn reading_from(
    placeholder: bool,
    exes: impl IntoIterator<Item = Option<impl AsRef<Path>>>,
) -> super::Reading {
    use aterm_types::macos_setup::{ToolsInstaller, tools_installer};
    let mut reading = super::Reading {
        placeholder,
        ..super::Reading::default()
    };
    for exe in exes.into_iter().flatten() {
        match tools_installer(exe.as_ref(), is_install_process) {
            Some(ToolsInstaller::CommandLine) => reading.install_process = true,
            Some(ToolsInstaller::AppleWindow) => reading.apple_window = true,
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

/// This Mac's [`super::Reading`] now: the placeholder there or not as one `lstat` finds it,
/// which follows no link, as `aterm init` reads it, each process's executable as the kernel
/// names it, and the stamp a live run of the root half wrote, matched with its process's start
/// as the kernel recorded it.
#[cfg(target_os = "macos")]
pub(super) fn installing_now() -> super::Reading {
    let path = std::path::Path::new(aterm_types::macos_setup::COMMAND_LINE_TOOLS_INSTALLING);
    let placeholder = std::fs::symlink_metadata(path).is_ok();
    let started = |pid| aterm_sysprobe::kproc::record(pid).map(|record| record.start_s);
    let mut reading = reading_from(placeholder, processes());
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
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file()
        || meta.uid() != owner
        || meta.mode() & 0o6022 != 0
        || meta.len() == 0
        || meta.len() > max as u64
    {
        return None;
    }
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len().saturating_add(1)).unwrap_or(0));
    file.take(max as u64 + 1).read_to_end(&mut bytes).ok()?;
    (!bytes.is_empty() && bytes.len() <= max).then_some(bytes)
}

#[cfg(not(target_os = "macos"))]
pub(super) fn installing_now() -> super::Reading {
    super::Reading::default()
}

/// Every process running now, as the executable the kernel names for it (`None` where it names
/// none); one that exited before its executable was read is left out.
#[cfg(target_os = "macos")]
fn processes() -> impl Iterator<Item = Option<std::path::PathBuf>> {
    process_ids()
        .into_iter()
        .filter_map(|pid| match aterm_uds::holders::exe_of(pid) {
            aterm_uds::holders::Exe::At(path) => Some(Some(path)),
            aterm_uds::holders::Exe::Unnamed => Some(None),
            aterm_uds::holders::Exe::Exited => None,
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

    /// [`super::super::activity`] over `paths`, each a running process's executable, beside the
    /// placeholder when `placeholder`.
    fn activity(
        placeholder: bool,
        paths: impl IntoIterator<Item = impl AsRef<Path>>,
    ) -> super::super::Activity {
        super::super::activity(reading_from(placeholder, paths.into_iter().map(Some)))
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
                    let mut reading = reading_from(placeholder, paths.iter().copied());
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
}
