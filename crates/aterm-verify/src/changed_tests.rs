// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A `-p` naming a package of the lock that the host's build graph lacks is named, with who
/// builds it; one the graph holds, one the lock lacks (a typo cargo names), a partial
/// version and a path spec read as cargo reads them.
#[test]
fn a_spec_outside_the_hosts_graph_is_named_before_cargo_panics_on_it() {
    let lock = lock_packages(
        "version = 4\n\n[[package]]\nname = \"windows-sys\"\nversion = \"0.52.0\"\n\n\
         [[package]]\nname = \"windows-sys\"\nversion = \"0.61.2\"\ndependencies = [\n \
         \"windows-link\",\n]\n\n[[package]]\nname = \"aterm-core\"\nversion = \"0.148.0\"\n",
    );
    assert_eq!(lock.len(), 3, "{lock:?}");
    let graph = parse_tree_packages(
        "aterm-core v0.148.0 (/r/crates/aterm-core)\n\
         windows-sys v0.52.0 (/r/crates/aterm-windows-sys-052)\n\
         windows-sys v0.52.0 (/r/crates/aterm-windows-sys-052) (*)\n",
    );
    let specs = |s: &[&str]| s.iter().map(|x| (*x).to_string()).collect::<Vec<_>>();
    assert_eq!(
        outside_graph(
            &specs(&[
                "windows-sys@0.61.2",
                "windows-sys@0.61",
                "path+file:///r/crates/aterm-windows-sys#windows-sys@0.61.2",
                "windows-sys@0.52.0",
                "aterm-core",
                "aterm-nonesuch",
                "windows-sys@0.6",
            ]),
            &lock,
            &graph
        ),
        specs(&[
            "windows-sys@0.61.2",
            "windows-sys@0.61",
            "path+file:///r/crates/aterm-windows-sys#windows-sys@0.61.2",
        ]),
    );
    let held = parse_tree_packages(
        "aterm-core v0.148.0 (/r/crates/aterm-core)\n\
         windows-sys v0.61.2 (/r/crates/aterm-windows-sys)\n",
    );
    assert!(
        outside_graph(&specs(&["windows-sys@0.61.2"]), &lock, &held).is_empty(),
        "negative control: a graph that holds it refuses nothing"
    );

    let consumers = direct_dependents(
        "0windows-sys v0.61.2 (/r/crates/aterm-windows-sys)\n\
         1aterm-window v0.30.13 (/r/crates/aterm-window)\n\
         2aterm-gui v0.148.0 (/r/crates/aterm-gui)\n\
         1aterm-window v0.30.13 (/r/crates/aterm-window) (*)\n",
    );
    assert_eq!(consumers, ["aterm-window"]);
    let line = foreign_refusal(&[Foreign {
        spec: "windows-sys@0.61.2".into(),
        consumers,
    }]);
    for named in [
        "-p windows-sys@0.61.2",
        "aterm-window",
        "verify cells -p windows-sys@0.61.2.",
    ] {
        assert!(line.contains(named), "{named}: {line}");
    }
}

fn select(
    paths: &[String],
    manifests: &Manifests,
    members: &Members,
    path_packages: &dyn Fn() -> Option<Vec<(String, String)>>,
    dependents: &dyn Fn(&str) -> Option<Vec<String>>,
) -> Selection {
    super::select(
        paths,
        manifests,
        members,
        path_packages,
        &|name| dependents(name).map(linking),
        &|_| replan_everything(),
        &|_, _| false,
    )
}

fn linking(links: Vec<String>) -> Reach {
    Reach {
        links,
        ..Reach::default()
    }
}

const MANIFEST: &str = r#"# The workspace.
[workspace]
members = ["crates/*"]
default-members = ["crates/a"]

[workspace.package]
version = "0.1.0"
edition = "2024"

[workspace.dependencies]
# TLS.
ring = { version = "0.17.14", default-features = false }
serde = { package = "aterm-serde", path = "crates/aterm-serde" }

[profile.dev]
debug = "line-tables-only"

[profile.dev.package.aterm-png]
opt-level = 2

[patch.crates-io]
libc = { path = "crates/aterm-libc" }
"#;

#[test]
fn a_root_manifest_change_selects_what_its_tables_reach() {
    let edit = |from: &str, to: &str| {
        assert!(MANIFEST.contains(from), "{from}");
        manifest_replan(Some(MANIFEST), Some(&MANIFEST.replace(from, to)))
    };
    let packages = |names: &[&str]| Replan::Packages(names.iter().map(|n| n.to_string()).collect());
    assert_eq!(edit("# TLS.", "# TLS, via ring."), Replan::Nothing);
    assert_eq!(
        edit(
            "

[profile.dev]",
            "


[profile.dev]"
        ),
        Replan::Nothing
    );
    assert_eq!(
        edit(
            r#"default-members = ["crates/a"]"#,
            r#"default-members = ["crates/a", "crates/b"]"#
        ),
        Replan::Nothing
    );
    assert_eq!(edit(r#""0.17.14""#, r#""0.17.15""#), packages(&["ring"]));
    assert_eq!(
        edit(
            r#"path = "crates/aterm-serde" }"#,
            r#"path = "crates/aterm-serde", features = ["derive"] }"#
        ),
        packages(&["aterm-serde"]),
        "a renamed row selects the package it names"
    );
    assert_eq!(
        edit(
            "[workspace.dependencies]
",
            "[workspace.dependencies]
aterm-x = { path = \"crates/x\" }
"
        ),
        packages(&["aterm-x"])
    );
    assert_eq!(
        edit("opt-level = 2", "opt-level = 1"),
        packages(&["aterm-png"])
    );
    assert_eq!(
        edit(r#"version = "0.1.0""#, r#"version = "0.2.0""#),
        packages(&["aterm"]),
        "a bump reaches the one package that reads the product version"
    );
    for (from, to) in [
        ("libc = { path", "libm = { path"),
        (r#"edition = "2024""#, r#"edition = "2021""#),
        (
            r#"version = "0.1.0""#,
            "version = \"0.2.0\"\nrust-version = \"1.90\"",
        ),
        (r#"debug = "line-tables-only""#, "debug = false"),
        (
            "[profile.dev.package.aterm-png]",
            "[profile.dev.package.\"*\"]",
        ),
        (
            r#"members = ["crates/*"]"#,
            r#"members = ["crates/*", "x"]"#,
        ),
        ("[patch.crates-io]", "[patch.crates-io"),
    ] {
        assert!(
            matches!(edit(from, to), Replan::Everything(_)),
            "{from} -> {to} must widen"
        );
    }
    assert!(matches!(
        manifest_replan(None, Some(MANIFEST)),
        Replan::Everything(_)
    ));
}

const LOCK: &str = r#"version = 4

[[package]]
name = "aterm-http"
version = "0.105.0"
dependencies = [
 "ring",
]

[[package]]
name = "ring"
version = "0.17.14"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "aaa"
"#;

#[test]
fn a_lock_change_selects_the_packages_whose_entries_changed() {
    let packages = |names: &[&str]| Replan::Packages(names.iter().map(|n| n.to_string()).collect());
    assert_eq!(lock_replan(Some(LOCK), Some(LOCK)), Replan::Nothing);
    let bumped = LOCK.replace("0.17.14", "0.17.15").replace("aaa", "bbb");
    assert_eq!(lock_replan(Some(LOCK), Some(&bumped)), packages(&["ring"]));
    let dropped = LOCK.replace(" \"ring\",\n", "").replace(
        &LOCK[LOCK.find("[[package]]\nname = \"ring\"").expect("ring")..],
        "",
    );
    assert_eq!(
        lock_replan(Some(LOCK), Some(&dropped)),
        packages(&["aterm-http"]),
        "ring left the lock: its one dependent's entry changed"
    );
    let twice = format!(
        "{LOCK}\n[[package]]\nname = \"ring\"\nversion = \"0.16.20\"\nsource = \"registry+x\"\n"
    );
    assert_eq!(
        lock_replan(Some(LOCK), Some(&twice)),
        packages(&["ring@0.16.20"])
    );
    assert!(matches!(
        lock_replan(
            Some(LOCK),
            Some(&LOCK.replace("version = 4", "version = 3"))
        ),
        Replan::Everything(_)
    ));
    assert!(matches!(
        lock_replan(Some(LOCK), Some("[[package")),
        Replan::Everything(_)
    ));
}

#[test]
fn a_replanning_change_narrows_to_what_it_reaches() {
    let m = members(&["aterm-http", "aterm-gui", "aterm-grid"]);
    let edges: &'static [(&str, &str)] = &[("ring", "aterm-http"), ("aterm-http", "aterm-gui")];
    let pick = |replan: Replan| {
        super::select(
            &paths(&["Cargo.toml"]),
            &manifests(),
            &m,
            &no_packages,
            &|name| graph(edges)(name).map(linking),
            &|_| replan.clone(),
            &|_, _| false,
        )
    };
    assert_eq!(narrowed(&pick(Replan::Nothing)), (vec![], vec![]));
    assert_eq!(
        narrowed(&pick(Replan::Packages(BTreeSet::from(
            ["ring".to_string()]
        )))),
        (
            vec!["ring".to_string()],
            vec!["aterm-gui".to_string(), "aterm-http".to_string()]
        )
    );
    assert!(matches!(
        pick(Replan::Everything("[patch] changed".into())),
        Selection::Widened(why) if why == "Cargo.toml changed: [patch] changed"
    ));
}

/// A cone's lint compiles every target of a crate whose own files changed and of one a changed
/// file is tested by. CONTROL: a seed the change reached any other way (a lock row, a root
/// manifest row, a fork) can move code under any member's tests, so the whole cone keeps its
/// test targets, and a root manifest row does so whichever files changed beside it.
#[test]
fn only_what_the_change_touched_has_all_its_targets_linted() {
    let m = members(&["aterm-grid", "aterm-gui", "aterm-update", "aterm-release"]);
    let edges: &'static [(&str, &str)] = &[("ring", "aterm-grid"), ("aterm-grid", "aterm-gui")];
    let forks = || Some(vec![("ring".to_string(), "vendor/ring".to_string())]);
    let all_targets = |changed: &[&str], replan: Replan| match super::select(
        &paths(changed),
        &manifests(),
        &m,
        &forks,
        &|name| graph(edges)(name).map(linking),
        &|_| replan.clone(),
        &|_, _| false,
    ) {
        Selection::Narrowed { all_targets, .. } => all_targets,
        Selection::Widened(why) => panic!("widened unexpectedly: {why}"),
    };
    assert_eq!(
        all_targets(&["crates/aterm-grid/src/lib.rs"], Replan::Nothing),
        ["aterm-grid"],
        "its dependent aterm-gui is in the cone for its default targets"
    );
    assert_eq!(
        all_targets(&["publish/config.sh"], Replan::Nothing),
        ["aterm-release"]
    );
    assert_eq!(
        all_targets(
            &["crates/aterm-grid/src/lib.rs", "publish/config.sh"],
            Replan::Nothing
        ),
        ["aterm-grid", "aterm-release"]
    );
    let cone_of_ring = ["aterm-grid", "aterm-gui"];
    let lock_row = Replan::Packages(BTreeSet::from(["ring".to_string()]));
    assert_eq!(all_targets(&["Cargo.lock"], lock_row.clone()), cone_of_ring);
    assert_eq!(
        all_targets(&["Cargo.toml"], lock_row),
        cone_of_ring,
        "a [workspace.dependencies] row"
    );
    assert_eq!(
        all_targets(&["vendor/ring/src/lib.rs"], Replan::Nothing),
        cone_of_ring
    );
    assert_eq!(
        all_targets(
            &["Cargo.toml"],
            Replan::Packages(BTreeSet::from(["aterm-grid".to_string()]))
        ),
        cone_of_ring,
        "a row naming a member changes how its dependents build it"
    );
    assert_eq!(
        all_targets(
            &["Cargo.toml", "crates/aterm-grid/src/lib.rs"],
            Replan::Packages(BTreeSet::from(["aterm-grid".to_string()]))
        ),
        cone_of_ring,
        "and still does when that member's own files changed with it"
    );
    assert_eq!(
        all_targets(
            &["crates/aterm-grid/Cargo.toml", "Cargo.lock"],
            Replan::Packages(BTreeSet::from(["aterm-grid".to_string()]))
        ),
        ["aterm-grid"],
        "a member's own lock entry, from its own manifest, reaches no one else's tests"
    );
}

/// A crate the cone holds only by a dev edge leaves the lint, because its library and binaries
/// link nothing that changed, and stays in the test compile, which takes its tests. One a seed
/// also links keeps its default targets, and one a changed file is tested by keeps all its
/// targets. CONTROL: a fork keeps the whole cone in the first pass, those crates included.
#[test]
fn a_crate_the_cone_holds_only_by_a_dev_edge_leaves_the_lint() {
    let m = members(&["seed", "seed-two", "normal", "dev", "both", "aterm-update"]);
    let reach = |name: &str| {
        let (links, dev_only): (&[&str], &[&str]) = match name {
            "seed" => (&["seed", "normal"], &["dev", "both", "aterm-update"]),
            "ring" => (&["normal"], &["dev"]),
            _ => (&["seed-two", "both"], &[]),
        };
        let owned = |names: &[&str]| names.iter().map(ToString::to_string).collect();
        Some(Reach {
            links: owned(links),
            dev_only: owned(dev_only),
            direct: Vec::new(),
        })
    };
    let forks = || Some(vec![("ring".to_string(), "vendor/ring".to_string())]);
    let pick = |changed: &[&str]| {
        let manifests: Manifests = ["crates/seed", "crates/seed-two"].into_iter().collect();
        super::select(
            &paths(changed),
            &manifests,
            &m,
            &forks,
            &reach,
            &|_| Replan::Nothing,
            &|_, _| false,
        )
    };
    let touched = [
        "crates/seed/src/lib.rs",
        "crates/seed-two/src/lib.rs",
        "tools/install.sh",
    ];
    let selection = pick(&touched);
    let Selection::Narrowed {
        crates,
        all_targets,
        unlinted,
        ..
    } = &selection
    else {
        panic!("widened unexpectedly: {selection:?}");
    };
    let cone_of_all = ["aterm-update", "both", "dev", "normal", "seed", "seed-two"];
    assert_eq!(crates, &cone_of_all);
    assert_eq!(all_targets, &["aterm-update", "seed", "seed-two"]);
    assert_eq!(unlinted, &["dev"]);
    let (scope, _) = cone("main", &selection);
    let linted = crate::stages::tippy_passes(&scope)
        .into_iter()
        .flat_map(|pass| pass.args)
        .collect::<Vec<_>>();
    assert!(
        !linted.iter().any(|arg| arg == "dev"),
        "the lint leaves it out: {linted:?}"
    );
    assert!(
        crate::stages::test_compile_args(&scope)
            .iter()
            .any(|arg| arg == "dev"),
        "the test compile keeps it"
    );
    let Selection::Narrowed {
        all_targets,
        unlinted,
        ..
    } = pick(&["crates/seed/src/lib.rs", "vendor/ring/src/lib.rs"])
    else {
        panic!("widened unexpectedly");
    };
    assert_eq!(
        all_targets,
        ["aterm-update", "both", "dev", "normal", "seed"]
    );
    assert!(unlinted.is_empty(), "{unlinted:?}");
}

/// What depends directly on a seed, over any edge, reaches the scope: its required-features
/// targets are linted beside the seed's, and those of a member further away are not. A direct
/// dependent outside the workspace is no member. CONTROL: one only further away, `far`.
#[test]
fn the_direct_dependents_of_a_seed_reach_the_scope() {
    let m = members(&["seed", "near", "far", "dev"]);
    let reach = |name: &str| {
        let owned = |names: &[&str]| names.iter().map(ToString::to_string).collect();
        (name == "seed").then(|| Reach {
            links: owned(&["far", "near", "seed"]),
            dev_only: owned(&["dev"]),
            direct: owned(&["dev", "near", "registry-only"]),
        })
    };
    let manifests: Manifests = ["crates/seed"].into_iter().collect();
    let selection = super::select(
        &paths(&["crates/seed/src/lib.rs"]),
        &manifests,
        &m,
        &|| Some(Vec::new()),
        &reach,
        &|_| Replan::Nothing,
        &|_, _| false,
    );
    let Selection::Narrowed {
        direct_dependents, ..
    } = &selection
    else {
        panic!("widened unexpectedly: {selection:?}");
    };
    assert_eq!(direct_dependents, &["dev", "near"]);
    let (scope, _) = cone("main", &selection);
    let gated = |name: &str| scope.gated_spec_of(name);
    assert_eq!(
        ["seed", "near", "dev", "far"].map(gated),
        [
            Some("seed".to_string()),
            Some("near".to_string()),
            Some("dev".to_string()),
            None
        ]
    );
}

#[test]
fn a_move_an_ignored_gitlink_and_a_quoted_path_are_each_listed() {
    let repo = crate::mktemp_dir("atv-changed-listed").expect("mktemp");
    let sub = repo.join("vendor/astream");
    let path_env = std::env::var_os("PATH").unwrap_or_default();
    let git = |dir: &Path, args: &[&str]| {
        let out = git(dir, &path_env)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(["-c", "advice.addEmbeddedRepo=false"])
            .args(args)
            .output()
            .expect("git runs");
        assert!(out.status.success(), "git {args:?}: {out:?}");
    };
    for dir in ["crates/a/src", "crates/b/src", "vendor/astream"] {
        std::fs::create_dir_all(repo.join(dir)).expect("dir");
    }
    std::fs::write(repo.join("crates/a/src/x.rs"), "pub fn x() {}\n").expect("write");
    git(&sub, &["init", "-q"]);
    git(&sub, &["commit", "-q", "--allow-empty", "-m", "one"]);
    git(&repo, &["init", "-q"]);
    for (key, value) in [
        ("diff.renames", "true"),
        ("diff.ignoreSubmodules", "all"),
        ("core.quotePath", "true"),
    ] {
        git(&repo, &["config", key, value]);
    }
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "base"]);
    git(&repo, &["branch", "shared"]);
    git(&repo, &["mv", "crates/a/src/x.rs", "crates/b/src/x.rs"]);
    git(&sub, &["commit", "-q", "--allow-empty", "-m", "two"]);
    git(&repo, &["add", "vendor/astream"]);
    git(&repo, &["commit", "-qm", "move x out of a; bump astream"]);
    std::fs::write(repo.join("crates/b/src/é.rs"), "\n").expect("write");
    let listed = changed_paths(&repo, &path_env, "shared").map(|(_, paths)| paths);
    let _ = std::fs::remove_dir_all(&repo);
    assert_eq!(
        listed.expect("listed"),
        [
            "crates/a/src/x.rs",
            "crates/b/src/x.rs",
            "crates/b/src/é.rs",
            "vendor/astream"
        ]
    );
}

#[test]
fn an_edit_an_index_flag_hides_is_listed() {
    let repo = crate::mktemp_dir("atv-changed-flagged").expect("mktemp");
    let path_env = std::env::var_os("PATH").unwrap_or_default();
    let git = |args: &[&str]| {
        let out = git(&repo, &path_env)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .output()
            .expect("git runs");
        assert!(out.status.success(), "git {args:?}: {out:?}");
    };
    for c in ["a", "b", "c"] {
        std::fs::create_dir_all(repo.join(format!("crates/{c}/src"))).expect("dir");
        std::fs::write(repo.join(format!("crates/{c}/src/lib.rs")), "\n").expect("write");
    }
    let names: Vec<String> = (0..257).map(|i| format!("source-{i:04}.rs")).collect();
    #[cfg(unix)]
    let names = {
        let mut names = names;
        names.extend(["line\nbreak.rs".into(), "\"quoted.rs".into()]);
        names
    };
    for name in &names {
        std::fs::write(repo.join(name), "unchanged\n").expect("write");
    }
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-qm", "base"]);
    git(&["branch", "shared"]);
    git(&["update-index", "--assume-unchanged", "crates/a/src/lib.rs"]);
    git(&["update-index", "--skip-worktree", "crates/b/src/lib.rs"]);
    git(&["update-index", "--skip-worktree", "crates/c/src/lib.rs"]);
    let mut args = vec!["update-index", "--assume-unchanged", "--"];
    args.extend(names.iter().map(String::as_str));
    git(&args);
    std::fs::write(repo.join("crates/a/src/lib.rs"), "pub fn a() {}\n").expect("write");
    std::fs::write(repo.join("crates/b/src/lib.rs"), "pub fn b() {}\n").expect("write");
    let mut want: Vec<String> = names.iter().skip(256).cloned().collect();
    want.push(names[0].clone());
    for name in &want {
        std::fs::write(repo.join(name), "changed\n").expect("write");
    }
    want.extend(["crates/a/src/lib.rs".into(), "crates/b/src/lib.rs".into()]);
    want.sort();
    let listed = changed_paths(&repo, &path_env, "shared").map(|(_, paths)| paths);
    let _ = std::fs::remove_dir_all(&repo);
    assert_eq!(listed.expect("listed"), want);
}

fn members(names: &[&str]) -> Members {
    Members::new(
        names
            .iter()
            .map(|n| Member {
                name: (*n).to_string(),
                dir: format!("crates/{n}"),
                has_lib: true,
            })
            .collect(),
    )
}

fn manifests() -> Manifests {
    [
        "crates/aterm-grid",
        "crates/aterm-gui",
        "crates/aterm-render",
        "crates/aterm-render/tests/font_oracle/recorder",
        "crates/aterm-scrollback",
        "crates/aterm-lab",
        "libc-oracle",
    ]
    .into_iter()
    .collect()
}

fn paths(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_string()).collect()
}

fn no_packages() -> Option<Vec<(String, String)>> {
    Some(Vec::new())
}

fn graph(edges: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<Vec<String>> {
    move |name: &str| {
        let mut out = vec![name.to_string()];
        let mut i = 0;
        while i < out.len() {
            let cur = out[i].clone();
            for (dep, user) in edges {
                if *dep == cur && !out.iter().any(|s| s == user) {
                    out.push((*user).to_string());
                }
            }
            i += 1;
        }
        Some(out)
    }
}

const EDGES: &[(&str, &str)] = &[
    ("aterm-grid", "aterm-gui"),
    ("aterm-gui", "aterm-cli"),
    ("aterm-grid", "aterm-bench"),
];

/// A member whose change only its own tests compile is judged alone: the cone holds none of its
/// dependents and says so. `confined` is asked of each member with the files of its own that
/// changed. CONTROL: the same change unconfined reaches its dependents, a confined seed beside
/// an unconfined one leaves the other's in, and a member a lock row also names is never alone.
#[test]
fn a_change_confined_to_its_own_tests_judges_its_member_alone() {
    let m = members(&["aterm-grid", "aterm-gui", "aterm-cli", "aterm-bench"]);
    let pick = |changed: &[&str], confined: &dyn Fn(&str, &[&str]) -> bool, replan: Replan| {
        super::select(
            &paths(changed),
            &manifests(),
            &m,
            &no_packages,
            &|name| graph(EDGES)(name).map(linking),
            &|_| replan.clone(),
            confined,
        )
    };
    let grid = [
        "crates/aterm-grid/src/lib.rs",
        "crates/aterm-grid/src/tests.rs",
    ];
    let asked = std::cell::RefCell::new(Vec::new());
    let sel = pick(
        &grid,
        &|member, owned| {
            asked.borrow_mut().push((member.to_string(), paths(owned)));
            true
        },
        Replan::Nothing,
    );
    assert_eq!(
        asked.into_inner(),
        [("aterm-grid".to_string(), paths(&grid))]
    );
    assert_eq!(
        sel,
        Selection::Narrowed {
            seeds: paths(&["aterm-grid"]),
            crates: paths(&["aterm-grid"]),
            any_lib: true,
            all_targets: paths(&["aterm-grid"]),
            unlinted: Vec::new(),
            direct_dependents: Vec::new(),
            alone: paths(&["aterm-grid"]),
        }
    );
    let (scope, summary) = cone("base", &sel);
    assert_eq!(summary, "the change cone since base: aterm-grid (alone)");
    assert!(scope.touches("aterm-grid") && !scope.reaches_dependents("aterm-grid"));
    let unconfined = pick(&grid, &|_, _| false, Replan::Nothing);
    assert_eq!(
        narrowed(&unconfined),
        (
            paths(&["aterm-grid"]),
            paths(&["aterm-bench", "aterm-cli", "aterm-grid", "aterm-gui"])
        )
    );
    assert!(!cone("base", &unconfined).1.contains("(alone)"));
    let both = pick(
        &[
            "crates/aterm-grid/src/lib.rs",
            "crates/aterm-gui/src/lib.rs",
        ],
        &|member, _| member == "aterm-grid",
        Replan::Nothing,
    );
    assert_eq!(
        narrowed(&both),
        (
            paths(&["aterm-grid", "aterm-gui"]),
            paths(&["aterm-cli", "aterm-grid", "aterm-gui"])
        )
    );
    let locked = pick(
        &["Cargo.lock", "crates/aterm-grid/src/lib.rs"],
        &|_, _| true,
        Replan::Packages(BTreeSet::from(["aterm-grid".to_string()])),
    );
    assert!(
        matches!(&locked, Selection::Narrowed { alone, crates, .. }
            if alone.is_empty() && crates.contains(&"aterm-gui".to_string())),
        "{locked:?}"
    );
}

/// End to end on a real checkout and graph: a change inside a member's test module, to a file
/// only its tests mount, or to a comment no macro reads, plain or doc, selects that member alone;
/// a change to a shipped line selects its dependent too, and so does a doc comment that moves the
/// code after it, sits on a derived item or in a macro's body; a test-only change to a file
/// another member embeds by a `../` literal keeps that reader in the cone with its own dependents
/// but leaves the member's linker out, and the reader stays whole when its own change is
/// test-only. CONTROL: a file another member names by its repo path, or a change gathered from no
/// one tree, keeps every dependent.
#[test]
fn a_test_only_change_on_a_real_checkout_selects_its_member_alone() {
    let made = crate::mktemp_dir("atv-changed-own-tests").expect("mktemp");
    let root = std::fs::canonicalize(&made).expect("the fixture's own path");
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nresolver='2'\nmembers=['crates/a','crates/b','crates/c']\n",
    )
    .expect("workspace manifest");
    let services = "/// The tester.\npub fn tester() -> &'static str {\n    // The name.\n    \"tester\"\n}\n\n/// A record.\n#[derive(Debug)]\npub struct Record;\n\nmacro_rules! one {\n    () => {\n        // One.\n        1\n    };\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn names() {\n        assert_eq!(super::tester(), \"tester\");\n    }\n}\n";
    let untested = "#[cfg(test)]\nmod tests {}\n";
    let tested = "#[cfg(test)]\nmod tests {\n    fn t() {}\n}\n";
    let reader = "pub const PEEK: &str = include_str!(\"../../a/src/peek.rs\");\npub const TEST_PEEK: &str = include_str!(\"../../a/tests/peek.rs\");\npub const NAMED: &str = \"crates/a/src/named.rs\";\n";
    for (dir, rest, files) in [
        (
            "a",
            "",
            vec![
                (
                    "src/lib.rs",
                    "pub mod services;\n\n#[cfg(test)]\n#[path = \"lib_tests.rs\"]\nmod tests;\n",
                ),
                ("src/services.rs", services),
                ("src/lib_tests.rs", "#[test]\nfn one() {}\n"),
                ("src/peek.rs", untested),
                ("src/named.rs", untested),
                ("tests/integration.rs", "#[test]\nfn one() {}\n"),
                ("tests/peek.rs", "#[test]\nfn one() {}\n"),
            ],
        ),
        (
            "b",
            "[dependencies]\na={path='../a'}\n",
            vec![("src/lib.rs", "pub use a::services::tester;\n")],
        ),
        ("c", "", vec![("src/lib.rs", reader)]),
    ] {
        let package = root.join("crates").join(dir);
        std::fs::create_dir_all(package.join("src")).expect("fixture source dir");
        for (path, text) in files {
            std::fs::create_dir_all(package.join(path).parent().unwrap())
                .expect("fixture target dir");
            std::fs::write(package.join(path), text).expect("fixture source");
        }
        std::fs::write(
            package.join("Cargo.toml"),
            format!("[package]\nname='{dir}'\nversion='0.1.0'\nedition='2021'\n{rest}"),
        )
        .expect("package manifest");
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    targo_tree(
        &root,
        Path::new(env!("CARGO")),
        &path,
        &["tree", "--workspace"],
    )
    .expect("cargo writes the lock");
    let checkout = Committed::new(&root);
    let picked = |selection: Selection| match selection {
        Selection::Narrowed {
            seeds,
            crates,
            alone,
            ..
        } => (seeds, crates, alone),
        Selection::Widened(why) => panic!("widened: {why}"),
    };
    let test_hunk = services.replace("\"tester\");", "\"probe\");");
    let tests = picked(checkout.change(&[("crates/a/src/services.rs", &test_hunk)]));
    let mounted =
        picked(checkout.change(&[("crates/a/src/lib_tests.rs", "#[test]\nfn two() {}\n")]));
    let integration =
        picked(checkout.change(&[("crates/a/tests/integration.rs", "#[test]\nfn two() {}\n")]));
    let integration_reader =
        picked(checkout.change(&[("crates/a/tests/peek.rs", "#[test]\nfn two() {}\n")]));
    let shipped = picked(checkout.change(&[(
        "crates/a/src/services.rs",
        &services.replace("    \"tester\"\n", "    \"probe\"\n"),
    )]));
    let embedded = picked(checkout.change(&[("crates/a/src/peek.rs", tested)]));
    let reader_tests = format!("{reader}{tested}");
    let both = picked(checkout.change(&[
        ("crates/a/src/peek.rs", tested),
        ("crates/c/src/lib.rs", &reader_tests),
    ]));
    let named = picked(checkout.change(&[("crates/a/src/named.rs", tested)]));
    let gathered = picked(checkout.gathered(&[("crates/a/src/services.rs", &test_hunk)]));
    let commented = |from: &str, to: &str| {
        picked(checkout.change(&[("crates/a/src/services.rs", &services.replace(from, to))]))
    };
    let doc = commented("/// The tester.", "/// The one tester.");
    let plain = commented("// The name.", "// The tester's name.");
    let shifted = commented("/// The tester.", "/// The tester.\n/// More.");
    let derived = commented("/// A record.", "/// The record.");
    let in_macro = commented("// One.", "// The one.");
    let _ = std::fs::remove_dir_all(&made);
    let a = paths(&["a"]);
    let ab = paths(&["a", "b"]);
    let ac = paths(&["a", "c"]);
    assert_eq!(tests, (a.clone(), a.clone(), a.clone()));
    assert_eq!(mounted, (a.clone(), a.clone(), a.clone()));
    assert_eq!(integration, (a.clone(), a.clone(), a.clone()));
    assert_eq!(integration_reader, (ac.clone(), ac.clone(), a.clone()));
    assert_eq!(shipped, (a.clone(), ab.clone(), Vec::new()));
    assert_eq!(embedded, (ac.clone(), ac.clone(), a.clone()));
    assert_eq!(both, (ac.clone(), ac, a.clone()));
    assert_eq!(named, (a.clone(), ab.clone(), Vec::new()));
    assert_eq!(doc, (a.clone(), a.clone(), a.clone()));
    assert_eq!(plain, (a.clone(), a.clone(), a.clone()));
    for control in [shifted, derived, in_macro] {
        assert_eq!(control, (a.clone(), ab.clone(), Vec::new()));
    }
    assert_eq!(gathered, (a, ab, Vec::new()));
}

fn narrowed(sel: &Selection) -> (Vec<String>, Vec<String>) {
    match sel {
        Selection::Narrowed { seeds, crates, .. } => (seeds.clone(), crates.clone()),
        Selection::Widened(why) => panic!("widened unexpectedly: {why}"),
    }
}

#[test]
fn a_path_is_owned_by_the_member_whose_manifest_is_nearest() {
    let m = Members::new(
        [
            "aterm-grid",
            "aterm-gui",
            "aterm-render",
            "aterm-scrollback",
            "aterm-lab",
        ]
        .into_iter()
        .map(|name| Member {
            name: name.to_string(),
            dir: format!("crates/{name}"),
            has_lib: true,
        })
        .chain([Member {
            name: "windows-sys".to_string(),
            dir: "crates/aterm-windows-sys-052".to_string(),
            has_lib: true,
        }])
        .collect(),
    );
    let mut mf = manifests();
    mf.insert("");
    mf.insert("crates/aterm-windows-sys-052");
    mf.insert("crates/aterm-windows-sys");
    for (path, want) in [
        ("crates/aterm-grid/src/lib.rs", Some("aterm-grid")),
        ("crates/aterm-grid/tests/deep/nested.rs", Some("aterm-grid")),
        ("crates/aterm-grid/Cargo.toml", Some("aterm-grid")),
        (
            "crates/aterm-scrollback/src/lib.rs",
            Some("aterm-scrollback"),
        ),
        ("crates/aterm-render/src/lib.rs", Some("aterm-render")),
        ("crates/aterm-render/SPECS.md", None),
        ("crates/aterm-render/README.md", Some("aterm-render")),
        ("crates/aterm-render/src/SPECS.md", Some("aterm-render")),
        (
            "crates/aterm-render/tests/font_oracle/recorder/src/main.rs",
            None,
        ),
        ("libc-oracle/src/lib.rs", None),
        ("docs/PROCESS.md", None),
        ("tools/install.sh", None),
        ("README.md", None),
        ("AGENTS.md", None),
        (
            "crates/aterm-windows-sys-052/src/lib.rs",
            Some("windows-sys"),
        ),
        ("crates/aterm-windows-sys/src/lib.rs", None),
    ] {
        assert_eq!(owning_member(path, &mf, &m), want, "{path}");
    }
}

#[test]
fn the_cone_is_the_seeds_and_what_builds_on_them() {
    let m = members(&["aterm-grid", "aterm-gui", "aterm-cli", "aterm-bench"]);
    let pick = |p: &[&str], dependents: &dyn Fn(&str) -> Option<Vec<String>>| {
        narrowed(&select(
            &paths(p),
            &manifests(),
            &m,
            &no_packages,
            dependents,
        ))
    };
    let all = paths(&["aterm-bench", "aterm-cli", "aterm-grid", "aterm-gui"]);
    assert_eq!(
        pick(&["crates/aterm-grid/src/lib.rs"], &graph(EDGES)),
        (paths(&["aterm-grid"]), all.clone())
    );
    assert_eq!(
        pick(
            &[
                "crates/aterm-grid/src/lib.rs",
                "crates/aterm-gui/src/app.rs",
                "crates/aterm-gui/src/other.rs",
            ],
            &graph(EDGES)
        ),
        (paths(&["aterm-grid", "aterm-gui"]), all)
    );
    assert_eq!(
        pick(&["crates/aterm-grid/Cargo.toml"], &graph(EDGES)).0,
        ["aterm-grid"]
    );
    assert_eq!(
        pick(&["crates/aterm-grid/src/lib.rs"], &|_| {
            Some(vec!["aterm-gui".into(), "some-registry-crate".into()])
        })
        .1,
        ["aterm-grid", "aterm-gui"],
        "a changed crate stays in the cone though the answer leaves it out"
    );
    let (seeds, crates) = pick(&["docs/PROCESS.md", "tools/install.sh"], &graph(EDGES));
    assert!(seeds.is_empty() && crates.is_empty());
}

/// The graph the cone closes over, read from a real workspace. A member that names a crate only
/// among its dev-dependencies (as aterm-models names every anchored crate, for its source-spec
/// closure test) reaches it, and so does one that dev-depends on what links it normally. A dev
/// edge is the final hop: nothing that depends on that member links the crate, so `downstream`
/// and `tested_closure` stay out. A package only a dev edge reaches still reaches its dev
/// dependent (`astream-term` and `aterm-conformance`), and a normal edge that only a
/// dev-dependency's feature turns on still counts (`aterm-wasm-abi` on its macros). The whole
/// fixture asks cargo for colour by its config, which the read must not obey. A package only
/// another target's graph holds reads as reaching nothing here, so a change to its files widens,
/// where a change to a member's narrows.
#[test]
fn a_dev_edge_is_the_final_hop_of_the_cone() {
    let made = crate::mktemp_dir("atv-changed-dev-edge").expect("mktemp");
    let root = std::fs::canonicalize(&made).expect("the fixture's own path");
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nresolver='2'\nexclude=['vendored','offhost']\nmembers=['anchored','closure',\
         'downstream','tested_closure','linked','tested_linked','conformance','macros','abi',\
         'consumer','other']\n",
    )
    .expect("workspace manifest");
    std::fs::create_dir_all(root.join(".cargo")).expect("config dir");
    std::fs::write(root.join(".cargo/config.toml"), "[term]\ncolor='always'\n").expect("config");
    for (name, deps) in [
        ("anchored", ""),
        (
            "closure",
            "[dev-dependencies]\nanchored={path='../anchored'}\n",
        ),
        (
            "downstream",
            "[dependencies]\nclosure={path='../closure'}\n",
        ),
        (
            "tested_closure",
            "[dev-dependencies]\nclosure={path='../closure'}\n",
        ),
        ("linked", "[dependencies]\nanchored={path='../anchored'}\n"),
        (
            "tested_linked",
            "[dev-dependencies]\nlinked={path='../linked'}\n",
        ),
        ("vendored", ""),
        (
            "conformance",
            "[dev-dependencies]\nvendored={path='../vendored'}\n",
        ),
        ("macros", ""),
        (
            "abi",
            "[dependencies]\nmacros={path='../macros',optional=true}\n\
             [features]\nexport=['dep:macros']\n\
             [dev-dependencies]\nabi={path='.',features=['export']}\n",
        ),
        ("consumer", "[dependencies]\nabi={path='../abi'}\n"),
        ("offhost", ""),
        (
            "other",
            "[target.'cfg(target_os=\"hermit\")'.dependencies]\noffhost={path='../offhost'}\n",
        ),
    ] {
        let package = root.join(name);
        std::fs::create_dir_all(package.join("src")).expect("fixture source dir");
        std::fs::write(package.join("src/lib.rs"), "").expect("empty library");
        std::fs::write(
            package.join("Cargo.toml"),
            format!("[package]\nname='{name}'\nversion='1.0.0'\nedition='2021'\n{deps}"),
        )
        .expect("package manifest");
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    let m = Members::new(
        [
            "anchored",
            "closure",
            "downstream",
            "tested_closure",
            "linked",
            "tested_linked",
            "conformance",
            "macros",
            "abi",
            "consumer",
            "other",
        ]
        .map(|name| Member {
            name: name.to_string(),
            dir: name.to_string(),
            has_lib: true,
        })
        .to_vec(),
    );
    let found = |of: &str| {
        invert(&root, Path::new(env!("CARGO")), &path, of, &m).expect("the inverted graph reads")
    };
    let owned = |names: &[&str]| -> Vec<String> {
        names.iter().map(|n| (*n).to_string()).collect::<Vec<_>>()
    };
    let reach = |links: &[&str], dev_only: &[&str], direct: &[&str]| Reach {
        links: owned(links),
        dev_only: owned(dev_only),
        direct: owned(direct),
    };
    let (anchored, vendored, macros) = (found("anchored"), found("vendored"), found("macros"));
    let offhost = found("offhost");
    let unknown = invert(
        &root,
        Path::new(env!("CARGO")),
        &path,
        "no-such-package",
        &m,
    );
    let checkout = Committed::new(&root);
    let after_change_to = |file: &str| checkout.change(&[(file, "pub fn changed() {}\n")]);
    let member_file = after_change_to("anchored/src/lib.rs");
    let offhost_file = after_change_to("offhost/src/lib.rs");
    let _ = std::fs::remove_dir_all(&made);
    assert_eq!(
        anchored,
        reach(
            &["anchored", "linked"],
            &["closure", "tested_linked"],
            &["closure", "linked"]
        ),
        "a dev dependent is direct; what depends on a direct dependent is not"
    );
    assert_eq!(vendored, reach(&[], &["conformance"], &["conformance"]));
    assert_eq!(macros, reach(&["abi", "consumer", "macros"], &[], &["abi"]));
    assert_eq!(
        offhost,
        Reach::default(),
        "this host's graph does not hold it"
    );
    assert_eq!(unknown, None, "a spec the graph cannot answer widens");
    assert!(
        matches!(&member_file, Selection::Narrowed { seeds, .. } if seeds == &["anchored"]),
        "{member_file:?}"
    );
    assert!(
        matches!(&offhost_file, Selection::Widened(why) if why.contains("offhost")),
        "{offhost_file:?}"
    );
}

/// A fixture checkout, whose lock cargo has written, committed as the branch `base`, on which a
/// change is resolved with this test's cargo.
struct Committed<'a> {
    root: &'a Path,
    path: std::ffi::OsString,
    tools: Toolchain,
}

/// A workspace-wide config obligation from an octopus already selects every crate.
/// NEGATIVE CONTROL: without that path the unreadable shared source must still be
/// visited and reported; ordinary selections cannot skip shared-input discovery.
#[test]
fn an_octopus_config_obligation_needs_no_shared_input_cross_product() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let physical = dir.path().canonicalize().unwrap();
    let root = physical.as_path();
    std::fs::create_dir_all(root.join("a/src")).unwrap();
    std::fs::create_dir(root.join(".cargo")).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nresolver='2'\nmembers=['a']\n",
    )
    .unwrap();
    std::fs::write(
        root.join("a/Cargo.toml"),
        "[package]\nname='a'\nversion='1.0.0'\nedition='2024'\n",
    )
    .unwrap();
    std::fs::write(
        root.join("Cargo.lock"),
        "version = 4\n\n[[package]]\nname = 'a'\nversion = '1.0.0'\n",
    )
    .unwrap();
    std::fs::write(root.join("a/src/lib.rs"), "pub fn value() {}\n").unwrap();
    std::fs::write(root.join("a/src/unreadable.rs"), [0xff]).unwrap();
    let fixture = Committed::new(root);
    let read = |args: &[&str]| {
        let out = git(root, &fixture.path)
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}: {out:?}");
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    };
    let base = read(&["rev-parse", "HEAD"]);
    let tree = read(&["rev-parse", "HEAD^{tree}"]);
    let a = read(&["commit-tree", &tree, "-p", &base, "-m", "a"]);
    let b = read(&["commit-tree", &tree, "-p", &base, "-m", "b"]);
    std::fs::write(
        root.join(".cargo/config.toml"),
        "# compilation configuration\n",
    )
    .unwrap();
    fixture.git(&["add", "."]);
    let tree = read(&["write-tree"]);
    let tip = read(&[
        "commit-tree",
        &tree,
        "-p",
        &base,
        "-p",
        &a,
        "-p",
        &b,
        "-m",
        "archive",
    ]);
    let input = crate::release_input::discover(root, &fixture.path, &base, &tip).unwrap();
    assert_eq!(input.paths, [".cargo/config.toml"]);
    let pick = |paths| resolve_paths(root, &fixture.tools, &fixture.path, &base, paths, None);
    let selected = pick(input.paths);
    assert!(
        matches!(selected, Selection::Widened(ref why) if why.starts_with(".cargo/config.toml changed:")),
        "{selected:?}"
    );
    let control = pick(vec!["a/src/lib.rs".into()]);
    assert!(
        matches!(control, Selection::Widened(ref why) if why.contains("cannot read shared input")),
        "{control:?}"
    );
}

impl<'a> Committed<'a> {
    fn new(root: &'a Path) -> Self {
        let mut tools = Toolchain::discover(
            Some(Path::new("/nonexistent")),
            Path::new("/nonexistent"),
            OsStr::new(""),
            None,
            None,
        );
        tools.targo = env!("CARGO").into();
        tools.refused = None;
        let committed = Self {
            root,
            path: std::env::var_os("PATH").unwrap_or_default(),
            tools,
        };
        committed.git(&["init", "-q"]);
        committed.git(&["add", "-A"]);
        committed.git(&["commit", "-qm", "base"]);
        committed.git(&["branch", "base"]);
        committed
    }

    fn git(&self, args: &[&str]) {
        let out = git(self.root, &self.path)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .output()
            .expect("git runs");
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }

    /// What [`resolve`] selects with each repo-relative file of `files` holding its text; the
    /// files are restored after.
    fn change(&self, files: &[(&str, &str)]) -> Selection {
        self.written(files, || {
            resolve(self.root, &self.tools, &self.path, "base")
        })
    }

    /// What [`resolve_paths`] selects for the same change gathered from no one tree, as
    /// `release_input` gathers its unreceipted commits'.
    fn gathered(&self, files: &[(&str, &str)]) -> Selection {
        let paths = files.iter().map(|(file, _)| (*file).to_string()).collect();
        self.written(files, || {
            resolve_paths(self.root, &self.tools, &self.path, "base", paths, None)
        })
    }

    fn written(&self, files: &[(&str, &str)], select: impl FnOnce() -> Selection) -> Selection {
        for (file, text) in files {
            std::fs::write(self.root.join(file), text).expect("the change");
        }
        let selection = select();
        self.git(&["checkout", "-q", "--", "."]);
        selection
    }
}

/// A package outside the workspace can have a member's name, as `crates/aterm-windows-sys` has
/// `crates/aterm-windows-sys-052`'s, so a directory says which is which. A change to the one
/// outside is a seed of its own, named with its version since its bare name is the member's: it
/// widens where only another target's graph holds it, and where this host's does, it selects
/// what depends on it and not the member. CONTROL: a change to the member narrows to it and what
/// builds on it, and a version the read cannot follow widens.
#[test]
fn a_same_named_package_outside_the_workspace_is_no_member() {
    let made = crate::mktemp_dir("atv-changed-same-name").expect("mktemp");
    let root = std::fs::canonicalize(&made).expect("the fixture's own path");
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nresolver='2'\nexclude=['crates/shared','crates/held']\n\
         members=['crates/shared-052','crates/user','crates/far','crates/held-052',\
         'crates/near']\n",
    )
    .expect("workspace manifest");
    for (dir, name, version, rest) in [
        ("shared-052", "shared", "0.52.0", ""),
        ("shared", "shared", "0.61.2", "[workspace]\n"),
        (
            "user",
            "user",
            "1.0.0",
            "[dependencies]\nshared={path='../shared-052'}\n",
        ),
        (
            "far",
            "far",
            "1.0.0",
            "[target.'cfg(target_os=\"hermit\")'.dependencies]\n\
             shared={path='../shared'}\n",
        ),
        ("held-052", "held", "0.52.0", ""),
        ("held", "held", "0.61.2", "[workspace]\n"),
        (
            "near",
            "near",
            "1.0.0",
            "[dependencies]\nheld={path='../held'}\n",
        ),
    ] {
        let package = root.join("crates").join(dir);
        std::fs::create_dir_all(package.join("src")).expect("fixture source dir");
        std::fs::write(package.join("src/lib.rs"), "").expect("empty library");
        std::fs::write(
            package.join("Cargo.toml"),
            format!("[package]\nname='{name}'\nversion='{version}'\nedition='2021'\n{rest}"),
        )
        .expect("package manifest");
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    targo_tree(
        &root,
        Path::new(env!("CARGO")),
        &path,
        &["tree", "--workspace"],
    )
    .expect("cargo writes the lock");
    let checkout = Committed::new(&root);
    let after_change_to = |file: &str| checkout.change(&[(file, "pub fn changed() {}\n")]);
    let member = after_change_to("crates/shared-052/src/lib.rs");
    let off_host = after_change_to("crates/shared/src/lib.rs");
    let on_host = after_change_to("crates/held/src/lib.rs");
    let _ = std::fs::remove_dir_all(&made);
    assert_eq!(
        narrowed(&member),
        (paths(&["shared"]), paths(&["shared@0.52.0", "user"]))
    );
    assert!(
        matches!(&off_host, Selection::Widened(why) if why.contains("shared@0.61.2")),
        "{off_host:?}"
    );
    assert_eq!(
        narrowed(&on_host),
        (paths(&["held@0.61.2"]), paths(&["near"]))
    );
    let m = Members::new(vec![Member {
        name: "shared".into(),
        dir: "crates/shared-052".into(),
        has_lib: true,
    }]);
    assert_eq!(
        outside_packages(
            "shared v0.52.0 (/r/crates/shared-052)\nshared v0.61.2 (/r/crates/shared)\n",
            "/r/",
            &m,
        ),
        Some(vec![(
            "shared@0.61.2".to_string(),
            "crates/shared".to_string()
        )])
    );
    assert_eq!(
        outside_packages("shared 0.61.2 (/r/crates/shared)\n", "/r/", &m),
        None
    );
}

/// The tree's prefixes say what depends on what, so a line that reads as neither a section
/// header nor a package one level under what it follows fails the whole read, which widens:
/// parsing past it would narrow the cone to whatever did parse. An ANSI-coloured tree (these
/// lines are cargo's under `CARGO_TERM_COLOR=always`) would otherwise read as one root per line.
/// An empty printout is cargo's `nothing to print`, which [`select`] widens on instead. CONTROL:
/// the same tree in plain text reads.
#[test]
fn a_tree_read_it_cannot_follow_fails_rather_than_narrowing() {
    let m = Members::new(
        ["x", "a", "b", "c", "d", "e"]
            .map(|name| Member {
                name: name.to_string(),
                dir: name.to_string(),
                has_lib: true,
            })
            .to_vec(),
    );
    let read = |tree: &str| reached(tree, "/r/", &m);
    let plain = "x v1.0.0 (/r/x)\n\
                 |-- a v1.0.0 (/r/a)\n\
                 |   [dev-dependencies]\n\
                 |   `-- b v1.0.0 (/r/b)\n\
                 `-- c v1.0.0 (proc-macro) (/r/c) (*)\n\
                 [dev-dependencies]\n\
                 `-- d v1.0.0 (/r/d)\n\
                 \x20   `-- e v1.0.0 (/r/e)\n\
                 \n\
                 registry v2.0.0\n";
    assert_eq!(
        read(plain),
        Some(Reach {
            links: ["a", "c", "x"].map(String::from).to_vec(),
            dev_only: ["b", "d"].map(String::from).to_vec(),
            direct: ["a", "c", "d"].map(String::from).to_vec(),
        })
    );
    let coloured = "x v1.0.0 (/r/x)\n\
                    \x1b[2m|--\x1b[0m a v1.0.0 (/r/a)\n\
                    \x1b[2m|\x1b[0m   \x1b[96m\x1b[1m[dev-dependencies]\x1b[39m\x1b[22m\n\
                    \x1b[2m|\x1b[0m   \x1b[1m\x1b[36m`--\x1b[0m b v1.0.0 (/r/b)\n";
    assert_eq!(read(coloured), None);
    assert_eq!(
        read("x v1.0.0 (/r/x)\n\x1b[2m|--\x1b[0m a v1.0.0 (/r/a)\n"),
        None,
        "a prefix alone"
    );
    for misread in [
        "x v1.0.0 (/r/x)\nwarning: not a package\n",
        "x v1.0.0 (/r/x)\n|--\n",
        "x v1.0.0 (/r/x)\n|       `-- deep v1.0.0 (/r/d)\n",
        "x v1.0.0 (/r/x)\n  a v1.0.0 (/r/a)\n",
        "x v1.0.0 (/r/x)\n[unknown-dependencies]\n",
        "[dev-dependencies]\n",
        "|   [dev-dependencies]\n",
    ] {
        assert_eq!(read(misread), None, "{misread:?}");
    }
    assert_eq!(read(""), Some(Reach::default()));
    assert_eq!(read("\n"), Some(Reach::default()));
}

/// A seed whose answer holds no member says this host's graph does not hold it, so the seed is
/// not read as selecting nothing. A lock row off this host's graph is the real case.
#[test]
fn a_seed_this_hosts_graph_does_not_hold_widens() {
    let m = members(&["aterm-grid", "aterm-gui"]);
    let row = Replan::Packages(BTreeSet::from(["offhost".to_string()]));
    let pick = |dependents: &dyn Fn(&str) -> Option<Vec<String>>| {
        super::select(
            &paths(&["Cargo.lock"]),
            &manifests(),
            &m,
            &no_packages,
            &|name| dependents(name).map(linking),
            &|_| row.clone(),
            &|_, _| false,
        )
    };
    for answer in [vec![], vec!["offhost".to_string()]] {
        let sel = pick(&|_| Some(answer.clone()));
        assert!(
            matches!(&sel, Selection::Widened(why) if why.contains("offhost")),
            "{answer:?}: {sel:?}"
        );
        assert!(cone("main", &sel).0.is_workspace());
    }
    assert_eq!(
        narrowed(&pick(&|_| Some(vec!["aterm-gui".to_string()]))),
        (vec!["offhost".to_string()], vec!["aterm-gui".to_string()]),
        "one that reaches a member narrows"
    );
}

/// clippy reads the nearest config above a manifest, so a root one is every other member's
/// lint, and no member owns it: it widens, through the replan a checkout answers. CONTROL: a
/// member's own config is that member's, and a rustfmt config selects nothing, because no
/// compile reads it and `fmt` takes the whole tree whatever the selection.
#[test]
fn a_root_lint_config_widens_and_a_members_own_selects_that_member() {
    let m = members(&["aterm-grid", "aterm-link"]);
    let pick = |changed: &str| {
        super::select(
            &paths(&[changed]),
            &manifests(),
            &m,
            &no_packages,
            &|name| graph(EDGES)(name).map(linking),
            &|p| {
                replan_of(
                    Path::new("/no-such-checkout"),
                    OsStr::new(""),
                    "HEAD",
                    p,
                    &m,
                )
            },
            &|_, _| false,
        )
    };
    for config in ["clippy.toml", ".clippy.toml", "rust-toolchain"] {
        assert_eq!(
            pick(config),
            Selection::Widened(format!(
                "{config} changed: it can change how every crate is built or linted"
            ))
        );
    }
    assert_eq!(
        narrowed(&pick("crates/aterm-grid/clippy.toml")).0,
        ["aterm-grid"]
    );
    for formatting in ["rustfmt.toml", ".rustfmt.toml"] {
        let (seeds, crates) = narrowed(&pick(formatting));
        assert!(seeds.is_empty() && crates.is_empty(), "{formatting}");
    }
    assert!(!crate::checks::Check::Fmt.reads(&Scope::changed("main", Vec::new(), false)));
}

/// A lock move compiles every member with another compiler: a change to the root
/// `trust.lock` replans everything, as `rust-toolchain.toml` does, and a file of that name
/// below the root, which verify's discovery (from the root) never reads, does not.
#[test]
fn a_trust_lock_change_replans_everything() {
    assert!(replans_everything("trust.lock"));
    assert!(!replans_everything("crates/x/trust.lock"));
}

#[test]
fn whatever_the_selection_cannot_answer_widens() {
    let m = members(&["aterm-grid", "aterm-link"]);
    let lib = "crates/aterm-grid/src/lib.rs";
    let fork = || {
        Some(vec![(
            "astream-wire".to_string(),
            "vendor/astream/crates/astream-wire".to_string(),
        )])
    };
    let mut cases: Vec<(String, Selection)> = REPLANNING_PATHS
        .iter()
        .map(|f| {
            (
                format!("{f} changed"),
                select(
                    &paths(&[lib, f]),
                    &manifests(),
                    &m,
                    &no_packages,
                    &graph(EDGES),
                ),
            )
        })
        .collect();
    cases.push((
        "no members".into(),
        select(
            &paths(&[lib]),
            &manifests(),
            &Members::default(),
            &no_packages,
            &graph(EDGES),
        ),
    ));
    cases.push((
        "an unreadable inverted graph".into(),
        select(&paths(&[lib]), &manifests(), &m, &no_packages, &|_| None),
    ));
    cases.push((
        "unreadable path packages".into(),
        select(
            &paths(&["vendor/astream"]),
            &manifests(),
            &m,
            &|| None,
            &graph(EDGES),
        ),
    ));
    cases.push((
        "a touched package's dependents unread".into(),
        select(&paths(&["vendor/astream"]), &manifests(), &m, &fork, &|c| {
            (c != "astream-wire").then(Vec::new)
        }),
    ));
    let no_targo = Toolchain::discover(
        Some(Path::new("/nonexistent")),
        Path::new("/nonexistent"),
        OsStr::new(""),
        None,
        None,
    );
    cases.push((
        "no targo".into(),
        resolve(Path::new("/"), &no_targo, OsStr::new(""), "main"),
    ));
    for (case, sel) in cases {
        assert!(matches!(sel, Selection::Widened(_)), "{case}: {sel:?}");
        assert!(cone("main", &sel).0.is_workspace(), "{case}");
    }
}

#[test]
fn a_file_a_members_tests_exercise_puts_that_member_in_the_cone() {
    let m = members(&["aterm-update", "aterm-gui"]);
    let edges: &'static [(&str, &str)] = &[("aterm-update", "aterm-gui")];
    let sel = select(
        &paths(&["tools/install.sh"]),
        &manifests(),
        &m,
        &no_packages,
        &graph(edges),
    );
    assert_eq!(narrowed(&sel), (vec![], vec!["aterm-update".to_string()]));
    let m = members(&["aterm-update", "aterm-bootstrap-verify", "aterm-gui"]);
    let sel = select(
        &paths(&["tools/install.sh"]),
        &manifests(),
        &m,
        &no_packages,
        &graph(edges),
    );
    assert_eq!(
        narrowed(&sel),
        (
            vec![],
            vec![
                "aterm-bootstrap-verify".to_string(),
                "aterm-update".to_string()
            ]
        ),
        "both of install.sh's suites"
    );
    let m = members(&["aterm-release"]);
    for file in [
        "publish/manifest.txt",
        "publish/pre-promote",
        "publish/transforms.sh",
    ] {
        let sel = select(&paths(&[file]), &manifests(), &m, &no_packages, &graph(&[]));
        assert_eq!(
            narrowed(&sel),
            (vec![], vec!["aterm-release".to_string()]),
            "{file}"
        );
    }
    assert_eq!(
        tested_by("publishing/x").next(),
        None,
        "a sibling sharing the prefix"
    );
    assert_eq!(
        tested_by("tools/install.sh").collect::<Vec<_>>(),
        ["aterm-update", "aterm-bootstrap-verify"]
    );
    let m = members(&["aterm-update", "aterm-gui"]);
    let sel = select(
        &paths(&["docs/PERF-REGRESSION-DEFENCE.md"]),
        &manifests(),
        &m,
        &no_packages,
        &graph(edges),
    );
    assert_eq!(narrowed(&sel), (vec![], vec![]), "a file no test claims");
}

#[test]
fn a_vendored_package_or_its_gitlink_selects_the_members_that_depend_on_it() {
    const VENDORED: &[(&str, &str)] = &[
        ("astream-cap", "astream-broker"),
        ("astream-broker", "aterm-link"),
        ("astream-cap", "aterm-link"),
        ("aterm-link", "aterm"),
        ("indexmap", "aterm-gui"),
    ];
    let m = members(&["aterm", "aterm-grid", "aterm-gui", "aterm-link"]);
    let packages = || {
        Some(
            [
                ("astream-broker", "vendor/astream/crates/astream-broker"),
                ("astream-cap", "vendor/astream/crates/astream-cap"),
                ("indexmap", "vendor/indexmap"),
            ]
            .map(|(n, d)| (n.to_string(), d.to_string()))
            .to_vec(),
        )
    };
    let pick = |p: &[&str]| select(&paths(p), &manifests(), &m, &packages, &graph(VENDORED));

    let bump = pick(&["vendor/astream"]);
    assert_eq!(
        narrowed(&bump),
        (
            paths(&["astream-broker", "astream-cap"]),
            paths(&["aterm", "aterm-link"])
        )
    );
    assert_eq!(
        cone("origin/main", &bump).0.crates(),
        ["aterm", "aterm-link"],
        "a vendored seed is no crate of the cone"
    );
    assert!(
        cone("origin/main", &bump).0.touches("astream-cap"),
        "and is still what the change touched"
    );
    assert!(!cone("origin/main", &bump).0.touches("aterm-link"));
    assert_eq!(
        narrowed(&pick(&["vendor/indexmap/src/map.rs"])).1,
        ["aterm-gui"]
    );
    let (seeds, crates) = narrowed(&pick(&[
        "vendor/README-astream.md",
        "vendor/forge.toml",
        "vendor/astream-other/src/lib.rs",
    ]));
    assert!(seeds.is_empty() && crates.is_empty());
}

#[test]
fn shared_source_and_asset_consumers_join_the_change_cone_transitively() {
    let repo = crate::mktemp_dir("atv-shared-inputs").expect("mktemp");
    let path_env = std::env::var_os("PATH").unwrap_or_default();
    let write = |path: &str, text: &str| {
        let file = repo.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    };
    write("crates/a/src/shared.rs", "pub const VALUE: u8 = 1;");
    write("crates/a/assets/font.bin", "font");
    write(
        "crates/b/src/lib.rs",
        "#[path = \"../../a/src/shared.rs\"] mod shared;",
    );
    write("crates/c/src/lib.rs", "include!(\"../../b/src/lib.rs\");");
    write(
        "crates/d/src/lib.rs",
        "const FONT: &[u8] = include_bytes!(r\"../../a/assets/font.bin\");",
    );
    write(
        "crates/e/src/lib.rs",
        "const QUOTE: &str = \"\\\"\"; include_str!(\"../../a/assets/font.bin\");",
    );
    write(
        "crates/f/tests/pin.rs",
        "include_bytes!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/../a/assets/font.bin\"));",
    );
    write(
        "crates/g/tests/pin.rs",
        "format!(\"{}/../a/assets/font.bin\", env!(\"CARGO_MANIFEST_DIR\"));",
    );
    let status = git(&repo, &path_env).args(["init", "-q"]).status().unwrap();
    assert!(status.success());
    let m = Members::new(
        ["a", "b", "c", "d", "e", "f", "g"]
            .into_iter()
            .map(|name| Member {
                name: name.into(),
                dir: format!("crates/{name}"),
                has_lib: true,
            })
            .collect(),
    );
    let listed = listed_files(&repo, &path_env).expect("the fixture lists");
    let source = with_shared_inputs(&repo, &listed, paths(&["crates/a/src/shared.rs"]), &m)
        .unwrap()
        .paths;
    assert_eq!(
        source,
        paths(&[
            "crates/a/src/shared.rs",
            "crates/b/src/lib.rs",
            "crates/c/src/lib.rs"
        ])
    );
    let asset = with_shared_inputs(&repo, &listed, paths(&["crates/a/assets/font.bin"]), &m)
        .unwrap()
        .paths;
    assert_eq!(
        asset,
        paths(&[
            "crates/a/assets/font.bin",
            "crates/d/src/lib.rs",
            "crates/e/src/lib.rs",
            "crates/f/tests/pin.rs",
            "crates/g/tests/pin.rs"
        ])
    );
    let docs = paths(&["docs/PROCESS.md"]);
    assert_eq!(
        with_shared_inputs(&repo, &listed, docs.clone(), &m)
            .unwrap()
            .paths,
        docs
    );
    std::fs::remove_dir_all(repo).unwrap();
}

#[test]
fn a_directory_literal_seeds_its_consumer() {
    let root = Path::new("/repo");
    let member = root.join("crates/aterm-theme-pack");
    let changed = BTreeSet::from([root.join("contrib/themes/pack/Iceberg Dark.conf")]);
    let build = member.join("build.rs");
    let test = member.join("src/tests.rs");
    let folder = "let pack = dir.join(\"../../contrib/themes/pack/\");";
    assert!(shares_changed_input(&build, folder, &member, &changed));
    assert!(shares_changed_input(&test, folder, &member, &changed));
    let path = "let pack = dir.join(\"../../contrib/themes/pack\");";
    assert!(!shares_changed_input(&build, path, &member, &changed));
    assert!(!shares_changed_input(&test, path, &member, &changed));
    let elsewhere = BTreeSet::from([root.join("contrib/fonts/x.ttf")]);
    assert!(!shares_changed_input(&build, folder, &member, &elsewhere));
}

/// A literal that continues the manifest directory, by `concat!` or a format argument, names
/// what the same path spelled relatively names, and starts at the member and never at the
/// source's folder. It names a folder as any literal does, by a trailing `/`. CONTROL: another
/// file, a path that stays in the member, and a `{}` that does not lead a path segment name
/// nothing.
#[test]
fn a_literal_that_continues_the_manifest_dir_starts_at_the_member() {
    let root = Path::new("/repo");
    let member = root.join("crates/a");
    let test = member.join("tests/pin.rs");
    let lib = member.join("src/lib.rs");
    let changed = BTreeSet::from([root.join("crates/b/x")]);
    for text in [
        "include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/../b/x\"));",
        "format!(\"{}/../b/x\", env!(\"CARGO_MANIFEST_DIR\"));",
        "format!(\"{dir}/../b/x\");",
        "format!(\"{dir:?}/../b/x\");",
        "concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/../b/\", \"x\");",
        "format!(\"{}/../b/\", dir);",
    ] {
        for source in [&test, &lib] {
            assert!(
                shares_changed_input(source, text, &member, &changed),
                "{text} in {}",
                source.display()
            );
        }
    }

    let continued = "concat!(dir, \"/../../b/x\");";
    let relative = "include_str!(\"../../b/x\");";
    let above = BTreeSet::from([root.join("b/x")]);
    assert!(shares_changed_input(&lib, relative, &member, &changed));
    assert!(shares_changed_input(&lib, relative, &member, &above));
    assert!(!shares_changed_input(&lib, continued, &member, &changed));
    assert!(shares_changed_input(&lib, continued, &member, &above));

    let under_a_folder = BTreeSet::from([root.join("crates/b/steps/one.jsonl")]);
    let folder = "concat!(dir, \"/../b/steps/\");";
    let no_slash = "concat!(dir, \"/../b/steps\");";
    assert!(shares_changed_input(&lib, folder, &member, &under_a_folder));
    assert!(!shares_changed_input(
        &lib,
        no_slash,
        &member,
        &under_a_folder
    ));

    for text in [
        "concat!(dir, \"/../c/x\");",
        "concat!(dir, \"/x/../b\");",
        "format!(\"{}/x/../b\", dir);",
        "format!(\"{}../b/x\", dir);",
        "format!(\"{{}}/../b/x\");",
    ] {
        assert!(
            !shares_changed_input(&test, text, &member, &changed),
            "{text}"
        );
    }
}

#[test]
fn a_bare_parent_literal_seeds_nothing() {
    let root = Path::new("/repo");
    let member = root.join("crates/x");
    let lib = member.join("src/lib.rs");
    let changed = BTreeSet::from([root.join("crates/y/src/z.rs")]);
    for text in [
        "if s.starts_with(\"../\") {}",
        "let up = \"../../\";",
        "let crates = \"../\";",
        "let here = \"./../\";",
        "concat!(dir, \"/../\");",
        "concat!(dir, \"/../../\");",
        "format!(\"{}/../\", dir);",
        "format!(\"{dir}/../../\");",
    ] {
        assert!(
            !shares_changed_input(&lib, text, &member, &changed),
            "{text}"
        );
    }
    let named = "let y = dir.join(\"../y/\");";
    assert!(shares_changed_input(&lib, named, &member, &changed));
}

/// The trie finds exactly the paths `text.contains` finds: nested and overlapping paths, a
/// path repeated in the list, one that only a later start spells, prefixes that never
/// complete, and text around multi-byte characters.
///
/// NEGATIVE CONTROL: walk from the first byte only, or report only the deepest path of a
/// walk, and `b/c` or `a` goes missing from what this asserts.
#[test]
fn the_path_trie_finds_what_contains_finds() {
    let paths = [
        "a",
        "a/b",
        "a/b/c.rs",
        "b/c",
        "a/b",
        "crates/x/src/lib.rs",
        "crates/x/src/lib.rs.bak",
        "zz/never",
        "é/ü.rs",
    ];
    let trie = PathTrie::new(paths);
    for text in [
        "",
        "a",
        "xa/b/c.rs",
        "include!(\"../../crates/x/src/lib.rs\");",
        "crates/x/src/lib.r",
        "zz/neve and b/c",
        "«é/ü.rs»",
        "aaaa/b/c",
    ] {
        let want: BTreeSet<usize> = paths
            .iter()
            .enumerate()
            .filter(|(_, p)| text.contains(*p))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(trie.contained(text), want, "{text:?}");
    }
}

/// The set lookups answer what the scan over every changed path answered: a changed path at
/// or above the target, or, for a folder literal, one under it; a sibling sharing a name
/// prefix (`steps2`, `steps.txt`) is neither.
#[test]
fn changed_set_lookups_match_the_scan_they_replace() {
    let root = Path::new("/repo");
    let member = root.join("crates/a");
    let lib = member.join("src/lib.rs");
    let scan = |text: &str, changed: &[std::path::PathBuf]| {
        let parent = lib.parent().unwrap();
        text.split('"').any(|literal| {
            if !literal.contains("../") {
                return false;
            }
            let (path, start) = match continued_path(literal) {
                Some(path) => (path, member.as_path()),
                None => (literal, parent),
            };
            let folder = path.ends_with('/')
                && path
                    .split('/')
                    .any(|segment| !segment.is_empty() && segment != "." && segment != "..");
            [start, member.as_path()].into_iter().any(|base| {
                let mut target = std::path::PathBuf::new();
                for component in base.join(path).components() {
                    match component {
                        std::path::Component::ParentDir => {
                            target.pop();
                        }
                        std::path::Component::CurDir => {}
                        component => target.push(component.as_os_str()),
                    }
                }
                let folder = folder && !member.starts_with(&target);
                !target.starts_with(&member)
                    && changed
                        .iter()
                        .any(|p| target.starts_with(p) || (folder && p.starts_with(&target)))
            })
        })
    };
    let sets: [&[&str]; 7] = [
        &["crates/b/steps/one.jsonl"],
        &["crates/b/steps2/one.jsonl"],
        &["crates/b/steps.txt"],
        &["crates/b/steps.txt", "crates/b/steps/one.jsonl"],
        &["crates/b"],
        &["crates/b/steps"],
        &["crates/c/x", "crates/b/x"],
    ];
    let texts = [
        "concat!(dir, \"/../b/steps/\");",
        "concat!(dir, \"/../b/steps\");",
        "include_str!(\"../../b/steps/one.jsonl\");",
        "include_str!(\"../../b/x\");",
        "concat!(dir, \"/../b/\");",
        "let up = \"../../\";",
    ];
    for set in sets {
        let list: Vec<_> = set.iter().map(|p| root.join(p)).collect();
        let changed: BTreeSet<_> = list.iter().cloned().collect();
        for text in texts {
            assert_eq!(
                shares_changed_input(&lib, text, &member, &changed),
                scan(text, &list),
                "{text} with {set:?}"
            );
        }
    }
}

#[test]
fn a_member_whose_name_the_lock_shares_is_named_with_its_version() {
    let lock = "version = 4\n\n[[package]]\nname = \"aterm-gui\"\nversion = \"0.1.0\"\n\n\
                [[package]]\nname = \"rustix\"\nversion = \"0.38.44\"\n\n\
                [[package]]\nname = \"rustix\"\nversion = \"1.1.4\"\n\
                source = \"registry+https://github.com/rust-lang/crates.io-index\"\n\n\
                [[patch.unused]]\nname = \"aterm-gui\"\nversion = \"0.0.1\"\n";
    let shared = lock_shared_names(lock);
    assert_eq!(shared, BTreeSet::from(["rustix".to_string()]));
    let listing = "aterm-gui v0.1.0 (/repo/crates/aterm-gui)\n\
                 rustix v0.38.44 (/repo/crates/aterm-rustix-compat)\n\
                 rustix v1.1.4\n";
    let versions = parse_graph_versions(listing, "/repo/");
    assert_eq!(versions.len(), 2);
    let specs = versions
        .into_iter()
        .filter(|(n, _)| shared.contains(n))
        .map(|(n, v)| (n.clone(), format!("{n}@{v}")))
        .collect();
    let m = members(&["aterm-gui", "rustix"]).with_specs(specs);
    assert_eq!(m.spec("rustix"), "rustix@0.38.44");
    assert_eq!(m.spec("aterm-gui"), "aterm-gui");
    assert_eq!(m.specs(), ["aterm-gui", "rustix@0.38.44"]);
    let edges: &'static [(&str, &str)] = &[("rustix", "aterm-gui")];
    let sel = select(
        &paths(&["crates/rustix/src/lib.rs"]),
        &["crates/rustix"].into_iter().collect(),
        &m,
        &no_packages,
        &graph(edges),
    );
    let (seeds, crates) = narrowed(&sel);
    assert_eq!(seeds, ["rustix"]);
    assert_eq!(crates, ["aterm-gui", "rustix@0.38.44"]);
}

#[test]
fn the_cone_hands_the_checks_its_crates_or_nothing() {
    let m = members(&["aterm-grid", "aterm-gui", "aterm-cli", "aterm-bench"]);
    let sel = |p: &[&str]| select(&paths(p), &manifests(), &m, &no_packages, &graph(EDGES));
    let (scope, _) = cone("origin/main", &sel(&["crates/aterm-grid/src/lib.rs"]));
    assert_eq!(
        scope.crates(),
        ["aterm-bench", "aterm-cli", "aterm-grid", "aterm-gui"]
    );
    assert!(
        cone("origin/main", &sel(&["docs/x.md"]))
            .0
            .selects_nothing()
    );
}

#[test]
fn the_lib_question_is_answered_from_the_manifest_or_yes() {
    let scratch = aterm_tempfile::Builder::new()
        .prefix("atv-scoped-lib-")
        .suffix(".noindex")
        .tempdir()
        .unwrap();
    let tmp = scratch.path();
    let bin_only = tmp.join("crates/binonly");
    std::fs::create_dir_all(bin_only.join("src")).unwrap();
    std::fs::write(
        bin_only.join("Cargo.toml"),
        "[package]\nname = \"binonly\"\n",
    )
    .unwrap();
    assert!(!crate_dir_has_lib(tmp, "binonly"));
    std::fs::write(bin_only.join("src/lib.rs"), "").unwrap();
    assert!(crate_dir_has_lib(tmp, "binonly"));
    let declared = tmp.join("crates/declared");
    std::fs::create_dir_all(&declared).unwrap();
    std::fs::write(
        declared.join("Cargo.toml"),
        "[package]\nname = \"declared\"\n[lib]\npath = \"other.rs\"\n",
    )
    .unwrap();
    assert!(crate_dir_has_lib(tmp, "declared"));
    assert!(crate_dir_has_lib(tmp, "no-such-member"));
    let m = Members::new(vec![
        Member {
            name: "aterm-lab".into(),
            dir: "crates/aterm-lab".into(),
            has_lib: false,
        },
        Member {
            name: "aterm-grid".into(),
            dir: "crates/aterm-grid".into(),
            has_lib: true,
        },
    ]);
    assert!(!m.any_has_lib(&["aterm-lab".to_string()]));
    assert!(m.any_has_lib(&["aterm-lab".to_string(), "aterm-grid".to_string()]));
    let none = Members::new(vec![Member {
        name: "aterm-lab".into(),
        dir: "crates/aterm-lab".into(),
        has_lib: false,
    }]);
    assert!(none.any_has_lib(&["aterm-lab".to_string()]));
    assert!(!declares_lib_target(
        "[package]\nname=\"x\"\n[[bin]]\nname=\"x\"\n"
    ));
}

#[test]
fn graph_lines_are_parsed_by_name_and_by_being_inside_the_repo() {
    let text = "\
aterm-grid v0.10.0 (/repo/crates/aterm-grid)
aterm-error-derive v0.10.0 (proc-macro) (/repo/crates/aterm-error-derive)
aterm-gui v0.10.0 (/repo/crates/aterm-gui) (*)
libc v0.2.0
indexmap v2.14.0 (/repo/vendor/indexmap)
";
    let names: Vec<String> = parse_graph_entries(text, "/repo/")
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    assert_eq!(
        names,
        ["aterm-grid", "aterm-error-derive", "aterm-gui", "indexmap"]
    );
    let text = "a v1.0.0 (/repo (copy)/crates/a (local)) (*)\nb v1.0.0 (proc-macro) (/repo (copy)/crates/b)";
    assert_eq!(
        parse_graph_entries(text, "/repo (copy)/"),
        [
            ("a".into(), "crates/a (local)".into()),
            ("b".into(), "crates/b".into()),
        ]
    );
    assert!(parse_graph_entries("a v1.0.0 (/repo (copy)/crates/a", "/repo (copy)/").is_empty());
}

/// The root prefix a Windows checkout's `cargo tree` lines start with, from each spelling the
/// root reaches it in: git's `C:/…`, `fs::canonicalize`'s `\\?\C:\…` and `\\?\UNC\…`, and the
/// plain one. NEGATIVE CONTROL: git's `C:/…` root with only the separator appended, the prefix
/// this replaced, reads no package, and the cone then widens to the whole workspace.
#[test]
fn a_windows_root_is_spelled_as_cargo_tree_prints_it() {
    let tree = "a v1.0.0 (C:\\w\\crates\\a)\nb v1.0.0 (\\\\srv\\share\\w\\b) (*)\nserde v1.0.0\n";
    for root in ["C:/w", "C:\\w", "\\\\?\\C:\\w", "C:\\w\\"] {
        assert_eq!(
            parse_graph_entries(tree, &tree_prefix(root, true)),
            [("a".into(), "crates\\a".into())],
            "{root}"
        );
    }
    assert_eq!(
        parse_graph_entries(tree, &tree_prefix("\\\\?\\UNC\\srv\\share\\w", true)),
        [("b".into(), "b".into())]
    );
    assert!(parse_graph_entries(tree, "C:/w\\").is_empty());
    assert_eq!(tree_prefix("/repo (copy)", false), "/repo (copy)/");
    assert_eq!(tree_prefix("/", false), "/");
}

#[test]
fn the_package_name_comes_from_the_package_table_and_nowhere_else() {
    assert_eq!(
        manifest_package_name("[package]\nname = \"aterm-grid\"\nversion = \"0.1.0\"\n").as_deref(),
        Some("aterm-grid")
    );
    assert_eq!(
        manifest_package_name("[workspace]\nmembers = [\"crates/*\"]\n"),
        None,
        "a virtual manifest declares no package"
    );
    assert_eq!(
        manifest_package_name("[package]\nname=\"tight\"\n").as_deref(),
        Some("tight")
    );
    assert_eq!(
        manifest_package_name("[package]\nversion = \"1\"\n\n[[bin]]\nname = \"notthis\"\n"),
        None,
        "only the package name is a -p spec"
    );
    assert_eq!(
        manifest_package_name("[package]\nname = \"first\"\n\n[lib]\nname = \"second\"\n")
            .as_deref(),
        Some("first")
    );
    assert_eq!(
        manifest_package_name(
            "[package.metadata.docs]\nname = \"wrong\"\n[package] # comment\nname = 'single'\n"
        )
        .as_deref(),
        Some("single")
    );
    assert!(declares_lib_target(
        "  [lib] # comment\npath = \"src/lib.rs\"\n"
    ));
    assert!(!declares_lib_target("[lib-extra]\nname = \"x\"\n"));
}

/// No path the selection widens for, and no file a member tests from outside its crate, is in
/// the no-reach class `verify land` composes past (`aterm_census::no_reach::class`): a change
/// to one is a build input, so composing it unjudged would credit a check that never read it.
#[test]
fn what_replans_or_is_tested_from_outside_is_never_no_reach() {
    let layout = aterm_census::no_reach::Layout::new(
        Some("[workspace]\nmembers = [\"crates/*\"]\n"),
        ["Cargo.toml", "crates/aterm-update/Cargo.toml"],
    );
    for path in REPLANNING_PATHS
        .iter()
        .chain(TESTED_BY.iter().map(|(path, _)| path))
    {
        assert!(!aterm_census::no_reach::class(path, &layout), "{path}");
        if path.ends_with('/') {
            let inside = format!("{path}README.md");
            assert!(!aterm_census::no_reach::class(&inside, &layout), "{inside}");
        }
    }
}
