// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// Every cell's commands go through the executor the caller installed on its thread, the
/// cells the walk resolves on threads of its own included, so a caller that owns them (verify's
/// bounded walk) can stop and reap each. NEGATIVE CONTROL: a thread that does not inherit the
/// executor spawns its `tree` itself, which the executor never counts.
#[test]
fn every_cells_commands_stay_in_the_callers_custody() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let asked = std::sync::Arc::new(AtomicUsize::new(0));
    let counted = std::sync::Arc::clone(&asked);
    let parallel_mode = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let allowed = parallel_mode.clone();
    let caller = std::thread::current().id();
    let executor: std::sync::Arc<aterm_uds::output::Executor> =
        std::sync::Arc::new(move |_: &mut aterm_uds::spawnfd::Command| {
            if !allowed.load(Ordering::SeqCst) {
                assert_eq!(std::thread::current().id(), caller);
            }
            counted.fetch_add(1, Ordering::SeqCst);
            Err(std::io::Error::other("refused by the caller"))
        });
    let cells: Vec<Cell> = ["one", "two", "three"]
        .into_iter()
        .map(|name| Cell {
            name: name.into(),
            triple: format!("{name}-unknown-none"),
            package: "aterm".into(),
        })
        .collect();
    for parallel in [true, false] {
        parallel_mode.store(parallel, Ordering::SeqCst);
        let cargo = resolve::Cargo::pinned("/nonexistent/targo", "/nonexistent")
            .with_parallel_cells(parallel);
        let before = asked.load(Ordering::SeqCst);
        let resolved = aterm_uds::output::with_executor(executor.clone(), || {
            resolve_cells(&cargo, &aterm_tempfile::manifest_dir(), &cells)
        });
        assert_eq!(resolved.len(), cells.len());
        for (log, cell) in &resolved {
            let Err(why) = cell else {
                panic!("the caller refused its command: {log}");
            };
            assert!(why.contains("refused by the caller"), "{why}\n{log}");
        }
        assert!(asked.load(Ordering::SeqCst) - before >= cells.len());
    }
}

/// A GREEN run whose attest could not diff a fork says so instead of "provenance attested";
/// the control, with nothing unverified, keeps the plain words.
#[test]
fn a_green_verdict_names_the_forks_ob7_could_not_diff() {
    let counts = |unverified: &[String]| {
        green_verdict(&GreenCounts {
            forks: 0,
            absorbed: 0,
            first_party: 14,
            cells: 8,
            live_all: 0,
            live_some: 0,
            live_build: 0,
            live_lane: 0,
            carved: 0,
            notes: unverified.len(),
            unverified,
        })
    };
    let plain = counts(&[]);
    assert!(plain.contains("; provenance attested;"), "{plain}");
    let winit = counts(&["winit".to_string()]);
    assert!(
        winit.contains("provenance attested except [OB-7] for winit (UNVERIFIED)"),
        "{winit}"
    );
}

/// Only a `path` patch is a fork; `git` and `version` patches are named
/// apart, and a manifest with no patch table has neither.
#[test]
fn a_git_or_version_patch_is_not_counted_as_a_path_fork() {
    let text = "\
[patch.crates-io]
alpha = { path = \"vendor/alpha\" }
beta = { git = \"https://example.invalid/beta\" }
gamma = { version = \"1.2.3\" }
";
    let (forks, other) = parse_patch_table(text, Path::new("/w/Cargo.toml")).unwrap();
    assert_eq!(
        forks,
        [PatchEntry {
            name: "alpha".into(),
            key: "alpha".into(),
            path: "vendor/alpha".into()
        }]
    );
    assert_eq!(other, ["beta", "gamma"]);
    let (forks, other) = parse_patch_table("[package]\nname = \"x\"\n", Path::new("x")).unwrap();
    assert!(forks.is_empty() && other.is_empty());
}

#[test]
fn the_carve_ledger_reads_both_the_table_and_the_inline_form() {
    let tables = "\
[[carved]]
path = \"crates/aterm-window/src/platform_impl/orbital\"
reason = \"aterm ships no Orbital GUI\"

[[carved]]
path = \"vendor/libm/src/math/arch\"
reason = \"no arch intrinsics reach the shipped build\"
";
    let rows = parse_carve_ledger(tables, Path::new("/w/vendor/forge.toml")).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0].path,
        "crates/aterm-window/src/platform_impl/orbital"
    );
    assert_eq!(rows[1].reason, "no arch intrinsics reach the shipped build");

    let inline = "carved = [ { path = \"vendor/a/b\", reason = \"r\" } ]\n";
    let rows = parse_carve_ledger(inline, Path::new("f")).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].path, "vendor/a/b");
}

/// A carve row the gate cannot hold to the tree is refused, never skipped:
/// no path, a path outside the repository, or a key of the wrong shape.
#[test]
fn a_carve_row_that_cannot_be_checked_is_refused() {
    for text in [
        "[[carved]]\nreason = \"x\"\n",
        "[[carved]]\npath = \"/etc/passwd\"\n",
        "[[carved]]\npath = \"../outside\"\n",
        "[[carved]]\npath = \"vendor/../../x\"\n",
        "[[carved]]\npath = \"./vendor/x\"\n",
        "[[carved]]\npath = \"vendor/./x\"\n",
        "[[carved]]\npath = \"vendor//x\"\n",
        "[[carved]]\npath = \"vendor/x/\"\n",
        "[[carved]]\npath = \"vendor\\\\x\"\n",
        "carved = \"vendor/x\"\n",
    ] {
        assert!(parse_carve_ledger(text, Path::new("f")).is_err(), "{text}");
    }
}

#[test]
fn a_workspace_forest_is_read_as_one_tree_per_member() {
    let text = "\
0a v0.1.0 (/w/crates/a)
1x v1.0.0
2y v1.0.0

0b v0.1.0 (/w/crates/b)
1z v2.0.0 (/w/crates/z)
";
    let trees = parse_forest(text).expect("two members");
    let roots: Vec<&str> = trees.iter().map(|(g, _)| g.root.name.as_str()).collect();
    assert_eq!(roots, ["a", "b"]);
    assert!(trees[1].0.nodes.iter().any(|n| n.name == "z"));
    assert!(!trees[0].0.nodes.iter().any(|n| n.name == "z"));
    assert!(parse_forest("").is_err(), "no tree is no resolve");
}

#[test]
fn classify_separates_the_fork_from_its_unpatched_siblings() {
    let text = "\
0root v0.1.0 (/w/crates/root)
1winnow v0.7.15 (/w/vendor/winnow)
1dep v1.0.0
2winnow v1.0.3
";
    let (graph, paths) = resolve::parse_tree(text).unwrap();
    let (live, sibs) = classify(&graph, &paths, "winnow", &plain("/w/vendor/winnow"));
    assert!(live, "the path package at the declared dir is the fork");
    assert_eq!(sibs.len(), 1);
    assert_eq!(sibs[0].0, PkgId::new("winnow", "1.0.3"));
    assert!(sibs[0].1.is_none(), "the sibling came from the registry");

    let (live, sibs) = classify(&graph, &paths, "winnow", &plain("/w/forks/winnow"));
    assert!(!live);
    assert_eq!(sibs.len(), 2);
}

/// An un-renamed row at `want`, claiming every copy of its name.
fn plain(want: &str) -> RowScope {
    RowScope {
        want: PathBuf::from(want),
        others: Vec::new(),
        renamed: false,
        version: Some("0.7.15".into()),
    }
}

/// Two version lines of one package, patched by a renamed row: a copy in
/// another line is not the row's sibling, one in its own line is, and
/// another row's target is never one.
#[test]
fn a_renamed_row_answers_for_its_own_version_line_only() {
    let text = "\
0root v0.1.0 (/w/crates/root)
1ring v0.17.14
2windows-sys v0.52.0 (/w/crates/aterm-windows-sys-052)
1http v0.1.0 (/w/crates/http)
2windows-sys v0.61.2
";
    let (graph, paths) = resolve::parse_tree(text).unwrap();
    let renamed = |others: Vec<PathBuf>| RowScope {
        want: PathBuf::from("/w/crates/aterm-windows-sys-052"),
        others,
        renamed: true,
        version: Some("0.52.0".into()),
    };
    let (live, sibs) = classify(&graph, &paths, "windows-sys", &renamed(Vec::new()));
    assert!(live);
    assert!(sibs.is_empty(), "0.61.2 is another line: {sibs:?}");

    let mut unscoped = renamed(Vec::new());
    unscoped.renamed = false;
    let (_, sibs) = classify(&graph, &paths, "windows-sys", &unscoped);
    assert_eq!(sibs.len(), 1, "an un-renamed row owns every copy");
    assert_eq!(sibs[0].0, PkgId::new("windows-sys", "0.61.2"));

    let own_line = "\
0root v0.1.0 (/w/crates/root)
1ring v0.17.14
2windows-sys v0.52.0 (/w/crates/aterm-windows-sys-052)
1old v1.0.0
2windows-sys v0.52.1
";
    let (graph, paths) = resolve::parse_tree(own_line).unwrap();
    let (_, sibs) = classify(&graph, &paths, "windows-sys", &renamed(Vec::new()));
    assert_eq!(sibs.len(), 1);
    assert_eq!(sibs[0].0, PkgId::new("windows-sys", "0.52.1"));

    let mut blind = renamed(Vec::new());
    blind.version = None;
    let (graph, paths) = resolve::parse_tree(text).unwrap();
    let (_, sibs) = classify(&graph, &paths, "windows-sys", &blind);
    assert_eq!(sibs.len(), 1);

    let two_rows = "\
0root v0.1.0 (/w/crates/root)
1ring v0.17.14
2windows-sys v0.52.0 (/w/crates/aterm-windows-sys-052)
1http v0.1.0 (/w/crates/http)
2windows-sys v0.52.0 (/w/crates/other-052)
";
    let (graph, paths) = resolve::parse_tree(two_rows).unwrap();
    let (_, sibs) = classify(
        &graph,
        &paths,
        "windows-sys",
        &renamed(vec![PathBuf::from("/w/crates/other-052")]),
    );
    assert!(sibs.is_empty(), "{sibs:?}");
}

#[test]
fn a_renamed_row_is_read_as_the_package_it_names() {
    let text = "\
[patch.crates-io]
windows-sys-052 = { package = \"windows-sys\", path = \"crates/aterm-windows-sys-052\" }
";
    let (forks, other) = parse_patch_table(text, Path::new("/w/Cargo.toml")).unwrap();
    assert!(other.is_empty());
    assert_eq!(
        forks,
        [PatchEntry {
            name: "windows-sys".into(),
            key: "windows-sys-052".into(),
            path: "crates/aterm-windows-sys-052".into()
        }]
    );
}

/// A registry sibling in the lock keeps an oracle honest; its absence is capture.
#[test]
fn a_registry_sibling_is_what_separates_an_escaped_oracle_from_a_captured_one() {
    let escaped = vec![("0.7.8".to_owned(), false), ("0.7.7".to_owned(), true)];
    assert_eq!(
        oracle_verdict(&escaped),
        OracleVerdict::Escapes("0.7.7".to_owned())
    );

    let captured = vec![("0.7.8".to_owned(), false)];
    assert_eq!(oracle_verdict(&captured), OracleVerdict::Captured);

    assert_eq!(oracle_verdict(&[]), OracleVerdict::Captured);
}

/// Plain, target-gated, renamed and feature-optional holders are found; non-optional normal
/// dependencies, inherited entries and path entries are not.
#[test]
fn the_oracle_scanner_finds_plain_and_renamed_holders() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    std::fs::write(root.join("Cargo.toml"), "[workspace]\n").expect("root manifest");
    for (name, body) in [
        (
            "renamed",
            "[dev-dependencies]\narrayvec_upstream = { package = \"arrayvec\", version = \"=0.7.7\" }\n",
        ),
        (
            "plain",
            "[target.'cfg(unix)'.dev-dependencies]\nmemchr = \"2\"\n",
        ),
        (
            "optional_oracle",
            "[dependencies]\nsha2_ref = { package = \"sha2\", version = \"=0.10.8\", optional = true }\n\
             normal_only = \"1.0\"\n\
             [target.'cfg(unix)'.dependencies]\n\
             blake_ref = { package = \"blake3\", version = \"1.5\", optional = true }\n",
        ),
        (
            "inherited",
            "[dev-dependencies]\narrayvec = { workspace = true }\nmemchr = { path = \"x\" }\n\
             [dependencies]\nsha2 = { workspace = true, optional = true }\n",
        ),
    ] {
        std::fs::create_dir_all(root.join("crates").join(name)).expect("crate dir");
        std::fs::write(root.join("crates").join(name).join("Cargo.toml"), body)
            .expect("member manifest");
    }
    let arrayvec = dev_dependency_oracles(root, "arrayvec");
    assert!(
        arrayvec.len() == 1 && arrayvec[0].0.contains("renamed") && arrayvec[0].1 == "=0.7.7",
        "the RENAMED oracle, and only it, must be found: {arrayvec:?}"
    );
    let memchr = dev_dependency_oracles(root, "memchr");
    assert!(
        memchr.len() == 1 && memchr[0].0.contains("plain") && memchr[0].1 == "2",
        "the target-gated oracle, and only it, must be found: {memchr:?}"
    );
    let sha2 = dev_dependency_oracles(root, "sha2");
    assert!(
        sha2.len() == 1 && sha2[0].0.contains("optional_oracle") && sha2[0].1 == "=0.10.8",
        "optional [dependencies] oracle must be found and workspace-inherited skipped: {sha2:?}"
    );
    let blake3 = dev_dependency_oracles(root, "blake3");
    assert!(
        blake3.len() == 1 && blake3[0].0.contains("optional_oracle") && blake3[0].1 == "1.5",
        "optional target-gated [dependencies] oracle must be found: {blake3:?}"
    );
    assert!(
        dev_dependency_oracles(root, "normal_only").is_empty(),
        "non-optional [dependencies] entries are not dev/optional oracles"
    );
}

/// `[OB-12]` through the verb over a fake cargo: `sh` runs the root's
/// `tree` script as cargo's `tree` verb, printing per root package, triple
/// and edge kind.
#[cfg(unix)]
#[test]
fn patch_liveness_is_judged_in_every_cell_at_its_own_root() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    for (rel, text) in [
        (
            "Cargo.toml",
            "[workspace]\n\n[patch.crates-io]\nfoo = { path = \"crates/foo\" }\n",
        ),
        (
            "crates/foo/Cargo.toml",
            "[package]\nname = \"foo\"\nversion = \"1.0.0\"\nlicense = \"Apache-2.0\"\n",
        ),
        ("crates/foo/src/lib.rs", ""),
        (
            "Cargo.lock",
            "version = 4\n\n[[package]]\nname = \"foo\"\nversion = \"1.0.0\"\n",
        ),
        ("NOTICE", ""),
        ("deny.toml", "[licenses]\nallow = []\n"),
        (
            "vendor/forge.toml",
            "[forge]\n\
             loc_method = \"rs-physical-all-files-v1\"\n\
             graph_method = \"cargo-tree-normal-no-dedupe-locked-offline-v1\"\n",
        ),
        (
            "tree",
            "while [ $# -gt 0 ]; do\n\
             case $1 in -p) pkg=$2 ;; --target) triple=$2 ;; --edges) edges=$2 ;; \
             --workspace) ws=1 ;; esac\n\
             shift\n\
             done\n\
             if [ -n \"$ws\" ]; then\n\
             echo '0aterm v0.1.0'; echo '1bar v1.0.0'; echo; echo '0lane v0.1.0'\n\
             [ -f lane ] && echo \"1foo v1.0.0 ($(pwd -P)/crates/foo)\"\n\
             [ -f registry ] && echo '1foo v1.0.0'\n\
             exit 0\n\
             fi\n\
             case \"$pkg $edges\" in broken*|'nobuild normal,build') exit 101 ;; esac\n\
             echo \"0$pkg v0.1.0\"\n\
             case \"$pkg $edges\" in\n\
             aterm*|web*|'tool normal,build'|'mixed normal,build')\n\
             echo \"1foo v1.0.0 ($(pwd -P)/crates/foo)\" ;;\n\
             esac\n\
             case \"$pkg $triple $edges\" in\n\
             'web wasm32-unknown-unknown '*|'mixed '*' normal,build') echo '1foo v0.9.0' ;;\n\
             esac\n",
        ),
    ] {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("fixture dir");
        std::fs::write(&path, text).expect("fixture file");
    }
    let cargo = Cargo::pinned("/bin/sh", "/usr/bin:/bin");
    let cell = |package: &str, triple: &str| Cell {
        name: format!("{package}-cell"),
        triple: triple.into(),
        package: package.into(),
    };
    let native = cell("aterm", "aarch64-apple-darwin");
    let web = cell("web", "wasm32-unknown-unknown");
    let tool = cell("tool", "aarch64-apple-darwin");
    let mixed = cell("mixed", "aarch64-apple-darwin");
    let bare = cell("bare", "aarch64-apple-darwin");
    let broken = cell("broken", "aarch64-apple-darwin");
    let nobuild = cell("nobuild", "aarch64-apple-darwin");
    let rows: [(&[&Cell], (bool, bool)); 7] = [
        (&[&bare, &native], (true, false)),
        (&[&tool, &bare], (true, false)),
        (&[&web, &native], (false, false)),
        (&[&bare], (false, false)),
        (&[&mixed], (false, false)),
        (&[&broken, &native], (false, true)),
        (&[&nobuild], (false, true)),
    ];
    for (cells, want) in rows {
        let cells: Vec<Cell> = cells.iter().copied().cloned().collect();
        let v = report_over(&cargo, root, &cells);
        assert_eq!((v.ok, v.could_not_run.is_some()), want, "{}", v.log);
        assert!(v.ok || v.log.contains("FAIL [OB-12]"), "{}", v.log);
    }
    // A patch no cell reaches is dead unless the workspace's resolve with
    // every feature reaches it at the patch's path (a lane's feature); the
    // registry's package there is not the patch.
    for (marker, want_ok) in [("registry", false), ("lane", true)] {
        std::fs::write(root.join(marker), "").expect("marker");
        let v = report_over(&cargo, root, std::slice::from_ref(&bare));
        assert_eq!(
            (v.ok, v.could_not_run.is_some()),
            (want_ok, false),
            "{}",
            v.log
        );
        assert_eq!(
            v.log.contains("live only OUTSIDE the cells"),
            want_ok,
            "{}",
            v.log
        );
        assert_eq!(v.log.contains("DEAD PATCH"), !want_ok, "{}", v.log);
        std::fs::remove_file(root.join(marker)).expect("marker");
    }
    let cells: Vec<Cell> = [&native, &web, &broken, &tool, &bare, &mixed]
        .into_iter()
        .cloned()
        .collect();
    let roots: Vec<Option<String>> = resolve_cells(&cargo, root, &cells)
        .into_iter()
        .map(|(_, resolved)| resolved.ok().map(|(graph, _)| graph.root.name))
        .collect();
    let own: Vec<Option<String>> = cells
        .iter()
        .map(|c| (c.package != "broken").then(|| c.package.clone()))
        .collect();
    assert_eq!(roots, own, "each cell's own graph, in cells order");

    std::fs::create_dir_all(root.join("vendor")).expect("vendor dir");
    std::fs::write(
        root.join("vendor/forge.toml"),
        "[[carved]]\npath = \"crates/foo/src/carved.rs\"\nreason = \"dead\"\n",
    )
    .expect("forge.toml");
    std::os::unix::fs::symlink(
        root.join("crates/foo/src/missing-target.rs"),
        root.join("crates/foo/src/carved.rs"),
    )
    .expect("broken symlink");
    let v = report_over(&cargo, root, &[bare, native]);
    assert!(
        !v.ok && v.log.contains("FAIL [OB-13]"),
        "a broken symlink at a carved path must fail OB-13:\n{}",
        v.log
    );
}
