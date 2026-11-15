// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use std::path::Path;

use super::{Adoption, Found, Learned, Record, Slot, Vaulted};
use crate::identity::{LoadError, Source, VaultEntry};

fn adopted(slot: Slot, login: &str, fingerprint: &str) -> Vaulted {
    Vaulted {
        slot,
        found: Found::Adopted {
            login: login.to_string(),
            fingerprint: fingerprint.to_string(),
        },
    }
}

/// The vault names the release token by the public owner it writes for.
#[test]
fn the_release_file_is_named_for_the_public_owner() {
    assert!(
        Slot::Release
            .entry()
            .ends_with(&format!("_{}", aterm_types::channel::PUBLIC_OWNER))
    );
    assert_eq!(Slot::named("dev"), Some(Slot::Dev));
    assert_eq!(Slot::named("release"), Some(Slot::Release));
    assert_eq!(Slot::named("ext"), None);
}

/// A record agh writes reads back whole; `checked` drops a row that is not a fingerprint
/// and an owner name, and `read_record` judges a file that loses a row so, or that does not
/// parse, damaged, never an empty record.
#[test]
fn the_record_reads_back_and_a_row_it_cannot_trust_damages_it() {
    let record = Record {
        dev: Some(Learned {
            fingerprint: "0123456789ab".to_string(),
            login: "fixture-dev".to_string(),
        }),
        release: Some(Learned {
            fingerprint: "ba9876543210".to_string(),
            login: "fixture-release-bot".to_string(),
        }),
    };
    let read: Record = aterm_toml::from_str(&record.text()).expect("the record parses");
    assert_eq!(read.checked(), record);
    let forged = "[dev]\nfingerprint = \"0123456789ab\"\nlogin = \"a/b\"\n\
                  [release]\nfingerprint = \"XYZ\"\nlogin = \"fixture-release-bot\"\n";
    let read: Record = aterm_toml::from_str(forged).expect("the rows parse");
    assert_eq!(read.checked(), Record::default());

    let root = crate::test_fixtures::scratch_dir("vault-damaged");
    let dirs = crate::config::Dirs::under(root.path());
    super::record_for_tests(&dirs, &[]).expect("the agh directory");
    for text in [forged, "[dev\n", "[later]\nfield = 1\n"] {
        aterm_uds::private_file::write_private(&dirs.config, super::RECORD_FILE, text.as_bytes())
            .expect("the record");
        let fault = super::read_record(&dirs).expect_err("damaged");
        assert!(
            fault.damaged && fault.fault.rule == "file",
            "{text}: {fault:?}"
        );
    }
}

/// The adopted rows route as the canonical agh.toml does (design §6.2): the dev token
/// first for every owner's plain reads, the public owner's included, each token the only
/// writer of its own owner.
#[test]
fn the_adopted_rows_route_as_the_canonical_config() {
    let home = Path::new("/nonexistent-home");
    let adoption = Adoption::judge(
        vec![
            adopted(Slot::Dev, "fixture-dev", "0123456789ab"),
            adopted(Slot::Release, "fixture-release-bot", "ba9876543210"),
        ],
        home,
        None,
    );
    let router = adoption.router().expect("the adopted rows validate");
    use crate::policy::ChainStep;
    for owner in ["fixture-ext", "alabsystems"] {
        assert_eq!(
            router.read_chain(Some(owner)),
            [ChainStep::Profile("dev".to_string()), ChainStep::Anonymous],
            "{owner}"
        );
    }
    assert_eq!(
        router.read_chain(Some("fixture-dev")),
        [ChainStep::Profile("dev".to_string())]
    );
    assert_eq!(router.writer_of("fixture-dev"), Some("dev"));
    assert_eq!(router.writer_of("alabsystems"), Some("release"));
    assert_eq!(router.writer_of("fixture-ext"), None);
}

/// The seed is `vault = true` alone, which parses and validates; it is `None` while the store
/// holds neither vault entry (whatever `~/.secrets` holds), for a profile named `dev` or
/// `release`, and for a vault entry of the two as `--file`.
#[test]
fn the_seed_joins_the_vault_and_names_no_row() {
    let root = crate::test_fixtures::scratch_dir("vault-seed");
    let dirs = crate::config::Dirs::under(root.path());
    let ext = Source::Entry(VaultEntry::new("ext", &dirs.home));
    let dev = Slot::Dev.source(&dirs.home);
    let file = Slot::Dev.path(&dirs.home);
    std::fs::create_dir_all(file.parent().expect("the vault directory")).expect("the home");
    std::fs::write(&file, "x").expect("an old build's file");
    assert_eq!(
        super::seed(&dirs, "ext", &ext),
        None,
        "a file under ~/.secrets is no vault entry"
    );
    super::put_entry_for_tests(&dirs.home, Slot::Dev.entry(), Some(b"x"));
    for name in ["dev", "release"] {
        assert_eq!(super::seed(&dirs, name, &ext), None, "{name} routes alone");
    }
    assert_eq!(
        super::seed(&dirs, "ext", &dev),
        None,
        "a vault entry named as another profile's routes alone"
    );
    let seed = super::seed(&dirs, "ext", &ext).expect("a seed");
    let parsed = crate::config::parse(&seed).expect("the seed parses");
    assert!(parsed.vault);
    assert!(parsed.profiles.is_empty() && parsed.owners.is_empty());
    let validated =
        crate::config::validate(&parsed, &dirs.home, &|_| None).expect("the seed validates");
    assert!(validated.joins_vault());
}

/// An agh.toml that says `vault = true` leaves profiles `dev` and `release` to the vault:
/// L10 refuses one it names, and the same rows without the key pass.
#[test]
fn a_vault_joining_agh_toml_names_no_vault_profile() {
    let home = Path::new("/nonexistent-home");
    let rows = "[profiles.dev]\ntoken_file = \"~/dev.token\"\ntier = \"dev\"\n";
    let joined = crate::config::parse(&format!("vault = true\n{rows}")).expect("parses");
    let faults = crate::config::validate(&joined, home, &|_| None).expect_err("L10");
    assert!(faults.iter().any(|f| f.rule == "L10"), "{faults:?}");
    let alone = crate::config::parse(rows).expect("parses");
    assert!(crate::config::validate(&alone, home, &|_| None).is_ok());
}

/// A dev token of the public owner breaks the floor: it is left out, reported, and the
/// release token still routes.
#[test]
fn a_dev_token_of_the_public_owner_is_left_out() {
    let home = Path::new("/nonexistent-home");
    let adoption = Adoption::judge(
        vec![
            adopted(
                Slot::Dev,
                aterm_types::channel::PUBLIC_OWNER,
                "0123456789ab",
            ),
            adopted(Slot::Release, "fixture-release-bot", "ba9876543210"),
        ],
        home,
        None,
    );
    let router = adoption.router().expect("the release rows still route");
    assert!(adoption.faults().iter().any(|f| f.rule == "L2"));
    assert!(matches!(adoption.vaulted()[0].found, Found::Faulted { .. }));
    assert!(adoption.adopted("release") && !adoption.adopted("dev"));
    assert_eq!(router.writer_of("alabsystems"), Some("release"));
    assert!(router.profile("dev").is_none());
    let lines = adoption.note();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].starts_with("agh: note vault=dev why="),
        "{lines:?}"
    );
}

/// Two vault tokens GitHub answers one login for: neither's tier is known, so neither
/// routes, each is left out with the vault's step, and the L7 fault is reported once.
#[test]
fn two_tokens_of_one_login_route_neither() {
    let home = Path::new("/nonexistent-home");
    let adoption = Adoption::judge(
        vec![
            adopted(Slot::Dev, "fixture-dev", "0123456789ab"),
            adopted(Slot::Release, "Fixture-Dev", "ba9876543210"),
        ],
        home,
        None,
    );
    let router = adoption.router().expect("no refusal");
    assert!(!adoption.adopted_any());
    assert!(router.profile("dev").is_none() && router.profile("release").is_none());
    let rules: Vec<&str> = adoption.faults().iter().map(|f| f.rule).collect();
    assert_eq!(rules, ["L7"]);
    for vaulted in adoption.vaulted() {
        assert!(
            matches!(&vaulted.found, Found::Faulted { next, .. } if *next == super::pair_next()),
            "{vaulted:?}"
        );
    }
    let lines = adoption.note();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].starts_with("agh: note vault=dev,release why="),
        "{lines:?}"
    );
    let mut quoted = String::new();
    crate::cli::out::receipt_value(&mut quoted, &super::pair_next());
    assert!(lines[0].ends_with(&format!(" next={quoted}")), "{lines:?}");
}

/// A login learned for a slot whose record another call filled meanwhile with another login
/// is not written: the slot is reported moved, and the call refuses it. The same login (in
/// any case) is written.
#[test]
fn a_learned_login_never_overwrites_another_recorded_meanwhile() {
    let root = crate::test_fixtures::scratch_dir("vault-race");
    let dirs = crate::config::Dirs::under(root.path());
    super::record_for_tests(&dirs, &[(Slot::Dev, "0123456789ab", "fixture-x")])
        .expect("the record");
    let learn = |login: &str| super::Learn {
        slot: Slot::Dev,
        kind: crate::identity::TokenKind::Classic,
        recorded: None,
        row: Learned {
            fingerprint: "ba9876543210".to_string(),
            login: login.to_string(),
        },
    };
    let (moved, refused) = super::record_learned(&dirs, &[learn("fixture-y")]);
    assert_eq!(moved, vec![(Slot::Dev, "fixture-x".to_string())]);
    assert!(refused.is_none(), "{refused:?}");
    let record = super::read_record(&dirs).expect("the record reads");
    assert_eq!(
        record.dev.as_ref().map(|r| r.fingerprint.as_str()),
        Some("0123456789ab")
    );
    let (moved, refused) = super::record_learned(&dirs, &[learn("Fixture-X")]);
    assert!(moved.is_empty() && refused.is_none());
    let record = super::read_record(&dirs).expect("the record reads");
    assert_eq!(
        record.dev.as_ref().map(|r| r.fingerprint.as_str()),
        Some("ba9876543210")
    );
}

/// The subsets tried, largest first, dev before release.
#[test]
fn the_subsets_keep_the_most_tokens_dev_first() {
    assert_eq!(
        super::subsets(&[Slot::Dev, Slot::Release]),
        vec![
            vec![Slot::Dev, Slot::Release],
            vec![Slot::Dev],
            vec![Slot::Release],
            vec![],
        ]
    );
    assert_eq!(super::subsets(&[]), vec![Vec::<Slot>::new()]);
}

/// An absent token's step is the vault's: a slot's sync step, another vault entry's need, and
/// none for a token file of the person's own, which the vault does not hold.
#[test]
fn an_absent_token_is_asked_of_the_vault_by_its_entry() {
    use crate::identity::Profile;
    use crate::policy::Tier;
    let home = Path::new("/h");
    let named = |text: &str| {
        let source = crate::config::token_source(text, home).expect("a token_file");
        Profile::new("w", source, Tier::Dev)
    };
    assert_eq!(
        super::token_step(&named("~/.secrets/gh_access_token")),
        Some(Slot::Dev.sync_step())
    );
    let other = super::token_step(&named("/h/.secrets/gh_access_token_org")).expect("a step");
    assert!(
        other.starts_with("aterm secrets sync applies the vault's current entries, ")
            && other.contains(" aterm secrets need entry:gh_access_token_org ")
            && !other.contains("~/.secrets"),
        "{other}"
    );
    assert_eq!(super::token_step(&named("/h/tokens/w.token")), None);
}

/// A `token_file` under `~/.secrets` is a vault entry, judged from its text alone; any other path
/// is a file of the person's own, and a path deeper inside `~/.secrets` names nothing.
#[test]
fn a_secrets_path_names_a_vault_entry_and_nothing_else_there() {
    let home = Path::new("/h");
    let entry = |name: &str| Some(Source::Entry(VaultEntry::new(name, home)));
    assert_eq!(
        crate::config::token_source("~/.secrets/x", home),
        entry("x")
    );
    assert_eq!(
        crate::config::token_source("/h/.secrets/x", home),
        entry("x")
    );
    assert_eq!(
        crate::config::token_source("/h/tokens/x", home),
        Some(Source::File("/h/tokens/x".into()))
    );
    assert_eq!(
        crate::config::token_source("/elsewhere/.secrets/x", home),
        Some(Source::File("/elsewhere/.secrets/x".into()))
    );
    for deeper in ["~/.secrets/a/b", "~/.secrets", "~/.secrets/../x/y"] {
        assert_eq!(crate::config::token_source(deeper, home), None, "{deeper}");
    }
    assert_eq!(crate::config::token_source("relative", home), None);
}

/// A vault entry reads from the store and never from the file an older build wrote under
/// `~/.secrets`: a file there that differs neither supplies nor blocks the token, and a file
/// whose entry the store does not hold is no token (the negative control).
#[test]
fn a_vault_entry_reads_the_store_and_never_the_old_file() {
    use crate::identity::{Profile, load};
    use crate::policy::Tier;
    let root = crate::test_fixtures::scratch_dir("vault-in-place");
    let home = root.path().join("home");
    let token = agh_fake::token("store");
    let profile = Profile::new("dev", Slot::Dev.source(&home), Tier::Dev);
    let file = Slot::Dev.path(&home);
    std::fs::create_dir_all(file.parent().expect("the folder")).expect("the home");
    std::fs::write(&file, agh_fake::token("file")).expect("an old build's file");
    assert_eq!(load(&profile).map(|_| ()), Err(LoadError::Missing));
    super::put_entry_for_tests(&home, Slot::Dev.entry(), Some(token.as_bytes()));
    std::fs::write(&file, agh_fake::token("forged")).expect("the old file rewritten");
    let cred = load(&profile).expect("the store's token");
    assert_eq!(
        cred.fingerprint(),
        crate::identity::fingerprint(token.as_bytes())
    );
    assert_eq!(cred.file_id(), None);
    std::fs::remove_file(&file).expect("the old file removed");
    let cred = load(&profile).expect("the store's token without the file");
    assert_eq!(
        cred.fingerprint(),
        crate::identity::fingerprint(token.as_bytes())
    );
}

/// An owner no routed token writes is asked of the vault, and the row that routes it names the
/// vault entry by its agh.toml spelling.
#[test]
fn an_uncovered_owner_names_the_entry_and_its_file() {
    let step = super::owner_step("fixture-ext", "NAME", "dev|external");
    assert!(step.starts_with("aterm secrets need entry:NAME "), "{step}");
    assert!(step.contains("--file ~/.secrets/NAME "), "{step}");
    assert!(step.contains("--owner fixture-ext --write"), "{step}");
}
