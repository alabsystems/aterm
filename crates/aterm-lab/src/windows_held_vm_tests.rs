// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn artifact_get_requires_a_literal_absolute_file_and_new_absolute_output() {
    let args = |remote: &str, out: &str| vec![remote.into(), "--out".into(), out.into()];
    assert_eq!(
        parse_get(&args(
            r"C:\lane\artifacts\test.exe",
            "/private/artifacts/new"
        ))
        .unwrap(),
        (
            "C:/lane/artifacts/test.exe".into(),
            PathBuf::from("/private/artifacts/new")
        )
    );
    for remote in [
        "relative.exe",
        "C:relative.exe",
        "//server/share/file",
        "C:/lane/*.exe",
        "C:/lane/file\n.exe",
        "C:/lane/file:stream",
    ] {
        assert!(
            parse_get(&args(remote, "/private/artifacts/new")).is_err(),
            "{remote}"
        );
    }
    assert!(parse_get(&args("C:/lane/file", "relative")).is_err());
    assert!(parse_get(&[]).is_err());
}

#[test]
fn held_identity_requires_every_observed_component() {
    let held = HeldVm {
        pid: 42,
        birth: aterm_uds::process::ProcessBirth {
            seconds: 123,
            microseconds: 456,
        },
        plan: "d15bf16566021279".into(),
    };
    let actual = Some(DaemonProcess {
        pid: held.pid,
        birth: held.birth,
    });
    let mut booter = crate::vm_admit::Booter::me("test", Path::new("/test"));
    booter.kept = true;
    booter.vm_pid = Some(42);
    booter.vm_birth = Some((123, 456));
    assert!(held.matches(actual, &booter, Some(&held.plan)));
    assert!(!held.matches(None, &booter, Some(&held.plan)));
    assert!(!held.matches(actual, &booter, None));
    assert!(!held.matches(actual, &booter, Some("0000000000000000")));
    for changed in [
        DaemonProcess {
            pid: 43,
            birth: held.birth,
        },
        DaemonProcess {
            pid: 42,
            birth: aterm_uds::process::ProcessBirth {
                seconds: 124,
                microseconds: 456,
            },
        },
    ] {
        assert!(!held.matches(Some(changed), &booter, Some(&held.plan)));
    }
    booter.vm_birth = None;
    assert!(!held.matches(actual, &booter, Some(&held.plan)));
    booter.vm_birth = Some((123, 456));
    booter.vm_pid = None;
    assert!(!held.matches(actual, &booter, Some(&held.plan)));
    booter.vm_pid = Some(42);
    booter.kept = false;
    assert!(!held.matches(actual, &booter, Some(&held.plan)));
}

#[test]
fn attach_custody_arguments_are_mandatory_and_exact() {
    let args = |s: &str| s.split_whitespace().map(str::to_owned).collect::<Vec<_>>();
    let good = args("--pid 42 --birth 123:456 --plan d15bf16566021279 exec cargo test");
    assert!(parse_attach(&good).is_ok());
    for bad in [
        "",
        "--pid 42 exec cargo test",
        "--pid 1 --birth 123:456 --plan d15bf16566021279 exec x",
        "--pid 42 --birth 123:1000000 --plan d15bf16566021279 exec x",
        "--pid 42 --birth 123:456 --plan unknown exec x",
        "--pid 42 --birth 123:456 --plan d15bf16566021279",
    ] {
        assert!(parse_attach(&args(bad)).is_err(), "{bad}");
    }
}

#[test]
fn attaching_unknown_layout_cannot_create_or_recover_it() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    let layout = Layout::under(root.path());
    let held = HeldVm {
        pid: 42,
        birth: aterm_uds::process::ProcessBirth {
            seconds: 123,
            microseconds: 456,
        },
        plan: "d15bf16566021279".into(),
    };
    assert!(attach_existing(layout.clone(), &held).is_err());
    assert!(!layout.home.exists());
    assert!(!layout.cache.exists());
    // Even an existing layout containing an unrelated sentinel stays intact.
    std::fs::create_dir_all(&layout.home).unwrap();
    std::fs::write(layout.home.join("preserve"), b"existing held data").unwrap();
    assert!(attach_existing(layout.clone(), &held).is_err());
    assert_eq!(
        std::fs::read(layout.home.join("preserve")).unwrap(),
        b"existing held data"
    );
    assert!(!layout.run().exists());
}
