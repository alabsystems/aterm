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
    let bytes = br#"{"schema":2,"account":"fixture","repositories":[{"id":1,"name":"code","owner":{"login":"fixture"},"full_name":"fixture/code","clone_url":"https://github.com/fixture/code.git","private":true,"default_branch":"main","required_submodules":["vendor/library"]}]}"#;
    sys.vault_set(&user.home, inventory::ENTRY, bytes);
    inventory::Inventory::read(&sys, &user).unwrap();
    (sys, user)
}

/// A real file vault beside `home`, its store holding `bytes` as the private inventory: what a
/// test whose port is [`crate::port::Live`] reads in place.
#[cfg(unix)]
fn seeded_inventory(vault: &Path, home: &Path, bytes: &[u8]) {
    use aterm_secrets::{client, text::Audience};
    let paths = client::Paths::for_home(home);
    client::create(&paths, vault.to_str().unwrap(), None, Some("fixture")).unwrap();
    let admin = client::admin_open(&paths).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(&Audience::Everyone, inventory::ENTRY, bytes)
        .unwrap();
    client::admin_publish(&paths, &admin, &doc).unwrap();
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
        profile(&sys, &user).and_then(|p| asked(p.and_then(|p| p.github))),
        Ok(Asked::Off)
    ));
    assert!(Checkouts::capture(&sys, &user).paths().is_empty());
}

#[cfg(unix)]
#[test]
fn native_dependency_object_arrival_moves_cached_metadata_without_a_git_process() {
    use std::os::unix::fs::MetadataExt as _;

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
    let checkout = home.join("code");
    let admin = checkout.join(".git/modules/library");
    for directory in [&admin.join("objects/aa"), &checkout.join("vendor/library")] {
        std::fs::create_dir_all(directory).unwrap();
    }
    let vault = aterm_tempfile::tempdir().unwrap();
    seeded_inventory(&vault.path().join("fixture.vault"), &home, br#"{"schema":2,"account":"fixture","repositories":[{"id":1,"name":"code","owner":{"login":"fixture"},"full_name":"fixture/code","clone_url":"https://github.com/fixture/code.git","private":true,"default_branch":"main","required_submodules":["vendor/library"]}]}"#);
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
    sys.vault_set(&user.home, inventory::ENTRY, &bytes);
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
    use std::os::unix::fs::MetadataExt as _;

    let scratch = aterm_tempfile::tempdir().unwrap();
    let base = scratch.path().canonicalize().unwrap();
    let home = base.join("home");
    let root = home.join("code");
    let foreign = base.join("other-application-state");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::create_dir_all(foreign.join(".git")).unwrap();
    seeded_inventory(&base.join("fixture.vault"), &home, br#"{"schema":2,"account":"fixture","repositories":[{"id":1,"name":"code","owner":{"login":"fixture"},"full_name":"fixture/code","private":true,"default_branch":"main","clone_url":"https://github.com/fixture/code.git"}]}"#);
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

/// A class as the note's word spells it, so a failure names classes and not bits.
fn spelled(moved: MarkMoved) -> String {
    aterm_secrets::converge::StartNote {
        moved,
        ..aterm_secrets::converge::StartNote::default()
    }
    .word()
}

/// EACH WATCHED PATH THAT MOVES THE MARK NAMES ITS CLASS, AND ONLY ITS CLASS: in a checkout
/// with a required submodule and every watched file present, changing one file's bytes moves
/// exactly that file's class (`config`, `config-worktree`, `index`, `packed-refs`, `shallow`,
/// a submodule's `gitdir` pointer, the current ref, `alternates`, `.gitmodules`, a gitfile); a
/// new entry in a watched folder moves that folder's class alone (a checkout's `.git` is
/// `git-dir`, a submodule's administrative folder `admin-dir`, `refs`, `refs/heads`,
/// `objects/pack`, a loose-object folder); a checkout folder's own mode is `root`, and the mode
/// of a folder on the way to the submodule's checkout, not watched itself, `shape`; a new
/// loose-object folder is `objects-dir` and `added`; and `HEAD` naming another ref is `head`,
/// `added` and `removed`. A look at an unchanged checkout moves nothing, and each later capture
/// is equal to the one before it exactly when nothing moved. Limit: the Fake digests a folder
/// by its entries' names, so an entry made and removed between two looks moves nothing here;
/// what the installed build's sets say is read from its ledger lines.
/// NEGATIVE CONTROL: watching `index` as `config` gives `m=config` where `m=index` is wanted.
#[test]
fn each_path_that_moves_the_mark_names_its_class() {
    let (fake, user) = fixture();
    let (sys, user) = (&fake, &user);
    let root = Path::new(&user.home).join("code");
    let admin = root.join(".git");
    let module = admin.join("modules/library");
    let mode = |path: &Path, mode: u32| {
        sys.stat_of(
            path.to_str().unwrap(),
            Stat {
                uid: user.uid,
                gid: user.gid,
                mode,
                dir: true,
                symlink: false,
                len: 0,
            },
        );
    };
    for path in [
        &root,
        &admin,
        &admin.join("refs"),
        &admin.join("refs/heads"),
        &admin.join("objects"),
        &admin.join("objects/pack"),
        &admin.join("objects/info"),
        &admin.join("objects/aa"),
        &root.join("vendor"),
        &root.join("vendor/library"),
        &admin.join("modules"),
        &module,
        &module.join("refs"),
        &module.join("refs/heads"),
        &module.join("objects"),
    ] {
        directory(sys, user, path);
    }
    for (path, bytes) in [
        (root.join(".gitmodules"), &b"[submodule]"[..]),
        (admin.join("config"), b"[core]"),
        (admin.join("config.worktree"), b"[core]"),
        (admin.join("HEAD"), b"ref: refs/heads/main\n"),
        (admin.join("index"), b"DIRC"),
        (admin.join("packed-refs"), b"# pack-refs"),
        (admin.join("shallow"), b"0000"),
        (admin.join("refs/heads/main"), b"1111\n"),
        (admin.join("objects/info/alternates"), b""),
        (admin.join("objects/aa/object"), b"object"),
        (
            root.join("vendor/library/.git"),
            b"gitdir: ../../.git/modules/library\n",
        ),
        (module.join("config"), b"[core]"),
        (module.join("HEAD"), b"ref: refs/heads/main\n"),
        (module.join("gitdir"), b"../../../vendor/library/.git\n"),
    ] {
        file(sys, user, &path, bytes);
    }
    let mut before = Checkouts::capture(sys, user);
    assert!(before.paths().contains(&admin.join("objects/aa")));
    assert!(before.paths().contains(&module.join("gitdir")));
    let again = Checkouts::capture(sys, user);
    assert_eq!(spelled(again.moved(&before)), "");
    assert!(again == before);
    let mut look = |change: &dyn Fn(), wanted: MarkMoved| {
        change();
        let after = Checkouts::capture(sys, user);
        let moved = after.moved(&before);
        assert_eq!(spelled(moved), spelled(wanted));
        assert_eq!(moved.is_empty(), after == before);
        before = after;
    };
    let write = |path: PathBuf| move || file(sys, user, &path, b"changed");
    for (path, class) in [
        (admin.join("config"), MarkMoved::CONFIG),
        (admin.join("config.worktree"), MarkMoved::CONFIG_WORKTREE),
        (admin.join("index"), MarkMoved::INDEX),
        (admin.join("packed-refs"), MarkMoved::PACKED_REFS),
        (admin.join("shallow"), MarkMoved::SHALLOW),
        (module.join("gitdir"), MarkMoved::POINTER),
        (admin.join("refs/heads/main"), MarkMoved::CURRENT_REF),
        (admin.join("objects/info/alternates"), MarkMoved::ALTERNATES),
        (root.join(".gitmodules"), MarkMoved::GITMODULES),
        (module.join("config"), MarkMoved::CONFIG),
    ] {
        look(&write(path), class);
    }
    look(
        &|| {
            file(
                sys,
                user,
                &root.join("vendor/library/.git"),
                b"gitdir: ../../.git/modules/library\r\n",
            );
        },
        MarkMoved::GIT_DIR,
    );
    for (path, class) in [
        (admin.join("ORIG_HEAD"), MarkMoved::GIT_DIR),
        (module.join("FETCH_HEAD"), MarkMoved::ADMIN_DIR),
        (admin.join("refs/heads/other"), MarkMoved::REFS_HEADS_DIR),
        (
            admin.join("objects/pack/pack-1.idx"),
            MarkMoved::OBJECTS_DIR,
        ),
        (admin.join("objects/aa/other"), MarkMoved::OBJECTS_FANOUT),
    ] {
        look(&write(path), class);
    }
    look(
        &|| directory(sys, user, &admin.join("refs/tags")),
        MarkMoved::REFS_DIR,
    );
    look(&|| mode(&root, 0o750), MarkMoved::ROOT);
    look(&|| mode(&root.join("vendor"), 0o750), MarkMoved::SHAPE);
    look(
        &|| directory(sys, user, &admin.join("objects/bb")),
        MarkMoved::OBJECTS_DIR.with(MarkMoved::ADDED),
    );
    look(
        &|| file(sys, user, &admin.join("HEAD"), b"ref: refs/heads/other\n"),
        MarkMoved::HEAD
            .with(MarkMoved::ADDED)
            .with(MarkMoved::REMOVED),
    );
    look(&|| {}, MarkMoved::default());
}

/// A PATH'S CLASS TAKES NO PART IN WHAT A CAPTURE DECIDES: two captures that differ only in the
/// class a path was watched as are equal and nothing moved between them, so no look calls for
/// work, or calls for none, because of a class.
/// NEGATIVE CONTROL: classes compared as the map they are make the two captures unequal.
#[test]
fn a_class_takes_no_part_in_what_a_capture_decides() {
    let (sys, user) = fixture();
    let root = Path::new(&user.home).join("code");
    directory(&sys, &user, &root);
    directory(&sys, &user, &root.join(".git"));
    let capture = Checkouts::capture(&sys, &user);
    let mut relabelled = capture.clone();
    assert!(!relabelled.classes.0.is_empty());
    for class in relabelled.classes.0.values_mut() {
        *class = MarkMoved::SHAPE;
    }
    assert!(relabelled == capture);
    assert!(relabelled.moved(&capture).is_empty());
    assert!(capture.moved(&relabelled).is_empty());
}
