// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::path::{Path, PathBuf};

use astream_effects::EffectRecord;

use super::*;
use crate::supervise::CtlReply;
use crate::supervise::tape::codec::Item;

/// The lab's names on a recording machine, as a promote's scrub takes them.
/// The home is `/Users/_a`, which the public export keeps as written: it
/// doubles the separator of every `/Users/<letter>` (aterm-census's
/// `export::rewrite`), which a scrub refuses as a path with an empty name.
fn lab_scrub() -> Scrub {
    let mut s = Scrub::new();
    s.path(
        Path::new("/Users/_a/wt/target/real-claude-lab"),
        token::STATE,
    )
    .expect("state")
    .path(Path::new("/Users/_a/wt"), token::CHECKOUT)
    .expect("checkout")
    .path(Path::new("/Users/_a"), token::HOME)
    .expect("home")
    .path(Path::new("/var/folders/kb/xyz/T/"), token::TMP)
    .expect("tmp")
    .word("alice", token::USER)
    .expect("user")
    .word("alices-mbp", token::HOST)
    .expect("host");
    s
}

/// THE LONGEST NAME FIRST, A PATH TO ITS END: the lab's state inside the
/// checkout inside the home is each its own token; a path that goes on
/// past a name with more of its last name (`/Users/_ab`) is not that name's;
/// a path stands wherever it stands — after `%20` in a title, inside JSON.
#[test]
fn a_path_is_taken_whole_and_the_longest_first() {
    let s = lab_scrub();
    let cases = [
        (
            "home=/Users/_a/wt/target/real-claude-lab/runs/t/scratch/home",
            "home=/lab/state/runs/t/scratch/home",
        ),
        ("dir=/Users/_a/wt/crates", "dir=/lab/checkout/crates"),
        ("/Users/_a", "/lab/home"),
        ("/Users/_a/", "/lab/home/"),
        (
            "/Users/_ab/x and /Users/_a.b",
            "/Users/_ab/x and /Users/_a.b",
        ),
        (
            "title=sh%20/Users/_a/wt/target/real-claude-lab/runs/t/claude.sh",
            "title=sh%20/lab/state/runs/t/claude.sh",
        ),
        (
            r#"{"cwd":"/Users/_a/proj","x":"/Users/_a"}"#,
            r#"{"cwd":"/lab/home/proj","x":"/lab/home"}"#,
        ),
    ];
    for (input, want) in cases {
        assert_eq!(s.text(input), want, "{input}");
    }
}

/// macOS NAMES ITS TEMPORARY DIRECTORIES TWO WAYS: a path under `/var`,
/// `/tmp` or `/etc` is taken under `/private` too, and one under
/// `/private/var` without it; the trailing `/` a `TMPDIR` carries is no part
/// of the name.
#[test]
fn a_private_directory_is_taken_in_both_spellings() {
    let s = lab_scrub();
    assert_eq!(s.text("/var/folders/kb/xyz/T/x"), "/lab/tmp/x");
    assert_eq!(s.text("/private/var/folders/kb/xyz/T/x"), "/lab/tmp/x");
    assert_eq!(s.text("/var/folders/kb/xyz/T"), "/lab/tmp");
    let mut p = Scrub::new();
    p.path(Path::new("/private/tmp/at.Ab12"), "/lab/sock")
        .expect("a socket dir");
    assert_eq!(
        p.text("cwd=/tmp/at.Ab12/proj and /private/tmp/at.Ab12/proj"),
        "cwd=/lab/sock/proj and /lab/sock/proj"
    );
}

/// A WORD (the user, the host) is taken where no letter stands beside it —
/// after a `%20`, before a `-` or a digit — and nowhere inside another word;
/// the host's longer name goes before the user's inside it.
#[test]
fn a_word_is_taken_only_where_no_letter_stands_beside_it() {
    let s = lab_scrub();
    let cases = [
        ("user alice here", "user lab-user here"),
        ("by%20alice", "by%20lab-user"),
        ("alices-mbp.local", "lab-host.local"),
        ("alice-2", "lab-user-2"),
        ("malice and alicea", "malice and alicea"),
    ];
    for (input, want) in cases {
        assert_eq!(s.text(input), want, "{input}");
    }
}

/// A NAME IN A PRESS GUARD: the guard a press is bound to its row by writes
/// the row's `.` as `\.` and its spaces as `\x20` (`policy::guard`), so a
/// name holding either is taken in that spelling too, as the token's.
#[test]
fn a_name_is_taken_in_a_press_guards_spelling_too() {
    let mut s = Scrub::new();
    s.path(Path::new("/Users/_a.b/my lab"), token::STATE)
        .expect("a path")
        .word("host.local", token::HOST)
        .expect("a word");
    let guard = crate::supervise::policy::guard::row_guard("   cat /Users/_a.b/my lab/x.txt");
    assert_eq!(
        s.text(&guard),
        crate::supervise::policy::guard::row_guard("   cat /lab/state/x.txt")
    );
    assert_eq!(s.text(r"^on\x20host\.local\s*$"), r"^on\x20lab-host\s*$");
}

/// A NAME TOO NEAR THE ROOT, OR TOO SHORT, IS REFUSED: taken everywhere it
/// would rewrite paths and words that are no one's.
#[test]
fn a_name_that_would_take_too_much_is_refused() {
    let mut s = Scrub::new();
    for path in ["/", "/Users", "/tmp/", "relative/dir", "/a//b"] {
        assert!(s.path(Path::new(path), "/lab/x").is_err(), "{path}");
    }
    for word in ["", "al", "a b", "a/b"] {
        assert!(s.word(word, "lab-user").is_err(), "{word:?}");
    }
    assert_eq!(s.names().count(), 0, "nothing refused was taken");
}

/// A CONTROL ROUND TRIP IS READ AND WRITTEN AGAIN: its words and its reply's
/// streams are rewritten, and the bytes read back as a request and a reply
/// (its encoding counts lengths, which a rewrite in place would break); any
/// other door's body is rewritten where it stands; door names, stamps and
/// clocks are kept.
#[test]
fn a_control_round_trip_is_rewritten_through_its_codec() {
    let s = lab_scrub();
    let request = codec::ctl_request(&["@s-1", "send", "cat /Users/_a/notes"]);
    let reply = codec::ctl_reply(&Ok(CtlReply {
        code: 0,
        stdout: "OK cwd=/Users/_a/wt\n".to_string(),
        stderr: "alice\n".to_string(),
    }));
    let items = vec![
        Item::stamped(
            EffectRecord::Exchange {
                door: doors::CTL.to_string(),
                request,
                reply,
            },
            codec::Stamp {
                mono_ns: 7,
                wall_ms: 9,
            },
        ),
        Item::effect(EffectRecord::Emit {
            door: doors::JOURNAL.to_string(),
            bytes: br#"{"line":"EVENT idle /Users/_a/wt"}"#.to_vec(),
        }),
        Item::effect(EffectRecord::FileErr {
            path: "/Users/_a/x".to_string(),
            kind: std::io::ErrorKind::NotFound,
        }),
        Item::effect(EffectRecord::Mono(42)),
        Item::End(End::Returned {
            how: "error: /Users/_a gone".to_string(),
        }),
    ];
    let (header, out) = s
        .tape(Some("home=/Users/_a\nuser=alice\n"), &items)
        .expect("a tape that reads");
    assert_eq!(header.as_deref(), Some("home=/lab/home\nuser=lab-user\n"));
    let Some(EffectRecord::Exchange {
        door,
        request,
        reply,
    }) = out[0].record()
    else {
        panic!("an exchange");
    };
    assert_eq!(door, doors::CTL);
    assert_eq!(
        codec::ctl_args(request).expect("a request"),
        ["@s-1", "send", "cat /lab/home/notes"]
    );
    let reply = codec::ctl_reply_of(reply)
        .expect("a reply")
        .expect("a success");
    assert_eq!(reply.stdout, "OK cwd=/lab/checkout\n");
    assert_eq!(reply.stderr, "lab-user\n");
    assert!(matches!(
        out[0],
        Item::Effect {
            stamp: Some(codec::Stamp {
                mono_ns: 7,
                wall_ms: 9
            }),
            ..
        }
    ));
    assert_eq!(
        out[1].record(),
        Some(&EffectRecord::Emit {
            door: doors::JOURNAL.to_string(),
            bytes: br#"{"line":"EVENT idle /lab/checkout"}"#.to_vec(),
        })
    );
    assert_eq!(
        out[2].record(),
        Some(&EffectRecord::FileErr {
            path: "/lab/home/x".to_string(),
            kind: std::io::ErrorKind::NotFound,
        })
    );
    assert_eq!(out[3], items[3]);
    assert_eq!(
        out[4],
        Item::End(End::Returned {
            how: "error: /lab/home gone".to_string()
        })
    );
}

/// A control request that does not read cannot be rewritten without its
/// lengths: the scrub refuses the tape, naming the item.
#[test]
fn a_control_request_that_does_not_read_is_refused() {
    let items = vec![Item::effect(EffectRecord::Exchange {
        door: doors::CTL.to_string(),
        request: vec![9, 1],
        reply: Vec::new(),
    })];
    let e = lab_scrub()
        .tape(None, &items)
        .expect_err("an unreadable request");
    assert!(e.contains("item 0 (ctl)"), "{e}");
}

/// THE LINT: `/Users/`, the recording machine's home and every name the
/// scrub takes — a word only where no letter stands beside it, any case —
/// each with its first line; and a text over the corpus's size.
#[test]
fn the_lint_names_what_a_corpus_may_not_hold() {
    let s = lab_scrub();
    assert!(
        lint(
            "home=/lab/home\nby lab-user\n",
            &s,
            Some(Path::new("/Users/_a"))
        )
        .is_empty()
    );
    let problems = lint(
        "ok\nsaw /users/zed\nAlice was here\nmalice is not\n",
        &s,
        Some(Path::new("/Volumes/h/a")),
    );
    assert_eq!(problems.len(), 2, "{problems:?}");
    assert!(
        problems[0].contains("\"/Users/\" on line 2"),
        "{problems:?}"
    );
    assert!(problems[1].contains("\"alice\" on line 3"), "{problems:?}");
    let home = lint("x /Volumes/h/a/y", &s, Some(Path::new("/Volumes/h/a/")));
    assert_eq!(home.len(), 1, "{home:?}");
    let big = "x".repeat(TEXT_MAX + 1);
    let size = lint(&big, &Scrub::new(), None);
    assert!(size.len() == 1 && size[0].contains("512 KiB"), "{size:?}");
}

/// THE TAPES A CHECK IS NAMED: a file as it is named; a directory's tapes at
/// any depth, in path order, its other files and its links passed over.
#[test]
fn a_check_reads_every_tape_under_a_directory() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let root = dir.path();
    std::fs::create_dir_all(root.join("b/c")).expect("dirs");
    for name in ["b/c/z.atape.txt", "b/a.atape", "a.atape.txt", "README.md"] {
        std::fs::write(root.join(name), b"x").expect("file");
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(root.join("a.atape.txt"), root.join("b/link.atape.txt"))
        .expect("a link");
    let named = root.join("named.txt");
    std::fs::write(&named, b"x").expect("file");
    let found = tapes_in(&[root.to_path_buf(), named.clone()]).expect("a listing");
    let want: Vec<PathBuf> = ["a.atape.txt", "b/a.atape", "b/c/z.atape.txt"]
        .iter()
        .map(|n| root.join(n))
        .chain([named])
        .collect();
    assert_eq!(found, want);
    assert!(tapes_in(&[root.join("missing")]).is_err());
}

/// A CHECK'S EXIT: a divergence first, then a tape not replayed, a stale one,
/// a reproduced panic; 0 only when every tape took its acts.
#[test]
fn a_checks_exit_is_its_worst_tapes() {
    assert_eq!(worst(&[]), 0);
    assert_eq!(worst(&[0, 0]), 0);
    assert_eq!(worst(&[0, 4, 3]), 3);
    assert_eq!(worst(&[3, 2]), 2);
    assert_eq!(worst(&[2, 1, 4]), 1);
}
