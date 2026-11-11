// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
fn words(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

#[test]
fn one_guard_is_removed_without_reinterpreting_a_program_payload() {
    let mut args = words(&["--window", FLAG, "17", "--control-sock", "/private/socket"]);
    assert_eq!(
        take(&mut args),
        Ok(Some(GuardBundle {
            lane: 17,
            memory: None
        }))
    );
    assert_eq!(
        args,
        words(&["--window", "--control-sock", "/private/socket"])
    );
    for boundary in ["--", "-e", "--command"] {
        let mut args = words(&["--window", boundary, "program", FLAG, "0"]);
        let original = args.clone();
        assert_eq!(take(&mut args), Ok(None));
        assert_eq!(args, original);
        let mut args = words(&[FLAG, "9", boundary, "program", FLAG, "0"]);
        assert_eq!(
            take(&mut args),
            Ok(Some(GuardBundle {
                lane: 9,
                memory: None
            }))
        );
        assert_eq!(args, words(&[boundary, "program", FLAG, "0"]));
    }
}

#[test]
fn ambiguous_and_invalid_offers_fail_without_consuming_words() {
    for mut args in [
        words(&[FLAG]),
        words(&[FLAG, "0"]),
        words(&[FLAG, "1"]),
        words(&[FLAG, "2"]),
        words(&[FLAG, "-3"]),
        words(&[FLAG, "+3"]),
        words(&[FLAG, "3x"]),
        words(&[FLAG, "2147483648"]),
        words(&["--lab-window-guard=3"]),
        words(&[FLAG, "9", FLAG, "10"]),
        words(&[FLAG, "--", "9"]),
    ] {
        let original = args.clone();
        assert!(take(&mut args).is_err(), "{args:?}");
        assert_eq!(args, original);
    }
}

#[test]
fn functional_offer_has_exact_distinct_roles_and_an_original_leaf() {
    let mut args = words(&[
        "--window",
        MEMORY_FLAG,
        "18",
        "1234567890123-42-0.claim",
        FLAG,
        "17",
    ]);
    assert_eq!(
        take(&mut args),
        Ok(Some(GuardBundle {
            lane: 17,
            memory: Some(MemoryRef {
                descriptor: 18,
                claim: "1234567890123-42-0.claim".into()
            })
        }))
    );
    assert_eq!(args, words(&["--window"]));
    for mut args in [
        words(&[MEMORY_FLAG, "18", "123-42-0.claim"]),
        words(&[FLAG, "17", MEMORY_FLAG, "17", "123-42-0.claim"]),
        words(&[FLAG, "17", MEMORY_FLAG, "18"]),
        words(&[FLAG, "17", MEMORY_FLAG, "0", "123-42-0.claim"]),
        words(&[FLAG, "17", MEMORY_FLAG, "-18", "123-42-0.claim"]),
        words(&[FLAG, "17", MEMORY_FLAG, "18", "../x.claim"]),
        words(&[FLAG, "17", MEMORY_FLAG, "18", "/x.claim"]),
        words(&[FLAG, "17", MEMORY_FLAG, "18", ".claim"]),
        words(&[FLAG, "17", MEMORY_FLAG, "18", "x.wait"]),
        words(&[
            FLAG,
            "17",
            MEMORY_FLAG,
            "18",
            "x.claim",
            MEMORY_FLAG,
            "19",
            "x.claim",
        ]),
    ] {
        let original = args.clone();
        assert!(take(&mut args).is_err(), "{args:?}");
        assert_eq!(args, original);
    }
    let mut payload = words(&[
        FLAG,
        "17",
        MEMORY_FLAG,
        "18",
        "123-42-0.claim",
        "--",
        "program",
        MEMORY_FLAG,
        "0",
        "../bad",
    ]);
    assert!(take(&mut payload).unwrap().unwrap().memory.is_some());
    assert_eq!(
        payload,
        words(&["--", "program", MEMORY_FLAG, "0", "../bad"])
    );
    let mut oversized = words(&[FLAG, "17", MEMORY_FLAG, "18"]);
    oversized.push(format!("{}.claim", "a".repeat(96)).into());
    let original = oversized.clone();
    assert!(take(&mut oversized).is_err());
    assert_eq!(oversized, original);
}
