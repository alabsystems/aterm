// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::release_keychain::fake::{Fake, Kc, World};

const COMMAND_CHILD: &str = "ATERM_APPLE_COMMAND_TEST_CHILD";

fn command_child(mode: &str) -> aterm_uds::spawnfd::Command {
    let mut command =
        aterm_uds::spawnfd::command(std::env::current_exe().expect("this test executable"));
    command
        .args([
            "--exact",
            "apple::tests::signing_command_child",
            "--nocapture",
        ])
        .env(COMMAND_CHILD, mode);
    command
}

#[test]
fn signing_command_child() {
    let Some(mode) = std::env::var_os(COMMAND_CHILD) else {
        return;
    };
    let mut input = Vec::new();
    std::io::stdin()
        .read_to_end(&mut input)
        .expect("closed stdin");
    assert!(
        input.is_empty(),
        "provisioning must not consume terminal input"
    );
    match mode.to_str().expect("test mode") {
        "ok" => {
            println!("ordinary command output");
            eprintln!("ordinary command diagnostic");
        }
        "fail" => {
            eprintln!("fixture signing failure\nmore detail");
            std::process::exit(7);
        }
        "block" => loop {
            std::thread::park();
        },
        _ => panic!("unknown fixture mode"),
    }
}

#[test]
fn local_command_success_and_failure_keep_their_exit_meaning() {
    assert!(run_command(&mut command_child("ok"), COMMAND_WAIT).is_ok());
    assert_eq!(
        run_command(&mut command_child("fail"), COMMAND_WAIT),
        Err("fixture signing failure".to_string())
    );
    let absent = aterm_tempfile::tempdir().expect("tempdir");
    let error = run_command(
        &mut aterm_uds::spawnfd::command(absent.path().join("absent")),
        COMMAND_WAIT,
    )
    .expect_err("spawn failure");
    assert!(error.contains("could not start"), "{error}");
    assert!(!error.contains("requested termination"), "{error}");
}

/// A signing command that never answers is stopped at the deadline it was given, and the
/// error names that deadline. Stopping and reaping it is `output_within`'s, which aterm-uds
/// proves (`a_child_past_its_deadline_is_killed_and_reaped`); a child that never finishes
/// meets a short deadline however slowly it starts, so this waits out no minute.
#[test]
fn a_signing_command_that_never_answers_is_stopped_at_its_deadline() {
    let error = run_command(&mut command_child("block"), Duration::from_secs(1))
        .expect_err("the blocked signing command must not pass");
    assert!(error.contains("did not finish within 1 s"), "{error}");
}

/// A read-only look names provision, with the operator's folder, as the step when it can
/// make the on-disk certificate an identity; a refused certificate (named), no current one,
/// or no key is the owner's errand at Apple, and a folder or key it could not read blocks.
#[test]
fn a_key_whose_certificate_is_on_disk_is_provisions_step_not_an_errand() {
    let key = Path::new("/nonexistent/devid-m9.key");
    let fake = Fake::new(World::new(&"0".repeat(40)));
    let kc = Keychains::new(Path::new("/nonexistent"), &fake);
    let blocked = |found: OnDisk, cert_dir: Option<&Path>| match read_only(
        &kc,
        "m9",
        Leave::Look,
        Vec::new(),
        Some(key),
        cert_dir,
        &|_| found.clone(),
    ) {
        Outcome::Blocked { what, fix } => {
            assert!(what.contains("'m9'") && what.contains("keychain"), "{what}");
            fix
        }
        _ => panic!("{found:?} is provision's step, not an errand"),
    };
    let fix = blocked(OnDisk::Unimported, None);
    assert!(
        fix.contains("provision --id m9`, which imports") && !fix.contains("--cert-dir"),
        "{fix}"
    );
    let fix = blocked(OnDisk::Unchained, Some(Path::new("/x/Downloads")));
    assert!(
        fix.contains("provision --id m9 --cert-dir /x/Downloads`, which installs"),
        "{fix}"
    );
    for (key, found) in [
        (
            Some(key),
            OnDisk::Refused(PathBuf::from("/x/Downloads/old.cer")),
        ),
        (Some(key), OnDisk::Nothing),
        (None, OnDisk::Unimported),
    ] {
        assert!(
            matches!(
                read_only(&kc, "m9", Leave::Look, Vec::new(), key, None, &|_| found
                    .clone()),
                Outcome::Todo { .. }
            ),
            "{key:?} {found:?}"
        );
    }
    for (why, fix) in [
        ("/x/locked: Permission denied (os error 13)", "readable"),
        ("could not read the public half of /k", "openssl rsa -in"),
    ] {
        match read_only(&kc, "m9", Leave::Look, Vec::new(), Some(key), None, &|_| {
            OnDisk::Unlooked(why.into())
        }) {
            Outcome::Blocked { what, fix: said } => {
                assert!(what.contains(why) && said.contains(fix), "{what} / {said}");
            }
            _ => panic!("what a real run cannot read stops the check too: {why}"),
        }
    }
    let refused = OnDisk::Refused(PathBuf::from("/x/Downloads/old.cer"));
    match read_only(
        &kc,
        "m9",
        Leave::Look,
        Vec::new(),
        Some(key),
        Some(Path::new("/x/Downloads")),
        &|_| refused.clone(),
    ) {
        Outcome::Todo { what, next } => {
            assert!(what.contains("/x/Downloads/old.cer"), "{what}");
            assert!(
                next.contains("provision --id m9 --cert-dir /x/Downloads`"),
                "{next}"
            );
        }
        _ => panic!("a refused certificate is an errand at Apple"),
    }
}

/// A certificate on disk is judged by its key, its expiry and the keychain; a refused or
/// expired one is passed over for another for the key, and the first refused is named when
/// every one is refused.
#[test]
fn a_certificate_on_disk_is_judged_by_its_key_its_end_and_the_keychain() {
    let scratch = aterm_tempfile::tempdir().expect("tempdir");
    let dir = scratch.path();
    let fake = Fake::new(World::new(&"0".repeat(40)));
    let kc = Keychains::new(dir, &fake);
    let on_disk = |key: &Path, watch: &Watch, invalid: &[String]| on_disk(&kc, key, watch, invalid);
    let issuer_in_keychain = |leaf: &str| issuer_in_keychain(&kc, leaf);
    let path = |name: &str| dir.join(name).to_string_lossy().into_owned();
    let openssl = |args: &[&str]| {
        assert!(
            capture("/usr/bin/openssl", args, None).is_some(),
            "openssl {args:?}"
        );
    };
    for key in ["k.pem", "other.pem"] {
        openssl(&["genrsa", "-out", &path(key), "2048"]);
    }
    let pem = path("c.pem");
    openssl(&[
        "req",
        "-new",
        "-x509",
        "-key",
        &path("k.pem"),
        "-subj",
        "/CN=probe",
        "-days",
        "30",
        "-out",
        &pem,
    ]);
    openssl(&[
        "x509",
        "-in",
        &pem,
        "-outform",
        "DER",
        "-out",
        &path("c.cer"),
    ]);
    let watch = Watch::new(None, Some(dir));
    let key = dir.join("k.pem");
    let cert = MatchedCert {
        path: dir.join("c.cer"),
        form: "DER",
    };
    let sha1 = cert_sha1(&cert).expect("a fingerprint");
    assert_eq!(
        [&[][..], &["0".repeat(40)][..]].map(|invalid| on_disk(&key, &watch, invalid)),
        [OnDisk::Unimported, OnDisk::Unimported]
    );
    assert_eq!(
        on_disk(&key, &watch, &[sha1.to_ascii_lowercase()]),
        OnDisk::Unchained
    );
    let seen =
        |watch: &Watch, invalid: &[String], secs, issuer: &dyn Fn(&str) -> Option<String>| {
            let look = look(&key, watch, invalid, secs, issuer).expect("look");
            (look.cert.map(|cert| cert.path), look.judged)
        };
    assert_eq!(
        seen(&watch, &[], 10 * 365 * 24 * 60 * 60, &issuer_in_keychain),
        (None, OnDisk::Nothing)
    );
    assert_eq!(
        on_disk(&dir.join("other.pem"), &watch, std::slice::from_ref(&sha1)),
        OnDisk::Nothing
    );
    let found = |_: &str| Some("issuer".to_owned());
    let refused = OnDisk::Refused(cert.path.clone());
    assert_eq!(
        seen(&watch, std::slice::from_ref(&sha1), 0, &found),
        (None, refused.clone())
    );
    assert_eq!(
        seen(&watch, std::slice::from_ref(&sha1), 0, &|_| None),
        (Some(cert.path.clone()), OnDisk::Unchained)
    );

    // The renewal lives in a folder read after the first, so the order is the test's.
    let later = dir.join("later");
    std::fs::create_dir(&later).expect("dir");
    let renewed = path("d.pem");
    openssl(&[
        "req",
        "-new",
        "-x509",
        "-key",
        &path("k.pem"),
        "-subj",
        "/CN=renewed",
        "-days",
        "400",
        "-out",
        &renewed,
    ]);
    let renewal = later.join("d.cer");
    openssl(&[
        "x509",
        "-in",
        &renewed,
        "-outform",
        "DER",
        "-out",
        &renewal.to_string_lossy(),
    ]);
    let renewed_sha1 = cert_sha1(&MatchedCert {
        path: renewal.clone(),
        form: "DER",
    })
    .expect("a fingerprint");
    let watch = Watch {
        named: Some(dir.to_path_buf()),
        own: Some(later),
    };
    let both = [sha1.clone(), renewed_sha1];
    let sixty_days = 60 * 24 * 60 * 60;
    assert_eq!(
        [
            seen(&watch, std::slice::from_ref(&sha1), 0, &found),
            seen(&watch, &[], sixty_days, &found),
            seen(&watch, &both, 0, &found),
        ],
        [
            (Some(renewal.clone()), OnDisk::Unimported),
            (Some(renewal), OnDisk::Unimported),
            (None, refused),
        ],
        "passed over: refused, ended; every one refused names the first"
    );

    // A folder that cannot be read, with nothing installable elsewhere, is no answer.
    let locked = dir.join("locked");
    let empty = dir.join("empty");
    std::fs::create_dir(&locked).expect("dir");
    std::fs::create_dir(&empty).expect("dir");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    let blind = Watch {
        named: Some(locked.clone()),
        own: Some(empty),
    };
    let blind_look = on_disk(&key, &blind, &[]);
    let missing = dir.join("missing.pem");
    let unreadable_key = on_disk(&missing, &blind, &[]);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    assert!(
        matches!(&blind_look, OnDisk::Unlooked(why) if why.contains("locked")),
        "{blind_look:?}"
    );
    // The key's own error takes the slot-sparing fix, wherever it is reported.
    let OnDisk::Unlooked(why) = unreadable_key else {
        panic!("{unreadable_key:?}");
    };
    assert!(
        matches!(unlooked(why, &missing), Outcome::Blocked { fix, .. } if fix.contains("openssl rsa -in")),
        "a key that cannot be read is proven before a slot is spent"
    );
}

/// A scratch HOME holding a Developer ID pair in `~/.aterm/apple` as the vault fills it
/// (`devid-<id>.key` and `.cer`, no request), and its certificate's SHA-1.
fn vault_filled_home(id: &str) -> (aterm_tempfile::TempDir, PathBuf, String) {
    let scratch = aterm_tempfile::tempdir().expect("scratch HOME");
    let home = std::fs::canonicalize(scratch.path()).expect("canonical scratch");
    let apple = apple_dir_in(&home);
    std::fs::create_dir_all(&apple).expect(".aterm/apple");
    std::fs::create_dir_all(home.join("Library/Keychains")).expect("Keychains");
    let key = key_path(&apple, id).to_string_lossy().into_owned();
    let pem = home.join("cert.pem").to_string_lossy().into_owned();
    let cer = apple.join(format!("devid-{id}.cer"));
    let team = pins::APPLE_TEAM_ID;
    let subject = format!("/CN=Developer ID Application: Placeholder Org ({team})/OU={team}/C=US");
    for args in [
        &["genrsa", "-out", &key, "2048"][..],
        &[
            "req", "-new", "-x509", "-key", &key, "-subj", &subject, "-days", "30", "-out", &pem,
        ],
        &[
            "x509",
            "-in",
            &pem,
            "-outform",
            "DER",
            "-out",
            &cer.to_string_lossy(),
        ],
    ] {
        assert!(
            capture("/usr/bin/openssl", args, None).is_some(),
            "openssl {args:?}"
        );
    }
    let sha1 = cert_sha1(&MatchedCert {
        path: cer,
        form: "DER",
    })
    .expect("a fingerprint");
    (scratch, home, sha1)
}

/// Whether a logged call names the login keychain other than to keep it on the search
/// list where it was.
fn names_login(call: &[String], login: &Path) -> bool {
    let keeps_it = call[1] == "list-keychains"
        && call
            .iter()
            .position(|a| a == "-s")
            .is_some_and(|at| call.get(at + 1).map(String::as_str) == login.to_str());
    !keeps_it && call.iter().any(|a| a.contains("login.keychain"))
}

/// `ship provision` on a fresh Mac the vault filled: the pair goes into the release
/// keychain, made, opened and given its partition list with its own password; the search
/// list keeps login first; the test signature names the release keychain; and no call
/// names the login keychain. A second run makes, imports and rewrites nothing.
#[test]
fn provision_on_a_fresh_home_fills_the_release_keychain_and_never_the_login_one() {
    let (_scratch, home, sha1) = vault_filled_home("m9");
    let login = crate::release_keychain::login_keychain_path(&home);
    let mut world = World::new(&sha1);
    world.keychains.insert(login.clone(), Kc::default());
    world.search_list = vec![login.clone()];
    let fake = Fake::new(world);
    let kc = Keychains::new(&home, &fake);
    let release = kc.release();

    match acquire_in(&kc, "m9", true, Leave::Write, None) {
        Outcome::Ready { ids, .. } => assert_eq!(ids, std::slice::from_ref(&sha1)),
        Outcome::Blocked { what, fix } => panic!("blocked: {what} / {fix}"),
        _ => panic!("a vault-filled pair provisions in one run"),
    }
    {
        let world = fake.0.borrow();
        for call in &world.log {
            assert!(!names_login(call, &login), "names login: {call:?}");
        }
        let filled = &world.keychains[&release];
        assert!(filled.partition && !filled.locked, "{filled:?}");
        assert_eq!((filled.certs.clone(), filled.keys), (vec![sha1.clone()], 1));
        assert_eq!(world.keychains[&login], Kc::default(), "login untouched");
        assert_eq!(world.search_list, [login.clone(), release.clone()]);
    }
    let probes = fake.calls("codesign", "--force");
    assert_eq!(probes.len(), 1, "{probes:?}");
    assert!(
        probes[0]
            .windows(2)
            .any(|w| w[0] == "--keychain" && Path::new(&w[1]) == release),
        "{probes:?}"
    );

    let before = fake.0.borrow().keychains.clone();
    fake.0.borrow_mut().log.clear();
    assert!(matches!(
        acquire_in(&kc, "m9", true, Leave::Write, None),
        Outcome::Ready { .. }
    ));
    assert_eq!(fake.0.borrow().keychains, before);
    for sub in ["create-keychain", "import"] {
        assert!(
            fake.calls("security", sub).is_empty(),
            "a second run ran {sub}"
        );
    }
    assert!(
        fake.writes().iter().all(|c| c[1] != "list-keychains"),
        "a second run rewrote the search list"
    );

    // A key codesign cannot use there, its partition list not settable now, is provision's
    // to fix, with no login password.
    {
        let mut world = fake.0.borrow_mut();
        world.keychains.get_mut(&release).unwrap().partition = false;
        world.fail = Some("set-key-partition-list");
    }
    let Outcome::Blocked { fix, .. } = verdict(&kc, "m9", vec![sha1.clone()], Leave::Write) else {
        panic!("a key codesign cannot use is blocked");
    };
    assert!(fix.contains("provision --id m9"), "{fix}");
    assert!(!fix.contains("asks for your login"), "{fix}");

    // A release keychain that cannot be listed is re-run, never sent to the login keychain.
    fake.0.borrow_mut().fail = Some("find-identity");
    let Outcome::Blocked { fix, .. } = verdict(&kc, "m9", vec![sha1], Leave::Write) else {
        panic!("an unreadable keychain is blocked");
    };
    assert!(fix.contains("could not be read"), "{fix}");
    assert!(!fix.contains("login"), "{fix}");
}

/// A run without leave to change follows `leave` on a release keychain that holds the
/// identity, its key's partition list gone: `ship provision --check` and a dry release
/// only open it (no write but the unlock, no codesign) and say so; a real release's audit
/// re-asserts the list and test-signs before the publish.
#[test]
fn a_check_only_looks_and_a_real_release_audit_test_signs() {
    let sha1 = "C".repeat(40);
    let scratch = aterm_tempfile::tempdir().expect("scratch HOME");
    let home = std::fs::canonicalize(scratch.path()).expect("canonical scratch");
    let mut world = World::new(&sha1);
    world.search_list = vec![crate::release_keychain::login_keychain_path(&home)];
    let fake = Fake::new(world);
    let kc = Keychains::new(&home, &fake);
    kc.install_pair(&home.join("c.cer"), &home.join("k.key"))
        .expect("install");
    let release = kc.release();
    let stop = || {
        let mut world = fake.0.borrow_mut();
        let kept = world.keychains.get_mut(&release).unwrap();
        kept.partition = false;
        kept.locked = true;
        world.log.clear();
    };

    stop();
    match acquire_in(&kc, "m9", false, Leave::Look, None) {
        Outcome::Ready { ids, note } => {
            assert_eq!(ids, std::slice::from_ref(&sha1));
            assert!(note.is_some_and(|n| n.contains(OPENED_NOTE)));
        }
        Outcome::Blocked { what, fix } => panic!("a look is ready: {what} / {fix}"),
        _ => panic!("a look at a held identity is ready"),
    }
    let changed: Vec<_> = fake
        .writes()
        .into_iter()
        .filter(|c| c[1] != "unlock-keychain")
        .collect();
    assert!(changed.is_empty(), "a look only unlocks: {changed:?}");
    assert!(fake.calls("codesign", "--force").is_empty());

    stop();
    match acquire_in(&kc, "m9", false, Leave::Write, None) {
        Outcome::Ready { note, .. } => {
            assert!(!note.is_some_and(|n| n.contains(OPENED_NOTE)));
        }
        Outcome::Blocked { what, fix } => panic!("the audit signs: {what} / {fix}"),
        _ => panic!("a real release's audit test-signs"),
    }
    assert!(fake.0.borrow().keychains[&release].partition);
    assert!(!fake.calls("codesign", "--force").is_empty());
    let added: Vec<_> = fake
        .writes()
        .into_iter()
        .filter(|c| !matches!(c[1].as_str(), "unlock-keychain" | "set-key-partition-list"))
        .collect();
    assert!(added.is_empty(), "nothing is imported or listed: {added:?}");
}

/// A Mac provisioned before the release keychain keeps signing from its login keychain:
/// provision and its check accept the identity there, write nothing, and only the fix for
/// a key codesign cannot use there names the login password.
#[test]
fn a_login_keychain_provisioned_mac_is_accepted_with_nothing_written() {
    let sha1 = "B".repeat(40);
    let scratch = aterm_tempfile::tempdir().expect("scratch HOME");
    let home = std::fs::canonicalize(scratch.path()).expect("canonical scratch");
    let login = crate::release_keychain::login_keychain_path(&home);
    let mut world = World::new(&sha1);
    world.keychains.insert(login.clone(), Kc::holding(&sha1));
    world.search_list = vec![login.clone()];
    let fake = Fake::new(world);
    let kc = Keychains::new(&home, &fake);
    for may_change in [true, false] {
        match acquire_in(&kc, "m9", may_change, Leave::Look, None) {
            Outcome::Ready { ids, .. } => assert_eq!(ids, std::slice::from_ref(&sha1)),
            Outcome::Blocked { what, fix } => panic!("blocked: {what} / {fix}"),
            _ => panic!("the login keychain's identity is accepted"),
        }
    }
    assert!(fake.writes().is_empty(), "{:?}", fake.writes());
    let probes = fake.calls("codesign", "--force");
    assert!(
        !probes.is_empty()
            && probes
                .iter()
                .all(|p| !p.contains(&"--keychain".to_string())),
        "codesign finds it on the search list as it always has: {probes:?}"
    );
    for leave in [Leave::Write, Leave::Look] {
        assert_eq!(pinned_identity_proves(&kc, &sha1, leave), Ok(Proof::Signed));
    }

    fake.0
        .borrow_mut()
        .keychains
        .get_mut(&login)
        .unwrap()
        .partition = false;
    let Outcome::Blocked { fix, .. } = verdict(&kc, "m9", vec![sha1.clone()], Leave::Look) else {
        panic!("a key codesign cannot use is blocked");
    };
    assert!(
        fix.contains("asks for your login keychain password"),
        "{fix}"
    );
    assert!(fake.writes().is_empty(), "{:?}", fake.writes());
}

#[test]
fn nothing_outside_aterms_own_folder_is_read_unless_the_operator_named_it() {
    let home = Path::new("/fixture/home");
    let own = Watch::new(Some(home), None);
    let dirs = own.dirs();
    assert_eq!(dirs.len(), 1, "{dirs:?}");
    assert!(dirs[0].ends_with(".aterm/apple"), "{dirs:?}");
    assert!(
        !dirs
            .iter()
            .any(|d| d.to_string_lossy().contains("Downloads")),
        "{dirs:?}"
    );
    let named = Watch::new(Some(home), Some(Path::new("/fixture/named/Downloads")));
    let dirs = named.dirs();
    assert_eq!(dirs.len(), 2, "{dirs:?}");
    assert_eq!(
        dirs[0],
        Path::new("/fixture/named/Downloads"),
        "the named folder is looked in first"
    );
    assert!(dirs[1].ends_with(".aterm/apple"), "{dirs:?}");
}

#[test]
fn the_consent_note_names_only_a_guarded_folder_the_operator_named() {
    let home = Path::new("/fixture/home");
    let note = |named: Option<&Path>| {
        Watch::new(Some(home), named).consent_note_for(home, Path::to_path_buf)
    };
    assert!(note(None).is_none());
    assert!(note(Some(&home.join("Downloads"))).is_some());
    assert!(note(Some(&home.join("Library/Containers/example"))).is_some());
    assert!(note(Some(&home.join(".aterm/apple-scratch"))).is_none());
    assert!(note(Some(&home.join("Downloadsx"))).is_none());
    assert!(note(Some(Path::new("/fixture/other/Downloads"))).is_none());
}

/// NEGATIVE CONTROL: canonicalizing only complete existing paths leaves the missing
/// child under the alias while resolving its home, so the guarded root is missed.
#[cfg(unix)]
#[test]
fn a_missing_guarded_child_under_a_home_alias_uses_the_same_resolved_prefix() {
    const PROBE: &str = "ATERM_APPLE_ALIAS_PROBE";
    if std::env::var_os(PROBE).is_some() {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        let named = home.join("Downloads");
        assert!(
            Watch::new(Some(&home), Some(&named))
                .consent_note()
                .is_some()
        );
        return;
    }
    // Keep the unresolved spelling outside macOS's absolute /Volumes roots, even
    // when the caller's TMPDIR is on a protected volume.
    let base = Path::new("/tmp/.ubd");
    std::fs::create_dir_all(base).unwrap();
    let dir = aterm_tempfile::Builder::new().tempdir_in(base).unwrap();
    let resolved_base = std::fs::canonicalize(base).unwrap();
    let resolved_dir = std::fs::canonicalize(dir.path()).unwrap();
    let here = base.join(resolved_dir.strip_prefix(&resolved_base).unwrap());
    let home = here.join("home");
    let alias = here.join("alias");
    std::fs::create_dir(&home).unwrap();
    std::os::unix::fs::symlink(&home, &alias).unwrap();
    let missing = alias.join("Downloads");
    let watch = Watch::new(Some(&alias), Some(&missing));
    let old = |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    assert!(watch.consent_note_for(&alias, old).is_none());
    let mut child = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
    child.args(["--exact", "apple::tests::a_missing_guarded_child_under_a_home_alias_uses_the_same_resolved_prefix"])
        .env(PROBE, "1").env("HOME", &alias);
    let output =
        aterm_uds::deadline::output_within(&mut child, aterm_spec::wait::MUST_HAPPEN).unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("1 passed"), "{stdout}");
    assert!(!missing.exists());
}

#[test]
fn the_certificate_is_selected_by_digest_not_by_position() {
    let stream = "SHA-256 hash: AAAA\nSHA-1 hash: 1111111111111111111111111111111111abcdef\n\
                  -----BEGIN CERTIFICATE-----\nolder\n-----END CERTIFICATE-----\n\
                  SHA-256 hash: BBBB\nSHA-1 hash: 2222222222222222222222222222222222FEDCBA\n\
                  -----BEGIN CERTIFICATE-----\nnewer\n-----END CERTIFICATE-----\n";
    let second = leaf_pem_by_sha1(stream, "2222222222222222222222222222222222fedcba")
        .expect("the second certificate");
    assert!(second.contains("newer"), "{second}");
    assert!(!second.contains("older"), "{second}");
    assert!(
        leaf_pem_by_sha1(stream, "1111111111111111111111111111111111ABCDEF")
            .is_some_and(|p| p.contains("older"))
    );
    assert!(leaf_pem_by_sha1(stream, &"3".repeat(40)).is_none());
}

#[test]
fn the_issuing_key_is_read_out_of_both_identifier_forms() {
    let leaf = "        X509v3 extensions:\n            \
                X509v3 Authority Key Identifier: \n                \
                keyid:F8:3A:0C:69:11:76:E0:ED:AC:D1:EB:A6:59:FA:37:D5:C4:55:B0:1E\n\n            \
                X509v3 Subject Key Identifier: \n                \
                7A:7C:DC:1E:A9:B4:E0:95:1A:80:1B:6C:69:46:8D:94:C2:6D:61:57\n            \
                X509v3 Key Usage: critical\n";
    assert_eq!(
        key_id_from_text(leaf, AUTHORITY_KEY_ID).as_deref(),
        Some("F83A0C691176E0EDACD1EBA659FA37D5C455B01E")
    );
    assert_eq!(
        key_id_from_text(leaf, SUBJECT_KEY_ID).as_deref(),
        Some("7A7CDC1EA9B4E0951A801B6C69468D94C26D6157")
    );
    assert!(key_id_from_text("X509v3 Basic Constraints: critical\n", AUTHORITY_KEY_ID).is_none());
}

#[test]
fn the_expiry_date_reads_as_iso() {
    assert_eq!(
        enddate_iso("notAfter=Aug 18 16:36:18 2031 GMT\n").as_deref(),
        Some("2031-08-18")
    );
    assert_eq!(
        enddate_iso("notAfter=Sep  1 00:00:00 2027 GMT").as_deref(),
        Some("2027-09-01")
    );
    assert!(enddate_iso("notBefore=Aug 17 16:36:19 2026 GMT").is_none());
}

/// While the errand waits, a wrong `.cer` is named on the look it arrives on and not while it
/// stays; one that left the folder leaves what the wait reports and is named again if it
/// comes back.
#[test]
fn a_wrong_certificate_is_named_once_when_it_arrives() {
    let cer = |name: &str| PathBuf::from(format!("/w/{name}.cer"));
    let mut first = Vec::new();
    assert_eq!(take_arrivals(vec![cer("a")], &mut first), [cer("a")]);
    let mut rejected = vec![cer("a")];
    assert_eq!(
        take_arrivals(vec![cer("a"), cer("b")], &mut rejected),
        [cer("b")]
    );
    assert_eq!(rejected, [cer("a"), cer("b")]);
    assert!(take_arrivals(vec![cer("a"), cer("b")], &mut rejected).is_empty());
    assert_eq!(rejected, [cer("a"), cer("b")]);
    assert!(take_arrivals(vec![cer("b")], &mut rejected).is_empty());
    assert_eq!(rejected, [cer("b")], "a .cer that left is not reported");
    assert_eq!(
        take_arrivals(vec![cer("a"), cer("b")], &mut rejected),
        [cer("a")],
        "a .cer that came back is named again"
    );
}

#[test]
fn write_credentials_profile_in_refuses_symlink_machine_key_and_writes_0600() {
    use std::os::unix::fs::PermissionsExt;

    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-apple-creds-")
        .tempdir()
        .unwrap();
    let ring = dir.path();
    let real_key = ring.join("real.key");
    let link_key = ring.join("machine.key");
    std::fs::write(&real_key, [0x42u8; 32]).unwrap();
    std::os::unix::fs::symlink(&real_key, &link_key).unwrap();
    let roster = ring.join("machines.toml");
    let err = write_credentials_profile_in(ring, "m1", &roster, Some("deadbeef")).unwrap_err();
    assert!(err.contains("cannot read this machine's key"), "{err}");

    std::fs::remove_file(&link_key).unwrap();
    std::fs::rename(&real_key, &link_key).unwrap();
    let written = write_credentials_profile_in(ring, "m1", &roster, Some("deadbeef"))
        .unwrap()
        .expect("created profile");
    let mode = std::fs::symlink_metadata(&written)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
    let content = std::fs::read_to_string(&written).unwrap();
    assert!(content.contains("machine_id = \"m1\"\n"));
    assert!(content.contains("signing_identity_sha1 = \"deadbeef\"\n"));
    // Calling a second time returns Ok(None) without overwriting.
    assert_eq!(
        write_credentials_profile_in(ring, "m1", &roster, None).unwrap(),
        None
    );
    assert_eq!(
        enddate_iso("notAfter=Aug 18 16:36:18 2031 GMT").as_deref(),
        Some("2031-08-18")
    );
    assert_eq!(enddate_iso("notAfter=Aug +18 16:36:18 2031 GMT"), None);
    assert_eq!(enddate_iso("notAfter=Aug 18 16:36:18 +2031 GMT"), None);
}
