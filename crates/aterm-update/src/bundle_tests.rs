// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn a_probe_error_is_the_person_s_only_when_the_write_was_refused() {
    let parent = Path::new("/Applications");
    for errno in [libc::EACCES, libc::EPERM, libc::EROFS] {
        let error = std::io::Error::from_raw_os_error(errno);
        assert!(
            matches!(
                classify_probe_error(parent, &error),
                ParentWrite::NotWritable(_)
            ),
            "{error}"
        );
    }
    for errno in [
        libc::ENOSPC,
        libc::EDQUOT,
        libc::EIO,
        libc::EINTR,
        libc::EMFILE,
    ] {
        let error = std::io::Error::from_raw_os_error(errno);
        assert!(
            matches!(
                classify_probe_error(parent, &error),
                ParentWrite::Passing(_)
            ),
            "{error}"
        );
    }
}

/// A leftover probe is retried under a fresh name; an unwritable folder still says so.
#[test]
fn a_leftover_probe_name_is_probed_again_under_a_fresh_one() {
    use std::os::unix::fs::PermissionsExt;
    let root = crate::paths::scratch_dir("probe-parent");
    let app = root.join("aterm.app");
    std::fs::create_dir_all(&app).unwrap();
    let leftover = root.join(format!(".aterm-update-probe-{}", std::process::id()));
    std::fs::create_dir(&leftover).unwrap();
    assert_eq!(probe_parent_write(&app), ParentWrite::Writable);
    assert!(leftover.is_dir(), "the leftover is not ours to remove");
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o555)).unwrap();
    let refused = probe_parent_write(&app);
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        matches!(refused, ParentWrite::NotWritable(_)),
        "{refused:?}"
    );
}

/// The doctor and the updater must never disagree about the same install:
/// whatever `resolve_from` refuses to act on is exactly what `posture_from`
/// reports as unable to update, and only the two fixable states advise a move.
#[test]
fn posture_agrees_with_what_the_updater_will_act_on() {
    let b = resolve_from(Path::new("/Applications/aterm.app/Contents/MacOS/aterm")).unwrap();
    assert_eq!(b.app_root, Path::new("/Applications/aterm.app"));
    assert_eq!(
        b.exe,
        Path::new("/Applications/aterm.app/Contents/MacOS/aterm")
    );
    for (p, want) in [
        (
            "/Applications/aterm.app/Contents/MacOS/aterm",
            InstallPosture::Installed,
        ),
        (
            "/Users//ana/Applications/aterm.app/Contents/MacOS/aterm",
            InstallPosture::Installed,
        ),
        (
            "/Volumes/aterm 0.67.0/aterm.app/Contents/MacOS/aterm",
            InstallPosture::MountedImage,
        ),
        (
            "/Volumes/Ext/Applications/aterm.app/Contents/MacOS/aterm",
            InstallPosture::MountedImage,
        ),
        (
            "/private/var/folders/zz/AppTranslocation/ABC/d/aterm.app/Contents/MacOS/aterm",
            InstallPosture::Translocated,
        ),
        (
            "/Volumes/x/private/var/AppTranslocation/ABC/d/aterm.app/Contents/MacOS/aterm",
            InstallPosture::Translocated,
        ),
        (
            "/Users//ana/aterm/target/release/aterm",
            InstallPosture::NotABundle,
        ),
        ("/usr/local/bin/aterm", InstallPosture::NotABundle),
        ("/tmp/aterm", InstallPosture::NotABundle),
    ] {
        let path = Path::new(p);
        assert_eq!(posture_from(path), want, "{p}");
        assert_eq!(
            resolve_from(path).is_some(),
            want == InstallPosture::Installed,
            "the updater and the doctor disagree about {p}"
        );
        assert_eq!(
            want.remedy().is_some(),
            matches!(
                want,
                InstallPosture::MountedImage | InstallPosture::Translocated
            ),
            "{p}"
        );
    }
}

#[test]
fn dev_mark_reads_only_an_explicit_true() {
    let plist = |body: &str| {
        format!("<plist><dict><key>CFBundleVersion</key><string>7</string>{body}</dict></plist>")
    };
    assert!(plist_marks_dev_build(&plist(
        "<key>ATermDevBuild</key><string>true</string>"
    )));
    assert!(plist_marks_dev_build(&plist(
        "<key>ATermDevBuild</key>\n\t<string>TRUE</string>"
    )));
    assert!(!plist_marks_dev_build(&plist("")));
    assert!(!plist_marks_dev_build(&plist(
        "<key>ATermDevBuild</key><string>false</string>"
    )));
    assert!(!plist_marks_dev_build(&plist(
        "<key>ATermDevBuild</key><string></string>"
    )));
    assert!(!plist_marks_dev_build("\u{0}bplist00 binary garbage"));
    assert!(!plist_marks_dev_build(
        "<plist><dict><key>ATermDevBuild</key><key>Other</key><string>true</string></dict></plist>"
    ));
}

#[test]
fn the_chunked_grant_is_declared_only_by_its_exact_token() {
    let plist = |body: &str| {
        format!("<plist><dict><key>CFBundleVersion</key><string>7</string>{body}</dict></plist>")
    };
    assert!(plist_declares_grant_chunks(&plist(
        "<key>ATermHandoffGrantChunks</key>\n\t<string>chunks1</string>"
    )));
    for undeclared in [
        "",
        "<key>ATermHandoffGrantChunks</key><string></string>",
        "<key>ATermHandoffGrantChunks</key><string>chunks2</string>",
        "<key>ATermHandoffGrantChunks</key><string>true</string>",
        "<key>ATermHandoffGrantChunks</key><key>Other</key><string>chunks1</string>",
    ] {
        assert!(
            !plist_declares_grant_chunks(&plist(undeclared)),
            "{undeclared:?} declares nothing"
        );
    }
    assert!(!plist_declares_grant_chunks("\u{0}bplist00 binary garbage"));
}

/// This build reads chunked grants, so the bundle it ships in declares them.
#[test]
fn the_shipped_info_plist_declares_the_chunked_grant() {
    let template = include_str!("../../../apps/aterm-mac/Info.plist");
    assert!(plist_declares_grant_chunks(template));
}

#[test]
fn the_sessionless_grant_is_declared_only_by_its_exact_token() {
    let plist = |body: &str| {
        format!("<plist><dict><key>CFBundleVersion</key><string>7</string>{body}</dict></plist>")
    };
    assert!(plist_declares_grant_empty(&plist(
        "<key>ATermHandoffGrantEmpty</key>\n\t<string>empty1</string>"
    )));
    for undeclared in [
        "",
        "<key>ATermHandoffGrantEmpty</key><string></string>",
        "<key>ATermHandoffGrantEmpty</key><string>empty2</string>",
        "<key>ATermHandoffGrantEmpty</key><string>true</string>",
        "<key>ATermHandoffGrantEmpty</key><key>Other</key><string>empty1</string>",
        "<key>ATermHandoffGrantChunks</key><string>chunks1</string>",
    ] {
        assert!(
            !plist_declares_grant_empty(&plist(undeclared)),
            "{undeclared:?} declares nothing"
        );
    }
    assert!(!plist_declares_grant_empty("\u{0}bplist00 binary garbage"));
    let both = plist(
        "<key>ATermHandoffGrantChunks</key><string>chunks1</string>\
         <key>ATermHandoffGrantEmpty</key><string>empty1</string>",
    );
    assert!(plist_declares_grant_empty(&both) && plist_declares_grant_chunks(&both));
}

/// This build reads sessionless grants, so the bundle it ships in declares them.
#[test]
fn the_shipped_info_plist_declares_the_sessionless_grant() {
    let template = include_str!("../../../apps/aterm-mac/Info.plist");
    assert!(plist_declares_grant_empty(template));
}

#[test]
fn the_desk_grant_is_declared_only_by_its_exact_token() {
    let plist = |body: &str| {
        format!("<plist><dict><key>CFBundleVersion</key><string>7</string>{body}</dict></plist>")
    };
    assert!(plist_declares_grant_desk(&plist(
        "<key>ATermHandoffGrantDesk</key>\n\t<string>desk1</string>"
    )));
    for undeclared in [
        "",
        "<key>ATermHandoffGrantDesk</key><string></string>",
        "<key>ATermHandoffGrantDesk</key><string>desk2</string>",
        "<key>ATermHandoffGrantDesk</key><string>true</string>",
        "<key>ATermHandoffGrantDesk</key><key>Other</key><string>desk1</string>",
        "<key>ATermHandoffGrantEmpty</key><string>empty1</string>",
        "<key>ATermHandoffGrantEmpty</key><string>desk1</string>",
    ] {
        assert!(
            !plist_declares_grant_desk(&plist(undeclared)),
            "{undeclared:?} declares nothing"
        );
    }
    assert!(!plist_declares_grant_desk("\u{0}bplist00 binary garbage"));
}

/// This build rebuilds a lone Workers list, so the bundle it ships in declares it.
#[test]
fn the_shipped_info_plist_declares_the_desk_grant() {
    let template = include_str!("../../../apps/aterm-mac/Info.plist");
    assert!(plist_declares_grant_desk(template));
}
