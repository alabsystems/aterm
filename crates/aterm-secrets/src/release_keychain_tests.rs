// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::fake::{Fake, Kc, World};
use super::*;
use aterm_update_core::pins::APPLE_TEAM_ID as TEAM;

const SHA1: &str = "A1B2C3D4E5F60718293A4B5C6D7E8F9012345678";

/// A scratch HOME with the folders macOS gives every user, and its login keychain.
fn home() -> (aterm_tempfile::TempDir, PathBuf) {
    let dir = aterm_tempfile::tempdir().expect("scratch HOME");
    let home = std::fs::canonicalize(dir.path()).expect("canonical scratch");
    std::fs::create_dir_all(home.join("Library/Keychains")).expect("Keychains");
    std::fs::create_dir_all(home.join(".aterm/apple")).expect(".aterm/apple");
    (dir, home)
}

fn mentions_login(call: &[String]) -> bool {
    call.iter().any(|a| a.contains("login.keychain"))
}

/// A first provision on a fresh Mac makes, opens, fills and lists the release keychain and
/// names no other keychain in any call; the search list keeps its entries and their order.
#[test]
fn a_fresh_home_gets_a_release_keychain_and_the_login_keychain_is_never_named() {
    let (_dir, home) = home();
    let login = login_keychain_path(&home);
    let other = PathBuf::from("/Library/Keychains/Other.keychain");
    let mut world = World::new(SHA1);
    world.keychains.insert(login.clone(), Kc::default());
    world.search_list = vec![login.clone(), other.clone()];
    let fake = Fake::new(world);
    let kc = Keychains::new(&home, &fake);
    let release = kc.release();
    assert_eq!(
        release,
        home.join("Library/Keychains/aterm-release.keychain-db")
    );

    kc.install_pair(
        &home.join(".aterm/apple/c.cer"),
        &home.join(".aterm/apple/k.key"),
    )
    .expect("install");

    let pass = password_path(&home);
    let stored = std::fs::read_to_string(&pass).expect("the password file");
    let stored = stored.trim();
    assert!(
        aterm_codec::hex::is_hex(stored, 64),
        "256 random bits as hex"
    );
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&pass), 0o600);
        assert_eq!(mode(pass.parent().unwrap()), 0o700);
        assert_eq!(mode(&release), 0o600, "the made keychain is private");
    }
    let world = fake.0.borrow();
    assert_eq!(
        world.keychains[&release],
        Kc {
            password: stored.to_string(),
            locked: false,
            certs: vec![SHA1.to_string()],
            keys: 1,
            partition: true,
        }
    );
    assert_eq!(world.keychains[&login], Kc::default(), "login untouched");
    assert_eq!(
        world.search_list,
        [login.clone(), other, release.clone()],
        "every entry kept, in order, the release keychain appended"
    );
    // Only the search-list rewrite names the login keychain, to keep it where it was.
    for call in &world.log {
        let keeps_the_list = call[1] == "list-keychains"
            && call.iter().position(|a| a == "-s").is_some_and(|at| {
                call[at + 1] == login.display().to_string() && call.len() == at + 4
            });
        assert!(
            keeps_the_list || !mentions_login(call),
            "a call names the login keychain: {call:?}"
        );
    }
    drop(world);
    let shown = release.display().to_string();
    for call in fake.writes() {
        let target_is_release =
            call.last() == Some(&shown) || call.windows(2).any(|w| w[0] == "-k" && w[1] == shown);
        let search_list = call[1] == "list-keychains";
        assert!(
            target_is_release || search_list,
            "a write names a keychain other than the release one: {call:?}"
        );
    }
    let argv = |words: &[&str]| words.iter().map(|w| (*w).to_string()).collect::<Vec<_>>();
    assert_eq!(
        fake.calls("security", "set-key-partition-list"),
        [argv(&[
            "security",
            "set-key-partition-list",
            "-S",
            "apple-tool:,apple:,codesign:",
            "-s",
            "-k",
            stored,
            &shown
        ])]
    );
    assert_eq!(
        fake.calls("security", "set-keychain-settings"),
        [argv(&["security", "set-keychain-settings", &shown])],
        "no -l or -u: it never locks by itself"
    );
    for import in fake.calls("security", "import") {
        assert!(
            import.windows(2).any(|w| w[0] == "-T" && w[1] == CODESIGN)
                && import
                    .windows(2)
                    .any(|w| w[0] == "-T" && w[1] == PRODUCTSIGN),
            "{import:?}"
        );
    }
    assert_eq!(kc.identities(TEAM, true), Ok(vec![SHA1.to_string()]));
    assert_eq!(kc.holds(TEAM, SHA1), Ok(true));
    for leave in [Leave::Write, Leave::Look] {
        assert_eq!(
            kc.signing_keychain(TEAM, SHA1, leave),
            Ok(Some(release.clone()))
        );
    }
    // A keychain an earlier aterm made as the umask said is made private when opened.
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&release, std::fs::Permissions::from_mode(0o644)).unwrap();
        kc.install_pair(
            &home.join(".aterm/apple/c.cer"),
            &home.join(".aterm/apple/k.key"),
        )
        .expect("install again");
        assert_eq!(
            std::fs::metadata(&release).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

/// Only this user's singly-linked keychain file is made private; a link and a hard-linked
/// file are left as they are, for the signing proof's stamp to refuse.
#[test]
fn only_an_owned_single_keychain_file_is_made_private() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = aterm_tempfile::tempdir().unwrap();
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    let open = |p: &Path| {
        std::fs::write(p, b"keychain").unwrap();
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o644)).unwrap();
    };
    let plain = dir.path().join("plain.keychain-db");
    open(&plain);
    make_private(&plain).unwrap();
    assert_eq!(mode(&plain), 0o600);

    let target = dir.path().join("target.keychain-db");
    open(&target);
    let link = dir.path().join("link.keychain-db");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    make_private(&link).unwrap();
    assert_eq!(mode(&target), 0o644, "a link's target is not changed");

    let twice = dir.path().join("twice.keychain-db");
    open(&twice);
    std::fs::hard_link(&twice, dir.path().join("again.keychain-db")).unwrap();
    make_private(&twice).unwrap();
    assert_eq!(mode(&twice), 0o644, "a hard-linked file is not changed");
}

/// Converging an existing release keychain changes nothing a second time: no keychain is
/// made, nothing imported, the search list is not rewritten, the password is kept.
#[test]
fn a_second_provision_makes_nothing_and_rewrites_nothing() {
    let (_dir, home) = home();
    let login = login_keychain_path(&home);
    let mut world = World::new(SHA1);
    world.search_list = vec![login.clone()];
    let fake = Fake::new(world);
    let kc = Keychains::new(&home, &fake);
    kc.install_pair(&home.join("c.cer"), &home.join("k.key"))
        .expect("install");
    let password = std::fs::read(password_path(&home)).unwrap();
    let before = fake.0.borrow().keychains.clone();
    let list = fake.0.borrow().search_list.clone();
    for _ in 0..2 {
        fake.0
            .borrow_mut()
            .keychains
            .get_mut(&kc.release())
            .unwrap()
            .locked = true;
        fake.0.borrow_mut().log.clear();
        kc.converge(TEAM).expect("converge");
        let world = fake.0.borrow();
        assert_eq!(world.keychains, before);
        assert_eq!(world.search_list, list);
        drop(world);
        for (sub, why) in [
            ("create-keychain", "nothing is made"),
            ("import", "nothing is imported"),
        ] {
            assert!(fake.calls("security", sub).is_empty(), "{why}");
        }
        assert!(
            fake.writes().iter().all(|c| c[1] != "list-keychains"),
            "the search list is not rewritten"
        );
        let shown = kc.release().display().to_string();
        assert_eq!(
            fake.calls("security", "set-keychain-settings"),
            [["security", "set-keychain-settings", shown.as_str()].map(String::from)],
            "never locks by itself, re-asserted with no -l or -u"
        );
    }
    assert_eq!(std::fs::read(password_path(&home)).unwrap(), password);

    // A Mac with no release keychain converges with no call at all.
    let (_dir, bare) = self::home();
    let fake = Fake::new(World::new(SHA1));
    Keychains::new(&bare, &fake).converge(TEAM).expect("no-op");
    assert!(fake.0.borrow().log.is_empty());
}

/// The search list is appended to, never replaced, and never written from an empty read.
#[test]
fn the_search_list_write_keeps_every_entry_in_order() {
    let (_dir, home) = home();
    let entries = vec![
        login_keychain_path(&home),
        PathBuf::from("/Users//x/Library/Keychains/aterm-dev.keychain-db"),
        PathBuf::from("/Library/Keychains/System.keychain"),
    ];
    let mut world = World::new(SHA1);
    world.search_list = entries.clone();
    let fake = Fake::new(world);
    let kc = Keychains::new(&home, &fake);
    kc.install_pair(&home.join("c.cer"), &home.join("k.key"))
        .expect("install");
    let mut want = entries;
    want.push(kc.release());
    assert_eq!(fake.0.borrow().search_list, want);

    let (_dir, home) = self::home();
    let fake = Fake::new(World::new(SHA1));
    let refused = Keychains::new(&home, &fake)
        .install_pair(&home.join("c.cer"), &home.join("k.key"))
        .expect_err("an empty read is never written back");
    assert!(refused.what.contains("read back empty"), "{refused:?}");
    assert!(fake.0.borrow().search_list.is_empty());
}

/// A Mac provisioned before the release keychain signs from its login keychain: the
/// identity is found there read-only, codesign gets no `--keychain`, and nothing writes.
#[test]
fn a_login_keychain_identity_is_accepted_and_left_alone() {
    let (_dir, home) = home();
    let login = login_keychain_path(&home);
    let mut world = World::new(SHA1);
    world.keychains.insert(login.clone(), Kc::holding(SHA1));
    world.search_list = vec![login.clone()];
    let fake = Fake::new(world);
    let kc = Keychains::new(&home, &fake);
    assert_eq!(kc.identities(TEAM, true), Ok(vec![SHA1.to_string()]));
    assert_eq!(kc.holds(TEAM, SHA1), Ok(false));
    for leave in [Leave::Write, Leave::Look] {
        assert_eq!(kc.signing_keychain(TEAM, SHA1, leave), Ok(None));
    }
    kc.converge(TEAM).expect("nothing to converge");
    assert_eq!(kc.state(TEAM), Ok(State::Missing));
    let row = kc.row("m9", TEAM, &|_| {
        panic!("nothing of the release keychain to probe")
    });
    assert!(
        matches!(&row, Row::Ready(line) if line.contains(SHA1) && line.contains("is absent")),
        "{row:?}"
    );
    assert!(fake.writes().is_empty(), "{:?}", fake.writes());
    assert_eq!(fake.0.borrow().keychains[&login], Kc::holding(SHA1));
}

/// A release keychain whose password is gone, or wrong, is reported and never deleted, and
/// codesign is never pointed at it (a locked keychain asks in a dialog).
#[test]
fn a_release_keychain_that_will_not_open_is_reported_not_removed() {
    let (_dir, home) = home();
    let fake = Fake::new(World::new(SHA1));
    let kc = Keychains::new(&home, &fake);
    fake.0.borrow_mut().search_list = vec![login_keychain_path(&home)];
    kc.install_pair(&home.join("c.cer"), &home.join("k.key"))
        .expect("install");
    let release = kc.release();
    fake.0
        .borrow_mut()
        .keychains
        .get_mut(&release)
        .unwrap()
        .locked = true;
    std::fs::remove_file(password_path(&home)).unwrap();
    let State::Locked {
        refusal,
        holds: true,
    } = kc.state(TEAM).expect("state")
    else {
        panic!("a keychain with no password file, holding the identity, is locked to us");
    };
    let file = password_path(&home).display().to_string();
    assert!(refusal.what.contains(&file), "{refusal:?}");
    assert!(!refusal.what.contains("does not unlock"), "{refusal:?}");
    assert!(refusal.fix.contains("move it aside"), "{refusal:?}");
    assert!(!refusal.fix.contains("login"), "{refusal:?}");
    for leave in [Leave::Write, Leave::Look] {
        assert!(kc.signing_keychain(TEAM, SHA1, leave).is_err());
        assert!(prove_can_sign(&kc, SHA1, leave).is_err());
    }
    assert!(kc.converge(TEAM).is_err());
    assert!(release.is_file(), "never deleted");
    assert!(fake.calls("security", "delete-keychain").is_empty());
    assert!(
        fake.calls("codesign", "--force").is_empty(),
        "the test signature never reached codesign"
    );

    std::fs::write(password_path(&home), format!("{}\n", "0".repeat(64))).unwrap();
    let State::Locked { refusal, .. } = kc.state(TEAM).expect("state") else {
        panic!("a wrong password does not open it");
    };
    assert!(refusal.what.contains("does not unlock"), "{refusal:?}");

    // A listing that fails is no proof the keychain holds nothing: codesign is not run.
    fake.0.borrow_mut().fail = Some("find-identity");
    fake.0.borrow_mut().log.clear();
    for leave in [Leave::Write, Leave::Look] {
        assert!(kc.signing_keychain(TEAM, SHA1, leave).is_err());
        assert!(prove_can_sign(&kc, SHA1, leave).is_err());
    }
    assert!(fake.calls("codesign", "--force").is_empty());
}

/// An unopenable release keychain that holds no identity (one provision made only for an
/// issuing intermediate, its password file since lost) blocks nothing on a Mac whose search
/// list holds the identity: the row reads Ready and converge leaves it. With no identity
/// anywhere it still blocks.
#[test]
fn an_unopenable_release_keychain_holding_nothing_blocks_no_login_keychain_mac() {
    let (_dir, home) = home();
    let login = login_keychain_path(&home);
    let mut world = World::new(SHA1);
    world.keychains.insert(login.clone(), Kc::holding(SHA1));
    world.search_list = vec![login.clone()];
    let fake = Fake::new(world);
    let kc = Keychains::new(&home, &fake);
    kc.install_certificate(&home.join("issuer.cer"))
        .expect("the intermediate");
    let release = kc.release();
    fake.0
        .borrow_mut()
        .keychains
        .get_mut(&release)
        .unwrap()
        .locked = true;
    std::fs::remove_file(password_path(&home)).unwrap();
    fake.0.borrow_mut().log.clear();

    assert!(matches!(
        kc.state(TEAM),
        Ok(State::Locked { holds: false, .. })
    ));
    let row = kc.row("m9", TEAM, &|_| panic!("nothing in it to probe"));
    assert!(
        matches!(&row, Row::Ready(line) if line.contains(SHA1) && line.contains("holds no")),
        "{row:?}"
    );
    kc.converge(TEAM).expect("nothing to converge in it");
    assert!(fake.writes().is_empty(), "{:?}", fake.writes());
    assert!(release.is_file(), "never deleted");

    fake.0.borrow_mut().keychains.remove(&login);
    assert!(matches!(
        kc.row("m9", TEAM, &|_| panic!("nothing to probe")),
        Row::Blocked { .. }
    ));
}

/// A provision re-run converges a release keychain that lost its partition list or its
/// place on the search list: both are restored with the keychain's own password, every
/// other search-list entry kept in its order.
#[test]
fn converge_restores_the_partition_list_and_the_search_list_entry() {
    let (_dir, home) = home();
    let login = login_keychain_path(&home);
    let other = PathBuf::from("/Library/Keychains/Other.keychain");
    let mut world = World::new(SHA1);
    world.search_list = vec![login.clone(), other.clone()];
    let fake = Fake::new(world);
    let kc = Keychains::new(&home, &fake);
    kc.install_pair(&home.join("c.cer"), &home.join("k.key"))
        .expect("install");
    let release = kc.release();
    {
        let mut world = fake.0.borrow_mut();
        let kept = world.keychains.get_mut(&release).unwrap();
        kept.partition = false;
        kept.locked = true;
        world.search_list = vec![login.clone(), other.clone()];
    }
    kc.converge(TEAM).expect("converge");
    let world = fake.0.borrow();
    assert!(
        world.keychains[&release].partition,
        "partition list restored"
    );
    assert_eq!(world.search_list, [login, other, release]);
}

/// codesign is never handed the release keychain while its key lacks the partition list
/// (a provision stopped between the import and `set-key-partition-list`): the list is
/// re-asserted with the keychain's own password first, so the test signature passes with
/// no dialog.
#[test]
fn codesign_never_meets_a_key_without_its_partition_list() {
    let (_dir, home) = home();
    let mut world = World::new(SHA1);
    world.search_list = vec![login_keychain_path(&home)];
    world.fail = Some("set-key-partition-list");
    let fake = Fake::new(world);
    let kc = Keychains::new(&home, &fake);
    kc.install_pair(&home.join("c.cer"), &home.join("k.key"))
        .expect_err("stopped before the partition list");
    let release = kc.release();
    {
        let mut world = fake.0.borrow_mut();
        world.fail = None;
        let stopped = world.keychains.get_mut(&release).unwrap();
        assert!(!stopped.partition && stopped.keys == 1, "{stopped:?}");
        stopped.locked = true;
    }
    fake.0.borrow_mut().log.clear();

    assert_eq!(
        prove_can_sign(&kc, SHA1, Leave::Write),
        Ok(Proof::Signed),
        "the test signature"
    );
    let log = fake.0.borrow().log.clone();
    let at = |tool: &str, first: &str| {
        log.iter()
            .position(|c| c[0] == tool && c[1] == first)
            .unwrap_or_else(|| panic!("no {tool} {first}: {log:?}"))
    };
    assert!(at("security", "set-key-partition-list") < at("codesign", "--force"));
    assert!(fake.0.borrow().keychains[&release].partition);
}

/// A password file another run put there first is never replaced and no partial file is
/// left beside it; a malformed one no keychain uses is moved aside, never kept or "restored".
#[test]
fn the_password_file_is_never_replaced_and_a_malformed_one_is_moved_aside() {
    let (_dir, home) = home();
    let path = password_path(&home);
    let first = format!("{}\n", "a".repeat(64));
    std::fs::write(&path, &first).unwrap();
    create_password(&home).expect_err("a password file is already there");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), first);
    let dir = path.parent().unwrap();
    let names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["release-keychain.pass"], "no partial file is left");

    std::fs::write(&path, "half").unwrap();
    let fake = Fake::new(World::new(SHA1));
    let refused = Keychains::new(&home, &fake)
        .install_pair(&home.join("c.cer"), &home.join("k.key"))
        .expect_err("a malformed password file is not used");
    assert!(refused.fix.contains(".bad'"), "{refused:?}");
    for word in ["backup", "keep"] {
        assert!(!refused.fix.contains(word), "{refused:?}");
    }
    assert!(fake.calls("security", "create-keychain").is_empty());
}

/// A release-keychain path that is a link to the login keychain is refused before any call
/// is made, so nothing is imported into the login keychain through it.
#[test]
fn a_release_keychain_path_leading_to_login_is_refused() {
    let (_dir, home) = home();
    let login = login_keychain_path(&home);
    std::fs::write(&login, b"").unwrap();
    let mut world = World::new(SHA1);
    world.keychains.insert(login.clone(), Kc::default());
    world.search_list = vec![login.clone()];
    let fake = Fake::new(world);
    let kc = Keychains::new(&home, &fake);
    std::os::unix::fs::symlink(&login, kc.release()).unwrap();
    let refused = kc
        .install_pair(&home.join("c.cer"), &home.join("k.key"))
        .expect_err("a link to login");
    assert!(
        refused.what.contains("login or System keychain"),
        "{refused:?}"
    );
    assert!(kc.install_certificate(&home.join("issuer.cer")).is_err());
    assert!(fake.writes().is_empty(), "{:?}", fake.writes());
    assert_eq!(fake.0.borrow().keychains[&login], Kc::default());
}

/// A release path aliases an owned scratch login keychain. The model accepts the stored
/// fixture password and exposes its identity at that alias, as security would after
/// following the link; the path guard must refuse before either operation is attempted.
fn release_alias() -> (aterm_tempfile::TempDir, PathBuf, Fake) {
    let (dir, home) = home();
    let login = login_keychain_path(&home);
    let release = keychain_path(&home);
    std::fs::write(&login, b"scratch login keychain").unwrap();
    std::os::unix::fs::symlink(&login, &release).unwrap();
    let password = "0".repeat(64);
    std::fs::write(password_path(&home), format!("{password}\n")).unwrap();
    let mut world = World::new(SHA1);
    world.keychains.insert(
        release.clone(),
        Kc {
            password,
            locked: true,
            partition: false,
            ..Kc::holding(SHA1)
        },
    );
    world.search_list = vec![release];
    (dir, home, Fake::new(world))
}

/// A matching password never grants leave to unlock or change a protected target through
/// the release path, in either a real signing pass or a look.
#[test]
fn signing_a_release_alias_to_login_is_refused_before_unlock() {
    let (_dir, home, fake) = release_alias();
    let kc = Keychains::new(&home, &fake);
    let mut failures = Vec::new();
    for leave in [Leave::Write, Leave::Look] {
        fake.0.borrow_mut().log.clear();
        let result = kc.signing_keychain(TEAM, SHA1, leave);
        let unlocks = fake.calls("security", "unlock-keychain").len();
        let partitions = fake.calls("security", "set-key-partition-list").len();
        if result.is_ok() || unlocks != 0 || partitions != 0 {
            failures.push(format!(
                "{leave:?}: expected refusal before calls, got {result:?}, \
                 unlocks={unlocks}, partition writes={partitions}"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));

    let (_dir, home) = self::home();
    let login = login_keychain_path(&home);
    let release = keychain_path(&home);
    std::fs::write(&login, b"scratch login keychain").unwrap();
    std::fs::hard_link(&login, &release).unwrap();
    let password = "0".repeat(64);
    std::fs::write(password_path(&home), format!("{password}\n")).unwrap();
    let mut world = World::new(SHA1);
    world.keychains.insert(
        release.clone(),
        Kc {
            password,
            locked: true,
            partition: false,
            ..Kc::holding(SHA1)
        },
    );
    world.search_list = vec![release];
    let fake = Fake::new(world);
    let kc = Keychains::new(&home, &fake);
    let refused = kc
        .signing_keychain(TEAM, SHA1, Leave::Write)
        .expect_err("hard link to login must be refused");
    assert!(refused.contains("login or System keychain"), "{refused}");
    assert!(fake.calls("security", "unlock-keychain").is_empty());
}

/// Whichever `security` step fails, the refusal says which, never the password itself; the
/// password's `Debug` is redacted.
#[test]
fn no_refusal_carries_the_password() {
    for (step, said_as) in [
        ("create-keychain", "security create-keychain"),
        ("unlock-keychain", "does not unlock"),
        ("set-keychain-settings", "security set-keychain-settings"),
        ("import", "security import (certificate)"),
        ("set-key-partition-list", "security set-key-partition-list"),
        ("list-keychains", "security list-keychains -d user"),
    ] {
        let (_dir, home) = home();
        let mut world = World::new(SHA1);
        world.search_list = vec![login_keychain_path(&home)];
        world.fail = Some(step);
        let fake = Fake::new(world);
        let refused = Keychains::new(&home, &fake)
            .install_pair(&home.join("c.cer"), &home.join("k.key"))
            .expect_err(step);
        let password = read_password(&home)
            .expect("readable")
            .expect("written before any keychain is made");
        let secret = password.0.as_str();
        let said = format!("{} {} {refused:?}", refused.what, refused.fix);
        assert!(refused.what.contains(said_as), "{step}: {said}");
        assert!(!said.contains(secret), "{step} leaked the password: {said}");
        assert!(!format!("{password:?}").contains(secret));
    }
}

/// A state, what the search list holds, the test signature's outcome, and the row's kind.
type Case<'a> = (
    State,
    &'a dyn Fn() -> Option<String>,
    &'a dyn Fn(&str) -> Result<Proof, String>,
    &'a str,
);

/// Each state reads as one line whose fix runs provision and never asks for the login
/// password.
#[test]
fn each_state_says_what_to_run() {
    let release = Path::new("/h/Library/Keychains/aterm-release.keychain-db");
    let none = || None;
    let legacy = || Some(SHA1.to_string());
    let passes = |_: &str| Ok(Proof::Signed);
    let opened = |_: &str| Ok(Proof::Opened);
    let fails = |_: &str| Err("errSecInternalComponent".to_string());
    let open = |listed| State::Open {
        identities: vec![SHA1.into()],
        listed,
    };
    let locked = |holds| State::Locked {
        refusal: Refusal::new("locked", "move it aside"),
        holds,
    };
    let empty = State::Open {
        identities: Vec::new(),
        listed: true,
    };
    let cases: [Case<'_>; 13] = [
        (State::Missing, &none, &passes, "todo"),
        (State::Missing, &legacy, &passes, "ready"),
        (locked(false), &none, &passes, "blocked"),
        (locked(false), &legacy, &passes, "ready"),
        (locked(true), &none, &passes, "blocked"),
        (locked(true), &legacy, &passes, "blocked"),
        (empty.clone(), &none, &passes, "todo"),
        (empty, &legacy, &passes, "ready"),
        (open(true), &none, &fails, "blocked"),
        (open(false), &none, &passes, "todo"),
        (open(true), &none, &passes, "ready"),
        (open(false), &none, &opened, "todo"),
        (open(true), &none, &opened, "ready"),
    ];
    for (state, elsewhere, probe, want) in cases {
        let row = judge("m9", release, state.clone(), elsewhere, probe);
        let (kind, text) = match &row {
            Row::Ready(line) => ("ready", line.clone()),
            Row::Todo { what, next } => ("todo", format!("{what} {next}")),
            Row::Blocked { what, fix } => ("blocked", format!("{what} {fix}")),
        };
        assert_eq!(kind, want, "{state:?}: {row:?}");
        assert!(
            !text.contains("login password") || text.contains("never your login"),
            "{text}"
        );
        if kind != "ready" && !matches!(state, State::Locked { .. }) {
            assert!(text.contains("provision --id m9"), "{text}");
        }
    }
    match judge("m9", release, open(true), &none, &passes) {
        Row::Ready(line) => {
            for word in ["open", SHA1, "partition list set", "on the search list"] {
                assert!(line.contains(word), "{line}");
            }
        }
        other => panic!("{other:?}"),
    }
    match judge("m9", release, open(true), &none, &opened) {
        Row::Ready(line) => {
            assert!(line.contains(OPENED_NOTE), "{line}");
            assert!(!line.contains("partition list set"), "{line}");
        }
        other => panic!("{other:?}"),
    }
}

/// Each state of the password file gets the fix that gets past it. With no keychain: one
/// that is not provision's hex (text or not) is moved aside, one that cannot be read is
/// made readable and kept with any keychain moved aside. With the keychain there, a
/// malformed one is restored or moved aside together with the keychain.
#[test]
fn each_password_file_state_gets_a_fix_that_gets_past_it() {
    let make = |kind: &str| {
        let (dir, home) = home();
        let path = password_path(&home);
        match kind {
            "binary" => std::fs::write(&path, [0xff, 0xfe, 0x00, 0x9f]).unwrap(),
            "text" => std::fs::write(&path, "half\n").unwrap(),
            _ => std::fs::create_dir(&path).unwrap(),
        }
        (dir, home)
    };
    for kind in ["binary", "text"] {
        let (_dir, home) = make(kind);
        assert!(
            matches!(stored_password(&home), Stored::Malformed),
            "{kind}"
        );
        let fake = Fake::new(World::new(SHA1));
        let refused = Keychains::new(&home, &fake)
            .install_pair(&home.join("c.cer"), &home.join("k.key"))
            .expect_err(kind);
        assert!(refused.fix.contains(".bad'"), "{kind}: {refused:?}");
        assert!(!refused.fix.contains("readable"), "{kind}: {refused:?}");
        assert!(fake.calls("security", "create-keychain").is_empty());
    }

    let (_dir, home) = make("unreadable");
    assert!(matches!(stored_password(&home), Stored::Unreadable(_)));
    let fake = Fake::new(World::new(SHA1));
    let refused = Keychains::new(&home, &fake)
        .install_pair(&home.join("c.cer"), &home.join("k.key"))
        .expect_err("a file that cannot be read");
    for word in ["readable", "move the two together"] {
        assert!(refused.fix.contains(word), "{refused:?}");
    }
    assert!(fake.calls("security", "create-keychain").is_empty());

    let (_dir, home) = self::home();
    let outside = home.join("outside.pass");
    std::fs::write(&outside, "0".repeat(64)).unwrap();
    std::os::unix::fs::symlink(&outside, password_path(&home)).unwrap();
    assert!(matches!(stored_password(&home), Stored::Unreadable(_)));

    let (_dir, home) = self::home();
    let mut world = World::new(SHA1);
    world.search_list = vec![login_keychain_path(&home)];
    let fake = Fake::new(world);
    let kc = Keychains::new(&home, &fake);
    kc.install_pair(&home.join("c.cer"), &home.join("k.key"))
        .expect("install");
    std::fs::write(password_path(&home), "half\n").unwrap();
    let refused = kc.unlock().expect_err("a malformed file opens nothing");
    let pass = password_path(&home).display().to_string();
    let shown = kc.release().display().to_string();
    assert!(refused.fix.contains("backup"), "{refused:?}");
    for moved in [&pass, &shown] {
        assert!(
            refused.fix.contains(&format!("mv '{moved}'")),
            "both move aside together: {refused:?}"
        );
    }
}

/// A password, made or read back from its file, holds its text where dropping it zeroes the
/// whole buffer ([`Zeroizing`]), and both spell the same password. This is a check on the
/// type: with a bare `String` in the field it does not compile. It does not watch freed
/// memory; that dropping a [`Zeroizing`] zeroes it is the `zeroize` crate's own claim.
#[test]
fn a_password_made_or_read_is_held_in_storage_that_zeroes_on_drop() {
    fn zeroed_on_drop(text: &Zeroizing<String>) -> &str {
        text.as_str()
    }
    let (_dir, home) = home();
    let made = create_password(&home).expect("made");
    let Stored::Password(read) = stored_password(&home) else {
        panic!("the password just written reads back");
    };
    assert_eq!(zeroed_on_drop(&read.0), zeroed_on_drop(&made.0));
    assert!(aterm_codec::hex::is_hex(zeroed_on_drop(&made.0), 64));
}

/// A password write that fails part-way leaves no password file and no partial file, so
/// the next run writes a whole one.
#[test]
fn a_password_write_that_fails_part_way_leaves_nothing() {
    let (_dir, home) = home();
    let refused = create_password_with(&home, &|file, bytes| {
        std::io::Write::write_all(file, &bytes[..10])?;
        Err(std::io::Error::other("no space left on device"))
    })
    .expect_err("the write failed");
    assert!(refused.what.contains("no space left"), "{refused:?}");
    let path = password_path(&home);
    assert!(!path.exists(), "no partial password file");
    let left: Vec<_> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert!(left.is_empty(), "{left:?}");

    let written = create_password(&home).expect("the next run writes one");
    assert!(matches!(
        stored_password(&home),
        Stored::Password(stored) if stored.0 == written.0
    ));

    let linked_root = aterm_tempfile::tempdir().unwrap();
    let linked_home = linked_root.path();
    let elsewhere = linked_home.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::create_dir_all(linked_home.join(".aterm")).unwrap();
    std::os::unix::fs::symlink(&elsewhere, linked_home.join(".aterm/apple")).unwrap();
    create_password(linked_home).expect_err("a symlinked .aterm/apple directory is refused");
    assert!(!elsewhere.join("release-keychain.pass").exists());
}
