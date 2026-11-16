// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::items::defaults::tests::user;
use crate::port::Fake;
use aterm_secrets::need::GhProfile;
use std::path::Path;

/// A Fake logged in as alice, whose home is the scratch `home`: a member of a scratch vault
/// whose store publishes `names` to everyone, so its registry names them (none: no vault, so
/// no registry applied). The registry is the store's own view, never a file under the home;
/// the values the tests read are the Fake's ([`Fake::vault_set`]).
fn lab(home: &Path, names: Option<&[&str]>) -> Fake {
    let mut who = user("alice", 501);
    who.home = home.to_string_lossy().into_owned();
    let mut sys = Fake::root();
    sys.euid = who.uid;
    sys.logged_in(&who);
    if let Some(names) = names {
        let paths = aterm_secrets::client::Paths::locate_for_home(home);
        aterm_secrets::client::create(
            &paths,
            home.join("fixture.vault").to_str().unwrap(),
            None,
            Some("controller"),
        )
        .unwrap();
        let admin = aterm_secrets::client::admin_open(&paths).unwrap();
        let mut doc = admin.doc.clone();
        for name in names {
            doc.set_entry(
                &aterm_secrets::text::Audience::Everyone,
                name,
                b"registered fixture entry",
            )
            .unwrap();
        }
        aterm_secrets::client::admin_publish(&paths, &admin, &doc).unwrap();
    }
    sys
}

/// `bytes` as the store's entry `entry` under `home`, as the port reads it in place
/// ([`Fake::vault_set`]).
fn landed(sys: &Fake, home: &Path, entry: &str, bytes: &[u8]) {
    sys.vault_set(home, entry, bytes);
}

/// The one scratch home the controller's other tests sign `sys` in to: its registry names the
/// `gh:fleet` and `machines-admin` entries. A test that finds no value files its need in this
/// home's ledger, never the person's.
fn vault_home() -> &'static Path {
    static HOME: std::sync::OnceLock<aterm_tempfile::TempDir> = std::sync::OnceLock::new();
    HOME.get_or_init(|| {
        let home = aterm_tempfile::tempdir().unwrap();
        drop(lab(
            home.path(),
            Some(&[
                aterm_secrets::need::catalog::FLEET_GH_TOKEN_ENTRY,
                aterm_secrets::need::catalog::MACHINES_ADMIN_ENTRY,
            ]),
        ));
        home
    })
    .path()
}

/// Sign `sys` in to [`vault_home`], whose registry names `entry`; its store holds `bytes` as
/// that entry, or none with `None`.
pub(crate) fn in_vault(sys: &Fake, entry: &str, bytes: Option<&[u8]>) {
    let home = vault_home();
    let mut who = user("controller", sys.euid);
    who.home = home.to_string_lossy().into_owned();
    sys.logged_in(&who);
    if let Some(bytes) = bytes {
        sys.vault_set(home, entry, bytes);
    }
}

fn ledger(home: &Path) -> Vec<(String, &'static str)> {
    let paths = aterm_secrets::client::Paths::locate_for_home(home);
    need::peek(&paths)
        .unwrap()
        .into_iter()
        .map(|row| (row.want.key(), row.purpose.id()))
        .collect()
}

/// THE ONE PATH: the registry's entry is read in place from the store; nothing is filed.
#[test]
fn a_registered_value_is_read_in_place() {
    let home = aterm_tempfile::tempdir().unwrap();
    let sys = lab(
        home.path(),
        Some(&[
            aterm_secrets::need::catalog::FLEET_GH_TOKEN_ENTRY,
            aterm_secrets::need::catalog::MACHINES_ADMIN_ENTRY,
        ]),
    );
    landed(&sys, home.path(), "gh_access_token_fleet", b" ghp_fleet \n");
    landed(&sys, home.path(), "machines_admin_password", b"two words\n");
    let fleet = Want::Gh(GhProfile::Fleet);
    let token = resolve(&sys, &fleet, Purpose::MachinesKeys, true).unwrap();
    assert_eq!(token.bytes(), b"ghp_fleet");
    let password = resolve(&sys, &Want::MachinesAdmin, Purpose::MachinesEnroll, false).unwrap();
    assert_eq!(password.bytes(), b"two words");
    assert!(ledger(home.path()).is_empty());
}

/// A password entry written with CRLF reads as the password alone; one with a control
/// character inside it is no value.
#[test]
fn a_crlf_line_is_the_password_and_a_control_character_is_none() {
    for (bytes, read) in [
        (&b"two words\r\n"[..], Some(&b"two words"[..])),
        (&b"two\twords\n"[..], None),
    ] {
        let home = aterm_tempfile::tempdir().unwrap();
        let sys = lab(
            home.path(),
            Some(&[aterm_secrets::need::catalog::MACHINES_ADMIN_ENTRY]),
        );
        landed(&sys, home.path(), "machines_admin_password", bytes);
        let got = resolve(&sys, &Want::MachinesAdmin, Purpose::MachinesEnroll, false);
        assert_eq!(got.as_ref().ok().map(Secret::bytes), read, "{bytes:?}");
    }
}

/// NEGATIVE CONTROLS: no registry, a registry that names no entry, a missing entry and a
/// token with whitespace in it each file the want once under its purpose, and the line names
/// the want, the request and why, never a value. Each case names its own why, so a fixture
/// whose registry names nothing cannot pass every case at the registry.
#[test]
fn a_value_this_mac_lacks_is_filed_once_with_the_lab() {
    let fleet = Want::Gh(GhProfile::Fleet);
    type Case<'a> = (Option<&'a [&'a str]>, Option<&'a [u8]>, &'a str);
    let cases: [Case<'_>; 5] = [
        (None, None, aterm_secrets::registry::NO_REGISTRY),
        (
            Some(&["gh_access_token"]),
            None,
            "the vault registry names none",
        ),
        (
            Some(&["gh_access_token"]),
            Some(b"unregistered_token\n"),
            "the vault registry names none",
        ),
        (
            Some(&["gh_access_token_fleet"]),
            None,
            "the vault holds no gh_access_token_fleet here",
        ),
        (
            Some(&["gh_access_token_fleet"]),
            Some(b"ghp_a ghp_b\n"),
            "the vault entry gh_access_token_fleet holds no single value",
        ),
    ];
    for (names, bytes, expected) in cases {
        let home = aterm_tempfile::tempdir().unwrap();
        let sys = lab(home.path(), names);
        if let Some(bytes) = bytes {
            landed(&sys, home.path(), "gh_access_token_fleet", bytes);
        }
        let why = resolve(&sys, &fleet, Purpose::MachinesKeys, true).unwrap_err();
        assert!(
            why.contains("no usable gh:fleet")
                && why.contains(&format!("({expected})"))
                && why.contains("asked the lab's Macs (request "),
            "{why}"
        );
        assert!(!why.contains("ghp_"), "{why}");
        resolve(&sys, &fleet, Purpose::MachinesKeys, true).unwrap_err();
        assert_eq!(
            ledger(home.path()),
            [("gh:fleet".to_owned(), "machines.keys")],
            "{names:?}"
        );
    }
}

/// With no account home there is no registry to read and no ledger to file in: the line says
/// so and nothing is asked of anyone.
#[test]
fn no_home_is_no_lookup() {
    let sys = Fake::root();
    let why = resolve(&sys, &Want::MachinesAdmin, Purpose::MachinesEnroll, false).unwrap_err();
    assert!(why.contains("machines-admin"), "{why}");
    assert!(sys.calls().is_empty(), "{:?}", sys.calls());
}
