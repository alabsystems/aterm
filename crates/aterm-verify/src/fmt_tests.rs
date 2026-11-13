// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn tree(files: &[(&str, &str)]) -> aterm_tempfile::TempDir {
    let dir = aterm_tempfile::Builder::new()
        .prefix("atv-fmt-")
        .suffix(".noindex")
        .tempdir()
        .expect("scratch");
    for (rel, text) in files {
        let p = dir.path().join(rel);
        std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
        std::fs::write(p, text).expect("write");
    }
    dir
}

fn git_in(root: &Path, calls: &[&[&str]]) {
    let env = crate::EnvSnapshot::capture();
    for args in calls {
        let ok = crate::changed::git(root, &env.path)
            .args(*args)
            .status()
            .is_ok_and(|s| s.success());
        assert!(ok, "git {args:?} in the scratch checkout");
    }
}

#[test]
fn an_untracked_source_cargo_builds_is_listed_and_nothing_else_untracked() {
    let t = tree(&[
        ("Cargo.toml", "[workspace]\nmembers = [\"crates/a\"]\n"),
        (
            "crates/a/Cargo.toml",
            "[package]\nname = \"a\"\nedition = \"2024\"\n",
        ),
        ("crates/a/src/lib.rs", "pub fn tracked() {}\n"),
        (".gitignore", "crates/a/src/ignored.rs\n"),
        (
            "crates/a/fuzz/Cargo.toml",
            "[package]\nname = \"a-fuzz\"\n\n[dependencies.x]\npath = \"x\"\n\n\
             [[bin]]\nname = \"decode\"\npath = \"fuzz_targets/decode.rs\"\n\n\
             [[bin]]\nname = \"up\"\npath = \"../notes/up.rs\"\n",
        ),
        ("crates/a/fuzz/fuzz_targets/decode.rs", "fn main() {}\n"),
        (
            "crates/flat/Cargo.toml",
            "[package]\nname = \"flat\"\n\n[lib]\npath = \"lib.rs\"\n",
        ),
    ]);
    let root = t.path();
    git_in(root, &[&["init", "-q"], &["add", "."]]);
    for (rel, text) in [
        ("crates/a/src/new.rs", "pub fn new() {}\n"),
        ("crates/a/src/deep/mod.rs", "pub fn deep() {}\n"),
        ("crates/a/tests/t.rs", "#[test]\nfn t() {}\n"),
        ("crates/a/benches/b.rs", "fn main() {}\n"),
        ("crates/a/examples/e.rs", "fn main() {}\n"),
        ("crates/a/build.rs", "fn main() {}\n"),
        ("crates/a/notes/scratch.rs", "fn scratch( ){}\n"),
        ("crates/a/src/ignored.rs", "fn ignored( ){}\n"),
        ("scratch.rs", "fn scratch( ){}\n"),
        ("vendor/v/Cargo.toml", "[package]\nname = \"v\"\n"),
        ("vendor/v/src/lib.rs", "fn v( ){}\n"),
        ("crates/a/fuzz/fuzz_targets/new.rs", "fn main() {}\n"),
        ("crates/a/fuzz/fuzz_targets/util/mod.rs", "pub fn u() {}\n"),
        ("crates/a/fuzz/corpus/seed.rs", "fn seed( ){}\n"),
        ("crates/a/fuzz/scratch.rs", "fn scratch( ){}\n"),
        ("crates/a/fuzz/x/lib.rs", "fn x( ){}\n"),
        ("crates/flat/lib.rs", "pub fn flat() {}\n"),
        ("crates/flat/beside.rs", "pub fn beside() {}\n"),
        ("crates/flat/notes/n.rs", "fn n( ){}\n"),
        ("crates/a/notes/up.rs", "fn up( ){}\n"),
    ] {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
        std::fs::write(p, text).expect("write");
    }
    let env = crate::EnvSnapshot::capture();
    let files = rs_files_in(root, &env.path).expect("listed");
    assert_eq!(
        files,
        [
            "crates/a/benches/b.rs",
            "crates/a/build.rs",
            "crates/a/examples/e.rs",
            "crates/a/fuzz/fuzz_targets/decode.rs",
            "crates/a/fuzz/fuzz_targets/new.rs",
            "crates/a/fuzz/fuzz_targets/util/mod.rs",
            "crates/a/src/deep/mod.rs",
            "crates/a/src/lib.rs",
            "crates/a/src/new.rs",
            "crates/a/tests/t.rs",
            "crates/flat/beside.rs",
            "crates/flat/lib.rs",
        ]
    );
    let tracked = ls_files(root, &env.path, &["--cached"]).expect("listed");
    assert_eq!(
        tracked,
        [
            "crates/a/fuzz/fuzz_targets/decode.rs",
            "crates/a/src/lib.rs"
        ],
        "the control"
    );
}

#[test]
fn a_diff_path_keeps_a_colon_inside_the_checkout_path() {
    let out = "Diff in /a:b/crates/x/src/lib.rs:12:\n stuff\nDiff in /a:b/y.rs:3:\n\
               Diff in /a:b/y.rs:9:\nerror: something\n";
    let got: Vec<String> = diff_paths(out).into_iter().collect();
    assert_eq!(got, ["/a:b/crates/x/src/lib.rs", "/a:b/y.rs"]);
}

#[test]
fn a_path_is_made_relative_through_either_spelling_of_the_root() {
    let root = Path::new("/var/r");
    let canon = Path::new("/private/var/r");
    assert_eq!(relative_to("/var/r/a.rs", root, Some(canon)), "a.rs");
    assert_eq!(
        relative_to("/private/var/r/a.rs", root, Some(canon)),
        "a.rs"
    );
    assert_eq!(
        relative_to("/elsewhere/a.rs", root, Some(canon)),
        "/elsewhere/a.rs"
    );
}

#[test]
fn each_file_takes_its_own_crates_edition() {
    let t = tree(&[
        ("Cargo.toml", "[workspace.package]\nedition = \"2024\"\n"),
        ("a/Cargo.toml", "[package]\nedition.workspace = true\n"),
        ("b/Cargo.toml", "[package]\nedition = \"2021\"\n"),
    ]);
    let mut e = Editions::new(t.path());
    assert_eq!(e.manifest("a/src/lib.rs"), "2024");
    assert_eq!(e.manifest("b/src/deep/mod.rs"), "2021");
    assert_eq!(e.manifest("fixtures/x.rs"), "2024");
    let groups = by_edition(
        &["a/src/lib.rs".into(), "b/src/lib.rs".into(), "c.rs".into()],
        &mut e,
    );
    assert_eq!(
        groups,
        [
            ("2021".to_string(), vec!["b/src/lib.rs".to_string()]),
            (
                "2024".to_string(),
                vec!["a/src/lib.rs".to_string(), "c.rs".to_string()]
            ),
        ]
    );
}

#[test]
fn a_rustfmt_edition_that_contradicts_its_manifest_is_a_gap_until_written() {
    let t = tree(&[
        ("Cargo.toml", "[package]\nedition = \"2024\"\n"),
        ("rustfmt.toml", "edition = \"2024\"\n"),
        ("inner/Cargo.toml", "[package]\nedition = \"2021\"\n"),
    ]);
    let files = ["src/lib.rs".to_string(), "inner/src/lib.rs".to_string()];
    let gaps = edition_gaps(t.path(), &files, &mut Editions::new(t.path()));
    assert_eq!(
        gaps,
        [EditionGap {
            dir: "inner".into(),
            edition: "2021".into(),
            example: "inner/src/lib.rs".into(),
        }]
    );
    std::fs::write(t.path().join("inner/rustfmt.toml"), "edition = \"2021\"\n").expect("w");
    assert!(edition_gaps(t.path(), &files, &mut Editions::new(t.path())).is_empty());
    std::fs::remove_file(t.path().join("rustfmt.toml")).expect("rm");
    let gaps = edition_gaps(t.path(), &files, &mut Editions::new(t.path()));
    assert_eq!(gaps.len(), 1, "{gaps:?}");
    assert_eq!(gaps[0].dir, "");
}

#[test]
fn the_command_checks_unless_it_writes() {
    let files = ["a.rs".to_string()];
    let check = trustfmt_cmd(Path::new("/s/trustfmt"), "2021", &files, false).argv();
    assert_eq!(
        check,
        [
            "/s/trustfmt",
            "--check",
            "--color",
            "never",
            "--edition",
            "2021",
            "--unstable-features",
            "--skip-children",
            "a.rs"
        ]
    );
    let write = trustfmt_cmd(Path::new("/s/trustfmt"), "2021", &files, true).argv();
    assert!(!write.contains(&"--check".to_string()), "{write:?}");
}

fn run_of(ok: bool, output: &str) -> exec::Run {
    exec::Run {
        case_observations: None,
        stopped_at: None,
        hung: None,
        ok,
        output: output.to_string(),
        code: Some(i32::from(!ok)),
        spawn_error: None,
        usage: None,
    }
}

#[test]
fn a_child_separates_drift_from_files_it_could_not_read() {
    let root = Path::new("/r");
    assert_eq!(
        read_run(&run_of(true, ""), "2024", 3, root, None),
        Sweep::default()
    );
    let drift = read_run(
        &run_of(false, "Diff in /r/a.rs:1:\n-x\n+y\n"),
        "2024",
        3,
        root,
        None,
    );
    assert_eq!(
        drift.drifted,
        [Drift {
            path: "a.rs".into(),
            edition: "2024".into()
        }]
    );
    assert!(drift.unread.is_empty());
    let blind = read_run(&run_of(false, "dyld: missing\n"), "2024", 3, root, None);
    assert!(blind.drifted.is_empty());
    assert_eq!(blind.unread.len(), 1);
    assert!(blind.unread[0].contains("read none of 3"), "{blind:?}");
    let mixed = read_run(
        &run_of(
            false,
            "Diff in /r/a.rs:1:\n     error: String,\n-x\n+y\nerror: expected item, found `≐`\n",
        ),
        "2024",
        3,
        root,
        None,
    );
    assert_eq!(mixed.drifted.len(), 1);
    assert_eq!(mixed.unread, ["error: expected item, found `≐`"]);
    // An internal error (whitespace the formatter left behind in a `clean` block) names the
    // file it is in, rather than the whole batch reading as unread.
    let internal = read_run(
        &run_of(
            false,
            "error[internal]: left behind trailing whitespace\n --> /r/island.rs:8:8:1\n  |\n",
        ),
        "2024",
        3,
        root,
        None,
    );
    assert!(internal.drifted.is_empty());
    assert_eq!(
        internal.unread,
        ["error[internal]: left behind trailing whitespace (/r/island.rs:8:8:1)"]
    );
}

/// The toolchain this host's own drivers find, as `verify` would.
fn real_tools() -> crate::Toolchain {
    let repo = aterm_tempfile::manifest_dir().join("../..");
    let env = crate::EnvSnapshot::capture();
    let tools = crate::run_toolchain(&env, &repo);
    if tools.trustfmt.is_none() && env.trust_stage2_bin.is_some() {
        let discovered = crate::EnvSnapshot {
            trust_stage2_bin: None,
            ..env
        };
        return crate::run_toolchain(&discovered, &repo);
    }
    tools
}

/// A file the real formatter cannot parse is named by the formatter's own `error:` line, not
/// hidden behind "read none of N file(s)". `trustfmt_cmd` turns colour off once; with
/// `--color always` in its place, or the flag given twice, this test fails.
#[cfg(not(windows))]
#[test]
fn a_file_the_real_formatter_cannot_parse_is_named_by_its_error_line() {
    let tools = real_tools();
    let Some(bin) = tools.trustfmt.clone() else {
        aterm_uds::host::could_not_run(format!(
            "{}, so this machine cannot show the check either way",
            tools.missing_trustfmt_label()
        ));
    };
    let t = tree(&[("src/bad.rs", "pub fn x( ) { let = ; }\n")]);
    let root = t.path().to_path_buf();
    let scratch = t.path().join(".scratch");
    std::fs::create_dir_all(&scratch).expect("scratch");
    let ctx = Ctx::new_with_tools(
        root.clone(),
        crate::Scope::workspace(),
        crate::EnvSnapshot::capture(),
        scratch,
        tools,
    );
    let files = [root.join("src/bad.rs").to_string_lossy().into_owned()];
    let cmd = trustfmt_cmd(&bin, "2021", &files, false);
    let read = read_run(&exec::run(&cmd, ctx.exec_env()), "2021", 1, &root, None);
    assert!(
        read.unread
            .iter()
            .any(|l| l.starts_with("error: expected pattern")),
        "{read:?}"
    );
    assert!(
        read.unread.iter().all(|l| !l.contains("read none of")),
        "{read:?}"
    );
}

#[test]
fn the_real_formatter_reds_on_drift_and_the_fix_greens_it() {
    use crate::ladder::{Outcome, Severity};
    let tools = real_tools();
    let untidy_lib = "pub struct E {\n    error: String,\n}\npub fn wrong(  ) ->usize{let x=1;x}\n";
    let t = tree(&[
        (
            "Cargo.toml",
            "[package]\nname = \"fx\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
        ),
        ("rustfmt.toml", "edition = \"2021\"\n"),
        ("src/lib.rs", untidy_lib),
    ]);
    let root = t.path().to_path_buf();
    let scratch = t.path().join(".scratch");
    std::fs::create_dir_all(&scratch).expect("scratch");
    git_in(
        &root,
        &[
            &["init", "-q"],
            &["add", "Cargo.toml", "rustfmt.toml", "src"],
        ],
    );
    let untidy = "pub fn new(  ) ->usize{2}\n";
    std::fs::write(root.join("src/new.rs"), untidy).expect("write");
    std::fs::create_dir_all(root.join("notes")).expect("mkdir");
    std::fs::write(root.join("notes/scratch.rs"), untidy).expect("write");
    let check = |fix: bool| {
        let ctx = Ctx::new_with_tools(
            root.clone(),
            crate::Scope::workspace(),
            crate::EnvSnapshot::capture(),
            scratch.clone(),
            tools.clone(),
        )
        .with_fix(fix);
        let mut r = Report::new("fmt");
        run(&ctx, &mut r);
        r
    };
    if tools.trustfmt.is_none() {
        #[cfg(windows)]
        {
            let want = [(Outcome::Skip, tools.missing_trustfmt_label())];
            for fix in [false, true] {
                let r = check(fix);
                let rows: Vec<(Outcome, String)> =
                    r.outcomes().map(|(o, l)| (o, l.to_string())).collect();
                assert_eq!(rows, want, "fix={fix}");
            }
            assert_eq!(
                std::fs::read_to_string(root.join("src/lib.rs")).expect("read"),
                untidy_lib
            );
            assert_eq!(
                std::fs::read_to_string(root.join("src/new.rs")).expect("read"),
                untidy
            );
            return;
        }
        #[cfg(not(windows))]
        aterm_uds::host::could_not_run(format!(
            "{}, so this machine cannot show the check either way",
            tools.missing_trustfmt_label()
        ));
    }
    let red = check(false);
    let rows: Vec<(Outcome, String)> = red.outcomes().map(|(o, l)| (o, l.to_string())).collect();
    assert!(
        rows.iter()
            .all(|(o, _)| *o != Outcome::Fail(Severity::CouldNotRun)),
        "{rows:?}"
    );
    let ids: Vec<String> = red.findings().into_iter().map(|f| f.id).collect();
    assert_eq!(ids, ["src/lib.rs", "src/new.rs"], "{rows:?}");
    let fixed = check(true);
    assert!(
        fixed.outcomes().all(|(o, _)| o == Outcome::Ok),
        "{:?}",
        fixed.outcomes().collect::<Vec<_>>()
    );
    assert!(
        std::fs::read_to_string(root.join("src/lib.rs"))
            .expect("read")
            .contains("pub fn wrong() -> usize {")
    );
    assert!(
        std::fs::read_to_string(root.join("src/new.rs"))
            .expect("read")
            .contains("pub fn new() -> usize {")
    );
    assert_eq!(
        std::fs::read_to_string(root.join("notes/scratch.rs")).expect("read"),
        untidy,
        "the scratch file is never rewritten"
    );
    let green = check(false);
    assert!(
        green.outcomes().all(|(o, _)| o == Outcome::Ok),
        "{:?}",
        green.outcomes().collect::<Vec<_>>()
    );
}

#[test]
fn an_edition_line_is_read_and_a_workspace_key_is_not() {
    assert_eq!(
        toml_edition("# edition = \"x\"\nedition = '2021'\n").as_deref(),
        Some("2021")
    );
    assert_eq!(toml_edition("edition.workspace = true\n"), None);
    assert_eq!(toml_edition("edition = { workspace = true }\n"), None);
    assert_eq!(toml_edition("edition-guide = \"1.0\"\n"), None);
    assert_eq!(toml_edition("[package]\nname = \"a\"\n"), None);
    assert!(toml_workspace_edition(
        "edition.workspace = true # comment\n"
    ));
    assert!(toml_workspace_edition("edition = { workspace = true }\n"));
    assert!(!toml_workspace_edition(
        "edition = \"2021\" # inherited from workspace\n"
    ));
}

#[test]
fn formatting_keys_follow_selected_sources_edition_and_config() {
    let dir = tree(&[
        (
            "Cargo.toml",
            "[workspace.package]\nversion='1.0.0'\nedition='2024'\n",
        ),
        ("rustfmt.toml", "edition='2024'\n"),
        (
            "crates/a/Cargo.toml",
            "[package]\nname='a'\nversion='0.0.0'\nedition.workspace=true\n",
        ),
        ("crates/a/src/lib.rs", "pub fn a() {}\n"),
        ("crates/a/src/other.rs", "pub fn other() {}\n"),
        (
            "crates/b/Cargo.toml",
            "[package]\nname='b'\nversion='0.0.0'\nedition.workspace=true\n",
        ),
        ("crates/b/src/lib.rs", "pub fn b() {}\n"),
    ]);
    let mut ctx = Ctx::new(
        dir.path().into(),
        crate::Scope::workspace(),
        crate::EnvSnapshot::default(),
        dir.path().join("scratch"),
    );
    ctx.carry_context = Some("a".repeat(64));
    let files = vec!["crates/a/src/lib.rs".into(), "crates/b/src/lib.rs".into()];
    let keys = |files: &[String]| {
        input_plans(&ctx, files, &mut Editions::new(dir.path()))
            .into_iter()
            .map(|p| p.proof.unwrap().key)
            .collect::<Vec<_>>()
    };
    let original = keys(&files);
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[workspace.package]\nversion='2.0.0'\nedition='2024'\n",
    )
    .unwrap();
    assert_eq!(original, keys(&files));
    std::fs::write(dir.path().join("crates/a/README.md"), "new documentation\n").unwrap();
    std::fs::write(
        dir.path().join("crates/a/src/other.rs"),
        "pub fn unselected() {}\n",
    )
    .unwrap();
    assert_eq!(
        original,
        keys(&files),
        "neither documentation nor an unselected child is a --skip-children input"
    );
    let mut expanded = files.clone();
    expanded.push("crates/a/src/other.rs".into());
    assert_ne!(
        original[0],
        keys(&expanded)[0],
        "new selected files require judgment"
    );
    std::fs::write(dir.path().join(&files[0]), "pub fn changed() {}\n").unwrap();
    let changed = keys(&files);
    assert_ne!(original[0], changed[0]);
    assert_eq!(original[1], changed[1]);
    std::fs::write(dir.path().join("rustfmt.toml"), "edition='2021'\n").unwrap();
    assert_ne!(changed[1], keys(&files)[1]);
    let old_config = keys(&files);
    std::fs::write(dir.path().join("trustfmt.toml"), "edition='2024'\n").unwrap();
    assert_ne!(
        old_config,
        keys(&files),
        "the native config is an input too"
    );
    assert_eq!(Editions::new(dir.path()).rustfmt(&files[0]), "2024");
    std::fs::write(dir.path().join(".trustfmt.toml"), "edition='2021'\n").unwrap();
    assert_eq!(Editions::new(dir.path()).rustfmt(&files[0]), "2021");
}
