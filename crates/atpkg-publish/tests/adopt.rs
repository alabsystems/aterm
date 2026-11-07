// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `atpkg-publish adopt trust=<N>` (docs/DESIGN-fast-build-release-2026-11-05.md A14, A16) over a
//! scratch store and a scratch repository: a compiler move that breaks the key commits nothing
//! until its fix is on the branch (the design's negative control); the key runs the locked build's
//! `targo` whatever `PATH` holds; a build with the base's compiler skips the key; a build whose
//! `trustc` is not its `LINEAGE`'s, an uncommitted change besides the lock, and an absent build
//! are refused.
//!
//! Each store build's `bin` holds stand-ins: `trustc` answers `-vV` with a commit and
//! `--print sysroot` with its build, and `targo` appends its `$0` and verb to the fixture's log,
//! fails `lint` in a checkout holding `needs-fix` and no `fixed`, and on `build` logs which
//! `targo` `PATH` resolves and, in a checkout holding `move-head`, commits on the branch under the
//! key. The verified head index is a fold of the store's own build unless a test says otherwise.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use atpkg::trust_lock::{self, TrustLock};
use atpkg_publish::adopt::{self, Inputs, Key};
use atpkg_publish::ctx::Ctx;
use atpkg_publish::error::Error;
use atpkg_publish::host::Hosts;
use atpkg_publish::host::fake::FakeHost;

/// The compiler the base's build reports.
const COMMIT_A: &str = "29ebb403de7a102850d6cb10a62d5cc3baf723ed";
/// The compiler a train build moves to.
const COMMIT_B: &str = "b7e1c0ffee5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a";
/// Every fixture LINEAGE's `lineage_receipt_sha256`.
const RECEIPT_SHA: &str = "4cd1415f6a19170ee60a26f5fdb2e799e3fc2dbfc939caff1c64a83fa554c620";
/// The fixture `.gitattributes`: a generated row with rows after it.
const ATTRIBUTES: &str =
    "*.rs text eol=lf\ngen.rs linguist-generated\n\n# Shell scripts.\n*.sh text eol=lf\n";

fn exe(path: &Path, text: &str) {
    use std::os::unix::fs::PermissionsExt as _;
    put(path, text);
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

fn put(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
    std::fs::write(path, text).expect("write a fixture file");
}

/// `git -C <at> <args…>`, hermetic, which must succeed: its stdout, trimmed.
fn git(at: &Path, args: &[&str]) -> String {
    let out = atpkg_publish::testkit::git()
        .arg("-C")
        .arg(at)
        .args(args)
        .output()
        .expect("start fixture git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .expect("UTF-8")
        .trim()
        .to_string()
}

/// `git show <rev>:<path>`, untrimmed.
fn show(at: &Path, spec: &str) -> String {
    let out = atpkg_publish::testkit::git()
        .arg("-C")
        .arg(at)
        .args(["show", spec])
        .output()
        .expect("start fixture git");
    assert!(out.status.success(), "git show {spec}");
    String::from_utf8(out.stdout).expect("UTF-8")
}

fn has_branch(at: &Path, branch: &str) -> bool {
    atpkg_publish::testkit::git()
        .arg("-C")
        .arg(at)
        .args([
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ])
        .status()
        .expect("start fixture git")
        .success()
}

struct Fixture {
    _dir: aterm_tempfile::TempDir,
    root: PathBuf,
    prefix: PathBuf,
    repo: PathBuf,
    log: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let dir = crate::shared::scratch(label);
        let root = std::fs::canonicalize(dir.path()).expect("the scratch dir");
        Self {
            prefix: root.join("pkg"),
            repo: root.join("repo"),
            log: root.join("key.log"),
            root,
            _dir: dir,
        }
    }

    /// Store build `build`: its `trustc` reports `reports`, its `LINEAGE` names `lineage`.
    fn build(&self, build: u64, reports: &str, lineage: &str) {
        let dir = trust_lock::build_dir(&self.prefix, build);
        std::fs::create_dir_all(dir.join("lib/rustlib")).expect("lib/rustlib");
        put(&trust_lock::ready_marker(&self.prefix, build), "ok\n");
        exe(
            &dir.join("bin/trustc"),
            &format!(
                "#!/bin/sh\ncase \"$1\" in\n  -vV) printf 'rustc 1.101.0-dev\\ncommit-hash: \
                 %s\\nrelease: 1.101.0-dev\\n' {reports} ;;\n  --print) printf '%s\\n' '{}' \
                 ;;\nesac\n",
                dir.display()
            ),
        );
        exe(
            &dir.join("bin/targo"),
            &format!(
                "#!/bin/sh\nprintf '%s %s\\n' \"$0\" \"$1\" >> '{log}'\ncase \"$1\" in\n  lint) \
                 if [ -e needs-fix ] && [ ! -e fixed ]; then exit 1; fi ;;\n  build) printf \
                 'path %s\\n' \"$(command -v targo)\" >> '{log}'\n    if [ -e move-head ]; then \
                 git commit --quiet --allow-empty -m 'moved under the key' || exit 9; fi ;;\n\
                 esac\nexit 0\n",
                log = self.log.display()
            ),
        );
        put(
            &dir.join("LINEAGE"),
            &format!(
                "# LINEAGE: the compiler this tree ships and the pack-cut receipt that proves \
                 its lineage,\nschema=atpkg-lineage-v1\ncompiler_commit={lineage}\n\
                 compiler_release=1.101.0-dev\nlineage_receipt=SEED-MINT\n\
                 lineage_receipt_sha256={RECEIPT_SHA}\nddc_seed_relation=rebuilt\n"
            ),
        );
    }

    /// The lock of store build `build` whose compiler is `commit`, as adopt would write it.
    fn locked(&self, build: u64, commit: &str) -> TrustLock {
        TrustLock {
            build,
            tree_root: atpkg::tree::tree_root(&trust_lock::build_dir(&self.prefix, build))
                .expect("fold the build"),
            compiler_commit: commit.to_string(),
            receipt: format!("SEED-MINT sha256:{RECEIPT_SHA}"),
            stock_toolchain: "1.97.1".to_string(),
        }
    }

    /// The repository on `main`, whose key is `targo lint && targo build`, with `extra` files:
    /// its head.
    fn repo(&self, extra: &[(&str, String)]) -> String {
        std::fs::create_dir_all(&self.repo).expect("mkdir repo");
        git(&self.repo, &["init", "--quiet", "--initial-branch=main"]);
        for (key, value) in [
            ("user.name", "fixture"),
            ("user.email", "fixture@example.invalid"),
            ("commit.gpgsign", "false"),
        ] {
            git(&self.repo, &["config", key, value]);
        }
        put(
            &self.repo.join("rust-toolchain.toml"),
            "[toolchain]\nchannel = \"trust\"\n",
        );
        put(
            &self.repo.join("publish/config.sh"),
            "# Repo-specific configuration.\nCHECK_CMD_DEFAULT=\"true\"\n# The key.\n\
             TOOLCHAIN_ACCEPT_CMD_DEFAULT=\"targo lint && targo build\"\n",
        );
        put(&self.repo.join(".gitattributes"), ATTRIBUTES);
        put(&self.repo.join("README"), "a fixture checkout\n");
        for (path, text) in extra {
            put(&self.repo.join(path), text);
        }
        git(&self.repo, &["add", "-A"]);
        git(&self.repo, &["commit", "--quiet", "-m", "base"]);
        git(&self.repo, &["rev-parse", "HEAD"])
    }

    fn inputs(&self, build: u64, path_env: OsString) -> Inputs {
        let prefix = self.prefix.clone();
        Inputs {
            prefix: self.prefix.clone(),
            rustup_home: self.root.join("rustup"),
            path_env,
            cargo_home: self.root.join("cargo-home"),
            rustc_vars: Vec::new(),
            stage2_env: None,
            signed_root: Arc::new(move |build| {
                atpkg::tree::tree_root(&trust_lock::build_dir(&prefix, build))
                    .map_err(|e| e.to_string())
            }),
            repo: self.repo.clone(),
            base: "main".to_string(),
            build,
            stock: Some("1.97.1".to_string()),
        }
    }

    /// Store build `build`'s `trustc`.
    fn trustc(&self, build: u64) -> PathBuf {
        trust_lock::build_dir(&self.prefix, build).join("bin/trustc")
    }

    fn worktree(&self, build: u64) -> PathBuf {
        self.root.join(format!("repo-adopt-trust-{build}"))
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    /// The locked build's `targo`, physical.
    fn targo(&self, build: u64) -> PathBuf {
        std::fs::canonicalize(trust_lock::build_dir(&self.prefix, build).join("bin/targo"))
            .expect("the build's targo")
    }
}

fn plain_path() -> OsString {
    OsString::from("/usr/bin:/bin")
}

fn refusal(error: Error) -> String {
    match error {
        Error::Refused(message) => message,
        other => panic!("not a refusal: {other}"),
    }
}

#[test]
fn a_compiler_move_that_breaks_the_key_commits_nothing_until_the_fix_is_on_the_branch() {
    let f = Fixture::new("adopt-move");
    f.build(100, COMMIT_A, COMMIT_A);
    f.build(200, COMMIT_B, COMMIT_B);
    let base_lock = adopt::lock_text(&f.locked(100, COMMIT_A)).expect("the base's lock");
    let base = f.repo(&[
        ("trust.lock", base_lock),
        ("needs-fix", "the new compiler's added lint\n".to_string()),
    ]);
    let wt = f.worktree(200);

    let why = refusal(adopt::adopt(&f.inputs(200, plain_path())).expect_err("the key is red"));
    assert!(
        why.contains("step 1/2 `targo lint`") && why.contains("adopt/trust-200"),
        "{why}"
    );
    assert_eq!(
        git(&f.repo, &["rev-parse", "adopt/trust-200"]),
        base,
        "a red key commits nothing on the branch"
    );
    assert!(
        wt.join("trust.lock").is_file(),
        "the lock stays written for the fixes"
    );
    assert!(f.log().contains(" lint\n"), "the key ran: {}", f.log());

    // The fix lands on the adopt branch; the next run judges the lock with it.
    put(&wt.join("fixed"), "the fix\n");
    git(&wt, &["add", "fixed"]);
    git(
        &wt,
        &["commit", "--quiet", "-m", "fix the new compiler's lint"],
    );
    let fix = git(&wt, &["rev-parse", "HEAD"]);
    let adopted = adopt::adopt(&f.inputs(200, plain_path())).expect("green with the fix");
    assert_eq!(
        adopted.key,
        Key::Green(vec!["targo lint".to_string(), "targo build".to_string()])
    );
    assert!(adopted.committed);
    assert_eq!(adopted.worktree, wt);
    assert_eq!(
        adopted.commit,
        git(&f.repo, &["rev-parse", "adopt/trust-200"])
    );
    assert_eq!(
        git(&f.repo, &["rev-parse", "adopt/trust-200^"]),
        fix,
        "the lock commit sits on the fix"
    );

    let text = show(&f.repo, "adopt/trust-200:trust.lock");
    assert!(
        matches!(
            aterm_census::seal::check(&text),
            aterm_census::seal::Seal::Holds { line: 1 }
        ),
        "{text}"
    );
    let want = f.locked(200, COMMIT_B);
    assert_eq!(trust_lock::parse(&text), Ok(want.clone()));
    assert_eq!(
        trust_lock::recorded_tree_root(&f.prefix, 200).as_deref(),
        Some(want.tree_root.as_str()),
        "the fold is recorded for discovery"
    );
    assert_eq!(
        show(&f.repo, "adopt/trust-200:.gitattributes"),
        "*.rs text eol=lf\ngen.rs linguist-generated\ntrust.lock linguist-generated text \
         eol=lf\n\n# Shell scripts.\n*.sh text eol=lf\n",
        "the row joins the generated rows"
    );
    let message = git(&f.repo, &["log", "-1", "--format=%B", "adopt/trust-200"]);
    for fact in [
        "build: 200",
        &format!("tree_root: {}", want.tree_root),
        &format!("compiler_commit: {COMMIT_B}"),
        &format!("receipt: SEED-MINT sha256:{RECEIPT_SHA}"),
        "stock: 1.97.1",
        "TOOLCHAIN_ACCEPT_CMD_DEFAULT green",
    ] {
        assert!(message.contains(fact), "{fact:?} in {message}");
    }
    assert_eq!(
        git(&wt, &["status", "--porcelain", "--untracked-files=all"]),
        "",
        "nothing is left uncommitted"
    );
    assert_eq!(
        git(&f.repo, &["rev-parse", "main"]),
        base,
        "main is untouched"
    );
}

#[test]
fn the_key_runs_the_locked_builds_targo_whatever_path_holds_first() {
    let f = Fixture::new("adopt-path");
    f.build(200, COMMIT_B, COMMIT_B);
    f.repo(&[]);
    let decoy_log = f.root.join("decoy.log");
    exe(
        &f.root.join("decoy/targo"),
        &format!(
            "#!/bin/sh\nprintf 'decoy %s\\n' \"$0\" >> '{}'\nexit 1\n",
            decoy_log.display()
        ),
    );
    let path = std::env::join_paths([
        f.root.join("decoy"),
        PathBuf::from("/usr/bin"),
        PathBuf::from("/bin"),
    ])
    .expect("a PATH");

    let adopted = adopt::adopt(&f.inputs(200, path)).expect("green on the locked build");
    assert!(matches!(adopted.key, Key::Green(_)), "{:?}", adopted.key);
    let targo = f.targo(200);
    assert_eq!(
        f.log(),
        format!("{t} lint\n{t} build\npath {t}\n", t = targo.display()),
        "each step runs the locked build's targo, and a step's own `targo` resolves it too"
    );
    assert!(
        !decoy_log.exists(),
        "PATH's targo never ran: {}",
        std::fs::read_to_string(&decoy_log).unwrap_or_default()
    );
}

#[test]
fn a_build_with_the_bases_compiler_skips_the_key_and_commits_the_lock() {
    let f = Fixture::new("adopt-same");
    f.build(100, COMMIT_A, COMMIT_A);
    // A repack of the same compiler: another tree, the same commit.
    f.build(300, COMMIT_A, COMMIT_A);
    let base_lock = adopt::lock_text(&f.locked(100, COMMIT_A)).expect("the base's lock");
    f.repo(&[
        ("trust.lock", base_lock),
        // The key would refuse here: a skipped key never sees it.
        ("needs-fix", "a red no verdict can reach\n".to_string()),
    ]);

    let mut inputs = f.inputs(300, plain_path());
    inputs.stock = Some("1.98.0".to_string());
    let adopted = adopt::adopt(&inputs).expect("adopted without the key");
    match &adopted.key {
        Key::Skipped(why) => assert!(
            why.contains(COMMIT_A) && why.contains("no verdict"),
            "{why}"
        ),
        other => panic!("the key ran: {other:?}"),
    }
    assert_eq!(f.log(), "", "no key step ran");
    assert!(adopted.committed);
    assert_eq!(
        trust_lock::parse(&show(&f.repo, "adopt/trust-300:trust.lock")),
        Ok(TrustLock {
            stock_toolchain: "1.98.0".to_string(),
            ..f.locked(300, COMMIT_A)
        })
    );
    let message = git(&f.repo, &["log", "-1", "--format=%B", "adopt/trust-300"]);
    assert!(
        message.contains("TOOLCHAIN_ACCEPT_CMD_DEFAULT skipped")
            && message.contains("stock: 1.98.0 (moved from 1.97.1)"),
        "the commit says the key was skipped and the stock moved: {message}"
    );
}

#[test]
fn a_build_whose_trustc_is_not_its_lineages_compiler_is_refused() {
    let f = Fixture::new("adopt-lineage");
    f.build(400, COMMIT_A, COMMIT_B);
    f.build(401, COMMIT_A, COMMIT_A);
    f.repo(&[]);

    let why = refusal(adopt::adopt(&f.inputs(400, plain_path())).expect_err("refused"));
    assert!(
        why.contains("LINEAGE") && why.contains(COMMIT_A) && why.contains(COMMIT_B),
        "{why}"
    );
    assert!(!f.worktree(400).exists(), "refused before any worktree");
    assert!(!has_branch(&f.repo, "adopt/trust-400"));

    // The same build with its own compiler's LINEAGE is adopted.
    let adopted = adopt::adopt(&f.inputs(401, plain_path())).expect("a consistent build");
    assert_eq!(adopted.lock.compiler_commit, COMMIT_A);
}

#[test]
fn an_uncommitted_change_besides_the_lock_is_refused_before_the_key_runs() {
    let f = Fixture::new("adopt-dirty");
    f.build(100, COMMIT_A, COMMIT_A);
    f.build(200, COMMIT_B, COMMIT_B);
    let base_lock = adopt::lock_text(&f.locked(100, COMMIT_A)).expect("the base's lock");
    f.repo(&[
        ("trust.lock", base_lock),
        ("needs-fix", "the new compiler's added lint\n".to_string()),
    ]);
    let wt = f.worktree(200);
    refusal(adopt::adopt(&f.inputs(200, plain_path())).expect_err("the key is red"));
    let ran = f.log();

    // A fix in the worktree, uncommitted: the key would pass on it, and the lock would land
    // without it.
    put(&wt.join("fixed"), "the fix\n");
    let why = refusal(adopt::adopt(&f.inputs(200, plain_path())).expect_err("uncommitted"));
    assert!(
        why.contains("commit the fixes on adopt/trust-200 first") && why.contains("fixed"),
        "{why}"
    );
    assert_eq!(f.log(), ran, "refused before the key runs");

    // A .gitattributes change besides the lock's row is a change too.
    git(&wt, &["add", "fixed"]);
    git(
        &wt,
        &["commit", "--quiet", "-m", "fix the new compiler's lint"],
    );
    let mut attributes = std::fs::read_to_string(wt.join(".gitattributes")).expect("written");
    attributes.push_str("*.md text eol=lf\n");
    put(&wt.join(".gitattributes"), &attributes);
    let why = refusal(adopt::adopt(&f.inputs(200, plain_path())).expect_err("uncommitted"));
    assert!(why.contains(".gitattributes"), "{why}");
    assert_eq!(f.log(), ran, "refused before the key runs");

    git(&wt, &["checkout", "--", ".gitattributes"]);
    let adopted = adopt::adopt(&f.inputs(200, plain_path())).expect("committed fixes only");
    assert!(adopted.committed);
}

#[test]
fn an_absent_build_is_refused_naming_its_install() {
    let f = Fixture::new("adopt-absent");
    f.build(100, COMMIT_A, COMMIT_A);
    f.repo(&[]);
    let why = refusal(adopt::adopt(&f.inputs(999, plain_path())).expect_err("absent"));
    assert!(
        why.contains("`aterm pkg install trust --build 999`"),
        "{why}"
    );
    assert!(!f.worktree(999).exists());
    assert!(!has_branch(&f.repo, "adopt/trust-999"));
}

#[test]
fn the_command_line_names_one_trust_build() {
    let dir = crate::shared::scratch("adopt-usage");
    let ctx = Ctx::new(
        dir.path(),
        None,
        None,
        atpkg_publish::testkit::NOW,
        Hosts::single(Arc::new(FakeHost::new())),
    );
    for line in [
        &[][..],
        &["trust=x"][..],
        &["trust="][..],
        &["rustc=5"][..],
        &["trust=5", "trust=6"][..],
        &["trust=5", "--stock"][..],
        &["trust=5", "--bogus"][..],
    ] {
        let args: Vec<String> = line.iter().map(|w| (*w).to_string()).collect();
        assert!(
            matches!(adopt::run(&ctx, &args, &|_| None), Err(Error::Usage(_))),
            "{line:?} is a usage error"
        );
    }
    // NEGATIVE CONTROL: a whole line parses and reaches the store, which holds no build 5.
    let repo = dir.path().display().to_string();
    for line in [
        &[
            "trust=5",
            "--repo",
            &repo,
            "--base=main",
            "--stock",
            "1.97.1",
        ][..],
        &[
            "--stock=1.97.1",
            "--repo",
            &repo,
            "--base",
            "main",
            "trust=5",
        ][..],
    ] {
        let args: Vec<String> = line.iter().map(|w| (*w).to_string()).collect();
        let why = refusal(adopt::run(&ctx, &args, &|_| None).expect_err("no build 5"));
        assert!(
            why.contains("`aterm pkg install trust --build 5`"),
            "{line:?}: {why}"
        );
    }
}

/// THE BRANCH'S OWN HEAD JUDGES THE MOVE: a reused branch made before `--base` moved is judged
/// from the compiler its head compiles with, not the base's, and keeps its head's `[stock]`.
/// NEGATIVE CONTROL: judged from the base, which moved to the new compiler, the key would skip.
#[test]
fn a_reused_branch_is_judged_from_its_own_head() {
    let f = Fixture::new("adopt-reused");
    f.build(100, COMMIT_A, COMMIT_A);
    f.build(200, COMMIT_B, COMMIT_B);
    f.build(300, COMMIT_B, COMMIT_B);
    let base_lock = adopt::lock_text(&f.locked(100, COMMIT_A)).expect("the base's lock");
    f.repo(&[
        ("trust.lock", base_lock),
        ("needs-fix", "the new compiler's added lint\n".to_string()),
    ]);
    let mut inputs = f.inputs(200, plain_path());
    inputs.stock = None;
    refusal(adopt::adopt(&inputs).expect_err("the key is red"));
    let wt = f.worktree(200);
    put(&wt.join("fixed"), "the fix\n");
    git(&wt, &["add", "fixed"]);
    git(
        &wt,
        &["commit", "--quiet", "-m", "fix the new compiler's lint"],
    );
    // main moves to another build of the new compiler, with another stock.
    let moved = adopt::lock_text(&TrustLock {
        stock_toolchain: "1.98.0".to_string(),
        ..f.locked(300, COMMIT_B)
    })
    .expect("main's new lock");
    put(&f.repo.join("trust.lock"), &moved);
    git(&f.repo, &["commit", "--quiet", "-am", "main adopts 300"]);
    let before = f.log();

    let adopted = adopt::adopt(&inputs).expect("green with the fix");
    assert!(
        matches!(&adopted.key, Key::Green(_)),
        "the head compiles with build 100, so the key judges the move: {:?}",
        adopted.key
    );
    assert!(f.log().len() > before.len(), "the key ran again");
    assert_eq!(adopted.lock.stock_toolchain, "1.97.1", "the head's stock");
    let message = git(&f.repo, &["log", "-1", "--format=%B", "adopt/trust-200"]);
    assert!(
        message.contains("stock: 1.97.1 (unchanged)")
            && message.contains(&format!(
                "on adopt/trust-200 at {}",
                &git(&wt, &["rev-parse", "HEAD^"])[..12]
            )),
        "{message}"
    );
}

/// A TREE WITH NO LOCK NAMES ITS STOCK: no `[stock]` carries over, so adopt asks for `--stock`.
#[test]
fn a_first_lock_needs_its_stock_named() {
    let f = Fixture::new("adopt-bootstrap");
    f.build(200, COMMIT_B, COMMIT_B);
    f.repo(&[]);
    let mut inputs = f.inputs(200, plain_path());
    inputs.stock = None;
    let why = refusal(adopt::adopt(&inputs).expect_err("no stock"));
    assert!(why.contains("--stock X.Y.Z"), "{why}");
    assert_eq!(f.log(), "", "refused before the key");
    inputs.stock = Some("1.97".to_string());
    assert!(matches!(adopt::adopt(&inputs), Err(Error::Usage(_))));
    inputs.stock = Some("1.97.1".to_string());
    let adopted = adopt::adopt(&inputs).expect("named");
    assert_eq!(adopted.stock_line(), "1.97.1 (the branch carried no lock)");
}

/// WHATEVER TAKES THE LOCKED COMPILER'S PLACE IS REFUSED before the key runs: `TRUST_STAGE2_BIN`,
/// a `build.rustc` in `$CARGO_HOME`, a `RUSTC` naming another compiler, a dev-linked checkout.
/// NEGATIVE CONTROL: a `RUSTC` naming the locked build's own compiler is taken.
#[test]
fn whatever_takes_the_locked_compilers_place_is_refused() {
    let f = Fixture::new("adopt-movers");
    f.build(100, COMMIT_A, COMMIT_A);
    f.build(200, COMMIT_B, COMMIT_B);
    f.repo(&[]);

    let mut inputs = f.inputs(200, plain_path());
    inputs.stage2_env = Some(OsString::from("/elsewhere/bin"));
    let why = refusal(adopt::adopt(&inputs).expect_err("an override"));
    assert!(why.contains("unset TRUST_STAGE2_BIN"), "{why}");
    assert!(!f.worktree(200).exists(), "refused before any worktree");

    let config = f.root.join("cargo-home/config.toml");
    put(
        &config,
        &format!("[build]\nrustc = \"{}\"\n", f.trustc(100).display()),
    );
    let why = refusal(adopt::adopt(&f.inputs(200, plain_path())).expect_err("build.rustc"));
    assert!(
        why.contains(&format!(
            "Fix: remove build.rustc from {}",
            config.display()
        )) && why.contains(COMMIT_A),
        "{why}"
    );
    std::fs::remove_file(&config).expect("rm config");

    let mut inputs = f.inputs(200, plain_path());
    inputs.rustc_vars = vec![("RUSTC", f.trustc(100).display().to_string())];
    let why = refusal(adopt::adopt(&inputs).expect_err("RUSTC"));
    assert!(why.contains("Fix: unset RUSTC"), "{why}");

    let link = f.prefix.join("links/trust");
    let checkout = f.root.join("trust-checkout");
    exe(&checkout.join("bin/targo"), "#!/bin/sh\nexit 0\n");
    std::fs::create_dir_all(checkout.join("lib")).expect("lib");
    put(
        &link,
        &format!(
            "schema = 1\nprogram = \"trust\"\ncheckout = \"{}\"\nbins = [\"bin/targo\"]\n",
            checkout.display()
        ),
    );
    let why = refusal(adopt::adopt(&f.inputs(200, plain_path())).expect_err("a dev link"));
    assert!(why.contains("aterm pkg unlink trust"), "{why}");
    std::fs::remove_file(&link).expect("rm link");
    assert_eq!(f.log(), "", "no key step ran while anything took its place");

    let mut inputs = f.inputs(200, plain_path());
    inputs.rustc_vars = vec![("RUSTC", f.trustc(200).display().to_string())];
    let adopted = adopt::adopt(&inputs).expect("RUSTC is the locked compiler");
    assert!(matches!(adopted.key, Key::Green(_)), "{:?}", adopted.key);
}

/// A LOCK NAMES ONLY A BUILD THE VERIFIED HEAD INDEX SIGNS, with the tree it signs: a build the
/// index refuses (withdrawn, floored) or signs another tree for is refused before any worktree.
#[test]
fn a_build_the_index_does_not_sign_as_held_is_refused() {
    let f = Fixture::new("adopt-index");
    f.build(200, COMMIT_B, COMMIT_B);
    f.repo(&[]);
    let mut inputs = f.inputs(200, plain_path());
    inputs.signed_root =
        Arc::new(|_| Err("trust 200 is withdrawn or below the channel's floor".to_string()));
    let why = refusal(adopt::adopt(&inputs).expect_err("withdrawn"));
    assert!(
        why.contains("signs no Trust build 200") && why.contains("withdrawn"),
        "{why}"
    );
    inputs.signed_root = Arc::new(|_| Ok("e".repeat(64)));
    let why = refusal(adopt::adopt(&inputs).expect_err("another tree"));
    assert!(
        why.contains(&format!("signs {}", "e".repeat(64)))
            && why.contains("`aterm pkg install trust --build 200`"),
        "{why}"
    );
    assert!(!f.worktree(200).exists(), "refused before any worktree");
    assert!(!has_branch(&f.repo, "adopt/trust-200"));
}

/// A BRANCH THAT MOVES UNDER THE KEY COMMITS NOTHING: the key judged the head it started on.
#[test]
fn a_branch_that_moves_under_the_key_commits_nothing() {
    let f = Fixture::new("adopt-moved");
    f.build(100, COMMIT_A, COMMIT_A);
    f.build(200, COMMIT_B, COMMIT_B);
    let base_lock = adopt::lock_text(&f.locked(100, COMMIT_A)).expect("the base's lock");
    f.repo(&[
        ("trust.lock", base_lock),
        ("move-head", "the build step commits\n".to_string()),
    ]);
    let why = refusal(adopt::adopt(&f.inputs(200, plain_path())).expect_err("moved"));
    assert!(
        why.contains("adopt/trust-200 moved from") && why.contains("while"),
        "{why}"
    );
    assert_eq!(
        git(&f.repo, &["log", "-1", "--format=%s", "adopt/trust-200"]),
        "moved under the key",
        "no lock commit on top"
    );
    assert!(
        git(&f.worktree(200), &["status", "--porcelain"]).contains("trust.lock"),
        "the lock stays written and uncommitted"
    );
}

/// A WORKTREE PATH THAT IS NOT THIS REPOSITORY'S ADOPT BRANCH IS REFUSED, left as it is.
#[test]
fn a_worktree_path_on_another_branch_is_refused() {
    let f = Fixture::new("adopt-path-taken");
    f.build(200, COMMIT_B, COMMIT_B);
    f.repo(&[]);
    let wt = f.worktree(200);
    git(
        &f.repo,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "other",
            &wt.display().to_string(),
        ],
    );
    let why = refusal(adopt::adopt(&f.inputs(200, plain_path())).expect_err("taken"));
    assert!(
        why.contains("is not") && why.contains("adopt/trust-200"),
        "{why}"
    );
    assert_eq!(git(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]), "other");
    assert!(!wt.join("trust.lock").exists());
}

/// A `LINEAGE` THAT DOES NOT NAME A COMPILER AND RECEIPT IS REFUSED, each for its reason.
#[test]
fn a_malformed_lineage_is_refused() {
    let f = Fixture::new("adopt-lineage-shape");
    f.build(500, COMMIT_A, COMMIT_A);
    f.repo(&[]);
    let lineage = trust_lock::build_dir(&f.prefix, 500).join("LINEAGE");
    let good = std::fs::read_to_string(&lineage).expect("LINEAGE");
    // The store records the tree it staged; the edits below stand in for packs that shipped so.
    trust_lock::fold_and_record(&trust_lock::build_dir(&f.prefix, 500)).expect("fold");
    for (from, to, says) in [
        (
            "schema=atpkg-lineage-v1",
            "schema=atpkg-lineage-v0",
            "schema",
        ),
        (
            &format!("compiler_commit={COMMIT_A}")[..],
            "compiler_commit=29ebb403",
            "40 lowercase hex",
        ),
        (
            "lineage_receipt=SEED-MINT",
            "lineage_receipt=",
            "names no receipt",
        ),
        (
            &format!("lineage_receipt_sha256={RECEIPT_SHA}")[..],
            "lineage_receipt_sha256=4cd1",
            "64 lowercase hex",
        ),
        (
            "ddc_seed_relation=rebuilt",
            "schema=atpkg-lineage-v1",
            "given twice",
        ),
    ] {
        assert!(good.contains(from), "{from}");
        put(&lineage, &good.replacen(from, to, 1));
        let why = refusal(adopt::adopt(&f.inputs(500, plain_path())).expect_err(says));
        assert!(
            why.contains(says) && why.contains("LINEAGE"),
            "{says}: {why}"
        );
    }
    put(&lineage, &good);
    adopt::adopt(&f.inputs(500, plain_path())).expect("a well-formed LINEAGE");
}
