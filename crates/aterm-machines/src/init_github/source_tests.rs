// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_profile_memo_requires_the_read_descriptor_and_every_prefix_to_match() {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path().canonicalize().unwrap();
    let parent = home.join(".config/aterm");
    std::fs::create_dir_all(&parent).unwrap();
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = parent.join("init.toml");
    let other = parent.join("other-profile");
    for (path, bytes) in [
        (&path, b"[develop]\ncheckouts=[]\n".as_slice()),
        (&other, b"# other owned version\n".as_slice()),
    ] {
        std::fs::write(path, bytes).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let user = User {
        name: "fixture".into(),
        uid: path.metadata().unwrap().uid(),
        gid: 0,
        home: home.to_string_lossy().into_owned(),
        shell: String::new(),
        admin: false,
    };
    let sys = crate::port::Live::new();
    let before = PathFrame::read(&sys, &user, &path, INPUT_MAX);
    let read = sys.read_owned_file_observed(&path, user.uid, INPUT_MAX, false);
    let after = PathFrame::read(&sys, &user, &path, INPUT_MAX);
    let settled = sys.now().saturating_add(3);
    assert!(
        before.accepts(&read, &after, settled),
        "valid settled descriptor control"
    );
    assert!(
        !before.accepts(&read, &after, 0),
        "unsettled data never populate the memo"
    );
    // A different owned descriptor cannot borrow the pathname's later metadata,
    // including when a namespace changes away and back between path observations.
    let substituted = sys.read_owned_file_observed(&other, user.uid, INPUT_MAX, false);
    assert!(!before.accepts(&substituted, &after, settled));
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o750)).unwrap();
    let changed = PathFrame::read(&sys, &user, &path, INPUT_MAX);
    assert!(
        !before.accepts(&read, &changed, settled),
        "prefix admission is part of the key"
    );
}

#[test]
fn a_dangling_helper_keeps_its_declared_target_observable() {
    let root = aterm_tempfile::tempdir().unwrap();
    let root_path = root.path().canonicalize().unwrap();
    let home = root_path.join("home");
    let alias = root_path.join("alias/aterm");
    let user = User {
        name: "fixture".into(),
        uid: aterm_uds::peer::our_uid(),
        gid: 0,
        home: home.to_string_lossy().into_owned(),
        shell: String::new(),
        admin: false,
    };
    let [launcher, store] = aterm_types::installed_command::legacy(Path::new(&user.home));
    std::fs::create_dir_all(launcher.parent().unwrap()).unwrap();
    std::fs::create_dir_all(alias.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&alias, &launcher).unwrap();
    let sys = crate::port::Live::new();
    for package in ["first", "second"] {
        if package == "second" {
            std::fs::remove_file(&alias).unwrap();
        }
        let relative = PathBuf::from(format!("../{package}/bin/aterm"));
        std::os::unix::fs::symlink(&relative, &alias).unwrap();
        assert!(sys.canonicalize(&launcher).is_err(), "the target is absent");
        let paths = watch_paths(&sys, &user);
        assert!(paths.contains(&launcher));
        assert!(
            paths.contains(&store),
            "both absent-declaration locations are watched"
        );
        assert!(paths.contains(&alias));
        assert!(
            paths.contains(&alias.parent().unwrap().join(relative)),
            "arrival away from the unchanged launcher must stay observable: {paths:?}"
        );
    }
    // A missing directory behind a parent link has the same arrival boundary.
    std::fs::remove_file(&alias).unwrap();
    std::fs::remove_file(&launcher).unwrap();
    std::fs::remove_dir(launcher.parent().unwrap()).unwrap();
    let package = root_path.join("third/bin");
    std::os::unix::fs::symlink(&package, launcher.parent().unwrap()).unwrap();
    assert!(sys.canonicalize(&launcher).is_err());
    assert!(watch_paths(&sys, &user).contains(&package.join("aterm")));

    use aterm_types::installed_command as command;
    use std::os::unix::fs::PermissionsExt as _;
    let declaration = command::path(&home);
    std::fs::create_dir_all(declaration.parent().unwrap()).unwrap();
    let custom = root_path.join("custom bin/aterm");
    let payload = root_path.join("custom store/aterm");
    std::fs::create_dir_all(custom.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&payload, &custom).unwrap();
    let select = |target: &Path| {
        std::fs::write(
            &declaration,
            command::Selection::new(target.to_path_buf())
                .unwrap()
                .encode()
                .unwrap(),
        )
        .unwrap();
        std::fs::set_permissions(&declaration, std::fs::Permissions::from_mode(0o600)).unwrap();
    };
    select(&custom);
    let paths = watch_paths(&sys, &user);
    assert!(paths.contains(&declaration));
    assert!(paths.contains(&custom));
    assert!(
        paths.contains(&payload),
        "a custom dangling target can arrive"
    );
    assert!(
        !paths.contains(&launcher),
        "a declaration retires legacy locations"
    );
    let replacement = root_path.join("replacement store/aterm");
    select(&replacement);
    let paths = watch_paths(&sys, &user);
    assert!(paths.contains(&replacement));
    assert!(!paths.contains(&custom));
    assert!(!paths.contains(&payload), "the old selection retires");
    std::fs::write(&declaration, b"not a declaration").unwrap();
    let paths = watch_paths(&sys, &user);
    assert!(paths.contains(&declaration));
    assert!(!paths.contains(&launcher), "malformed does not fall back");
}
