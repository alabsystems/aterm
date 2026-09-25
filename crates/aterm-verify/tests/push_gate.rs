// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE PUSH GATE, MEASURED RATHER THAN DESCRIBED.
//!
//! `.githooks/pre-push` is the only thing between an ungated commit and
//! `origin/main` — aterm has no CI by owner decision. It has been wrong about
//! itself twice: the gate announced it as "pre-push L0 gate active" while the
//! hook ran nothing, and then twenty files were edited to say "ADVISORY"
//! instead of closing the gap. Both times the words were maintained by hand and
//! nothing compared them with the file.
//!
//! So this file RUNS THE HOOK. Every law below is a real `bash .githooks/pre-push`
//! against a real receipt directory, and the last one holds
//! [`aterm_verify::HOOK_CLAIM`] — the sentence the gate prints to every fresh
//! clone — against what the hook was just measured doing.
#![cfg(unix)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use aterm_verify::receipt::{self, Receipt};

const ZERO: &str = "0000000000000000000000000000000000000000";

/// A git checkout with one commit, the repo's own hook, and a receipts dir.
struct Push {
    root: PathBuf,
    sha: String,
}

impl Push {
    fn new(name: &str) -> Self {
        let root = aterm_verify::mktemp_dir(name)
            .expect("scratch")
            .join("repo");
        fs::create_dir_all(&root).expect("mkdir");
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .current_dir(&root)
                .args([
                    "-c",
                    "user.name=gate",
                    "-c",
                    "user.email=gate@example.invalid",
                    "-c",
                    "commit.gpgsign=false",
                    "-c",
                    "init.defaultBranch=main",
                ])
                .args(args)
                .output()
                .expect("git runs");
            assert!(out.status.success(), "git {args:?}");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        fs::write(root.join("a.txt"), "one\n").expect("write");
        git(&["init", "-q"]);
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "one"]);
        let sha = git(&["rev-parse", "HEAD"]);
        Self { root, sha }
    }

    /// The hook this repository actually ships, not a copy of it.
    fn hook() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.githooks/pre-push")
            .canonicalize()
            .expect("the repo ships .githooks/pre-push")
    }

    fn receipt(&self, r: &Receipt) {
        receipt::write(&self.root, r).expect("the receipt is written");
    }

    /// One more commit on the current branch touching exactly `files`
    /// (each written with fresh content): its sha.
    fn commit(&self, files: &[&str]) -> String {
        for f in files {
            let path = self.root.join(f);
            let prior = fs::read_to_string(&path).unwrap_or_default();
            fs::write(&path, format!("{prior}{f} moved\n")).expect("write");
        }
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .current_dir(&self.root)
                .args([
                    "-c",
                    "user.name=gate",
                    "-c",
                    "user.email=gate@example.invalid",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .output()
                .expect("git runs");
            assert!(out.status.success(), "git {args:?}");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "next"]);
        git(&["rev-parse", "HEAD"])
    }

    /// Run git in the fixture with the identity the fixture commits under.
    fn git(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .current_dir(&self.root)
            .args([
                "-c",
                "user.name=gate",
                "-c",
                "user.email=gate@example.invalid",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// The gated-merge shape: the remote's tip `tip` moved past the base on
    /// `main` (a peer's push), a side branch off the base carries the gated
    /// commit `gated`, and `merge` is git's own `--no-ff` merge of the side
    /// branch into `main`. Returns `(tip, gated, merge)`.
    fn peer_tip_and_gated_merge(&self) -> (String, String, String) {
        let base = self.sha.clone();
        let tip = self.commit(&["a.txt"]);
        self.git(&["checkout", "-q", "-b", "side", &base]);
        let gated = self.commit(&["b.txt"]);
        self.git(&["checkout", "-q", "main"]);
        self.git(&["merge", "-q", "--no-ff", "--no-edit", "side"]);
        let merge = self.git(&["rev-parse", "HEAD"]);
        (tip, gated, merge)
    }

    /// Push `sha` on `main` into an empty remote ref: `(exit code, stderr)`.
    fn push(&self, sha: &str, bypass: bool) -> (i32, String) {
        self.push_line(
            &format!("refs/heads/main {sha} refs/heads/main {ZERO}\n"),
            bypass,
        )
    }

    /// Run the hook on exactly this ref line — the four fields git hands a
    /// pre-push hook: local ref, local sha, remote ref, remote sha.
    fn push_line(&self, line: &str, bypass: bool) -> (i32, String) {
        self.push_line_in(line, bypass, &self.root, &[])
    }

    /// [`Self::push_line`] from `cwd`, with `extra` in the hook's environment —
    /// for the laws about a hook that cannot resolve its repository.
    fn push_line_in(
        &self,
        line: &str,
        bypass: bool,
        cwd: &Path,
        extra: &[(&str, &str)],
    ) -> (i32, String) {
        let mut c = Command::new("bash");
        c.arg(Self::hook())
            .arg("origin")
            .arg("git@example.invalid:x/y.git")
            .current_dir(cwd)
            .env_remove("ATERM_PUSH_NO_GATE")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if bypass {
            c.env("ATERM_PUSH_NO_GATE", "1");
        }
        for (k, v) in extra {
            c.env(k, v);
        }
        let mut child = c.spawn().expect("the hook runs");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(line.as_bytes())
            .expect("the hook reads its ref list");
        let out = child.wait_with_output().expect("the hook exits");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

fn green(sha: &str) -> Receipt {
    Receipt {
        head: sha.to_string(),
        mode: "fast".into(),
        scope: "workspace".into(),
        verdict: "PASS".into(),
        merge_contract: true,
        skipped: "none".into(),
        when: 1_758_000_000,
    }
}

/// A clean, unskipped `--changed` PASS: everything a narrowed run can say.
fn changed_pass(sha: &str) -> Receipt {
    Receipt {
        scope: "changed".into(),
        merge_contract: false,
        ..green(sha)
    }
}

/// THE RELEASE CUTTER'S CLAIM. `crates/aterm-release` claims a build number by
/// pushing "release: vX.Y.Z (build N)" from its cut tree: origin's tip plus
/// one `RELEASES.ledger` line and the rolled `CHANGELOG.md`, and nothing else
/// (`ledger::claim`). No gate ran on that commit and none needs to — no code
/// enters — so the hook admits it, judged against the remote's CURRENT tip, the
/// sha git hands the hook.
#[test]
fn a_release_claim_over_the_remote_tip_is_admitted_without_a_receipt() {
    let p = Push::new("atv-push-claim-admitted");
    let tip = p.sha.clone();
    let claim = p.commit(&["CHANGELOG.md", "RELEASES.ledger"]);
    let (code, err) = p.push_line(
        &format!("refs/heads/main {claim} refs/heads/main {tip}\n"),
        false,
    );
    assert_eq!(code, 0, "a claim over the remote's tip is admitted: {err}");
    assert!(
        err.contains("bookkeeping over the remote's tip") && err.contains("RELEASES.ledger"),
        "…and the hook says what it admitted and why: {err}"
    );
}

/// THE CLAIM A CUT LANDS ON A MAIN THAT MOVED (2026-09-23). The cutter builds
/// the PUBLISHED commit; when peers pushed after `pub publish`, its release
/// commit (the published commit + the two claim files) reaches main as a merge
/// onto the tip, whose tree is the tip's plus the same two files
/// (`ledger::landing_commit`). Against the remote's tip that merge moves only
/// `CHANGELOG.md` and `RELEASES.ledger`, so the hook admits it as a claim; the
/// peer's code it sits on is already on the remote. NEGATIVE CONTROL: the same
/// merge carrying a code change too is refused.
#[test]
fn a_release_claim_landing_as_a_merge_onto_a_moved_tip_is_admitted() {
    let p = Push::new("atv-push-claim-merge");
    let published = p.sha.clone();
    let tip = p.commit(&["a.txt"]); // a peer's push after the publish
    p.git(&["checkout", "-q", "-b", "release", &published]);
    let release = p.commit(&["CHANGELOG.md", "RELEASES.ledger"]);
    p.git(&["checkout", "-q", "main"]);
    for f in ["CHANGELOG.md", "RELEASES.ledger"] {
        let body = p.git(&["show", &format!("{release}:{f}")]);
        fs::write(p.root.join(f), format!("{body}\n")).expect("write");
    }
    p.git(&["add", "CHANGELOG.md", "RELEASES.ledger"]);
    let tree = p.git(&["write-tree"]);
    let landed = p.git(&[
        "commit-tree",
        &tree,
        "-p",
        &tip,
        "-p",
        &release,
        "-m",
        "lands",
    ]);
    let (code, err) = p.push_line(
        &format!("refs/heads/main {landed} refs/heads/main {tip}\n"),
        false,
    );
    assert_eq!(
        code, 0,
        "a merge-shaped claim over the tip is admitted: {err}"
    );
    assert!(err.contains("bookkeeping over the remote's tip"), "{err}");

    fs::write(p.root.join("a.txt"), "code the merge sneaks in\n").expect("write");
    p.git(&["add", "a.txt"]);
    let tree = p.git(&["write-tree"]);
    let sneaky = p.git(&[
        "commit-tree",
        &tree,
        "-p",
        &tip,
        "-p",
        &release,
        "-m",
        "sneaks",
    ]);
    let (code, err) = p.push_line(
        &format!("refs/heads/main {sneaky} refs/heads/main {tip}\n"),
        false,
    );
    assert_eq!(code, 1, "code in the merge owes a receipt: {err}");
}

/// The claim's shape is the whole admission: one more path in that diff is
/// code entering main, and code owes a receipt.
#[test]
fn a_claim_shaped_commit_that_also_moves_code_is_refused() {
    let p = Push::new("atv-push-claim-with-code");
    let tip = p.sha.clone();
    let claim = p.commit(&["CHANGELOG.md", "RELEASES.ledger", "a.txt"]);
    let (code, err) = p.push_line(
        &format!("refs/heads/main {claim} refs/heads/main {tip}\n"),
        false,
    );
    assert_eq!(code, 1, "a.txt moved too, so the push is refused: {err}");
    assert!(err.contains("no gate receipt"), "{err}");
}

/// Judged against the REMOTE'S tip, not the local parent: a ledger-only
/// commit that does not fast-forward what the remote holds is not the
/// cutter's claim (the claim is a compare-and-swap on origin's tip and
/// regenerates on a lost race), and is refused like any other commit.
#[test]
fn a_ledger_only_commit_that_does_not_fast_forward_the_remote_is_refused() {
    let p = Push::new("atv-push-claim-diverged");
    let remote_tip = p.commit(&["a.txt"]);
    // Back to the first commit, on a side branch; the "claim" hangs off it.
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .current_dir(&p.root)
            .args(args)
            .output()
            .expect("git runs");
        assert!(out.status.success(), "git {args:?}");
    };
    git(&["checkout", "-q", "-b", "side", &p.sha]);
    let claim = p.commit(&["CHANGELOG.md", "RELEASES.ledger"]);
    let (code, err) = p.push_line(
        &format!("refs/heads/side {claim} refs/heads/main {remote_tip}\n"),
        false,
    );
    assert_eq!(code, 1, "not a fast-forward of the remote's tip: {err}");
}

/// A SLOW GATE ON A FAST-MOVING MAIN. The gate takes over an hour and peers
/// push every few minutes, so a receipt for the exact remote tip is a race the
/// gate loses by construction. What is admitted instead is git's own automatic
/// merge of a receipted commit onto the remote's current tip — the merge's
/// tree byte-equal to `git merge-tree` of its two parents, so nothing was
/// resolved or added by hand — and the receipted side's receipt stands for it.
#[test]
fn a_clean_automatic_merge_of_a_receipted_commit_onto_the_remote_tip_is_admitted() {
    let p = Push::new("atv-push-merge-admitted");
    let (tip, gated, merge) = p.peer_tip_and_gated_merge();
    p.receipt(&green(&gated));
    let (code, err) = p.push_line(
        &format!("refs/heads/main {merge} refs/heads/main {tip}\n"),
        false,
    );
    assert_eq!(
        code, 0,
        "the merge is admitted on the gated side's receipt: {err}"
    );
    assert!(
        err.contains("automatic merge") && err.contains(&gated[..7]),
        "…and the hook names the gated side it stood on: {err}"
    );
}

/// …and the gated side must be FULLY receipted: a merge standing on a
/// narrowed run is refused, even with a full receipt on the base under it.
#[test]
fn a_clean_merge_of_a_narrowly_receipted_commit_is_refused() {
    let p = Push::new("atv-push-merge-changed");
    let base = p.sha.clone();
    let (tip, gated, merge) = p.peer_tip_and_gated_merge();
    p.receipt(&green(&base));
    p.receipt(&changed_pass(&gated));
    let (code, err) = p.push_line(
        &format!("refs/heads/main {merge} refs/heads/main {tip}\n"),
        false,
    );
    assert_eq!(code, 1, "the gated side ran only a narrowed gate: {err}");
    assert!(err.contains("no gate receipt"), "{err}");
}

/// The receipted side is the whole admission: the same merge with no receipt
/// on its side is a merge of two ungated commits, and is refused.
#[test]
fn the_same_merge_with_an_unreceipted_side_is_refused() {
    let p = Push::new("atv-push-merge-unreceipted");
    let (tip, _gated, merge) = p.peer_tip_and_gated_merge();
    let (code, err) = p.push_line(
        &format!("refs/heads/main {merge} refs/heads/main {tip}\n"),
        false,
    );
    assert_eq!(code, 1, "no receipt on either side: {err}");
    assert!(err.contains("no gate receipt"), "{err}");
}

/// Byte-equal to git's own result, or nothing: a merge commit that also
/// carries an edit of its own — a hand-resolved conflict has the same shape —
/// holds content nobody gated, and is refused.
#[test]
fn a_merge_that_adds_anything_beyond_gits_own_result_is_refused() {
    let p = Push::new("atv-push-merge-hand-edited");
    let (tip, gated, _merge) = p.peer_tip_and_gated_merge();
    p.receipt(&green(&gated));
    fs::write(p.root.join("c.txt"), "by hand\n").expect("write");
    p.git(&["add", "-A"]);
    p.git(&["commit", "-q", "--amend", "--no-edit"]);
    let amended = p.git(&["rev-parse", "HEAD"]);
    let (code, err) = p.push_line(
        &format!("refs/heads/main {amended} refs/heads/main {tip}\n"),
        false,
    );
    assert_eq!(
        code, 1,
        "c.txt is not in git's merge of the two parents: {err}"
    );
    assert!(err.contains("no gate receipt"), "{err}");
}

/// Onto the REMOTE'S tip, not onto anything: a merge whose parents do not
/// include what the remote holds now would carry the remote's own commits
/// past the hook as if they were the gated side's.
#[test]
fn a_merge_that_does_not_sit_on_the_remote_tip_is_refused() {
    let p = Push::new("atv-push-merge-off-tip");
    let base = p.sha.clone();
    let (_tip, gated, merge) = p.peer_tip_and_gated_merge();
    p.receipt(&green(&gated));
    // The remote still holds the base: neither parent of the merge is it.
    let (code, err) = p.push_line(
        &format!("refs/heads/main {merge} refs/heads/main {base}\n"),
        false,
    );
    assert_eq!(code, 1, "neither parent is the remote's tip: {err}");
}

/// A NARROWED PASS ADMITS NOTHING, even stacked on a fully receipted parent.
/// The `--changed` cone is the change's dependency closure, and other crates'
/// tests read files no dependency edge names (aterm-census reads aterm-gui's
/// app_render.rs; aterm-release reads tools/ and CHANGELOG.md), so its PASS
/// is not a claim about the tree the push sends. The control: the same
/// commit with a whole-tree receipt goes.
#[test]
fn a_change_scoped_pass_admits_nothing_even_over_a_fully_receipted_parent() {
    let p = Push::new("atv-push-changed");
    p.receipt(&green(&p.sha));
    let head = p.commit(&["b.txt"]);
    p.receipt(&changed_pass(&head));
    let (code, err) = p.push(&head, false);
    assert_eq!(code, 1, "{err}");
    assert!(
        err.contains("did NOT discharge the merge contract") && err.contains("scope changed"),
        "the refusal names the narrowed scope: {err}"
    );
    assert!(err.contains("tools/verify.sh --fast"), "{err}");
    assert!(
        !err.contains("--changed"),
        "the remedy offers no narrowed run: {err}"
    );

    p.receipt(&green(&head));
    let (code, err) = p.push(&head, false);
    assert_eq!(code, 0, "{err}");
}

/// ONE RECEIPT STORE PER REPOSITORY (2026-09-23). A gate run in one worktree
/// admits the push from another, because both resolve the same git common dir
/// — until then each checkout kept its own `.aterm-verify/receipts/` and a
/// PASS never reached the checkout that pushed. The negative control is a
/// receipt for a different commit, which admits nothing.
#[test]
fn a_receipt_written_in_one_worktree_admits_the_push_from_another() {
    let p = Push::new("atv-push-worktree");
    let linked = p.root.parent().expect("scratch").join("linked");
    p.git(&[
        "worktree",
        "add",
        "-q",
        linked.to_str().expect("utf-8"),
        "-b",
        "side",
    ]);
    receipt::write(&linked, &green(&"f".repeat(40))).expect("written from the linked worktree");
    let (code, err) = p.push(&p.sha, false);
    assert_eq!(
        code, 1,
        "a receipt for another commit admits nothing: {err}"
    );

    receipt::write(&linked, &green(&p.sha)).expect("written from the linked worktree");
    let (code, err) = p.push(&p.sha, false);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("carry a passing gate receipt"), "{err}");
}

/// A PURE VERSION BUMP OVER THE REMOTE'S TIP IS BOOKKEEPING (2026-09-23): the
/// workspace version moved in Cargo.toml and every member's Cargo.lock entry,
/// one old value to one new one, and nothing else. The negative controls: the
/// same bump carrying a third-party version change, and the same bump
/// carrying code.
#[test]
fn a_pure_version_bump_over_the_remote_tip_owes_no_receipt() {
    let manifest = |v: &str| {
        format!("[workspace]\nmembers = [\"a\"]\n\n[workspace.package]\nversion = \"{v}\"\n")
    };
    let lock = |v: &str, dep: &str| {
        format!(
            "version = 4\n\n[[package]]\nname = \"a\"\nversion = \"{v}\"\n\n[[package]]\n\
             name = \"b\"\nversion = \"{v}\"\n\n[[package]]\nname = \"dep\"\nversion = \"{dep}\"\n"
        )
    };
    let setup = |name: &str| {
        let p = Push::new(name);
        fs::write(p.root.join("Cargo.toml"), manifest("0.91.0")).expect("write");
        fs::write(p.root.join("Cargo.lock"), lock("0.91.0", "1.0.0")).expect("write");
        p.git(&["add", "-A"]);
        p.git(&["commit", "-q", "-m", "base"]);
        let tip = p.git(&["rev-parse", "HEAD"]);
        (p, tip)
    };
    let bump = |p: &Push, dep: &str, code_too: bool| {
        fs::write(p.root.join("Cargo.toml"), manifest("0.92.0")).expect("write");
        fs::write(p.root.join("Cargo.lock"), lock("0.92.0", dep)).expect("write");
        if code_too {
            fs::write(p.root.join("a.txt"), "code\n").expect("write");
        }
        p.git(&["add", "-A"]);
        p.git(&["commit", "-q", "-m", "bump"]);
        p.git(&["rev-parse", "HEAD"])
    };

    let (p, tip) = setup("atv-push-bump");
    let bumped = bump(&p, "1.0.0", false);
    let (code, err) = p.push_line(
        &format!("refs/heads/main {bumped} refs/heads/main {tip}\n"),
        false,
    );
    assert_eq!(code, 0, "a pure version bump is bookkeeping: {err}");
    assert!(err.contains("workspace version"), "{err}");

    let (p, tip) = setup("atv-push-bump-dep");
    let bumped = bump(&p, "1.0.1", false);
    let (code, err) = p.push_line(
        &format!("refs/heads/main {bumped} refs/heads/main {tip}\n"),
        false,
    );
    assert_eq!(code, 1, "a dependency moved too: {err}");
    assert!(err.contains("no gate receipt"), "{err}");

    let (p, tip) = setup("atv-push-bump-code");
    let bumped = bump(&p, "1.0.0", true);
    let (code, err) = p.push_line(
        &format!("refs/heads/main {bumped} refs/heads/main {tip}\n"),
        false,
    );
    assert_eq!(code, 1, "code moved too: {err}");
    assert!(err.contains("no gate receipt"), "{err}");
}

/// A tag moves no branch: the commit it names either already sits on the
/// remote or arrives through a branch push this hook judges. The cutter pushes
/// its fence, its lease and the vX.Y.0 tag from this checkout.
#[test]
fn a_tag_push_needs_no_receipt() {
    let p = Push::new("atv-push-tag");
    let (code, err) = p.push_line(
        &format!("refs/tags/v0.0.1 {} refs/tags/v0.0.1 {ZERO}\n", p.sha),
        false,
    );
    assert_eq!(code, 0, "a tag owes no receipt: {err}");
    assert!(!err.contains("REFUSED"), "{err}");
}

/// THE GAP THIS CLOSES. A commit no gate ever ran on does not leave.
#[test]
fn a_commit_with_no_receipt_is_refused_and_told_how_to_get_one() {
    let p = Push::new("atv-push-none");
    let (code, err) = p.push(&p.sha, false);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("no gate receipt for this commit"), "{err}");
    assert!(
        err.contains("tools/verify.sh --fast"),
        "the remedy is the command, not an adjective: {err}"
    );
    assert!(
        err.contains("ATERM_PUSH_NO_GATE=1"),
        "and the escape is named out loud: {err}"
    );
}

/// A whole-tree PASS on exactly these bytes, and the push goes.
#[test]
fn a_clean_merge_contract_receipt_admits_the_push() {
    let p = Push::new("atv-push-green");
    p.receipt(&green(&p.sha));
    let (code, err) = p.push(&p.sha, false);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("carry a passing gate receipt"), "{err}");
}

/// GIT'S CHATTER IS NOT THE ANSWER. For one day this hook read the repository
/// root as `$(git rev-parse --show-toplevel 2>&1)`, folding stderr into the
/// VALUE — and git writes to stderr while SUCCEEDING whenever a `GIT_TRACE`
/// variable is exported. Measured 2026-09-21 before the fix: with `GIT_TRACE=1`
/// the command exits 0 and prints the right path, `$root` becomes the trace
/// lines plus the path, `[ -d "$root" ]` is false, and a push whose commit
/// carries a PASSING receipt is REFUSED with "the repository could not be
/// resolved" about a repository git resolved perfectly. Fail-closed, so nothing
/// ungated ever escaped — but this hook's whole contract is that a refusal
/// NAMES what it could not check, and here it named something that was not so.
#[test]
fn git_chatter_on_a_successful_command_does_not_become_the_repository_root() {
    for var in ["GIT_TRACE", "GIT_TRACE2", "GIT_TRACE_PERFORMANCE"] {
        let p = Push::new("atv-push-trace");
        p.receipt(&green(&p.sha));
        let line = format!("refs/heads/main {} refs/heads/main {ZERO}\n", p.sha);
        let (code, err) = p.push_line_in(&line, false, &p.root, &[(var, "1")]);
        assert_eq!(
            code, 0,
            "{var}=1 makes git chatty on stderr while succeeding; the push still \
             carries a passing receipt and must be admitted: {err}"
        );
        assert!(err.contains("carry a passing gate receipt"), "{var}: {err}");
        assert!(
            !err.contains("could not be resolved"),
            "{var}: the hook must not diagnose a healthy repository as unresolvable: {err}"
        );
    }
}

/// AN OLDER GATE'S RECEIPT ADMITS NOTHING, and the refusal says so in one
/// sentence: format 1 carried `tree dirty …` receipts that format 2 never
/// writes, so a format-1 file cannot be read as today's claim. The control is
/// the same commit with a format-2 receipt, which goes.
#[test]
fn a_receipt_in_an_older_format_is_refused_in_one_sentence() {
    let p = Push::new("atv-push-format-1");
    let dir = receipt::dir(&p.root).expect("the store");
    fs::create_dir_all(&dir).expect("mkdir");
    fs::write(
        dir.join(&p.sha),
        format!(
            "aterm-verify receipt 1\nhead {}\ntree clean\nmode fast\nscope workspace\n\
             verdict PASS\nmerge-contract yes\nskipped none\nwhen 1\n",
            p.sha
        ),
    )
    .expect("write");
    let (code, err) = p.push(&p.sha, false);
    assert_eq!(code, 1, "{err}");
    assert!(
        err.contains("not in this gate's receipt format") && err.contains("an older gate's"),
        "{err}"
    );

    p.receipt(&green(&p.sha));
    let (code, err) = p.push(&p.sha, false);
    assert_eq!(code, 0, "{err}");
}

/// A narrowed, skipping or failing run never discharged the contract, whatever
/// its exit code was.
#[test]
fn a_run_that_did_not_discharge_the_contract_is_refused() {
    let p = Push::new("atv-push-narrow");
    let mut r = green(&p.sha);
    r.merge_contract = false;
    r.scope = "crate:aterm-gui".into();
    p.receipt(&r);
    let (code, err) = p.push(&p.sha, false);
    assert_eq!(code, 1, "{err}");
    assert!(
        err.contains("did NOT discharge the merge contract"),
        "{err}"
    );
}

/// A SKIP IS NOT A PASS, and the refusal says WHICH skip — the difference
/// between "the gate refused your change" and "this machine cannot present, so
/// the gui smoke skipped", which is the whole of an operator's next move on a
/// headless box. The contract is unchanged (nothing skipped is what discharges
/// it); what is added is that the tool says why.
#[test]
fn a_skipping_run_is_refused_and_the_refusal_names_the_skip() {
    let p = Push::new("atv-push-skip");
    let mut r = green(&p.sha);
    r.merge_contract = false;
    r.skipped = "gui smoke (ATERM_SKIP_GUI_SMOKE)".into();
    p.receipt(&r);
    let (code, err) = p.push(&p.sha, false);
    assert_eq!(code, 1, "{err}");
    assert!(
        err.contains("gui smoke (ATERM_SKIP_GUI_SMOKE)"),
        "the refusal names the skip rather than only its consequence: {err}"
    );
}

/// A FILE THE HOOK CANNOT READ IS NOT PERMISSION. The one direction this must
/// never fail in.
#[test]
fn a_receipt_the_hook_cannot_parse_refuses_the_push() {
    let p = Push::new("atv-push-garbage");
    let dir = receipt::dir(&p.root).expect("the store");
    fs::create_dir_all(&dir).expect("mkdir");
    fs::write(dir.join(&p.sha), "merge-contract yes\ntree clean\n").expect("write");
    let (code, err) = p.push(&p.sha, false);
    assert_eq!(
        code, 1,
        "a file without the magic line is no receipt: {err}"
    );
    assert!(err.contains("or no receipt at all"), "{err}");
}

/// A HOOK THAT CANNOT JUDGE REFUSES. MEASURED 2026-09-21, before the fix: the
/// shipped hook with `GIT_DIR` pointing at nothing — and the same hook run from
/// a directory that is no repository — exited 0 with NOTHING on stderr. That
/// was `[ -n "$root" ] || exit 0`, and during the full-disk episode of
/// 2026-09-20 it let cb770c598 reach origin/main with no passing receipt:
/// nothing else in the file admits a plain commit without one. A pre-push hook
/// only ever runs inside a repository, so an unresolvable root is a broken
/// environment, and a broken environment is a refusal that says what it could
/// not check and names both escapes.
#[test]
fn a_hook_that_cannot_resolve_its_repository_refuses_and_says_so() {
    let p = Push::new("atv-push-no-repo");
    // A passing receipt exists; through a broken GIT_DIR it is unreachable,
    // and unreachable must never read as "nothing to judge".
    p.receipt(&green(&p.sha));
    let nowhere = p.root.join("nowhere");
    let line = format!("refs/heads/main {} refs/heads/main {ZERO}\n", p.sha);
    let (code, err) = p.push_line_in(
        &line,
        false,
        &p.root,
        &[("GIT_DIR", nowhere.to_str().expect("utf-8 path"))],
    );
    assert_eq!(
        code, 1,
        "an unresolvable repository is a refusal, never exit 0: {err}"
    );
    assert!(
        err.contains("REFUSED") && err.contains("could not be checked"),
        "{err}"
    );
    assert!(
        err.contains("repository could not be resolved"),
        "…and it says WHAT it could not check: {err}"
    );
    assert!(
        err.contains("ATERM_PUSH_NO_GATE=1") && err.contains("--no-verify"),
        "…and names both escapes: {err}"
    );

    // Outside any repository at all — a ceiling keeps git from finding one
    // above the scratch directory, whatever the machine keeps in /tmp.
    let outside = p.root.parent().expect("scratch").join("outside");
    fs::create_dir_all(&outside).expect("mkdir");
    let ceiling = outside
        .parent()
        .expect("scratch")
        .to_str()
        .expect("utf-8 path")
        .to_string();
    let (code, err) = p.push_line_in(
        &line,
        false,
        &outside,
        &[("GIT_CEILING_DIRECTORIES", ceiling.as_str())],
    );
    assert_eq!(code, 1, "no repository is a refusal, never exit 0: {err}");
    assert!(err.contains("repository could not be resolved"), "{err}");

    // The named escape still works when nothing else can — it is checked
    // before the environment is, on purpose.
    let (code, err) = p.push_line_in(
        &line,
        true,
        &p.root,
        &[("GIT_DIR", nowhere.to_str().expect("utf-8 path"))],
    );
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("GATE BYPASSED"), "{err}");
}

/// THE RECEIPTS DIRECTORY MUST BE A DIRECTORY THE HOOK CAN READ. MEASURED
/// 2026-09-21, before the fix: a regular file at `.aterm-verify/receipts`
/// refused, and so did a mode-000 receipts directory holding a PASSING
/// receipt — both as "no gate receipt for this commit", which sends the
/// operator to run a gate whose receipt write then fails on the same path.
/// Refusing was never the problem; the diagnosis was. Both now refuse as what
/// they are: an environment the hook cannot judge, with the path named.
#[cfg(unix)]
#[test]
fn a_receipts_path_that_is_not_a_readable_directory_refuses_as_unjudgeable() {
    use std::os::unix::fs::PermissionsExt as _;

    // A regular file where the receipts directory should be.
    let p = Push::new("atv-push-receipts-file");
    let store = receipt::dir(&p.root).expect("the store");
    fs::create_dir_all(store.parent().expect("aterm-verify/")).expect("mkdir");
    fs::write(&store, "not a directory\n").expect("write");
    let (code, err) = p.push(&p.sha, false);
    assert_eq!(code, 1, "{err}");
    assert!(
        err.contains("could not be checked")
            && err.contains("aterm-verify/receipts/ is not a directory"),
        "{err}"
    );
    assert!(
        !err.contains("no gate receipt"),
        "the refusal names the environment, not a missing receipt: {err}"
    );

    // The store's own directory as a file.
    let p = Push::new("atv-push-state-file");
    fs::write(p.root.join(".git/aterm-verify"), "not a directory\n").expect("write");
    let (code, err) = p.push(&p.sha, false);
    assert_eq!(code, 1, "{err}");
    assert!(
        err.contains(".git/aterm-verify/ is not a directory"),
        "{err}"
    );

    // A receipts directory the hook cannot read, holding a receipt that would
    // admit the push — the one direction that must never open.
    let p = Push::new("atv-push-receipts-unreadable");
    p.receipt(&green(&p.sha));
    let dir = receipt::dir(&p.root).expect("the store");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o000)).expect("chmod");
    let (code, err) = p.push(&p.sha, false);
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).expect("chmod back");
    assert_eq!(code, 1, "{err}");
    assert!(
        err.contains("aterm-verify/receipts/ is not a readable directory"),
        "{err}"
    );
    assert!(!err.contains("no gate receipt"), "{err}");

    // And readable again, the same receipt admits: the refusal was the mode.
    let (code, err) = p.push(&p.sha, false);
    assert_eq!(code, 0, "{err}");
}

/// A RECEIPT THE HOOK CANNOT READ IS NOT "NO RECEIPT". Measured before the fix:
/// a mode-000 receipt refused with `head: … Permission denied` on stderr and
/// then "the receipt does not say whether the tree was clean" — fail-closed by
/// accident of an empty field, not by design. Now it is named.
#[cfg(unix)]
#[test]
fn a_receipt_the_hook_cannot_read_refuses_as_unjudgeable() {
    use std::os::unix::fs::PermissionsExt as _;

    let p = Push::new("atv-push-receipt-unreadable");
    p.receipt(&green(&p.sha));
    let file = receipt::dir(&p.root).expect("the store").join(&p.sha);
    fs::set_permissions(&file, fs::Permissions::from_mode(0o000)).expect("chmod");
    let (code, err) = p.push(&p.sha, false);
    fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).expect("chmod back");
    assert_eq!(code, 1, "{err}");
    assert!(
        err.contains("could not be checked") && err.contains("exists but cannot be read"),
        "{err}"
    );
    assert!(
        !err.contains("Permission denied"),
        "no tool noise, one sentence: {err}"
    );
}

/// A GIT COMMAND THAT FAILS IS NOT AN ANSWER. The claim and merge admissions
/// ask git about the remote's tip; when that tip is not in this repository (a
/// push over a remote that moved and was never fetched), `git merge-base
/// --is-ancestor` FAILS rather than answering "no". Measured before the fix:
/// the failure read as "not a claim", and the push was refused as "no gate
/// receipt" — the right exit, the wrong sentence. Now it is refused as what it
/// is, with the question the operator has to answer first.
#[test]
fn a_git_failure_inside_the_judgement_refuses_as_unjudgeable() {
    let p = Push::new("atv-push-git-fails");
    let unfetched = "1".repeat(40);
    let (code, err) = p.push_line(
        &format!("refs/heads/main {} refs/heads/main {unfetched}\n", p.sha),
        false,
    );
    assert_eq!(code, 1, "{err}");
    assert!(
        err.contains("could not be checked") && err.contains("merge-base --is-ancestor failed"),
        "{err}"
    );
    assert!(err.contains("is it fetched?"), "{err}");
    assert!(!err.contains("no gate receipt"), "{err}");
}

/// Deleting a ref pushes no code, so there is nothing to have gated.
#[test]
fn deleting_a_ref_needs_no_receipt() {
    let p = Push::new("atv-push-delete");
    let (code, err) = p.push(ZERO, false);
    assert_eq!(code, 0, "{err}");
}

/// The exception exists, and it is LOUD — an unescapable gate is what taught
/// the bypass the first time.
#[test]
fn the_named_bypass_works_and_says_so() {
    let p = Push::new("atv-push-bypass");
    let (code, err) = p.push(&p.sha, true);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("GATE BYPASSED"), "{err}");
    assert!(err.contains("nothing was checked"), "{err}");
}

/// A HOOK THAT IS NOT EXECUTABLE IS NOT A HOOK, and git says nothing about it.
///
/// This is the one way the whole push gate can disappear silently: git runs
/// `core.hooksPath/pre-push` only if the file is executable, and a checkout,
/// a patch application or an editor that drops the bit leaves a repository that
/// pushes everything with no refusal and no message. Nothing else in this tree
/// looks at the bit, so this does — and it checks the COMMITTED mode as well as
/// the working tree's, because that is what every other clone will get.
#[test]
fn the_hook_is_executable_here_and_in_the_index() {
    use std::os::unix::fs::PermissionsExt as _;

    let hook = Push::hook();
    let mode = fs::metadata(&hook)
        .expect("stat the hook")
        .permissions()
        .mode();
    assert!(
        mode & 0o111 != 0,
        "{} is not executable ({mode:o}) — git would skip it and every push would go          ungated, silently",
        hook.display()
    );

    let root = hook.parent().and_then(Path::parent).expect("repo root");
    let out = Command::new("git")
        .args(["ls-files", "-s", "--", ".githooks/pre-push"])
        .current_dir(root)
        .output()
        .expect("git runs");
    let row = String::from_utf8_lossy(&out.stdout);
    assert!(
        row.starts_with("100755 "),
        "the COMMITTED mode is not executable, so a fresh clone gets an inert push gate:          {row:?}"
    );
}

/// THE SENTENCE AND THE FILE AGREE.
///
/// [`aterm_verify::HOOK_CLAIM`] is printed to every operator on a fresh clone.
/// Both previous spellings of it were false — "L0 gate active" for a hook that
/// ran nothing, then "ADVISORY" after this one grew teeth — because the words
/// lived in one file and the behaviour in another and nothing compared them.
/// This does.
#[test]
fn the_claim_the_gate_prints_is_what_the_hook_was_just_measured_doing() {
    let claim = aterm_verify::HOOK_CLAIM;
    let p = Push::new("atv-push-claim");
    let (blocked, _) = p.push(&p.sha, false);
    p.receipt(&green(&p.sha));
    let (admitted, _) = p.push(&p.sha, false);
    let (bypassed, _) = p.push(&p.sha, true);
    let nowhere = p.root.join("nowhere");
    let (unjudgeable, _) = p.push_line_in(
        &format!("refs/heads/main {} refs/heads/main {ZERO}\n", p.sha),
        false,
        &p.root,
        &[("GIT_DIR", nowhere.to_str().expect("utf-8 path"))],
    );

    assert_eq!((blocked, admitted), (1, 0), "the hook blocks, then admits");
    assert_eq!(bypassed, 0, "the bypass works");
    assert_eq!(unjudgeable, 1, "and a hook that cannot judge refuses");
    assert!(
        claim.contains("BLOCKS") && claim.contains("receipt"),
        "the claim must say what was just measured: {claim}"
    );
    assert!(
        claim.contains("REFUSES when it cannot judge"),
        "…including that it fails CLOSED, which was just measured: {claim}"
    );
    assert!(
        claim.contains("ATERM_PUSH_NO_GATE=1"),
        "…and name the exception that was just measured: {claim}"
    );
    assert!(
        !claim.to_ascii_lowercase().contains("advisory"),
        "the hook is not advisory any more: {claim}"
    );
}
