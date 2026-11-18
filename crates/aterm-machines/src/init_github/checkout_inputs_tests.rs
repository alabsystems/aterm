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
    let selected = selected(&sys, &user).unwrap();
    let mut capture_calls = BTreeMap::<PathBuf, usize>::new();
    let scope = Checkouts::capture_with(&sys, &user, &selected, &mut |path| {
        *capture_calls.entry(path.to_path_buf()).or_default() += 1;
        sys.scoped_path_metadata(path)
    });
    eprintln!(
        "checkout capture: repositories={} metadata_reads={} distinct_paths={} home_reads={} max_reads={}",
        selected.len(),
        capture_calls.values().sum::<usize>(),
        capture_calls.len(),
        capture_calls.get(home).copied().unwrap_or_default(),
        capture_calls.values().max().copied().unwrap_or_default()
    );
    assert!(
        capture_calls.values().all(|count| *count <= 2),
        "each component is read at most once for discovery and once for final observation"
    );
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

    directory(&sys, &user, &blocked);
    let mut replaced = false;
    let captured = Checkouts::capture_with(&sys, &user, &selected, &mut |path| {
        // The first checkout was discovered before the second. Change its parent while
        // discovery continues, independently of whether final observation reads it again.
        if path == home.join("code-1") && !replaced {
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
            replaced = true;
        }
        sys.scoped_path_metadata(path)
    });
    assert!(replaced);
    assert!(
        captured
            .metadata()
            .shapes
            .iter()
            .any(|(path, shape)| path == &blocked && shape.is_err()),
        "discovery's old parent admission must not become final metadata"
    );
    assert!(
        captured.metadata() == captured.observe(&sys),
        "the captured metadata must agree with an independently admitted final observation"
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
    let selected = selected(&sys, &user).unwrap();
    let mut calls = BTreeMap::<PathBuf, usize>::new();
    let scope = Checkouts::capture_with(&sys, &user, &selected, &mut |path| {
        *calls.entry(path.to_path_buf()).or_default() += 1;
        sys.scoped_path_metadata(path)
    });
    eprintln!(
        "native checkout capture: repositories={} metadata_reads={} distinct_paths={} home_reads={} max_reads={}",
        selected.len(),
        calls.values().sum::<usize>(),
        calls.len(),
        calls.get(&home).copied().unwrap_or_default(),
        calls.values().max().copied().unwrap_or_default()
    );
    assert!(
        calls.values().all(|count| *count <= 2),
        "discovery admission is bounded, and final metadata is freshly observed"
    );
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
/// class each path was watched as are equal, nothing moved between them, and they need no write
/// of a pass's own to account for them ([`Checkouts::accounted`] with none recorded), so no look
/// calls for work, or calls for none, because of a class.
/// NEGATIVE CONTROL: a `Class` compared as the word it is (`#[derive(PartialEq, Eq)]` in place
/// of its `eq` that answers `true`) makes the two captures unequal: `relabelled == capture`
/// fails.
#[test]
fn a_class_takes_no_part_in_what_a_capture_decides() {
    let (sys, user) = fixture();
    let root = Path::new(&user.home).join("code");
    directory(&sys, &user, &root);
    directory(&sys, &user, &root.join(".git"));
    let capture = Checkouts::capture(&sys, &user);
    let mut relabelled = capture.clone();
    assert!(!relabelled.paths.is_empty());
    for (_, class) in relabelled.paths.values_mut() {
        *class = Class(MarkMoved::SHAPE);
    }
    assert!(
        capture
            .paths
            .values()
            .all(|(_, class)| class.0 != MarkMoved::SHAPE),
        "every path of the capture is watched as another class than the relabelled one's"
    );
    assert!(relabelled == capture);
    assert!(relabelled.moved(&capture).is_empty());
    assert!(capture.moved(&relabelled).is_empty());
    let none = CheckoutWrites::default();
    for (later, earlier) in [(&relabelled, &capture), (&capture, &relabelled)] {
        assert_eq!(
            later.accounted(earlier, &none, false),
            Accounted::Own { made: false }
        );
    }
}

/// A PATH BOTH CAPTURES WATCH MOVES ITS CLASS WHERE ONLY ONE OF THEM READS ITS VERSION: a later
/// capture that reads the checkout folder's version, which the one before watched as `root`
/// without reading it, names `root`, and so does the comparison the other way round.
/// NEGATIVE CONTROL: a `moved` that compares only the versions both captures read names nothing:
/// `""` where `"m=root"` is wanted.
#[test]
fn a_version_only_one_capture_reads_moves_its_class() {
    let (sys, user) = fixture();
    let root = Path::new(&user.home).join("code");
    directory(&sys, &user, &root);
    directory(&sys, &user, &root.join(".git"));
    let before = Checkouts::capture(&sys, &user);
    assert!(matches!(before.paths.get(&root), Some((false, _))));
    assert!(
        before
            .metadata
            .versions
            .iter()
            .all(|(path, _)| path != &root)
    );
    let mut after = before.clone();
    after.paths.get_mut(&root).unwrap().0 = true;
    after.metadata = after.observe(&sys);
    assert!(
        after
            .metadata
            .versions
            .iter()
            .any(|(path, _)| path == &root)
    );
    assert_eq!(spelled(after.moved(&before)), spelled(MarkMoved::ROOT));
    assert_eq!(spelled(before.moved(&after)), spelled(MarkMoved::ROOT));
}

/// A CONVERGE PASS'S OWN WRITES ACCOUNT FOR A CHECKOUT'S MOVE EXACTLY, AND NEVER FOR ITS MAKING
/// UNLESS THE PASS MADE IT: each effect run through [`PassWrites::around`] records the checkout's
/// part of the capture before and after it, and a later capture that differs from an earlier one
/// by those writes alone ([`Checkouts::accounted`]) is the pass's own: a config written in place
/// and a fetch's new loose-object folder are. What only a checkout's making moves is not, from an
/// effect that made nothing: `HEAD` naming another ref (the current ref `added` and `removed`),
/// a checkout folder's mode (`root`); it is from one that made the checkout (`made`). A write
/// the pass did not make, after its effect or before it began, accounts for nothing; an effect
/// still under way defers the judgment while its pass's process lives, and accounts for nothing
/// once it is gone.
/// NEGATIVE CONTROL: [`Checkouts::structural`] answering `false` absorbs the `HEAD` switch and
/// the mode change and fails their assertions; answering `true` fails the loose-object folder's.
#[test]
fn a_passs_own_writes_account_for_a_checkout_move_exactly_and_never_its_making() {
    use crate::sync::repository_refresh::checkout_writes;

    let (sys, user) = fixture();
    let root = Path::new(&user.home).join("code");
    let admin = root.join(".git");
    for path in [
        &root,
        &admin,
        &admin.join("refs"),
        &admin.join("refs/heads"),
        &admin.join("objects"),
    ] {
        directory(&sys, &user, path);
    }
    let config = admin.join("config");
    file(&sys, &user, &config, b"[core]");
    file(&sys, &user, &admin.join("HEAD"), b"ref: refs/heads/main\n");
    file(&sys, &user, &admin.join("refs/heads/main"), b"1111\n");
    let pid = std::process::id();
    sys.born(pid, 1_000);
    let scratch = aterm_tempfile::tempdir().unwrap();
    let pass = RepositoryPass::open(scratch.path(), 0)
        .unwrap()
        .pass
        .expect("no other pass holds the lock");
    let writes = PassWrites::new(&sys, &user, Arc::clone(&pass))
        .expect("the default inventory's checkouts are watched");
    // A look as the observer makes it: a fresh capture, judged against `before` by the writes
    // the pass record holds.
    let look = |before: &Checkouts| {
        let after = Checkouts::capture(&sys, &user);
        let recorded = checkout_writes(scratch.path());
        let accounted = after.accounted(before, &recorded, recorded.running(&sys));
        (accounted, after)
    };
    let mut before = Checkouts::capture(&sys, &user);

    writes.around(&sys, &root, || {
        file(&sys, &user, &config, b"[core]\n\tuntrackedCache = true");
        ((), Wrote::Own)
    });
    let (accounted, after) = look(&before);
    assert_eq!(accounted, Accounted::Own { made: false }, "a config");
    before = after;

    writes.around(&sys, &root, || {
        directory(&sys, &user, &admin.join("objects/ab"));
        file(&sys, &user, &admin.join("objects/ab/object"), b"fetched");
        ((), Wrote::Own)
    });
    let (accounted, after) = look(&before);
    assert!(after.paths().contains(&admin.join("objects/ab")));
    assert_eq!(
        accounted,
        Accounted::Own { made: false },
        "a fetch's new loose-object folder"
    );
    before = after;

    writes.around(&sys, &root, || {
        file(&sys, &user, &admin.join("refs/heads/other"), b"2222\n");
        file(&sys, &user, &admin.join("HEAD"), b"ref: refs/heads/other\n");
        ((), Wrote::Own)
    });
    let (accounted, after) = look(&before);
    assert_eq!(
        spelled(after.moved(&before)),
        spelled(
            MarkMoved::HEAD
                .with(MarkMoved::REFS_HEADS_DIR)
                .with(MarkMoved::ADDED)
                .with(MarkMoved::REMOVED)
        )
    );
    assert_eq!(
        accounted,
        Accounted::Not,
        "HEAD naming another ref is a checkout's making, which no fetch does"
    );
    before = after;

    writes.around(&sys, &root, || {
        file(&sys, &user, &admin.join("HEAD"), b"ref: refs/heads/main\n");
        ((), Wrote::Made)
    });
    let (accounted, after) = look(&before);
    assert_eq!(
        accounted,
        Accounted::Own { made: false },
        "an effect that made the checkout moves what its making moves"
    );
    before = after;

    writes.around(&sys, &root, || {
        sys.stat_of(
            root.to_str().unwrap(),
            Stat {
                uid: user.uid,
                gid: user.gid,
                mode: 0o750,
                dir: true,
                symlink: false,
                len: 0,
            },
        );
        ((), Wrote::Own)
    });
    let (accounted, after) = look(&before);
    assert_eq!(accounted, Accounted::Not, "a checkout folder's mode");
    before = after;

    file(&sys, &user, &config, b"[core]\n\tbare = false");
    let (accounted, after) = look(&before);
    assert_eq!(accounted, Accounted::Not, "a write after the pass's");
    before = after;

    file(
        &sys,
        &user,
        &config,
        b"[core]\n\tbare = false\n\tlogallrefupdates = true",
    );
    writes.around(&sys, &root, || {
        file(
            &sys,
            &user,
            &config,
            b"[core]\n\tbare = false\n\tlogallrefupdates = true\n\tuntrackedCache = true",
        );
        ((), Wrote::Own)
    });
    let (accounted, after) = look(&before);
    assert_eq!(
        accounted,
        Accounted::Not,
        "a write before the pass's effect began, which the effect began from"
    );
    before = after;

    writes.around(&sys, &root, || {
        file(&sys, &user, &config, b"[core]\n\tfsmonitor = false");
        assert_eq!(
            look(&before).0,
            Accounted::Running,
            "an effect under way, its pass's process alive"
        );
        sys.born(pid, 2_000);
        assert_eq!(
            look(&before).0,
            Accounted::Not,
            "an effect whose pass's process is gone"
        );
        sys.born(pid, 1_000);
        ((), Wrote::Own)
    });
    assert_eq!(
        look(&before).0,
        Accounted::Own { made: false },
        "the effect once it ended"
    );
}

/// WHAT ONLY A CHECKOUT'S MAKING MOVES IS JUDGED IN THAT CHECKOUT ALONE: where one effect of a
/// pass made a checkout's `.gitmodules` change (`made`) and another wrote a second checkout's
/// config in place, both are the pass's own; the same `.gitmodules` change from an effect that
/// made nothing is not.
/// NEGATIVE CONTROL: [`Checkouts::structural`] reading the versions of every checkout, not only
/// the one at `root`, takes the first checkout's `.gitmodules` for the second's making:
/// `Not` where `Own { made: false }` is wanted.
#[test]
fn what_only_a_making_moves_is_judged_in_its_own_checkout() {
    use crate::sync::repository_refresh::checkout_writes;

    let (sys, user) = fixture();
    let home = Path::new(&user.home);
    let bytes = br#"{"schema":2,"account":"fixture","repositories":[{"id":1,"name":"code","owner":{"login":"fixture"},"full_name":"fixture/code","clone_url":"https://github.com/fixture/code.git","private":true,"default_branch":"main","required_submodules":["vendor/library"]},{"id":2,"name":"other","owner":{"login":"fixture"},"full_name":"fixture/other","clone_url":"https://github.com/fixture/other.git","private":true,"default_branch":"main"}]}"#;
    sys.vault_set(&user.home, inventory::ENTRY, bytes);
    inventory::Inventory::read(&sys, &user).unwrap();
    let (code, other) = (home.join("code"), home.join("other"));
    for path in [&code, &code.join(".git"), &other, &other.join(".git")] {
        directory(&sys, &user, path);
    }
    let gitmodules = code.join(".gitmodules");
    let config = other.join(".git/config");
    file(&sys, &user, &gitmodules, b"[submodule]");
    file(&sys, &user, &config, b"[core]");
    sys.born(std::process::id(), 1_000);
    let scratch = aterm_tempfile::tempdir().unwrap();
    let pass = RepositoryPass::open(scratch.path(), 0)
        .unwrap()
        .pass
        .expect("no other pass holds the lock");
    let writes = PassWrites::new(&sys, &user, Arc::clone(&pass))
        .expect("the default inventory's checkouts are watched");
    let mut before = Checkouts::capture(&sys, &user);
    assert!(before.paths().contains(&gitmodules) && before.paths().contains(&config));
    for (made, wanted) in [
        (Wrote::Made, Accounted::Own { made: false }),
        (Wrote::Own, Accounted::Not),
    ] {
        writes.around(&sys, &code, || {
            file(&sys, &user, &gitmodules, format!("{made:?}").as_bytes());
            ((), made)
        });
        writes.around(&sys, &other, || {
            file(
                &sys,
                &user,
                &config,
                format!("[core]\n# {made:?}").as_bytes(),
            );
            ((), Wrote::Own)
        });
        let after = Checkouts::capture(&sys, &user);
        assert_eq!(
            spelled(after.moved(&before)),
            spelled(MarkMoved::GITMODULES.with(MarkMoved::CONFIG))
        );
        let recorded = checkout_writes(scratch.path());
        assert_eq!(
            after.accounted(&before, &recorded, recorded.running(&sys)),
            wanted,
            "`.gitmodules` from an effect that wrote {made:?}"
        );
        before = after;
    }
}
