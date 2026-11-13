// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::resolve_check;
use crate::store::{FIRST_EXEC_MARKS_SUFFIX, held_unproven, sidecar_path};
use std::path::{Path, PathBuf};

fn build_with(root: &Path, name: &str, script: impl AsRef<[u8]>, mode: u32) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let bin = root.join("store/tool").join(name).join("bin").join("tool");
    std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
    std::fs::write(&bin, script).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(mode)).unwrap();
    bin
}

/// The build is held from before the spawn: an exit of the program's own releases it, a
/// signal death leaves it held for every process, and a spawn the kernel refused holds
/// nothing. A signal death and the kernel's refusal of what the build shipped (no execute
/// bit, an image it will not execute, which is not run as a shell script instead) are
/// verdicts on the build; a hold that cannot be written is not.
#[test]
fn a_resolve_check_holds_its_build_until_the_program_exited_on_its_own() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = aterm_tempfile::Builder::new()
        .prefix("atpkg-resolve-hold-")
        .tempdir()
        .unwrap();
    let build = |name: &str| dir.path().join("store/tool").join(name);
    let mark = |name: &str| sidecar_path(&build(name), FIRST_EXEC_MARKS_SUFFIX).unwrap();

    let ran = build_with(
        dir.path(),
        "1",
        format!(
            "#!/bin/sh\ntest -d '{}' || kill -KILL $$\n",
            mark("1").display()
        ),
        0o755,
    );
    assert_eq!(
        resolve_check(&ran),
        Ok(()),
        "the hold is on disk while it runs"
    );
    assert!(!mark("1").exists(), "an exit of its own releases it");
    assert!(!held_unproven(&build("1")));

    let killed = build_with(dir.path(), "2", "#!/bin/sh\nkill -KILL $$\n", 0o755);
    assert!(
        resolve_check(&killed).is_err_and(|u| u.verdict),
        "a signal death is a verdict on the build"
    );
    assert!(mark("2").is_dir(), "a signal death leaves its build held");
    assert!(held_unproven(&build("2")));

    let refused = build_with(dir.path(), "3", "#!/bin/sh\nexit 0\n", 0o644);
    assert!(
        resolve_check(&refused).is_err_and(|u| u.verdict),
        "so is a spawn the kernel refused for its bytes"
    );
    assert!(
        !mark("3").exists(),
        "a program that never started holds nothing"
    );

    let garbage = build_with(dir.path(), "5", [0xcf, 0xfa, 0xed, 0xfe, 0, 0, 0, 0], 0o755);
    assert!(
        resolve_check(&garbage).is_err_and(|u| u.verdict && u.why.contains("format")),
        "an executable image the kernel will not execute is refused, not run by a shell"
    );

    // A hold that cannot be written (here, a program folder it may not write) is a fault of
    // this run, not a verdict on the build.
    let ro = dir.path().join("ro");
    let unheld = build_with(&ro, "4", "#!/bin/sh\nexit 0\n", 0o755);
    let folder = ro.join("store/tool");
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o555)).unwrap();
    let got = resolve_check(&unheld);
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(got.is_err_and(|u| !u.verdict && u.why.contains("cannot hold")));
}

/// A refusal is a verdict on the build only for what it shipped: `ENOEXEC`, or `EACCES` on a
/// file with no execute bit. NEGATIVE CONTROL: `EACCES` on a file that may be executed (a
/// `noexec` mount, a policy) and `EPERM` (an execution policy) are the machine's.
#[test]
fn a_refusal_is_a_verdict_only_for_what_the_build_shipped() {
    use super::refused_by_its_bytes;
    let dir = aterm_tempfile::Builder::new()
        .prefix("atpkg-resolve-bytes-")
        .tempdir()
        .unwrap();
    let shipped_plain = build_with(dir.path(), "1", "#!/bin/sh\n", 0o644);
    let executable = build_with(dir.path(), "2", "#!/bin/sh\n", 0o755);
    let errno = std::io::Error::from_raw_os_error;
    assert!(refused_by_its_bytes(&errno(libc::ENOEXEC), &executable));
    assert!(refused_by_its_bytes(&errno(libc::EACCES), &shipped_plain));
    assert!(!refused_by_its_bytes(&errno(libc::EACCES), &executable));
    assert!(!refused_by_its_bytes(&errno(libc::EPERM), &shipped_plain));
    assert!(!refused_by_its_bytes(&errno(libc::EAGAIN), &shipped_plain));
}
