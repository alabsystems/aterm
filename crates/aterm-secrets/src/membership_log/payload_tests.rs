// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::SecretBytes;
use crate::text::{Audience, Entry};

fn encrypted_records() -> Vec<Record> {
    let mut log = found(&[(A, "m1")]);
    let (draft, key) = log
        .draft_epoch(&A.id(), 2, EpochAudience::Everyone, 1)
        .unwrap();
    let epoch = log.keep(draft.sign(&A.seed(), A.incarnation()).unwrap());
    let wrap = log
        .draft_wrap(&A.id(), 3, *epoch.id(), &key, A.identity().recipient())
        .unwrap()
        .sign(&A.seed(), A.incarnation())
        .unwrap();
    let wrap = log.keep(wrap);
    let entry = Entry {
        name: "payload-owner".into(),
        value: SecretBytes::new(vec![0xa7; 4096]),
        audience: Audience::Everyone,
        if_absent: false,
        anchor: None,
    };
    let put = log
        .draft_put(&A.id(), 4, *epoch.id(), &key, &entry)
        .unwrap()
        .sign(&A.seed(), A.incarnation())
        .unwrap();
    let put = log.keep(put);
    let delete = log
        .draft_delete(&A.id(), 5, *epoch.id(), &key, &entry.name)
        .unwrap()
        .sign(&A.seed(), A.incarnation())
        .unwrap();
    vec![wrap, put, delete]
}

fn ciphertext(body: &Body) -> &Payload {
    match body {
        Body::Wrap { capsule, .. } => capsule,
        Body::Put { ciphertext, .. } | Body::Delete { ciphertext, .. } => ciphertext,
        _ => panic!("fixture requires encrypted records"),
    }
}

#[test]
fn decoded_ciphertext_payloads_outlive_input_and_record_owners() {
    let records = encrypted_records();
    let mut retained = Vec::new();
    for original in &records {
        let mut input = original.bytes().to_vec();
        let decoded = Record::decode(&input).unwrap();
        assert_eq!(decoded.id(), original.id());
        assert_eq!(decoded.bytes(), original.bytes());
        let expected = ciphertext(original.body()).as_ref().to_vec();
        let body = decoded.body().clone();
        input.fill(0);
        assert_eq!(ciphertext(decoded.body()).as_ref(), expected);
        assert_eq!(decoded.bytes(), original.bytes());
        drop(input);
        drop(decoded);
        retained.push((body, expected));
    }
    drop(records);
    for (body, expected) in retained {
        assert_eq!(ciphertext(&body).as_ref(), expected);
        let standalone = ciphertext(&body).clone();
        drop(body);
        assert_eq!(standalone.as_ref(), expected);
    }
}

#[test]
fn a_draft_can_replace_copied_payload_bytes_without_changing_signed_records() {
    for original in encrypted_records() {
        let old_wire = original.bytes().to_vec();
        let old_payload = ciphertext(original.body()).as_ref().to_vec();
        let make_draft = |body| Draft {
            network: original.header.network,
            sequence: original.header.sequence,
            previous: original.header.previous,
            parents: original.header.parents.clone(),
            authored_ms: original.header.authored_ms,
            body,
        };
        let unchanged = make_draft(original.body().clone())
            .sign(&A.seed(), A.incarnation())
            .unwrap();
        assert_eq!(unchanged.bytes(), old_wire);
        let mut body = original.body().clone();
        let mut replacement = ciphertext(&body).as_ref().to_vec();
        replacement[0] ^= 1;
        match &mut body {
            Body::Wrap { capsule, .. } => *capsule = replacement.clone().into(),
            Body::Put { ciphertext, .. } | Body::Delete { ciphertext, .. } => {
                *ciphertext = replacement.clone().into();
            }
            _ => unreachable!(),
        }
        let changed = make_draft(body).sign(&A.seed(), A.incarnation()).unwrap();
        assert_ne!(changed.id(), original.id());
        assert_eq!(ciphertext(changed.body()).as_ref(), replacement);
        assert_eq!(ciphertext(original.body()).as_ref(), old_payload);
        assert_eq!(original.bytes(), old_wire);
        assert_eq!(unchanged.bytes(), old_wire);
    }
}

#[test]
fn ciphertext_payloads_keep_signature_truncation_and_length_refusals() {
    for original in encrypted_records() {
        let payload = ciphertext(original.body()).as_ref();
        let at = original
            .bytes()
            .windows(payload.len())
            .position(|bytes| bytes == payload)
            .unwrap();
        let mut corrupt = original.bytes().to_vec();
        corrupt[at] ^= 1;
        assert!(Record::decode(&corrupt).is_err());
        assert!(Record::decode(&original.bytes()[..at + payload.len() / 2]).is_err());
        let mut trailing = original.bytes().to_vec();
        trailing.push(0);
        assert!(Record::decode(&trailing).is_err());
        let mut body = original.body().clone();
        match &mut body {
            Body::Wrap { capsule, .. } => *capsule = Vec::new().into(),
            Body::Put { ciphertext, .. } | Body::Delete { ciphertext, .. } => {
                *ciphertext = vec![0; 16].into();
            }
            _ => unreachable!(),
        }
        let draft = Draft {
            network: original.header.network,
            sequence: original.header.sequence,
            previous: original.header.previous,
            parents: original.header.parents.clone(),
            authored_ms: original.header.authored_ms,
            body,
        };
        assert!(draft.sign(&A.seed(), A.incarnation()).is_err());
    }
    assert!(Record::decode(&vec![0; MAX_RECORD + 1]).is_err());
}
