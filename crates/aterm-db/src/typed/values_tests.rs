// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The tracker's value types by their grammars, apart from the corpus that holds them to the
//! publication's data (`typed::tests::the_publication_384b3a9f_…`).

use super::*;
use aterm_json::json;

/// A commit is lower-case hex, abbreviated to 7 or more digits or whole (40, or 64); a digest is
/// `sha256:` and 64 of them.
/// NEGATIVE CONTROL: with `commit` taking any length, `` "abc123" is no commit `` fails.
#[test]
fn a_commit_is_seven_to_forty_lower_case_hex_and_a_digest_is_sha256_and_sixty_four() {
    let full = "0a386e53d70f1c2b3a4d5e6f708192a3b4c5d6e7";
    for good in ["384b3a9f", "0a386e5", full, &"ab".repeat(32)] {
        assert_eq!(commit(good), Ok(()), "{good:?} is a commit");
    }
    for bad in [
        "",
        "abc123",
        "384B3A9F",
        "384b3a9g",
        &format!("{full}0"),
        " 384b3a9f",
    ] {
        assert!(commit(bad).is_err(), "{bad:?} is no commit");
    }
    let hex = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    assert_eq!(digest(&format!("sha256:{hex}")), Ok(()));
    for bad in [
        hex.to_owned(),
        format!("sha256:{}", &hex[1..]),
        format!("sha256:{}", hex.to_uppercase()),
        format!("sha1:{hex}"),
    ] {
        assert!(digest(&bad).is_err(), "{bad:?} is no digest");
    }
}

/// A path is relative to its repository, `/`-separated with no empty, `.` or `..` part, a
/// trailing `/` for a directory and a `*` for any run of a name; long paths and test names stand.
/// NEGATIVE CONTROL: with `path` skipping its `..` check, `` "../secrets" is no path `` fails.
#[test]
fn a_path_is_relative_and_never_climbs_out_of_its_repository() {
    for good in [
        "docs/",
        "CLAUDE.md",
        "crates/aterm-db/src/typed/keys_slug_tests.rs",
        "crates/trust-js-*",
        "first-party/trust-ir",
        "crates/aterm-db/src/typed/a_very_long_test_module_name_that_runs_past_forty.rs",
    ] {
        assert_eq!(path(good), Ok(()), "{good:?} is a path");
    }
    for bad in [
        "",
        "/etc/passwd",
        "~/publication/goals",
        "../secrets",
        "docs/../x",
        "docs//x",
        "./docs",
        "publish/ comments",
        "a\\b",
        "a\tb",
    ] {
        assert!(path(bad).is_err(), "{bad:?} is no path");
    }
    assert!(
        path(&"a/".repeat(600)).is_err(),
        "a path is at most 1024 bytes"
    );
}

/// A decimal is written exactly: an optional `-`, whole digits with no leading zero and an
/// optional fraction, its trailing zeros kept; a zero has no sign.
/// NEGATIVE CONTROL: with `decimal` taking a leading zero, `` "007" is no decimal `` fails.
#[test]
fn a_decimal_is_exact_text_with_no_leading_zero_and_no_signed_zero() {
    for good in ["0", "0.68", "1322.1367705", "-3", "-0.5", "10.50", "117"] {
        assert_eq!(decimal(good), Ok(()), "{good:?} is a decimal");
    }
    for bad in [
        "", "007", ".5", "5.", "1e3", "+1", "-0", "-0.00", "1,121", "0.68.0", "12 ms", "NaN",
    ] {
        assert!(decimal(bad).is_err(), "{bad:?} is no decimal");
    }
}

/// A link to a row of another table family (design §4.6) is its own type: its schema word names
/// the family, the collection and how the row is named, and its value is the row's key or, for
/// a machine, the 32-digit Commons machine uuid, never the label a person calls the machine.
/// NEGATIVE CONTROL, observed: with `link` taking `keys::is_label` for a machine too, `` a
/// label is no link to a machine `` fails.
#[test]
fn a_link_names_another_familys_row_and_a_machine_by_its_uuid() {
    use crate::alab::registry::MACHINE_LINK;
    use crate::typed::{Link, Named, Ty, cell};
    assert_eq!(
        Ty::Link(MACHINE_LINK).word(),
        json!({"link": {
            "application": "alab",
            "role": "registry",
            "collection": "machine",
            "named": "machine-uuid",
        }})
    );
    let machine = Ty::Link(MACHINE_LINK);
    assert_eq!(
        cell::rule(&machine, &json!("0123456789abcdef0123456789abcdef"), false),
        Ok(())
    );
    for (bad, why) in [
        (json!("m72"), "a label"),
        (json!("mc-0011223344556677"), "the row's key"),
        (json!("0123456789ABCDEF0123456789ABCDEF"), "upper-case hex"),
        (json!("0123456789abcdef"), "half a uuid"),
        (json!(7), "a number"),
    ] {
        assert!(
            cell::rule(&machine, &bad, false).is_err(),
            "{why} is no link to a machine"
        );
    }
    let by_key = Ty::Link(Link {
        named: Named::Key,
        ..MACHINE_LINK
    });
    assert_eq!(by_key.word()["link"]["named"], json!("key"));
    assert_eq!(
        cell::rule(&by_key, &json!("mc-0011223344556677"), false),
        Ok(())
    );
    for bad in ["a.b", "two words", ""] {
        assert!(cell::rule(&by_key, &json!(bad), false).is_err(), "{bad:?}");
    }
    assert_eq!(cell::rule(&machine, &Value::Null, true), Ok(()));
}

/// A principal names its session, its program or what an imported record said; its machine is
/// its machine uuid, never a label (design §4.6), its account a key, and no member it does not
/// name rides along (the name `mac`, which the member had before any record held one, is such a
/// member). Its value reads back as itself.
/// NEGATIVE CONTROL: with `principal` not asking for a session, program or `said`, `` only a
/// model is no principal `` fails.
#[test]
fn a_principal_names_who_acted_and_reads_back_as_itself() {
    let agent = Principal {
        session: Some("s-47051b9a207955a92b14".into()),
        program: Some("claude".into()),
        machine: Some("0123456789abcdef0123456789abcdef".into()),
        account: Some("ac-0011223344556677".into()),
        model: Some("claude-opus-5-5".into()),
        harness: Some("land".into()),
        said: None,
    };
    let value = agent.to_value();
    assert_eq!(principal(&value), Ok(()));
    assert_eq!(Principal::from_value(&value), Ok(agent));
    let imported = Principal::said("s595dac on m18 (disk-custody design session)");
    assert_eq!(principal(&imported.to_value()), Ok(()));
    assert_eq!(
        principal(&json!({"session": "15", "machine": "0123456789abcdef0123456789abcdef"})),
        Ok(())
    );
    for (bad, why) in [
        (json!({"model": "claude-opus-5-5"}), "only a model"),
        (json!({}), "nothing"),
        (
            json!({"session": "15", "machine": "m72 desk"}),
            "a machine in words",
        ),
        (
            json!({"session": "15", "machine": "m72"}),
            "a machine's label",
        ),
        (
            json!({"session": "15", "mac": "m72"}),
            "the member's name before it was machine",
        ),
        (json!({"session": "15", "email": "a"}), "an unnamed member"),
        (json!({"session": "s 15"}), "a session with a space"),
        (json!({"said": " "}), "a blank said"),
        (json!({"program": 7}), "a number"),
        (json!("15@m72"), "a bare string"),
    ] {
        assert!(principal(&bad).is_err(), "{why} is no principal");
    }
}

/// Evidence is a commit, a run, a measurement, a path, a URL or a note, each with exactly its
/// members in their types; what a command line types reads back as the same text.
/// NEGATIVE CONTROL: with `evidence` letting an unnamed member stand, `` a commit with a note
/// is refused `` fails.
#[test]
fn evidence_is_typed_and_its_text_round_trips() {
    for text in [
        "commit:aterm@0a386e53d70",
        "measure:lab_objects=5",
        "path:aterm:docs/DESIGN-lab-tracker-2026-11-14.md",
        "note:landed as 1083bb91b7, pre-push green; reviewed by s-ed8f5c@m60",
        "https://github.example.invalid/runs/7?attempt=2#step:3",
    ] {
        let piece = Evidence::parse(text).unwrap_or_else(|why| panic!("{text:?}: {why}"));
        assert_eq!(piece.spell(), text);
        assert_eq!(Evidence::from_value(&piece.to_value()), Ok(piece));
    }
    let spaced = Evidence::parse("note:  landed ").unwrap();
    assert_eq!(spaced, Evidence::note("landed"));
    assert_eq!(Evidence::parse(&spaced.spell()), Ok(spaced));
    let run = Evidence::Run {
        command: "targo --unverified test -p aterm-db --lib typed::tests".into(),
        exit: 0,
        output: Some("test result: ok. 1 passed; 0 failed".into()),
        repo: Some("aterm".into()),
        commit: Some("0a386e53d70".into()),
    };
    assert_eq!(evidence(&run.to_value()), Ok(()));
    assert_eq!(Evidence::from_value(&run.to_value()), Ok(run));
    for text in [
        "commit:aterm",
        "commit:aterm@HEAD",
        "measure:x=1,121",
        "path:aterm:/etc/hosts",
        "note:",
        "http://plain.example.invalid",
        "https://0123456789abcdef0123456789abcdef01234567@github.example.invalid/o/r.git",
        "https://ann@host.example.invalid/x",
        "landed",
    ] {
        assert!(Evidence::parse(text).is_err(), "{text:?} is no evidence");
    }
    for (bad, why) in [
        (
            json!({"kind": "commit", "repo": "aterm", "commit": "0a386e5", "text": "x"}),
            "a commit with a note",
        ),
        (
            json!({"kind": "run", "command": "true"}),
            "a run with no exit",
        ),
        (
            json!({"kind": "run", "command": "true", "exit": 256}),
            "a run past 255",
        ),
        (json!({"kind": "rumour", "text": "x"}), "an unknown kind"),
        (
            json!({"kind": "note", "text": " landed"}),
            "a note with space around it",
        ),
        (json!({"text": "x"}), "no kind"),
    ] {
        assert!(evidence(&bad).is_err(), "{why} is refused");
    }
}
