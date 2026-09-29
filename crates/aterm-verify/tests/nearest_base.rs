// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE NEAREST BASE IS OPT-IN, AND IT NEVER EXCUSES WHAT MAY HAVE CHANGED
//! (2026-09-28, `--nearest-base`, [`aterm_verify::nearest`]).
//!
//! Each case builds a real repository: a three-crate workspace (`a` builds on
//! `b` by path, `c` stands alone) whose main commit M0 carries a baseline
//! receipt listing a failed test of `a`, one of `c` and a whole-row red; main
//! then moves to M1 without a receipt, and a branch forks off M1. Every arm of
//! the rule is driven through [`differential::resolve_with`] exactly as a run
//! resolves its base, with a negative control beside it: a change inside the
//! crate, in a crate it builds on, or to `Cargo.lock` makes its red NEW; a
//! crate that names a path outside itself never qualifies; a base past the
//! bound is the absolute rule; a whole-row red is never inherited through it;
//! and without the flag the plan is the exact rule's, byte for byte.
//!
//! UNIX-PINNED, as `differential_soundness.rs` is: real repositories through
//! a shell-less `git`.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use aterm_verify::differential::{self, Against, BaseMode, Disposition, NewWhy, Plan, Tools};
use aterm_verify::ladder::Finding;
use aterm_verify::nearest;
use aterm_verify::receipt::{self, Failure, Receipt};

const TOOLCHAIN: &str = "/store/trust/9192/bin trustc 43f8b339fe0c";
const CHECKERS: &str = "ty = absent; trust-ir = absent; ay = absent";
const BUILD_CONFIG: &str = "repo/.cargo/config.toml=0123456789abcdef0123456789abcdef01234567";

const RED_A: &str = "-p a --test render -- grid::row_renders";
const RED_C: &str = "-p c --lib -- tests::c_is_red";
const ROW: &str = "tippy -D warnings (workspace, all targets)";
const HASH: &str = "0123456789abcdef";

fn tools() -> Tools {
    Tools {
        toolchain: TOOLCHAIN.into(),
        checkers: CHECKERS.into(),
        build_env: Some("none".into()),
        build_config: Some(BUILD_CONFIG.into()),
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn git(dir: &Path, args: &[&str]) -> String {
    git_at(dir, args, None)
}

/// `git` with a fixed committer date, when given.
fn git_at(dir: &Path, args: &[&str], when: Option<u64>) -> String {
    let mut c = Command::new("git");
    c.args([
        "-c",
        "commit.gpgsign=false",
        "-c",
        "core.hooksPath=/dev/null",
    ])
    .args(args)
    .current_dir(dir)
    .env("GIT_AUTHOR_NAME", "t")
    .env("GIT_AUTHOR_EMAIL", "t@t")
    .env("GIT_COMMITTER_NAME", "t")
    .env("GIT_COMMITTER_EMAIL", "t@t");
    if let Some(t) = when {
        c.env("GIT_COMMITTER_DATE", format!("@{t} +0000"))
            .env("GIT_AUTHOR_DATE", format!("@{t} +0000"));
    }
    let out = c.output().expect("git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Write `files` and commit them (at `when`, when given): the new commit.
fn commit(dir: &Path, files: &[(&str, &str)], when: Option<u64>) -> String {
    for (path, body) in files {
        let p = dir.join(path);
        std::fs::create_dir_all(p.parent().expect("a parent")).expect("mkdir");
        std::fs::write(p, body).expect("write");
    }
    git(dir, &["add", "-A"]);
    git_at(dir, &["commit", "-q", "-m", "c"], when);
    git(dir, &["rev-parse", "HEAD"])
}

/// Main on `origin` at `commit`, fetched, as `differential_soundness.rs`
/// builds it — so freshness is read from a real remote.
fn origin_main(root: &Path, commit: &str) {
    let bare = root.join(".git").join("test-origin.git");
    if !bare.exists() {
        let bare_s = bare.to_str().expect("utf-8");
        git(root, &["init", "-q", "--bare", bare_s]);
        git(root, &["remote", "add", "origin", bare_s]);
    }
    git(
        root,
        &[
            "push",
            "-q",
            "-f",
            "origin",
            &format!("{commit}:refs/heads/main"),
        ],
    );
    git(root, &["update-ref", "refs/remotes/origin/main", commit]);
}

/// The workspace at M0.
const WORKSPACE: [(&str, &str); 9] = [
    (
        "Cargo.toml",
        "[workspace]\nmembers = [\"crates/*\"]\nresolver = \"2\"\n",
    ),
    ("Cargo.lock", "version = 4\n"),
    (
        "crates/a/Cargo.toml",
        "[package]\nname = \"a\"\nversion = \"0.1.0\"\n\n[dependencies]\nb = { path = \
         \"../b\" }\n",
    ),
    ("crates/a/src/lib.rs", "pub fn a() -> u8 { b::b() }\n"),
    (
        "crates/b/Cargo.toml",
        "[package]\nname = \"b\"\nversion = \"0.1.0\"\n",
    ),
    ("crates/b/src/lib.rs", "pub fn b() -> u8 { 1 }\n"),
    (
        "crates/c/Cargo.toml",
        "[package]\nname = \"c\"\nversion = \"0.1.0\"\n",
    ),
    ("crates/c/src/lib.rs", "pub fn c() -> u8 { 3 }\n"),
    ("docs/NOTES.md", "notes\n"),
];

fn red(id: &str, since: &str, since_when: u64) -> Failure {
    Failure {
        id: id.into(),
        hash: HASH.into(),
        since: since.into(),
        since_when,
    }
}

/// M0's baseline receipt: `a`'s test, `c`'s test and a whole row, red for an
/// hour.
fn baseline(root: &Path, m0: &str, when: u64) -> Receipt {
    Receipt {
        head: m0.to_string(),
        tree: receipt::tree_of(root, m0),
        mode: "fast".into(),
        scope: "workspace".into(),
        verdict: "FAIL".into(),
        merge_contract: false,
        skipped: "none".into(),
        toolchain: TOOLCHAIN.into(),
        checkers: CHECKERS.into(),
        build_env: Some("none".into()),
        build_config: Some(BUILD_CONFIG.into()),
        failures: Some(vec![
            red(RED_A, m0, when),
            red(RED_C, m0, when),
            red(ROW, m0, when),
        ]),
        baseline: true,
        when,
        ..Receipt::default()
    }
}

/// The repository: main at M0 (receipted, committed `age` seconds ago), then
/// M1 = M0 plus `between` (no receipt), `origin/main` at M1, and HEAD one
/// commit off it on a branch — `(root, m0, m1, head)`.
fn repo(tag: &str, between: &[(&str, &str)], age: u64) -> (PathBuf, String, String, String) {
    let root = aterm_verify::mktemp_dir(tag).expect("mktemp");
    git(&root, &["init", "-q", "-b", "main"]);
    let t0 = now() - age;
    let m0 = commit(&root, &WORKSPACE, Some(t0));
    receipt::write(&root, &baseline(&root, &m0, t0)).expect("receipt");
    let m1 = commit(&root, between, None);
    origin_main(&root, &m1);
    git(&root, &["switch", "-q", "-c", "feature"]);
    let head = commit(&root, &[("branch.txt", "a change")], None);
    (root, m0, m1, head)
}

/// A failure the branch finds, the same as main's at M0.
fn again(id: &str, package: Option<&str>) -> Finding {
    Finding {
        id: id.into(),
        hash: HASH.into(),
        opaque: None,
        timing: None,
        package: package.map(str::to_string),
    }
}

/// Resolve with the nearest base, expecting M0: the reds as judged now.
fn nearest_reds(root: &Path, head: &str, m0: &str, m1: &str) -> differential::BaseReds {
    let plan = differential::resolve_with(root, head, false, &tools(), BaseMode::Nearest);
    let Plan::Judge(found) = &plan else {
        panic!("--nearest-base finds M0: {plan:?}");
    };
    assert_eq!(found.commit, m0);
    let gate = found.nearest.as_ref().expect("a nearest gate");
    assert_eq!(gate.merge_base, m1);
    assert_eq!(gate.steps, 1);
    let line = plan.header_line(false, head);
    assert!(
        line.contains("NEAREST (--nearest-base, opt-in)")
            && line.contains(&format!("the merge-base with origin/main, {}", &m1[..9]))
            && line.contains(&format!("{} is the newest", &m0[..9])),
        "{line}"
    );
    let Against::Base(reds) = plan.against(now()) else {
        panic!("judged against M0");
    };
    reds
}

/// THE CONTROL, AND WHAT NEVER CROSSES. Main moved only in `c` (and a doc):
/// `a`'s red, same failure, crate and its dependency untouched, is inherited
/// from M0 — and the receipt says `base-mode nearest`; `c`'s red is NEW,
/// because `c` changed; the whole-row red is NEW although it is main's with
/// the same fingerprint, because a row is inherited only from the exact
/// merge-base.
#[test]
fn a_test_whose_blast_radius_did_not_move_is_inherited_and_nothing_else() {
    let (root, m0, m1, head) = repo(
        "atv-near-ctl",
        &[
            ("crates/c/src/lib.rs", "pub fn c() -> u8 { 4 }\n"),
            ("docs/NOTES.md", "more notes\n"),
        ],
        3600,
    );
    let reds = nearest_reds(&root, &head, &m0, &m1);
    assert!(matches!(
        differential::judge(&again(RED_A, Some("a")), &reds),
        Disposition::Inherited(_)
    ));
    assert_eq!(
        differential::judge(&again(RED_C, Some("c")), &reds),
        Disposition::New(NewWhy::Opaque(nearest::RADIUS_CHANGED))
    );
    assert_eq!(
        differential::judge(&again(ROW, None), &reds),
        Disposition::New(NewWhy::Opaque(nearest::WHOLE_ROW))
    );
    // Through the same base by the exact rule, the row would have been main's:
    // the gate, not the fingerprint, is what made it NEW.
    let exact = differential::BaseReds {
        nearest: None,
        ..reds.clone()
    };
    assert!(differential::judge(&again(ROW, None), &exact).excused());
    std::fs::remove_dir_all(&root).ok();
}

/// A CHANGE INSIDE THE CRATE between M0 and M1: main may have fixed `a`'s
/// red there, so the branch's is NEW.
#[test]
fn a_change_inside_the_crate_makes_its_red_new() {
    let (root, m0, m1, head) = repo(
        "atv-near-own",
        &[("crates/a/src/lib.rs", "pub fn a() -> u8 { b::b() + 1 }\n")],
        3600,
    );
    let reds = nearest_reds(&root, &head, &m0, &m1);
    assert_eq!(
        differential::judge(&again(RED_A, Some("a")), &reds),
        Disposition::New(NewWhy::Opaque(nearest::RADIUS_CHANGED))
    );
    std::fs::remove_dir_all(&root).ok();
}

/// A CHANGE IN A CRATE IT BUILDS ON: `b` moved, and `a` is built on it.
#[test]
fn a_change_in_a_dependency_crate_makes_its_red_new() {
    let (root, m0, m1, head) = repo(
        "atv-near-dep",
        &[("crates/b/src/lib.rs", "pub fn b() -> u8 { 2 }\n")],
        3600,
    );
    let reds = nearest_reds(&root, &head, &m0, &m1);
    assert_eq!(
        differential::judge(&again(RED_A, Some("a")), &reds),
        Disposition::New(NewWhy::Opaque(nearest::RADIUS_CHANGED))
    );
    // `c` builds on neither: the control.
    assert!(differential::judge(&again(RED_C, Some("c")), &reds).excused());
    std::fs::remove_dir_all(&root).ok();
}

/// `Cargo.lock` CHANGED: every crate may build other code.
#[test]
fn a_changed_lockfile_makes_every_red_new() {
    let (root, m0, m1, head) = repo(
        "atv-near-lock",
        &[("Cargo.lock", "version = 4\n# moved\n")],
        3600,
    );
    let reds = nearest_reds(&root, &head, &m0, &m1);
    for (id, package) in [(RED_A, "a"), (RED_C, "c")] {
        assert_eq!(
            differential::judge(&again(id, Some(package)), &reds),
            Disposition::New(NewWhy::Opaque(nearest::WORKSPACE_CHANGED)),
            "{id}"
        );
    }
    std::fs::remove_dir_all(&root).ok();
}

/// FEATURE UNIFICATION, THE NEGATIVE CONTROL. `c` does not build on `a` or
/// `a` on `c`; `c` dev-depends on `b`, which `a` builds on, and between M0 and
/// M1 ONLY `c`'s manifest changed — to switch on a feature of `b`. The
/// resolver unifies it across the workspace, so `a`'s test compiles against a
/// different `b` with `Cargo.lock` unmoved: `a`'s red is NEW, not main's.
#[test]
fn a_feature_switched_on_from_outside_the_radius_makes_the_red_new() {
    let root = aterm_verify::mktemp_dir("atv-near-feat").expect("mktemp");
    git(&root, &["init", "-q", "-b", "main"]);
    let t0 = now() - 3600;
    let mut files = WORKSPACE.to_vec();
    files.retain(|(p, _)| !matches!(*p, "crates/b/Cargo.toml" | "crates/c/Cargo.toml"));
    files.push((
        "crates/b/Cargo.toml",
        "[package]\nname = \"b\"\nversion = \"0.1.0\"\n\n[features]\nx = []\n",
    ));
    files.push((
        "crates/c/Cargo.toml",
        "[package]\nname = \"c\"\nversion = \"0.1.0\"\n\n[dev-dependencies]\nb = { path = \
         \"../b\" }\n",
    ));
    let m0 = commit(&root, &files, Some(t0));
    receipt::write(&root, &baseline(&root, &m0, t0)).expect("receipt");
    let m1 = commit(
        &root,
        &[(
            "crates/c/Cargo.toml",
            "[package]\nname = \"c\"\nversion = \"0.1.0\"\n\n[dev-dependencies]\nb = { path = \
             \"../b\", features = [\"x\"] }\n",
        )],
        None,
    );
    origin_main(&root, &m1);
    git(&root, &["switch", "-q", "-c", "feature"]);
    let head = commit(&root, &[("branch.txt", "x")], None);
    let reds = nearest_reds(&root, &head, &m0, &m1);
    assert_eq!(
        differential::judge(&again(RED_A, Some("a")), &reds),
        Disposition::New(NewWhy::Opaque(nearest::WORKSPACE_CHANGED))
    );
    std::fs::remove_dir_all(&root).ok();
}

/// A SYMLINK OUT OF A CRATE — `crates/b/fix -> ../../docs/NOTES.md`, which
/// `git grep` never searches — makes `b` (and `a`, built on it) read outside
/// itself; `c` still qualifies.
#[test]
fn a_symlink_out_of_a_crate_never_qualifies() {
    let root = aterm_verify::mktemp_dir("atv-near-link").expect("mktemp");
    git(&root, &["init", "-q", "-b", "main"]);
    let t0 = now() - 3600;
    std::fs::create_dir_all(root.join("crates/b")).expect("mkdir");
    std::os::unix::fs::symlink("../../docs/NOTES.md", root.join("crates/b/fix")).expect("ln");
    let m0 = commit(&root, &WORKSPACE, Some(t0));
    assert!(git(&root, &["ls-files", "-s", "crates/b/fix"]).starts_with("120000 "));
    receipt::write(&root, &baseline(&root, &m0, t0)).expect("receipt");
    let m1 = commit(
        &root,
        &[("docs/NOTES.md", "the file b's link reads\n")],
        None,
    );
    origin_main(&root, &m1);
    git(&root, &["switch", "-q", "-c", "feature"]);
    let head = commit(&root, &[("branch.txt", "x")], None);
    let reds = nearest_reds(&root, &head, &m0, &m1);
    assert_eq!(
        differential::judge(&again(RED_A, Some("a")), &reds),
        Disposition::New(NewWhy::Opaque(nearest::READS_OUTSIDE))
    );
    assert!(differential::judge(&again(RED_C, Some("c")), &reds).excused());
    std::fs::remove_dir_all(&root).ok();
}

/// A CRATE THAT NAMES A PATH OUTSIDE ITSELF — here `b`, which `a` builds on,
/// reads `../../docs` in a test — never qualifies, even when nothing in the
/// dependency graph moved; `c` does not, and still qualifies.
#[test]
fn a_crate_reading_outside_its_directory_never_qualifies() {
    let root = aterm_verify::mktemp_dir("atv-near-out").expect("mktemp");
    git(&root, &["init", "-q", "-b", "main"]);
    let t0 = now() - 3600;
    let mut files = WORKSPACE.to_vec();
    files.push((
        "crates/b/tests/docs.rs",
        "#[test]\nfn reads() { std::fs::read(\"../../docs/NOTES.md\").unwrap(); }\n",
    ));
    let m0 = commit(&root, &files, Some(t0));
    receipt::write(&root, &baseline(&root, &m0, t0)).expect("receipt");
    let m1 = commit(&root, &[("docs/NOTES.md", "the file b reads\n")], None);
    origin_main(&root, &m1);
    git(&root, &["switch", "-q", "-c", "feature"]);
    let head = commit(&root, &[("branch.txt", "x")], None);
    let reds = nearest_reds(&root, &head, &m0, &m1);
    assert_eq!(
        differential::judge(&again(RED_A, Some("a")), &reds),
        Disposition::New(NewWhy::Opaque(nearest::READS_OUTSIDE))
    );
    assert!(differential::judge(&again(RED_C, Some("c")), &reds).excused());
    std::fs::remove_dir_all(&root).ok();
}

/// BEYOND THE BOUND. M0 is 30 h of committer time before M1: past the day,
/// so there is no nearest base and the run is judged by the absolute rule,
/// saying why. The commit bound, driven at one commit: M0 two commits back
/// is past it.
#[test]
fn a_base_beyond_the_bound_is_the_absolute_rule() {
    let (root, _m0, m1, head) = repo(
        "atv-near-far",
        &[("crates/c/src/lib.rs", "pub fn c() -> u8 { 4 }\n")],
        30 * 3600,
    );
    let plan = differential::resolve_with(&root, &head, false, &tools(), BaseMode::Nearest);
    let Plan::Absolute { why, .. } = &plan else {
        panic!("a base a day and more before the merge-base: {plan:?}");
    };
    assert!(
        why.contains("--nearest-base: no receipt that serves this run on the 0 main commit(s)")
            && why.contains("older than the bound"),
        "{why}"
    );

    let (root2, m0, m1b, _) = repo(
        "atv-near-steps",
        &[("crates/c/src/lib.rs", "pub fn c() -> u8 { 4 }\n")],
        3600,
    );
    // One more main commit: M0 is now two commits before the merge-base.
    git(&root2, &["switch", "-q", "main"]);
    let m2 = commit(&root2, &[("docs/NOTES.md", "again\n")], None);
    assert_ne!(m2, m1b);
    let tight = nearest::Bound {
        commits: 1,
        span_secs: nearest::NEAREST_SPAN_SECS,
    };
    let err = nearest::find(&root2, &m2, &tools(), tight).expect_err("past one commit");
    assert!(err.contains("on the 1 main commit(s)"), "{err}");
    let found = nearest::find(&root2, &m2, &tools(), nearest::Bound::DEFAULT).expect("inside");
    assert_eq!(
        (found.commit.as_str(), found.nearest.map(|g| g.steps)),
        (m0.as_str(), Some(2))
    );
    std::fs::remove_dir_all(&root).ok();
    std::fs::remove_dir_all(&root2).ok();
    let _ = m1;
}

/// OFF BY DEFAULT, BYTE FOR BYTE. Without the flag a run whose merge-base has
/// no receipt is judged by the absolute rule exactly as before — the same
/// plan `resolve` gives, the same words, nothing about a nearest base — and
/// with it, a merge-base that HAS a receipt is judged by it exactly as the
/// exact rule judges: the flag changes only the plan of a run with none.
#[test]
fn opt_in_off_is_the_exact_rule() {
    let (root, _m0, m1, head) = repo(
        "atv-near-off",
        &[("crates/c/src/lib.rs", "pub fn c() -> u8 { 4 }\n")],
        3600,
    );
    // Main's notes exist on origin (about another commit), so the fetch the
    // exact rule makes succeeds and the reason is fully determined.
    git(
        &root,
        &[
            "notes",
            "--ref=aterm-verify",
            "add",
            "-m",
            "not a receipt",
            &head,
        ],
    );
    git(
        &root,
        &[
            "push",
            "-q",
            "origin",
            "refs/notes/aterm-verify:refs/notes/aterm-verify",
        ],
    );
    let plan = differential::resolve(&root, &head, false, &tools());
    assert_eq!(
        plan,
        differential::resolve_with(&root, &head, false, &tools(), BaseMode::Exact)
    );
    let Plan::Absolute { why, chain } = &plan else {
        panic!("no receipt for the merge-base: the absolute rule: {plan:?}");
    };
    let m = &m1[..9];
    assert_eq!(
        why,
        &format!(
            "no usable receipt for the base {m} (the merge-base with origin/main): no receipt; \
             no published note either; `tools/verify.sh --baseline` on {m} records one"
        )
    );
    assert!(chain.as_ref().is_some_and(|c| c.nearest.is_none()));
    assert_eq!(
        plan.header_line(false, &head),
        format!("verify: base — {why}: every red counts (the absolute rule)\n")
    );

    // A merge-base with its own receipt: the flag changes nothing.
    let t1 = now() - 60;
    receipt::write(&root, &baseline(&root, &m1, t1)).expect("receipt");
    let exact = differential::resolve(&root, &head, false, &tools());
    let near = differential::resolve_with(&root, &head, false, &tools(), BaseMode::Nearest);
    assert_eq!(exact, near);
    let Plan::Judge(found) = &near else {
        panic!("M1's own receipt: {near:?}");
    };
    assert!(found.nearest.is_none() && found.commit == m1);
    assert!(!near.header_line(false, &head).contains("NEAREST"));
    std::fs::remove_dir_all(&root).ok();
}

/// THE RECEIPT. A run judged through a nearest base says so, `base-mode
/// nearest <B>` beside `base <B>`, and reads it back; one by the exact rule
/// writes no such line, so its text is what it always was.
#[test]
fn the_receipt_names_a_nearest_base_and_only_one() {
    let exact = Receipt {
        head: "h".repeat(40),
        mode: "fast".into(),
        scope: "workspace".into(),
        verdict: "PASS".into(),
        merge_contract: true,
        skipped: "none".into(),
        base: Some("b".repeat(40)),
        inherited: vec![RED_A.into()],
        when: 1,
        ..Receipt::default()
    };
    let text = exact.render();
    assert!(!text.contains("base-mode"), "{text}");
    assert_eq!(
        text,
        format!(
            "aterm-verify receipt 2\nhead {h}\nmode fast\nscope workspace\nverdict PASS\n\
             merge-contract yes\nskipped none\nbase {b}\ninherited {RED_A}\nwhen 1\n",
            h = "h".repeat(40),
            b = "b".repeat(40)
        )
    );
    let near = Receipt {
        base_nearest: Some("b".repeat(40)),
        ..exact
    };
    let text = near.render();
    assert!(
        text.contains(&format!(
            "base {b}\nbase-mode nearest {b}\n",
            b = "b".repeat(40)
        )),
        "{text}"
    );
    assert_eq!(Receipt::parse(&text), Some(near));
}
