// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

const FIXTURE: &str = "FIXTURE_KEYS";
const FIXTURE_MAX: usize = 4;

const K1: &str = "cw5gIGYQzX6xrhTXjXU9nYfLWeoIkiZ1yUX7d1wmdz8=";
const K2: &str = "bsuawZEJq6qhEpcUovJCFFfMXgp7AgLZHjPvd14qNdc=";
const NEW: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
const NEW2: &str = "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB=";

fn fixture() -> String {
    String::from(
        "// Copyright 2026 Andrew Yates\n\
         \n\
         /// The paper master. Empty here, and therefore INERT.\n\
         ///\n\
         /// A long doc comment mentioning PAPER_MASTER_PUBKEYS by name, twice:\n\
         /// PAPER_MASTER_PUBKEYS is a list for the same reason the keyset is.\n\
         pub const PAPER_MASTER_PUBKEYS: &[&str] = &[];\n\
         \n\
         /// The channel keyset. ORDER IS A CONTRACT: index 0 is the head.\n\
         pub const FIXTURE_KEYS: &[&str] = &[\n\
         \x20   // K1 — HEAD: the key this build signs with.\n\
         \x20   \"cw5gIGYQzX6xrhTXjXU9nYfLWeoIkiZ1yUX7d1wmdz8=\",\n\
         \x20   // K2 — accept-only.\n\
         \x20   \"bsuawZEJq6qhEpcUovJCFFfMXgp7AgLZHjPvd14qNdc=\",\n\
         ];\n\
         \n\
         pub const PKG_ROOT_PUBKEY: &str = \"whatever\";\n",
    )
}

fn changed(e: Edit) -> (String, Vec<String>) {
    match e {
        Edit::Changed { text, members } => (text, members),
        Edit::AlreadyPresent { .. } => panic!("expected a change, got AlreadyPresent"),
    }
}

#[test]
fn both_declaration_shapes_are_read_and_the_head_is_index_zero() {
    let src = fixture();
    let master = read_anchor(&src, MASTER_ANCHOR).expect("the empty form parses");
    assert!(master.members.is_empty());
    assert_eq!(master.head(), None);

    let channel = read_anchor(&src, FIXTURE).expect("the open form parses");
    assert_eq!(channel.members, vec![K1.to_string(), K2.to_string()]);
    assert_eq!(channel.head(), Some(K1), "index 0 is the head");
}

#[test]
fn appending_the_same_key_twice_produces_one_entry() {
    let src = fixture();
    let (once, members) = changed(
        append_member(&src, FIXTURE, NEW, &["first pass"], FIXTURE_MAX)
            .expect("the first append plans"),
    );
    assert_eq!(
        members,
        vec![K1.to_string(), K2.to_string(), NEW.to_string()]
    );
    assert_eq!(once.matches(NEW).count(), 1, "one entry after one append");

    let second = append_member(&once, FIXTURE, NEW, &["second pass"], FIXTURE_MAX)
        .expect("the second append plans");
    match second {
        Edit::AlreadyPresent { members } => {
            assert_eq!(
                members,
                vec![K1.to_string(), K2.to_string(), NEW.to_string()]
            );
        }
        Edit::Changed { .. } => panic!("a re-append must be a no-op, not a second entry"),
    }
    let after = read_anchor(&once, FIXTURE).unwrap();
    assert_eq!(after.members.iter().filter(|m| *m == NEW).count(), 1);
    assert_eq!(once.matches(NEW).count(), 1);
}

#[test]
fn the_existing_keys_survive_and_the_head_stays_at_index_zero() {
    let src = fixture();
    let (text, _) = changed(append_member(&src, FIXTURE, NEW, &["m3"], FIXTURE_MAX).unwrap());
    let after = read_anchor(&text, FIXTURE).unwrap();
    assert_eq!(
        after.members,
        vec![K1.to_string(), K2.to_string(), NEW.to_string()],
        "the incumbents keep their order and the new key lands at the tail"
    );
    assert_eq!(after.head(), Some(K1), "the head is never reordered");

    let (text2, _) = changed(append_member(&text, FIXTURE, NEW2, &["m11"], FIXTURE_MAX).unwrap());
    let after2 = read_anchor(&text2, FIXTURE).unwrap();
    assert_eq!(after2.head(), Some(K1));
    assert_eq!(after2.members.len(), 4);
}

#[test]
fn nothing_outside_the_edited_block_is_disturbed() {
    let src = fixture();
    let (text, _) =
        changed(append_member(&src, FIXTURE, NEW, &["provenance"], FIXTURE_MAX).unwrap());
    let before: Vec<&str> = src.lines().collect();
    let after: Vec<&str> = text.lines().collect();
    assert_eq!(
        after.len(),
        before.len() + 2,
        "one comment line and one key line"
    );
    let mut i = 0usize;
    for line in &before {
        let pos = after[i..]
            .iter()
            .position(|l| l == line)
            .expect("every original line survives, in order");
        i += pos + 1;
    }
    assert!(text.contains("/// The paper master. Empty here, and therefore INERT."));
    assert!(text.contains("/// The channel keyset. ORDER IS A CONTRACT: index 0 is the head."));
    assert!(text.contains("pub const PKG_ROOT_PUBKEY: &str = \"whatever\";"));
    assert!(
        read_anchor(&text, MASTER_ANCHOR)
            .unwrap()
            .members
            .is_empty()
    );
}

#[test]
fn the_empty_form_opens_into_a_one_member_list_without_touching_its_doc() {
    let src = fixture();
    let (text, members) = changed(
        append_member(
            &src,
            MASTER_ANCHOR,
            NEW,
            &["the paper master, armed by setup"],
            MAX_MASTER_MEMBERS,
        )
        .unwrap(),
    );
    assert_eq!(members, vec![NEW.to_string()]);
    assert_eq!(read_anchor(&text, MASTER_ANCHOR).unwrap().members, members);
    assert!(
        text.contains("/// PAPER_MASTER_PUBKEYS is a list for the same reason the keyset is."),
        "the doc comment naming the constant must survive verbatim"
    );
    assert!(!text.contains("= &[];"), "the empty form is gone");
    assert_eq!(
        read_anchor(&text, FIXTURE).unwrap().members,
        vec![K1.to_string(), K2.to_string()]
    );

    let last = "pub const PAPER_MASTER_PUBKEYS: &[&str] = &[];";
    let (text, members) =
        changed(append_member(last, MASTER_ANCHOR, NEW, &[], MAX_MASTER_MEMBERS).unwrap());
    assert_eq!(read_anchor(&text, MASTER_ANCHOR).unwrap().members, members);
    assert!(text.ends_with("];"), "{text:?}");
}

#[test]
fn trailing_whitespace_on_a_neighbouring_line_does_not_poison_the_indent() {
    let src = fixture()
        .replace(
            "    // K1 — HEAD: the key this build signs with.",
            "    // K1 — HEAD: the key this build signs with.  ",
        )
        .replace(
            "    \"cw5gIGYQzX6xrhTXjXU9nYfLWeoIkiZ1yUX7d1wmdz8=\",",
            "    \"cw5gIGYQzX6xrhTXjXU9nYfLWeoIkiZ1yUX7d1wmdz8=\",   ",
        );
    assert_ne!(
        src,
        fixture(),
        "the fixture must actually carry the whitespace"
    );

    let (text, members) = changed(
        append_member(&src, FIXTURE, NEW, &["m3"], FIXTURE_MAX)
            .expect("a line with trailing whitespace is still a line"),
    );
    assert_eq!(
        members,
        vec![K1.to_string(), K2.to_string(), NEW.to_string()]
    );
    assert!(text.contains("\n    // m3\n"), "{text}");
    let mut entry = String::from("\n    \"");
    entry.push_str(NEW);
    entry.push_str("\",\n");
    assert!(text.contains(&entry), "{text}");
    assert!(
        !text.contains("\"cw//"),
        "no key bytes leaked into the comment: {text}"
    );
    assert_eq!(read_anchor(&text, FIXTURE).unwrap().members, members);
}

#[test]
fn a_multibyte_comment_with_trailing_whitespace_does_not_panic() {
    let src = "pub const FIXTURE_KEYS: &[&str] = &[\n\
         \x20   // é    \n\
         \x20   \"cw5gIGYQzX6xrhTXjXU9nYfLWeoIkiZ1yUX7d1wmdz8=\",\n\
         ];\n";
    let anchor = read_anchor(src, FIXTURE).expect("this must not panic");
    assert_eq!(anchor.members, vec![K1.to_string()]);
    assert_eq!(
        anchor.indent, "    ",
        "the indent is the leading run and nothing else"
    );
    let (text, _) = changed(append_member(src, FIXTURE, NEW, &["m3"], FIXTURE_MAX).unwrap());
    assert!(
        text.contains("    // é    \n"),
        "the comment survives verbatim: {text}"
    );
    assert_eq!(read_anchor(&text, FIXTURE).unwrap().members.len(), 2);
}

#[test]
fn a_crlf_file_keeps_its_line_endings() {
    let src = "pub const PAPER_MASTER_PUBKEYS: &[&str] = &[];\r\n\
         pub const FIXTURE_KEYS: &[&str] = &[\r\n\
         \x20   \"cw5gIGYQzX6xrhTXjXU9nYfLWeoIkiZ1yUX7d1wmdz8=\",\r\n\
         ];\r\n";
    let (text, _) = changed(append_member(src, FIXTURE, NEW, &["m3"], FIXTURE_MAX).unwrap());
    assert!(text.contains("    // m3\r\n"), "{text:?}");
    assert!(
        !text.contains("// m3\n    \""),
        "no bare LF was spliced in: {text:?}"
    );
    assert_eq!(text.matches('\n').count(), text.matches("\r\n").count());

    let (text, _) =
        changed(append_member(src, MASTER_ANCHOR, NEW2, &["master"], MAX_MASTER_MEMBERS).unwrap());
    assert_eq!(
        text.matches('\n').count(),
        text.matches("\r\n").count(),
        "{text:?}"
    );
    assert_eq!(
        read_anchor(&text, MASTER_ANCHOR).unwrap().members,
        vec![NEW2.to_string()]
    );
}

#[test]
fn unrecognised_shapes_are_refused_not_guessed_at() {
    let err = read_anchor("// nothing here\n", FIXTURE).unwrap_err();
    assert!(err.contains("no `pub const"), "{err}");
    assert!(err.contains("will not guess"), "{err}");

    let twice = fixture().replace(
        "pub const PKG_ROOT_PUBKEY: &str = \"whatever\";",
        "pub const FIXTURE_KEYS: &[&str] = &[];",
    );
    let err = read_anchor(&twice, FIXTURE).unwrap_err();
    assert!(err.contains("2 declarations"), "{err}");
    let nested = fixture() + "mod nested {\n    pub const FIXTURE_KEYS: &[&str] = &[];\n}\n";
    assert_eq!(
        read_anchor(&nested, FIXTURE).unwrap().members,
        [K1.to_string(), K2.to_string()]
    );

    let arrayed = fixture().replace(
        "pub const FIXTURE_KEYS: &[&str] = &[",
        "pub const FIXTURE_KEYS: &[&str; 2] = &[",
    );
    let err = read_anchor(&arrayed, FIXTURE).unwrap_err();
    assert!(err.contains("refusing to guess"), "{err}");

    let computed = fixture().replace(
        "    \"bsuawZEJq6qhEpcUovJCFFfMXgp7AgLZHjPvd14qNdc=\",",
        "    SOME_OTHER_CONST,",
    );
    let err = read_anchor(&computed, FIXTURE).unwrap_err();
    assert!(err.contains("does not recognise"), "{err}");
    assert!(err.contains("SOME_OTHER_CONST"), "{err}");

    let no_comma = fixture().replace(
        "    \"bsuawZEJq6qhEpcUovJCFFfMXgp7AgLZHjPvd14qNdc=\",",
        "    \"bsuawZEJq6qhEpcUovJCFFfMXgp7AgLZHjPvd14qNdc=\"",
    );
    assert!(read_anchor(&no_comma, FIXTURE).is_err());

    for line in [
        "    \"bsuawZEJq6qhEpcUovJCFFfMXgp7AgLZHjPvd14qNd\\x41\",",
        "    \"bsuawZEJq6qhEpcUovJCFFfMXgp7AgLZHjPvd14qNdc=\", \"x\",",
        "    ];\n    \"bsuawZEJq6qhEpcUovJCFFfMXgp7AgLZHjPvd14qNdc=\",",
    ] {
        let odd = fixture().replace(
            "    \"bsuawZEJq6qhEpcUovJCFFfMXgp7AgLZHjPvd14qNdc=\",",
            line,
        );
        assert!(read_anchor(&odd, FIXTURE).is_err(), "{line}");
    }

    let swallowed = fixture().replace("];\n", "");
    let err = read_anchor(&swallowed, FIXTURE).unwrap_err();
    assert!(err.contains("does not recognise"), "{err}");
    assert!(err.contains("PKG_ROOT_PUBKEY"), "{err}");

    let unterminated = "pub const FIXTURE_KEYS: &[&str] = &[\n    \"x\",\n";
    let err = read_anchor(unterminated, FIXTURE).unwrap_err();
    assert!(err.contains("never closed"), "{err}");

    assert!(append_member(&computed, FIXTURE, NEW, &[], 4).is_err());
    assert!(append_member(&fixture(), FIXTURE, NEW, &[], 4).is_ok());
}

#[test]
fn an_anchor_holding_an_empty_member_is_refused() {
    let bricked = fixture().replace(
        "    \"cw5gIGYQzX6xrhTXjXU9nYfLWeoIkiZ1yUX7d1wmdz8=\",",
        "    \"\",",
    );
    assert_eq!(read_anchor(&bricked, FIXTURE).unwrap().members[0], "");
    let err = append_member(&bricked, FIXTURE, NEW, &[], 4).unwrap_err();
    assert!(err.contains("empty string"), "{err}");
    assert!(err.contains("brick"), "{err}");
}

#[test]
fn a_malformed_key_is_refused_before_any_edit_is_planned() {
    let src = fixture();
    let truncated = &NEW[..43];
    let err = append_member(&src, FIXTURE, truncated, &[], 4).unwrap_err();
    assert!(err.contains("exactly 44"), "{err}");
    assert!(err.contains("43"), "{err}");

    let unpadded = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    assert_eq!(unpadded.len(), 44, "the fixture must be the right length");
    assert!(append_member(&src, FIXTURE, unpadded, &[], 4).is_err());

    let double_padded = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==";
    assert_eq!(double_padded.len(), 44);
    assert!(append_member(&src, FIXTURE, double_padded, &[], 4).is_err());

    let interior_pad = "A=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
    assert_eq!(interior_pad.len(), 44);
    assert!(append_member(&src, FIXTURE, interior_pad, &[], 4).is_err());

    let bad_alphabet = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA \"=";
    assert_eq!(bad_alphabet.len(), 44);
    let err = append_member(&src, FIXTURE, bad_alphabet, &[], 4).unwrap_err();
    assert!(err.contains("non-base64"), "{err}");

    assert!(append_member(&src, FIXTURE, "", &[], 4).is_err());
}

#[test]
fn the_keyset_ceiling_is_enforced_by_the_writer() {
    let src = fixture();
    let (one, _) =
        changed(append_member(&src, MASTER_ANCHOR, NEW, &[], MAX_MASTER_MEMBERS).unwrap());
    let (two, _) =
        changed(append_member(&one, MASTER_ANCHOR, NEW2, &[], MAX_MASTER_MEMBERS).unwrap());
    let third = "CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC=";
    let err = append_member(&two, MASTER_ANCHOR, third, &[], MAX_MASTER_MEMBERS).unwrap_err();
    assert!(err.contains("ceiling is 2"), "{err}");
    assert!(err.contains("rotation window"), "{err}");
}

#[test]
fn verification_catches_a_write_that_did_not_land() {
    let src = fixture();
    let (text, members) = changed(append_member(&src, FIXTURE, NEW, &["m3"], FIXTURE_MAX).unwrap());
    verify_members(&text, FIXTURE, &members).expect("the intended write verifies");

    let err = verify_members(&src, FIXTURE, &members).unwrap_err();
    assert!(err.contains("holds 2 keys but 3 were intended"), "{err}");

    let mangled = text.replace(NEW, "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAB=");
    assert_ne!(mangled, text, "the mutation must actually change the text");
    let err = verify_members(&mangled, FIXTURE, &members).unwrap_err();
    assert!(err.contains("[2]"), "{err}");
    assert!(err.contains("was intended"), "{err}");

    let reordered = {
        let mut m = members.clone();
        m.swap(0, 2);
        m
    };
    assert!(verify_members(&text, FIXTURE, &reordered).is_err());

    let err = verify_members("// gone\n", FIXTURE, &members).unwrap_err();
    assert!(err.contains("could not be read back"), "{err}");

    assert!(append_member(&src, FIXTURE, NEW, &["two\nlines"], FIXTURE_MAX).is_err());
    assert!(append_member(&src, FIXTURE, NEW, &["two\rlines"], FIXTURE_MAX).is_err());
}

#[test]
fn the_shipped_pins_file_is_a_shape_this_writer_understands() {
    let path = aterm_tempfile::manifest_dir().join("../aterm-update-core/src/pins.rs");
    let path = path.to_str().expect("a UTF-8 checkout path");
    let src =
        String::from_utf8(crate::fsio::read_bytes(path).expect("the anchor file is in the tree"))
            .unwrap();
    let master = read_anchor(&src, MASTER_ANCHOR).expect("the master anchor is readable");
    assert_eq!(
        master.members,
        aterm_update_core::pins::PAPER_MASTER_PUBKEYS
            .iter()
            .map(|k| (*k).to_string())
            .collect::<Vec<_>>(),
        "what the writer reads out of the source must be what the compiler compiled in"
    );
}
