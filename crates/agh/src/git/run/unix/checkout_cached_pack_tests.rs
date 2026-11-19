// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn index(names: &[String], hash: &str) -> Vec<u8> {
    let mut bytes = vec![0; HEADER + names.len() * 28 + 40];
    bytes[..8].copy_from_slice(&[255, b't', b'O', b'c', 0, 0, 0, 2]);
    for bucket in 0..256 {
        let count = names
            .iter()
            .filter(|name| u8::from_str_radix(&name[..2], 16).unwrap() <= bucket as u8)
            .count() as u32;
        bytes[8 + bucket * 4..12 + bucket * 4].copy_from_slice(&count.to_be_bytes());
    }
    for (i, name) in names.iter().enumerate() {
        assert!(aterm_codec::hex::decode_into(
            name.as_bytes(),
            &mut bytes[HEADER + 20 * i..HEADER + 20 * (i + 1)]
        ));
    }
    let end = bytes.len();
    assert!(aterm_codec::hex::decode_into(
        hash.as_bytes(),
        &mut bytes[end - 40..end - 20]
    ));
    bytes
}

#[test]
fn index_advice_requires_the_complete_exact_ordered_batch_and_pack_binding() {
    let hash = "c".repeat(40);
    let names = vec!["a".repeat(40), "b".repeat(40)];
    let bytes = index(&names, &hash);
    assert!(member_hint(&bytes, &names, &hash));
    for wanted in [
        vec![names[0].clone()],
        vec![names[0].clone(), names[1].clone(), "d".repeat(40)],
        vec![names[0].clone(), "d".repeat(40)],
        vec![names[1].clone(), names[0].clone()],
        vec![names[0].clone(), names[0].clone()],
    ] {
        assert!(!member_hint(&bytes, &wanted, &hash));
    }
    assert!(!member_hint(&bytes, &names, &"d".repeat(40)));
    for length in [0, 7, HEADER - 1, bytes.len() - 1] {
        assert!(!member_hint(&bytes[..length], &names, &hash));
    }
    let mut unknown = bytes.clone();
    unknown[7] = 3;
    assert!(!member_hint(&unknown, &names, &hash));
    let mut extra = bytes.clone();
    extra.extend_from_slice(&[0; 1024]);
    assert!(!member_hint(&extra, &names, &hash));
}

#[test]
fn a_wide_index_is_bounded_by_the_actual_batch_not_a_retained_output_tail() {
    let hash = "c".repeat(40);
    let names: Vec<_> = (0..MAX_OBJECTS).map(|n| format!("{n:040x}")).collect();
    let bytes = index(&names, &hash);
    assert!(bytes.len() > 64 * 1024);
    assert!(member_hint(&bytes, &names, &hash));
    let mut hidden = vec![0; 1024 * 1024];
    hidden.extend_from_slice(&bytes);
    assert!(!member_hint(&hidden, &names, &hash));
    let mut too_many = names.clone();
    too_many.push("d".repeat(40));
    assert!(!member_hint(&index(&too_many, &hash), &too_many, &hash));
}

#[test]
fn local_pack_data_replays_without_a_filesystem_answer_in_either_tape_mode() {
    use crate::fx::{
        Fx,
        tape::{TapeHeader, TapeMode},
    };
    for mode in [TapeMode::Anonymous, TapeMode::Credentialed] {
        let root = crate::test_fixtures::scratch_dir("pack-data-tape");
        let path = root.path().join("data.atape");
        let mut header = TapeHeader::for_run(
            &["cache-data".into()],
            &crate::wire::Endpoints::github_com(),
        );
        header.mode = mode;
        let recording = Fx::record_for_tests(&path, &header).unwrap();
        assert_eq!(
            recording
                .local_data("blob pair", "exact-batch", &mut || "declined".into())
                .unwrap(),
            "declined"
        );
        recording.finish(0).unwrap();
        let (replay, _) = Fx::replay_for_tests(&path).unwrap();
        assert_eq!(
            replay
                .local_data("blob pair", "exact-batch", &mut || panic!(
                    "replay must not reopen cache DATA"
                ))
                .unwrap(),
            "declined"
        );
        replay.finish(0).unwrap();
        assert!(replay.children_run().is_empty());
    }
}
