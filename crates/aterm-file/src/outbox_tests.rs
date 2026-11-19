// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Draft, Edit, Judged};
use crate::{File, Name, Refused, Root};
use aterm_json::Value;

fn stated(key: &str, column: &str, text: &str, judged: Judged) -> Draft {
    Draft {
        born: false,
        revert: None,
        edits: vec![Edit {
            key: format!("{key}.{}", super::SELF),
            column: column.to_owned(),
            elem: None,
            value: Value::from(text),
            text: text.to_owned(),
            expected: Vec::new(),
            judged,
        }],
    }
}

/// The outbox judges a cell by its column's family (docs/DESIGN-lab-tracker-2026-11-14.md §7,
/// finding 34; ATERM-KI-1114-592): a commit's full hash or a known issue's id is queued in a
/// column judged [`Judged::Secret`] (the tracker's commit and prose columns) and a read lays it
/// over the snapshot, while a free-text column ([`Judged::Text`]) still refuses both; a provider
/// token is refused in either, and a line holding one in a commit column, written by another
/// build, is never shown by a read.
///
/// NEGATIVE CONTROL, observed: with [`Judged::refuses`] answering
/// `credential_shaped || phone_shaped` for both judgments (the outbox before TK-30), the commit
/// column's write of the hash is refused: `the commit column queues a full hash:
/// Err(Malformed("a secret is never written to a shared file"))`.
#[test]
fn an_intent_holding_a_full_commit_hash_in_a_commit_column_is_queued_and_a_token_in_any_column_is_refused()
 {
    let dir = aterm_tempfile::tempdir().expect("a scratch folder");
    let root = Root::at(&dir.path().join("files"));
    let file = File::at(&root, &Name::parse("lab/tracker").expect("a name"));
    let hash = "aead5f36955c1b0e0d6a8f2b7c4e9d1a3f5b7c90";
    let issue = "closes ATERM-KI-1114-592";
    // Bound apart from its prefix, so no scanner of this source reads it as a key.
    let token = ["ghp", "_", &"Zx9q".repeat(9)].concat();
    assert_eq!(hash.len(), 40);

    let queued = file.state_mine(
        "harness",
        stated("tk-30", "commit", hash, Judged::Secret),
        1,
    );
    assert!(
        queued.is_ok(),
        "the commit column queues a full hash: {queued:?}"
    );
    let queued = file.state_mine("harness", stated("tk-30", "note", issue, Judged::Secret), 2);
    assert!(
        queued.is_ok(),
        "a prose column queues an issue's id: {queued:?}"
    );

    for (value, judged) in [
        (hash, Judged::Text),
        (issue, Judged::Text),
        (token.as_str(), Judged::Text),
        (token.as_str(), Judged::Secret),
    ] {
        let refused = file.state_mine("harness", stated("tk-31", "commit", value, judged), 3);
        assert!(
            matches!(refused, Err(Refused::Malformed(_))),
            "{judged:?} refuses a value of {} bytes: {refused:?}",
            value.len()
        );
    }
    let outbox = file.outbox();
    assert_eq!(
        outbox.queued.len(),
        2,
        "only the two accepted intents are queued"
    );

    let shown = |file: &File, column: &str| {
        file.read()
            .rows()
            .iter()
            .find(|row| row.pk == format!("tk-30.{}", super::SELF))
            .map(|row| row.cell(column).to_owned())
            .unwrap_or_default()
    };
    assert_eq!(shown(&file, "commit"), hash, "a read lays the queued hash");
    assert_eq!(shown(&file, "note"), issue);

    // A line another build left holding a token in a commit column: a read shows the mark.
    let path = root.outbox(file.name());
    let lines = std::fs::read_to_string(&path).expect("the outbox");
    assert!(lines.contains("\"judged\":\"secret\""), "{lines}");
    std::fs::write(&path, lines.replace(hash, &token)).expect("planted");
    assert_eq!(shown(&file, "commit"), aterm_value_guard::REFUSED);
}
