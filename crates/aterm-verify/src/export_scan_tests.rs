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
/// with `lib` added after, and beside it an engine checkout, its baseline and registry
/// committed, that `PUBLICATION_ENGINE` names.
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
    git(&engine, &["init", "-q", "-b", "main"]);
    git(&engine, &["add", "-A"]);
    git(&engine, &["commit", "-q", "-m", "engine"]);
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
        publication_engine: Some(engine.into_os_string()),
        ..crate::EnvSnapshot::default()
    };
    let scope = Scope::changed("main", Vec::new(), false);
    (dir, Ctx::new(root, scope, env, scratch))
}

fn verdict(ctx: &Ctx, scanner: Result<Scanner, String>) -> (Vec<Outcome>, Vec<String>, Report) {
    let mut r = Report::new("export-scan");
    run_with(ctx, &mut r, || scanner);
    (
        r.outcomes().map(|(o, _)| o).collect(),
        r.findings().iter().map(|f| f.id.clone()).collect(),
        r,
    )
}

/// Release input scopes the real content guard to unreceipted files. The same forbidden row
/// in a file covered by a landing receipt is the negative control: it is not scanned again.
#[test]
fn release_input_scans_the_unchecked_paths_only() {
    let (_dir, mut ctx) = checkout("// quokkaword\n");
    std::fs::write(ctx.root.join("crates/x/src/checked.rs"), "// quokkaword\n").unwrap();
    ctx.input_paths = Some(vec!["crates/x/src/lib.rs".into()]);
    let (_, ids, _) = verdict(&ctx, Err("fixture scanner unavailable".into()));
    assert_eq!(ids, ["crates/x/src/lib.rs:1"]);
    ctx.input_paths = Some(Vec::new());
    let (_, ids, _) = verdict(&ctx, Err("fixture scanner unavailable".into()));
    assert!(ids.is_empty());
    ctx.input_paths = None;
    let (_, ids, _) = verdict(&ctx, Err("fixture scanner unavailable".into()));
    assert_eq!(
        ids.len(),
        2,
        "the ordinary dev scan still judges both files"
    );
}

/// A real hand merge is discovered and its forbidden-content row reaches the existing guard.
/// Attaching the genuine checked-tree shape to that same merge removes it from the input.
#[test]
fn a_hand_merge_reaches_the_content_guard_but_a_checked_merge_does_not() {
    use aterm_types::land_trailer::{Checked, Row, Stated};
    let (_dir, mut ctx) = checkout("// quokkaword\n");
    let base = crate::head_commit(&ctx.root, &ctx.path_env).unwrap();
    git(&ctx.root, &["checkout", "-qb", "topic"]);
    git(&ctx.root, &["add", "-A"]);
    git(&ctx.root, &["commit", "-qm", "topic"]);
    git(&ctx.root, &["checkout", "-q", "main"]);
    git(
        &ctx.root,
        &["merge", "--no-ff", "-qm", "hand merge", "topic"],
    );
    ctx.input_paths = Some(
        crate::release_input::discover(&ctx.root, &ctx.path_env, &base, "HEAD")
            .unwrap()
            .paths,
    );
    let (_, ids, _) = verdict(&ctx, Err("fixture scanner unavailable".into()));
    assert_eq!(ids, ["crates/x/src/lib.rs:1"]);
    let tree = crate::changed::git(&ctx.root, &ctx.path_env)
        .args(["rev-parse", "HEAD^{tree}"])
        .output()
        .unwrap();
    assert!(tree.status.success());
    let receipt = Stated {
        checked: vec![Checked {
            tree: String::from_utf8(tree.stdout).unwrap().trim().into(),
            base: base.clone(),
            secs: 1,
            rows: ["fmt", "census", "export-scan", "lint"]
                .into_iter()
                .map(|check| Row {
                    check: check.into(),
                    word: "ok".into(),
                })
                .collect(),
        }],
        ..Stated::default()
    };
    git(
        &ctx.root,
        &[
            "commit",
            "--amend",
            "-qm",
            &format!("land\n\n{}", receipt.trailers()),
        ],
    );
    let input = crate::release_input::discover(&ctx.root, &ctx.path_env, &base, "HEAD").unwrap();
    assert!(input.paths.is_empty());
    ctx.input_paths = Some(input.paths);
    let (_, ids, _) = verdict(&ctx, Err("fixture scanner unavailable".into()));
    assert!(ids.is_empty());
}

/// Everything the stage shows: its rows and each finding's place and words.
fn shown(r: &Report) -> String {
    let mut text = r.render();
    for f in r.findings() {
        text.push_str(&f.id);
        text.push('\n');
        text.push_str(&f.message.join("\n"));
        text.push('\n');
    }
    text
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
    let scanner = aterm_uds::host::need_tool("gitleaks");
    let key = planted_key();
    let (_dir, ctx) = checkout(&format!("// a fixture\nlet api_key = \"{key}\";\n"));
    let (outcomes, ids, report) = verdict(&ctx, Ok(Scanner::Fixture(scanner.clone())));
    require_fixture(&report).unwrap_or_else(|why| aterm_uds::host::could_not_run(why));
    assert_eq!(
        outcomes,
        [Outcome::Fail(Severity::GateFailed)],
        "{ids:?}; {report:#?}"
    );
    assert_eq!(ids, ["crates/x/src/lib.rs:2"], "{report:#?}");
    let (_dir, ctx) = checkout("// a fixture\nlet api_key = \"\";\n");
    let (outcomes, ids, report) = verdict(&ctx, Ok(Scanner::Fixture(scanner)));
    require_fixture(&report).unwrap_or_else(|why| aterm_uds::host::could_not_run(why));
    assert_eq!(outcomes, [Outcome::Ok], "{ids:?}; {report:#?}");
}

/// The values of the two test lines the engine's gitleaks refused in cuts on 2026-11-01, made at
/// run time so this file holds neither: a tape test's generation beside the word `key`, and a db
/// test row's twelve-digit `key`.
fn incident_values() -> [String; 2] {
    [
        ["if-gen=", "0.1096"].concat(),
        ["012345", "6789ab"].concat(),
    ]
}

/// Those two lines, each as first written (`fixed` false) or as its fix forward wrote it
/// (65cd90c756 bound the generation apart from the word; 064d85a9a9 made the row's value twelve
/// of one letter): the tape test's, then the db test's.
fn incident_lines(fixed: bool) -> [String; 2] {
    const WORD: &str = "key";
    let [generation, digits] = incident_values();
    if fixed {
        [
            format!("let gen_1096 = \"{generation}\";\nlet enter = [\"{WORD}\", gen_1096];\n"),
            format!("let row = json!({{\"{WORD}\": \"a\".repeat(12)}});\n"),
        ]
    } else {
        [
            format!("let enter = [\"{WORD}\", \"{generation}\"];\n"),
            format!("let row = json!({{\"{WORD}\": \"{digits}\"}});\n"),
        ]
    }
}

/// THE CHANGE IS WHAT THE BRANCH COMMITTED AND WHAT IT HAS NOT YET: the two lines the engine
/// refused on 2026-11-01, one committed on a topic branch and one an unstaged edit of a file that
/// branch tracks, are each found at their line by the gitleaks guard's rule, and neither value is
/// shown. Once a later commit fixes the first forward and the edit is rewritten as the second's
/// fix, the change passes: the snapshot is judged, as the export judges it.
#[test]
fn a_line_committed_on_the_branch_or_left_uncommitted_is_found_by_place_and_rule() {
    let scanner = aterm_uds::host::need_tool("gitleaks");
    let [tape, db] = incident_lines(false);
    let (_dir, ctx) = checkout(&format!("// a tape test\n{tape}"));
    let lib = ctx.root.join("crates/x/src/lib.rs");
    let row = ctx.root.join("crates/x/src/row.rs");
    std::fs::write(&row, "// a db test\n").expect("row.rs");
    git(&ctx.root, &["checkout", "-q", "-b", "topic"]);
    git(&ctx.root, &["add", "-A"]);
    git(&ctx.root, &["commit", "-q", "-m", "the branch's commit"]);
    std::fs::write(&row, format!("// a db test\n{db}")).expect("an unstaged edit");
    let (_, mut ids, r) = verdict(&ctx, Ok(Scanner::Fixture(scanner.clone())));
    require_fixture(&r).unwrap_or_else(|why| aterm_uds::host::could_not_run(why));
    let text = shown(&r);
    ids.sort();
    assert_eq!(
        ids,
        ["crates/x/src/lib.rs:2", "crates/x/src/row.rs:2"],
        "{text}"
    );
    assert!(
        r.findings().iter().all(|f| f
            .message
            .iter()
            .any(|m| m.starts_with("gitleaks: generic-api-key"))),
        "{text}"
    );
    for value in incident_values() {
        assert!(!text.contains(&value), "the value is shown: {text}");
    }
    let [tape, db] = incident_lines(true);
    std::fs::write(&lib, format!("// a tape test\n{tape}")).expect("the fix forward");
    git(
        &ctx.root,
        &[
            "commit",
            "-q",
            "-m",
            "the fix forward",
            "--",
            "crates/x/src/lib.rs",
        ],
    );
    std::fs::write(&row, format!("// a db test\n{db}")).expect("the edit fixed");
    let (outcomes, ids, r) = verdict(&ctx, Ok(Scanner::Fixture(scanner)));
    require_fixture(&r).unwrap_or_else(|why| aterm_uds::host::could_not_run(why));
    assert_eq!(outcomes, [Outcome::Ok], "{ids:?}; {r:#?}");
}

/// A HOST WITHOUT GITLEAKS IS COULD NOT RUN, NEVER A PASS: over a change gitleaks would pass,
/// the stage that could not acquire it says so in a COULD NOT RUN row that names where it comes
/// from, beside the pass of the guards it could run.
#[test]
fn a_host_without_gitleaks_is_could_not_run_never_a_pass() {
    let (_dir, ctx) = checkout("// a fixture\nlet api_key = \"\";\n");
    let mut r = Report::new("export-scan");
    run_with(&ctx, &mut r, || Err("no signed scanner in fixture".into()));
    let rows: Vec<(Outcome, String)> = r.outcomes().map(|(o, l)| (o, l.to_string())).collect();
    let outcomes: Vec<Outcome> = rows.iter().map(|(o, _)| *o).collect();
    assert_eq!(
        outcomes,
        [Outcome::Ok, Outcome::Fail(Severity::CouldNotRun)],
        "{rows:#?}"
    );
    assert!(
        rows[1].1.contains("gitleaks") && rows[1].1.contains("aterm pkg install gitleaks"),
        "{rows:#?}"
    );
}

/// GITLEAKS' REPORT AND THE VERDICT HOLD NO MATCHED SECRET: gitleaks writes its report redacted,
/// so the key it found is neither in that file (which a run ended before its cleanup leaves in
/// the scratch folder) nor in anything the stage shows.
#[test]
fn neither_gitleaks_report_nor_the_verdict_holds_the_secret() {
    let scanner = aterm_uds::host::need_tool("gitleaks");
    let key = planted_key();
    let line = format!("// a fixture\nlet api_key = \"{key}\";\n");
    let (_dir, ctx) = checkout(&line);
    let (_, _, r) = verdict(&ctx, Ok(Scanner::Fixture(scanner.clone())));
    require_fixture(&r).unwrap_or_else(|why| aterm_uds::host::could_not_run(why));
    assert!(!r.findings().is_empty(), "{}", r.render());
    assert!(!shown(&r).contains(&key), "{}", shown(&r));
    let work = ctx.scratch.join("redaction.noindex");
    std::fs::create_dir_all(work.join("tree/crates/x/src")).expect("tree");
    let tree = work.join("tree").canonicalize().expect("canonical");
    std::fs::write(tree.join(CONFIG), ACCEPTED).expect("config");
    std::fs::write(tree.join("crates/x/src/lib.rs"), &line).expect("lib.rs");
    let leaks =
        gitleaks_leaks(&ctx, &Scanner::Fixture(scanner), &work, &tree).expect("gitleaks ran");
    assert_eq!(
        leaks.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(),
        ["crates/x/src/lib.rs:2"]
    );
    let written = std::fs::read_to_string(work.join("gitleaks.json")).expect("its report");
    assert!(written.contains("\"RuleID\""), "{written}");
    assert!(
        !written.contains(&key),
        "the report holds the key: {written}"
    );
}

/// THE ENGINE'S BASELINE IS ITS COMMIT, NEVER ITS WORKING TREE: a row a session left
/// uncommitted in the named engine's checkout judges nothing, and the label names the commit
/// the scan held the export to. NEGATIVE CONTROL: the same row, committed, is a finding.
#[test]
fn an_uncommitted_engine_row_is_not_the_engines_until_it_is_committed() {
    let (_dir, ctx) = checkout("// a fixture\n// wombatword\n");
    let engine = std::path::PathBuf::from(ctx.env.engine_var("PUBLICATION_ENGINE").expect("named"));
    let rows = engine.join(rows::BASELINE);
    std::fs::write(&rows, "# Paths\nquokkaword\nwombatword\n").expect("edit");
    let scan = |ctx: &Ctx| {
        let mut r = Report::new("export-scan");
        run_with(ctx, &mut r, || Err("no signed scanner in fixture".into()));
        let labels: Vec<String> = r.outcomes().map(|(_, l)| l.to_string()).collect();
        let ids: Vec<String> = r.findings().iter().map(|f| f.id.clone()).collect();
        (labels, ids)
    };
    let (labels, uncommitted) = scan(&ctx);
    assert!(
        !uncommitted.contains(&"crates/x/src/lib.rs:2".to_string()),
        "an uncommitted row is no part of the engine: {uncommitted:?}"
    );
    assert!(
        labels.iter().any(|l| l.contains("at HEAD")),
        "the label names the commit: {labels:#?}"
    );
    git(&engine, &["commit", "-q", "-am", "row"]);
    let (_, committed) = scan(&ctx);
    assert!(
        committed.contains(&"crates/x/src/lib.rs:2".to_string()),
        "NEGATIVE CONTROL: committed, the row holds: {committed:?}"
    );
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
    let (_, with, _) = verdict(&ctx, Err("no signed scanner in fixture".into()));
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
fn scanner_freshness_transport_is_could_not_run_not_an_install_remedy() {
    let (_dir, ctx) = checkout("pub fn clean() {}\n");
    let mut report = Report::new("export-scan");
    run_with(&ctx, &mut report, || {
        Err("signed channel freshness transport: curl: (56) HTTP 504".into())
    });
    let rows: Vec<(Outcome, String)> = report
        .outcomes()
        .map(|(outcome, line)| (outcome, line.to_string()))
        .collect();
    let (_, line) = rows
        .iter()
        .find(|(outcome, line)| {
            *outcome == Outcome::Fail(Severity::CouldNotRun) && line.contains("HTTP 504")
        })
        .unwrap_or_else(|| panic!("transport detail did not reach COULD NOT RUN: {rows:#?}"));
    assert!(line.contains("transport answers"), "{line}");
    assert!(
        !line.contains("pkg install"),
        "negative control: replacing local bytes cannot establish channel freshness: {line}"
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
