// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_copy_is_named_by_its_keeper() {
    let birth = Birth {
        seconds: 1_790_000_000,
        micros: 42,
    };
    let name = copy_name(512, birth);
    assert_eq!(name, "512-1790000000-42");
    assert_eq!(owner_of(&name), Some((512, birth)));
    assert_eq!(owner_of(&format!("{name}.partial")), Some((512, birth)));
    for other in [
        "",
        "512",
        "512-1",
        "512-1-2-3",
        "x-1-2",
        ".DS_Store",
        "+512-1-2",
        "0512-1-2",
        "512-01-2",
        "512-1-02",
        "512-1-+2.partial",
    ] {
        assert_eq!(owner_of(other), None, "{other:?}");
    }
}

#[test]
fn the_sweep_removes_only_copies_whose_keeper_is_gone_and_ran() {
    let scratch = crate::test_scratch_dir("copies");
    let dir = scratch.path();
    let live = Birth {
        seconds: 100,
        micros: 1,
    };
    let names = [
        ("7-100-1", false, true, true),           // its keeper, pid 7, lives
        ("7-100-2", true, false, false),          // pid 7 is another process now
        ("8-100-1", true, false, false),          // pid 8 is gone
        ("9-100-1", false, true, false),          // pid 9 is gone, its copy never said it ran
        ("8-100-1.partial", false, false, false), // half made, its keeper gone
        ("7-100-1.partial", false, true, true),   // half made by a live keeper
        ("notes", false, true, true),             // not a copy
    ];
    for (name, ran, _, _) in names {
        std::fs::create_dir_all(dir.join(name).join("aterm.app")).expect("copy");
        if ran {
            mark_ran(&dir.join(name));
        }
    }
    let born = |pid| (pid == 7).then_some(live);
    let now = std::time::SystemTime::now();
    assert_eq!(sweep_at(dir, born, now), 3);
    for (name, _, kept, _) in names {
        assert_eq!(dir.join(name).exists(), kept, "{name}");
    }
    let later = now + aterm_uds::deadline::FIRST_EXEC_HANG;
    assert_eq!(sweep_at(dir, born, later), 1);
    for (name, _, _, kept) in names {
        assert_eq!(dir.join(name).exists(), kept, "{name}, later");
    }
}

/// On a volume that clones (APFS), `copy_tree` clones each file instead of writing its bytes, so
/// a keeper's copy of its code (600 MB in a debug build) waits on no disk throughput and takes
/// no free space: the copy carries its source's extended attribute, which no byte copy writes.
/// Negative control: a byte copy of the same file does not carry it.
#[cfg(target_vendor = "apple")]
#[test]
fn a_copied_file_is_a_clone_of_its_source() {
    let scratch = crate::test_scratch_dir("clone");
    let dir = scratch.path();
    let source = dir.join("code");
    std::fs::write(&source, vec![0x5a; 1 << 20]).expect("code");
    let (held, _) = aterm_uds::regular_file::open(&source).expect("open the code");
    if !aterm_uds::regular_file::clone_held(&held, &dir.join("probe")).expect("clone probe") {
        aterm_spec::host::could_not_run("this scratch volume cannot clone a file");
    }
    let xattr = |args: &[&str], path: &Path| {
        aterm_uds::deadline::output_until(
            aterm_uds::spawnfd::command("/usr/bin/xattr")
                .args(args)
                .arg(path),
            std::time::Instant::now() + std::time::Duration::from_secs(60),
        )
        .expect("xattr")
    };
    const NAME: &str = "com.aterm.keeper.clonetest";
    let set = xattr(&["-w", NAME, "1"], &source);
    assert!(set.status.success(), "{set:?}");
    let carries = |path: &Path| xattr(&["-p", NAME], path).status.success();
    let copy = dir.join("copy");
    copy_tree(&source, &copy).expect("copy");
    assert_eq!(
        std::fs::read(&copy).expect("the copy"),
        std::fs::read(&source).expect("the code")
    );
    assert!(carries(&copy), "the copy is no clone of its source");
    let bytes = dir.join("bytes");
    std::fs::write(&bytes, std::fs::read(&source).expect("the code")).expect("byte copy");
    assert!(!carries(&bytes), "a byte copy carries the attribute");
}

/// An ad-hoc-signed bundle (Info.plist and a resource sealed, an argv0 alias as a symbolic
/// link) copied by `copy_tree` passes `codesign --verify --strict`. Negative control: the copy
/// with its sealed resource changed does not.
#[cfg(target_vendor = "apple")]
#[test]
fn a_copied_bundle_keeps_its_signature() {
    aterm_uds::deadline::require_trust_service();
    let scratch = crate::test_scratch_dir("bundle");
    let dir = scratch.path();
    let app = dir.join("t.app");
    let macos = app.join("Contents").join("MacOS");
    std::fs::create_dir_all(&macos).expect("bundle");
    std::fs::create_dir_all(app.join("Contents").join("Resources")).expect("resources");
    std::fs::write(
        app.join("Contents").join("Info.plist"),
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\
         <key>CFBundleExecutable</key><string>t</string>\
         <key>CFBundleIdentifier</key><string>com.aterm.keeper.copytest</string>\
         </dict></plist>\n",
    )
    .expect("plist");
    std::fs::write(app.join("Contents").join("Resources").join("r.txt"), "r").expect("r");
    std::fs::copy("/usr/bin/true", macos.join("t")).expect("exe");
    std::os::unix::fs::symlink("t", macos.join("t-alias")).expect("alias");
    let codesign = |args: &[&str], path: &Path| {
        aterm_uds::deadline::output_until(
            aterm_uds::spawnfd::command("/usr/bin/codesign")
                .args(args)
                .arg(path),
            std::time::Instant::now() + std::time::Duration::from_secs(60),
        )
        .expect("codesign")
    };
    let sign = codesign(&["--force", "--sign", "-"], &app);
    assert!(sign.status.success(), "{sign:?}");
    let verify = |path: &Path| codesign(&["--verify", "--strict"], path);
    assert!(verify(&app).status.success(), "the original verifies");
    let copy = dir.join("copy").join("t.app");
    std::fs::create_dir_all(copy.parent().expect("parent")).expect("dir");
    copy_tree(&app, &copy).expect("copy");
    let checked = verify(&copy);
    assert!(checked.status.success(), "the copy: {checked:?}");
    assert!(
        std::fs::symlink_metadata(copy.join("Contents/MacOS/t-alias"))
            .expect("alias")
            .file_type()
            .is_symlink()
    );
    std::fs::write(copy.join("Contents").join("Resources").join("r.txt"), "x").expect("r");
    assert!(!verify(&copy).status.success(), "a changed copy verifies");
}

/// An XML `Info.plist` naming `version`, behind a comment that names another.
fn info_plist(version: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!-- <key>CFBundleShortVersionString</key>\n\
         <string>9.9.0</string> -->\n<plist version=\"1.0\">\n<dict>\n\t\
         <key>CFBundleShortVersionString</key>\n\t<string>{version}</string>\n</dict>\n</plist>\n"
    )
}

/// Another file at a keeper's executable path counts for a restart only when its bundle says it
/// is a release at least as new as the keeper: an older bundle, one with no `Info.plist`, one whose
/// version is no release version, and a keeper of no release version each read as
/// [`ImageLook::Older`], so a downgrade never restarts the keeper into an older build. A version
/// inside a comment is not read.
///
/// NEGATIVE CONTROL (run by hand, restored): with `look` reading every other file as
/// [`ImageLook::Replaced`], the older, bare, unversioned and unranked cases fail.
#[test]
fn a_replacement_counts_only_when_its_bundle_is_at_least_the_keepers_version() {
    use crate::core::ImageLook;
    let scratch = crate::test_scratch_dir("image");
    let contents = scratch.path().join("Contents");
    let macos = contents.join("MacOS");
    std::fs::create_dir_all(&macos).expect("a bundle");
    let exe = macos.join("aterm");
    std::fs::write(&exe, b"the build the keeper started as").expect("write");
    let plist = contents.join("Info.plist");
    std::fs::write(&plist, info_plist("0.206.0")).expect("write");
    let image = ExeImage::at(&exe).expect("it reads").serving("0.206.0");
    assert_eq!(image.look(), ImageLook::Same);
    let next = exe.with_extension("next");
    std::fs::write(&next, b"the build an update put there").expect("write");
    std::fs::rename(&next, &exe).expect("rename over it");
    let replaced = ImageLook::Replaced(exe.display().to_string());
    let older = ImageLook::Older(exe.display().to_string());
    for (version, look) in [
        ("0.207.0", &replaced),
        ("0.206.0", &replaced),
        ("1.20261117.1", &replaced),
        ("0.205.0", &older),
        ("0.206.0+gabcdef", &older),
        ("", &older),
    ] {
        std::fs::write(&plist, info_plist(version)).expect("write");
        assert_eq!(&image.look(), look, "a bundle of {version:?}");
    }
    std::fs::write(&plist, info_plist("0.207.0")).expect("write");
    assert_eq!(
        ExeImage::at(&exe)
            .expect("it reads")
            .serving("0.206.0")
            .look(),
        ImageLook::Same,
        "a keeper started as this file"
    );
    let mut unranked = image.clone().serving("test");
    assert_eq!(unranked.look(), older, "a keeper of no release version");
    unranked = unranked.serving("0.206.0");
    assert_eq!(unranked.look(), replaced);
    std::fs::remove_file(&plist).expect("remove");
    assert_eq!(image.look(), older, "a bundle with no Info.plist");
    let bare = scratch.path().join("aterm");
    std::fs::write(&bare, b"a bare build").expect("write");
    std::fs::write(scratch.path().join("Info.plist"), info_plist("0.207.0")).expect("write");
    assert_eq!(
        bundle_version(&bare),
        None,
        "an executable outside a bundle"
    );
}
