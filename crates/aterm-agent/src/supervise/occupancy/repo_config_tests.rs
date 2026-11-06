// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A `repo =` source's remote, read out of `.git/config` in process.

use super::*;
use crate::supervise::occupancy::fault::{self, Fault};

const PLAIN: &str = "[core]\n\trepositoryformatversion = 0\n\tbare = false\n\
                     [remote \"origin\"]\n\turl = git@h:o/r.git\n\
                     \tfetch = +refs/heads/*:refs/remotes/origin/*\n\
                     [branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/main\n";

fn read(text: &str, remote: &str) -> Result<String, RepoConfigRefusal> {
    remote_url(text.as_bytes(), remote).map(|url| url.stored())
}

#[test]
fn a_plain_remote_reads_and_another_name_does_not() {
    assert_eq!(read(PLAIN, "origin").as_deref(), Ok("git@h:o/r.git"));
    assert_eq!(read(PLAIN, "upstream"), Err(RepoConfigRefusal::NoUrl));
    assert_eq!(
        read(PLAIN, "Origin"),
        Err(RepoConfigRefusal::NoUrl),
        "a remote's name is exact"
    );
    for text in [
        "[Remote \"origin\"]\nURL = git@h:o/r.git\n",
        "[remote \"origin\"] # mine\nurl = \"git@h:o/r.git\" ; a comment\n",
        "\u{feff}[remote \"origin\"]\n  url=git@h:o/r.git   \n",
        "[remote \"origin\"]\nurl = git@h:o/r.git\n[remote \"other\"]\nurl = git@x:y/z.git\n",
    ] {
        assert_eq!(
            read(text, "origin").as_deref(),
            Ok("git@h:o/r.git"),
            "{text:?}"
        );
    }
    assert_eq!(
        read("[remote \"a\\\"b\"]\nurl = git@h:o/r.git\n", "a\"b").as_deref(),
        Ok("git@h:o/r.git"),
        "a quoted subsection's escape"
    );
}

/// AN INCLUDE REFUSES THE WHOLE FILE: the file it names is a second read the owner never pointed
/// aterm at (here under `~/Documents`, a privacy-guarded folder). NEGATIVE CONTROL
/// `IncludeFollowed`: a reader that reads past the include reads the remote.
#[test]
fn an_include_refuses_and_a_reader_that_follows_it_reads_the_remote() {
    let with_include = format!("[include]\n\tpath = ~/Documents/x\n{PLAIN}");
    assert_eq!(
        read(&with_include, "origin"),
        Err(RepoConfigRefusal::Include)
    );
    assert_eq!(
        fault::with(Fault::IncludeFollowed, || read(&with_include, "origin")).as_deref(),
        Ok("git@h:o/r.git")
    );
    for text in [
        format!("{PLAIN}[includeIf \"gitdir:~/\"]\n\tpath = ~/x\n"),
        format!("{PLAIN}[IncludeIf \"onbranch:main\"]\n\tpath = x\n"),
        format!("[include.x]\n{PLAIN}"),
    ] {
        assert_eq!(
            read(&text, "origin"),
            Err(RepoConfigRefusal::Include),
            "{text:?}"
        );
    }
}

#[test]
fn each_shape_it_cannot_vouch_for_refuses() {
    let long = format!("{PLAIN}{}", "#".repeat(70 * 1024));
    assert_eq!(read(&long, "origin"), Err(RepoConfigRefusal::Long));
    assert_eq!(
        remote_url(b"[remote \"origin\"]\nurl = \xff\n", "origin").map(|u| u.stored()),
        Err(RepoConfigRefusal::Unreadable)
    );
    for (text, want) in [
        (
            "[remote \"origin\"]\nurl = git@h:o/\\\nr.git\n",
            RepoConfigRefusal::Continuation,
        ),
        (
            "[remote \"origin\"\nurl = git@h:o/r.git\n",
            RepoConfigRefusal::Unreadable,
        ),
        (
            "[remote \"origin\"] x\nurl = git@h:o/r.git\n",
            RepoConfigRefusal::Unreadable,
        ),
        (
            "[remote \"origin\"]\n= git@h:o/r.git\n",
            RepoConfigRefusal::Unreadable,
        ),
        (
            "[remote \"origin\"]\nurl = \"git@h:o/r.git\n",
            RepoConfigRefusal::Unreadable,
        ),
        (
            "[remote \"origin\"]\nurl = git@h:o/r.git\nurl = git@x:o/r.git\n",
            RepoConfigRefusal::Several,
        ),
        (
            "[remote \"origin\"]\nurl = ext::sh -c x\n",
            RepoConfigRefusal::Url(UrlRefusal::Scheme),
        ),
    ] {
        assert_eq!(read(text, "origin"), Err(want), "{text:?}");
    }
}

/// A TOKEN IN THE OWNER'S REMOTE REFUSES THE SOURCE with the URL's own word, and nothing of the
/// URL leaves the reader: its only result is a [`RemoteUrl`] or a closed refusal.
#[test]
fn a_token_in_the_remote_refuses_with_the_urls_word() {
    let token = ["tok3n", "PLANT", &"q".repeat(30)].concat();
    let text = [
        "[remote \"origin\"]\nurl = https://",
        "x-access-token:",
        &token,
        "@h/o/r.git\n",
    ]
    .concat();
    let refused = read(&text, "origin");
    assert_eq!(refused, Err(RepoConfigRefusal::Url(UrlRefusal::Userinfo)));
    assert_eq!(
        RepoConfigRefusal::Url(UrlRefusal::Userinfo).word(),
        "userinfo"
    );
    assert!(!format!("{refused:?}").contains(&token));
    assert_eq!(RepoConfigRefusal::Include.word(), "repo-config");
}
