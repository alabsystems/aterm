// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The record of a checkout's facts: its readers, against this host's real native git in a
//! scratch home, and the fixtures other tests script a checkout's record with on a [`Fake`].

use super::*;
use crate::checkout::Stands;
use crate::checkout::tests::{Repos, subcommands};
use crate::port::spawns::Spawns;
use crate::port::{Fake, Live, Ran};
use std::sync::atomic::{AtomicUsize, Ordering};

/// The config listing a scripted checkout's record hears ([`config_listing`]): each of
/// `entries`, a key and its value, in the checkout's own `.git/config`.
pub(crate) fn listing_of(entries: &[(&str, &str)]) -> String {
    entries
        .iter()
        .map(|(key, value)| format!("local\0file:.git/config\0{key}\n{value}\0"))
        .collect()
}

/// `path` a checkout on `sys` as its record of facts reads it ([`Facts::of`]): the top of a
/// worktree, not shallow, its index at `index` and its alternates file beside it (there only
/// where the test puts one), `config` the entries its own config holds ([`listing_of`]), HEAD
/// on `main` and origin's HEAD on `origin/main`, as a clone leaves them. A [`Fake`] holds no
/// HEAD file, so both are asked of `symbolic-ref`, and keeps no record unless a test gives it
/// one, so each read runs these again. Its refs are [`script_refs`]'.
pub(crate) fn script(sys: &Fake, who: &User, path: &Path, index: &Path, config: &[(&str, &str)]) {
    let alternates = index.with_file_name("objects/info/alternates");
    sys.on(
        &rev_parse(sys, who, path).display(),
        Ran::ok(&format!(
            "true\n\n{}\nfalse\n{}\n{}\n",
            path.display(),
            index.display(),
            alternates.display()
        )),
    );
    sys.on(
        &config_listing(sys, who, path).display(),
        Ran::ok(&listing_of(config)),
    );
    sys.on(
        &git(sys, who, Some(path))
            .args(["symbolic-ref", "--quiet", "HEAD"])
            .display(),
        Ran::ok("refs/heads/main\n"),
    );
    sys.on(
        &git(sys, who, Some(path))
            .args(["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"])
            .display(),
        Ran::ok("refs/remotes/origin/main\n"),
    );
}

/// The refs the record of [`script`]'s checkout at `path` reads ([`ref_listing`] of `main`
/// and `origin/main`): both at `head`, or neither, a branch with no commit yet.
pub(crate) fn script_refs(sys: &Fake, who: &User, path: &Path, head: Option<&str>) {
    let listed = head.map_or_else(String::new, |head| {
        format!("{head} commit refs/heads/main\n{head} commit refs/remotes/origin/main\n")
    });
    sys.on(&main_refs(sys, who, path).display(), Ran::ok(&listed));
}

/// The `for-each-ref` the record of [`script`]'s checkout at `path` runs.
pub(crate) fn main_refs(sys: &Fake, who: &User, path: &Path) -> Cmd {
    ref_listing(
        sys,
        who,
        path,
        &[
            "refs/heads/main".to_owned(),
            "refs/remotes/origin/main".to_owned(),
        ],
    )
}

/// How many gits `sys` ran in `path` so far.
fn gits_in(sys: &Spawns<'_>, path: &Path) -> usize {
    sys.record()
        .iter()
        .filter(|(cmd, _)| {
            cmd.args
                .windows(2)
                .any(|pair| pair[0] == "-C" && pair[1] == path.as_os_str())
        })
        .count()
}

/// A CONFIG LISTING IS READ ENTRY BY ENTRY: its scope, the file it is in, its key and its value,
/// none for a key written with no `=`; a listing that does not end its last entry, or cuts one
/// short, is no listing.
#[test]
fn a_config_listing_is_read_entry_by_entry() {
    let listing = b"local\0file:.git/config\0remote.origin.url\nhttps://github.com/alice/w.git\0\
                    global\0file:/home/alice/.gitconfig\0hook.y.enabled\0";
    let read = entries(listing).unwrap();
    assert_eq!(read.len(), 2);
    assert_eq!(read[0].scope, b"local");
    assert_eq!(read[0].origin, b"file:.git/config");
    assert_eq!(read[0].key, b"remote.origin.url");
    assert_eq!(
        read[0].value.as_deref(),
        Some(b"https://github.com/alice/w.git".as_slice())
    );
    assert_eq!(read[1].key, b"hook.y.enabled");
    assert_eq!(read[1].value, None);
    assert_eq!(
        config_file(Path::new("/home/alice/w"), &read[0].origin),
        Some(PathBuf::from("/home/alice/w/.git/config"))
    );
    assert_eq!(config_file(Path::new("/w"), b"command line:"), None);
    assert!(entries(b"").unwrap().is_empty());
    assert!(entries(b"local\0file:.git/config\0remote.origin.url\nx").is_none());
    assert!(entries(b"local\0file:.git/config\0").is_none());
}

/// A REF A RECORD READS IS A PLAIN NAME UNDER `refs/`: a part that is empty, starts with `.`
/// (reftable's `.invalid`), climbs, or holds a character git refuses or reads as a pattern is
/// not one, so no such name becomes a `for-each-ref` pattern or a watched path.
#[test]
fn a_ref_a_record_reads_is_a_plain_name_under_refs() {
    for plain in [
        "refs/heads/main",
        "refs/heads/feature/x",
        "refs/remotes/origin/main",
    ] {
        assert!(plain_ref(plain), "{plain}");
    }
    for not in [
        "HEAD",
        "refs/heads/.invalid",
        "refs/heads/a..b",
        "refs/heads/",
        "refs//main",
        "refs/heads/m*",
        "refs/heads/m?",
        "refs/heads/m[1]",
        "refs/heads/a b",
        "refs/heads/x.lock",
        "refs/heads/x@{1}",
        "heads/main",
    ] {
        assert!(!plain_ref(not), "{not}");
    }
    assert_eq!(
        ref_paths(Path::new("/w/.git"), "refs/heads/feature/x"),
        [
            PathBuf::from("/w/.git/refs"),
            PathBuf::from("/w/.git/refs/heads"),
            PathBuf::from("/w/.git/refs/heads/feature"),
            PathBuf::from("/w/.git/refs/heads/feature/x"),
        ]
    );
}

/// A RECORD IS BOUND ONLY TO THE FILES IT WATCHES ([`bound`]): an entry from a watched file, and
/// an unconditional include of a watched file by its absolute path (a managed GNU Git's own
/// system file, which aterm watches, includes `/etc/gitconfig` so), leave it reusable.
/// NEGATIVE CONTROLS: the same include where `/etc/gitconfig` is not watched, an include of a
/// file it does not watch, one git resolves against something else (`~/`, a relative path),
/// one with no value, a conditional include even of a watched file, an entry from a file it
/// does not watch and one from the command line each leave it unreusable.
#[test]
fn a_record_is_bound_only_to_the_files_it_watches() {
    let path = Path::new("/home/alice/w");
    let system = "/home/alice/.local/share/aterm/pkg/store/git/1/etc/gitconfig";
    let watch = [
        PathBuf::from(system),
        PathBuf::from("/etc/gitconfig"),
        path.join(".git/config"),
    ];
    let entry = |scope: &str, origin: &str, key: &str, value: Option<&str>| Entry {
        scope: scope.as_bytes().to_vec(),
        origin: origin.as_bytes().to_vec(),
        key: key.as_bytes().to_vec(),
        value: value.map(|value| value.as_bytes().to_vec()),
    };
    let system_origin = format!("file:{system}");
    let include =
        |value: Option<&str>| entry("system", system_origin.as_str(), "include.path", value);
    for kept in [
        entry(
            "local",
            "file:.git/config",
            "remote.origin.url",
            Some("https://github.com/alice/w.git"),
        ),
        include(Some("/etc/gitconfig")),
        entry(
            "system",
            "file:/etc/gitconfig",
            "include.path",
            Some(system),
        ),
    ] {
        assert!(bound(path, &kept, &watch), "{kept:?}");
    }
    assert!(
        !bound(path, &include(Some("/etc/gitconfig")), &watch[..1]),
        "control: /etc/gitconfig not watched"
    );
    for unbound in [
        include(Some("/opt/other.gitconfig")),
        include(Some("~/.gitconfig")),
        include(Some("gitconfig")),
        include(None),
        entry(
            "system",
            system_origin.as_str(),
            "includeif.gitdir:/home/alice/.path",
            Some("/etc/gitconfig"),
        ),
        entry(
            "global",
            "file:/opt/other.gitconfig",
            "remote.origin.url",
            Some("x"),
        ),
        entry("command", "command line:", "remote.origin.url", Some("x")),
    ] {
        assert!(!bound(path, &unbound, &watch), "{unbound:?}");
    }
}

/// A RECORD WATCHES THE GIT THAT READ IT AND ITS BUILD'S OWN CONFIG FILES ([`program_files`]),
/// named both as the program's path names them and where it resolves, which is how a git that
/// finds its prefix from its own path names its system file. NEGATIVE CONTROL: with no link on
/// the way, each is named once.
#[test]
fn a_record_watches_the_git_build_s_own_files_where_they_resolve() {
    let program = Path::new("/home/alice/.local/share/aterm/pkg/store/git/1/bin/git");
    let sys = Fake::default();
    let named = [
        "/home/alice/.local/share/aterm/pkg/store/git/1/etc/gitconfig",
        "/home/alice/.local/share/aterm/pkg/store/git/1/share/git-core/gitconfig",
        "/home/alice/.local/share/aterm/pkg/store/git/1/bin/git",
    ]
    .map(PathBuf::from);
    assert_eq!(program_files(&sys, program), named, "control: no link");
    sys.link("/home/alice", "/data/alice");
    let watched = program_files(&sys, program);
    for file in named.iter().cloned().chain(
        [
            "/data/alice/.local/share/aterm/pkg/store/git/1/etc/gitconfig",
            "/data/alice/.local/share/aterm/pkg/store/git/1/share/git-core/gitconfig",
            "/data/alice/.local/share/aterm/pkg/store/git/1/bin/git",
        ]
        .map(PathBuf::from),
    ) {
        assert!(watched.contains(&file), "{file:?}: {watched:?}");
    }
}

/// A RECORD IS READ WITH THREE GITS, REUSED WITH NONE WHILE ITS FILES STAND, AND READ AGAIN ONCE
/// ONE CHANGES: a clean clone's facts take `rev-parse`, `config` and `for-each-ref` (HEAD and
/// origin's HEAD read from their files); asked again, nothing runs and the same record answers;
/// once its tracking is changed (`git config` rewrites `.git/config`), the next ask reads it
/// again and sees the new upstream.
/// NEGATIVE CONTROL: a record reused without its metadata recheck still says the branch tracks
/// origin after the change, where this one says it does not.
#[test]
fn a_record_is_reused_while_its_files_stand_and_read_again_once_one_changes() {
    let live = Live::new();
    let repos = Repos::new(&live, "facts-record");
    let sys = Spawns::new(&live);
    let first = Facts::of(&sys, &repos.who, &repos.work).unwrap();
    assert!(first.reusable(), "{first:?}");
    assert_eq!(
        subcommands(&sys.record()),
        ["rev-parse", "config", "for-each-ref"],
        "{:?}",
        sys.record()
    );
    assert!(first.top());
    assert!(!first.shallow() && !first.partial());
    assert!(first.tracks_origin("main"));
    assert_eq!(first.head(), &Head::Symbolic("refs/heads/main".into()));
    let head = repos.head(&repos.work);
    assert_eq!(first.head_commit(), Some(head.as_str()));
    assert_eq!(first.unborn(), Ok(None));
    assert_eq!(first.fetched_head(), Ok(("main".to_owned(), head.clone())));
    assert!(first.watch().contains(&repos.work.join(".git/config")));

    let again = Facts::of(&sys, &repos.who, &repos.work).unwrap();
    assert!(Arc::ptr_eq(&first, &again), "the record answers again");
    assert_eq!(gits_in(&sys, &repos.work), 3);

    repos.g(&repos.work, &["config", "branch.main.remote", "other"]);
    let changed = Facts::of(&sys, &repos.who, &repos.work).unwrap();
    assert!(!Arc::ptr_eq(&first, &changed));
    assert_eq!(gits_in(&sys, &repos.work), 6, "{:?}", sys.record());
    assert!(!changed.tracks_origin("main"));
    assert!(
        first.tracks_origin("main"),
        "control: the old record says it tracks"
    );
}

/// The gits a checkout runs before and after its fetch, by subcommand: what follows the first
/// `fetch` in `ran`.
fn after_fetch(ran: &[String]) -> Vec<&str> {
    ran.iter()
        .skip_while(|sub| *sub != "fetch")
        .skip(1)
        .map(String::as_str)
        .collect()
}

/// Make `work`'s `main` track another remote, as a person's `git config` would.
fn retarget(who: &User, work: &Path) {
    let live = Live::new();
    let ran = live
        .run(&git(&live, who, Some(work)).args(["config", "branch.main.remote", "other"]))
        .unwrap();
    assert!(ran.success(), "{}", ran.describe());
}

/// A CHECKOUT CHANGED BETWEEN ITS RECORD AND THE MERGE IS READ AGAIN AND REFUSED, AS BEFORE THE
/// RECORD: align reads a clean checkout behind its origin, fetches, reads it again from a record
/// filled after the fetch, and before its merge the guard asks whether the branch still tracks
/// origin ([`super::super::tracks_origin`]); where its tracking was changed after that record
/// was read (just before the guard's last check), the record is read again (a second config
/// listing after the fetch), the change is seen, and nothing merges: HEAD stays where it was.
/// NEGATIVE CONTROL: the same alignment with the tracking left alone reuses the record (one
/// config listing after the fetch) and fast-forwards to origin's tip.
#[test]
fn a_checkout_changed_between_its_record_and_the_merge_is_read_again_and_refused() {
    let live = Live::new();
    for change in [false, true] {
        let repos = Repos::new(&live, "facts-merge-guard");
        let before = repos.head(&repos.work);
        let tip = repos.advance("two.txt", "two\n");
        let sys = Spawns::new(&live);
        let origin = repos.admitted();
        let (who, work) = (&repos.who, &repos.work);
        let checks = AtomicUsize::new(0);
        let guard = || {
            // align checks at its start, before its fetch and before its merge.
            if checks.fetch_add(1, Ordering::SeqCst) == 2 && change {
                retarget(who, work);
            }
            if super::super::tracks_origin(&sys, who, work, "main") {
                Ok(())
            } else {
                Err("upstream tracking changed during refresh; work preserved".to_owned())
            }
        };
        let aligned = super::super::align_guarded(&sys, who, work, &origin, &guard);
        assert_eq!(checks.load(Ordering::SeqCst), 3, "{change}: {aligned:?}");
        let ran = subcommands(&sys.record());
        let after = after_fetch(&ran);
        assert!(ran.iter().any(|sub| sub == "fetch"), "{ran:?}");
        if change {
            let unread = aligned.unwrap_err();
            assert!(
                unread.why.contains("upstream tracking changed"),
                "{unread:?}"
            );
            assert_eq!(repos.head(&repos.work), before);
            assert!(!ran.iter().any(|sub| sub == "merge"), "{ran:?}");
            assert_eq!(
                after.iter().filter(|sub| **sub == "config").count(),
                2,
                "read again after the change: {after:?}"
            );
        } else {
            assert!(
                matches!(&aligned, Ok(Stands::Behind { by: 1, .. })),
                "{aligned:?}"
            );
            assert_eq!(repos.head(&repos.work), tip);
            assert_eq!(
                after.iter().filter(|sub| **sub == "config").count(),
                1,
                "control: the record stands for the guard: {after:?}"
            );
        }
    }
}
