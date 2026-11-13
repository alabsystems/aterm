// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const PIN: [u8; 32] = [53; 32];
const NEXT: [u8; 32] = [67; 32];
const UNRELATED: [u8; 32] = [71; 32];
const VAULT: &str = "lineage-test";

fn wire(name: &str, seeds: &[[u8; 32]]) -> Vec<u8> {
    signed(
        name,
        seeds.last().unwrap(),
        envelope::test_chain(name, seeds),
    )
}

fn signed(name: &str, seed: &[u8; 32], handoff: Vec<([u8; 32], [u8; 64])>) -> Vec<u8> {
    envelope::sign_unchecked(
        &envelope::Unsigned {
            name: name.into(),
            seq: 1,
            time: 1,
            handoff,
            parents: Vec::new(),
            lockers: vec![envelope::Locker {
                label: "admin".into(),
                armored: envelope::test_armor(b"inert lineage fixture"),
            }],
        },
        seed,
    )
    .unwrap()
}

fn configured() -> (aterm_tempfile::TempDir, Paths) {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(home.path());
    super::super::save_config(
        &paths,
        &Config {
            source: Source::Path(home.path().join("unused.secrets")),
            mirrors: Vec::new(),
            name: VAULT.into(),
            signer: Some(envelope::signer_public(&PIN).unwrap()),
            followed: Vec::new(),
            host: "m1".into(),
        },
    )
    .unwrap();
    (home, paths)
}

#[test]
fn genesis_lineage_accepts_an_endorsed_rotation_before_the_pin_is_updated() {
    let (_home, paths) = configured();
    let pin = envelope::signer_public(&PIN).unwrap();
    assert!(genesis_signer_under(&Reads::new(&paths), &pin).unwrap());
    let bytes = wire(VAULT, &[PIN, NEXT]);
    fsx::write_atomic(&paths.cache_file(), &bytes, 0o600).unwrap();
    assert!(
        genesis_signer_under(
            &Reads::new(&paths),
            &envelope::signer_public(&NEXT).unwrap()
        )
        .unwrap()
    );
}

#[test]
fn genesis_lineage_refuses_unverified_or_unrelated_cached_claims() {
    let valid = wire(VAULT, &[PIN, NEXT]);
    let damaged = String::from_utf8(valid.clone())
        .unwrap()
        .replace("seq: 1\n", "seq: 2\n")
        .into_bytes();
    assert_ne!(valid, damaged, "the negative fixture changed signed bytes");
    let cases = [
        ("unrelated signer", wire(VAULT, &[NEXT])),
        ("other vault", wire("other-vault", &[PIN, NEXT])),
        ("damaged signature", damaged),
        ("unrelated handoff", wire(VAULT, &[UNRELATED, NEXT])),
    ];
    for (why, bytes) in cases {
        let (_home, paths) = configured();
        fsx::write_atomic(&paths.cache_file(), &bytes, 0o600).unwrap();
        assert!(
            !genesis_signer_under(
                &Reads::new(&paths),
                &envelope::signer_public(&NEXT).unwrap()
            )
            .unwrap(),
            "accepted {why}"
        );
    }
}

/// A store holding only a genesis `seed` signed for `root`, which takes a genesis of `seed`'s
/// key alone: what a caller hands [`commons_log_under`] when the store it classifies
/// was not read under this Mac's lineage (a removal plan's or a retirement's candidate).
fn founded_by(seed: &[u8; 32], root: Root) -> Store {
    let request = crate::commons::binding::Request::create(
        root.network,
        root.authority,
        [0x21; 16],
        [1; 16],
        &Identity::from_secret_bytes([0x41; 32]),
        &[1; 32],
    )
    .unwrap();
    let entrant = Entrant {
        request,
        node_key: [0xAA; 32],
        name: "m1".into(),
        label: "m1".into(),
    };
    let genesis = Record::found(seed, vec![entrant], 1).unwrap();
    let mut store = Store::dormant(&[envelope::signer_public(seed).unwrap()]);
    assert!(store.insert(genesis).unwrap());
    store
}

/// PF4: a store's log is classified for judging a Commons peer, and a log of another vault
/// lineage or another root is told apart from one with nothing to judge by
/// ([`CommonsLog`]), whether or not it carries Commons epochs: root and lineage are judged
/// first.
///
/// NEGATIVE CONTROL (in the test): the `Option` reading
/// ([`trusted_commons_log_from_store`], all there was before PF4) answers `None` for the
/// foreign log and for this vault's log with no Commons epoch alike, so nothing reading it
/// could refuse the first without refusing the second.
#[test]
fn a_log_of_another_lineage_is_told_from_one_with_nothing_to_judge() {
    let (_home, paths) = configured();
    let root = Root {
        network: [3; 16],
        authority: [4; 32],
    };
    let trust = crate::commons::journal::Trust {
        network: root.network,
        authority: root.authority,
        genesis: [0; 32],
    };
    let kind = |store: &Store, trust| match commons_log_under(&Reads::new(&paths), store, trust)
        .unwrap()
    {
        CommonsLog::Trusted(..) => "trusted",
        CommonsLog::NoLog => "no log",
        CommonsLog::NoCommonsEpochs => "no commons epochs",
        CommonsLog::OtherRoot => "other root",
        CommonsLog::OtherLineage => "other lineage",
    };
    let ours = founded_by(&PIN, root);
    let foreign = founded_by(&UNRELATED, root);
    assert_eq!(kind(&Store::dormant(&[]), trust), "no log");
    assert_eq!(kind(&ours, trust), "no commons epochs");
    assert_eq!(kind(&foreign, trust), "other lineage");
    let mut elsewhere = trust;
    elsewhere.network[0] ^= 1;
    assert_eq!(kind(&ours, elsewhere), "other root");
    assert_eq!(kind(&foreign, elsewhere), "other root");

    for (why, store) in [("ours", &ours), ("foreign", &foreign)] {
        assert!(
            trusted_commons_log_from_store(&paths, store, trust)
                .unwrap()
                .is_none(),
            "NEGATIVE CONTROL {why}: the Option reading names neither"
        );
    }
}

#[test]
fn genesis_lineage_does_not_admit_a_broken_prefix_before_the_trusted_pin() {
    let (_home, paths) = configured();
    let mut handoff = envelope::test_chain(VAULT, &[UNRELATED, PIN, NEXT]);
    handoff[0].1[0] ^= 1;
    let bytes = signed(VAULT, &NEXT, handoff);
    fsx::write_atomic(&paths.cache_file(), &bytes, 0o600).unwrap();
    assert!(
        genesis_signer_under(
            &Reads::new(&paths),
            &envelope::signer_public(&NEXT).unwrap()
        )
        .unwrap(),
        "the valid suffix from the pinned key remains usable"
    );
    assert!(
        !genesis_signer_under(
            &Reads::new(&paths),
            &envelope::signer_public(&UNRELATED).unwrap()
        )
        .unwrap(),
        "a forged predecessor is not part of the verified suffix"
    );
}
