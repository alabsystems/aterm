// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::path::Path;

use super::*;
use crate::ladder::{Outcome, Severity};

/// aterm's own config, which the engine accepts.
const ACCEPTED: &str = "[extend]\nuseDefault = true\n";

#[test]
fn the_config_policy_refuses_what_the_engine_refuses_and_nothing_else() {
    assert_eq!(config_refusals(Some(ACCEPTED), false), Vec::<String>::new());
    let own = std::fs::read_to_string(aterm_tempfile::manifest_dir().join("../..").join(CONFIG))
        .expect("the checkout's own gitleaks config");
    assert_eq!(
        config_refusals(Some(&own), false),
        Vec::<String>::new(),
        "{own}"
    );
    let custom = "[extend]\nuseDefault = true\n[[rules]]\nid = \"repo-extra-x\"\nregex = 'x'\n";
    assert_eq!(config_refusals(Some(custom), false), Vec::<String>::new());
    for (text, why) in [
        ("[extend]\nuseDefault = \"yes\"\n", "useDefault"),
        (
            "[extend]\nuseDefault = true\ndisabledRules = [\"jwt\"]\n",
            "disables",
        ),
        (
            "[extend]\nuseDefault = true\npath = \"other.toml\"\n",
            "extends a config",
        ),
        (
            "[extend]\nuseDefault = true\n[allowlist]\npaths = ['x']\n",
            "repository-wide",
        ),
        (
            "[extend]\nuseDefault = true\n[[allowlists]]\npaths = ['x']\n",
            "repository-wide",
        ),
        (
            "[extend]\nuseDefault = true\n[[rules]]\nid = \"jwt\"\n",
            "repo-extra-",
        ),
        (
            "[extend]\nuseDefault = true\n[[rules]]\nid = \"repo-extra-x\"\n[rules.allowlist]\n",
            "defines an allowlist",
        ),
        (
            "rules = [1]\n[extend]\nuseDefault = true\n",
            "not a list of tables",
        ),
        (
            "[extend]\nuseDefault = true\n[[rules]]\nid = \"repo-extra-x\"\n[rules.x]\nregexTarget = \"line\"\n",
            "regexTarget",
        ),
        ("[extend\n", "does not parse"),
    ] {
        let refused = config_refusals(Some(text), false);
        assert!(
            refused.iter().any(|r| r.contains(why)),
            "{text:?} → {refused:?}"
        );
    }
    assert!(config_refusals(None, false)[0].contains("carries no"));
    assert!(config_refusals(Some(ACCEPTED), true)[0].contains(IGNORE));
}

const LOCK: &str = "version = 4\n\n[[package]]\nname = \"crypto_secretbox\"\nversion = \"0.1.1\"\n\n\
                    [[package]]\nname = \"curve25519-dalek\"\nversion = \"4.1.3\"\n\n\
                    [[package]]\nname = \"app\"\nversion = \"0.1.0\"\ndependencies = [\n \
                    \"crypto_secretbox\",\n \"curve25519-dalek 4.1.3\",\n \"not_a_package\",\n]\n";

/// The 1-based line of `LOCK` holding `needle`.
fn lock_line(needle: &str) -> i64 {
    let at = LOCK
        .split('\n')
        .position(|l| l.contains(needle))
        .expect("in LOCK");
    i64::try_from(at + 1).expect("small")
}

#[test]
fn a_lockfile_region_clears_only_when_every_line_names_a_declared_package() {
    let first = lock_line("\"crypto_secretbox\",");
    let second = lock_line("\"curve25519-dalek 4.1.3\"");
    let stranger = lock_line("not_a_package");
    assert!(lock_dependency_region(LOCK, first, second));
    assert!(!lock_dependency_region(LOCK, first, stranger));
    assert!(!lock_dependency_region(LOCK, stranger, stranger));
    assert!(
        !lock_dependency_region(LOCK, 4, 4),
        "a `name = ` line is no entry"
    );
    assert!(!lock_dependency_region(LOCK, 0, first));
    assert!(!lock_dependency_region(LOCK, second, first));
    assert!(!lock_dependency_region(LOCK, first, 999));
    assert!(!lock_dependency_region("not toml [", 1, 1));
}

#[test]
fn the_report_keeps_every_finding_the_engine_does_not_clear() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let tree = dir.path();
    std::fs::write(tree.join("Cargo.lock"), LOCK).expect("lock");
    let lock = tree.join("Cargo.lock");
    let first = lock_line("\"crypto_secretbox\",");
    let leak = |file: &Path, rule: &str, start: i64, end: i64| {
        format!(
            "{{\"RuleID\":\"{rule}\",\"File\":{},\"StartLine\":{start},\"EndLine\":{end},\"Secret\":\"REDACTED\"}}",
            aterm_json::to_string(&file.to_string_lossy().into_owned()).expect("json")
        )
    };
    let report = format!(
        "[{},{},{},{}]",
        leak(&lock, ENTROPY_RULE, first, first),
        leak(&lock, "jwt", first, first),
        leak(
            &lock,
            ENTROPY_RULE,
            lock_line("not_a_package"),
            lock_line("not_a_package")
        ),
        leak(&tree.join("crates/x/src/lib.rs"), ENTROPY_RULE, 3, 3),
    );
    let kept = adjudicate(&report, tree).expect("a list");
    assert_eq!(
        kept,
        [
            Leak {
                path: "Cargo.lock".into(),
                line: first,
                rule: "jwt".into()
            },
            Leak {
                path: "Cargo.lock".into(),
                line: lock_line("not_a_package"),
                rule: ENTROPY_RULE.into()
            },
            Leak {
                path: "crates/x/src/lib.rs".into(),
                line: 3,
                rule: ENTROPY_RULE.into()
            },
        ]
    );
    assert!(adjudicate("{}", tree).is_err());
    assert!(adjudicate("[", tree).is_err());
}

#[test]
fn the_registry_names_its_repositories() {
    let names = registry_names("order = []\n[repos.aterm]\npath = \"~/aterm\"\n[repos.ay]\n")
        .expect("names");
    assert_eq!(names, ["aterm", "ay"]);
    assert!(registry_names("order = []\n").is_err());
    assert!(registry_names("[repos\n").is_err());
}

/// A string gitleaks' entropy rule takes for an API key, made at run time so this file holds
/// none.
fn planted_key() -> String {
    const ALPHABET: &[u8] = b"abcdefghijkmnopqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    (0..32)
        .map(|i| char::from(ALPHABET[(i * 37 + 11) % ALPHABET.len()]))
        .collect()
}

fn git(dir: &Path, args: &[&str]) {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let out = crate::changed::git(dir, &path)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(args)
        .output()
        .expect("git runs");
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

/// A checkout whose export carries `crates/x/src/lib.rs` holding `lib`, committed on `main`
/// with `lib` added after, and an engine checkout beside it.
fn checkout(lib: &str) -> (aterm_tempfile::TempDir, Ctx) {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("repo");
    let engine = dir.path().join("engine");
    let write = |path: &Path, text: &str| {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, text).expect("write");
    };
    write(&engine.join(rows::BASELINE), "# Paths\nquokkaword\n");
    write(
        &engine.join(REGISTRY),
        "[repos.aterm]\npath = \"~/aterm\"\n",
    );
    write(&root.join(MANIFEST), "crates\n.gitleaks.toml\n");
    write(&root.join(CONFIG), ACCEPTED);
    write(&root.join("crates/aterm-gui/Cargo.toml"), "[package]\n");
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "base"]);
    write(&root.join("crates/x/src/lib.rs"), lib);
    let scratch = dir.path().join("scratch");
    std::fs::create_dir_all(&scratch).expect("scratch");
    let env = crate::EnvSnapshot {
        path: std::env::var_os("PATH").unwrap_or_default(),
        publication_engine: Some(engine),
        ..crate::EnvSnapshot::default()
    };
    let scope = Scope::changed("main", Vec::new(), false);
    (dir, Ctx::new(root, scope, env, scratch))
}

fn verdict(ctx: &Ctx) -> (Vec<Outcome>, Vec<String>) {
    let mut r = Report::new("export-scan");
    run_with(ctx, &mut r, || {
        Ok(Scanner::Fixture(aterm_uds::host::need_tool("gitleaks")))
    });
    if r.outcomes()
        .any(|(outcome, _)| outcome == Outcome::Fail(Severity::CouldNotRun))
    {
        eprintln!(
            "export-scan fixture could not run: {:?}",
            r.outcomes().collect::<Vec<_>>()
        );
    }
    require_fixture(&r).unwrap_or_else(|why| aterm_uds::host::could_not_run(why));
    (
        r.outcomes().map(|(o, _)| o).collect(),
        r.findings().iter().map(|f| f.id.clone()).collect(),
    )
}

fn require_fixture(report: &Report) -> Result<(), String> {
    let failed = report
        .outcomes()
        .any(|(outcome, _)| outcome == Outcome::Fail(Severity::GateFailed));
    let unavailable = report
        .outcomes()
        .any(|(outcome, _)| outcome == Outcome::Fail(Severity::CouldNotRun));
    if unavailable && !failed {
        Err(report.render())
    } else {
        Ok(())
    }
}

#[test]
fn an_unavailable_export_fixture_is_not_a_product_failure() {
    let mut report = Report::new("export fixture");
    report.pass("the independent export checks passed");
    report.cannot_run("signed scanner unavailable");
    let why = require_fixture(&report).expect_err("no scanner verdict was observed");
    assert!(why.contains("signed scanner unavailable"));
    report.fail("an independent guard actually refused the export");
    assert_eq!(
        require_fixture(&report),
        Ok(()),
        "a real refusal still fails"
    );
}

/// The three cut reds the engine's gitleaks guard found from 2026-11-01 to 11-05 were each an
/// entropy-rule hit in a test fixture; the dev set finds the same shape at its line, before a
/// push, and passes the same file without it.
#[test]
fn a_fixture_gitleaks_refuses_is_found_before_a_cut() {
    let key = planted_key();
    let (_dir, ctx) = checkout(&format!("// a fixture\nlet api_key = \"{key}\";\n"));
    let (outcomes, ids) = verdict(&ctx);
    assert_eq!(outcomes, [Outcome::Fail(Severity::GateFailed)], "{ids:?}");
    assert_eq!(ids, ["crates/x/src/lib.rs:2"]);
    let (_dir, ctx) = checkout("// a fixture\nlet api_key = \"\";\n");
    let (outcomes, ids) = verdict(&ctx);
    assert_eq!(outcomes, [Outcome::Ok], "{ids:?}");
}

#[test]
fn a_cone_that_changes_a_scan_input_scans_the_whole_export() {
    let (_dir, ctx) = checkout("// x\n");
    let path = ctx.path_env.clone();
    let cone = listed(&ctx.root, &path, &ctx.scope).expect("lists");
    assert!(!cone.whole, "{cone:?}");
    assert_eq!(cone.paths, ["crates/x/src/lib.rs", CONFIG]);
    assert!(widens(MANIFEST) && widens(rows::EXTRA));
    // One path in each of WIDENS: the scan inputs, the transforms and the scan itself.
    for input in [
        "publish/transforms.toml",
        CONFIG,
        IGNORE,
        "crates/aterm-census/src/export.rs",
        "crates/aterm-census/src/export_scan/org.rs",
        "crates/aterm-verify/src/export_scan.rs",
        "crates/aterm-release/src/transforms.rs",
    ] {
        let file = ctx.root.join(input);
        let before = std::fs::read(&file).ok();
        std::fs::create_dir_all(file.parent().expect("parent")).expect("mkdir");
        std::fs::write(&file, "changed\n").expect("change");
        let widened = listed(&ctx.root, &path, &ctx.scope).expect("lists");
        match before {
            Some(bytes) => std::fs::write(&file, bytes).expect("restore"),
            None => std::fs::remove_file(&file).expect("remove"),
        }
        assert!(
            widened.whole && widened.what.contains(input),
            "{input}: {widened:?}"
        );
        assert!(
            widened
                .paths
                .contains(&"crates/aterm-gui/Cargo.toml".to_string())
        );
    }
    let all = listed(&ctx.root, &path, &Scope::workspace()).expect("lists");
    assert!(all.whole && all.paths.contains(&"crates/x/src/lib.rs".to_string()));
}

/// Without the engine's checkout the export's own rows still hold, beside a COULD NOT RUN that
/// names the checkout.
#[test]
fn without_the_engine_the_exports_own_rows_still_hold() {
    let (_dir, mut ctx) = checkout("// zebraword\n// quokkaword\n");
    std::fs::create_dir_all(ctx.root.join("publish")).expect("mkdir");
    std::fs::write(ctx.root.join(rows::EXTRA), "zebraword\n").expect("rows");
    let (_, with) = verdict(&ctx);
    assert_eq!(
        with.iter()
            .filter(|id| id.contains("lib.rs"))
            .collect::<Vec<_>>(),
        ["crates/x/src/lib.rs:1", "crates/x/src/lib.rs:2"]
    );
    ctx.env.publication_engine = None;
    let mut r = Report::new("export-scan");
    run_with(&ctx, &mut r, || Err("no signed scanner in fixture".into()));
    let rows: Vec<(Outcome, String)> = r.outcomes().map(|(o, l)| (o, l.to_string())).collect();
    assert!(
        rows.iter()
            .any(|(o, l)| *o == Outcome::Fail(Severity::GateFailed)
                && l.contains("the export's own rows")),
        "{rows:#?}"
    );
    assert!(
        rows.iter()
            .any(|(o, l)| *o == Outcome::Fail(Severity::CouldNotRun)
                && l.contains("PUBLICATION_ENGINE")),
        "{rows:#?}"
    );
    let ids: Vec<String> = r.findings().iter().map(|f| f.id.clone()).collect();
    assert!(
        ids.contains(&"crates/x/src/lib.rs:1".to_string())
            && !ids.contains(&"crates/x/src/lib.rs:2".to_string()),
        "the export's row holds, the engine's baseline row is not judged: {ids:?}"
    );
}

#[test]
fn the_gitlink_is_read_as_the_astream_files_the_export_carries() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let path = std::env::var_os("PATH").unwrap_or_default();
    let write = |rel: &str, text: &str| {
        let file = root.join(rel);
        std::fs::create_dir_all(file.parent().expect("parent")).expect("mkdir");
        std::fs::write(file, text).expect("write");
    };
    write(MANIFEST, "crates\n");
    write(
        "crates/x/Cargo.toml",
        "[dependencies]\nw = { path = \"../../vendor/astream/crates/w\" }\n",
    );
    // A manifest the export withholds reaches nothing.
    write(
        "tools/t/Cargo.toml",
        "[dependencies]\nv = { path = \"../../vendor/astream/crates/v\" }\n",
    );
    git(root, &["init", "-q"]);
    let unchanged = with_astream(root, &path, vec!["a.rs".into()]).expect("no gitlink");
    assert_eq!(unchanged, ["a.rs"]);
    let missing = with_astream(root, &path, vec![ASTREAM.into()]);
    assert!(missing.is_err_and(|why| why.contains("submodule update")));
    for (rel, text) in [
        (
            "Cargo.toml",
            "[workspace.dependencies]\nu = { path = \"crates/u\" }\n",
        ),
        (
            "crates/w/Cargo.toml",
            "[dependencies]\nu = { workspace = true }\n",
        ),
        ("crates/w/src/lib.rs", ""),
        ("crates/w/tests/t.rs", ""),
        ("crates/u/Cargo.toml", "[package]\n"),
        ("crates/u/src/lib.rs", ""),
        ("crates/v/Cargo.toml", "[package]\n"),
        ("crates/v/src/lib.rs", ""),
        ("README.md", ""),
    ] {
        write(&format!("{ASTREAM}/{rel}"), text);
    }
    let astream = root.join(ASTREAM);
    git(&astream, &["init", "-q"]);
    git(&astream, &["add", "-A"]);
    git(&astream, &["commit", "-q", "-m", "pin"]);
    let carried = with_astream(root, &path, vec![ASTREAM.into(), "b.rs".into()]).expect("lists");
    assert_eq!(
        carried,
        [
            "b.rs",
            "vendor/astream/Cargo.toml",
            "vendor/astream/crates/u/Cargo.toml",
            "vendor/astream/crates/u/src/lib.rs",
            "vendor/astream/crates/w/Cargo.toml",
            "vendor/astream/crates/w/src/lib.rs",
        ],
        "the crates the exported manifests reach, through astream's workspace, and none other"
    );
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "base"]);
    // A payload clears against astream's files only while its checkout is the pin's tree.
    let cut = || committed(root, &path).expect("lists");
    let w = "vendor/astream/crates/w/src/lib.rs";
    assert!(cut().iter().any(|p| p == w), "{:?}", cut());
    write(&format!("{ASTREAM}/crates/w/src/lib.rs"), "// changed\n");
    assert!(
        !cut().iter().any(|p| p.starts_with("vendor/astream/")),
        "{:?}",
        cut()
    );
    write(&format!("{ASTREAM}/crates/w/src/lib.rs"), "");
    assert!(cut().iter().any(|p| p == w), "{:?}", cut());
    let scope = Scope::changed("HEAD", Vec::new(), false);
    let cone = || listed(root, &path, &scope).expect("lists").paths;
    write("crates/x/src/lib.rs", "");
    assert_eq!(
        cone(),
        ["crates/x/src/lib.rs", CONFIG],
        "a source change carries no astream file"
    );
    write(
        "crates/x/Cargo.toml",
        "[dependencies]\nv = { path = \"../../vendor/astream/crates/v\" }\n",
    );
    let widened = cone();
    assert!(
        widened.contains(&"vendor/astream/crates/v/src/lib.rs".to_string())
            && !widened.contains(&"vendor/astream/crates/w/src/lib.rs".to_string()),
        "a manifest change re-reads which astream crates the export carries: {widened:?}"
    );
    let gitlinks = |scope: &Scope| listed(root, &path, scope).expect("lists").gitlinks;
    assert_eq!(gitlinks(&Scope::workspace()), [ASTREAM]);
    assert!(gitlinks(&scope).is_empty(), "the cone did not change it");
    // A checkout that holds another commit than the gitlink pins is not what the export carries.
    git(&astream, &["commit", "-q", "--allow-empty", "-m", "ahead"]);
    let stale = listed(root, &path, &scope);
    assert!(
        stale.as_ref().is_err_and(|why| why.contains("pins")),
        "{stale:?}"
    );
    git(root, &["add", ASTREAM]);
    assert_eq!(gitlinks(&scope), [ASTREAM], "the cone changed the gitlink");
}

/// A payload clears a hit only against a binary a cut of `HEAD` writes: an untracked, staged or
/// modified file is none of them.
#[test]
fn only_the_files_a_cut_of_head_writes_can_clear_a_hit() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let path = std::env::var_os("PATH").unwrap_or_default();
    let write = |rel: &str, bytes: &[u8]| std::fs::write(root.join(rel), bytes).expect("write");
    for rel in ["kept.bin", "modified.bin", "restaged.bin"] {
        write(rel, b"\0committed");
    }
    git(root, &["init", "-q"]);
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "base"]);
    write("modified.bin", b"\0changed");
    write("restaged.bin", b"\0changed");
    write("added.bin", b"\0new");
    git(root, &["add", "restaged.bin", "added.bin"]);
    write("untracked.bin", b"\0new");
    assert_eq!(committed(root, &path).expect("lists"), ["kept.bin"]);
}
