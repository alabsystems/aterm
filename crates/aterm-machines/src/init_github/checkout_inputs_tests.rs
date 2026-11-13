// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::port::{Fake, Stat};

fn fixture() -> (Fake, User) {
    let user = User {
        name: "fixture".into(),
        uid: 501,
        gid: 20,
        home: "/checkout-input-home".into(),
        shell: "/bin/zsh".into(),
        admin: false,
    };
    let mut sys = Fake::root();
    sys.euid = user.uid;
    sys.logged_in(&user);
    directory(&sys, &user, Path::new(&user.home));
    let path = inventory::file(&user.home);
    let bytes = br#"{"schema":2,"account":"fixture","repositories":[{"id":1,"name":"code","owner":{"login":"fixture"},"full_name":"fixture/code","clone_url":"https://github.com/fixture/code.git","private":true,"default_branch":"main","required_submodules":["vendor/library"]}]}"#;
    file(&sys, &user, &path, bytes);
    inventory::Inventory::read(&sys, &user).unwrap();
    (sys, user)
}

fn directory(sys: &Fake, user: &User, path: &Path) {
    // Fake's namespace listing is backed by its file entries; Stat supplies the actual
    // directory kind. Include both so the production fanout enumerator sees this entry.
    sys.file(path.to_str().unwrap(), b"").stat_of(
        path.to_str().unwrap(),
        Stat {
            uid: user.uid,
            gid: user.gid,
            mode: 0o700,
            dir: true,
            symlink: false,
            len: 0,
        },
    );
}

fn file(sys: &Fake, user: &User, path: &Path, bytes: &[u8]) {
    sys.file(path.to_str().unwrap(), bytes).stat_of(
        path.to_str().unwrap(),
        Stat {
            uid: user.uid,
            gid: user.gid,
            mode: 0o600,
            dir: false,
            symlink: false,
            len: bytes.len() as u64,
        },
    );
}

#[test]
fn selected_checkout_and_required_dependency_repairs_change_only_local_facts() {
    let (sys, user) = fixture();
    let home = Path::new(&user.home);
    let root = home.join("code");
    directory(&sys, &user, &root);
    let missing = Checkouts::capture(&sys, &user);
    let admin = root.join(".git");
    directory(&sys, &user, &admin);
    assert!(missing.observe(&sys) != missing.metadata());
    for path in [
        root.join("vendor"),
        root.join("vendor/library"),
        admin.join("modules"),
        admin.join("modules/library"),
        admin.join("modules/library/objects"),
        admin.join("modules/library/objects/aa"),
    ] {
        directory(&sys, &user, &path);
    }
    let dependency = root.join("vendor/library/.git");
    file(
        &sys,
        &user,
        &dependency,
        b"gitdir: ../../.git/modules/library\n",
    );
    file(
        &sys,
        &user,
        &admin.join("modules/library/objects/aa/object"),
        b"object",
    );
    let scope = Checkouts::capture(&sys, &user);
    assert!(
        scope
            .paths()
            .contains(&admin.join("modules/library/objects/aa"))
    );
    assert!(scope.paths().contains(&dependency));
    file(
        &sys,
        &user,
        &root.join("unrelated.txt"),
        b"working file changes are not readiness",
    );
    assert!(scope.observe(&sys) == scope.metadata());
    file(&sys, &user, &admin.join("config"), b"repaired origin");
    assert!(scope.observe(&sys) != scope.metadata());
    let scope = Checkouts::capture(&sys, &user);
    // The dependency's own Git configuration can be repaired without moving its
    // destination or gitfile, independently of the parent's origin repair.
    file(
        &sys,
        &user,
        &admin.join("modules/library/config"),
        b"repaired dependency origin",
    );
    assert!(scope.observe(&sys) != scope.metadata());
    assert!(sys.calls().iter().all(|call| call == "identity"));
}

#[test]
fn gitfiles_cannot_enlist_unselected_or_symlinked_private_directories() {
    let (sys, user) = fixture();
    let root = Path::new(&user.home).join("code");
    for path in [&root, &root.join("vendor"), &root.join("vendor/library")] {
        directory(&sys, &user, path);
    }
    let marker = root.join("vendor/library/.git");
    for pointer in [
        "/private/app-data",
        "../../../Library/private",
        "../../.git/../../Library/private",
    ] {
        file(
            &sys,
            &user,
            &marker,
            format!("gitdir: {pointer}\n").as_bytes(),
        );
        let scope = Checkouts::capture(&sys, &user);
        assert!(scope.paths().iter().all(|path| path.starts_with(&root)));
        assert!(
            !scope
                .paths()
                .iter()
                .any(|path| path.to_string_lossy().contains("app-data")
                    || path.to_string_lossy().contains("Library"))
        );
    }
    let admin = root.join(".git");
    directory(&sys, &user, &admin);
    sys.link(admin.join("modules").to_str().unwrap(), "/private/app-data");
    file(
        &sys,
        &user,
        &marker,
        b"gitdir: ../../.git/modules/library\n",
    );
    let scope = Checkouts::capture(&sys, &user);
    let before = scope.metadata();
    file(
        &sys,
        &user,
        Path::new("/private/app-data/library/config"),
        b"foreign",
    );
    assert!(scope.observe(&sys) == before);
    file(
        &sys,
        &user,
        &Path::new(&user.home).join(".config/aterm/init.toml"),
        b"[github]\nenabled=false\n",
    );
    // The canonical profile parser, rather than filesystem presence, chooses scope.
    assert!(matches!(
        profile(&sys, &user).and_then(|p| asked(p.and_then(|p| p.github), &user)),
        Ok(Asked::Off)
    ));
    assert!(Checkouts::capture(&sys, &user).paths().is_empty());
}

#[cfg(unix)]
#[test]
fn native_dependency_object_arrival_moves_cached_metadata_without_a_git_process() {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path().canonicalize().unwrap();
    let user = User {
        name: "fixture".into(),
        uid: std::fs::metadata(&home).unwrap().uid(),
        gid: 20,
        home: home.to_string_lossy().into_owned(),
        shell: String::new(),
        admin: false,
    };
    let private = inventory::file(home.to_str().unwrap());
    let checkout = home.join("code");
    let admin = checkout.join(".git/modules/library");
    for directory in [
        private.parent().unwrap(),
        &admin.join("objects/aa"),
        &checkout.join("vendor/library"),
    ] {
        std::fs::create_dir_all(directory).unwrap();
    }
    std::fs::write(&private, br#"{"schema":2,"account":"fixture","repositories":[{"id":1,"name":"code","owner":{"login":"fixture"},"full_name":"fixture/code","clone_url":"https://github.com/fixture/code.git","private":true,"default_branch":"main","required_submodules":["vendor/library"]}]}"#).unwrap();
    std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(
        checkout.join("vendor/library/.git"),
        b"gitdir: ../../.git/modules/library\n",
    )
    .unwrap();
    let sys = crate::port::Live::new();
    inventory::Inventory::read(&sys, &user).unwrap();
    let before = Checkouts::capture(&sys, &user);
    assert!(before.paths().contains(&admin.join("objects/aa")));
    std::fs::write(checkout.join("working.txt"), b"unrelated work").unwrap();
    assert!(before.observe(&sys) == before.metadata());
    // The object-storage repair producer changes an existing fanout, keeping the root,
    // gitfile, HEAD, origin, credentials and inventory unchanged.
    std::fs::write(admin.join("objects/aa/recovered"), b"object").unwrap();
    assert!(before.observe(&sys) != before.metadata());
    let repaired = Checkouts::capture(&sys, &user);
    assert!(repaired.observe(&sys) == repaired.metadata());
}

#[test]
fn a_53_repository_round_observes_each_distinct_component_once() {
    let (sys, user) = fixture();
    let home = Path::new(&user.home);
    let mut repositories = Vec::new();
    for number in 0..53 {
        let name = format!("code-{number}");
        repositories.push(aterm_json::json!({"id":number+1,"name":name,"owner":{"login":"fixture"},"full_name":format!("fixture/{name}"),"private":true,"default_branch":"main","clone_url":format!("https://github.com/fixture/{name}.git"),"required_submodules":["vendor/library"]}));
        let root = home.join(name);
        for path in [
            &root,
            &root.join(".git"),
            &root.join(".git/modules"),
            &root.join(".git/modules/library"),
            &root.join(".git/modules/library/objects"),
            &root.join(".git/modules/library/objects/aa"),
            &root.join("vendor"),
            &root.join("vendor/library"),
        ] {
            directory(&sys, &user, path);
        }
        file(
            &sys,
            &user,
            &root.join("vendor/library/.git"),
            b"gitdir: ../../.git/modules/library\n",
        );
        file(
            &sys,
            &user,
            &root.join(".git/modules/library/objects/aa/object"),
            b"object",
        );
    }
    let bytes = aterm_json::to_vec(
        &aterm_json::json!({"schema":2,"account":"fixture","repositories":repositories}),
    )
    .unwrap();
    file(
        &sys,
        &user,
        &inventory::file(home.to_str().unwrap()),
        &bytes,
    );
    inventory::Inventory::read(&sys, &user).unwrap();
    let scope = Checkouts::capture(&sys, &user);
    let mut calls = BTreeMap::<PathBuf, usize>::new();
    let observed = scope.observe_with(&mut |path| {
        *calls.entry(path.to_path_buf()).or_default() += 1;
        sys.scoped_path_metadata(path)
    });
    assert!(observed == scope.metadata());
    assert!(
        calls.values().all(|count| *count == 1),
        "shared ancestors must not multiply scoped metadata observations"
    );
    assert!(
        calls.len() <= scope.paths.len() + 2 * 53 + 1,
        "{} scoped observations for {} declared boundaries",
        calls.len(),
        scope.paths.len()
    );
    let blocked = home.join("code-0/.git/modules");
    sys.stat_of(
        blocked.to_str().unwrap(),
        crate::port::Stat {
            uid: user.uid,
            gid: user.gid,
            mode: 0o700,
            dir: false,
            symlink: true,
            len: 0,
        },
    );
    calls.clear();
    let changed = scope.observe_with(&mut |path| {
        *calls.entry(path.to_path_buf()).or_default() += 1;
        sys.scoped_path_metadata(path)
    });
    assert!(changed != observed);
    assert!(
        !calls
            .keys()
            .any(|path| path.starts_with(blocked.join("library"))),
        "a newly linked parent stops deeper metadata reads in the next round"
    );
}

#[cfg(unix)]
#[test]
fn replacing_a_checked_parent_cannot_cross_the_scoped_metadata_boundary() {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    let scratch = aterm_tempfile::tempdir().unwrap();
    let base = scratch.path().canonicalize().unwrap();
    let home = base.join("home");
    let root = home.join("code");
    let foreign = base.join("other-application-state");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::create_dir_all(foreign.join(".git")).unwrap();
    std::fs::create_dir(home.join(".secrets")).unwrap();
    let private = inventory::file(home.to_str().unwrap());
    std::fs::write(&private, br#"{"schema":2,"account":"fixture","repositories":[{"id":1,"name":"code","owner":{"login":"fixture"},"full_name":"fixture/code","private":true,"default_branch":"main","clone_url":"https://github.com/fixture/code.git"}]}"#).unwrap();
    std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(root.join(".git/config"), b"selected origin").unwrap();
    std::fs::write(foreign.join(".git/config"), b"foreign origin").unwrap();
    let user = User {
        name: "fixture".into(),
        uid: std::fs::metadata(&home).unwrap().uid(),
        gid: 20,
        home: home.to_string_lossy().into_owned(),
        shell: String::new(),
        admin: false,
    };
    let sys = crate::port::Live::new();
    let scope = Checkouts::capture(&sys, &user);
    let config = root.join(".git/config");
    assert!(
        scope
            .metadata
            .versions
            .iter()
            .any(|(path, version)| path == &config && version.is_ok())
    );
    let mut replaced = false;
    let after = scope.observe_with(&mut |path| {
        let stat = sys.scoped_path_metadata(path)?;
        if path == root && !replaced {
            std::fs::rename(&root, home.join("saved-code")).unwrap();
            std::os::unix::fs::symlink(&foreign, &root).unwrap();
            replaced = true;
        }
        Ok(stat)
    });
    assert!(replaced);
    assert!(
        after
            .versions
            .iter()
            .any(|(path, version)| path == &config && version.is_err()),
        "a parent swapped after its cached admission must not yield foreign metadata"
    );
}
