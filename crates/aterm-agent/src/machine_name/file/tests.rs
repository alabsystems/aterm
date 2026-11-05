// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::os::unix::fs::PermissionsExt as _;

use super::*;

/// Late 2026.
const NOW: u64 = 1_793_000_000_000;

fn state() -> aterm_tempfile::TempDir {
    let root = aterm_tempfile::tempdir().expect("a tempdir");
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))
        .expect("a private dir");
    root
}

fn nick(word: &str) -> Nickname {
    Nickname::parse(word).expect("a nickname")
}

fn num(n: u32) -> Number {
    Number::new(n).expect("a number")
}

fn node(i: u8) -> NodeId {
    NodeId::new(&[i; 16]).expect("a node id")
}

/// Every field present.
fn full() -> MachineName {
    let held = Held {
        num: Some(num(7)),
        class: Class::Seeded,
        since: 3,
        base: Some(node(2)),
        tgen: 1,
        sgen: 0,
    };
    MachineName {
        num: Some(num(8)),
        class: Class::Chosen,
        since: NOW - 10,
        rev: 4,
        prev: Some(num(7)),
        base: Some(node(2)),
        tgen: 2,
        sgen: 1,
        act: NOW,
        cap: Some(Cap {
            value: 40,
            since: NOW - 5,
        }),
        was: vec![
            Was {
                num: num(7),
                retired: false,
                lost: false,
            },
            Was {
                num: num(3),
                retired: true,
                lost: true,
            },
        ],
        by: Some(By {
            node: NodeId::new(&[9; 8]).expect("a mesh id"),
            kind: ByKind::Person,
        }),
        nick: nick("Heron"),
        nick_class: NickClass::Chosen,
        migration: Some(Migration::Waiting),
        undo: Some(held.clone()),
        fleets: vec![FleetClaim {
            digest: [0xab; 8],
            top: 41,
            claim: held,
            lost: vec![num(5)],
            synced: true,
        }],
    }
}

#[test]
fn the_file_spells_every_field_and_reads_back_whole() {
    for name in [full(), MachineName::unnumbered(nick("otter"), NOW)] {
        let text = name.to_toml().expect("serializes");
        assert_eq!(MachineName::from_toml(&text), Ok(name), "{text}");
    }
}

#[test]
fn the_title_is_the_number_and_the_nickname() {
    assert_eq!(full().title(), "m8 Heron");
    assert_eq!(
        MachineName::unnumbered(nick("otter"), NOW).title(),
        "m0 Otter",
        "a drawn noun is capitalised; never numbered reads m0"
    );
}

#[test]
fn a_field_outside_its_grammar_is_refused() {
    let good = full().to_toml().expect("serializes");
    assert!(MachineName::from_toml(&good).is_ok(), "control");
    for (from, to) in [
        ("num = 8", "num = 0"),
        ("num = 8", "num = 1000000"),
        ("nick = \"Heron\"", "nick = \"m12\""),
        ("class = \"chosen\"", "class = \"person\""),
        ("nick_class = \"chosen\"", "nick_class = \"drawn\""),
        ("migration = \"waiting\"", "migration = \"later\""),
        (
            "digest = \"abababababababab\"",
            "digest = \"ABABABABABABABAB\"",
        ),
        ("digest = \"abababababababab\"", "digest = \"abab\""),
    ] {
        assert!(good.contains(from), "the file spells {from}:\n{good}");
        let bad = good.replacen(from, to, 1);
        assert!(MachineName::from_toml(&bad).is_err(), "{to}");
    }
    let mut nine = full();
    nine.was = (1..=9)
        .map(|n| Was {
            num: num(n),
            retired: false,
            lost: false,
        })
        .collect();
    let text = nine.to_toml().expect("serializes");
    assert!(MachineName::from_toml(&text).is_err(), "nine `was` entries");
}

#[test]
fn the_first_reader_creates_it_and_every_other_reads_that_back() {
    let root = state();
    assert_eq!(read(root.path()).expect("reads"), None);
    let (first, created) = read_or_create(root.path(), || {
        Ok(MachineName::unnumbered(nick("otter"), NOW))
    })
    .expect("creates");
    assert!(created);
    let mode = std::fs::metadata(root.path().join(FILE))
        .expect("the file")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    let again = read_or_create(root.path(), || panic!("no second create")).expect("reads");
    assert_eq!(again, (first, false));
}

#[test]
fn racing_first_readers_all_see_one_name() {
    let root = state();
    let words = ["otter", "heron", "kestrel", "puffin"];
    let seen: Vec<(MachineName, bool)> = std::thread::scope(|scope| {
        let handles: Vec<_> = words
            .iter()
            .map(|word| {
                let dir = root.path();
                scope.spawn(move || {
                    read_or_create(dir, || Ok(MachineName::unnumbered(nick(word), NOW)))
                        .expect("reads or creates")
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("joins"))
            .collect()
    });
    let on_disk = read(root.path()).expect("reads").expect("there");
    assert!(seen.iter().all(|(name, _)| *name == on_disk), "{seen:?}");
    assert_eq!(
        seen.iter().filter(|(_, created)| *created).count(),
        1,
        "one reader wrote it: {seen:?}"
    );
}

#[test]
fn a_change_replaces_the_file_under_its_lock() {
    let root = state();
    assert_eq!(
        replace(root.path(), |_| Ok(())).map_err(|e| e.kind()),
        Err(io::ErrorKind::NotFound),
        "no file to change yet"
    );
    read_or_create(root.path(), || {
        Ok(MachineName::unnumbered(nick("otter"), NOW))
    })
    .expect("creates");
    let named = replace(root.path(), |name| {
        name.num = Some(num(179));
        name.rev += 1;
        Ok(())
    })
    .expect("replaces");
    assert_eq!(named.title(), "m179 Otter");
    assert_eq!(read(root.path()).expect("reads"), Some(named.clone()));
    let refused = replace(root.path(), |name| {
        name.num = Some(num(5));
        Err(io::Error::other("refused"))
    });
    assert!(refused.is_err());
    assert_eq!(
        read(root.path()).expect("reads"),
        Some(named),
        "a refused edit writes nothing"
    );
}

#[test]
fn a_drawn_nickname_is_a_noun_none_in_view_uses() {
    let taken = nick("otter");
    let drawn = draw_nick(|word| word.eq_ignore_ascii_case(taken.as_str())).expect("draws");
    assert!(nouns::NOUNS.contains(&drawn.as_str()), "{drawn}");
    assert!(
        MachineName::unnumbered(drawn, NOW)
            .title()
            .starts_with("m0 ")
    );
}
