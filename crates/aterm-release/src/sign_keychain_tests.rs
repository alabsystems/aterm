// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    AppleTier, DevIdIdentity, ReleaseCredentials, app_sign_args, devid_sign_args, dmg_sign_args,
    locate_identity, resolve_apple_tier_with,
};
use crate::release_keychain::fake::{Fake, Kc, World};
use crate::release_keychain::{Keychains, login_keychain_path};
use aterm_update_core::pins::APPLE_TEAM_ID as TEAM;
use std::path::{Path, PathBuf};

const SHA1: &str = "C0FFEE0000000000000000000000000000C0FFEE";

fn scratch() -> (aterm_tempfile::TempDir, PathBuf) {
    let dir = aterm_tempfile::tempdir().expect("scratch HOME");
    let home = std::fs::canonicalize(dir.path()).expect("canonical scratch");
    std::fs::create_dir_all(home.join("Library/Keychains")).expect("Keychains");
    (dir, home)
}

fn argv(id: &DevIdIdentity) -> Vec<String> {
    devid_sign_args(id)
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect()
}

/// The release audit changes keychain metadata after a successful automatic proof.
/// The next profile load must keep that proven shared identity, even when its old pin
/// is unavailable. Readiness still requires a new proof of the changed keychain.
#[test]
fn profile_reload_keeps_shared_identity_after_partition_upkeep() {
    use aterm_secrets::{client, fsx, materialize::digest_hex, text::Audience};
    use aterm_update_core::roster::{Machine, Roster, SUPPORTED_SCHEMA};
    use ring::signature::{Ed25519KeyPair, KeyPair as _};
    use std::os::unix::fs::MetadataExt as _;

    let (_dir, home) = scratch();
    let paths = client::Paths::for_home(&home);
    client::test_create_unbound(
        &paths,
        home.join("fixture.vault").to_str().unwrap(),
        None,
        Some("m1"),
    )
    .unwrap();
    let certificate = b"shared signing certificate fixture";
    let private_key = b"shared signing private key fixture";
    let shared = aterm_codec::hex::encode_lower(
        ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, certificate).as_ref(),
    )
    .to_ascii_uppercase();
    let admin = client::admin_open(&paths).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(
        &Audience::Everyone,
        "~/.aterm/apple/devid-shared.cer",
        certificate,
    )
    .unwrap();
    doc.set_entry(
        &Audience::Everyone,
        "~/.aterm/apple/devid-shared.key",
        private_key,
    )
    .unwrap();
    client::admin_publish(&paths, &admin, &doc).unwrap();

    let rng = ring::rand::SystemRandom::new();
    let key = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
    let signer = Ed25519KeyPair::from_pkcs8(key.as_ref()).unwrap();
    let master_key = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
    let master = Ed25519KeyPair::from_pkcs8(master_key.as_ref()).unwrap();
    let anchor = aterm_secrets::b64::encode_padded(master.public_key().as_ref());
    let ring = home.join(".aterm/release-ring");
    fsx::ensure_private_dir(&ring.join("roster")).unwrap();
    let profile = ring.join("release-credentials.toml");
    fsx::write_atomic(&profile, format!(
        "signing_key = \"{}\"\nmachine_id = \"m1\"\nmachine_roster = \"~/.aterm/release-ring/roster/aterm-machines.toml\"\nnotary_profile = \"fixture\"\nsigning_identity_sha1 = \"{SHA1}\"\n",
        aterm_secrets::b64::encode_padded(key.as_ref()),
    ).as_bytes(), 0o600).unwrap();
    fsx::write_atomic(&ring.join("machine.key"), key.as_ref(), 0o600).unwrap();
    let mut roster = Roster {
        schema: SUPPORTED_SCHEMA,
        roster_seq: 1,
        valid_until: "9999-12-31T00:00:00Z".into(),
        revoked: vec![],
        machines: vec![Machine {
            id: "m1".into(),
            pubkey: aterm_secrets::b64::encode_padded(signer.public_key().as_ref()),
            added_at: "1970-01-01T00:00:00Z".into(),
            not_after: None,
        }],
    };
    let roster_text = roster.to_toml().unwrap();
    fsx::write_atomic(
        &ring.join("roster/aterm-machines.toml"),
        roster_text.as_bytes(),
        0o644,
    )
    .unwrap();
    fsx::write_atomic(
        &ring.join("roster/aterm-machines.toml.sig"),
        master.sign(roster_text.as_bytes()).as_ref(),
        0o644,
    )
    .unwrap();

    let mut world = World::new(&shared);
    let login = login_keychain_path(&home);
    world.keychains.insert(login.clone(), Kc::default());
    world.search_list.push(login);
    let fake = Fake::new(world);
    let kc = Keychains::new(&home, &fake);
    kc.install_pair(Path::new("/fixture/certificate"), Path::new("/fixture/key"))
        .unwrap();
    assert_eq!(
        crate::release_keychain::prove_can_sign(
            &kc,
            &shared,
            crate::release_keychain::Leave::Write
        )
        .unwrap(),
        crate::release_keychain::Proof::Signed
    );
    let meta = std::fs::symlink_metadata(kc.release()).unwrap();
    let password = fsx::read_private(&crate::release_keychain::password_path(&home), 1024)
        .unwrap()
        .unwrap();
    let local = format!(
        "{}:{}:{}:{}:{}:{}",
        meta.dev(),
        meta.ino(),
        meta.len(),
        meta.mtime(),
        meta.mtime_nsec(),
        digest_hex(password.as_slice())
    );
    let proved = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    fsx::write_atomic(
        &paths.dir.join("release-signing-proof.toml"),
        format!(
            "material = \"{}:{}\"\nlocal = {local:?}\nidentity = {shared:?}\nproved = {proved}\n",
            digest_hex(certificate),
            digest_hex(private_key),
        )
        .as_bytes(),
        0o600,
    )
    .unwrap();
    let selection = |profile: &Path| {
        client::release::test_signing_observation(&paths, profile, &[&anchor], proved as i64)
            .unwrap()
    };
    let observation = || selection(&profile);
    let load = || {
        ReleaseCredentials::load_with_identity(&profile, Some(&home), |_, profile| {
            selection(profile).0
        })
        .unwrap()
    };
    assert_eq!(load().signing_identity_sha1(), Some(shared.as_str()));
    assert!(matches!(
        observation().1,
        client::release::SigningStatus::Ready(_)
    ));

    struct PartitionWrite<'a> {
        fake: &'a Fake,
        path: PathBuf,
    }
    impl crate::dev_identity::Run for PartitionWrite<'_> {
        fn run(
            &self,
            cmd: &mut aterm_uds::spawnfd::Command,
            wait: std::time::Duration,
        ) -> Result<std::process::Output, aterm_uds::deadline::RunError> {
            let partition =
                cmd.get_args().next() == Some(std::ffi::OsStr::new("set-key-partition-list"));
            let output = crate::dev_identity::Run::run(self.fake, cmd, wait)?;
            if partition && output.status.success() {
                std::fs::write(&self.path, b"partition list reasserted").unwrap();
            }
            Ok(output)
        }
    }
    let upkeep = PartitionWrite {
        fake: &fake,
        path: kc.release(),
    };
    let before = load();
    let AppleTier::Active { identity, .. } =
        resolve_apple_tier_with(TEAM, Some(&before), Some(&home), &upkeep).unwrap()
    else {
        panic!("active Apple tier")
    };
    assert_eq!(identity.sha1, shared);
    assert_eq!(identity.keychain.as_deref(), Some(kc.release().as_path()));
    assert_ne!(std::fs::metadata(kc.release()).unwrap().len(), meta.len());
    assert!(matches!(
        observation().1,
        client::release::SigningStatus::Pending(_)
    ));
    let after = load();
    assert_eq!(after.signing_identity_sha1(), Some(shared.as_str()));
    let AppleTier::Active { identity, .. } =
        resolve_apple_tier_with(TEAM, Some(&after), Some(&home), &fake).unwrap()
    else {
        panic!("active Apple tier after reload")
    };
    assert_eq!(identity.sha1, shared);
    assert!(
        !fake
            .0
            .borrow()
            .keychains
            .values()
            .any(|kc| kc.certs.iter().any(|id| id == SHA1)),
        "the profile's old identity is unavailable"
    );

    let unrelated = home.join("unrelated-release.toml");
    fsx::write_atomic(&unrelated, &std::fs::read(&profile).unwrap(), 0o600).unwrap();
    let unrelated =
        ReleaseCredentials::load_with_identity(&unrelated, Some(&home), |_, profile| {
            selection(profile).0
        })
        .unwrap();
    assert_eq!(unrelated.signing_identity_sha1(), Some(SHA1));

    let admin = client::admin_open(&paths).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(
        &Audience::Everyone,
        "~/.aterm/apple/devid-shared.key",
        b"changed signing material",
    )
    .unwrap();
    client::admin_publish(&paths, &admin, &doc).unwrap();
    assert_eq!(load().signing_identity_sha1(), Some(SHA1));
    let admin = client::admin_open(&paths).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(
        &Audience::Everyone,
        "~/.aterm/apple/devid-shared.key",
        private_key,
    )
    .unwrap();
    client::admin_publish(&paths, &admin, &doc).unwrap();
    assert_eq!(load().signing_identity_sha1(), Some(shared.as_str()));

    roster.revoked.push("m1".into());
    let revoked = roster.to_toml().unwrap();
    fsx::write_atomic(
        &ring.join("roster/aterm-machines.toml"),
        revoked.as_bytes(),
        0o644,
    )
    .unwrap();
    fsx::write_atomic(
        &ring.join("roster/aterm-machines.toml.sig"),
        master.sign(revoked.as_bytes()).as_ref(),
        0o644,
    )
    .unwrap();
    assert_eq!(load().signing_identity_sha1(), Some(SHA1));
}

/// A Mac `ship provision` filled signs from the release keychain by path: the cut opens it
/// with its stored password, re-asserts the key's partition list (lost here), names it, and
/// codesign gets `--keychain` before `--sign`.
#[test]
fn signing_names_the_release_keychain() {
    let (_dir, home) = scratch();
    let login = login_keychain_path(&home);
    let mut world = World::new(SHA1);
    world.keychains.insert(login.clone(), Kc::default());
    world.search_list = vec![login];
    let fake = Fake::new(world);
    let kc = Keychains::new(&home, &fake);
    kc.install_pair(Path::new("/vault/devid.cer"), Path::new("/vault/devid.key"))
        .expect("provision");
    let release = kc.release();
    {
        let mut world = fake.0.borrow_mut();
        let kept = world.keychains.get_mut(&release).unwrap();
        kept.locked = true;
        kept.partition = false;
    }
    fake.0.borrow_mut().log.clear();

    let id = locate_identity(TEAM, None, Some(&home), &fake).expect("the identity");
    assert_eq!(id.sha1, SHA1);
    assert_eq!(id.keychain.as_deref(), Some(release.as_path()));
    let kept = fake.0.borrow().keychains[&release].clone();
    assert!(!kept.locked, "opened for codesign");
    assert!(
        kept.partition,
        "codesign meets a key it may use with no dialog"
    );
    let shown = release.display().to_string();
    assert_eq!(argv(&id), ["--keychain", shown.as_str(), "--sign", SHA1]);
    assert!(
        fake.0
            .borrow()
            .log
            .iter()
            .all(|c| !c.iter().any(|a| a.contains("login.keychain"))),
        "{:?}",
        fake.0.borrow().log
    );
    assert_eq!(
        locate_identity(TEAM, Some(&SHA1.to_lowercase()), Some(&home), &fake)
            .expect("pinned")
            .keychain,
        Some(release.clone())
    );

    // One that will not open is refused before codesign could ask for its password.
    std::fs::remove_file(crate::release_keychain::password_path(&home)).unwrap();
    fake.0
        .borrow_mut()
        .keychains
        .get_mut(&release)
        .unwrap()
        .locked = true;
    let refused = locate_identity(TEAM, None, Some(&home), &fake).expect_err("locked");
    assert!(refused.contains("move it aside"), "{refused}");
}

/// A release keychain that will not open and holds no identity (one provision made only
/// for an issuing intermediate, its password file since lost) does not stop a Mac whose
/// identity is in its login keychain: the cut signs from there, with no `--keychain`.
#[test]
fn an_unopenable_release_keychain_holding_nothing_leaves_the_login_identity_usable() {
    let (_dir, home) = scratch();
    let login = login_keychain_path(&home);
    let mut world = World::new(SHA1);
    world.keychains.insert(login.clone(), Kc::holding(SHA1));
    world.search_list = vec![login];
    let fake = Fake::new(world);
    let kc = Keychains::new(&home, &fake);
    let issuer = home.join("issuer.cer");
    kc.install_certificate(&issuer).expect("the intermediate");
    let release = kc.release();
    {
        let mut world = fake.0.borrow_mut();
        let made = world.keychains.get_mut(&release).expect("made");
        made.certs.clear();
        made.locked = true;
    }
    std::fs::remove_file(crate::release_keychain::password_path(&home)).unwrap();
    fake.0.borrow_mut().log.clear();

    let id = locate_identity(TEAM, None, Some(&home), &fake).expect("the login identity");
    assert_eq!((id.sha1.as_str(), id.keychain.as_deref()), (SHA1, None));
    assert!(fake.writes().iter().all(|c| c[1] == "unlock-keychain"));
    assert!(fake.0.borrow().keychains[&release].locked, "never opened");
}

/// The argv a cut hands codesign. With the identity only on the search list (m3, provisioned
/// in its login keychain) it is the argv every cut ran before the release keychain existed,
/// word for word; with the identity in the release keychain, `--keychain <it>` comes just
/// before `--sign` and nothing else changes.
#[test]
fn the_codesign_argv_is_unchanged_for_a_login_keychain_identity() {
    let words = |args: Vec<std::ffi::OsString>| -> Vec<String> {
        args.iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    };
    let (app, ent, dmg) = (
        Path::new("/d/aterm.app"),
        Path::new("/d/ent.plist"),
        Path::new("/d/aterm.dmg"),
    );
    let mut id = DevIdIdentity {
        sha1: SHA1.into(),
        common_name: format!("Developer ID Application: Placeholder Org ({TEAM})"),
        keychain: None,
    };
    let before_app = [
        "--force",
        "--options",
        "runtime",
        "--timestamp",
        "--entitlements",
        "/d/ent.plist",
        "--sign",
        SHA1,
        "/d/aterm.app",
    ];
    let before_dmg = ["--force", "--timestamp", "--sign", SHA1, "/d/aterm.dmg"];
    assert_eq!(words(app_sign_args(app, ent, &id)), before_app);
    assert_eq!(words(dmg_sign_args(dmg, &id)), before_dmg);

    let release = "/h/Library/Keychains/aterm-release.keychain-db";
    id.keychain = Some(PathBuf::from(release));
    let named = |before: &[&str]| -> Vec<String> {
        let at = before.iter().position(|w| *w == "--sign").expect("--sign");
        let mut want: Vec<String> = before.iter().map(|w| (*w).to_string()).collect();
        want.splice(at..at, ["--keychain".to_string(), release.to_string()]);
        want
    };
    assert_eq!(words(app_sign_args(app, ent, &id)), named(&before_app));
    assert_eq!(words(dmg_sign_args(dmg, &id)), named(&before_dmg));
}

/// A Mac provisioned before the release keychain signs from its login keychain, found on
/// the search list as before: no `--keychain`, nothing opened or written.
#[test]
fn a_login_keychain_identity_signs_as_it_always_has() {
    let (_dir, home) = scratch();
    let login = login_keychain_path(&home);
    let mut world = World::new(SHA1);
    world.keychains.insert(login.clone(), Kc::holding(SHA1));
    world.search_list = vec![login.clone()];
    let fake = Fake::new(world);
    let id = locate_identity(TEAM, None, Some(&home), &fake).expect("the identity");
    assert_eq!((id.sha1.as_str(), id.keychain.as_deref()), (SHA1, None));
    assert_eq!(argv(&id), ["--sign", SHA1]);
    assert!(fake.writes().is_empty(), "{:?}", fake.writes());
    assert_eq!(fake.0.borrow().keychains[&login], Kc::holding(SHA1));
    assert!(
        locate_identity(TEAM, Some(&"D".repeat(40)), Some(&home), &fake).is_err(),
        "a pin the keychains do not hold is refused"
    );
}
