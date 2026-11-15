// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// An obviously synthetic phrase (the key-material census reads a stepped run as synthetic).
const PHRASE: &str = "0123456789abcdefghjkmnpqrstvwxyz0123456789abcdefghj0";

#[test]
fn the_grammar_folds_what_a_reader_forgives_and_names_no_symbol_in_a_refusal() {
    let mut out = [0u8; LEN];
    symbols_into(PHRASE.as_bytes(), &mut out).unwrap();
    assert_eq!(&out[..], PHRASE.as_bytes());
    let grouped: String = PHRASE
        .as_bytes()
        .chunks(4)
        .map(|c| std::str::from_utf8(c).unwrap().to_ascii_uppercase())
        .collect::<Vec<_>>()
        .join("-");
    let mut again = [0u8; LEN];
    symbols_into(format!(" {grouped}\n").as_bytes(), &mut again).unwrap();
    assert_eq!(again, out, "case, hyphens and whitespace are forgiven");
    let mut folded = [0u8; LEN];
    let oil = PHRASE.replacen('0', "o", 1).replacen('1', "l", 1);
    symbols_into(oil.as_bytes(), &mut folded).unwrap();
    assert_eq!(folded, out, "o reads as 0 and l as 1");

    let mut bad = [7u8; LEN];
    let typo = PHRASE.replacen('5', "u", 1);
    assert_eq!(
        symbols_into(typo.as_bytes(), &mut bad),
        Err(PhraseError::BadChar { position: 6 })
    );
    assert_eq!(bad, [0u8; LEN], "a refusal leaves no symbol behind");
    assert_eq!(
        symbols_into(&PHRASE.as_bytes()[1..], &mut bad),
        Err(PhraseError::Length { got: 51 })
    );
    for refusal in [
        PhraseError::BadChar { position: 6 },
        PhraseError::Length { got: 51 },
    ] {
        assert!(!refusal.message().contains(&PHRASE[..8]));
    }
    assert!(well_formed(PHRASE.as_bytes()));
    assert!(!well_formed(b""));
    assert!(!well_formed(format!("{PHRASE}0").as_bytes()));
}

/// A Mac whose registry names no phrase files `paper-master` for roster signing, and the
/// refusal names that request, never a person.
#[cfg(unix)]
#[test]
fn a_mac_without_the_phrase_files_paper_master_and_says_so() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = crate::client::Paths::for_home(root.path());
    let why = read(&paths).unwrap_err();
    assert!(why.contains("`aterm secrets need paper-master`"), "{why}");
    let needs = crate::need::list(&paths).unwrap();
    assert!(
        needs
            .iter()
            .any(|need| need.want == crate::need::Want::PaperMaster
                && need.purpose == crate::need::Purpose::ReleaseRoster),
        "{needs:?}"
    );
}

/// A new phrase goes into the vault through the admin publish, lands where the registry points,
/// and reads back; a second never replaces it, and bytes that are no phrase are never stored.
///
/// NEGATIVE CONTROL: before the store, the same read finds none.
#[cfg(unix)]
#[test]
fn a_stored_phrase_reads_back_from_the_registry_and_is_never_replaced() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = crate::client::Paths::for_home(&root.path().join("admin"));
    let source = root.path().join("test.vault");
    crate::client::create(&paths, source.to_str().unwrap(), None, Some("admin")).unwrap();
    assert!(read(&paths).is_err(), "CONTROL: no phrase before the store");

    assert!(store(&paths, b"not a phrase").is_err());
    store(&paths, PHRASE.as_bytes()).unwrap();
    assert_eq!(read(&paths).unwrap().as_slice(), PHRASE.as_bytes());
    let registry = crate::registry::read(&paths).unwrap();
    let entry = registry.iter().find(|entry| entry.name == ENTRY).unwrap();
    assert_eq!(entry.purpose, "the release roster's paper master phrase");
    assert!(!format!("{registry:?}").contains(PHRASE));

    let other = PHRASE.replacen('0', "z", 1);
    let refused = store(&paths, other.as_bytes()).unwrap_err().to_string();
    assert!(refused.contains("already holds"), "{refused}");
    assert!(!refused.contains(&PHRASE[..8]) && !refused.contains(&other[..8]));
    assert_eq!(read(&paths).unwrap().as_slice(), PHRASE.as_bytes());
}
