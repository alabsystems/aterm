// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_agent::supervise::tape::store;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;

#[test]
fn rehoming_preserves_an_existing_home_and_initializes_only_a_new_one() {
    let mut world = World::new(CONFIG_OFF).expect("fresh private world");
    let original_home = world.home.clone();
    std::fs::write(original_home.join(".profile"), b"owner profile").unwrap();
    std::fs::write(original_home.join(".shrc"), b"owner shell configuration").unwrap();
    std::fs::set_permissions(&original_home, std::fs::Permissions::from_mode(0o750)).unwrap();
    assert!(world.rehome(&original_home).is_err());
    assert_eq!(world.home, original_home);
    assert_eq!(
        std::fs::read(original_home.join(".profile")).unwrap(),
        b"owner profile"
    );
    assert_eq!(
        std::fs::read(original_home.join(".shrc")).unwrap(),
        b"owner shell configuration"
    );
    assert_eq!(
        std::fs::metadata(&original_home)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o750
    );
    let new_home = world.dir.join("new-home");
    world.rehome(&new_home).expect("new private home");
    assert_eq!(world.home, new_home);
    assert_eq!(
        std::fs::read(new_home.join(".profile")).unwrap(),
        b"PS1='$ '\n"
    );
    for (name, value) in world.env() {
        if matches!(
            name.as_str(),
            "TMPDIR"
                | "TMP"
                | "TEMP"
                | "XDG_CACHE_HOME"
                | "XDG_DATA_HOME"
                | "XDG_STATE_HOME"
                | "ATERM_STATE_HOME"
        ) {
            let path = Path::new(&value);
            assert!(path.starts_with(&new_home), "{name} follows the new HOME");
            assert!(path.is_dir(), "{name} already exists");
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
    }
}

#[test]
fn a_new_world_publishes_a_home_that_its_hosted_store_can_admit() {
    let world = World::new(CONFIG_OFF).expect("fresh private world");
    let env = world.env();
    let home = PathBuf::from(
        &env.iter()
            .find(|(name, _)| name == "HOME")
            .expect("private HOME")
            .1,
    );
    assert_eq!(home, world.home);
    for name in [
        "TMPDIR",
        "TMP",
        "TEMP",
        "XDG_CACHE_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "ATERM_STATE_HOME",
    ] {
        let path = PathBuf::from(
            &env.iter()
                .find(|(key, _)| key == name)
                .expect("explicit private root")
                .1,
        );
        assert!(path.starts_with(&home), "{name} belongs to the owned HOME");
        assert!(path.is_dir(), "{name} already exists");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    let state = PathBuf::from(
        &env.iter()
            .find(|(name, _)| name == "ATERM_STATE_HOME")
            .unwrap()
            .1,
    );
    let root = store::store_under(&state);
    let session = store::session_dir(&root, Some("s-world"));
    std::fs::create_dir_all(&session).expect("fresh owned store");
    for dir in [&root, &session] {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .expect("private fixture directory");
    }
    let tape = session.join("owned.atape");
    let mut file = store::create(&tape).expect("new private tape");
    file.write_all(b"owned diagnostic bytes")
        .expect("fixture bytes");
    drop(file);
    let admitted = store::try_list(&root, None);
    assert!(
        admitted
            .as_ref()
            .is_ok_and(|tapes| tapes.len() == 1 && tapes[0].path == tape && tapes[0].bytes == 22),
        "a newly owned Lab World must publish a HOME whose hosted store admits its private tape: {admitted:?}"
    );
}
