// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn doc() -> Document {
    let recipient = || age::Identity::generate().unwrap().recipient().to_bech32();
    let mut doc = Document::new_vault("m22", &recipient());
    for host in ["m3", "andrews"] {
        doc.add_machine(host, &recipient()).unwrap();
    }
    let m3 = Audience::Hosts(vec!["m3".into()]);
    doc.set_entry(&m3, "notary", b"apple").unwrap();
    doc.set_entry(&m3, "other", b"m3 only").unwrap();
    doc
}

#[test]
fn a_copy_puts_only_the_entry_in_the_requesters_own_section_and_leaves_the_header() {
    let published = doc();
    let mut draft = published.clone();
    let copied = copy_into_host(&published, &mut draft, "notary", "andrews", true).unwrap();
    assert_eq!(copied, Copied::Copied);
    let own = Audience::Hosts(vec!["andrews".into()]);
    let m3 = Audience::Hosts(vec!["m3".into()]);
    let mine = draft.entry(&own, "notary").unwrap();
    assert!(mine.if_absent && mine.value.as_slice() == b"apple");
    assert!(
        draft.entry(&own, "other").is_none(),
        "the rest of [m3] stays m3's"
    );
    assert!(draft.entry(&m3, "notary").is_some() && draft.entry(&m3, "other").is_some());
    assert!(
        draft
            .render()
            .as_str()
            .unwrap()
            .lines()
            .any(|line| line == "[m3]"),
        "[m3]'s header is unchanged"
    );
    // The copy parses as any vault text does, and a second copy changes nothing.
    Document::parse(draft.render().as_str().unwrap()).unwrap();
    assert_eq!(
        copy_into_host(&published, &mut draft, "notary", "andrews", true).unwrap(),
        Copied::AlreadyThere
    );
    assert_eq!(
        copy_into_host(&published, &mut draft, "absent", "andrews", false).unwrap(),
        Copied::NotHeld
    );
}

/// A host that already reads the entry through a section it shares gets no second copy,
/// which the vault text would refuse (one name in two sections that share a host).
#[test]
fn a_host_that_reads_the_entry_through_a_shared_section_gets_no_copy() {
    let mut published = doc();
    let shared = Audience::Hosts(vec!["andrews".into(), "m3".into()]);
    published.set_entry(&shared, "shared", b"both").unwrap();
    published
        .set_entry(&Audience::Everyone, "everywhere", b"all")
        .unwrap();
    let mut draft = published.clone();
    for entry in ["shared", "everywhere"] {
        assert_eq!(
            copy_into_host(&published, &mut draft, entry, "andrews", false).unwrap(),
            Copied::AlreadyThere,
            "{entry}"
        );
    }
    assert!(
        draft.render().as_slice() == published.render().as_slice(),
        "the draft is unchanged"
    );
    Document::parse(draft.render().as_str().unwrap()).unwrap();
}
