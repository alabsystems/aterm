// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read-only evidence that Apple's Command Line Tools installer is running.

#[cfg(any(target_os = "macos", all(test, unix)))]
use std::path::Path;

#[cfg(any(target_os = "macos", all(test, unix)))]
const APP: &str =
    "Install Command Line Developer Tools.app/Contents/MacOS/Install Command Line Developer Tools";

#[cfg(any(target_os = "macos", all(test, unix)))]
fn installing_from(placeholder: bool, paths: impl IntoIterator<Item = impl AsRef<Path>>) -> bool {
    paths.into_iter().any(|path| {
        let path = path.as_ref();
        path.is_absolute() && (path.ends_with(APP) || (placeholder && is_softwareupdate(path)))
    })
}

#[cfg(any(target_os = "macos", all(test, unix)))]
fn is_softwareupdate(path: &::std::path::Path) -> bool {
    path == ::std::path::Path::new("/usr/sbin/softwareupdate")
}

#[cfg(target_os = "macos")]
pub(super) fn installing_now() -> bool {
    let placeholder =
        std::path::Path::new(aterm_types::macos_setup::COMMAND_LINE_TOOLS_INSTALLING).exists();
    installing_from(placeholder, process_paths())
}

#[cfg(not(target_os = "macos"))]
pub(super) fn installing_now() -> bool {
    false
}

#[cfg(target_os = "macos")]
fn process_paths() -> impl Iterator<Item = std::path::PathBuf> {
    process_ids()
        .into_iter()
        .filter_map(|pid| match aterm_uds::holders::exe_of(pid) {
            aterm_uds::holders::Exe::At(path) => Some(path),
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

    const INSTALLER: &str = "/System/Library/CoreServices/Install Command Line Developer Tools.app/Contents/MacOS/Install Command Line Developer Tools";

    #[test]
    fn a_leftover_placeholder_without_a_live_installer_is_not_an_install() {
        assert!(!installing_from(true, std::iter::empty::<&str>()));
        assert!(!installing_from(
            true,
            ["/System/Library/CoreServices/Software Update.app/Contents/Resources/softwareupdated"]
        ));
        assert!(!installing_from(true, ["/tmp/softwareupdate"]));
    }

    #[test]
    fn only_the_tools_installers_live_evidence_makes_the_wait_calm() {
        assert!(installing_from(true, ["/usr/sbin/softwareupdate"]));
        assert!(installing_from(false, [INSTALLER]));
        assert!(!installing_from(false, ["/usr/sbin/softwareupdate"]));
        assert!(!installing_from(
            false,
            ["/tmp/Install Command Line Developer Tools.app/Contents/MacOS/unrelated"]
        ));
        assert!(!installing_from(
            false,
            [
                "Install Command Line Developer Tools.app/Contents/MacOS/Install Command Line Developer Tools"
            ]
        ));
    }
}
