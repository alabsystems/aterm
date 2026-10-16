// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHICH COMMIT THIS RUNNING CUTTER IS, judged once, as the process starts.
//!
//! A cutter judges a release by the rules compiled into it, so it acts for a commit only
//! as that commit's own build ([`crate::gates::own_cutter`]). Cargo writes a dep-info file
//! beside every binary it links (`target/<profile>/aterm-release.d`) naming each
//! first-party source the binary was compiled from. At its start this process is the
//! build of the checkout those sources sit in, at that checkout's `HEAD`, when:
//!
//! 1. that checkout is the one rustc compiled this crate in ([`BUILT_IN`]): cargo hands a
//!    checkout the binary another one linked into a target directory they share whenever
//!    its own files are older than that binary, and rewrites the dep-info to name it;
//! 2. no checkout source the dep-info names changed after the link (cargo's own freshness
//!    rule, so a binary built from an edit and run after the edit was reverted is refused:
//!    the revert is newer than the link);
//! 3. the checkout's cutter sources ([`SOURCE_INPUTS`]) are clean against its `HEAD`.
//!
//! The verdict is the start's because a release moves its clone to the commit it cuts: a
//! binary started at another commit stays that commit's build and re-runs as the new
//! one's (`release.rs`). Manifests and the lock file are not in the dep-info, so a binary
//! run directly after a manifest-only change is not seen; `targo --unverified ship` is a
//! `targo run`, which relinks first. Only this crate records where it was compiled, so
//! checkouts that share a target directory never build the cutter there
//! (`publish/pre-promote` builds in its own).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::SystemTime;

use crate::ledger::{GitCli, GitRunner};

/// Why this binary is not a commit's own cutter, and the one way to run one that is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotOwn {
    /// What the judgment found.
    pub why: String,
    /// The remedy: what to do, ending in what to run again.
    pub fix: String,
}

impl NotOwn {
    fn new(why: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            why: why.into(),
            fix: fix.into(),
        }
    }
}

/// What this running cutter is: the commit it is the build of, or why it is none.
pub type Stamp = Result<String, NotOwn>;

/// The checkout paths whose state decides the cutter binary: a superset of its
/// first-party build closure (every path dependency lives under `crates` or `vendor`).
pub const SOURCE_INPUTS: [&str; 6] = [
    "crates",
    "vendor",
    ".cargo",
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
];

/// The cutter's own crate, and its entry point, by which the dep-info names the checkout.
const OWN_CRATE: &str = "crates/aterm-release";
const OWN_MAIN: &str = "crates/aterm-release/src/main.rs";

/// Where rustc compiled this crate. Cargo does not fingerprint it, so a binary cargo
/// reuses from another checkout keeps that checkout's.
const BUILT_IN: &str = env!("CARGO_MANIFEST_DIR");

/// The remedy when cargo did not link this binary from its checkout as it stands.
const RELINK: &str = "run it as `targo --unverified ship <verb>` from a clean checkout of the \
                      commit it acts for, a `targo run` that relinks it from that tree first";

static RUNNING: OnceLock<Stamp> = OnceLock::new();

/// Judge this process. `main` calls it first, before any step moves a checkout.
pub fn judge_at_start() {
    RUNNING.get_or_init(|| match std::env::current_exe() {
        Ok(exe) => judge(&exe, Path::new(BUILT_IN)),
        Err(e) => Err(NotOwn::new(
            format!("it cannot name its own executable ({e})"),
            RELINK,
        )),
    });
}

/// This process's verdict, from [`judge_at_start`].
///
/// # Errors
/// This process is no commit's build, or was never judged.
pub fn running() -> Result<&'static str, &'static NotOwn> {
    RUNNING
        .get_or_init(|| Err(NotOwn::new("it was not judged when it started", RELINK)))
        .as_ref()
        .map(String::as_str)
}

/// The module's three rules for the executable `exe`, whose crate rustc compiled in
/// `built_in`.
fn judge(exe: &Path, built_in: &Path) -> Stamp {
    let linked = fs::metadata(exe)
        .and_then(|meta| meta.modified())
        .map_err(|e| NotOwn::new(format!("{} has no link time ({e})", exe.display()), RELINK))?;
    let dep_info = exe.with_extension("d");
    let text = fs::read_to_string(&dep_info).map_err(|e| {
        NotOwn::new(
            format!(
                "cannot read {} ({e}), the dep-info cargo writes beside a binary it links",
                dep_info.display()
            ),
            RELINK,
        )
    })?;
    let sources = dep_info_sources(&text, exe).ok_or_else(|| {
        NotOwn::new(
            format!("{} does not describe {}", dep_info.display(), exe.display()),
            RELINK,
        )
    })?;
    let root = checkout_of(&sources).ok_or_else(|| {
        NotOwn::new(
            format!("{} names no {OWN_MAIN}", dep_info.display()),
            RELINK,
        )
    })?;
    if !same_dir(&root.join(OWN_CRATE), built_in) {
        return Err(NotOwn::new(
            format!(
                "it was compiled in {}, not in {} whose sources its dep-info names (cargo \
                 reused another checkout's build from a target directory the two share)",
                built_in.display(),
                root.display()
            ),
            format!(
                "`targo clean{} -p aterm-release` in {} with the target directory that build \
                 used (cargo reuses the other checkout's binary until it is gone), then run it \
                 again",
                profile_flag(exe),
                root.display()
            ),
        ));
    }
    let target_root = exe.parent().and_then(Path::parent).unwrap_or(&root);
    let edited = checkout_sources(sources, &root, target_root);
    if let Some(why) = changed_after(&edited, linked) {
        return Err(NotOwn::new(why, RELINK));
    }
    let dirty: Vec<String> = crate::clean::dirty_against_head(&root)
        .ok_or_else(|| {
            NotOwn::new(
                format!("git could not list the state of {}", root.display()),
                format!(
                    "run `git status` in {} and mend what it reports, then run it again",
                    root.display()
                ),
            )
        })?
        .into_keys()
        .filter(|path| is_source_input(path))
        .collect();
    if !dirty.is_empty() {
        return Err(NotOwn::new(
            format!(
                "its checkout {} has uncommitted cutter sources ({})",
                root.display(),
                dirty.iter().take(3).cloned().collect::<Vec<_>>().join(", ")
            ),
            format!(
                "commit or remove what `git status` lists under {}'s cutter sources ({}), then \
                 run it again",
                root.display(),
                SOURCE_INPUTS.join(", ")
            ),
        ));
    }
    head(&GitCli::new(&root)).ok_or_else(|| {
        NotOwn::new(
            format!("{} has no HEAD commit", root.display()),
            format!(
                "check out a commit in {}, then run it again",
                root.display()
            ),
        )
    })
}

/// The same directory, spelled alike or resolving alike; a directory that is gone is
/// no match.
fn same_dir(a: &Path, b: &Path) -> bool {
    a == b
        || matches!(
            (fs::canonicalize(a), fs::canonicalize(b)),
            (Ok(a), Ok(b)) if a == b
        )
}

/// `targo clean`'s spelling of the profile whose directory holds `exe`.
fn profile_flag(exe: &Path) -> String {
    match exe
        .parent()
        .and_then(Path::file_name)
        .and_then(|n| n.to_str())
    {
        Some("debug") | None => String::new(),
        Some("release") => " --release".to_string(),
        Some(profile) => format!(" --profile {profile}"),
    }
}

/// `HEAD`'s commit id, when it is a full one.
fn head(git: &dyn GitRunner) -> Option<String> {
    let out = git
        .git(&["rev-parse", "--verify", "-q", "HEAD^{commit}"])
        .ok()
        .filter(crate::ledger::RunOut::success)?;
    let commit = out.stdout_utf8().trim().to_string();
    aterm_codec::hex::is_lower(&commit, 40).then_some(commit)
}

/// The sources a cargo dep-info file names for `target`: Makefile rules, one per line,
/// `\ ` escaping a space in a path.
fn dep_info_sources(text: &str, target: &Path) -> Option<Vec<PathBuf>> {
    let same = |a: &Path| a == target || fs::canonicalize(a).ok() == fs::canonicalize(target).ok();
    text.lines().find_map(|line| {
        let words = split_escaped(line);
        let (first, rest) = words.split_first()?;
        let out = first.strip_suffix(':')?;
        same(Path::new(out)).then(|| rest.iter().map(PathBuf::from).collect())
    })
}

/// A dep-info line's words: split at spaces, except a backslash-escaped one.
fn split_escaped(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&' ') => {
                word.push(' ');
                chars.next();
            }
            ' ' => {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            }
            _ => word.push(c),
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

/// The checkout whose [`OWN_MAIN`] is among `sources`.
fn checkout_of(sources: &[PathBuf]) -> Option<PathBuf> {
    sources
        .iter()
        .find(|source| source.ends_with(OWN_MAIN))
        .and_then(|main| main.ancestors().nth(4))
        .map(Path::to_path_buf)
}

/// The sources a person edits: inside the checkout, outside its `.git` and outside the
/// binary's target root. A build script's `OUT_DIR` output (under the target or build
/// directory) is rewritten by builds that need not relink this binary.
fn checkout_sources(sources: Vec<PathBuf>, root: &Path, target_root: &Path) -> Vec<PathBuf> {
    sources
        .into_iter()
        .filter(|source| {
            source.starts_with(root)
                && !source.starts_with(target_root)
                && !source.components().any(|c| c.as_os_str() == ".git")
        })
        .collect()
}

/// The first source modified after `linked`, or gone, said as the refusal.
fn changed_after(sources: &[PathBuf], linked: SystemTime) -> Option<String> {
    sources.iter().find_map(
        |source| match fs::metadata(source).and_then(|meta| meta.modified()) {
            Ok(modified) if modified <= linked => None,
            Ok(_) => Some(format!(
                "{} changed after this cutter was linked",
                source.display()
            )),
            Err(_) => Some(format!(
                "{} is gone since this cutter was linked",
                source.display()
            )),
        },
    )
}

/// Is the repository path `path` one of [`SOURCE_INPUTS`] or beneath one?
fn is_source_input(path: &str) -> bool {
    SOURCE_INPUTS.iter().any(|input| {
        path.strip_prefix(input)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::testing;
    use std::time::Duration;

    /// A checkout with a cutter source and a doc committed, and a linked "binary"
    /// beside it whose dep-info names that source.
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
        // Linked after its source, as cargo links.
        let source_time = fs::metadata(work.join(OWN_MAIN))
            .unwrap()
            .modified()
            .unwrap();
        set_modified(&exe, source_time + Duration::from_secs(5));
        Built {
            _dir: dir,
            work,
            exe,
            head,
        }
    }

    fn judged(b: &Built) -> Stamp {
        judge(&b.exe, &b.work.join(OWN_CRATE))
    }

    #[test]
    fn a_fresh_build_of_a_clean_checkout_is_its_heads_cutter() {
        let b = built();
        assert_eq!(judged(&b), Ok(b.head.clone()));
        // a doc is not a cutter source: editing one changes nothing
        fs::write(b.work.join("README.md"), "edited\n").unwrap();
        assert_eq!(judged(&b), Ok(b.head.clone()));
    }

    /// The revert case: a source newer than the link means the binary may hold what the
    /// source no longer says, so it is no commit's cutter until cargo relinks it.
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
        assert_eq!(not.fix, RELINK);
        fs::remove_file(b.work.join(OWN_MAIN)).unwrap();
        let not = judged(&b).expect_err("gone");
        assert!(not.why.contains("is gone"), "{not:?}");
    }

    #[test]
    fn an_uncommitted_cutter_source_refuses() {
        let b = built();
        let linked = fs::metadata(&b.exe).unwrap().modified().unwrap();
        fs::write(b.work.join("Cargo.lock"), "# not committed\n").unwrap();
        set_modified(&b.work.join("Cargo.lock"), linked - Duration::from_secs(60));
        let not = judged(&b).expect_err("dirty");
        assert!(
            not.why.contains("uncommitted cutter sources (Cargo.lock)"),
            "{not:?}"
        );
        assert!(not.fix.contains("git status"), "{not:?}");
    }

    /// Cargo hands a checkout the binary another one linked into a shared target directory
    /// and rewrites the dep-info to name it: only where rustc compiled the crate tells.
    /// The same checkout under another spelling (a symlinked path) is the same checkout.
    #[test]
    fn a_binary_compiled_in_another_checkout_refuses_until_it_is_cleaned() {
        let b = built();
        let not = judge(&b.exe, Path::new("/elsewhere/crates/aterm-release")).expect_err("reused");
        assert!(not.why.contains("compiled in /elsewhere/"), "{not:?}");
        assert!(
            not.fix
                .starts_with("`targo clean --profile gate -p aterm-release` in "),
            "{not:?}"
        );
        #[cfg(unix)]
        {
            let alias = b.work.with_file_name("alias");
            std::os::unix::fs::symlink(&b.work, &alias).unwrap();
            assert_eq!(judge(&b.exe, &alias.join(OWN_CRATE)), Ok(b.head.clone()));
        }
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

    /// The verdict is the start's: nothing judges the checkout as it stands later, after a
    /// release may have moved it, so a process `main` did not judge is no commit's build.
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
            Path::new("/r/s/target"),
        );
        assert_eq!(kept, [PathBuf::from("/r/s/crates/a.rs")]);
        assert!(is_source_input("crates/a/b.rs") && is_source_input("Cargo.lock"));
        assert!(!is_source_input("cratesx/a.rs") && !is_source_input("docs/a.md"));
    }
}
