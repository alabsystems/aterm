// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::ledger::testing;
use std::time::Duration;

struct Built {
    _dir: aterm_tempfile::TempDir,
    work: PathBuf,
    exe: PathBuf,
    head: String,
}

fn git(work: &Path, args: &[&str]) -> String {
    let out = testing::git()
        .arg("-C")
        .arg(work)
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn set_modified(path: &Path, at: SystemTime) {
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(at)
        .unwrap();
}

/// Date the folder at `path` (opened to read, as a folder cannot be opened to write).
fn set_folder_modified(path: &Path, at: SystemTime) {
    fs::File::open(path).unwrap().set_modified(at).unwrap();
}

/// Date every file and folder under `path`, it included, but `.git` and the symbolic links,
/// which nothing here dates without following them.
#[cfg(unix)]
fn backdate(path: &Path, at: SystemTime) {
    for entry in fs::read_dir(path).unwrap() {
        let entry = entry.unwrap();
        let kind = entry.file_type().unwrap();
        if entry.file_name() == ".git" || kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            backdate(&entry.path(), at);
        } else {
            set_modified(&entry.path(), at);
        }
    }
    set_folder_modified(path, at);
}

fn built() -> Built {
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-release-cutter-")
        .tempdir()
        .unwrap();
    let work = dir.path().join("work with space");
    testing::init(
        &work,
        &[
            ("user.name", "Test"),
            ("user.email", "test@example.invalid"),
            ("commit.gpgsign", "false"),
        ],
    );
    fs::create_dir_all(work.join("crates/aterm-release/src")).unwrap();
    fs::write(work.join(OWN_MAIN), "fn main() {}\n").unwrap();
    fs::write(work.join("README.md"), "prose\n").unwrap();
    git(&work, &["add", "--all"]);
    git(&work, &["commit", "-q", "-m", "the cutter"]);
    let head = git(&work, &["rev-parse", "HEAD"]);
    let gate = dir.path().join("target/gate");
    fs::create_dir_all(&gate).unwrap();
    let exe = gate.join("aterm-release");
    fs::write(&exe, "linked").unwrap();
    let escaped = |p: &Path| p.display().to_string().replace(' ', "\\ ");
    fs::write(
        exe.with_extension("d"),
        format!("{}: {}\n", escaped(&exe), escaped(&work.join(OWN_MAIN))),
    )
    .unwrap();
    // Linked a day after its sources: later than any file or folder a test changes without
    // giving it a time of its own, so only those a test dates after the link are.
    let source_time = fs::metadata(work.join(OWN_MAIN))
        .unwrap()
        .modified()
        .unwrap();
    set_modified(&exe, source_time + Duration::from_secs(24 * 60 * 60));
    Built {
        _dir: dir,
        work,
        exe,
        head,
    }
}

/// The judgment of a cutter run directly, from its link.
fn judged(b: &Built) -> Result<String, NotOwn> {
    judge(&b.exe, Since::Link(linked(b)))
}

/// Whether `fix` is the remedy for a stale input of a cutter not judged from a `targo`'s
/// start: clean its build in its checkout, then run it as `targo` runs it.
fn cleans_first(b: &Built, fix: &str) -> bool {
    fix.starts_with(&format!(
        "`targo clean --profile gate -p aterm-release` in {} (",
        b.work.display()
    )) && fix.ends_with(&format!("then {RELINK}"))
}

#[test]
fn a_fresh_build_of_a_clean_checkout_is_its_heads_cutter() {
    let b = built();
    assert_eq!(judged(&b), Ok(b.head.clone()));
    fs::write(b.work.join("README.md"), "edited\n").unwrap();
    assert_eq!(judged(&b), Ok(b.head.clone()));
}

#[test]
fn a_source_changed_after_the_link_refuses() {
    let b = built();
    let linked = fs::metadata(&b.exe).unwrap().modified().unwrap();
    set_modified(&b.work.join(OWN_MAIN), linked + Duration::from_secs(1));
    let not = judged(&b).expect_err("stale");
    assert!(
        not.why.contains("changed after this cutter was linked"),
        "{not:?}"
    );
    assert!(cleans_first(&b, &not.fix), "{not:?}");
    fs::remove_file(b.work.join(OWN_MAIN)).unwrap();
    let not = judged(&b).expect_err("gone");
    assert!(not.why.contains("is gone"), "{not:?}");
}

/// A build input there that no commit holds refuses, as does one edited and not committed,
/// though each is older than the link.
#[test]
fn an_uncommitted_cutter_source_refuses() {
    let b = built();
    let old = fs::metadata(&b.exe).unwrap().modified().unwrap() - Duration::from_secs(60);
    let lock = b.work.join("Cargo.lock");
    fs::write(&lock, "# not committed\n").unwrap();
    set_modified(&lock, old);
    let not = judged(&b).expect_err("untracked");
    assert!(
        not.why.contains("reads Cargo.lock, which") && not.why.contains("does not track"),
        "{not:?}"
    );
    commit(&b.work, "the lockfile");
    fs::write(&lock, "# edited, not committed\n").unwrap();
    set_modified(&lock, old);
    let not = judged(&b).expect_err("dirty");
    assert!(
        not.why.contains("uncommitted cutter sources (Cargo.lock)"),
        "{not:?}"
    );
    assert!(not.fix.contains("git status"), "{not:?}");
}

fn linked(b: &Built) -> SystemTime {
    fs::metadata(&b.exe).unwrap().modified().unwrap()
}

/// Write `text` at `rel` in the checkout, modified `at`.
fn place(b: &Built, rel: &str, text: &str, at: SystemTime) {
    let path = b.work.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, text).unwrap();
    set_modified(&path, at);
}

/// Commit everything in the checkout; the new `HEAD`.
fn commit(work: &Path, message: &str) -> String {
    git(work, &["add", "--all"]);
    git(work, &["commit", "-q", "-m", message]);
    git(work, &["rev-parse", "HEAD"])
}

/// Make the binary's dep-info name its main and `rels`, files in its checkout.
fn compiled(b: &Built, rels: &[&str]) {
    let escaped = |p: &Path| p.display().to_string().replace(' ', "\\ ");
    let words: Vec<String> = std::iter::once(OWN_MAIN)
        .chain(rels.iter().copied())
        .map(|rel| escaped(&b.work.join(rel)))
        .collect();
    fs::write(
        b.exe.with_extension("d"),
        format!("{}: {}\n", escaped(&b.exe), words.join(" ")),
    )
    .unwrap();
}

#[cfg(unix)]
mod directory_inputs {
    use super::*;

    const WATCH: &str = "fixtures/steps";
    const CHILD: &str = "fixtures/steps/nested/b.jsonl";

    fn fixture() -> (Built, String) {
        let b = built();
        let old = linked(&b) - Duration::from_secs(60);
        place(&b, "fixtures/steps/a.jsonl", "first\n", old);
        place(&b, CHILD, "nested\n", old);
        place(
            &b,
            ".gitignore",
            "fixtures/steps/ignored.jsonl\nfixtures/steps/ignored-dir/\n",
            old,
        );
        let head = commit(&b.work, "tracked directory inputs");
        compiled(&b, &[WATCH]);
        backdate(&b.work, old);
        (b, head)
    }

    #[test]
    fn a_directory_dep_info_entry_accepts_its_tracked_tree() {
        let (b, head) = fixture();
        assert_eq!(judged(&b), Ok(head));
    }

    #[test]
    fn directory_inputs_reject_nested_edits_and_removals_outside_source_prefixes() {
        for remove in [false, true] {
            let (b, _) = fixture();
            let old = linked(&b) - Duration::from_secs(60);
            if remove {
                fs::remove_file(b.work.join(CHILD)).unwrap();
            } else {
                place(&b, CHILD, "different\n", old);
            }
            backdate(&b.work, old);
            let not = judged(&b).expect_err("changed descendant is a compiled input");
            assert!(
                not.why.contains("uncommitted cutter sources") && not.why.contains(CHILD),
                "{not:?}"
            );
        }
    }

    #[test]
    fn directory_input_freshness_retains_directory_entry_changes() {
        let (b, _) = fixture();
        let nested = b.work.join("fixtures/steps/nested");
        let transient = nested.join("removed.jsonl");
        fs::write(&transient, "added then removed\n").unwrap();
        fs::remove_file(transient).unwrap();
        set_folder_modified(&nested, linked(&b) + Duration::from_secs(1));
        let not = judged(&b).expect_err("a removed entry still changes the directory");
        assert!(
            not.why.contains("fixtures/steps/nested changed after"),
            "{not:?}"
        );
    }

    #[test]
    fn directory_inputs_reject_untracked_ignored_and_empty_entries() {
        for (name, directory) in [
            ("loose.jsonl", false),
            ("ignored.jsonl", false),
            ("empty", true),
            ("ignored-dir", true),
        ] {
            let (b, _) = fixture();
            let path = b.work.join(WATCH).join(name);
            if directory {
                fs::create_dir(&path).unwrap();
            } else {
                fs::write(&path, "not committed\n").unwrap();
            }
            backdate(&b.work, linked(&b) - Duration::from_secs(60));
            let not = judged(&b).expect_err("untracked directory input");
            assert!(
                not.why.contains(name) && not.why.contains("does not track"),
                "{not:?}"
            );
        }
    }

    #[test]
    fn directory_inputs_keep_internal_links_and_their_targets_in_the_proof() {
        let (b, _) = fixture();
        let old = linked(&b) - Duration::from_secs(60);
        place(&b, "fixtures/target.jsonl", "target\n", old);
        let alias = b.work.join(WATCH).join("alias.jsonl");
        std::os::unix::fs::symlink("../target.jsonl", &alias).unwrap();
        std::os::unix::fs::symlink("nested", b.work.join(WATCH).join("nested-alias")).unwrap();
        let head = commit(&b.work, "internal file and directory links");
        backdate(&b.work, old);
        assert_eq!(judged(&b), Ok(head.clone()));

        place(&b, "fixtures/target.jsonl", "target changed\n", old);
        let not = judged(&b).expect_err("linked target outside watched directory still checked");
        assert!(not.why.contains("fixtures/target.jsonl"), "{not:?}");
        place(&b, "fixtures/target.jsonl", "target\n", old);
        assert_eq!(judged(&b), Ok(head));

        fs::remove_file(&alias).unwrap();
        std::os::unix::fs::symlink("nested/b.jsonl", &alias).unwrap();
        let not = judged(&b).expect_err("link identity itself remains checked");
        assert!(not.why.contains("fixtures/steps/alias.jsonl"), "{not:?}");
    }

    #[test]
    fn directory_inputs_refuse_escape_and_cycles() {
        for cycle in [false, true] {
            let (b, _) = fixture();
            let old = linked(&b) - Duration::from_secs(60);
            let link = b.work.join(WATCH).join("link");
            let target = if cycle { "." } else { "../../../outside" };
            fs::write(b.work.with_file_name("outside"), "outside\n").unwrap();
            std::os::unix::fs::symlink(target, &link).unwrap();
            commit(&b.work, "a directory input link");
            backdate(&b.work, old);
            let not = judged(&b).expect_err("escaping and cyclic directory walks refuse");
            assert!(
                not.why.contains(if cycle {
                    "symbolic-link cycle"
                } else {
                    "resolves outside"
                }),
                "{not:?}"
            );
        }
    }
}

/// A commit that changes only a build input dep-info does not list (the lockfile, a crate's
/// manifest) is no build of the binary linked before it, though no listed source changed and
/// the checkout is clean; the same commit made before the link is that binary's. NEGATIVE
/// CONTROL: compare the dep-info's sources alone, and the lockfile's commit is the binary's.
#[test]
fn a_build_input_committed_after_the_link_refuses() {
    for input in [
        "Cargo.lock",
        "Cargo.toml",
        "crates/aterm-release/Cargo.toml",
        "rust-toolchain.toml",
        ".cargo/config.toml",
    ] {
        let b = built();
        let after = linked(&b) + Duration::from_secs(1);
        place(&b, input, "# committed after the link\n", after);
        commit(&b.work, "only a build input");
        let not = judged(&b).expect_err(input);
        assert!(
            not.why
                .ends_with(&format!("{input} changed after this cutter was linked")),
            "{input}: {not:?}"
        );
        assert!(cleans_first(&b, &not.fix), "{not:?}");

        let b = built();
        place(
            &b,
            input,
            "# committed before\n",
            linked(&b) - Duration::from_secs(60),
        );
        let head = commit(&b.work, "a build input before the link");
        assert_eq!(judged(&b), Ok(head), "{input}");
    }
}

/// A build input there at the link and removed after it (by a commit checked out since, or by
/// hand) refuses though nothing that is there changed: the folder that held it changed after
/// the link, so it may have lost it. That holds for the cutter's own package manifest, gone
/// from the folder no source's package root then names, and for a manifest above it
/// (`crates/Cargo.toml`, where cargo looks for a workspace). One whose folder is unchanged
/// since the link was not there when it was linked. NEGATIVE CONTROLS: check only the build
/// inputs that are there, and each removal is judged the new HEAD's build; take the package
/// roots from the sources alone, and the removed package manifest is.
#[test]
fn a_build_input_removed_after_the_link_refuses() {
    for input in [
        "Cargo.lock",
        "rust-toolchain.toml",
        ".cargo/config.toml",
        "crates/aterm-release/Cargo.toml",
        "crates/Cargo.toml",
    ] {
        let b = built();
        let at = linked(&b);
        place(
            &b,
            "crates/aterm-release/Cargo.toml",
            "# the package root's manifest\n",
            at - Duration::from_secs(60),
        );
        place(
            &b,
            input,
            "# committed before the link\n",
            at - Duration::from_secs(60),
        );
        commit(&b.work, "a build input before the link");
        git(&b.work, &["rm", "-q", input]);
        let head = commit(&b.work, "the build input removed");
        let gone = fs::canonicalize(&b.work).unwrap().join(input);
        let folder = gone
            .ancestors()
            .skip(1)
            .find(|folder| folder.exists())
            .unwrap()
            .to_path_buf();
        set_folder_modified(&folder, at + Duration::from_secs(1));
        let not = judged(&b).expect_err(input);
        let dated = format!("{} changed after this cutter was linked", folder.display());
        assert!(
            not.why.starts_with(&dated)
                && not
                    .why
                    .ends_with("is not there: it may have been removed since"),
            "{input}: {not:?}"
        );
        assert!(cleans_first(&b, &not.fix), "{not:?}");
        set_folder_modified(&folder, at - Duration::from_secs(1));
        assert_eq!(judged(&b), Ok(head), "{input}, removed before the link");
    }
}

/// A compiled input read through a symbolic link names the link and the file it leads to,
/// each held to git and to the link: a tracked link, to the file or to a folder on its way,
/// retargeted (uncommitted) to another tracked one refuses by the link's name, though what it
/// leads to now is tracked and clean; a link made again after the link (a commit checked out
/// since that retargets it) refuses as changed, though every file is older than the link.
/// Links committed and untouched, or put back, are HEAD's. NEGATIVE CONTROLS: name each input
/// by the file it ends at alone, and the retargeted link is HEAD's build; date each link by
/// the file it leads to, and the link made again is.
#[cfg(unix)]
#[test]
fn a_compiled_input_read_through_a_link_names_the_link_and_its_file() {
    let b = built();
    let old = linked(&b) - Duration::from_secs(60);
    for rel in ["docs/a.md", "docs/b.md", "gen/one/x.rs", "gen/two/x.rs"] {
        place(&b, rel, "committed\n", old);
    }
    let link = |target: &str, rel: &str| {
        let at = b.work.join(rel);
        drop(fs::remove_file(&at));
        std::os::unix::fs::symlink(target, at).unwrap();
    };
    link("a.md", "docs/link.md");
    link("one", "gen/now");
    let head = commit(&b.work, "a link to a file and one to a folder");
    compiled(&b, &["docs/link.md", "gen/now/x.rs"]);
    assert_eq!(judged(&b), Ok(head.clone()), "committed links");

    for (target, was, rel) in [("b.md", "a.md", "docs/link.md"), ("two", "one", "gen/now")] {
        link(target, rel);
        let not = judged(&b).expect_err(rel);
        assert!(
            not.why
                .contains(&format!("uncommitted cutter sources ({rel})")),
            "{rel}: {not:?}"
        );
        link(was, rel);
        assert_eq!(judged(&b), Ok(head.clone()), "{rel} put back");
    }

    link("b.md", "docs/link.md");
    commit(&b.work, "the link retargeted");
    let past = SystemTime::now() - Duration::from_secs(60 * 60);
    backdate(&b.work, past - Duration::from_secs(60));
    set_modified(&b.exe, past);
    compiled(&b, &["docs/link.md"]);
    let not = judged(&b).expect_err("a link made again after the link");
    assert!(
        not.why
            .ends_with("docs/link.md changed after this cutter was linked"),
        "{not:?}"
    );
}

/// A build input that is a symbolic link, or is read through one (`.cargo` a link to a
/// folder), is held as a compiled input read through a link is: the file it leads to, edited
/// and not committed, refuses by that file's name; a commit checked out after the link that
/// retargets the link refuses by the link's name, though every file is older than the link
/// and the checkout is clean. Committed and untouched, or put back, it is HEAD's. NEGATIVE
/// CONTROLS: date each link by the file it leads to, and each retargeted link is the new
/// HEAD's build; hold to git only a build input's own name, and each edited file is HEAD's.
#[cfg(unix)]
#[test]
fn a_build_input_read_through_a_link_is_held_to_the_link_and_its_file() {
    for (rel, was, now, read, other) in [
        (
            "rust-toolchain.toml",
            "toolchains/a.toml",
            "toolchains/b.toml",
            "toolchains/a.toml",
            "toolchains/b.toml",
        ),
        (
            ".cargo",
            "cfgs/a",
            "cfgs/b",
            "cfgs/a/config.toml",
            "cfgs/b/notes.md",
        ),
    ] {
        let b = built();
        let old = linked(&b) - Duration::from_secs(60);
        place(&b, read, "committed\n", old);
        place(&b, other, "committed\n", old);
        std::os::unix::fs::symlink(was, b.work.join(rel)).unwrap();
        let head = commit(&b.work, "a build input read through a link");
        assert_eq!(judged(&b), Ok(head.clone()), "{rel} committed");

        place(&b, read, "edited, not committed\n", old);
        let not = judged(&b).expect_err(read);
        assert!(
            not.why
                .contains(&format!("uncommitted cutter sources ({read})")),
            "{rel}: {not:?}"
        );
        place(&b, read, "committed\n", old);
        assert_eq!(judged(&b), Ok(head), "{read} put back");

        fs::remove_file(b.work.join(rel)).unwrap();
        std::os::unix::fs::symlink(now, b.work.join(rel)).unwrap();
        commit(&b.work, "the link retargeted");
        let past = SystemTime::now() - Duration::from_secs(60 * 60);
        backdate(&b.work, past - Duration::from_secs(60));
        set_modified(&b.exe, past);
        let not = judged(&b).expect_err(rel);
        assert!(
            not.why
                .ends_with(&format!("{rel} changed after this cutter was linked")),
            "{rel}: {not:?}"
        );
        assert!(cleans_first(&b, &not.fix), "{not:?}");
    }
}

/// A compiled input read through a tracked link that leaves the checkout and comes back into
/// it refuses, though the link and the file it ends at are tracked and clean: no commit holds
/// the hop outside, so retargeting it would change what the cutter reads unseen. A link that
/// stays inside is HEAD's. NEGATIVE CONTROL: refuse only an input that ends outside the
/// checkout, and the link out and back is HEAD's build.
#[cfg(unix)]
#[test]
fn a_link_out_of_the_checkout_and_back_refuses() {
    let b = built();
    place(
        &b,
        "docs/a.md",
        "committed\n",
        linked(&b) - Duration::from_secs(60),
    );
    let hop = b.work.with_file_name("hop.md");
    std::os::unix::fs::symlink(b.work.join("docs/a.md"), &hop).unwrap();
    std::os::unix::fs::symlink("a.md", b.work.join("docs/in.md")).unwrap();
    std::os::unix::fs::symlink("../../hop.md", b.work.join("docs/out.md")).unwrap();
    let head = commit(&b.work, "a link inside and one out and back");
    compiled(&b, &["docs/in.md"]);
    assert_eq!(judged(&b), Ok(head), "a link that stays inside");
    compiled(&b, &["docs/out.md"]);
    let not = judged(&b).expect_err("a link out of the checkout and back");
    assert!(
        not.why.contains("docs/out.md, which resolves outside")
            && not.why.contains("or leaves it on the way"),
        "{not:?}"
    );
}

/// A compiled input no commit holds refuses: one `.gitignore` hides, in the checkout or in a
/// submodule of it, one untracked outside the cutter's source folders, and one a link takes
/// outside the checkout; so does a tracked one changed since `HEAD` outside them, named
/// through `..`. A tracked one in a submodule or named through `..` is the commit's.
/// NEGATIVE CONTROL: leave the dep-info's own inputs out of the git checks, and the ignored
/// `gen/out.rs` is `HEAD`'s build.
#[test]
fn a_compiled_input_no_commit_holds_refuses() {
    let b = built();
    let old = linked(&b) - Duration::from_secs(60);
    let sub = b.work.with_file_name("sub");
    testing::init(
        &sub,
        &[
            ("user.name", "Test"),
            ("user.email", "test@example.invalid"),
            ("commit.gpgsign", "false"),
        ],
    );
    fs::write(sub.join(".gitignore"), "gen/\n").unwrap();
    fs::write(sub.join("lib.rs"), "pub fn f() {}\n").unwrap();
    commit(&sub, "the submodule");
    let url = sub.to_str().unwrap();
    let file = "protocol.file.allow=always";
    git(
        &b.work,
        &["-c", file, "submodule", "add", "-q", url, "vendor/sub"],
    );
    fs::write(b.work.join(".gitignore"), "gen/\n").unwrap();
    place(&b, "docs/included.md", "committed\n", old);
    let head = commit(&b.work, "a submodule, an ignore rule, a document");
    set_modified(&b.work.join("vendor/sub/lib.rs"), old);
    let through = "crates/aterm-release/../../docs/included.md";
    compiled(&b, &["vendor/sub/lib.rs", "docs/included.md", through]);
    assert_eq!(
        judged(&b),
        Ok(head),
        "tracked inputs, one in a submodule and one named through `..`"
    );

    place(&b, "gen/out.rs", "generated\n", old);
    place(&b, "vendor/sub/gen/out.rs", "generated\n", old);
    place(&b, "loose.rs", "untracked\n", old);
    for (input, words) in [
        ("gen/out.rs", "reads gen/out.rs, which"),
        (
            "vendor/sub/gen/out.rs",
            "reads vendor/sub/gen/out.rs, which",
        ),
        ("loose.rs", "reads loose.rs, which"),
    ] {
        compiled(&b, &["vendor/sub/lib.rs", input]);
        let not = judged(&b).expect_err(input);
        assert!(
            not.why.contains(words) && not.why.contains("does not track"),
            "{input}: {not:?}"
        );
        assert!(
            not.fix.starts_with(&format!("commit {input} in ")),
            "{not:?}"
        );
    }

    #[cfg(unix)]
    {
        let outside = b.work.with_file_name("outside.rs");
        fs::write(&outside, "no commit holds this\n").unwrap();
        set_modified(&outside, old);
        std::os::unix::fs::symlink(&outside, b.work.join("crates/outside.rs")).unwrap();
        compiled(&b, &["crates/outside.rs"]);
        let not = judged(&b).expect_err("an input outside the checkout");
        assert!(
            not.why
                .contains("crates/outside.rs, which resolves outside"),
            "{not:?}"
        );
        fs::remove_file(b.work.join("crates/outside.rs")).unwrap();
    }

    place(&b, "docs/included.md", "edited, not committed\n", old);
    compiled(&b, &[through]);
    let not = judged(&b).expect_err("an edited input");
    assert!(
        not.why
            .contains("uncommitted cutter sources (docs/included.md)"),
        "{not:?}"
    );
}

#[test]
fn a_foreign_dep_info_refuses() {
    let b = built();
    fs::write(
        b.exe.with_extension("d"),
        "/elsewhere/aterm-release: /x.rs\n",
    )
    .unwrap();
    let not = judged(&b).expect_err("foreign");
    assert!(not.why.contains("does not describe"), "{not:?}");
    assert_eq!(not.fix, RELINK);
}

#[test]
fn a_verdict_is_never_taken_after_the_start() {
    let not = running().expect_err("never judged at its start");
    assert_eq!(not.fix, RELINK);
}

#[test]
fn the_clean_remedy_names_the_profile_the_binary_sits_in() {
    let flag = |dir: &str| profile_flag(&Path::new("/t").join(dir).join("aterm-release"));
    assert_eq!(flag("debug"), "");
    assert_eq!(flag("release"), " --release");
    assert_eq!(flag("gate"), " --profile gate");
}

#[test]
fn dep_info_words_keep_escaped_spaces_and_name_the_checkout() {
    assert_eq!(
        split_escaped("/a\\ b/t: /a\\ b/x.rs  /c.rs"),
        ["/a b/t:", "/a b/x.rs", "/c.rs"]
    );
    let main = PathBuf::from("/r/s").join(OWN_MAIN);
    assert_eq!(
        checkout_of(&[PathBuf::from("/x.rs"), main]),
        Some(PathBuf::from("/r/s"))
    );
    let kept = checkout_sources(
        vec![
            PathBuf::from("/r/s/crates/a.rs"),
            PathBuf::from("/r/s/target/debug/build/x/out/gen.rs"),
            PathBuf::from("/cache/build/x/out/gen.rs"),
            PathBuf::from("/r/s/.git/HEAD"),
        ],
        Path::new("/r/s"),
        &[Path::new("/r/s/target")],
    );
    assert_eq!(kept, [PathBuf::from("/r/s/crates/a.rs")]);
    assert!(is_source_input("crates/a/b.rs") && is_source_input("Cargo.lock"));
    assert!(!is_source_input("cratesx/a.rs") && !is_source_input("docs/a.md"));
    assert!(
        dep_info_sources(
            "/missing-other-output: /source.rs",
            Path::new("/missing-output")
        )
        .is_none()
    );
    assert!(
        dep_info_sources("/missing-output: /source.rs", Path::new("/missing-output")).is_none()
    );
}

#[cfg(unix)]
#[test]
fn checkout_root_aliases_keep_generated_outputs_out_and_real_inputs_in() {
    use std::os::unix::fs::symlink;
    let b = built();
    let alias = b._dir.path().join("checkout-alias");
    symlink(&b.work, &alias).unwrap();
    let output = b.work.join("artifact-output/gate/aterm-release");
    fs::create_dir_all(output.parent().unwrap()).unwrap();
    fs::write(&output, "linked").unwrap();
    set_modified(&output, linked(&b));
    let generated = alias.join("artifact-output/gate/build/fixture/out/generated.rs");
    fs::create_dir_all(generated.parent().unwrap()).unwrap();
    fs::write(&generated, "generated").unwrap();
    let write_deps = |extra: &Path| {
        let escaped = |p: &Path| p.display().to_string().replace(' ', "\\ ");
        fs::write(
            output.with_extension("d"),
            format!(
                "{}: {} {}\n",
                escaped(&output),
                escaped(&alias.join(OWN_MAIN)),
                escaped(extra)
            ),
        )
        .unwrap();
    };
    let verdict = || judge(&output, Since::Link(linked(&b)));
    write_deps(&generated);
    assert_eq!(verdict(), Ok(b.head.clone()));
    let untracked = b.work.join("crates/aterm-release/src/untracked.rs");
    fs::write(&untracked, "real input").unwrap();
    write_deps(&untracked);
    let refused = verdict().expect_err("canonical-spelled real source must still be attested");
    assert!(refused.why.contains("untracked.rs"), "{refused:?}");
    fs::remove_file(untracked).unwrap();
    let source_alias = alias.join("crates/aterm-release/src/output-alias.rs");
    symlink(&generated, &source_alias).unwrap();
    git(
        &b.work,
        &["add", "--", "crates/aterm-release/src/output-alias.rs"],
    );
    git(&b.work, &["commit", "-qm", "tracked input link"]);
    write_deps(&source_alias);
    assert!(
        verdict().is_err(),
        "a source link into generated output is still an input"
    );
}

#[cfg(unix)]
#[test]
fn only_the_matched_binary_output_namespace_may_use_a_target_directory_alias() {
    use std::os::unix::fs::symlink;
    let b = built();
    let physical = b._dir.path().join("build-output");
    let alias = b.work.join("output-alias");
    fs::create_dir_all(physical.join("gate/build/fixture/out")).unwrap();
    symlink(&physical, &alias).unwrap();
    let output = physical.join("gate/aterm-release");
    fs::write(&output, "linked").unwrap();
    set_modified(&output, linked(&b));
    let generated = alias.join("gate/build/fixture/out/generated.rs");
    fs::write(&generated, "generated").unwrap();
    let write_deps = |extra: &Path| {
        let escaped = |p: &Path| p.display().to_string().replace(' ', "\\ ");
        fs::write(
            output.with_extension("d"),
            format!(
                "{}: {} {}\n",
                escaped(&alias.join("gate/aterm-release")),
                escaped(&b.work.join(OWN_MAIN)),
                escaped(extra)
            ),
        )
        .unwrap();
    };
    let verdict = || judge(&output, Since::Link(linked(&b)));
    write_deps(&generated);
    assert_eq!(verdict(), Ok(b.head.clone()));
    let source_alias = b.work.join("crates/aterm-release/src/aliased-input.rs");
    symlink(&generated, &source_alias).unwrap();
    git(
        &b.work,
        &["add", "--", "crates/aterm-release/src/aliased-input.rs"],
    );
    git(&b.work, &["commit", "-qm", "tracked input link"]);
    write_deps(&source_alias);
    assert!(
        verdict().is_err(),
        "a non-output source link must not be canonicalized away"
    );
}

#[cfg(unix)]
#[test]
fn a_third_checkout_alias_cannot_hide_untracked_or_removed_inputs() {
    use std::os::unix::fs::symlink;
    let b = built();
    let alias = b._dir.path().join("third-checkout-alias");
    symlink(&b.work, &alias).unwrap();
    let old = linked(&b) - Duration::from_secs(60);
    place(&b, "fixtures/material.txt", "compiled input", old);
    let write_deps = |extra: &Path| {
        let escaped = |p: &Path| p.display().to_string().replace(' ', "\\ ");
        fs::write(
            b.exe.with_extension("d"),
            format!(
                "{}: {} {}\n",
                escaped(&b.exe),
                escaped(&b.work.join(OWN_MAIN)),
                escaped(extra)
            ),
        )
        .unwrap();
    };
    let input = alias.join("fixtures/material.txt");
    write_deps(&input);
    let untracked = judged(&b).expect_err("an alias must not hide an untracked input");
    assert!(
        untracked.why.contains("fixtures/material.txt"),
        "{untracked:?}"
    );
    let head = commit(&b.work, "tracked compiled fixture");
    assert_eq!(judged(&b), Ok(head.clone()));

    let external = b._dir.path().join("external-dependency.rs");
    fs::write(&external, "outside dependency").unwrap();
    write_deps(&external);
    assert_eq!(judged(&b), Ok(head));

    write_deps(&input);
    fs::remove_file(b.work.join("fixtures/material.txt")).unwrap();
    set_folder_modified(
        &b.work.join("fixtures"),
        linked(&b) + Duration::from_secs(1),
    );
    let removed = judged(&b).expect_err("a missing aliased leaf must remain an input");
    assert!(removed.why.contains("material.txt"), "{removed:?}");
}

#[cfg(unix)]
fn placed_at(b: &Built) -> SystemTime {
    placed(&fs::metadata(&b.exe).unwrap()).expect("a status change time")
}

/// A dependency crate's source edited while the `targo run` that ran this process was
/// building it (after that `targo` began, so after the unit that read it may have started,
/// and before the link) and committed refuses: a cutter whose executable was placed after
/// the `targo` that ran it began is judged from that start, not from its link. A run that
/// began after the edit is the new HEAD's build. A cutter no `targo` ran, or one placed
/// before its `targo` began, is judged from what it was handed, else its link. NEGATIVE
/// CONTROL: judge a launched cutter from its link, as before, and the edited source is the
/// new HEAD's build.
#[cfg(unix)]
#[test]
fn a_source_edited_while_the_launching_run_built_refuses() {
    let b = built();
    let began = placed_at(&b) - Duration::from_secs(60 * 60);
    let before = began - Duration::from_secs(60);
    let lib = "crates/lib/src/lib.rs";
    place(&b, "crates/lib/Cargo.toml", "[package]\n", before);
    place(&b, lib, "OLD RULES\n", before);
    let head = commit(&b.work, "a dependency crate");
    compiled(&b, &[lib]);
    backdate(&b.work, before);
    let run = |launcher, handed| {
        let own = Some(placed_at(&b));
        judge_process(&b.exe, launcher, own, handed)
    };
    assert_eq!(run(Some(began), None).stamp, Ok(Stamp::Built(head)));

    place(&b, lib, "NEW RULES\n", began + Duration::from_secs(60));
    let head = commit(&b.work, "new rules, committed while the run built");
    let not = run(Some(began), None)
        .stamp
        .expect_err("edited while the run built");
    assert!(
        not.why.ends_with(&format!(
            "{lib} changed after the process of the `targo` that started this cutter was \
             forked"
        )),
        "{not:?}"
    );
    assert_eq!(not.fix, RELINK);
    assert_eq!(run(Some(began), None).since, Some(Since::Launch(began)));
    let later = began + Duration::from_secs(120);
    assert_eq!(
        run(Some(later), None).stamp,
        Ok(Stamp::Built(head.clone())),
        "a run that began after the edit"
    );

    let placed_before = Some(placed_at(&b) + Duration::from_secs(1));
    for launcher in [None, placed_before] {
        assert_eq!(run(launcher, None).since, Some(Since::Link(linked(&b))));
        let handed = run(launcher, Some(began));
        assert_eq!(handed.since, Some(Since::Handed(began)));
        let not = handed.stamp.expect_err("handed");
        assert!(
            not.why.ends_with(&format!(
                "{lib} changed after the moment the cutter that handed this one on was \
                 judged from"
            )),
            "{not:?}"
        );
        assert_eq!(
            run(launcher, Some(later)).stamp,
            Ok(Stamp::Built(head.clone()))
        );
    }
}

/// A `targo run` that found the cutter fresh and left the link alone judges it from that
/// `targo`'s start, so a merge since the link that changed only what cargo relinks no unit
/// of it for (the lockfile, the `crates` folder's entries, the checkout's) leaves it the new
/// HEAD's build. A cutter run directly is judged from its link and refuses for the lockfile,
/// and its remedy cleans the build first, so a run judged from its link again relinks it.
/// NEGATIVE CONTROL: judge a launched cutter from its link, as before, and the lockfile
/// refuses on every run.
#[cfg(unix)]
#[test]
fn a_launched_run_is_judged_from_its_start_so_a_change_cargo_ignores_is_heads() {
    let b = built();
    let link = SystemTime::now() - Duration::from_secs(2 * 60 * 60);
    set_modified(&b.exe, link);
    place(&b, "crates/aterm-release/Cargo.toml", "[package]\n", link);
    place(&b, "Cargo.lock", "# before the merge\n", link);
    commit(&b.work, "the cutter's manifest and lockfile");
    backdate(&b.work, link - Duration::from_secs(60));
    let merged = link + Duration::from_secs(30 * 60);
    place(&b, "Cargo.lock", "# the merge's\n", merged);
    place(&b, "crates/other/Cargo.toml", "[package]\n", merged);
    place(&b, "crates/other/src/lib.rs", "\n", merged);
    let head = commit(&b.work, "a merge that adds a crate the cutter does not use");
    for folder in [b.work.clone(), b.work.join("crates")] {
        set_folder_modified(&folder, merged);
    }
    let began = link + Duration::from_secs(60 * 60);
    assert!(
        placed_at(&b) >= began,
        "the run placed it after its targo began"
    );
    let own = Some(placed_at(&b));
    let launched = judge_process(&b.exe, Some(began), own, None);
    assert_eq!(launched.stamp, Ok(Stamp::Built(head)));
    assert_eq!(launched.since, Some(Since::Launch(began)));

    let not = judged(&b).expect_err("judged from its link");
    assert!(
        not.why
            .ends_with("Cargo.lock changed after this cutter was linked"),
        "{not:?}"
    );
    assert!(cleans_first(&b, &not.fix), "{not:?}");
}

/// A folder between a source and its package root holds no manifest cargo reads, so one
/// dated after the link, by an update to a file the build never compiles, is no build input;
/// a folder above the package root still is. NEGATIVE CONTROL: take each source's manifest
/// candidates from its own folder up, and the dated `src` folder refuses.
#[cfg(unix)]
#[test]
fn a_folder_below_a_package_root_is_no_build_input() {
    let b = built();
    let at = linked(&b);
    place(
        &b,
        "crates/aterm-release/Cargo.toml",
        "[package]\n",
        at - Duration::from_secs(60),
    );
    place(
        &b,
        "crates/aterm-release/src/linux.rs",
        "// not compiled here\n",
        at + Duration::from_secs(1),
    );
    let head = commit(
        &b.work,
        "a file the build never compiles, updated after the link",
    );
    set_folder_modified(
        &b.work.join("crates/aterm-release/src"),
        at + Duration::from_secs(1),
    );
    assert_eq!(judged(&b), Ok(head));

    set_folder_modified(&b.work.join("crates"), at + Duration::from_secs(1));
    let not = judged(&b).expect_err("the folder above the package root");
    assert!(
        not.why
            .ends_with("crates/Cargo.toml is not there: it may have been removed since"),
        "{not:?}"
    );
}

/// The remedy for a stale input follows the moment the cutter was judged from: the run alone
/// for one judged from its `targo`'s start, which the next run is judged from again; the
/// build cleaned first for one judged from its link or a handed moment, as the next run may
/// be judged from its link (a start unread, a binary not placed between its `targo`'s start
/// and its own), which cargo moves only for a change it tracks. NEGATIVE CONTROL: choose the
/// remedy by whether this platform reads a process's start, and a cutter judged from its
/// link on macOS is told to run again alone, the refusal it would meet on every run.
#[test]
fn a_stale_input_names_a_clean_unless_judged_from_a_targos_start() {
    let b = built();
    let at = linked(&b);
    place(
        &b,
        "Cargo.lock",
        "# after the link\n",
        at + Duration::from_secs(1),
    );
    commit(&b.work, "a lockfile after the link");
    let before = at - Duration::from_secs(60);
    for since in [Since::Link(at), Since::Handed(before)] {
        let not = judge(&b.exe, since).expect_err("stale");
        assert!(cleans_first(&b, &not.fix), "{since:?}: {not:?}");
    }
    let not = judge(&b.exe, Since::Launch(before)).expect_err("stale");
    assert_eq!(not.fix, RELINK, "{not:?}");
}

/// The moment a cutter is judged from: the start of the `targo` that ran it when its
/// executable was placed after that start and before this process's own (to the grain the
/// kernel records a start in), whatever it was handed; else what it was handed; else its
/// link. NEGATIVE CONTROL: drop the bound by this process's start, and a binary placed after
/// the process started is judged from that `targo`'s start.
#[test]
fn a_cutter_is_judged_from_its_launch_else_its_hand_off_else_its_link() {
    let t = |s: u64| SystemTime::UNIX_EPOCH + Duration::from_secs(s);
    let us = Duration::from_micros(1);
    let (link, placed, handed) = (t(100), t(200), t(150));
    let launched = |began, own, handed| since(link, Some(placed), Some(began), Some(own), handed);
    assert_eq!(
        launched(t(190), t(210), Some(handed)),
        Since::Launch(t(190))
    );
    assert_eq!(launched(t(200), t(200), None), Since::Launch(t(200)));
    assert_eq!(
        launched(t(190), t(200) - us / 2, None),
        Since::Launch(t(190)),
        "placed within the grain of its recorded start"
    );
    for (placed, began, own, what) in [
        (
            Some(placed),
            Some(t(210)),
            Some(t(220)),
            "placed before its targo began",
        ),
        (
            Some(placed),
            Some(t(190)),
            Some(t(200) - us),
            "placed after this process started",
        ),
        (
            Some(placed),
            Some(t(190)),
            Some(t(199)),
            "placed after this process started",
        ),
        (None, Some(t(190)), Some(t(210)), "no placement read"),
        (Some(placed), None, Some(t(210)), "no targo ran it"),
        (Some(placed), Some(t(190)), None, "its own start unread"),
    ] {
        assert_eq!(
            since(link, placed, began, own, Some(handed)),
            Since::Handed(handed),
            "{what}"
        );
        assert_eq!(
            since(link, placed, began, own, None),
            Since::Link(link),
            "{what}"
        );
    }
}

/// A process's `targo` start is its parent's only when the parent is the program `$CARGO`
/// names, spelled through a link or not: a parent that is another program (one that
/// inherited `$CARGO` and started the cutter itself) gives no Launch moment, and the cutter
/// is judged from its link. NEGATIVE CONTROL: take the parent's start whatever program it
/// runs, and the other program's start is the cutter's Launch moment.
#[test]
fn a_parent_that_is_not_the_program_cargo_names_gives_no_launch() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let bin = dir.path().join("store/bin");
    fs::create_dir_all(&bin).unwrap();
    let targo = bin.join("targo");
    let other = dir.path().join("other");
    fs::write(&targo, "targo").unwrap();
    fs::write(&other, "another program").unwrap();
    let began = SystemTime::UNIX_EPOCH + Duration::from_secs(1_793_520_000);
    let cargo = Some(targo.as_os_str());
    assert_eq!(launcher(Some(&targo), cargo, Some(began)), Some(began));
    #[cfg(unix)]
    {
        let alias = dir.path().join("alias");
        std::os::unix::fs::symlink(&bin, &alias).unwrap();
        let named = alias.join("targo");
        assert_eq!(
            launcher(Some(&targo), Some(named.as_os_str()), Some(began)),
            Some(began),
            "$CARGO through a link"
        );
        assert_eq!(launcher(Some(&named), cargo, Some(began)), Some(began));
    }
    let gone = dir.path().join("gone");
    for (parent, named, start, what) in [
        (Some(other.as_path()), cargo, Some(began), "another program"),
        (None, cargo, Some(began), "its parent's executable unread"),
        (Some(targo.as_path()), None, Some(began), "no $CARGO"),
        (
            Some(targo.as_path()),
            Some(gone.as_os_str()),
            Some(began),
            "$CARGO names nothing",
        ),
        (
            Some(targo.as_path()),
            cargo,
            None,
            "its parent's start unread",
        ),
    ] {
        assert_eq!(launcher(parent, named, start), None, "{what}");
    }
    let (linked, placed, own) = (
        began - Duration::from_secs(60),
        began + Duration::from_secs(1),
        began + Duration::from_secs(2),
    );
    let judged = |parent: &Path| {
        since(
            linked,
            Some(placed),
            launcher(Some(parent), cargo, Some(began)),
            Some(own),
            None,
        )
    };
    assert_eq!(judged(&targo), Since::Launch(began));
    assert_eq!(
        judged(&other),
        Since::Link(linked),
        "another program's start"
    );
}

/// The kernel readers the hand-off and the run claim stand on: this process's ancestors
/// start at its parent, an ancestor's start is its record's and its image the inode of the
/// file it runs, and this process is no ancestor of itself. The test reruns itself as a
/// child, so the ancestor it reads runs this test binary, a file nothing relinks while it
/// runs; its own launcher's can be (`verify`'s test compile relinks the `aterm-verify` it
/// runs from). The launcher, which runs another file, shows the image read is the named
/// ancestor's and not the caller's.
#[cfg(target_os = "macos")]
#[test]
fn this_processs_ancestors_start_at_its_parent() {
    const PROBE: &str = "ATERM_RELEASE_ANCESTOR_PROBE";
    const NAME: &str = "cutter::tests::this_processs_ancestors_start_at_its_parent";
    let Some(parent_exe) = std::env::var_os(PROBE) else {
        let exe = std::env::current_exe().expect("this test executable");
        let mut probe = aterm_uds::spawnfd::command(&exe);
        probe
            .args([NAME, "--exact", "--nocapture", "--test-threads=1"])
            .env(PROBE, &exe);
        // A one-minute hang detector, not a latency budget.
        let out = aterm_uds::deadline::output_within(&mut probe, Duration::from_secs(60))
            .expect("the probe ran");
        let said = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && said.contains("ancestor probe held"),
            "{out:?}"
        );
        return;
    };
    let parent = std::os::unix::process::parent_id();
    let lineage = ancestors();
    assert_eq!(lineage.first(), Some(&parent), "{lineage:?}");
    let found = ancestor(parent).expect("the parent is an ancestor");
    assert_eq!(Some(found.start), started(parent));
    assert!(found.image.is_some(), "the parent's image reads");
    assert_eq!(found.image, inode(Path::new(&parent_exe)));
    let grand = *lineage.get(1).expect("a grandparent");
    let above = ancestor(grand).expect("the grandparent is an ancestor");
    assert_ne!(above.image, found.image, "the launcher runs another file");
    assert_eq!(ancestor(std::process::id()), None);
    println!("ancestor probe held");
}

/// A handed moment is read only beside a variable that names this executable, spelled
/// through a link or not, as [`since_value`] writes it, and only from the process it names
/// as the one that handed it when that is a live ancestor of this one, started when the value
/// says and running this very file. NEGATIVE CONTROLS: take the moment whatever process it
/// names, and one no ancestor handed is read; skip the image check, and an ancestor running
/// another executable hands its moment on.
#[cfg(unix)]
#[test]
fn a_handed_moment_is_read_only_for_this_executable() {
    let b = built();
    let at = SystemTime::UNIX_EPOCH + Duration::new(1_793_520_000, 7);
    let start = SystemTime::UNIX_EPOCH + Duration::new(1_793_519_000, 5_000);
    let value = since_value(at, 4242, start).unwrap();
    assert_eq!(value, "1793520000000000007 4242 1793519000000005000");
    let image = inode(&b.exe);
    assert!(image.is_some());
    let lineage = |pid: u32| (pid == 4242).then_some(Ancestor { start, image });
    let read = |named: Option<&Path>, since: Option<&str>| {
        handed_since(
            named.map(Path::as_os_str),
            since.map(OsStr::new),
            &b.exe,
            &lineage,
        )
    };
    assert_eq!(read(Some(&b.exe), Some(&value)), Some(at));
    let lane = b.exe.parent().unwrap().with_file_name("lane");
    std::os::unix::fs::symlink(b.exe.parent().unwrap(), &lane).unwrap();
    assert_eq!(
        read(Some(&lane.join("aterm-release")), Some(&value)),
        Some(at)
    );
    let other = b.exe.with_file_name("other");
    fs::write(&other, "linked").unwrap();
    let by = |pid: u32, start| since_value(at, pid, start).unwrap();
    let (gone, reused) = (by(4343, start), by(4242, start + Duration::from_secs(1)));
    for (named, since, what) in [
        (
            Some(other.as_path()),
            Some(value.as_str()),
            "another executable",
        ),
        (None, Some(value.as_str()), "no cutter named"),
        (Some(b.exe.as_path()), None, "no moment"),
        (
            Some(b.exe.as_path()),
            Some("1793520000000000007"),
            "no process that handed it",
        ),
        (
            Some(b.exe.as_path()),
            Some("1793520000.5 4242 1793519000000005000"),
            "not nanoseconds",
        ),
        (
            Some(b.exe.as_path()),
            Some(gone.as_str()),
            "a moment no ancestor handed",
        ),
        (
            Some(b.exe.as_path()),
            Some(reused.as_str()),
            "an ancestor that started at another time",
        ),
    ] {
        assert_eq!(read(named, since), None, "{what}");
    }
    let elsewhere = |pid: u32| {
        (pid == 4242).then_some(Ancestor {
            start,
            image: inode(&other),
        })
    };
    assert_eq!(
        handed_since(
            Some(b.exe.as_os_str()),
            Some(OsStr::new(&value)),
            &b.exe,
            &elsewhere
        ),
        None,
        "an ancestor running another executable"
    );
    assert_eq!(
        since_value(SystemTime::UNIX_EPOCH - Duration::from_secs(1), 4242, start),
        None
    );
}
