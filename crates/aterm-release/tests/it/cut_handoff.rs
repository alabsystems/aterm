// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE HANDOFF, END TO END — the real `aterm-release` binary, cutting a scratch
//! repository whose published commit its own cutter did not build.
//!
//! A real cut builds the commit `pub publish` recorded, in the cut tree beside the
//! operator's checkout, and a cutter whose sources differ from that commit's hands
//! the cut to the cutter built there (`publish::run_as_the_trees_cutter`). The pure
//! pieces are unit tests in `publish.rs`; this drives the whole path through the
//! binary's own `main`, with everything up to the handoff real:
//!
//! * a local bare `origin`, and a `git` shim on PATH that answers only
//!   `remote get-url origin` with a GitHub URL (the origin binding the cutter
//!   insists on) and hands every other call to the real git;
//! * a fake publication engine (`PUBLICATION_ENGINE`) whose ledger names an OLDER
//!   commit than the checkout's tip, as `pub publish` leaves it once peers push;
//! * a stub `targo` (`TRUST_STAGE2_BIN`) that records how it was called and writes,
//!   where the build would, a stub cutter that records how IT was called.
//!
//! What it proves: the build runs once, in the cut tree, into its own target dir; the
//! tree's cutter runs once, from the operator's checkout, with the same verb and the
//! marker naming the tree's commit; the operator's checkout never moves; and a cutter
//! started with that marker refuses rather than building again — the loop guard.
//! Hermetic: no network, no launchd, no real build.

use crate::publish;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const CARGO_TOML: &str = "[workspace]\nmembers = []\n\n[workspace.package]\nversion = \"0.92.0\"\n\
                          repository = \"https://github.com/o/r\"\n";

struct Scratch {
    root: PathBuf,
    /// The operator's checkout, on `main` at the tip.
    op: PathBuf,
    /// The commit the engine's ledger names — older than the tip.
    published: String,
    /// `main`'s tip, where the operator's checkout sits.
    tip: String,
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn executable(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// The real git, found before the shim is put in front of it.
fn real_git() -> PathBuf {
    std::env::var_os("PATH")
        .and_then(|path| {
            std::env::split_paths(&path)
                .map(|dir| dir.join("git"))
                .find(|candidate| candidate.is_file())
        })
        .expect("git on PATH")
}

fn scratch(label: &str) -> Scratch {
    let root = fs::canonicalize(std::env::temp_dir())
        .unwrap()
        .join(format!("aterm-cut-handoff-{label}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let op = root.join("op");
    fs::create_dir_all(&op).unwrap();
    git(&op, &["init", "-q", "-b", "main"]);
    for (key, value) in [
        ("user.name", "t"),
        ("user.email", "t@example.invalid"),
        ("commit.gpgsign", "false"),
    ] {
        git(&op, &["config", key, value]);
    }
    fs::write(op.join("Cargo.toml"), CARGO_TOML).unwrap();
    fs::write(op.join(".gitignore"), "/dist/\n/target/\n").unwrap();
    fs::write(op.join("lib.rs"), "published\n").unwrap();
    git(&op, &["add", "-A"]);
    git(&op, &["commit", "-q", "-m", "the published commit"]);
    let published = git(&op, &["rev-parse", "HEAD"]);
    fs::write(op.join("lib.rs"), "a peer's push\n").unwrap();
    git(
        &op,
        &["commit", "-q", "-am", "a peer's push after the publish"],
    );
    let tip = git(&op, &["rev-parse", "HEAD"]);
    let bare = root.join("origin.git");
    git(
        &root,
        &["clone", "-q", "--bare", op.to_str().unwrap(), "origin.git"],
    );
    git(&op, &["remote", "add", "origin", bare.to_str().unwrap()]);
    git(&op, &["fetch", "-q", "origin"]);

    let engine = root.join("engine");
    fs::create_dir_all(&engine).unwrap();
    fs::write(
        engine.join("mappings.json"),
        format!(
            r#"{{"schema":1,"mappings":{{"aterm":{{"{published}":{{"verified_at":"2026-09-23T10:00:00+00:00"}}}}}}}}"#
        ),
    )
    .unwrap();

    let bin = root.join("bin");
    fs::create_dir_all(&bin).unwrap();
    executable(
        &bin.join("git"),
        &format!(
            "#!/bin/sh\n\
             case \" $* \" in\n\
             *\" remote get-url origin \"*) echo https://github.com/o/r.git; exit 0 ;;\n\
             esac\n\
             exec '{}' \"$@\"\n",
            real_git().display()
        ),
    );
    let stage2 = root.join("stage2");
    fs::create_dir_all(&stage2).unwrap();
    fs::write(stage2.join("trustc"), "").unwrap();
    executable(
        &stage2.join("targo"),
        &format!(
            "#!/bin/sh\n\
             {{ echo \"cwd=$(pwd -P)\"; echo \"args=$*\"; echo \"target=$CARGO_TARGET_DIR\"; }} \
             >> '{log}/targo.log'\n\
             mkdir -p \"$CARGO_TARGET_DIR/release\"\n\
             cat > \"$CARGO_TARGET_DIR/release/aterm-release\" <<'CUTTER'\n\
             #!/bin/sh\n\
             {{ echo \"cwd=$(pwd -P)\"; echo \"args=$*\"; echo \"marker=$ATERM_CUT_REBUILT_FOR\"; }} \
             >> '{log}/cutter.log'\n\
             exit 0\n\
             CUTTER\n\
             chmod +x \"$CARGO_TARGET_DIR/release/aterm-release\"\n",
            log = root.display()
        ),
    );
    fs::create_dir_all(root.join("home")).unwrap();
    Scratch {
        root,
        op,
        published,
        tip,
    }
}

/// The real binary, run from the operator's checkout the way the launcher runs it.
fn cutter(s: &Scratch, args: &[&str], marker: Option<&str>) -> Output {
    let path = format!(
        "{}:{}",
        s.root.join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut command = Command::new(env!("CARGO_BIN_EXE_aterm-release"));
    command
        .args(args)
        .current_dir(&s.op)
        .env("PATH", path)
        .env("HOME", s.root.join("home"))
        .env("PUBLICATION_ENGINE", s.root.join("engine"))
        .env("TRUST_STAGE2_BIN", s.root.join("stage2"))
        .env_remove("ATERM_CUT_REBUILT_FOR");
    if let Some(commit) = marker {
        command.env("ATERM_CUT_REBUILT_FOR", commit);
    }
    command.output().unwrap()
}

fn log(s: &Scratch, name: &str) -> Option<String> {
    fs::read_to_string(s.root.join(name)).ok()
}

fn assert_operator_checkout_unmoved(s: &Scratch) {
    assert_eq!(git(&s.op, &["symbolic-ref", "HEAD"]), "refs/heads/main");
    assert_eq!(git(&s.op, &["rev-parse", "HEAD"]), s.tip);
    assert_eq!(
        fs::read_to_string(s.op.join("lib.rs")).unwrap(),
        "a peer's push\n"
    );
}

#[test]
fn a_cut_is_handed_to_the_published_commits_cutter_once_and_the_checkout_never_moves() {
    let s = scratch("fresh");
    let out = cutter(
        &s,
        &["cut", "--mac-only", "--min-build", "1790000001"],
        None,
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stdout:\n{stdout}\nstderr:\n{stderr}");

    let tree = s.root.join("op-cut.noindex");
    assert_eq!(
        log(&s, "targo.log").as_deref(),
        Some(
            format!(
                "cwd={}\nargs=--unverified build --release -p aterm-release\ntarget={}\n",
                tree.display(),
                tree.join("target").display()
            )
            .as_str()
        ),
        "one build, in the cut tree, into its own target dir\n{stdout}"
    );
    assert_eq!(
        log(&s, "cutter.log").as_deref(),
        Some(
            format!(
                "cwd={}\nargs=cut --min-build 1790000001\nmarker={}\n",
                s.op.display(),
                s.published
            )
            .as_str()
        ),
        "one run of the tree's cutter, from the operator's checkout, same verb, marked"
    );
    assert_eq!(git(&tree, &["rev-parse", "HEAD"]), s.published);
    assert_eq!(
        fs::read_to_string(tree.join("lib.rs")).unwrap(),
        "published\n",
        "the cut tree holds the published commit, not the tip"
    );
    assert_operator_checkout_unmoved(&s);
    assert!(stdout.contains("this checkout is not touched"), "{stdout}");
}

/// NEGATIVE CONTROL for the loop: the same cut, started as the rebuild of that very
/// commit (the marker names it), refuses — it never builds again.
#[test]
fn a_cutter_that_is_already_the_rebuild_refuses_instead_of_building_again() {
    let s = scratch("loop");
    let out = cutter(&s, &["cut"], Some(&s.published));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("is still not its own"), "{stderr}");
    assert!(stderr.contains("Nothing was claimed"), "{stderr}");
    assert!(log(&s, "targo.log").is_none(), "no second build");
    assert!(log(&s, "cutter.log").is_none(), "no second run");
    assert_operator_checkout_unmoved(&s);
}

/// A resume goes back to its journaled release commit in the cut tree and is handed
/// to THAT commit's cutter, as `cut --resume`.
#[test]
fn a_resume_is_handed_to_the_release_commits_cutter_in_the_cut_tree() {
    let s = scratch("resume");
    let journal = publish::Journal {
        format: publish::JOURNAL_FORMAT,
        version: "0.92.0".into(),
        build_number: 1_790_000_002,
        commit: s.published.clone(),
        min_build: None,
        arm64_only: false,
        manifest_signed: false,
        signature_required: false,
        signature_pubkey: None,
        verify_pubkey: None,
        signature_machine_id: None,
        release_id: None,
        release_intent: false,
        upload_intents: Vec::new(),
        linux: None,
        done: vec!["lock".into()],
    };
    fs::create_dir_all(s.op.join("dist")).unwrap();
    journal.save(&s.op.join("dist/cut-state.toml")).unwrap();

    let out = cutter(&s, &["cut", "--resume"], None);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stdout:\n{stdout}\nstderr:\n{stderr}");
    let cutter_log = log(&s, "cutter.log").unwrap();
    assert_eq!(
        cutter_log,
        format!(
            "cwd={}\nargs=cut --resume\nmarker={}\n",
            s.op.display(),
            s.published
        )
    );
    let tree = s.root.join("op-cut.noindex");
    assert_eq!(git(&tree, &["rev-parse", "HEAD"]), s.published);
    assert_operator_checkout_unmoved(&s);
}

/// A journal of ANOTHER format, as the cutter before publish-once wrote it (format 10:
/// the private-origin step list), naming the release commit.
fn older_journal(s: &Scratch, done: &[&str]) {
    let done: Vec<String> = done.iter().map(|step| format!("{step:?}")).collect();
    fs::create_dir_all(s.op.join("dist")).unwrap();
    fs::write(
        s.op.join("dist/cut-state.toml"),
        format!(
            "format = 10\nversion = \"0.92.0\"\nbuild_number = 1790000002\n\
             commit = \"{}\"\ndraft_create_issued = true\nmirror_create_issued = false\n\
             done = [{}]\n",
            s.published,
            done.join(", ")
        ),
    )
    .unwrap();
}

/// THE JOURNAL IS FINISHED BY THE CUTTER THAT WROTE IT, whatever its format. A cut an
/// older cutter started and left unfinished is resumed — and abandoned — by the cutter
/// built at its claim commit: this cutter reads the journal's HEADER, puts the cut tree
/// there and hands the verb over, before it would refuse a format it does not read.
#[test]
fn an_older_format_journal_is_resumed_and_abandoned_by_its_own_cutter() {
    for (verb, args) in [
        ("resume", &["cut", "--resume"][..]),
        ("abandon", &["cut", "--abandon", "v0.92.0"][..]),
    ] {
        let s = scratch(&format!("older-{verb}"));
        older_journal(&s, &["lock", "build", "selfcheck", "draft", "upload"]);
        let out = cutter(&s, args, None);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success(),
            "{verb}: stdout:\n{stdout}\nstderr:\n{stderr}"
        );
        assert_eq!(
            log(&s, "cutter.log"),
            Some(format!(
                "cwd={}\nargs={}\nmarker={}\n",
                s.op.display(),
                args.join(" "),
                s.published
            )),
            "{verb}: handed to the claim's cutter, same verb\n{stdout}"
        );
        let tree = s.root.join("op-cut.noindex");
        assert_eq!(git(&tree, &["rev-parse", "HEAD"]), s.published, "{verb}");
        assert!(
            s.op.join("dist/cut-state.toml").is_file(),
            "{verb}: the journal is its cutter's to change"
        );
        assert_operator_checkout_unmoved(&s);
    }
}

/// A FINISHED journal of another format is history: a fresh cut clears it and goes on
/// (here, to its own handoff) instead of refusing a file whose cut released its lease.
/// NEGATIVE CONTROL: the same journal short of `unlock` refuses the fresh cut by name,
/// before anything is claimed or built.
#[test]
fn a_finished_older_journal_is_history_to_a_fresh_cut() {
    let s = scratch("older-finished");
    older_journal(
        &s,
        &[
            "lock",
            "build",
            "selfcheck",
            "draft",
            "upload",
            "preflip",
            "tag",
            "flip",
            "archive",
            "verify",
            "mirror",
            "unlock",
        ],
    );
    let out = cutter(
        &s,
        &["cut", "--mac-only", "--min-build", "1790000001"],
        None,
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(
        !s.op.join("dist/cut-state.toml").exists(),
        "the finished journal was cleared"
    );
    assert!(log(&s, "cutter.log").is_some(), "and the cut went on");

    let s = scratch("older-unfinished");
    older_journal(
        &s,
        &["lock", "build", "selfcheck", "draft", "upload", "preflip"],
    );
    let out = cutter(
        &s,
        &["cut", "--mac-only", "--min-build", "1790000001"],
        None,
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("a cut is already in progress")
            && stderr.contains("after step \"preflip\" by a format-10 cutter"),
        "{stderr}"
    );
    assert!(log(&s, "targo.log").is_none(), "nothing built");
    assert!(s.op.join("dist/cut-state.toml").is_file());
}
