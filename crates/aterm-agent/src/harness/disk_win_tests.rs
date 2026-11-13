// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// Each hand-declared structure has the layout the Windows SDK gives it.
#[test]
fn the_declared_structures_have_the_sdks_layout() {
    assert_eq!(size_of::<ReparseGuidDataHeader>(), 24);
    assert_eq!(std::mem::offset_of!(ReparseGuidDataHeader, data_length), 4);
    assert_eq!(std::mem::offset_of!(ReparseGuidDataHeader, guid), 8);
    assert_eq!(size_of::<FileStandardInfo>(), 24);
    assert_eq!(std::mem::offset_of!(FileStandardInfo, end_of_file), 8);
    assert_eq!(std::mem::offset_of!(FileStandardInfo, number_of_links), 16);
    assert_eq!(std::mem::offset_of!(FileStandardInfo, delete_pending), 20);
    assert_eq!(std::mem::offset_of!(FileStandardInfo, directory), 21);
    assert_eq!(size_of::<FileBasicInfo>(), 40);
    assert_eq!(std::mem::offset_of!(FileBasicInfo, change_time), 24);
    assert_eq!(std::mem::offset_of!(FileBasicInfo, file_attributes), 32);
    assert_eq!(size_of::<FileDispositionInfoEx>(), 4);
    assert_eq!(size_of::<FileDispositionInfo>(), 1);
}

/// The registry's record of a system folder is expanded from the system
/// directory alone: `%SystemDrive%` is its drive and `%SystemRoot%` /
/// `%windir%` the folder above it, in any case; a record with no variable
/// is read as written.
#[test]
fn a_stored_folder_is_expanded_from_the_system_directory_alone() {
    let system = std::path::Path::new(r"D:\WINDOWS\system32");
    let expand = |s: &str| expand_system(s, system);
    let at = |s: &str| Some(std::path::PathBuf::from(s));
    assert_eq!(expand(r"%SystemDrive%\ProgramData"), at(r"D:\ProgramData"));
    assert_eq!(expand(r"%systemdrive%\ProgramData"), at(r"D:\ProgramData"));
    assert_eq!(expand(r"%SystemRoot%\Data"), at(r"D:\WINDOWS\Data"));
    assert_eq!(expand(r"%WINDIR%\Data"), at(r"D:\WINDOWS\Data"));
    assert_eq!(
        expand(r"E:\Shared\ProgramData"),
        at(r"E:\Shared\ProgramData")
    );
    assert_eq!(expand(r"%ProgramData%"), None);
    assert_eq!(expand(r"%USERPROFILE%\ProgramData"), None);
    assert_eq!(expand(r"%SystemDrive\ProgramData"), None);
    assert_eq!(expand("ProgramData"), None);
}

/// The system's folders are read from the system, not the environment
/// this process was given: an absolute System32 that holds the system's
/// own `kernel32.dll`, and an absolute ProgramData folder that exists.
#[test]
fn the_system_folders_are_read_from_the_system() {
    let system = system_directory().expect("the system directory");
    assert!(system.is_absolute(), "{system:?}");
    assert!(
        system
            .file_name()
            .is_some_and(|n| n.eq_ignore_ascii_case("System32")),
        "{system:?}"
    );
    assert!(system.join("kernel32.dll").is_file(), "{system:?}");
    let data = program_data().expect("ProgramData");
    assert!(data.is_absolute() && data.is_dir(), "{data:?}");
}

/// Set in the child [`the_system_folders_ignore_the_environment`] starts:
/// the file it writes its readings to, instead of running the test.
const FOLDERS_OUT: &str = "ATERM_DISK_WIN_FOLDERS_OUT";

/// The system's folders do not follow the environment a process was
/// handed: a child of this test whose `SystemRoot`, `windir`,
/// `SystemDrive`, `ProgramData` and `ALLUSERSPROFILE` all name a scratch
/// folder reads the very folders this process reads.
#[test]
fn the_system_folders_ignore_the_environment() {
    let text = |p: Option<std::path::PathBuf>| {
        p.map_or_else(String::new, |p| p.to_string_lossy().into_owned())
    };
    if let Some(out) = std::env::var_os(FOLDERS_OUT) {
        let handed = std::env::var("ProgramData").unwrap_or_default();
        let readings = format!(
            "{}\n{}\n{handed}\n",
            text(system_directory()),
            text(program_data())
        );
        std::fs::write(out, readings).expect("write the readings");
        return;
    }
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    let fake = scratch.path().join("NotTheSystem");
    std::fs::create_dir_all(fake.join("System32")).expect("a decoy");
    std::fs::create_dir_all(fake.join("ProgramData")).expect("a decoy");
    let out = scratch.path().join("folders.txt");
    let mut cmd = aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary"));
    cmd.args([
        "harness::disk::win::tests::the_system_folders_ignore_the_environment",
        "--exact",
        "--test-threads=1",
    ])
    .env(FOLDERS_OUT, &out);
    for var in [
        "SystemRoot",
        "windir",
        "SystemDrive",
        "ProgramData",
        "ALLUSERSPROFILE",
    ] {
        cmd.env(var, &fake);
    }
    let ran =
        crate::harness::align::capture_bounded(cmd, std::time::Duration::from_secs(60), 64 * 1024)
            .expect("the child runs and is reaped");
    assert!(ran.success(), "{}", String::from_utf8_lossy(&ran.stdout));
    let readings = std::fs::read_to_string(&out).expect("the child's readings");
    let lines: Vec<&str> = readings.lines().collect();
    let fake = fake.to_string_lossy();
    assert_eq!(
        lines,
        [
            text(system_directory()).as_str(),
            text(program_data()).as_str(),
            &*fake,
        ]
    );
}
