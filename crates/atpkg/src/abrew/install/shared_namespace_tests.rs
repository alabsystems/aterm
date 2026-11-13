// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn project_rejects_replaced_projection_destination() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    let outside = tmp.path().join("outside");
    let staging = tmp.path().join("staging");
    for path in [&source, &outside, &staging] {
        std::fs::create_dir(path).unwrap();
    }
    std::fs::write(source.join("leaf"), b"source bytes").unwrap();
    std::fs::write(outside.join("keep"), b"user bytes").unwrap();
    let fd = open_directory(tmp.path(), &source).unwrap();
    let mut injected = false;
    let parent = open_directory(&staging, &staging).unwrap();
    let result = project_with(
        &source,
        &fd,
        &parent,
        c"payload",
        &staging.join("payload"),
        &mut |destination| {
            if !injected {
                std::fs::rename(destination, staging.join("held-projection")).unwrap();
                symlink_dir(&outside, destination).unwrap();
                injected = true;
            }
            Ok(())
        },
    );
    assert!(injected, "the actual post-mkdir boundary must execute");
    assert!(
        !outside.join("leaf").exists(),
        "projection wrote a real alias outside its staging directory"
    );
    assert_eq!(std::fs::read(outside.join("keep")).unwrap(), b"user bytes");
    assert!(result.is_err());
}
#[test]
fn activation_restores_old_alias_when_projection_is_substituted_before_exchange() {
    use std::os::unix::fs::MetadataExt as _;
    let tmp = aterm_tempfile::tempdir().unwrap();
    let prefix = Prefix::at(&tmp.path().join("prefix"));
    prefix.ensure().unwrap();
    let make = |name: &str| {
        let keg = prefix.keg(name, "1.0");
        std::fs::create_dir_all(keg.join("share/vim")).unwrap();
        std::fs::write(keg.join("share/vim").join(name), name).unwrap();
        prefix
            .write_receipt(
                &keg,
                &[
                    ("name", name),
                    ("version", "1.0"),
                    ("installed_by", "abrew"),
                ],
            )
            .unwrap();
        keg
    };
    let left = make("left");
    let right = make("right");
    link_keg(&prefix, &left, false).unwrap();
    let namespace = prefix.root.join("share/vim");
    let old_identity = std::fs::symlink_metadata(&namespace).unwrap();
    let outside = tmp.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("keep"), b"user bytes").unwrap();
    let mut injected = false;
    let result = with_shared_namespaces_with(
        &prefix,
        Some(&right),
        &mut |stage| {
            std::fs::rename(stage, stage.parent().unwrap().join("held-projection")).unwrap();
            symlink_dir(&outside, stage).unwrap();
            injected = true;
            Ok(())
        },
        || -> Result<(), InstallError> {
            panic!("substituted projection must never reach activation")
        },
    );
    assert!(injected);
    assert!(result.is_err());
    assert_eq!(
        std::fs::read_link(&namespace).unwrap(),
        left.join("share/vim")
    );
    assert_eq!(
        std::fs::symlink_metadata(&namespace).unwrap().ino(),
        old_identity.ino()
    );
    assert_eq!(std::fs::read(outside.join("keep")).unwrap(), b"user bytes");
    assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 1);
}
