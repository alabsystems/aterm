// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::fake::{Fake, Kc, World};
use super::*;

fn scratch() -> (aterm_tempfile::TempDir, PathBuf) {
    let dir = aterm_tempfile::tempdir().expect("scratch dir");
    let path = std::fs::canonicalize(dir.path()).expect("canonical scratch");
    (dir, path)
}

fn lock_calls(fake: &Fake) -> Vec<String> {
    fake.0
        .borrow()
        .log
        .iter()
        .filter(|c| c[0] == "security" && c[1].ends_with("lock-keychain"))
        .map(|c| c[1..].join(" "))
        .collect()
}

fn mutating_calls(fake: &Fake) -> Vec<String> {
    fake.0
        .borrow()
        .log
        .iter()
        .filter(|c| {
            c[0] == "security"
                && [
                    "lock-keychain",
                    "unlock-keychain",
                    "create-keychain",
                    "delete-keychain",
                ]
                .contains(&c[1].as_str())
        })
        .map(|c| c.join(" "))
        .collect()
}

#[test]
fn arming_an_absent_keychain_is_silent_and_calls_nothing() {
    let (_dir, root) = scratch();
    let fake = Fake::new(World::default());
    assert_eq!(arm(&root.join("absent.keychain-db"), true, &fake), Ok(None));
    assert!(fake.0.borrow().log.is_empty());
}

#[test]
fn create_makes_one_identity_that_arming_finds_after_a_lock() {
    let (_dir, root) = scratch();
    let k = root.join("dev.keychain-db");
    let fake = Fake::new(World::default());
    create(&k, false, &fake).expect("create");
    assert!(k.is_file());
    {
        let world = fake.0.borrow();
        let kc = &world.keychains[&k];
        assert_eq!(kc.password, "", "the empty password is the ownership proof");
        assert_eq!(kc.identities.len(), 1);
        assert!(
            world.search_list.is_empty(),
            "--keychain never touches the list"
        );
    }
    let shown = k.display().to_string();
    let argv = |words: &[&str]| -> Vec<Vec<String>> {
        vec![words.iter().map(|w| (*w).to_string()).collect()]
    };
    assert_eq!(
        fake.calls("security", "set-keychain-settings"),
        argv(&["security", "set-keychain-settings", &shown])
    );
    assert_eq!(
        fake.calls("security", "set-key-partition-list"),
        argv(&[
            "security",
            "set-key-partition-list",
            "-S",
            "apple-tool:,apple:,codesign:",
            "-s",
            "-l",
            NAME,
            "-k",
            "",
            &shown,
        ])
    );
    assert_eq!(
        fake.calls("codesign", "--force"),
        Vec::<Vec<String>>::new(),
        "off the search list codesign cannot sign with it, so the self-check is skipped"
    );
    let leaf = fake.0.borrow().keychains[&k].identities[0].clone();

    assert_eq!(arm(&k, false, &fake), Ok(Some(leaf.clone())));
    for _ in 0..2 {
        fake.0.borrow_mut().keychains.get_mut(&k).unwrap().locked = true;
        fake.0.borrow_mut().log.clear();
        assert_eq!(arm(&k, false, &fake), Ok(Some(leaf.clone())));
        assert_eq!(
            lock_calls(&fake),
            [format!("unlock-keychain -p  {}", k.display())]
        );
    }

    create(&k, false, &fake).expect("create again");
    assert_eq!(
        fake.0.borrow().keychains[&k].identities,
        std::slice::from_ref(&leaf)
    );
    assert!(
        fake.calls("security", "import").is_empty(),
        "a second create imports nothing"
    );
}

#[test]
fn a_refused_bare_unlock_is_healed_by_lock_and_reopen() {
    let (_dir, root) = scratch();
    let k = root.join("dev.keychain-db");
    let fake = Fake::new(World::default());
    create(&k, false, &fake).expect("create");
    let leaf = fake.0.borrow().keychains[&k].identities[0].clone();
    {
        let mut world = fake.0.borrow_mut();
        world.keychains.get_mut(&k).unwrap().locked = true;
        world.refuse_unlocks = 1;
        world.log.clear();
    }
    assert_eq!(arm(&k, false, &fake), Ok(Some(leaf)));
    let shown = k.display();
    assert_eq!(
        lock_calls(&fake),
        [
            format!("unlock-keychain -p  {shown}"),
            format!("lock-keychain {shown}"),
            format!("unlock-keychain -p  {shown}"),
        ]
    );
    assert!(!fake.0.borrow().keychains[&k].locked, "left open");
}

#[test]
fn a_keychain_that_is_not_ours_is_refused_by_every_mode() {
    let (_dir, root) = scratch();
    let theirs = root.join("theirs.keychain-db");
    std::fs::write(&theirs, b"").unwrap();
    let mut world = World::default();
    world.keychains.insert(
        theirs.clone(),
        Kc {
            password: "not-ours".into(),
            locked: true,
            identities: Vec::new(),
        },
    );
    let fake = Fake::new(world);
    let refused = arm(&theirs, false, &fake).unwrap_err();
    assert!(
        refused.contains("did not unlock with the empty password"),
        "{refused}"
    );
    assert!(refused.contains("now LOCKED"), "{refused}");

    fake.0
        .borrow_mut()
        .keychains
        .get_mut(&theirs)
        .unwrap()
        .locked = false;
    let refused = create(&theirs, false, &fake).unwrap_err();
    assert!(refused.contains("not ours"), "{refused}");
    assert!(fake.calls("security", "import").is_empty());
    assert!(fake.0.borrow().keychains[&theirs].identities.is_empty());

    fake.0
        .borrow_mut()
        .keychains
        .get_mut(&theirs)
        .unwrap()
        .locked = false;
    let refused = remove(&theirs, &fake).unwrap_err();
    assert!(refused.contains("not ours"), "{refused}");
    assert!(
        theirs.is_file(),
        "remove deleted a keychain that is not ours"
    );
    assert!(fake.calls("security", "delete-keychain").is_empty());
}

#[test]
fn protected_keychains_are_refused_before_anything_touches_them() {
    let (_dir, root) = scratch();
    let system = root.join("System.keychain");
    std::fs::write(&system, b"").unwrap();
    let alias = root.join("alias.keychain-db");
    std::os::unix::fs::symlink(&system, &alias).unwrap();
    let mut spellings = vec![system.clone(), alias];
    let upper = root.join("SYSTEM.KEYCHAIN");
    if upper.exists() {
        spellings.push(upper);
    }
    for path in &spellings {
        let fake = Fake::new(World::default());
        let modes: [(&str, Result<(), String>); 3] = [
            ("arm", arm(path, true, &fake).map(|_| ())),
            ("create", create(path, true, &fake)),
            ("remove", remove(path, &fake)),
        ];
        for (mode, result) in modes {
            let refused = result.unwrap_err();
            assert!(
                refused.contains("is the login or default keychain"),
                "{mode} {}: {refused}",
                path.display()
            );
        }
        assert_eq!(
            mutating_calls(&fake),
            Vec::<String>::new(),
            "{}",
            path.display()
        );
        assert!(system.is_file() && path.exists());
    }
    let fresh = root.join("fresh");
    std::fs::create_dir(&fresh).unwrap();
    for name in ["login.keychain-db", "login.keychain"] {
        let fake = Fake::new(World::default());
        let refused = create(&fresh.join(name), true, &fake).unwrap_err();
        assert!(
            refused.contains("is the login or default keychain"),
            "{refused}"
        );
        assert!(fake.calls("security", "create-keychain").is_empty());
        assert_eq!(std::fs::read_dir(&fresh).unwrap().count(), 0);
    }
}

#[test]
fn the_guard_sees_one_keychain_through_every_spelling() {
    let (_dir, g) = scratch();
    std::fs::create_dir(g.join("sub")).unwrap();
    let work = g.join("work.keychain-db");
    std::fs::write(&work, b"").unwrap();
    std::fs::write(g.join("other.keychain-db"), b"").unwrap();
    std::fs::hard_link(&work, g.join("hard.keychain-db")).unwrap();
    std::os::unix::fs::symlink(&work, g.join("sym.keychain-db")).unwrap();
    std::os::unix::fs::symlink("../sym.keychain-db", g.join("sub/chain.keychain-db")).unwrap();
    std::os::unix::fs::symlink("gone.keychain-db", g.join("dangling.keychain-db")).unwrap();
    let named = [work.clone()];
    for spelling in [
        "work.keychain-db",
        "hard.keychain-db",
        "sym.keychain-db",
        "sub/chain.keychain-db",
        "sub/../work.keychain-db",
        "WORK.KEYCHAIN-DB",
    ] {
        assert!(protected(&g.join(spelling), &named), "{spelling}");
    }
    let gone = [g.join("gone.keychain-db")];
    for spelling in ["GONE.KEYCHAIN-DB", "dangling.keychain-db"] {
        assert!(protected(&g.join(spelling), &gone), "{spelling}");
    }
    assert!(!protected(&g.join("other.keychain-db"), &named));
    for login in ["LOGIN.KEYCHAIN-DB", "Login.Keychain", "login.keychain-db"] {
        assert!(protected(&g.join("fresh").join(login), &[]), "{login}");
    }
    assert!(!protected(&g.join("aterm-dev.keychain-db"), &named));
}

#[test]
fn the_search_list_is_rewritten_in_order_and_never_from_an_empty_read() {
    let k = Path::new("/k/aterm-dev.keychain-db");
    let login = PathBuf::from("/Users//nobody/Library/Keychains/login.keychain-db");
    let other = PathBuf::from("/Library/Keychains/Other.keychain");
    assert!(
        search_list_with(&[], k)
            .unwrap_err()
            .contains("read back empty")
    );
    assert_eq!(
        search_list_with(&[login.clone(), other.clone()], k),
        Ok(Some(vec![login.clone(), other.clone(), k.to_path_buf()]))
    );
    assert_eq!(
        search_list_with(&[login.clone(), k.to_path_buf()], k),
        Ok(None)
    );

    let (_dir, root) = scratch();
    let k = root.join("aterm-dev.keychain-db");
    let fake = Fake::new(World::default());
    create(&k, false, &fake).expect("create");
    let refused = arm(&k, true, &fake).unwrap_err();
    assert!(refused.contains("read back empty"), "{refused}");
    assert!(refused.contains("could not put"), "{refused}");
    let rewrites = |fake: &Fake| {
        fake.calls("security", "list-keychains")
            .into_iter()
            .filter(|c| c.contains(&"-s".to_string()))
            .collect::<Vec<_>>()
    };
    assert!(
        rewrites(&fake).is_empty(),
        "an empty read is never written back"
    );

    fake.0.borrow_mut().search_list = vec![login.clone(), other.clone()];
    let leaf = fake.0.borrow().keychains[&k].identities[0].clone();
    assert_eq!(arm(&k, true, &fake), Ok(Some(leaf)));
    assert_eq!(fake.0.borrow().search_list, [login, other, k.clone()]);
    assert_eq!(rewrites(&fake).len(), 1);
}

#[test]
fn create_on_the_search_list_signs_its_self_check_with_the_new_leaf() {
    let (_dir, root) = scratch();
    let k = root.join("aterm-dev.keychain-db");
    let login = root.join("login.keychain-db");
    let fake = Fake::new(World {
        search_list: vec![login.clone()],
        ..World::default()
    });
    create(&k, true, &fake).expect("create");
    let leaf = fake.0.borrow().keychains[&k].identities[0].clone();
    assert_eq!(fake.0.borrow().search_list, [login, k.clone()]);
    let signed = fake.calls("codesign", "--force");
    assert_eq!(signed.len(), 1);
    assert_eq!(signed[0][3], leaf, "signed by the leaf's hash");
    assert_eq!(fake.calls("codesign", "--verify").len(), 1);

    fake.0.borrow_mut().requirement = Some(
        "designated => identifier \"com.aterm.aterm.dev\" and certificate leaf = \
         H\"0000000000000000000000000000000000000000\"\n"
            .into(),
    );
    let refused = create(&k, true, &fake).unwrap_err();
    assert!(refused.contains("self-check FAILED"), "{refused}");
    assert!(!names_leaf("# designated => cdhash H\"240c\"\n", &leaf));
    let anchored = |clause: &str| {
        format!(
            "designated => identifier \"{DEV_BUNDLE_ID}\" and {clause} = H\"{}\"\n",
            leaf.to_ascii_lowercase()
        )
    };
    assert!(names_leaf(&anchored("certificate leaf"), &leaf));
    assert!(!names_leaf(&anchored("certificate root"), &leaf));
}

#[test]
fn a_service_that_does_not_answer_stops_in_one_line() {
    let (_dir, root) = scratch();
    let k = root.join("dev.keychain-db");
    let fake = Fake::new(World::default());
    create(&k, false, &fake).expect("create");
    fake.0.borrow_mut().hang_on = Some("security unlock-keychain".into());
    let stopped = arm(&k, false, &fake).unwrap_err();
    assert!(!stopped.contains('\n'), "{stopped}");
    assert!(
        stopped.contains("did not answer within 60 s (trustd/securityd); nothing was changed"),
        "{stopped}"
    );

    let half = root.join("half.keychain-db");
    fake.0.borrow_mut().hang_on = Some("security set-keychain-settings".into());
    let stopped = create(&half, false, &fake).unwrap_err();
    assert!(!stopped.contains('\n'), "{stopped}");
    assert!(
        stopped.contains(&format!(
            "the keychain this run had begun ({}) was deleted",
            half.display()
        )),
        "{stopped}"
    );
    assert!(!half.exists(), "no half-made keychain is left");
    assert!(!fake.0.borrow().keychains.contains_key(&half));
}

#[test]
fn remove_deletes_only_ours_and_a_second_remove_is_a_no_op() {
    let (_dir, root) = scratch();
    let k = root.join("dev.keychain-db");
    let fake = Fake::new(World::default());
    create(&k, false, &fake).expect("create");
    fake.0.borrow_mut().keychains.get_mut(&k).unwrap().locked = true;
    remove(&k, &fake).expect("remove");
    assert!(!k.exists() && fake.0.borrow().keychains.is_empty());
    assert_eq!(
        arm(&k, false, &fake),
        Ok(None),
        "nothing to arm after a remove"
    );
    fake.0.borrow_mut().log.clear();
    remove(&k, &fake).expect("a second remove");
    assert!(fake.0.borrow().log.is_empty());
}

#[test]
fn the_identity_listing_matches_the_name_exactly() {
    let hash = "0123456789ABCDEF0123456789ABCDEF01234567";
    let listing = format!(
        "\nPolicy: Code Signing\n  Matching identities\n  1) {hash} \"aterm dev 2\"\n  \
         2) {hash} \"aterm dev\" (CSSMERR_TP_NOT_TRUSTED)\n  3) {} \"aterm dev\"\n     3 \
         identities found\n\n  Valid identities only\n     0 valid identities found\n",
        hash.to_ascii_lowercase()
    );
    assert_eq!(identity_hashes(&listing), [hash]);
    assert!(identity_hashes("").is_empty());
}

/// The certificate the real /usr/bin/openssl makes from [`request_config`] is a
/// code-signing leaf: CN "aterm dev", the code-signing EKU, digitalSignature and
/// CA:false.
#[cfg(target_os = "macos")]
#[test]
fn the_certificate_is_a_code_signing_leaf() {
    let (_dir, root) = scratch();
    let k = Keychain {
        path: &root.join("unused.keychain-db"),
        run: &Bounded,
    };
    let (p12, password) = generate(&k, &root).expect("generate");
    assert!(p12.is_file() && password.len() == 32);
    let mut cmd = aterm_uds::spawnfd::command(OPENSSL);
    cmd.args(["x509", "-noout", "-text", "-in"])
        .arg(root.join("cert.pem"));
    let text = Bounded.run(&mut cmd, WAIT).expect("openssl x509");
    let text = String::from_utf8_lossy(&text.stdout);
    for want in [
        "CN=aterm dev",
        "Code Signing",
        "Digital Signature",
        "CA:FALSE",
    ] {
        assert!(
            text.contains(want),
            "the certificate lacks {want:?}: {text}"
        );
    }
}
