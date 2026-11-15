// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn destructive_words_match_their_inflections() {
    for (text, w) in [
        ("it deletes the rows", "delete"),
        ("the rows are deleted", "delete"),
        ("the table is dropped", "drop"),
        ("dropping it", "drop"),
        ("drops the legacy table", "drop"),
        ("removes node_modules", "remove"),
        ("wiping build/", "wipe"),
        ("wiped", "wipe"),
        ("resetting the branch", "reset"),
        ("the branch is force-pushed", "force-push"),
        ("the file is overwritten", "overwrite"),
        ("it overwrote the file", "overwrite"),
        ("this rewrote history", "rewrite history"),
        ("purges the cache", "purge"),
        ("destroys the volume", "destroy"),
        ("truncated the log", "truncate"),
        ("migrates the schema", "migrate"),
        ("deploys to prod", "deploy"),
        ("publishes the crate", "publish"),
        ("uninstalls it", "uninstall"),
        ("reverted the merge", "revert"),
        ("a full removal of the cache", "removal"),
        ("the deletion of stale branches", "deletion"),
        ("git push -f origin main", "push -f"),
        ("pushing -f to main", "push -f"),
        ("git push origin main -f", "push -f"),
        ("git push -vf origin main", "push -f"),
        ("git push origin feature.fix -vuf", "push -f"),
        ("git push origin and -f", "push -f"),
        (
            "git push --force-with-lease origin main",
            "force-with-lease",
        ),
        ("push --force-with-lease=main:4f2a1c9", "force-with-lease"),
    ] {
        assert_eq!(destructive_word(text), Some(w), "{text}");
    }
    for text in [
        "a dropdown menu",
        "the deployment guide",
        "reformat the file",
        "rename the table",
        "the migration tests pass",
        "git push origin main",
        "git push origin f",
        "git push origin main -ofoo",
        "git push origin main; tail -f the log",
        "tail -f the log",
        "cp -f a.txt b.txt",
    ] {
        assert_eq!(destructive_word(text), None, "{text}");
    }
}

#[test]
fn a_retired_token_line_is_a_done_claim() {
    for yes in [
        "ATERM-TASK-DONE",
        "All landed.\n`ATERM-TASK-DONE`",
        "All landed.\n**ATERM-TASK-DONE**\n",
        "All landed.\n  ATERM-TASK-DONE.  ",
    ] {
        assert!(says_done(Some(yes)), "{yes:?}");
    }
    for no in [
        "I will write ATERM-TASK-DONE when the task is finished.",
        "Fixed the parser; ATERM-TASK-DONE-ISH is next.",
        "",
    ] {
        assert!(!says_done(Some(no)), "{no:?}");
    }
    assert!(!says_done(None));
}

#[test]
fn a_remembered_tail_reads_as_a_fresh_read_of_it() {
    let tails = [
        "Suites are running.",
        "Should I delete the old branch? (y/n)",
        "Nothing left: the parser is on main.",
        "Suites are running!",
        "Want me to take that on?",
        "DONE",
        "Suites are running.",
    ];
    for _ in 0..2 {
        for t in tails {
            assert_eq!(classify_said(Some(t)), classify_said_read(t), "{t:?}");
            let fresh =
                needs::defense_line(t).is_some() || says_done_outright(&needs::without_needs(t));
            assert_eq!(says_done(Some(t)), fresh, "{t:?}");
        }
    }
    assert_eq!(classify_said(None), Said::Plain);
    assert!(!says_done(None));
    assert_ne!(classify_said_read(tails[0]), classify_said_read(tails[1]));
    assert_ne!(says_done(Some(tails[0])), says_done(Some(tails[2])));
}

/// ORDINARY COMPLETION PROSE IS NO DONE CLAIM, whatever it says of the work: the lexical done
/// reader is retired, and only a `Nothing left:` defense, the retired token alone on a line or a
/// bare `DONE` (the answer the done check asks for) says the work is done ([`says_done`]).
#[test]
fn ordinary_completion_prose_is_not_a_done_claim() {
    for no in [
        "Done. All tests pass.",
        "All done",
        "All done.",
        "I'm done: the parser is fixed.",
        "The task is complete.",
    ] {
        assert!(!says_done(Some(no)), "{no:?}");
    }
    for yes in ["Nothing left: the parser is on main.", "DONE", "**DONE**"] {
        assert!(says_done(Some(yes)), "{yes:?}");
    }
}
