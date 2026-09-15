// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The environment a run verifies: the snapshot, the source and toolchain
//! tripwires, and the timings side channel.
//!
//! WHY (2026-09-13). A `--fast` run took 14 h in a live checkout that was
//! pulled four times while it ran, and it still printed one verdict as if it
//! were about one tree. `gate_contract.rs` proves WHAT the ladder decides; these
//! prove that what it decides is about the tree and compiler the caller started
//! it on — and that the machinery added for that never touches the caller's
//! uncommitted work and never changes a byte of the ladder.
//!
//! Every fixture writes its own ignore rules: nothing here may depend on the
//! workspace's `.gitignore`.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use aterm_verify::exec::Timings;
use aterm_verify::identity::{self, ToolchainIdentity, TreeState, Tripwire};
use aterm_verify::snapshot::{self, SourceMode};
use aterm_verify::verdict::MERGE_CONTRACT_SENTENCE;
use aterm_verify::{Ctx, EnvSnapshot, Mode, Scope, exit, mktemp_dir, plan};

const IGNORES: &str = "*.log\n/target/\n/target-*/\n/.aterm-verify/\n";

fn path_env() -> std::ffi::OsString {
    std::env::var_os("PATH").unwrap_or_default()
}

/// `git` in `dir`, deterministic whatever the developer's own config says:
/// no signing, no hooks, a fixed identity, no redirects inherited from a hook.
fn git(dir: &Path, args: &[&str]) -> String {
    let mut c = Command::new("git");
    c.current_dir(dir).args([
        "-c",
        "user.name=gate",
        "-c",
        "user.email=gate@example.invalid",
        "-c",
        "commit.gpgsign=false",
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "init.defaultBranch=main",
    ]);
    for k in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
    ] {
        c.env_remove(k);
    }
    let out = c.args(args).output().expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn write(p: &Path, body: &str) {
    if let Some(d) = p.parent() {
        fs::create_dir_all(d).expect("mkdir");
    }
    fs::write(p, body).expect("write");
}

fn script(p: &Path, body: &str) {
    write(p, &format!("#!/bin/sh\n{body}\n"));
    fs::set_permissions(p, fs::Permissions::from_mode(0o755)).expect("chmod");
}

fn mtime(p: &Path) -> std::time::SystemTime {
    fs::metadata(p).expect("stat").modified().expect("mtime")
}

/// The stage2 driver body that also produces the two binaries the smokes drive
/// — the same stand-in `gate_contract.rs` uses, so a whole ladder runs green.
const ANSWERING_SMOKE: &str = r#"case "$*" in
  *aterm-gui*aterm-ctl*)
    mkdir -p target-drivers/debug
    cat >target-drivers/debug/aterm-gui <<'GUI'
#!/bin/sh
mkdir -p "$XDG_RUNTIME_DIR/aterm"
ln -s /dev/null "$XDG_RUNTIME_DIR/aterm/aterm.sock"
exec sleep 300
GUI
    cat >target-drivers/debug/aterm-ctl <<'CTL'
#!/bin/sh
case "$1" in
  cursor)  echo "OK row=0 col=0" ;;
  metrics) echo "OK frames=41 max_input_present_ms=8.100 redraw_retry_gated=0 present_drops=0 sync_rel_timeout=0 perf_reduced=0 wake_heals=0 " ;;
  send|key) echo "OK accepted" ;;
  *) echo "ERR unknown verb"; exit 1 ;;
esac
CTL
    chmod 755 target-drivers/debug/aterm-gui target-drivers/debug/aterm-ctl
    ;;
esac
exit 0"#;

/// A repo-shaped directory whose every helper passes — `gate_contract.rs`'s
/// synthetic repo, rebuilt here because integration tests share no code.
struct Fixture {
    base: PathBuf,
    root: PathBuf,
    stage2: PathBuf,
    scratch: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let base = mktemp_dir(tag).expect("mktemp");
        let (root, stage2, scratch) =
            (base.join("repo"), base.join("stage2"), base.join("scratch"));
        for d in [&root, &stage2, &scratch] {
            fs::create_dir_all(d).expect("mkdir");
        }
        write(&root.join("Cargo.toml"), "[workspace]\n");
        for rel in [
            "tools/verify.sh",
            "tools/test-install-channel.sh",
            "tools/test-atpkg-vendor-tooling.sh",
            "tools/test-atpkg-auto-vendor.sh",
            "tools/test-atpkg-pack-one-compiler.sh",
            "tools/test-trust-gate-verdict.sh",
            "tools/test-trust-contract-probe.sh",
            "tools/perf-arena/test-start-compare.sh",
            "libc-oracle/run.sh",
        ] {
            script(&root.join(rel), "exit 0");
        }
        script(
            &root.join("tools/grep_guard.sh"),
            "echo 'GUARD: PASS'; exit 0",
        );
        script(
            &root.join("tools/license_check.sh"),
            "echo 'LICENSE: PASS'; exit 0",
        );
        fs::create_dir_all(root.join("scripts")).expect("mkdir");
        script(
            &root.join("target-drivers/debug/aterm-redraw-conformance"),
            "exit 0",
        );
        for ex in [
            "objc_live_class_audit",
            "objc_ime_drive",
            "objc_toolbar_drive",
            "objc_window_drive",
            "objc_event_drive",
            "objc_alert_drive",
            "objc_swizzle_drive",
            "objc_bound_drive",
        ] {
            script(
                &root.join("target-drivers/debug/examples").join(ex),
                "exit 0",
            );
        }
        script(&stage2.join("trustdoc"), "exit 0");
        Self {
            base,
            root,
            stage2,
            scratch,
        }
    }

    /// A git checkout of the fixture, everything committed, the build output
    /// ignored by the fixture's own rules.
    fn git_init(&self) -> &Self {
        write(&self.root.join(".gitignore"), IGNORES);
        git(&self.root, &["init", "-q"]);
        git(&self.root, &["add", "-A"]);
        git(&self.root, &["commit", "-q", "-m", "fixture"]);
        self
    }

    /// `targo`: when `<base>/trigger` exists, run `on_build` during the
    /// workspace build; answer the smokes either way.
    fn with_targo(&self, on_build: &str) -> &Self {
        script(
            &self.stage2.join("targo"),
            &format!(
                "case \"$*\" in\n  \"--unverified build --workspace\")\n    \
                 if [ -e '{}' ]; then {on_build}; fi ;;\nesac\n{ANSWERING_SMOKE}",
                self.base.join("trigger").display()
            ),
        );
        self
    }

    fn arm_trigger(&self) {
        write(&self.base.join("trigger"), "");
    }

    fn ctx(&self) -> Ctx {
        let mut env = EnvSnapshot::capture();
        env.trust_stage2_bin = Some(self.stage2.clone());
        env.skip_gui_smoke = Some("1".into());
        env.trust_mc_sysroot = Some(self.root.join("no-trust-mc"));
        env.ay_bin_dir = Some(self.root.join("no-ay"));
        env.cargo_target_dir = None;
        Ctx::new(
            self.root.clone(),
            Mode::Fast,
            Scope::workspace(),
            false,
            env,
            self.scratch.clone(),
        )
    }

    fn run(&self, ctx: &Ctx) -> (String, i32) {
        let mut out: Vec<u8> = Vec::new();
        let code = aterm_verify::run(ctx, &mut out).expect("the ladder is writable");
        (String::from_utf8(out).expect("utf-8 ladder"), code)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.base).ok();
    }
}

#[test]
fn a_tree_that_moves_mid_run_never_claims_the_contract() {
    let repo = Fixture::new("atv-env-move");
    repo.git_init().with_targo(
        "git -c user.name=gate -c user.email=gate@example.invalid -c commit.gpgsign=false \
         commit -q --no-verify --allow-empty -m pulled-mid-run",
    );
    let started_on = git(&repo.root, &["rev-parse", "HEAD"]);

    // The same fixture, left alone, is not COULD NOT RUN — so what the moved
    // run reports below is the move and nothing else.
    let (calm, calm_code) = repo.run(&repo.ctx());
    assert_ne!(calm_code, exit::COULD_NOT_RUN, "{calm}");
    assert!(
        calm.contains(&format!("verify: source {started_on} in place ")),
        "a git root names the commit it verifies: {calm}"
    );
    assert!(!calm.contains("moved during this run"), "{calm}");

    repo.arm_trigger();
    let (ladder, code) = repo.run(&repo.ctx());
    let moved_to = git(&repo.root, &["rev-parse", "HEAD"]);
    assert_ne!(moved_to, started_on, "the fixture really moved HEAD");

    assert_eq!(code, exit::COULD_NOT_RUN, "{ladder}");
    assert!(!ladder.contains(MERGE_CONTRACT_SENTENCE), "{ladder}");
    assert!(
        ladder.contains(&format!("HEAD {started_on} -> {moved_to}")),
        "the new HEAD is named: {ladder}"
    );
    assert!(ladder.contains("=== source identity ==="), "{ladder}");
    // The stages after the move did not run on the moved tree.
    let test_block = ladder
        .split("\n=== ")
        .find(|b| b.starts_with("test ("))
        .expect("the test stage is still printed");
    assert!(
        test_block.contains("  FAIL  not run: the tree or the toolchain moved"),
        "{test_block}"
    );
    assert!(
        !ladder.contains("argv: --unverified test --workspace"),
        "{ladder}"
    );
}

#[test]
fn a_git_tree_that_cannot_be_read_in_place_is_could_not_run_before_any_stage() {
    // MEASURED 2026-09-13 (review of batch B): an unreadable untracked file made
    // `git hash-object` fail, the capture failure read as "not a git checkout",
    // and the run went ahead with NO source tripwire and no source line — so an
    // `--in-place` run could report green on a tree nobody was watching.
    let repo = Fixture::new("atv-env-unreadable");
    repo.git_init().with_targo("true");
    let secret = repo.root.join("secret.txt");
    write(&secret, "no one may read this\n");
    fs::set_permissions(&secret, fs::Permissions::from_mode(0o000)).expect("chmod");

    let (ladder, code) = repo.run(&repo.ctx());
    fs::set_permissions(&secret, fs::Permissions::from_mode(0o644)).expect("chmod back");

    assert_eq!(code, exit::COULD_NOT_RUN, "{ladder}");
    assert!(!ladder.contains(MERGE_CONTRACT_SENTENCE), "{ladder}");
    assert!(ladder.contains("=== source identity ==="), "{ladder}");
    assert!(
        ladder.contains("git could not read the tree"),
        "the reason is named: {ladder}"
    );
    assert!(
        !ladder.contains("=== build ("),
        "no stage ran on a tree the gate could not read: {ladder}"
    );

    // Readable again, the same fixture runs its ladder and names its source.
    let (calm, calm_code) = repo.run(&repo.ctx());
    assert_ne!(calm_code, exit::COULD_NOT_RUN, "{calm}");
    assert!(calm.contains("verify: source "), "{calm}");
}

/// The gate binary on `root`: `--fast` plus `extra`, the fixture's driver,
/// `path` as PATH, and none of the caller's gate channels.
fn gate(
    root: &Path,
    stage2: &Path,
    extra: &[&str],
    path: &std::ffi::OsStr,
) -> (String, Option<i32>) {
    let out = Command::new(env!("CARGO_BIN_EXE_aterm-verify"))
        .arg("--fast")
        .args(extra)
        .arg("--root")
        .arg(root)
        .env("PATH", path)
        .env("TRUST_STAGE2_BIN", stage2)
        .env("ATERM_SKIP_GUI_SMOKE", "1")
        .env_remove(snapshot::SNAPSHOT_ENV)
        .env_remove("ATERM_VERIFY_TIMINGS")
        .env_remove("CARGO_TARGET_DIR")
        .output()
        .expect("the gate binary runs");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.code(),
    )
}

/// Both spellings of a run on a checkout git cannot open: COULD NOT RUN before
/// any stage, never an in-place run with no source tripwire.
fn assert_never_unarmed(root: &Path, stage2: &Path, path: &std::ffi::OsStr, what: &str) {
    let manifest = fs::read(root.join("Cargo.toml")).expect("manifest");
    for extra in [&["--in-place"][..], &[][..]] {
        let (ladder, code) = gate(root, stage2, extra, path);
        let run = format!("{what} {extra:?}");
        assert_eq!(code, Some(exit::COULD_NOT_RUN), "{run}: {ladder}");
        assert!(!ladder.contains(MERGE_CONTRACT_SENTENCE), "{run}: {ladder}");
        assert!(
            !ladder.contains("=== build ("),
            "{run}: no stage ran: {ladder}"
        );
        assert!(
            ladder.contains("could not open"),
            "{run}: the reason is named: {ladder}"
        );
        assert!(
            !ladder.contains("is not a git checkout"),
            "{run}: never read as a root without a source: {ladder}"
        );
    }
    assert_eq!(
        fs::read(root.join("Cargo.toml")).expect("manifest"),
        manifest,
        "{what}: the driver that edits the tree never ran"
    );
}

#[test]
fn a_checkout_git_cannot_open_is_could_not_run_not_an_unarmed_in_place_run() {
    // MEASURED 2026-09-13 (review of batch B, round 2): a root that HAS a .git
    // but where `git rev-parse --show-toplevel` fails read as "not a git
    // checkout" — no source identity, and the snapshot mode fell back IN PLACE.
    // A driver that edited the tree mid-run then went VERIFY: PASS.
    let repo = Fixture::new("atv-env-noopen");
    repo.git_init()
        .with_targo("echo moved-mid-run >> Cargo.toml");
    repo.arm_trigger();

    // A linked worktree whose main checkout was renamed: its gitdir dangles.
    let wt = repo.base.join("wt");
    git(
        &repo.root,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            wt.to_str().expect("utf-8"),
            "HEAD",
        ],
    );
    // The ignored smoke drivers too, so the only thing wrong with `wt` is git.
    // They live in the driver lane's `target-drivers/` since the 2026-09-13 lane
    // split (a `target/` is copied as well when the fixture has one).
    for lane in ["target-drivers", "target"] {
        if !repo.root.join(lane).exists() {
            continue;
        }
        let copied = Command::new("cp")
            .arg("-R")
            .arg(repo.root.join(lane))
            .arg(wt.join(lane))
            .status()
            .expect("cp runs");
        assert!(copied.success(), "copy {lane} into the worktree");
    }
    // `Fixture::new` lays the redraw harness and the objc examples in the driver lane
    // (the aterm-gui/aterm-ctl stubs come only from ANSWERING_SMOKE's build, which
    // this test does not use), so the redraw harness proves the copy happened.
    assert!(
        wt.join("target-drivers/debug/aterm-redraw-conformance")
            .exists(),
        "the driver lane reached the worktree"
    );
    fs::rename(&repo.root, repo.base.join("repo-renamed")).expect("rename the main checkout");
    assert!(wt.join(".git").is_file());
    assert_never_unarmed(&wt, &repo.stage2, &path_env(), "dangling worktree gitdir");

    // A normal checkout with git absent from PATH.
    let plain = Fixture::new("atv-env-nogit");
    plain
        .git_init()
        .with_targo("echo moved-mid-run >> Cargo.toml");
    plain.arm_trigger();
    let bin = plain.base.join("nogit-bin");
    fs::create_dir_all(&bin).expect("mkdir");
    for tool in [
        "sh", "mkdir", "cat", "chmod", "ln", "sleep", "pwd", "rm", "mv", "echo", "mktemp",
    ] {
        if let Some(found) = ["/bin", "/usr/bin"]
            .iter()
            .map(|d| Path::new(d).join(tool))
            .find(|p| p.exists())
        {
            std::os::unix::fs::symlink(found, bin.join(tool)).expect("symlink");
        }
    }
    assert!(!bin.join("git").exists());
    assert_never_unarmed(
        &plain.root,
        &plain.stage2,
        bin.as_os_str(),
        "git absent from PATH",
    );
}

#[test]
fn an_edit_hidden_by_an_index_flag_is_verified_not_replaced_by_head() {
    // MEASURED 2026-09-13 (review of batch B, round 2): with `update-index
    // --skip-worktree a.txt` and `--assume-unchanged b.txt`, both edited, `git
    // status` and `git diff HEAD` are empty — so the identity, the sync and the
    // tripwire all skipped them, and the snapshot verified HEAD's bytes with no
    // `+dirty`.
    let repo = Fixture::new("atv-env-flags");
    write(&repo.root.join("a.txt"), "a1\n");
    write(&repo.root.join("b.txt"), "b1\n");
    write(&repo.root.join("c.txt"), "c1\n");
    repo.git_init();
    git(&repo.root, &["update-index", "--skip-worktree", "a.txt"]);
    git(
        &repo.root,
        &["update-index", "--assume-unchanged", "b.txt", "c.txt"],
    );
    write(&repo.root.join("a.txt"), "a2\n");
    write(&repo.root.join("b.txt"), "b2\n");
    assert_eq!(
        git(&repo.root, &["status", "--porcelain"]),
        "",
        "the edits are hidden"
    );

    // In place: the identity names both edits, and not the unedited flagged file.
    let tree = TreeState::capture(&repo.root, &path_env()).expect("readable");
    assert!(
        tree.dirty.contains_key("a.txt") && tree.dirty.contains_key("b.txt"),
        "{tree:?}"
    );
    assert!(!tree.dirty.contains_key("c.txt"), "{tree:?}");

    // Snapshot: the stages read the edited bytes, and the source says +dirty.
    script(
        &repo.stage2.join("targo"),
        "echo \"child: $* a=$(cat a.txt) b=$(cat b.txt) c=$(cat c.txt)\"; exit 0",
    );
    let (ladder, code) = gate(&repo.root, &repo.stage2, &[], &path_env());
    let head = git(&repo.root, &["rev-parse", "HEAD"]);
    assert!(
        ladder.contains(&format!("verify: source {head}+dirty ")),
        "{code:?}: {ladder}"
    );
    assert!(
        ladder.contains("child: --unverified build --workspace a=a2 b=b2 c=c1"),
        "{code:?}: {ladder}"
    );
    assert!(
        !ladder.contains("a=a1") && !ladder.contains("b=b1"),
        "{ladder}"
    );

    // The caller's flags and edits are untouched.
    let flags = git(&repo.root, &["ls-files", "-v", "a.txt", "b.txt", "c.txt"]);
    assert_eq!(flags, "S a.txt\nh b.txt\nh c.txt", "{flags}");
    assert_eq!(
        fs::read_to_string(repo.root.join("a.txt")).expect("a"),
        "a2\n"
    );
}

#[test]
fn a_changed_run_selects_the_crate_an_index_flag_hides_an_edit_in() {
    // MEASURED 2026-09-13 (review of batch B, round 3): `--changed --in-place`
    // read its paths from `git diff --name-only` and `ls-files --others` only,
    // so an edit under `--skip-worktree` selected 0 crates, built nothing, and
    // went VERIFY: PASS.
    let repo = Fixture::new("atv-env-changed-flag");
    for (dir, name) in [("crates/a", "crate-a"), ("crates/b", "crate-b")] {
        write(
            &repo.root.join(dir).join("Cargo.toml"),
            &format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n"),
        );
        write(&repo.root.join(dir).join("src/lib.rs"), "// v1\n");
    }
    repo.git_init();
    let root = fs::canonicalize(&repo.root).expect("root");
    let r = root.display();
    script(
        &repo.stage2.join("targo"),
        &format!(
            "case \"$*\" in\n  \"tree --workspace\"*)\n    \
             echo 'crate-a v0.1.0 ({r}/crates/a)'; echo 'crate-b v0.1.0 ({r}/crates/b)'; exit 0 ;;\n  \
             \"tree --invert crate-a\"*) echo 'crate-a v0.1.0 ({r}/crates/a)'; exit 0 ;;\n  \
             \"tree --invert crate-b\"*) echo 'crate-b v0.1.0 ({r}/crates/b)'; exit 0 ;;\n\
             esac\n{ANSWERING_SMOKE}"
        ),
    );
    let lib = root.join("crates/b/src/lib.rs");
    let changed = |what: &str| {
        let root_arg = root.to_str().expect("utf-8");
        let out = Command::new(env!("CARGO_BIN_EXE_aterm-verify"))
            .args([
                "--fast",
                "--changed",
                "--base",
                "HEAD",
                "--in-place",
                "--root",
                root_arg,
            ])
            .env("TRUST_STAGE2_BIN", &repo.stage2)
            .env("ATERM_SKIP_GUI_SMOKE", "1")
            .env_remove(snapshot::SNAPSHOT_ENV)
            .env_remove("ATERM_VERIFY_TIMINGS")
            .env_remove("CARGO_TARGET_DIR")
            .env_remove("ATERM_VERIFY_BASE")
            .output()
            .expect("the gate binary runs");
        let ladder = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(
            !ladder.contains("0 crate(s) selected") && !ladder.contains("<no crates selected>"),
            "{what}: an edited crate was left out of the selection: {ladder}"
        );
        assert!(
            ladder.contains("=== build (-p crate-b) ===")
                || ladder.contains("change scope: WIDENED"),
            "{what}: {ladder}"
        );
    };

    // Control: the fixture narrows a plain edit to its crate.
    write(&lib, "// v2\n");
    changed("a plain edit");

    // The same edit, hidden from `git diff` by the index flag.
    git(&root, &["checkout", "--", "crates/b/src/lib.rs"]);
    git(
        &root,
        &["update-index", "--skip-worktree", "crates/b/src/lib.rs"],
    );
    write(&lib, "// v2\n");
    assert_eq!(
        git(&root, &["status", "--porcelain"]),
        "",
        "the edit is hidden"
    );
    changed("a skip-worktree edit");
}

#[test]
fn a_selftest_in_a_checkout_with_an_unreadable_file_is_still_a_selftest() {
    // Round 2's early return for an unreadable source fired before the
    // --selftest ladder, so a selftest — which builds nothing and claims
    // nothing about the tree — read SELFTEST FAIL over one chmod-000 file.
    let repo = Fixture::new("atv-env-selftest");
    repo.git_init().with_targo("true");
    let secret = repo.root.join("secret.txt");
    write(&secret, "no one may read this\n");
    fs::set_permissions(&secret, fs::Permissions::from_mode(0o000)).expect("chmod");

    let plain = repo.ctx();
    let ctx = Ctx::new(
        plain.root.clone(),
        Mode::Fast,
        Scope::workspace(),
        true,
        plain.env.clone(),
        repo.scratch.clone(),
    );
    let (ladder, code) = repo.run(&ctx);
    fs::set_permissions(&secret, fs::Permissions::from_mode(0o644)).expect("chmod back");

    assert_eq!(code, exit::PASS, "{ladder}");
    assert!(ladder.contains("VERIFY: SELFTEST OK"), "{ladder}");
    assert!(!ladder.contains("=== source identity ==="), "{ladder}");
    assert!(!ladder.contains(MERGE_CONTRACT_SENTENCE), "{ladder}");
}

#[test]
fn a_compiler_rewritten_mid_run_never_claims_the_contract() {
    // Not a git checkout: the toolchain tripwire stands on its own.
    let repo = Fixture::new("atv-env-cc");
    script(
        &repo.stage2.join("trustc"),
        "echo 'rustc 1.99.0-dev (trustc 0.1.0)'; echo 'commit-hash: aaaaaaaaaaaa'",
    );
    // The toolchain resolves the physical path (`/tmp` is `/private/tmp` here).
    let trustc = fs::canonicalize(&repo.stage2)
        .expect("stage2")
        .join("trustc");
    repo.with_targo(&format!(
        "printf '#!/bin/sh\\necho commit-hash: bbbbbbbbbbbb\\n' > '{0}.new' && chmod 755 '{0}.new' && mv '{0}.new' '{0}'",
        trustc.display()
    ));

    let (calm, calm_code) = repo.run(&repo.ctx());
    assert_ne!(calm_code, exit::COULD_NOT_RUN, "{calm}");
    assert!(
        !calm.contains("verify: source"),
        "a non-git root prints no source line: {calm}"
    );

    repo.arm_trigger();
    let (ladder, code) = repo.run(&repo.ctx());
    assert_eq!(code, exit::COULD_NOT_RUN, "{ladder}");
    assert!(!ladder.contains(MERGE_CONTRACT_SENTENCE), "{ladder}");
    assert!(
        ladder.contains(&format!(
            "toolchain: {} was rewritten or replaced",
            trustc.display()
        )),
        "the compiler that moved is named: {ladder}"
    );
    assert!(ladder.contains("=== source identity ==="), "{ladder}");
}

/// A caller repo with every kind of uncommitted change a developer has.
struct Caller {
    base: PathBuf,
    root: PathBuf,
}

impl Caller {
    fn new(tag: &str) -> Self {
        let base = mktemp_dir(tag).expect("mktemp");
        let root = base.join("caller");
        fs::create_dir_all(&root).expect("mkdir");
        write(&root.join(".gitignore"), IGNORES);
        write(&root.join("a.txt"), "a1\n");
        write(&root.join("b.txt"), "b1\n");
        write(&root.join("sub/c.txt"), "c1\n");
        write(&root.join("run.sh"), "echo hi\n");
        git(&root, &["init", "-q"]);
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-q", "-m", "base"]);
        Self { base, root }
    }

    fn snap(&self) -> PathBuf {
        self.base.join("snap.noindex")
    }

    fn prepare(
        &self,
        env: &[(&str, &str)],
        commit: Option<&str>,
    ) -> Result<snapshot::Snapshot, String> {
        snapshot::prepare(&snapshot::Options {
            caller: &self.root,
            snapshot: self.snap(),
            path_env: &path_env(),
            lane_env: snapshot::lane_env(env.iter().map(|(k, v)| ((*k).into(), (*v).into()))),
            trustc_commit: commit.map(str::to_string),
        })
    }

    /// Everything a caller could lose: porcelain (index AND worktree) plus the
    /// bytes of every file that is not ignored build output.
    fn fingerprint(&self) -> (String, Vec<(String, Vec<u8>)>) {
        let porcelain = git(
            &self.root,
            &[
                "status",
                "--porcelain=v1",
                "--untracked-files=all",
                "--ignored",
            ],
        );
        let mut files = Vec::new();
        for p in [
            "a.txt",
            "b.txt",
            "sub/c.txt",
            "run.sh",
            "s.txt",
            "new/u.txt",
            "ignored.log",
        ] {
            files.push((
                p.to_string(),
                fs::read(self.root.join(p)).unwrap_or_default(),
            ));
        }
        (porcelain, files)
    }
}

impl Drop for Caller {
    fn drop(&mut self) {
        // The snapshot is a registered worktree of a repo under `base`; both go.
        fs::remove_dir_all(&self.base).ok();
    }
}

#[test]
fn the_snapshot_verifies_the_callers_head_and_diff_and_ignores_later_caller_moves() {
    let c = Caller::new("atv-env-snap");
    let head = git(&c.root, &["rev-parse", "HEAD"]);
    // Unstaged edit, staged edit, deletion, mode change, staged new file,
    // untracked file, untracked link, and an ignored file.
    write(&c.root.join("a.txt"), "a2\n");
    write(&c.root.join("b.txt"), "b2\n");
    git(&c.root, &["add", "b.txt"]);
    fs::remove_file(c.root.join("sub/c.txt")).expect("rm");
    fs::set_permissions(c.root.join("run.sh"), fs::Permissions::from_mode(0o755)).expect("chmod");
    write(&c.root.join("s.txt"), "s\n");
    git(&c.root, &["add", "s.txt"]);
    write(&c.root.join("new/u.txt"), "u\n");
    std::os::unix::fs::symlink("a.txt", c.root.join("link")).expect("symlink");
    write(&c.root.join("ignored.log"), "local only\n");
    let before = c.fingerprint();

    let s = c.prepare(&[], None).expect("snapshot");
    let snap = s.root.clone();

    // NEVER LOSES THE CALLER'S WORK: index, worktree and ignored files untouched.
    assert_eq!(c.fingerprint(), before, "the caller's checkout changed");

    assert_eq!(git(&snap, &["rev-parse", "HEAD"]), head);
    assert_eq!(fs::read_to_string(snap.join("a.txt")).expect("a"), "a2\n");
    assert_eq!(fs::read_to_string(snap.join("b.txt")).expect("b"), "b2\n");
    assert!(!snap.join("sub/c.txt").exists(), "the deletion is carried");
    assert_ne!(
        fs::metadata(snap.join("run.sh"))
            .expect("run.sh")
            .permissions()
            .mode()
            & 0o111,
        0
    );
    assert_eq!(fs::read_to_string(snap.join("s.txt")).expect("s"), "s\n");
    assert_eq!(
        fs::read_to_string(snap.join("new/u.txt")).expect("u"),
        "u\n"
    );
    assert_eq!(
        fs::read_link(snap.join("link")).expect("link"),
        PathBuf::from("a.txt")
    );
    assert!(
        !snap.join("ignored.log").exists(),
        "ignored files are not the caller's source"
    );

    let caller_tree = TreeState::capture(&c.root, &path_env()).expect("caller");
    assert_eq!(
        TreeState::capture(&snap, &path_env()).as_ref(),
        Some(&caller_tree)
    );
    assert_eq!(s.tree, caller_tree);

    // The header names the same sha and the same dirty digest the caller has.
    let wire = Tripwire::arm(
        &snap,
        &path_env(),
        ToolchainIdentity {
            files: vec![],
            commit: None,
        },
    );
    let digest = caller_tree.dirty_digest(&path_env()).expect("dirty");
    let line = wire
        .header_line(
            &SourceMode::Snapshot {
                caller: c.root.clone(),
            }
            .place(&snap),
        )
        .expect("a git root has a source line");
    assert!(
        line.starts_with(&format!(
            "verify: source {head}+dirty {} snapshot {}",
            &digest[..12],
            snap.display()
        )),
        "{line}"
    );

    // The caller moves on — a commit and an edit — while the run holds its snapshot.
    git(&c.root, &["commit", "-q", "-m", "caller moves on"]);
    write(&c.root.join("a.txt"), "a3\n");
    assert_eq!(
        wire.check(Duration::ZERO),
        None,
        "the snapshot did not move with the caller"
    );
    assert_eq!(fs::read_to_string(snap.join("a.txt")).expect("a"), "a2\n");

    // The next run follows the caller, and leaves an unchanged dirty file's
    // mtime alone so cargo does not rebuild its crate for nothing.
    let unchanged = snap.join("new/u.txt");
    let status = Command::new("touch")
        .args(["-t", "200001010000"])
        .arg(&unchanged)
        .status()
        .expect("touch");
    assert!(status.success());
    let old = mtime(&unchanged);
    s.finish();
    let s2 = c.prepare(&[], None).expect("second snapshot");
    assert_eq!(
        git(&snap, &["rev-parse", "HEAD"]),
        git(&c.root, &["rev-parse", "HEAD"])
    );
    assert_eq!(fs::read_to_string(snap.join("a.txt")).expect("a"), "a3\n");
    assert_eq!(
        mtime(&unchanged),
        old,
        "an unchanged dirty file was rewritten"
    );
    assert_eq!(
        s2.tree,
        TreeState::capture(&c.root, &path_env()).expect("caller")
    );
    s2.finish();
}

#[test]
fn a_copied_untracked_file_is_newer_than_its_sync_even_when_its_source_is_older() {
    // MEASURED 2026-09-13 (review of batch B): `std::fs::copy` keeps the source
    // mtime on macOS, so an untracked file copied in with DIFFERENT content and
    // an OLDER mtime than the lane's last build was judged fresh by cargo — the
    // build kept the old code while the source identity (content) matched.
    let c = Caller::new("atv-env-mtime");
    let src = c.root.join("new/u.txt");
    let old = |p: &Path| {
        let status = Command::new("touch")
            .args(["-t", "200001010000"])
            .arg(p)
            .status()
            .expect("touch");
        assert!(status.success());
    };
    write(&src, "v1\n");
    old(&src);
    // Read-only, so the stamp cannot depend on opening the copy for writing.
    fs::set_permissions(&src, fs::Permissions::from_mode(0o444)).expect("chmod");

    let before = std::time::SystemTime::now();
    let s = c.prepare(&[], None).expect("snapshot");
    let copied = s.root.join("new/u.txt");
    assert_eq!(fs::read_to_string(&copied).expect("copied"), "v1\n");
    assert!(
        mtime(&copied) >= before,
        "the copy kept its source's 2000 mtime: {:?}",
        mtime(&copied)
    );
    s.finish();

    // A content change whose source is OLDER than the snapshot's copy — what a
    // second checkout sharing the snapshot hands it — must still read as new.
    fs::set_permissions(&src, fs::Permissions::from_mode(0o644)).expect("chmod");
    write(&src, "v2\n");
    old(&src);
    let before = std::time::SystemTime::now();
    let s = c.prepare(&[], None).expect("resync");
    assert_eq!(fs::read_to_string(&copied).expect("copied"), "v2\n");
    assert!(
        mtime(&copied) >= before,
        "a changed file came in with an mtime older than the sync: {:?}",
        mtime(&copied)
    );
    s.finish();
}

#[test]
fn a_snapshot_run_arms_on_the_tree_its_sync_verified() {
    // Re-arming from scratch on the snapshot root would make anything that
    // landed there between the sync and the ladder the run's baseline.
    let c = Caller::new("atv-env-baseline");
    let s = c.prepare(&[], None).expect("snapshot");
    let none = || ToolchainIdentity {
        files: vec![],
        commit: None,
    };
    let armed = Tripwire::arm_against(&s.root, &path_env(), Some(s.tree.clone()), none());
    assert_eq!(armed.check(Duration::ZERO), None, "nothing moved yet");

    write(&s.root.join("stray.txt"), "landed after the sync\n");
    let from_scratch = Tripwire::arm(&s.root, &path_env(), none());
    assert_eq!(
        from_scratch.check(Duration::ZERO),
        None,
        "the hazard: a fresh arm adopts the stray file"
    );
    let armed = Tripwire::arm_against(&s.root, &path_env(), Some(s.tree.clone()), none());
    assert_eq!(armed.source, identity::SourceIdentity::Git(s.tree.clone()));
    let why = armed.check(Duration::ZERO).expect("tripped at arm");
    assert!(why.contains("stray.txt"), "{why}");
    s.finish();
}

#[test]
fn snapshot_sync_keeps_ignored_lane_dirs() {
    let c = Caller::new("atv-env-lanes");
    let s = c.prepare(&[], Some("c1")).expect("snapshot");
    let snap = s.root.clone();
    s.finish();

    // What a run leaves behind: warm lanes, plus a stray file and an edit a
    // stage wrote into the tree.
    write(&snap.join("target/debug/deps/libwarm.rlib"), "warm");
    write(&snap.join("target-regex/debug/deps/libwarm.rlib"), "warm");
    write(&snap.join("junk.txt"), "stray");
    write(&snap.join("a.txt"), "scribbled\n");

    let s = c.prepare(&[], Some("c1")).expect("resync");
    assert!(
        snap.join("target/debug/deps/libwarm.rlib").exists(),
        "git clean -x would have wiped target/"
    );
    assert!(snap.join("target-regex/debug/deps/libwarm.rlib").exists());
    assert!(
        !snap.join("junk.txt").exists(),
        "a stray file is not the caller's source"
    );
    assert_eq!(fs::read_to_string(snap.join("a.txt")).expect("a"), "a1\n");
    for lane in ["target", "target-regex"] {
        let stamp =
            fs::read_to_string(snap.join(lane).join(snapshot::STAMP_FILE)).expect("stamped");
        assert!(stamp.starts_with("trustc-commit c1\n"), "{stamp}");
    }
    assert!(s.notes.is_empty(), "same compiler, same env: {:?}", s.notes);
    s.finish();
}

#[test]
fn a_new_compiler_prunes_every_snapshot_lane_and_a_changed_variable_is_named() {
    let c = Caller::new("atv-env-prune");
    let s = c.prepare(&[], Some("c1")).expect("snapshot");
    let snap = s.root.clone();
    s.finish();
    write(&snap.join("target/debug/deps/libwarm.rlib"), "warm");
    let s = c.prepare(&[], Some("c1")).expect("stamp");
    s.finish();

    let s = c
        .prepare(&[("RUSTFLAGS", "-Zsomething")], Some("c1"))
        .expect("env change");
    assert!(
        s.notes
            .iter()
            .any(|n| n
                == "verify: lane target may rebuild cold — RUSTFLAGS changed since its last run"),
        "{:?}",
        s.notes
    );
    assert!(
        snap.join("target/debug/deps/libwarm.rlib").exists(),
        "an env change prunes nothing"
    );
    s.finish();

    let s = c
        .prepare(&[("RUSTFLAGS", "-Zsomething")], Some("c2"))
        .expect("new compiler");
    assert!(
        s.notes
            .iter()
            .any(|n| n.starts_with("verify: lane target pruned — trustc c1 -> c2")),
        "{:?}",
        s.notes
    );
    s.finish();
    assert!(
        !snap.join("target/debug").exists(),
        "the old compiler's artifacts are gone"
    );
    assert!(
        snap.join("target").join(snapshot::STAMP_FILE).exists(),
        "the lane is stamped afresh"
    );
    let trash = snap.join(identity::GATE_STATE_DIR).join("trash");
    assert_eq!(
        fs::read_dir(&trash).map(Iterator::count).unwrap_or(0),
        0,
        "deleted before exit"
    );
}

#[test]
fn a_second_gate_on_a_held_snapshot_is_could_not_run() {
    let c = Caller::new("atv-env-lock");
    let held = c.prepare(&[], None).expect("the first gate");

    let second = c
        .prepare(&[], None)
        .expect_err("a second gate on the same snapshot");
    assert!(second.contains("held by a running gate"), "{second}");
    assert!(
        second.contains(&format!("pid {}", std::process::id())),
        "{second}"
    );

    // End to end: the binary says COULD NOT RUN, exits 3, and runs no stage.
    let stage2 = c.base.join("stage2");
    script(&stage2.join("targo"), "echo \"argv: $*\"; exit 0");
    let out = Command::new(env!("CARGO_BIN_EXE_aterm-verify"))
        .args(["--fast", "--root"])
        .arg(&c.root)
        .env(snapshot::SNAPSHOT_ENV, c.snap())
        .env("TRUST_STAGE2_BIN", &stage2)
        .env_remove("ATERM_VERIFY_TIMINGS")
        .output()
        .expect("the gate binary runs");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(exit::COULD_NOT_RUN), "{stdout}");
    assert!(
        stdout.contains("  FAIL  snapshot: ") && stdout.contains("held by a running gate"),
        "{stdout}"
    );
    assert!(stdout.contains("VERIFY: COULD NOT RUN"), "{stdout}");
    assert!(!stdout.contains("=== build"), "no stage ran: {stdout}");
    assert!(!stdout.contains("argv:"), "no driver ran: {stdout}");

    // A lock whose gate is gone is broken, not obeyed forever.
    drop(held);
    let mut dead = Command::new("/bin/sh")
        .args(["-c", "exit 0"])
        .spawn()
        .expect("spawn");
    let dead_pid = dead.id();
    dead.wait().expect("reap");
    write(
        &c.snap().join(identity::GATE_STATE_DIR).join("lock"),
        &format!("pid {dead_pid}\nstarted 0\n"),
    );
    c.prepare(&[], None)
        .expect("a stale lock is broken")
        .finish();
}

/// A checkout's precious state: porcelain plus the bytes of the named files.
fn precious(dir: &Path, files: &[&str]) -> (String, Vec<Option<Vec<u8>>>) {
    (
        git(
            dir,
            &[
                "status",
                "--porcelain=v1",
                "--untracked-files=all",
                "--ignored",
            ],
        ),
        files.iter().map(|f| fs::read(dir.join(f)).ok()).collect(),
    )
}

#[test]
fn a_snapshot_path_naming_another_worktree_is_refused_and_untouched() {
    // MEASURED 2026-09-13 (review of batch B): `ATERM_VERIFY_SNAPSHOT=<sibling
    // worktree>` was accepted because it shares the caller's git common dir,
    // and the sync's `checkout --force` + `clean -fd` wiped its unstaged edit
    // and its untracked notes.
    let c = Caller::new("atv-env-sibling");
    let other = c.base.join("other");
    git(
        &c.root,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            other.to_str().expect("utf-8"),
            "HEAD",
        ],
    );
    write(&other.join("a.txt"), "PRECIOUS EDIT\n");
    write(&other.join("notes.txt"), "untracked notes\n");
    let files = ["a.txt", "notes.txt", "b.txt"];
    let before = precious(&other, &files);

    for target in [other.clone(), other.join("sub").join("snap")] {
        let err = snapshot::prepare(&snapshot::Options {
            caller: &c.root,
            snapshot: target.clone(),
            path_env: &path_env(),
            lane_env: snapshot::lane_env([]),
            trustc_commit: None,
        })
        .expect_err("another checkout is never a snapshot");
        assert!(err.contains("refusing"), "{}: {err}", target.display());
        assert_eq!(
            precious(&other, &files),
            before,
            "the sibling worktree was touched by {}",
            target.display()
        );
    }
    assert!(!other.join(identity::GATE_STATE_DIR).exists());

    // The gate's OWN snapshot is still accepted, and a deleted one whose
    // registration git still lists is recreated rather than refused.
    c.prepare(&[], None).expect("a fresh snapshot").finish();
    c.prepare(&[], None)
        .expect("the marked snapshot again")
        .finish();
    fs::remove_dir_all(c.snap()).expect("rm snapshot");
    c.prepare(&[], None)
        .expect("a deleted snapshot's stale registration is no checkout to protect")
        .finish();
}

#[test]
fn a_copy_of_a_marked_snapshot_is_refused_and_untouched() {
    // Review of batch B, round 2: the marker named the caller but not the
    // snapshot's own path, so a `cp -R` of a marked snapshot — whose `.git`
    // file still points at the original's admin dir, so it passes as a linked
    // worktree — was accepted and synced over, wiping whatever was in it.
    let c = Caller::new("atv-env-copy");
    c.prepare(&[], None).expect("the gate's snapshot").finish();
    let copy = c.base.join("copy.noindex");
    let copied = Command::new("cp")
        .arg("-R")
        .arg(c.snap())
        .arg(&copy)
        .status()
        .expect("cp runs");
    assert!(copied.success());
    write(&copy.join("a.txt"), "PRECIOUS EDIT IN THE COPY\n");
    write(&copy.join("notes.txt"), "untracked notes\n");
    let files = ["a.txt", "notes.txt", "b.txt"];
    let before = precious(&copy, &files);

    let err = snapshot::prepare(&snapshot::Options {
        caller: &c.root,
        snapshot: copy.clone(),
        path_env: &path_env(),
        lane_env: snapshot::lane_env([]),
        trustc_commit: None,
    })
    .expect_err("a copy of a snapshot is not that snapshot");
    assert!(err.contains("remove it"), "{err}");
    assert_eq!(precious(&copy, &files), before, "the copy was synced over");

    // The gate's own snapshot is still its own…
    c.prepare(&[], None).expect("the original").finish();
    // …and a marker that names no snapshot path (round 2's) is refused.
    let marker = c
        .snap()
        .join(identity::GATE_STATE_DIR)
        .join(snapshot::MARKER_FILE);
    write(
        &marker,
        &format!("aterm-verify snapshot\ncaller {}\n", c.root.display()),
    );
    write(&c.snap().join("a.txt"), "EDIT UNDER AN OLD MARKER\n");
    let err = c.prepare(&[], None).expect_err("an old marker is refused");
    assert!(err.contains("remove it"), "{err}");
    assert_eq!(
        fs::read_to_string(c.snap().join("a.txt")).expect("a"),
        "EDIT UNDER AN OLD MARKER\n"
    );
}

#[test]
fn a_snapshot_path_naming_the_main_checkout_is_refused_and_untouched() {
    // The same hazard from the other side: the caller is a linked worktree and
    // the snapshot path is the repository's MAIN checkout.
    let c = Caller::new("atv-env-main");
    let linked = c.base.join("linked");
    git(
        &c.root,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            linked.to_str().expect("utf-8"),
            "HEAD",
        ],
    );
    write(&c.root.join("a.txt"), "PRECIOUS EDIT\n");
    write(&c.root.join("notes.txt"), "untracked notes\n");
    let files = ["a.txt", "notes.txt", "b.txt"];
    let before = precious(&c.root, &files);

    let err = snapshot::prepare(&snapshot::Options {
        caller: &linked,
        snapshot: c.root.clone(),
        path_env: &path_env(),
        lane_env: snapshot::lane_env([]),
        trustc_commit: None,
    })
    .expect_err("the main checkout is never a snapshot");
    assert!(err.contains("refusing"), "{err}");
    assert_eq!(
        precious(&c.root, &files),
        before,
        "the main checkout was touched"
    );
    assert!(!c.root.join(identity::GATE_STATE_DIR).exists());
}

/// `  time  …` lines carry measurements, so they are masked; everything else
/// must be identical.
fn masked(ladder: &str) -> String {
    ladder
        .lines()
        .map(|l| {
            if l.starts_with("  time  ") {
                "  time  <masked>"
            } else {
                l
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn timings_do_not_change_a_byte_of_the_ladder() {
    let repo = Fixture::new("atv-env-timings");
    repo.with_targo("true");
    let (plain, plain_code) = repo.run(&repo.ctx());

    let tsv = repo.base.join("timings.tsv");
    let ctx = repo
        .ctx()
        .with_timings(Some(Timings::create(&tsv).expect("tsv")));
    let (timed, timed_code) = repo.run(&ctx);

    assert_eq!(timed_code, plain_code);
    assert_eq!(
        masked(&timed),
        masked(&plain),
        "timings leaked into the ladder"
    );

    // …and every stage says how long it took, on stdout, whatever the setting.
    let stages = plan::plan(&ctx).len();
    assert_eq!(
        plain.lines().filter(|l| l.starts_with("  time  ")).count(),
        stages,
        "{plain}"
    );

    let rows = fs::read_to_string(&tsv).expect("tsv written");
    let mut lines = rows.lines();
    assert_eq!(Some(Timings::HEADER.trim_end()), lines.next());
    let rows: Vec<Vec<&str>> = lines.map(|l| l.split('\t').collect()).collect();
    assert!(rows.iter().all(|r| r.len() == 8), "{rows:?}");
    assert_eq!(rows.iter().filter(|r| r[1] == "(stage)").count(), stages);
    let build = rows
        .iter()
        .find(|r| r[1].ends_with("targo --unverified build --workspace"))
        .expect("the build child has a row");
    assert!(
        build[0].starts_with("build ("),
        "attributed to its stage: {build:?}"
    );
    assert_eq!(build[2], "MainTarget");
    assert_eq!(build[5], "0");
    let (start, end): (f64, f64) = (
        build[3].parse().expect("start"),
        build[4].parse().expect("end"),
    );
    assert!(start <= end, "{build:?}");
}

#[test]
fn the_gates_own_channels_never_reach_a_child() {
    // A child that re-invoked the gate would otherwise write into this run's
    // timings TSV and queue on (or sync) this run's snapshot.
    let repo = Fixture::new("atv-env-channels");
    script(
        &repo.stage2.join("targo"),
        "echo \"child: $* tim=${ATERM_VERIFY_TIMINGS-unset} snap=${ATERM_VERIFY_SNAPSHOT-unset}\"; exit 0",
    );
    let out = Command::new(env!("CARGO_BIN_EXE_aterm-verify"))
        .args(["--fast", "--in-place", "--root"])
        .arg(&repo.root)
        .env("TRUST_STAGE2_BIN", &repo.stage2)
        .env("ATERM_SKIP_GUI_SMOKE", "1")
        .env("ATERM_VERIFY_TIMINGS", repo.base.join("timings.tsv"))
        .env(snapshot::SNAPSHOT_ENV, repo.base.join("unused-snapshot"))
        .output()
        .expect("the gate binary runs");
    let ladder = String::from_utf8_lossy(&out.stdout);
    assert!(
        ladder.contains("child: --unverified build --workspace tim=unset snap=unset"),
        "{ladder}"
    );
    assert!(
        repo.base.join("timings.tsv").exists(),
        "the gate itself still wrote it"
    );
}

#[test]
fn the_gate_binary_runs_its_ladder_in_the_snapshot_with_the_callers_target_dir_removed() {
    let repo = Fixture::new("atv-env-bin");
    repo.git_init();
    // Every targo child reports where it ran and which target dir it inherited.
    script(
        &repo.stage2.join("targo"),
        "echo \"child: $* cwd=$(pwd -P) ctd=${CARGO_TARGET_DIR-unset}\"; exit 0",
    );
    let head = git(&repo.root, &["rev-parse", "HEAD"]);
    let snap = fs::canonicalize(&repo.base)
        .expect("base")
        .join("repo-verify.noindex");

    let out = Command::new(env!("CARGO_BIN_EXE_aterm-verify"))
        .args(["--fast", "--root"])
        .arg(&repo.root)
        .env("TRUST_STAGE2_BIN", &repo.stage2)
        .env("CARGO_TARGET_DIR", repo.base.join("caller-target"))
        .env("ATERM_SKIP_GUI_SMOKE", "1")
        .env_remove(snapshot::SNAPSHOT_ENV)
        .env_remove("ATERM_VERIFY_TIMINGS")
        .output()
        .expect("the gate binary runs");
    let ladder = String::from_utf8_lossy(&out.stdout);

    assert!(
        ladder.starts_with("verify: toolchain "),
        "the first line is unchanged: {ladder}"
    );
    assert!(
        ladder.contains(&format!(
            "verify: source {head} snapshot {}",
            snap.display()
        )),
        "the default snapshot sits beside the checkout: {ladder}"
    );
    let build = format!(
        "child: --unverified build --workspace cwd={} ctd=unset",
        snap.display()
    );
    assert!(
        ladder.contains(&build),
        "the build ran in the snapshot, off the caller's target dir: {ladder}"
    );
    assert!(!ladder.contains("caller-target"), "{ladder}");
    // And the caller's checkout was only read.
    assert_eq!(git(&repo.root, &["status", "--porcelain"]), "");
    assert!(
        !snap.join(identity::GATE_STATE_DIR).join("lock").exists(),
        "released at exit"
    );
}

/// A caller whose HEAD commits `link` as a symlink to `target`, then hides
/// (skip-worktree) that it is now a real directory holding an untracked `x`.
fn symlink_swapped_for_a_directory(tag: &str, target: &dyn Fn(&Path) -> PathBuf) -> Caller {
    let c = Caller::new(tag);
    std::os::unix::fs::symlink(target(&c.base), c.root.join("link")).expect("symlink");
    git(&c.root, &["add", "link"]);
    git(&c.root, &["commit", "-q", "-m", "link"]);
    git(&c.root, &["update-index", "--skip-worktree", "link"]);
    fs::remove_file(c.root.join("link")).expect("rm link");
    write(&c.root.join("link/x"), "caller-dir-file\n");
    assert_eq!(git(&c.root, &["status", "--porcelain"]), "");
    c
}

#[test]
fn a_sync_never_writes_through_a_symlink_in_the_snapshot() {
    // MEASURED 2026-09-13 (review of batch B, round 3): the flag hid the
    // typechange, so the snapshot kept HEAD's `link` symlink, and copying the
    // caller's untracked `link/x` into it followed the link and overwrote a file
    // outside both checkouts.
    for (tag, absolute) in [
        ("atv-env-symlink-abs", true),
        ("atv-env-symlink-rel", false),
    ] {
        // Both victims sit beside the checkouts, under the caller's own base
        // (`<base>/snap.noindex` is the snapshot), so a failing run's cleanup
        // takes them too. `../victim` resolves, from the snapshot, into its
        // own parent directory.
        let c = if absolute {
            symlink_swapped_for_a_directory(tag, &|base| base.join("outside"))
        } else {
            symlink_swapped_for_a_directory(tag, &|_| PathBuf::from("../victim"))
        };
        let victim = c.base.join(if absolute { "outside" } else { "victim" });
        write(&victim.join("x"), "OUTSIDE PRECIOUS\n");
        let err = c
            .prepare(&[], None)
            .expect_err("a sync that would write through a symlink fails closed");
        assert_eq!(
            fs::read(victim.join("x")).expect("victim"),
            b"OUTSIDE PRECIOUS\n",
            "{tag}: a file outside the snapshot was written: {err}"
        );
        assert_eq!(
            fs::read_dir(&victim).expect("victim dir").count(),
            1,
            "{tag}: nothing was added beside it"
        );
        assert!(
            err.contains("symbolic link") && err.contains("link"),
            "{tag}: the link is named: {err}"
        );
        assert_eq!(
            fs::read_to_string(c.root.join("link/x")).expect("caller"),
            "caller-dir-file\n",
            "{tag}: the caller's file is kept"
        );
    }
}
