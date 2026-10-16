// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn temp_layout(label: &str) -> crate::testdir::ScratchLayout {
    crate::testdir::layout(&format!("atpkg-held-{label}-"))
}

/// The row a real hold writes (full 64-hex `tree_root` digests and the real retry text), so
/// the pending stub is tested at the length it prints.
fn real_held_row() -> String {
    crate::state::held_failed(
        "tree_root mismatch: expected \
         4f28d8d2926c281a46b2a5658cd455801f9f35c92de9a69d6f1e0d8000393478, got \
         d94ed3b079308667f51ac04350908bcc8e0f7c3a38d7cdc384f0c66bd8340075",
        "not downloaded again until a new build is published \u{2014} now: aterm pkg \
         install claude",
    )
}

/// A held row (`held: last attempt failed with …; <retry>`) is quoted by `which` as the
/// reason nothing runs, and said by the pending stub without the `error` arm's "the next
/// pass installs it" promise and without a bump, which could only repeat the hold.
#[test]
fn a_held_row_is_quoted_by_which_and_said_by_the_pending_stub_without_a_promise() {
    let layout = temp_layout("row");
    let row = real_held_row();
    record_status(
        &layout,
        "claude",
        crate::ProgramStatus {
            installed_build: None,
            state: row.clone(),
            tree_root: String::new(),
        },
        "bootstrap install claude: held".into(),
    );
    // `which`: a recorded row with no shim is quoted.
    let line = which_line(&layout, "claude", None).unwrap();
    assert_eq!(line, format!("claude → (nothing runs) — {row}"));
    assert!(!line.contains("next pass"), "{line}");

    // The pending stub: the row, verbatim, and nothing promised.
    let mut said: Vec<String> = Vec::new();
    let mut sink = |l: String| said.push(l);
    let mut io = PendingIo {
        say: &mut sink,
        wait: None,
        asker: crate::progress::Asker::Script,
    };
    assert_eq!(pending_state(&layout, "claude", &mut io), PendingNext::Done);
    let all = said.join("\n");
    // The last line is the hold (the program's introduction comes first).
    assert_eq!(
        said.last().map(String::as_str),
        Some(format!("atpkg: claude is {row} — Settings ▸ Packages shows details.").as_str()),
        "a real row fits both caps: the failure, digests whole, and the retry whole"
    );
    assert!(
        all.contains("now: aterm pkg install claude — Settings"),
        "the retry survives: {all}"
    );
    assert!(
        !all.contains("FAILED") && !all.contains("next pass") && !all.contains("retries it"),
        "no promise a pass cannot keep: {all}"
    );
    assert!(
        !layout.prefix.join("bump").exists(),
        "no bump: a pass can only repeat the hold"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// The stub caps the failure and never the retry: a failure longer than its cap is
/// elided, and the retry after it still prints whole.
#[test]
fn the_held_stub_line_caps_the_failure_and_prints_the_retry_whole() {
    let how = "not downloaded again until a new build is published \u{2014} now: aterm pkg \
               install claude";
    let long_why = "signer refused: ".to_string() + &"x".repeat(3 * HELD_WHY_CAP);
    let row = crate::state::held_failed(&long_why, how);
    let held = row.strip_prefix(crate::state::HELD_FAILED_PREFIX).unwrap();
    let line = held_stub_line("claude", held);
    let why_shown: String = long_why.chars().take(HELD_WHY_CAP).collect();
    assert_eq!(
        line,
        format!(
            "atpkg: claude is held: last attempt failed with {why_shown}…; {how} — \
             Settings ▸ Packages shows details."
        )
    );
    // A row with no seam at all (not one atpkg writes): the failure half, capped.
    let bare = held_stub_line("claude", &"y".repeat(3 * HELD_WHY_CAP));
    assert!(
        bare.ends_with("y… — Settings ▸ Packages shows details."),
        "{bare}"
    );
}

/// A member the memo held is the bootstrap lane's skip, not its failure: the canonical row
/// lands in the record (the pass outcome naming it), with no `error:` head and a
/// hard-failure count of 0, so the pass exits clean and the window keeps its normal
/// interval instead of the failure backoff. `doctor` still counts the row as a problem.
#[test]
fn a_held_member_is_the_bootstrap_lanes_skip_not_its_failure() {
    let layout = temp_layout("skip");
    let row = crate::state::held_failed(
        "tree_root mismatch: expected 15c4, got 3a44",
        "now: aterm pkg install claude",
    );
    assert_eq!(skip_held_member(&layout, "claude", row.clone()), 0);
    let status = crate::status::read(&layout).expect("the row was recorded");
    let recorded = status.programs.get("claude").expect("claude's row");
    assert_eq!(recorded.state, row, "the canonical row, verbatim");
    assert_eq!(recorded.installed_build, None, "nothing was installed");
    assert!(
        !recorded.state.starts_with("error"),
        "no `error:` head on a hold: {}",
        recorded.state
    );
    assert!(
        status
            .outcome
            .contains("bootstrap install claude: held: last attempt failed with"),
        "the pass outcome names the hold: {}",
        status.outcome
    );
    assert!(
        crate::doctor::recorded_problems(Some(&status))
            .iter()
            .any(|p| p.starts_with("claude: held: last attempt failed with")),
        "doctor still counts the held member as a problem"
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// `update --retry` forgets the digest-refusal memos — the named program's, or every
/// program's — and says how many; a bare `--retry` with none recorded says so too.
#[test]
fn update_retry_forgets_the_memos_and_says_how_many() {
    let layout = temp_layout("retry");
    for (program, build) in [("claude", 2026092201u64), ("codex", 2026092201)] {
        crate::store::record_held_refusal(
            &layout.build_dir(program, build),
            (crate::manifest::DigestAlg::Sha256, "aa"),
            "bb",
            "tree_root mismatch: expected bb, got cc",
            1_000,
        )
        .unwrap();
    }
    assert_eq!(crate::store::clear_stage_refusals(&layout, "claude"), 1);
    assert_eq!(crate::store::clear_stage_refusals(&layout, "claude"), 0);
    assert_eq!(crate::store::clear_all_stage_refusals(&layout), 1);
    assert_eq!(crate::store::clear_all_stage_refusals(&layout), 0);
    // …and the vendor lane's own memo in a vendor-direct program's stamp.
    crate::vendor_direct::ProgramStamp::record_refusal(
        &layout,
        "claude",
        "signer refused: bin/claude is not signed",
        crate::vendor_direct::Version::parse("2.1.280").unwrap(),
        &crate::vendor_direct::digests::AuthenticatedDigest::authenticated(
            crate::manifest::DigestAlg::Sha256,
            &"a".repeat(64),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(
        forget_vendor_refusal(&layout, "claude"),
        "a recorded one goes"
    );
    assert!(
        !forget_vendor_refusal(&layout, "claude"),
        "and says so once"
    );
    assert!(
        !forget_vendor_refusal(&layout, "ay"),
        "an index program keeps no stamp"
    );
    assert_eq!(
        retry_line(0),
        "atpkg: no download was being held back \u{2014} updating"
    );
    assert_eq!(retry_line(1), "atpkg: trying 1 held-back download again");
    assert_eq!(retry_line(2), "atpkg: trying 2 held-back downloads again");
    // The flag is admitted at the dispatch edge and named by the usage line.
    let (_, allowed) = NAME_TAKING_VERBS
        .iter()
        .find(|(v, _)| *v == "update")
        .unwrap();
    assert!(allowed.contains(&"--retry"));
    assert!(verb_usage("update").unwrap().contains("--retry"));
    // The one-operand gate counts the flag out, in either position, and still refuses a
    // second name at the edge, before the store lock.
    let argv = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    assert!(at_most_one_operand(&argv(&["claude", "--retry"]), "update").is_none());
    assert!(at_most_one_operand(&argv(&["--retry", "claude"]), "update").is_none());
    assert!(at_most_one_operand(&argv(&["--retry"]), "update").is_none());
    assert_eq!(
        at_most_one_operand(&argv(&["ta", "tb", "--retry"]), "update"),
        Some(ExitCode::from(2))
    );
    let _ = std::fs::remove_dir_all(&layout.prefix);
}

/// `update --retry <operand>` admits the name before it forgets anything: an operand that
/// is not one directory name is refused (exit 2) with nothing forgotten or run, and the
/// store's clear refuses it too, so a memo outside `store/` (`store/../outside`, or a
/// drive-relative `C:x` on Windows) survives every door that clears by name.
#[test]
fn update_retry_refuses_a_path_before_forgetting_anything() {
    let layout = temp_layout("retry-path");
    // `store/` exists, so `store/../outside` resolves on Unix too: without the gate the
    // clear below would reach the memo.
    std::fs::create_dir_all(layout.prefix.join("store")).unwrap();
    let outside = layout.prefix.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let memo = outside.join("2026092201.refused");
    std::fs::write(&memo, b"not the store's\n").unwrap();
    for traversal in ["../outside", "..\\outside", "..", "", "."] {
        assert_eq!(
            refuse_retry_operand(traversal),
            Some(ExitCode::from(2)),
            "{traversal:?} is not a program name"
        );
        assert_eq!(
            crate::store::clear_stage_refusals(&layout, traversal),
            0,
            "{traversal:?}"
        );
    }
    assert!(memo.is_file(), "the memo outside the store survives");
    // A drive-relative operand has no separator, yet on Windows `store.join("C:")` is `C:`
    // (drive C's current directory), so it is refused there; on Unix it is a name inside
    // the store.
    let store = layout.prefix.join("store");
    for drive in ["C:", "C:x", "C:.."] {
        let escapes = !store.join(drive).starts_with(&store);
        assert_eq!(
            escapes,
            cfg!(windows),
            "{drive:?}: the join leaves the store"
        );
        assert_eq!(
            refuse_retry_operand(drive).is_some(),
            escapes,
            "{drive:?} is a program name only where it stays in the store"
        );
        if escapes {
            assert_eq!(
                crate::store::clear_stage_refusals(&layout, drive),
                0,
                "{drive:?}"
            );
        }
    }
    for name in ["claude", "trust-cg", "alab-ty"] {
        assert_eq!(refuse_retry_operand(name), None, "{name} is a program name");
    }
    let _ = std::fs::remove_dir_all(&layout.prefix);
}
