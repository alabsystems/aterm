// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// Tests promoting, scrubbing, and replaying recorded supervise loop tapes.

use crate::supervise::tape::acts::Candidate;
use crate::supervise::tape::codec::Item;
use crate::supervise::tape::corpus::{self, Scrub, token};
use crate::supervise::tape::recover::recover;
use crate::supervise::tape::report::Verdict;

const OWNER: &str = "/Users/_owner";

fn bash_box(command: &str) -> Vec<String> {
    bash_one_row()
        .into_iter()
        .map(|row| {
            if row.trim() == "git log --oneline -5" {
                format!("   {command}")
            } else {
                row
            }
        })
        .collect()
}

fn script() -> Mock {
    let mut m = fenced(vec![
        busy_screen(),
        read_box(),
        busy_screen(),
        bash_box(&format!("cat {OWNER}/notes.txt")),
        busy_screen(),
        idle_screen(),
    ]);
    m.vanish_after = Some(3);
    m
}

fn recorded() -> Vec<u8> {
    let mut m = script();
    let mut opts = auto(30);
    opts.policy.approve = Approve::All;
    let run = record_watch(&mut m, &opts, |s| {
        s.set_approval_env(ApprovalEnv {
            home: Some(PathBuf::from(OWNER)),
            ..ApprovalEnv::of_process()
        });
    });
    run.tape.reframed(&run.header.encode())
}

fn recorded_edited(edit: impl FnOnce(&mut Vec<Item>)) -> Vec<u8> {
    let read = recover(&recorded());
    let mut items = read.items;
    edit(&mut items);
    crate::supervise::tape::testing::written(&read.header.expect("a header"), &items)
}

fn owner_scrub() -> Scrub {
    let mut s = Scrub::new();
    s.path(std::path::Path::new(OWNER), token::HOME)
        .expect("a home");
    s
}

fn promoted(bytes: &[u8], scrub: &Scrub) -> Result<corpus::Promoted, String> {
    corpus::promote(bytes, scrub, Some(std::path::Path::new(OWNER)))
}

#[test]
fn a_recorded_run_is_promoted_scrubbed_and_checks() {
    let bytes = recorded();
    let before = crate::supervise::tape::text::render(
        recover(&bytes).header.as_deref(),
        &recover(&bytes).items,
    );
    assert!(before.contains(OWNER), "the run named the owner's home");
    let kept = promoted(&bytes, &owner_scrub()).unwrap_or_else(|e| panic!("promoted: {e}"));
    assert!(!kept.text.contains("/Users/"), "{}", kept.text);
    assert!(
        kept.text.contains("home=/lab/home") && kept.text.contains("cat /lab/home/notes.txt"),
        "the header's home and the screen's path are the token"
    );
    assert_eq!(kept.census.approvals, 2, "{}", kept.census);
    assert!(kept.census.acts >= 2, "{}", kept.census);
    let checked =
        corpus::check(kept.text.as_bytes(), &Candidate::default()).expect("a corpus check");
    assert_eq!(checked.verdict, Verdict::Reproduced, "{}", checked.render());
    let account = &checked.acts.as_ref().expect("an acts report").account;
    assert_eq!(account.taken, account.acts);
    assert!(account.exact(), "{account:?}");

    let dir = crate::supervise::ScratchDir::new("drive-replay", "corpus-check");
    std::fs::create_dir_all(dir.join("nested")).expect("a nested directory");
    std::fs::write(dir.join("nested").join("run.atape.txt"), &kept.text).expect("written");
    std::fs::write(dir.join("README.md"), "not a tape").expect("written");
    let root = dir.join(".").to_string_lossy().into_owned();
    let args = |line: &[&str]| line.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    let ok = crate::drive_cli::replay_reply(&args(&["--check", &root])).expect("a check");
    assert_eq!(ok.code, 0, "{}", ok.text);
    assert!(ok.text.starts_with("REPRODUCED "), "{}", ok.text);
    let safe = crate::drive_cli::replay_reply(&args(&["--check", &root, "--set", "approve=safe"]))
        .expect("a check");
    assert_eq!(safe.code, 1, "{}", safe.text);
    assert!(safe.text.starts_with("DIVERGED "), "{}", safe.text);
}

#[test]
fn promote_refuses_a_planted_unscrubbed_path() {
    let bytes = recorded();
    let e = promoted(&bytes, &Scrub::new()).expect_err("the owner's home is on it");
    assert!(e.contains("\"/Users/\" on line"), "{e}");
    let planted = recorded_edited(|items| {
        let at = items.len() - 1;
        items.insert(
            at,
            Item::Note {
                kind: "planted".to_string(),
                text: "left by /Users//someone-else/work".to_string(),
            },
        );
    });
    let e = promoted(&planted, &owner_scrub()).expect_err("a planted path");
    assert!(e.contains("\"/Users/\" on line"), "{e}");
    assert!(
        promoted(&bytes, &owner_scrub()).is_ok(),
        "the same tape without the plant is promoted"
    );
}

#[test]
fn promote_refuses_a_scrub_that_changes_what_the_loop_does() {
    let mut scrub = owner_scrub();
    scrub.word("Yes", token::USER).expect("a word");
    let e = promoted(&recorded(), &scrub).expect_err("no option to press");
    assert!(
        e.contains("the scrubbed tape does not replay as the original did"),
        "{e}"
    );
}

#[test]
fn promote_refuses_a_tape_with_no_end_or_one_that_does_not_replay() {
    let endless = recorded_edited(|items| {
        items.retain(|i| !matches!(i, Item::End(_)));
    });
    let e = promoted(&endless, &owner_scrub()).expect_err("no end");
    assert!(e.contains("the tape has no end"), "{e}");
    let pressed_otherwise = recorded_edited(|items| {
        use crate::supervise::tape::{codec, doors};
        use astream_effects::EffectRecord;
        let key = items
            .iter_mut()
            .find_map(|i| match i {
                Item::Effect {
                    record: EffectRecord::Exchange { door, request, .. },
                    ..
                } if door == doors::CTL
                    && codec::ctl_args(request).is_ok_and(|w| {
                        w.iter()
                            .find(|w| !w.starts_with('@'))
                            .is_some_and(|w| w == "key")
                            && w.last().is_some_and(|w| w == "1")
                    }) =>
                {
                    Some(request)
                }
                _ => None,
            })
            .expect("a key pressed");
        let words: Vec<String> = codec::ctl_args(key)
            .expect("words")
            .into_iter()
            .map(|w| if w == "1" { "9".to_string() } else { w })
            .collect();
        let words: Vec<&str> = words.iter().map(String::as_str).collect();
        *key = codec::ctl_request(&words);
    });
    let e = promoted(&pressed_otherwise, &owner_scrub()).expect_err("an act changed");
    assert!(
        e.contains("the tape does not replay as recorded on this build"),
        "{e}"
    );
}
