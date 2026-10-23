// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The publication engine's hooks (`ship hook`), over scratch repositories.

use std::path::{Path, PathBuf};

use crate::hook::{
    Asked, PrePromote, Transforms, cutter_command, pre_promote, running_cutter_command,
    source_differs, transforms,
};
use crate::transforms::{Inputs, run};

const ACCOUNT: &str = concat!("aya", "tes");
const PRIVATE_ORG: &str = concat!("andrew", "yatesai");

/// env) skip macOS's xcrun trampoline.
#[derive(Clone)]
struct Git {
    env: Vec<(String, String)>,
}

impl Git {
    fn new(home: &Path) -> Self {
        Self {
            env: vec![
                ("PATH".into(), crate::git_first_path().into()),
                ("HOME".into(), home.to_string_lossy().into_owned()),
                ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
                ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
                ("XDG_CONFIG_HOME".into(), "/dev/null".into()),
                ("GIT_TERMINAL_PROMPT".into(), "0".into()),
                ("GIT_CONFIG_COUNT".into(), "4".into()),
                ("GIT_CONFIG_KEY_0".into(), "protocol.file.allow".into()),
                ("GIT_CONFIG_VALUE_0".into(), "always".into()),
                ("GIT_CONFIG_KEY_1".into(), "user.name".into()),
                ("GIT_CONFIG_VALUE_1".into(), "fixture".into()),
                ("GIT_CONFIG_KEY_2".into(), "user.email".into()),
                (
                    "GIT_CONFIG_VALUE_2".into(),
                    "fixture@example.invalid".into(),
                ),
                ("GIT_CONFIG_KEY_3".into(), "maintenance.auto".into()),
                ("GIT_CONFIG_VALUE_3".into(), "false".into()),
            ],
        }
    }

    fn ok(&self, dir: &Path, args: &[&str]) -> String {
        let o = crate::ledger::testing::git()
            .arg("-C")
            .arg(dir)
            .args(args)
            .envs(self.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git {args:?} in {}: {}",
            dir.display(),
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }

    fn init(&self, dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        self.ok(dir, &["init", "-q", "-b", "main"]);
    }

    fn commit_all(&self, dir: &Path, msg: &str) -> String {
        self.ok(dir, &["add", "-A"]);
        self.ok(dir, &["commit", "-q", "-m", msg]);
        self.ok(dir, &["rev-parse", "HEAD"])
    }
}

fn scratch() -> (aterm_tempfile::TempDir, PathBuf, Git) {
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-hooks-")
        .tempdir()
        .unwrap();
    let root = dir.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join("home")).unwrap();
    let git = Git::new(&root.join("home"));
    (dir, root, git)
}

/// records `TWO`, and whose working tree holds an uncommitted `THREE`.
fn repo_with_history(root: &Path, git: &Git) -> (PathBuf, String) {
    let sub = root.join("sub");
    git.init(&sub);
    std::fs::write(sub.join("lib.rs"), "// sub\n").unwrap();
    git.commit_all(&sub, "sub");
    let repo = root.join("repo");
    git.init(&repo);
    std::fs::write(repo.join("state"), "ONE\n").unwrap();
    git.ok(
        &repo,
        &[
            "submodule",
            "add",
            "-q",
            &sub.to_string_lossy(),
            "vendor/sub",
        ],
    );
    let first = git.commit_all(&repo, "one");
    std::fs::write(repo.join("state"), "TWO\n").unwrap();
    git.commit_all(&repo, "two");
    std::fs::write(repo.join("state"), "THREE\n").unwrap();
    (repo, first)
}

fn worktrees(git: &Git, repo: &Path) -> usize {
    git.ok(repo, &["worktree", "list", "--porcelain"])
        .lines()
        .filter(|l| l.starts_with("worktree "))
        .count()
}

/// and the check's verdict is the hook's. The worktree is gone afterwards.
#[test]
fn pre_promote_asks_the_commits_own_tree_and_leaves_no_worktree() {
    let (_dir, root, git) = scratch();
    let (repo, first) = repo_with_history(&root, &git);
    for (target, want_target, verdict) in [
        (None, repo.join("target/pre-promote.noindex"), 1),
        (
            Some(PathBuf::from("build/shared")),
            repo.join("build/shared/pre-promote.noindex"),
            0,
        ),
        (
            Some(root.join("elsewhere")),
            root.join("elsewhere/pre-promote.noindex"),
            1,
        ),
    ] {
        let facts = PrePromote {
            commit: Some(first[..10].to_string()),
            version: Some("0.104.0".into()),
            target,
            cutter: None,
            git_env: git.env.clone(),
        };
        let mut seen: Option<(PathBuf, String, PathBuf)> = None;
        let mut check = |asked: Asked<'_>, full: &str| -> Result<i32, String> {
            let Asked::Built { tree, target } = asked else {
                panic!("no running cutter was named: {asked:?}");
            };
            assert_eq!(git.ok(tree, &["rev-parse", "HEAD"]), full);
            assert_eq!(
                std::fs::read_to_string(tree.join("state")).unwrap(),
                "ONE\n"
            );
            assert_eq!(
                std::fs::read_to_string(tree.join("vendor/sub/lib.rs")).unwrap(),
                "// sub\n"
            );
            seen = Some((tree.to_path_buf(), full.to_string(), target.to_path_buf()));
            Ok(verdict)
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = pre_promote(&facts, &repo.join("vendor"), &mut check, &mut out, &mut err);
        assert_eq!(code, verdict, "{}", String::from_utf8_lossy(&err));
        let (tree, full, target) = seen.expect("the check ran");
        assert_eq!((full.as_str(), target), (first.as_str(), want_target));
        assert!(!tree.exists(), "the worktree is removed");
        assert_eq!(worktrees(&git, &repo), 1);
        assert!(String::from_utf8_lossy(&out).contains(&format!("{} (v0.104.0)", &first[..12])));
    }
    assert_eq!(
        std::fs::read_to_string(repo.join("state")).unwrap(),
        "THREE\n",
        "the working tree is untouched"
    );
}

/// another cutter. It runs as from the operator's shell.
#[test]
fn pre_promote_asks_the_running_releases_cutter_and_builds_nothing() {
    let (_dir, root, git) = scratch();
    let (repo, first) = repo_with_history(&root, &git);
    let cutter = root.join("aterm-release");
    std::fs::write(&cutter, "#!/bin/sh\nexit 0\n").unwrap();
    let plain = root.join("not-executable");
    std::fs::write(&plain, "#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&cutter, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    let facts = |named: &Path| PrePromote {
        commit: Some(first.clone()),
        version: Some("0.104.0".into()),
        cutter: Some(named.to_path_buf()),
        git_env: git.env.clone(),
        ..PrePromote::default()
    };
    let mut seen = None;
    let mut check = |asked: Asked<'_>, full: &str| {
        seen = Some(format!("{asked:?} {full}"));
        Ok(1)
    };
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = pre_promote(
        &facts(&cutter),
        &repo.join("vendor"),
        &mut check,
        &mut out,
        &mut err,
    );
    assert_eq!(code, 1, "the check's verdict is the hook's");
    assert_eq!(
        seen.as_deref(),
        Some(
            format!(
                "{:?} {first}",
                Asked::Running {
                    cutter: &cutter,
                    root: &repo
                }
            )
            .as_str()
        )
    );
    assert_eq!(worktrees(&git, &repo), 1, "no worktree was made");
    assert!(String::from_utf8_lossy(&out).contains("the running release's own cutter"));

    let mut asked = false;
    let mut check = |_: Asked<'_>, _: &str| {
        asked = true;
        Ok(0)
    };
    for named in [plain, root.join("missing")] {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(
            pre_promote(&facts(&named), &repo, &mut check, &mut out, &mut err),
            2
        );
        assert!(
            String::from_utf8_lossy(&err).contains("is not an executable file"),
            "{}",
            String::from_utf8_lossy(&err)
        );
    }
    assert!(
        !asked,
        "a cutter that cannot run is never replaced by another"
    );

    let cmd = running_cutter_command(
        &cutter,
        &repo,
        &["check", "--commit", &first, "--cut"],
        ["CARGO", "TARGO_LANE", "PATH"].map(String::from),
    );
    assert_eq!(cmd.get_program(), cutter.as_os_str());
    assert_eq!(
        cmd.get_args().collect::<Vec<_>>(),
        ["check", "--commit", first.as_str(), "--cut"]
    );
    assert_eq!(cmd.get_current_dir(), Some(repo.as_path()));
    let removed: Vec<String> = cmd
        .get_envs()
        .filter(|(_, v)| v.is_none())
        .map(|(k, _)| k.to_string_lossy().into_owned())
        .collect();
    assert_eq!(removed, ["CARGO", "TARGO_LANE"]);
}

/// target directory, in the commit's worktree.
#[test]
fn the_commits_own_cutter_inherits_nothing_from_the_alias() {
    let inherited = [
        "CARGO",
        "CARGO_MANIFEST_DIR",
        "CARGO_PKG_NAME",
        "CARGO_PRIMARY_PACKAGE",
        "TARGO_LANE",
        "TRUST_TARGO_FRONTEND",
        "DYLD_FALLBACK_LIBRARY_PATH",
        "PATH",
        "HOME",
        "TRUST_STAGE2_BIN",
    ]
    .map(String::from);
    let cmd = cutter_command(
        Path::new("/pin/targo"),
        Path::new("/scratch/tree"),
        &["check", "--commit", "abc"],
        Path::new("/repo/target"),
        &[("ROOT", std::ffi::OsStr::new("/repo"))],
        inherited,
    );
    let removed: Vec<String> = cmd
        .get_envs()
        .filter(|(_, v)| v.is_none())
        .map(|(k, _)| k.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        removed,
        [
            "CARGO",
            "CARGO_MANIFEST_DIR",
            "CARGO_PKG_NAME",
            "CARGO_PRIMARY_PACKAGE",
            "DYLD_FALLBACK_LIBRARY_PATH",
            "TARGO_LANE",
            "TRUST_TARGO_FRONTEND",
        ]
    );
    let set: Vec<(String, String)> = cmd
        .get_envs()
        .filter_map(|(k, v)| {
            v.map(|v| {
                (
                    k.to_string_lossy().into_owned(),
                    v.to_string_lossy().into_owned(),
                )
            })
        })
        .collect();
    assert_eq!(
        set,
        [
            ("CARGO_TARGET_DIR".to_string(), "/repo/target".to_string()),
            ("ROOT".into(), "/repo".into()),
        ]
    );
    assert_eq!(cmd.get_program(), "/pin/targo");
    assert_eq!(
        cmd.get_args().collect::<Vec<_>>(),
        ["--unverified", "ship", "check", "--commit", "abc"]
    );
    assert_eq!(cmd.get_current_dir(), Some(Path::new("/scratch/tree")));
}

/// check that could not run.
#[test]
fn pre_promote_refuses_rather_than_check_some_other_code() {
    let (_dir, root, git) = scratch();
    let (repo, first) = repo_with_history(&root, &git);
    let cases = [
        (None, repo.clone(), "PUB_SOURCE_COMMIT is not set"),
        (Some("0".repeat(40)), repo.clone(), "names no commit in"),
        (
            Some(first.clone()),
            root.join("home"),
            "not run from inside a git checkout",
        ),
    ];
    for (commit, cwd, words) in cases {
        let facts = PrePromote {
            commit,
            git_env: git.env.clone(),
            ..PrePromote::default()
        };
        let mut asked = false;
        let mut check = |_: Asked<'_>, _: &str| {
            asked = true;
            Ok(0)
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(
            pre_promote(&facts, &cwd, &mut check, &mut out, &mut err),
            2,
            "{words}"
        );
        assert!(!asked, "{words}");
        assert!(
            String::from_utf8_lossy(&err).contains(words),
            "{}",
            String::from_utf8_lossy(&err)
        );
    }
    let facts = PrePromote {
        commit: Some(first),
        git_env: git.env.clone(),
        ..PrePromote::default()
    };
    let mut check = |_: Asked<'_>, _: &str| Err("cannot run targo: not found".to_string());
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(
        pre_promote(&facts, &repo, &mut check, &mut out, &mut err),
        2
    );
    assert_eq!(worktrees(&git, &repo), 1);
}

/// One astream fixture crate: `crates/<name>` with `deps` below its `[package]`.
fn astream_crate(dir: &Path, name: &str, deps: &str) {
    std::fs::create_dir_all(dir.join(format!("crates/{name}/src"))).unwrap();
    std::fs::write(
        dir.join(format!("crates/{name}/Cargo.toml")),
        format!("[package]\nname = \"{name}\"\n\n{deps}"),
    )
    .unwrap();
    std::fs::write(dir.join(format!("crates/{name}/src/lib.rs")), "// ONE\n").unwrap();
}

/// astream-hash and cap's path dependency on it.
fn astream(root: &Path, git: &Git) -> PathBuf {
    let dir = root.join("astream");
    git.init(&dir);
    std::fs::write(
        dir.join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/*\"]\n\n[workspace.dependencies]\nastream-wire = { \
         path = \"crates/astream-wire\" }\nastream-digest = { path = \"crates/astream-digest\", \
         version = \"0.2.0\" }\nserde = \"1.0\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("tests")).unwrap();
    std::fs::write(dir.join("tests/private.rs"), "// never exported\n").unwrap();
    for c in [
        "astream-wire",
        "astream-digest",
        "astream-aead",
        "astream-agent",
        "astream-term",
    ] {
        astream_crate(&dir, c, "");
    }
    let wire = dir.join("crates/astream-wire");
    std::fs::write(wire.join("build.rs"), "fn main() {}\n").unwrap();
    std::fs::write(wire.join("contract_fn.rs"), "// SHARED\n").unwrap();
    for d in ["tests", "examples", "benches"] {
        std::fs::create_dir_all(wire.join(d)).unwrap();
        std::fs::write(wire.join(format!("{d}/private.rs")), "// never exported\n").unwrap();
    }
    astream_crate(
        &dir,
        "astream-cap",
        "[dependencies]\nastream-wire = { workspace = true }\nastream-digest.workspace = true # \
         a dotted key\nserde = { workspace = true }\n",
    );
    astream_crate(
        &dir,
        "astream-broker",
        "[features]\ncap = [\"dep:astream-cap\"]\n\n[dependencies]\nastream-wire = { workspace = \
         true }\nastream-cap = { path = \"../astream-cap\", optional = true \
         }\n\n[target.'cfg(unix)'.dependencies.astream-aead]\npath = \
         \"../astream-aead\"\n\n[dev-dependencies]\nastream-agent = { path = \"../astream-agent\" \
         }\n",
    );
    git.commit_all(&dir, "one");
    std::fs::write(dir.join("crates/astream-wire/src/lib.rs"), "// TWO\n").unwrap();
    astream_crate(&dir, "astream-hash", "");
    let cap = dir.join("crates/astream-cap/Cargo.toml");
    let mut text = std::fs::read_to_string(&cap).unwrap();
    text.push_str("astream-hash = { path = \"../astream-hash\" }\n");
    std::fs::write(cap, text).unwrap();
    git.commit_all(&dir, "two");
    dir
}

/// The exported crate that path-depends on astream's `dep`, as aterm-link does.
fn link_crate(export: &Path, dep: &str) {
    let link = export.join("crates/link");
    std::fs::create_dir_all(&link).unwrap();
    std::fs::write(
        link.join("Cargo.toml"),
        format!(
            "[package]\nname = \"link\"\n\n[dependencies]\n{dep} = {{ path = \
             \"../../vendor/astream/crates/{dep}\" }}\n"
        ),
    )
    .unwrap();
}

/// The astream crates an export carries.
fn exported_astream(export: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(export.join("vendor/astream/crates"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

/// moved on to TWO; the captured sha.
fn superproject(root: &Path, git: &Git, astream: &Path) -> (PathBuf, String) {
    let sp = root.join("super");
    git.init(&sp);
    std::fs::write(sp.join("LICENSE"), "Apache-2.0 fixture\n").unwrap();
    git.ok(
        &sp,
        &[
            "submodule",
            "add",
            "-q",
            &astream.to_string_lossy(),
            "vendor/astream",
        ],
    );
    git.ok(&sp.join("vendor/astream"), &["checkout", "-q", "HEAD~1"]);
    let captured = git.commit_all(&sp, "gitlink ONE");
    git.ok(&sp.join("vendor/astream"), &["checkout", "-q", "main"]);
    git.commit_all(&sp, "gitlink TWO");
    (sp, captured)
}

/// for, and a crate declaring a target whose file the export withholds.
fn export_fixture(export: &Path) -> String {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for f in ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml"] {
        std::fs::copy(repo.join(f), export.join(f)).unwrap();
    }
    std::fs::create_dir_all(export.join("crates/atpkg-publish/src/artifact")).unwrap();
    std::fs::copy(
        repo.join("crates/atpkg-publish/src/artifact/relocate.rs"),
        export.join("crates/atpkg-publish/src/artifact/relocate.rs"),
    )
    .unwrap();
    let fx = export.join("crates/fixture");
    std::fs::create_dir_all(fx.join("src")).unwrap();
    std::fs::create_dir_all(fx.join("tests")).unwrap();
    let lib = format!(
        "const REPO: &str = \"https://github.com/{PRIVATE_ORG}/aterm\";\n\
         const DEV_HOME: &str = \"/Users/{ACCOUNT}/aterm/target\";\n\
         const OTHER_HOME: &str = \"/Users//someone/Library\";\n\
         const TOOLCHAIN: &str = \"{}/build\";\n\
         const CLEAN: &str = \"{}/bin\";\n\
         const PROMPT: &[u8] = b\"{ACCOUNT}@mac ~ % \";\n",
        concat!("~", "/trust"),
        concat!("~", "/clean"),
    );
    std::fs::write(fx.join("src/lib.rs"), &lib).unwrap();
    std::fs::write(
        fx.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nautotests = false\n\n[[test]]\nname = \"suite\"\npath = \
         \"tests/suite.rs\"\n\n# The withheld one: its file is not in the export.\n[[test]]\nname \
         = \"withheld\"\npath = \"tests/withheld.rs\"\n\n[lints]\nworkspace = true\n",
    )
    .unwrap();
    std::fs::write(fx.join("tests/suite.rs"), "#[test]\nfn t() {}\n").unwrap();
    link_crate(export, "astream-broker");
    let oracle = export.join("crates/oracle");
    std::fs::create_dir_all(&oracle).unwrap();
    std::fs::write(
        oracle.join("Cargo.toml"),
        "[package]\nname = \"oracle\"\n\n[dev-dependencies]\nastream-term = { path = \
         \"../../vendor/astream/crates/astream-term\" }\n",
    )
    .unwrap();
    lib
}

/// Every guarded shape the rewrites answer to, found in `text`.
fn guarded(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let bytes = text.as_bytes();
    for (i, _) in text.match_indices("/Users/") {
        if bytes.get(i + 7).is_some_and(u8::is_ascii_alphabetic) {
            found.push(format!("/Users/{}", char::from(bytes[i + 7])));
        }
    }
    for needle in [
        concat!("~", "/trust"),
        concat!("~", "/clean"),
        ACCOUNT,
        PRIVATE_ORG,
    ] {
        if text.contains(needle) {
            found.push(needle.to_string());
        }
    }
    found
}

fn tree_text(dir: &Path, out: &mut String) {
    for e in std::fs::read_dir(dir).unwrap() {
        let e = e.unwrap();
        if e.file_type().unwrap().is_dir() {
            tree_text(&e.path(), out);
        } else if let Ok(t) = std::fs::read_to_string(e.path()) {
            out.push_str(&t);
        }
    }
}

/// withheld is dropped with its comment.
#[test]
fn the_transforms_write_astream_from_the_exported_commit_and_leave_no_guarded_shape() {
    let (_dir, root, git) = scratch();
    let astream = astream(&root, &git);
    let (sp, captured) = superproject(&root, &git, &astream);
    let export = root.join("export");
    std::fs::create_dir_all(&export).unwrap();
    let lib = export_fixture(&export);
    let committed_toml = std::fs::read_to_string(export.join("Cargo.toml")).unwrap();
    let workspace_version = committed_toml
        .lines()
        .skip_while(|l| *l != "[workspace.package]")
        .find_map(|l| l.strip_prefix("version = \""))
        .and_then(|v| v.strip_suffix('"'))
        .unwrap()
        .to_string();
    let dev_version = format!("{}.7", workspace_version.rsplit_once('.').unwrap().0);
    std::fs::write(
        export.join("Cargo.toml"),
        committed_toml.replacen(
            &format!("version = \"{workspace_version}\""),
            &format!("version = \"{dev_version}\""),
            1,
        ),
    )
    .unwrap();
    let toml_before = std::fs::read_to_string(export.join("Cargo.toml")).unwrap();
    let lock_before = std::fs::read_to_string(export.join("Cargo.lock")).unwrap();
    let pin_before = std::fs::read(export.join("rust-toolchain.toml")).unwrap();
    assert_eq!(guarded(&lib).len(), 6, "{:?}", guarded(&lib));

    let said = run(&Inputs {
        root: sp.clone(),
        export: export.clone(),
        captured: captured.clone(),
    })
    .unwrap();

    let v = export.join("vendor/astream");
    assert_eq!(
        std::fs::read_to_string(v.join("crates/astream-wire/src/lib.rs")).unwrap(),
        "// ONE\n"
    );
    let reached = [
        "astream-aead",
        "astream-broker",
        "astream-cap",
        "astream-digest",
        "astream-term",
        "astream-wire",
    ];
    assert_eq!(exported_astream(&export), reached);
    for c in reached {
        assert!(v.join(format!("crates/{c}/Cargo.toml")).is_file(), "{c}");
    }
    assert!(
        said[0].ends_with(&format!(": {}", reached.join(" "))),
        "{said:?}"
    );
    assert!(v.join("Cargo.toml").is_file() && v.join("LICENSE").is_file());
    assert!(!v.join("tests").exists(), "astream's tests stay private");
    let wire = v.join("crates/astream-wire");
    assert_eq!(
        std::fs::read_to_string(wire.join("contract_fn.rs")).unwrap(),
        "// SHARED\n"
    );
    assert!(wire.join("build.rs").is_file());
    for d in ["tests", "examples", "benches"] {
        assert!(!wire.join(d).exists(), "a crate's {d}/ stays private");
    }
    let rev = git.ok(&sp, &["ls-tree", &captured, "--", "vendor/astream"]);
    let rev = rev.split_whitespace().nth(2).unwrap();
    let readme = std::fs::read_to_string(v.join("README.md")).unwrap();
    assert!(readme.contains(rev), "{readme}");
    assert!(
        readme.contains(
            "`astream-aead`, `astream-broker`, `astream-cap`, `astream-digest`, `astream-term`, \
             `astream-wire`"
        ),
        "{readme}"
    );
    assert!(!export.join(".gitmodules").exists());

    assert_eq!(
        std::fs::read_to_string(export.join("crates/fixture/src/lib.rs")).unwrap(),
        "const REPO: &str = \"https://github.com/alabsystems/aterm\";\n\
         const DEV_HOME: &str = \"/Users//example/aterm/target\";\n\
         const OTHER_HOME: &str = \"/Users//someone/Library\";\n\
         const TOOLCHAIN: &str = \"$HOME/trust/build\";\n\
         const CLEAN: &str = \"$HOME/clean/bin\";\n\
         const PROMPT: &[u8] = b\"user@mac ~ % \";\n"
    );
    let mut all = String::new();
    tree_text(&export, &mut all);
    assert_eq!(guarded(&all), Vec::<String>::new());

    let fixture_toml = std::fs::read_to_string(export.join("crates/fixture/Cargo.toml")).unwrap();
    assert_eq!(
        fixture_toml,
        "[package]\nname = \"fixture\"\nautotests = false\n\n[[test]]\nname = \"suite\"\npath = \
         \"tests/suite.rs\"\n\n[lints]\nworkspace = true\n"
    );
    assert!(said.contains(&"  target-strip: crates/fixture/Cargo.toml [[test]] withheld (its file is not exported)".to_string()), "{said:?}");

    let relocate =
        std::fs::read_to_string(export.join("crates/atpkg-publish/src/artifact/relocate.rs"))
            .unwrap();
    assert!(
        relocate.contains("path.starts_with(\"/Users/\")"),
        "the machine-local prefix ships as written"
    );
    assert_eq!(
        std::fs::read(export.join("rust-toolchain.toml")).unwrap(),
        pin_before
    );
    assert_eq!(
        std::fs::read_to_string(export.join("Cargo.toml")).unwrap(),
        toml_before.replace(PRIVATE_ORG, "alabsystems")
    );
    assert_eq!(
        std::fs::read_to_string(export.join("Cargo.lock")).unwrap(),
        lock_before.replace(PRIVATE_ORG, "alabsystems")
    );
    assert!(toml_before.contains(&format!("version = \"{dev_version}\"")));
}

/// public clone resolves.
#[test]
fn the_astream_crates_are_derived_at_the_captured_gitlink() {
    let (_dir, root, git) = scratch();
    let astream = astream(&root, &git);
    let (sp, _) = superproject(&root, &git, &astream);
    let head = git.ok(&sp, &["rev-parse", "HEAD"]);
    let export = root.join("export");
    let fresh = |dep: &str| {
        let _ = std::fs::remove_dir_all(&export);
        std::fs::create_dir_all(&export).unwrap();
        link_crate(&export, dep);
    };
    fresh("astream-broker");
    run(&Inputs {
        root: sp.clone(),
        export: export.clone(),
        captured: head.clone(),
    })
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(export.join("vendor/astream/crates/astream-hash/src/lib.rs"))
            .unwrap(),
        "// ONE\n"
    );
    assert!(exported_astream(&export).contains(&"astream-hash".to_string()));

    fresh("astream-gone");
    let why = run(&Inputs {
        root: sp,
        export: export.clone(),
        captured: head,
    })
    .unwrap_err();
    assert!(
        why.contains("vendor/astream/crates/astream-gone, which astream"),
        "{why}"
    );
}

/// withheld is left byte for byte.
#[test]
fn a_withheld_package_leaves_the_exported_lock() {
    let (_dir, root, git) = scratch();
    let astream = astream(&root, &git);
    let (sp, _) = superproject(&root, &git, &astream);
    for (name, deps) in [
        ("app", ""),
        ("lab", "[dependencies]\napp = { path = \"../app\" }\n"),
    ] {
        std::fs::create_dir_all(sp.join(format!("crates/{name}"))).unwrap();
        std::fs::write(
            sp.join(format!("crates/{name}/Cargo.toml")),
            format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n\n{deps}"),
        )
        .unwrap();
    }
    let captured = git.commit_all(&sp, "app and lab");
    let lock = |lab: bool| {
        let lab = if lab {
            "[[package]]\nname = \"lab\"\nversion = \"0.1.0\"\ndependencies = [\n \"app\",\n \
             \"only-lab\",\n]\n\n"
        } else {
            ""
        };
        let only = if lab.is_empty() {
            ""
        } else {
            "\n[[package]]\nname = \"only-lab\"\nversion = \"1.0.0\"\nsource = \
             \"registry+https://github.com/rust-lang/crates.io-index\"\n"
        };
        format!(
            "# This file is automatically @generated by Cargo.\nversion = 4\n\n[[package]]\nname \
             = \"app\"\nversion = \"0.1.0\"\n\n{lab}[[package]]\nname = \"link\"\nversion = \
             \"0.1.0\"\n{only}"
        )
    };
    let export = root.join("export");
    std::fs::create_dir_all(export.join("crates/app")).unwrap();
    std::fs::copy(
        sp.join("crates/app/Cargo.toml"),
        export.join("crates/app/Cargo.toml"),
    )
    .unwrap();
    link_crate(&export, "astream-broker");
    std::fs::write(export.join("Cargo.lock"), lock(true)).unwrap();
    let said = run(&Inputs {
        root: sp.clone(),
        export: export.clone(),
        captured: captured.clone(),
    })
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(export.join("Cargo.lock")).unwrap(),
        lock(false)
    );
    for gone in ["  lock-prune: lab 0.1.0", "  lock-prune: only-lab 1.0.0"] {
        assert!(said.contains(&gone.to_string()), "{said:?}");
    }

    std::fs::create_dir_all(export.join("crates/lab")).unwrap();
    std::fs::copy(
        sp.join("crates/lab/Cargo.toml"),
        export.join("crates/lab/Cargo.toml"),
    )
    .unwrap();
    std::fs::write(export.join("Cargo.lock"), lock(true)).unwrap();
    run(&Inputs {
        root: sp,
        export: export.clone(),
        captured,
    })
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(export.join("Cargo.lock")).unwrap(),
        lock(true)
    );
}

/// exported commit but not astream's objects: refused by name, not staged from.
#[test]
fn an_uninitialized_astream_checkout_is_refused_by_name() {
    let (_dir, root, git) = scratch();
    let astream = astream(&root, &git);
    let (sp, captured) = superproject(&root, &git, &astream);
    let plain = root.join("super-uninit");
    git.init(&plain);
    std::fs::write(plain.join("LICENSE"), "Apache-2.0 fixture\n").unwrap();
    std::fs::create_dir_all(plain.join("vendor/astream")).unwrap();
    std::fs::write(plain.join("vendor/astream/README.md"), "vendored\n").unwrap();
    git.commit_all(&plain, "pre-submodule branch");
    git.ok(&plain, &["fetch", "-q", &sp.to_string_lossy(), &captured]);
    let export = root.join("export");
    std::fs::create_dir_all(&export).unwrap();
    let why = run(&Inputs {
        root: plain.clone(),
        export,
        captured,
    })
    .unwrap_err();
    assert!(
        why.contains(&format!(
            "vendor/astream is not an initialized submodule in {}",
            plain.display()
        )),
        "{why}"
    );
}

/// and whose third's say `TWO`, beside a lockfile and a README: (repo, [c0, c1, c2]).
fn cutter_history(root: &Path, git: &Git) -> (PathBuf, [String; 3]) {
    let repo = root.join("cutter");
    git.init(&repo);
    std::fs::write(repo.join("README.md"), "readme\n").unwrap();
    std::fs::write(repo.join("Cargo.lock"), "# lock\n").unwrap();
    let c0 = git.commit_all(&repo, "no cutter yet");
    let rules = repo.join("crates/aterm-release/src");
    std::fs::create_dir_all(&rules).unwrap();
    std::fs::write(rules.join("transforms.rs"), "// ONE\n").unwrap();
    let c1 = git.commit_all(&repo, "rules ONE");
    std::fs::write(rules.join("transforms.rs"), "// TWO\n").unwrap();
    let c2 = git.commit_all(&repo, "rules TWO");
    (repo, [c0, c1, c2])
}

/// commit's own cutter asked (its worktree's rules, the sha, the target), or neither.
#[derive(Debug, PartialEq, Eq)]
enum Ran {
    Here(String),
    There {
        rules: String,
        full: String,
        target: PathBuf,
    },
    Neither,
}

fn dispatch(facts: &Transforms) -> (i32, Ran, String) {
    let mut here_ran = None;
    let mut there_ran = None;
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = transforms(
        facts,
        &mut |inputs: &Inputs| {
            here_ran = Some(inputs.captured.clone());
            Ok(vec!["== rewritten".to_string()])
        },
        &mut |tree: &Path, full: &str, target: &Path| {
            let rules = tree.join("crates/aterm-release/src/transforms.rs");
            there_ran = Some((
                tree.to_path_buf(),
                std::fs::read_to_string(rules).unwrap(),
                full.to_string(),
                target.to_path_buf(),
            ));
            Ok(0)
        },
        &mut out,
        &mut err,
    );
    let ran = match (here_ran, there_ran) {
        (Some(sha), None) => Ran::Here(sha),
        (None, Some((tree, rules, full, target))) => {
            assert!(
                !tree.exists(),
                "the worktree is removed: {}",
                tree.display()
            );
            Ran::There {
                rules,
                full,
                target,
            }
        }
        (None, None) => Ran::Neither,
        both => panic!("ran both ways: {both:?}"),
    };
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&out),
        String::from_utf8_lossy(&err)
    );
    (code, ran, said)
}

/// this working tree's code.
#[test]
fn the_transforms_run_as_the_captured_commits_own_code() {
    let (_dir, root, git) = scratch();
    let (repo, [_, c1, c2]) = cutter_history(&root, &git);
    let facts = |captured: &str| Transforms {
        inputs: Inputs {
            root: repo.clone(),
            export: root.join("export"),
            captured: captured.to_string(),
        },
        source: repo.join("crates/aterm-release"),
        target: Some(PathBuf::from("build/shared")),
        delegated: None,
        git_env: git.env.clone(),
    };
    let rules = repo.join("crates/aterm-release/src/transforms.rs");
    let there = |rules: &str, full: &str| Ran::There {
        rules: rules.into(),
        full: full.into(),
        target: repo.join("build/shared/pre-promote.noindex"),
    };

    let (code, ran, said) = dispatch(&facts("HEAD"));
    assert_eq!((code, ran), (0, Ran::Here(c2.clone())), "{said}");
    assert!(said.contains("== rewritten"), "{said}");
    std::fs::write(repo.join("README.md"), "edited\n").unwrap();
    assert_eq!(dispatch(&facts(&c2)).1, there("// TWO\n", &c2));
    std::fs::write(repo.join("README.md"), "readme\n").unwrap();

    for (edit, undo) in [
        (
            Box::new(|| std::fs::write(&rules, "// WIP\n").unwrap()) as Box<dyn Fn()>,
            Box::new(|| std::fs::write(&rules, "// TWO\n").unwrap()) as Box<dyn Fn()>,
        ),
        (
            Box::new(|| std::fs::write(rules.with_file_name("new.rs"), "\n").unwrap()),
            Box::new(|| std::fs::remove_file(rules.with_file_name("new.rs")).unwrap()),
        ),
        (
            Box::new(|| std::fs::write(repo.join("Cargo.lock"), "# moved\n").unwrap()),
            Box::new(|| std::fs::write(repo.join("Cargo.lock"), "# lock\n").unwrap()),
        ),
    ] {
        edit();
        let (code, ran, said) = dispatch(&facts(&c2));
        assert_eq!((code, ran), (0, there("// TWO\n", &c2)), "{said}");
        assert!(
            said.contains(&format!(
                "running {}'s own `ship hook transforms`",
                &c2[..12]
            )),
            "{said}"
        );
        undo();
    }
    assert_eq!(source_differs(&repo, &git.env, &c2), None);

    let (code, ran, said) = dispatch(&facts(&c1[..10]));
    assert_eq!((code, ran), (0, there("// ONE\n", &c1)), "{said}");
    assert_eq!(worktrees(&git, &repo), 1);
    assert_eq!(std::fs::read_to_string(&rules).unwrap(), "// TWO\n");
}

/// differs (it would only hand the export on again).
#[test]
fn the_transforms_refuse_rather_than_run_other_rules() {
    let (_dir, root, git) = scratch();
    let (repo, [c0, c1, _]) = cutter_history(&root, &git);
    let facts = |captured: &str, delegated: Option<&str>| Transforms {
        inputs: Inputs {
            root: repo.clone(),
            export: root.join("export"),
            captured: captured.to_string(),
        },
        source: repo.join("crates/aterm-release"),
        target: None,
        delegated: delegated.map(str::to_string),
        git_env: git.env.clone(),
    };
    for (facts, words) in [
        (facts(&"0".repeat(40), None), "names no commit in"),
        (
            facts(&c0, None),
            "has no `ship hook transforms` (crates/aterm-release/src/transforms.rs is not in it)",
        ),
        (
            facts(&c1, Some(&c1)),
            "own cutter does not find its source to be",
        ),
    ] {
        let (code, ran, said) = dispatch(&facts);
        assert_eq!((code, ran), (1, Ran::Neither), "{said}");
        assert!(said.contains(words), "{words}: {said}");
    }
    assert_eq!(worktrees(&git, &repo), 1);
    let (code, ran, _) = dispatch(&facts(&c1, Some(&"f".repeat(40))));
    assert_eq!(code, 0);
    assert!(matches!(ran, Ran::There { .. }), "{ran:?}");
}

/// one ignored one: the captured worktree, then the identical checkout again.
#[test]
fn export_cutter_attests_dependency_sources_and_workspace_inputs() {
    let (_dir, root, git) = scratch();
    let (repo, _) = cutter_history(&root, &git);
    let tracked = [
        "crates/aterm-toml/src/lib.rs",
        "crates/aterm-json/src/lib.rs",
        "crates/aterm-uds/src/lib.rs",
        "crates/aterm-toml/build.rs",
        "Cargo.toml",
        "rust-toolchain.toml",
        ".cargo/config.toml",
    ];
    for path in tracked {
        let path = repo.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "# captured input\n").unwrap();
    }
    std::fs::write(
        repo.join(".gitignore"),
        ".cargo/local.toml\ncrates/aterm-toml/src/local.rs\ntarget/\n",
    )
    .unwrap();
    let captured = git.commit_all(&repo, "the cutter's effective source inputs");
    let facts = Transforms {
        inputs: Inputs {
            root: repo.clone(),
            export: root.join("export"),
            captured: captured.clone(),
        },
        source: repo.join("crates/aterm-release"),
        target: Some(root.join("shared-target")),
        delegated: None,
        git_env: git.env.clone(),
    };
    assert_eq!(dispatch(&facts).1, Ran::Here(captured.clone()));
    let differs = || source_differs(&facts.source, &git.env, &captured);
    let mut wrong = Vec::new();
    for path in tracked {
        let file = repo.join(path);
        std::fs::write(&file, "# local code or build setting\n").unwrap();
        if differs().is_none() {
            wrong.push(format!("tracked {path}"));
        }
        std::fs::write(file, "# captured input\n").unwrap();
        assert_eq!(differs(), None, "restored {path}");
    }
    let extras = [
        "crates/aterm-json/src/new.rs",
        "crates/aterm-toml/src/local.rs",
        ".cargo/local.toml",
        "target/ignored-build-artifact",
    ];
    for path in extras {
        let file = repo.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "local input\n").unwrap();
        if differs().is_none() {
            wrong.push(format!("extra {path}"));
        }
        std::fs::remove_file(file).unwrap();
        assert_eq!(differs(), None, "removed {path}");
    }
    assert!(
        wrong.is_empty(),
        "local inputs read as the captured source: {wrong:#?}"
    );
    let expected = Ran::There {
        rules: "// TWO\n".to_owned(),
        full: captured.clone(),
        target: root.join("shared-target/pre-promote.noindex"),
    };
    for (path, captured_body) in [
        ("crates/aterm-json/src/lib.rs", Some("# captured input\n")),
        ("target/ignored-build-artifact", None),
    ] {
        let file = repo.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "# local code or build setting\n").unwrap();
        let (code, ran, said) = dispatch(&facts);
        assert_eq!((code, &ran), (0, &expected), "{path}: {said}");
        match captured_body {
            Some(body) => std::fs::write(file, body).unwrap(),
            None => std::fs::remove_file(file).unwrap(),
        }
        assert_eq!(dispatch(&facts).1, Ran::Here(captured.clone()), "{path}");
    }
    assert_eq!(worktrees(&git, &repo), 1, "delegated trees were removed");
}

/// the recorded revision alone does not attest dirty or extra local files.
#[test]
fn export_cutter_attests_submodule_working_sources() {
    let (_dir, root, git) = scratch();
    let sub = root.join("dependency");
    git.init(&sub);
    std::fs::write(sub.join("lib.rs"), "// captured dependency\n").unwrap();
    std::fs::write(sub.join(".gitignore"), "local.rs\n").unwrap();
    git.commit_all(&sub, "dependency");
    let (repo, _) = cutter_history(&root, &git);
    git.ok(
        &repo,
        &[
            "submodule",
            "add",
            "-q",
            &sub.to_string_lossy(),
            "vendor/dependency",
        ],
    );
    let captured = git.commit_all(&repo, "the cutter's gitlink");
    let facts = Transforms {
        inputs: Inputs {
            root: repo.clone(),
            export: root.join("export"),
            captured: captured.clone(),
        },
        source: repo.join("crates/aterm-release"),
        target: Some(root.join("shared-target")),
        delegated: None,
        git_env: git.env.clone(),
    };
    assert_eq!(dispatch(&facts).1, Ran::Here(captured.clone()));
    let dependency = repo.join("vendor/dependency");
    let mut wrong = Vec::new();
    for (path, original) in [
        ("lib.rs", Some("// captured dependency\n")),
        ("local.rs", None),
    ] {
        std::fs::write(dependency.join(path), "// local dependency code\n").unwrap();
        let (code, ran, said) = dispatch(&facts);
        if code != 0 || !matches!(ran, Ran::There { ref full, .. } if *full == captured) {
            wrong.push(format!("submodule {path}: {code}, {ran:?}, {said}"));
        }
        if let Some(original) = original {
            std::fs::write(dependency.join(path), original).unwrap();
        } else {
            std::fs::remove_file(dependency.join(path)).unwrap();
        }
        assert_eq!(dispatch(&facts).1, Ran::Here(captured.clone()));
    }
    assert_eq!(worktrees(&git, &repo), 1);
    assert!(
        wrong.is_empty(),
        "local submodule code ran as attested: {wrong:#?}"
    );
}
