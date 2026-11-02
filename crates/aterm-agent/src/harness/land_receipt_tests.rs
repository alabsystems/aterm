// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::repo_work::{CleanAt, RepoWork};

/// THE FOUR LEGS, EACH FROM A REAL TEMPORARY REPOSITORY (§14.9, slice 9b): a repository pushed
/// to a bare origin is on `origin/main`; one pushed only to a branch has no origin main to check
/// against; one with no remote is committed alone; a plain folder names no repository. Each
/// repository's receipt names one 12-hex head, `rev-parse HEAD`'s first twelve digits, and its
/// journal line reads back whole. NEGATIVE CONTROL: the plain folder's reading names nothing, and
/// a finding or an unread repository has no receipt at all.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn the_four_legs_each_from_a_real_temporary_repository() {
    use crate::harness::repo_work::tests::{repo, scratch_git};
    use crate::harness::repo_work::{PROBE_LIMIT, probe};

    const DEFENSE: &str = "Merged the parser and pushed main.\nNothing left: the parser is committed \
                           and pushed to origin/main.\n- its tests ran green";
    const SAID: &str =
        "Nothing left: the parser is committed and pushed to origin/main. - its tests ran green";

    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let head_of =
        |work: &std::path::Path| scratch_git(work, &["rev-parse", "HEAD"]).trim().to_string();
    let receipt_of = |work: &std::path::Path| {
        let read = probe(work, Some(work), PROBE_LIMIT);
        LandReceipt::of(&read, Some(DEFENSE), 1_771_000_000_000)
            .unwrap_or_else(|| panic!("a clean reading has a receipt: {read:?}"))
    };
    let on_main = repo(&tmp.path().join("on-main"), true);
    let pushed = repo(&tmp.path().join("pushed"), false);
    let bare = tmp.path().join("pushed").join("remote.git");
    std::fs::create_dir_all(&bare).expect("bare");
    scratch_git(&bare, &["init", "-q", "--bare", "."]);
    scratch_git(
        &pushed,
        &["remote", "add", "origin", &bare.display().to_string()],
    );
    scratch_git(&pushed, &["push", "-q", "-u", "origin", "HEAD:wip"]);
    let alone = repo(&tmp.path().join("alone"), false);
    let plain = tmp.path().join("plain");
    std::fs::create_dir_all(&plain).expect("plain");

    for (name, work, leg, on) in [
        ("on-main", &on_main, ReceiptLeg::OnMain, "origin/main"),
        ("pushed", &pushed, ReceiptLeg::Pushed, ""),
        ("alone", &alone, ReceiptLeg::NoRemote, ""),
    ] {
        let r = receipt_of(work);
        let head = head_of(work);
        assert_eq!(
            (r.leg, r.head.as_str(), r.main_ref.as_str()),
            (leg, &head[..HEAD_DIGITS], on),
            "{}: {r:?}",
            leg.word()
        );
        assert_eq!(r.head.len(), 12, "{r:?}");
        // The probe's form of the repository's path: its end, at most 72 characters.
        assert!(r.repo.ends_with(&format!("/{name}/work")), "{r:?}");
        assert!(r.repo.chars().count() <= 72, "{r:?}");
        assert_eq!(r.said, SAID, "{r:?}");
        let line = r.line(104);
        assert!(
            line.starts_with(&format!(
                "RECEIPT seq=104 rule=land@v1 head={} on={} leg={} check=- repo=",
                &head[..12],
                if on.is_empty() { "-" } else { on },
                leg.word()
            )),
            "{line}"
        );
        assert_eq!(line.matches(&head[..12]).count(), 1, "one head: {line}");
        assert!(!line.contains(&head), "never the whole head: {line}");
        assert_eq!(
            LandReceipt::of_line(&line, r.at_ms),
            Some((104, r)),
            "{line}"
        );
    }

    let unicode_ref = format!("origin/{}", "日本語".repeat(5));
    let full_ref = format!("refs/remotes/{unicode_ref}");
    scratch_git(&on_main, &["update-ref", &full_ref, &head_of(&on_main)]);
    scratch_git(
        &on_main,
        &["symbolic-ref", "refs/remotes/origin/HEAD", &full_ref],
    );
    let unicode_receipt = receipt_of(&on_main);
    assert_eq!(unicode_receipt.main_ref, unicode_ref);
    let word = unicode_receipt.checked_word();
    assert_eq!(
        word.len(),
        155,
        "a valid default ref exceeds the former byte cap"
    );
    assert!(valid_checked_word(&word));

    let r = receipt_of(&plain);
    assert_eq!(
        (r.leg, r.head.as_str(), r.main_ref.as_str(), r.repo.as_str()),
        (ReceiptLeg::NoRepo, "", "", ""),
        "{r:?}"
    );
    let line = r.line(7);
    assert_eq!(
        line,
        format!("RECEIPT seq=7 rule=land@v1 head=- on=- leg=no-repo check=- repo=- {SAID}")
    );
    assert_eq!(LandReceipt::of_line(&line, r.at_ms), Some((7, r)));

    std::fs::write(on_main.join("wip.txt"), "work\n").expect("wip");
    let found = probe(&on_main, Some(&on_main), PROBE_LIMIT);
    assert!(found.found().is_some(), "{found:?}");
    assert_eq!(LandReceipt::of(&found, Some(DEFENSE), 0), None);
    assert_eq!(LandReceipt::of(&RepoRead::Unread, Some(DEFENSE), 0), None);
}

/// A CHECKED READING'S LEG, whatever the probe ran on: `remote` false is no remote, a remote with
/// no main ref is pushed, both is on main; the head is cut to twelve digits.
#[test]
fn a_checked_readings_leg_is_its_remote_and_main_ref() {
    let at = |remote: bool, main_ref: &str| {
        RepoRead::Checked(CleanAt {
            repo: "/src/proj".into(),
            head: "3f9a1c2e4b7d0123456789abcdef0123456789ab".into(),
            main_ref: main_ref.into(),
            remote,
            branch: "main".into(),
        })
    };
    for (read, leg, on) in [
        (at(true, "origin/main"), ReceiptLeg::OnMain, "origin/main"),
        (at(true, ""), ReceiptLeg::Pushed, ""),
        (at(false, ""), ReceiptLeg::NoRemote, ""),
    ] {
        let r = LandReceipt::of(&read, None, 5).expect("checked");
        assert_eq!(
            (r.leg, r.head.as_str(), r.main_ref.as_str(), r.repo.as_str()),
            (leg, "3f9a1c2e4b7d", on, "/src/proj")
        );
        let branch = if leg == ReceiptLeg::OnMain {
            "main"
        } else {
            ""
        };
        assert_eq!(r.branch, branch, "a branch on main alone: {r:?}");
        assert_eq!((r.said.as_str(), r.at_ms), ("", 5));
    }
    let found = RepoRead::Found(RepoWork::default());
    assert_eq!(LandReceipt::of(&found, None, 5), None);
}

/// THE AGENT'S WORDS ON A RECEIPT: its defense line and every line after it, one line, sanitized
/// when stored (hidden characters dropped, a control character a space), title-safe (no fact
/// separator and no spaced dash, which the title splits on), capped at 240 bytes with `…`. A
/// defense inside a code fence is no defense, and a reply without one says nothing.
#[test]
fn a_receipts_words_are_the_defense_and_after_one_line_title_safe_and_capped() {
    let tail = "I pushed it.\n\nNothing left: the parser \u{b7} its tests \u{2014} docs.\n\
                \u{202e}- the CI run\u{7} is green\n\n  ";
    assert_eq!(
        said_of(tail),
        "Nothing left: the parser, its tests, docs. - the CI run is green"
    );
    let fenced = "```\nNothing left: inside a fence\n```\nDone with it.";
    assert_eq!(said_of(fenced), "");
    assert_eq!(said_of("All done, nothing else."), "");
    let long = format!("Nothing left: {}", "pushed é ".repeat(60));
    let said = said_of(&long);
    assert!(said.len() <= SAID_BYTES, "{} bytes", said.len());
    assert!(said.len() > SAID_BYTES - 12, "{} bytes", said.len());
    assert!(
        said.ends_with('…') && said.starts_with("Nothing left: pushed é"),
        "{said}"
    );
    for words in [said_of(tail), said] {
        assert!(
            !words.contains(crate::harness::acts::FACT_SEPARATOR),
            "{words}"
        );
        assert!(!words.contains(" \u{2014} "), "{words}");
        assert!(
            !words
                .chars()
                .any(|c| c.is_control() || aterm_messages::text::is_hidden_char(c)),
            "{words:?}"
        );
    }
}

/// A RECEIPT LINE READS BACK AS WRITTEN, and a line this build did not write reads as none: another
/// rule, a head not twelve lowercase digits, a no-repo leg naming a head, an unknown leg, words a
/// receipt would not keep.
#[test]
fn a_receipt_line_reads_back_and_no_other_line_does() {
    let r = LandReceipt {
        head: "3f9a1c2e4b7d".into(),
        main_ref: "origin/main".into(),
        repo: "/Users//a b/src/proj%1".into(),
        branch: String::new(),
        leg: ReceiptLeg::OnMain,
        check: true,
        said: "Nothing left: pushed.".into(),
        at_ms: 9,
    };
    let line = r.line(41);
    assert_eq!(
        line,
        "RECEIPT seq=41 rule=land@v1 head=3f9a1c2e4b7d on=origin/main leg=on-main \
         check=land-check@v1 repo=/Users//a%20b/src/proj%251 Nothing left: pushed."
    );
    assert_eq!(LandReceipt::of_line(&line, 9), Some((41, r.clone())));
    let quiet = LandReceipt {
        said: String::new(),
        ..r.clone()
    };
    assert_eq!(
        LandReceipt::of_line(&quiet.line(3), 9),
        Some((3, quiet.clone()))
    );
    for bad in [
        line.replace("rule=land@v1", "rule=land-nudge@v1"),
        line.replace("3f9a1c2e4b7d", "3f9a1c2e4b7"),
        line.replace("3f9a1c2e4b7d", "3F9A1C2E4B7D"),
        line.replace("leg=on-main", "leg=no-repo"),
        line.replace("leg=on-main", "leg=landed"),
        line.replace("on=origin/main", "on=-"),
        line.replace("check=land-check@v1", "check=land@v1"),
        line.replace("check=land-check@v1", "check="),
        format!("{line} \u{b7} more"),
        line.replacen("RECEIPT", "DONE", 1),
        "RECEIPT seq=x rule=land@v1 head=- on=- leg=no-repo repo=-".to_string(),
    ] {
        assert_eq!(LandReceipt::of_line(&bad, 9), None, "{bad}");
    }
    for leg in ReceiptLeg::ALL {
        assert_eq!(ReceiptLeg::of_word(leg.word()), Some(leg));
    }
}

/// A RECEIPT NAMES ITS BRANCH AND A FOLDER THAT IS GONE (§14.9, slice 9c; §14.17 item 4): a
/// receipt on main keeps the branch whose stashes the probe counted, as `branch=` after `repo=`
/// (a line without one, as 9b wrote, reads back with none); a gone reading is a gone receipt
/// naming the folder, no head. Each reads back as written; a branch on any leg but on-main, and a
/// gone line naming a head or no folder, read as none. NEGATIVE CONTROL (run by hand): `of_line`
/// taking the branch token as the agent's words fails the first round trip.
#[test]
fn a_receipt_names_its_branch_and_a_folder_that_is_gone() {
    let on_main = LandReceipt {
        head: "3f9a1c2e4b7d".into(),
        main_ref: "origin/main".into(),
        repo: "/src/proj".into(),
        branch: "feat/a b".into(),
        leg: ReceiptLeg::OnMain,
        check: false,
        said: "Nothing left: pushed.".into(),
        at_ms: 9,
    };
    let line = on_main.line(7);
    assert_eq!(
        line,
        "RECEIPT seq=7 rule=land@v1 head=3f9a1c2e4b7d on=origin/main leg=on-main check=- \
         repo=/src/proj branch=feat/a%20b Nothing left: pushed."
    );
    assert_eq!(LandReceipt::of_line(&line, 9), Some((7, on_main.clone())));
    let quiet = LandReceipt {
        said: String::new(),
        ..on_main.clone()
    };
    assert_eq!(LandReceipt::of_line(&quiet.line(8), 9), Some((8, quiet)));
    let gone = LandReceipt::of(
        &RepoRead::Gone("/src/gone".into()),
        Some("Done.\nNothing left: the folder was removed."),
        4,
    )
    .expect("a gone folder lands");
    assert_eq!(
        (
            gone.leg,
            gone.head.as_str(),
            gone.main_ref.as_str(),
            gone.repo.as_str(),
            gone.branch.as_str()
        ),
        (ReceiptLeg::Gone, "", "", "/src/gone", "")
    );
    let gone_line = gone.line(5);
    assert_eq!(
        gone_line,
        "RECEIPT seq=5 rule=land@v1 head=- on=- leg=gone check=- repo=/src/gone Nothing left: the \
         folder was removed."
    );
    assert_eq!(LandReceipt::of_line(&gone_line, 4), Some((5, gone)));
    for bad in [
        line.replace("leg=on-main", "leg=pushed")
            .replace("on=origin/main", "on=-"),
        gone_line.replace("head=-", "head=3f9a1c2e4b7d"),
        gone_line.replace("repo=/src/gone", "repo=-"),
    ] {
        assert_eq!(LandReceipt::of_line(&bad, 9), None, "{bad}");
    }
}

/// THE TASK RECORD'S `checked=` WORD (§14.9, slice 9c): `<head>@<ref>` on main, the ref
/// percent-encoded; every other leg its word.
#[test]
fn the_checked_word_is_head_at_ref_or_the_legs_word() {
    let at = |leg: ReceiptLeg, main_ref: &str| LandReceipt {
        head: "3f9a1c2e4b7d".into(),
        main_ref: main_ref.into(),
        leg,
        ..LandReceipt::default()
    };
    assert_eq!(
        at(ReceiptLeg::OnMain, "origin/main").checked_word(),
        "3f9a1c2e4b7d@origin/main"
    );
    assert_eq!(
        at(ReceiptLeg::OnMain, "up stream/main").checked_word(),
        "3f9a1c2e4b7d@up%20stream/main"
    );
    for (leg, word) in [
        (ReceiptLeg::Pushed, "pushed"),
        (ReceiptLeg::NoRemote, "no-remote"),
        (ReceiptLeg::NoRepo, "no-repo"),
        (ReceiptLeg::Gone, "gone"),
    ] {
        assert_eq!(at(leg, "").checked_word(), word);
    }
}

#[test]
fn checked_words_keep_unicode_refs_within_the_shared_encoded_bound() {
    let unicode = format!("origin/{}", "日本語".repeat(5));
    let widest = "🚀".repeat(crate::harness::repo_work::PATH_CHARS);
    for reference in [&unicode, &widest] {
        let receipt = LandReceipt {
            head: "3f9a1c2e4b7d".to_string(),
            main_ref: reference.as_str().to_owned(),
            leg: ReceiptLeg::OnMain,
            ..LandReceipt::default()
        };
        let word = receipt.checked_word();
        assert!(
            word.len() > 128,
            "the former consumer bound refused this ref"
        );
        assert!(word.len() <= CHECKED_WORD_BYTES);
        assert!(valid_checked_word(&word));
        assert_eq!(
            aterm_codec::pct::decode(word.split_once('@').unwrap().1),
            reference.as_str(),
            "the receipt keeps the complete ref"
        );
    }
    assert_eq!(
        LandReceipt {
            head: "3f9a1c2e4b7d".to_string(),
            main_ref: widest,
            leg: ReceiptLeg::OnMain,
            ..LandReceipt::default()
        }
        .checked_word()
        .len(),
        CHECKED_WORD_BYTES,
        "the bound covers the maximum UTF-8 and percent expansion"
    );
    for suffix in ["", "%", "%GG", "%FF", "%00", "%6Dain", "日本語"] {
        assert!(!valid_checked_word(&format!("3f9a1c2e4b7d@{suffix}")));
    }
    assert!(!valid_checked_word(&format!(
        "3f9a1c2e4b7d@{}",
        "x".repeat(CHECKED_WORD_BYTES - HEAD_DIGITS)
    )));
}

/// THE RECEIPT SAYS THE LAND CHECK WAS ANSWERED (§14.17 item 5, slice 13a), and what the agent
/// said is worded by that one fact (`mode_words::said_words`, slice 9c): a receipt whose landing
/// stood on the check's `LANDED` journals `check=land-check@v1` and says the agent answered the
/// land check; one that answered none (`check=-`, or a line a build before the check wrote, with
/// no `check=` at all) says only what the agent said. NEGATIVE CONTROL: the unchecked receipt's
/// words are not the checked one's, and its line names no rule.
#[test]
fn the_receipt_says_whether_the_land_check_was_answered() {
    let checked = LandReceipt {
        head: "3f9a1c2e4b7d".into(),
        main_ref: "origin/main".into(),
        repo: "/src/proj".into(),
        branch: String::new(),
        leg: ReceiptLeg::OnMain,
        check: true,
        said: "Nothing left: pushed.".into(),
        at_ms: 9,
    };
    let unchecked = LandReceipt {
        check: false,
        ..checked.clone()
    };
    use crate::supervise::mode_words::{ANSWERED_CHECK, said_words};
    assert!(said_words(&checked).starts_with(ANSWERED_CHECK));
    assert!(!said_words(&unchecked).contains(ANSWERED_CHECK));
    assert_ne!(said_words(&checked), said_words(&unchecked));
    let line = checked.line(7);
    assert!(line.contains(" check=land-check@v1 "), "{line}");
    let quiet = unchecked.line(7);
    assert!(quiet.contains(" check=- "), "{quiet}");
    assert_eq!(LandReceipt::of_line(&line, 9), Some((7, checked)));
    assert_eq!(
        LandReceipt::of_line(&quiet, 9),
        Some((7, unchecked.clone()))
    );
    let older = quiet.replace(" check=-", "");
    assert!(!older.contains("check="), "{older}");
    assert_eq!(
        LandReceipt::of_line(&older, 9),
        Some((7, unchecked)),
        "a build before the check wrote no check= and answered none"
    );
}
