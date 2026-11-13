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
        &BTreeSet::new(),
        manifests,
        members,
        path_packages,
        &|name| dependents(name).map(linking),
        &|_| replan_everything(),
        &|_, _| false,
        &BTreeMap::new,
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
            &BTreeSet::new(),
            &manifests(),
            &m,
            &no_packages,
            &|name| graph(edges)(name).map(linking),
            &|_| replan.clone(),
            &|_, _| false,
            &BTreeMap::new,
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
        &BTreeSet::new(),
        &manifests(),
        &m,
        &forks,
        &|name| graph(edges)(name).map(linking),
        &|_| replan.clone(),
        &|_, _| false,
        &BTreeMap::new,
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
            &BTreeSet::new(),
            &manifests,
            &m,
            &forks,
            &reach,
            &|_| Replan::Nothing,
            &|_, _| false,
            &BTreeMap::new,
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
        &BTreeSet::new(),
        &manifests,
        &m,
        &|| Some(Vec::new()),
        &reach,
        &|_| Replan::Nothing,
        &|_, _| false,
        &BTreeMap::new,
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
    let pick = |changed: &[&str], judge: &dyn Fn(&str, &[&str]) -> bool, replan: Replan| {
        super::select(
            &paths(changed),
            &BTreeSet::new(),
            &manifests(),
            &m,
            &no_packages,
            &|name| graph(EDGES)(name).map(linking),
            &|_| replan.clone(),
            judge,
            &BTreeMap::new,
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

/// How a member observes a changed path decides what it brings: one whose tests alone observe
/// it joins with all of its targets and no dependent; one whose linked code observes a path of a
/// seed judged whole joins the same way, since that seed's dependents are walked already; one
/// whose linked code observes a path no member owns is a seed whose dependents are walked; one
/// that observes nothing, or is no member, adds nothing. `observe` is asked once, and its answer
/// stands whichever seeds are judged alone. CONTROL: with no observer, the confined seed is alone
/// and the whole one walks its dependent.
#[test]
fn an_observer_is_walked_or_joins_by_how_it_observes() {
    use crate::own_tests::Observed;
    let m = members(&["u", "ud", "v", "w", "wd", "x", "xd", "y", "yd", "z", "zd"]);
    let manifests: Manifests = m.0.iter().map(|member| member.dir.clone()).collect();
    const LINKS: &[(&str, &str)] = &[
        ("u", "ud"),
        ("w", "wd"),
        ("x", "xd"),
        ("y", "yd"),
        ("z", "zd"),
    ];
    let (x, y) = ("crates/x/src/lib.rs", "crates/y/src/lib.rs");
    let changed = paths(&[x, y]);
    let asked = std::cell::RefCell::new(Vec::new());
    let observed = std::cell::Cell::new(0);
    let pick = |observers: &[(&str, &str, Observed)]| {
        super::select(
            &changed,
            &BTreeSet::new(),
            &manifests,
            &m,
            &no_packages,
            &|name| graph(LINKS)(name).map(linking),
            &|_| replan_everything(),
            &|member, _| {
                asked.borrow_mut().push(member.to_string());
                member == "x"
            },
            &|| {
                observed.set(observed.get() + 1);
                let mut by: BTreeMap<String, BTreeMap<String, Observed>> = BTreeMap::new();
                for (namer, path, how) in observers {
                    by.entry((*namer).to_string())
                        .or_default()
                        .insert((*path).to_string(), *how);
                }
                by
            },
        )
    };
    let sel = pick(&[
        ("v", x, Observed::Nothing),
        ("z", x, Observed::Tests),
        ("w", y, Observed::Linked),
        ("u", "tools/r/src/main.rs", Observed::Linked),
        ("outsider", x, Observed::Linked),
    ]);
    assert_eq!(asked.take(), ["x", "y"]);
    assert_eq!(observed.replace(0), 1);
    let Selection::Narrowed {
        seeds,
        crates,
        all_targets,
        alone,
        ..
    } = sel
    else {
        panic!("widened: {sel:?}");
    };
    assert_eq!(seeds, paths(&["u", "x", "y"]));
    assert_eq!(crates, paths(&["u", "ud", "w", "x", "y", "yd", "z"]));
    assert_eq!(all_targets, paths(&["u", "w", "x", "y", "z"]));
    assert_eq!(alone, paths(&["x"]));
    let control = pick(&[]);
    assert_eq!(observed.get(), 1);
    assert!(
        matches!(&control, Selection::Narrowed { crates, alone, .. }
            if *crates == paths(&["x", "y", "yd"]) && *alone == paths(&["x"])),
        "{control:?}"
    );
}

/// A confined seed whose changed path a namer's linked code spells is judged whole, with the
/// namer's own rows, unless walking the namer adds strictly fewer members than the seed's own
/// dependents: a namer with a wide closure keeps the seed whole, and a seed with a wide closure
/// goes alone with a narrow namer walked. Neither side counts the seed or its namers, which the
/// cone holds either way; several namers count each member they add once; and a dev-only
/// dependent counts as one the cone holds. `dependents` is asked once of the seed and of each
/// namer, whichever wins. CONTROL: a namer that adds nothing walks beside the narrow seed, and
/// namers that add as many members as the wide seed keep it whole, as main judged it.
#[test]
fn a_confined_seeds_namers_walk_only_where_that_adds_fewer_members() {
    use crate::own_tests::Observed;
    let m = members(&[
        "a", "ad1", "b", "bd1", "bd2", "e", "ed", "m", "md1", "o", "o2", "p1", "p2", "px", "w",
        "wd1", "wd2",
    ]);
    let manifests: Manifests = m.0.iter().map(|member| member.dir.clone()).collect();
    const LINKS: &[(&str, &str)] = &[
        ("a", "ad1"),
        ("b", "bd1"),
        ("b", "bd2"),
        ("m", "md1"),
        ("p1", "px"),
        ("p2", "px"),
        ("w", "wd1"),
        ("w", "wd2"),
    ];
    const DEV: &[(&str, &str)] = &[("e", "ed")];
    let asked = std::cell::RefCell::new(Vec::new());
    let pick = |seed: &str, namers: &[&str]| {
        let path = format!("crates/{seed}/src/lib.rs");
        let selection = super::select(
            &paths(&[path.as_str()]),
            &BTreeSet::new(),
            &manifests,
            &m,
            &no_packages,
            &|name| {
                asked.borrow_mut().push(name.to_string());
                graph(LINKS)(name).map(|links| Reach {
                    links,
                    dev_only: DEV
                        .iter()
                        .filter(|(dep, _)| *dep == name)
                        .map(|(_, user)| (*user).to_string())
                        .collect(),
                    ..Reach::default()
                })
            },
            &|_| replan_everything(),
            &|_, _| true,
            &|| {
                namers
                    .iter()
                    .map(|namer| {
                        (
                            (*namer).to_string(),
                            BTreeMap::from([(path.clone(), Observed::Linked)]),
                        )
                    })
                    .collect()
            },
        );
        let mut dependents = asked.take();
        dependents.sort();
        (picked(selection), dependents)
    };
    let all = |seed: &str, namers: &[&str]| {
        let mut all = paths(namers);
        all.push(seed.to_string());
        all.sort();
        all
    };
    // `w` adds two members, `a` one: `a` is whole and `w` brings its own rows.
    assert_eq!(
        pick("a", &["w"]),
        (
            (paths(&["a"]), paths(&["a", "ad1", "w"]), Vec::new()),
            all("a", &["w"])
        )
    );
    // CONTROL: `o` adds none, fewer than `a`'s one: `a` is alone and `o` is walked.
    assert_eq!(
        pick("a", &["o"]),
        (
            (paths(&["a", "o"]), paths(&["a", "o"]), paths(&["a"])),
            all("a", &["o"])
        )
    );
    // `b` adds two members, `m` one: `b` is alone and `m` walks its dependent.
    assert_eq!(
        pick("b", &["m"]),
        (
            (paths(&["b", "m"]), paths(&["b", "m", "md1"]), paths(&["b"])),
            all("b", &["m"])
        )
    );
    // CONTROL: `w` adds two, as many as `b`: the tie keeps `b` whole.
    assert_eq!(
        pick("b", &["w"]),
        (
            (paths(&["b"]), paths(&["b", "bd1", "bd2", "w"]), Vec::new()),
            all("b", &["w"])
        )
    );
    // `o` and `o2` add none, counted without themselves, against `a`'s `ad1`: `a` is alone.
    assert_eq!(
        pick("a", &["o", "o2"]),
        (
            (
                paths(&["a", "o", "o2"]),
                paths(&["a", "o", "o2"]),
                paths(&["a"])
            ),
            all("a", &["o", "o2"])
        )
    );
    // `p1` and `p2` add one member between them, `px`, fewer than `b`'s two: `b` is alone.
    assert_eq!(
        pick("b", &["p1", "p2"]),
        (
            (
                paths(&["b", "p1", "p2"]),
                paths(&["b", "p1", "p2", "px"]),
                paths(&["b"])
            ),
            all("b", &["p1", "p2"])
        )
    );
    // CONTROL: `a` and `m` add one each, two between them, as many as `b`: `b` is whole.
    assert_eq!(
        pick("b", &["a", "m"]),
        (
            (
                paths(&["b"]),
                paths(&["a", "b", "bd1", "bd2", "m"]),
                Vec::new()
            ),
            all("b", &["a", "m"])
        )
    );
    // `e`'s one dependent is dev-only, more than `o`'s none: `e` is alone and `o` is walked.
    assert_eq!(
        pick("e", &["o"]),
        (
            (paths(&["e", "o"]), paths(&["e", "o"]), paths(&["e"])),
            all("e", &["o"])
        )
    );
}

/// A confined seed whose own linked code observes another changed path is judged whole where
/// main judged it whole, so the cone walks its dependents, which may read that path through it:
/// where another member spells one of its paths, in any class (a plain comment, a test, linked
/// code), or one of its paths reads another package's changed path. `confined` is not asked of
/// it. CONTROL: such a seed whose paths no member spells or reads is alone, as main judged it,
/// and a seed whose path is spelled but whose code observes nothing goes alone too.
#[test]
fn a_seed_whose_linked_code_observes_another_change_is_whole_where_main_kept_it_whole() {
    use crate::own_tests::Observed;
    let m = members(&["a", "ad", "b", "c"]);
    let manifests: Manifests = m.0.iter().map(|member| member.dir.clone()).collect();
    const LINKS: &[(&str, &str)] = &[("a", "ad")];
    let (a, b) = ("crates/a/src/lib.rs", "crates/b/src/lib.rs");
    let reader = "crates/a/tests/peek.rs";
    let asked = std::cell::RefCell::new(Vec::new());
    let pick = |changed: &[&str],
                readers: &[&str],
                judged: &[&str],
                observers: &[(&str, &str, Observed)]| {
        let selection = super::select(
            &paths(changed),
            &readers.iter().map(|path| (*path).to_string()).collect(),
            &manifests,
            &m,
            &no_packages,
            &|name| graph(LINKS)(name).map(linking),
            &|_| replan_everything(),
            &|member, _| {
                asked.borrow_mut().push(member.to_string());
                judged.contains(&member)
            },
            &|| {
                let mut by: BTreeMap<String, BTreeMap<String, Observed>> = BTreeMap::new();
                for (namer, path, how) in observers {
                    by.entry((*namer).to_string())
                        .or_default()
                        .insert((*path).to_string(), *how);
                }
                by
            },
        );
        (picked(selection), asked.take())
    };
    let whole = (paths(&["a", "b"]), paths(&["a", "ad", "b"]), Vec::new());
    // Each spells the other's path in its library: main kept both whole, and `ad`, whose tests
    // call `a`, which reads `b`'s file, ran.
    assert_eq!(
        pick(
            &[a, b],
            &[],
            &["a", "b"],
            &[("a", b, Observed::Linked), ("b", a, Observed::Linked)]
        ),
        (whole.clone(), Vec::new())
    );
    for how in [Observed::Nothing, Observed::Tests] {
        let (selection, asked) = pick(
            &[a, b],
            &[],
            &["a"],
            &[("a", b, Observed::Linked), ("c", a, how)],
        );
        let mut crates = whole.1.clone();
        if how == Observed::Tests {
            crates.push("c".to_string());
        }
        assert_eq!(selection, (whole.0.clone(), crates, Vec::new()), "{how:?}");
        assert_eq!(asked, paths(&["b"]), "{how:?}");
    }
    // `a`'s test reads another package's changed file: main's reader veto kept it whole.
    assert_eq!(
        pick(
            &[reader, b],
            &[reader],
            &["a"],
            &[("a", b, Observed::Linked)]
        ),
        (whole.clone(), paths(&["b"]))
    );
    // CONTROL: no member spells or reads `a`'s path, so main judged it alone and ran no `ad`.
    assert_eq!(
        pick(&[a, b], &[], &["a"], &[("a", b, Observed::Linked)]),
        (
            (paths(&["a", "b"]), paths(&["a", "b"]), paths(&["a"])),
            paths(&["a", "b"])
        )
    );
    // CONTROL: `c` spells `a`'s path in a plain comment, and `a` observes nothing.
    assert_eq!(
        pick(&[a, b], &[], &["a"], &[("c", a, Observed::Nothing)]),
        (
            (paths(&["a", "b"]), paths(&["a", "b"]), paths(&["a"])),
            paths(&["a", "b"])
        )
    );
}

/// A fixture member: its folder under `crates/`, its manifest rows, and its files by
/// package-relative path.
type Package<'a> = (&'a str, &'a str, Vec<(&'a str, String)>);

/// A fixture workspace at a fresh path: each `(dir, manifest rows, files)` is a member under
/// `crates/`, `extra` are other files by repo-relative path, and cargo has written the lock.
fn workspace(
    prefix: &str,
    packages: &[Package<'_>],
    extra: &[(&str, &str)],
) -> (std::path::PathBuf, std::path::PathBuf) {
    let made = crate::mktemp_dir(prefix).expect("mktemp");
    let root = std::fs::canonicalize(&made).expect("the fixture's own path");
    let members: Vec<String> = packages
        .iter()
        .map(|(dir, _, _)| format!("'crates/{dir}'"))
        .collect();
    std::fs::write(
        root.join("Cargo.toml"),
        format!(
            "[workspace]\nresolver='2'\nmembers=[{}]\n",
            members.join(",")
        ),
    )
    .expect("workspace manifest");
    let write = |path: &Path, text: &str| {
        std::fs::create_dir_all(path.parent().unwrap()).expect("fixture dir");
        std::fs::write(path, text).expect("fixture file");
    };
    for (dir, rest, files) in packages {
        let package = root.join("crates").join(dir);
        write(&package.join("src/lib.rs"), "");
        for (path, text) in files {
            write(&package.join(path), text);
        }
        write(
            &package.join("Cargo.toml"),
            &format!("[package]\nname='{dir}'\nversion='0.1.0'\nedition='2021'\n{rest}"),
        );
    }
    for (path, text) in extra {
        write(&root.join(path), text);
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    targo_tree(
        &root,
        Path::new(env!("CARGO")),
        &path,
        &["tree", "--workspace"],
    )
    .expect("cargo writes the lock");
    (made, root)
}

/// A selection's members whose test, bench and example targets the lint compiles.
fn all_targets(selection: &Selection) -> Vec<String> {
    match selection {
        Selection::Narrowed { all_targets, .. } => all_targets.clone(),
        Selection::Widened(why) => panic!("widened: {why}"),
    }
}

/// A selection's seeds, crates and alone seeds.
fn picked(selection: Selection) -> (Vec<String>, Vec<String>, Vec<String>) {
    match selection {
        Selection::Narrowed {
            seeds,
            crates,
            alone,
            ..
        } => (seeds, crates, alone),
        Selection::Widened(why) => panic!("widened: {why}"),
    }
}

/// End to end on a real checkout and graph: a change inside a member's test module, to a file
/// only its tests mount, or to a comment no macro reads, plain or doc, selects that member alone;
/// a change to a shipped line selects its dependents too, and so does a doc comment that moves the
/// code after it, sits on a derived item or in a macro's body; a test-only change to a file
/// another member embeds by a `../` literal keeps that reader in the cone with its own dependents
/// but leaves the member's linkers out, and the reader stays whole when its own change is
/// test-only. A member that spells another member's changed path observes it, whether that
/// member is judged alone or whole: where its library spells it beside a member judged alone,
/// whose two dependents are more than its one, as a seed with its dependents; where only its
/// tests do, with all of its own targets alone, and so does a member whose tests mount the file
/// that spells it; where a plain comment does, not at all; where a nested package under its
/// folder that no member is spells it, as a seed with its dependents, since its tests may run
/// that package; and where it spells two changed paths, one in its library and one in its tests,
/// in either order, as the library spelling. Beside a member judged whole, whose dependents the
/// cone walks already, a library spelling joins with its own rows and walks nothing. CONTROL: the
/// test spelling of the pair alone walks nothing, a nested package under no member is no member's
/// spelling, and a change gathered from no one tree keeps every dependent.
#[test]
fn a_test_only_change_on_a_real_checkout_selects_its_member_alone() {
    let services = "/// The tester.\npub fn tester() -> &'static str {\n    // The name.\n    \"tester\"\n}\n\n/// A record.\n#[derive(Debug)]\npub struct Record;\n\nmacro_rules! one {\n    () => {\n        // One.\n        1\n    };\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn names() {\n        assert_eq!(super::tester(), \"tester\");\n    }\n}\n";
    let untested = "#[cfg(test)]\nmod tests {}\n";
    let tested = "#[cfg(test)]\nmod tests {\n    fn t() {}\n}\n";
    let reader = "pub const PEEK: &str = include_str!(\"../../a/src/peek.rs\");\npub const TEST_PEEK: &str = include_str!(\"../../a/tests/peek.rs\");\npub const NAMED: &str = \"crates/a/src/named.rs\";\n";
    let recorder =
        "[package]\nname = \"recorder\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n";
    let recorded = |path: &str| {
        format!(
            "const SRC: &str = \"{path}\";\n\nfn main() {{\n    let _ = std::fs::read_to_string(SRC);\n}}\n"
        )
    };
    let owned = |files: &[(&'static str, &str)]| -> Vec<(&'static str, String)> {
        files
            .iter()
            .map(|(path, text)| (*path, (*text).to_string()))
            .collect()
    };
    let (made, root) = workspace(
        "atv-changed-own-tests",
        &[
            (
                "a",
                "",
                owned(&[
                    (
                        "src/lib.rs",
                        "pub mod services;\n\n#[cfg(test)]\n#[path = \"lib_tests.rs\"]\nmod tests;\n",
                    ),
                    ("src/services.rs", services),
                    ("src/lib_tests.rs", "#[test]\nfn one() {}\n"),
                    ("src/peek.rs", untested),
                    ("src/named.rs", untested),
                    ("src/named_tests.rs", untested),
                    ("src/named_support.rs", untested),
                    ("src/named_comment.rs", untested),
                    ("src/named_nested.rs", untested),
                    ("src/named_outside.rs", untested),
                    ("src/pair_a1.rs", untested),
                    ("src/pair_a2.rs", untested),
                    ("src/pair_b1.rs", untested),
                    ("src/pair_b2.rs", untested),
                    ("tests/integration.rs", "#[test]\nfn one() {}\n"),
                    ("tests/peek.rs", "#[test]\nfn one() {}\n"),
                ]),
            ),
            (
                "b",
                "[dependencies]\na={path='../a'}\n",
                owned(&[("src/lib.rs", "pub use a::services::tester;\n")]),
            ),
            ("b2", "[dependencies]\na={path='../a'}\n", Vec::new()),
            ("c", "", owned(&[("src/lib.rs", reader)])),
            ("d", "[dependencies]\nc={path='../c'}\n", Vec::new()),
            (
                "n",
                "",
                owned(&[
                    (
                        "src/lib.rs",
                        "#[cfg(test)]\nmod tests {\n    const NAMED: &str = \"crates/a/src/named_tests.rs\";\n}\n",
                    ),
                    ("tests/n.rs", "mod support;\n"),
                    (
                        "tests/support/mod.rs",
                        "pub const NAMED: &str = \"crates/a/src/named_support.rs\";\n",
                    ),
                ]),
            ),
            (
                "p",
                "",
                owned(&[("src/lib.rs", "// See crates/a/src/named_comment.rs.\n")]),
            ),
            (
                "q",
                "",
                owned(&[(
                    "tests/q.rs",
                    "#[path = \"../../n/tests/support/mod.rs\"]\nmod support;\n",
                )]),
            ),
            (
                "o",
                "",
                owned(&[
                    (
                        "tests/run.rs",
                        "#[test]\nfn run() {\n    let _ = \"tests/recorder\";\n}\n",
                    ),
                    ("tests/recorder/Cargo.toml", recorder),
                    (
                        "tests/recorder/src/main.rs",
                        &recorded("crates/a/src/named_nested.rs"),
                    ),
                ]),
            ),
            ("od", "[dependencies]\no={path='../o'}\n", Vec::new()),
            (
                "t",
                "",
                owned(&[(
                    "src/lib.rs",
                    "pub const FIRST: &str = \"crates/a/src/pair_a1.rs\";\npub const LAST: &str = \"crates/a/src/pair_b2.rs\";\n\n#[cfg(test)]\nmod tests {\n    const SECOND: &str = \"crates/a/src/pair_a2.rs\";\n    const FIRST: &str = \"crates/a/src/pair_b1.rs\";\n}\n",
                )]),
            ),
            ("u", "[dependencies]\nt={path='../t'}\n", Vec::new()),
        ],
        &[
            ("tools/rec/Cargo.toml", recorder),
            (
                "tools/rec/src/main.rs",
                &recorded("crates/a/src/named_outside.rs"),
            ),
        ],
    );
    let checkout = Committed::new(&root);
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
    let named_shipped = checkout.change(&[("crates/a/src/named.rs", "pub fn x() {}\n")]);
    let shipped_rows = all_targets(&named_shipped);
    let named_shipped = picked(named_shipped);
    let named_tests = checkout.change(&[("crates/a/src/named_tests.rs", tested)]);
    let named_support = checkout.change(&[("crates/a/src/named_support.rs", tested)]);
    let watched = [&named_tests, &named_support].map(all_targets);
    let (named_tests, named_support) = (picked(named_tests), picked(named_support));
    // `t` spells one of each pair in its library and the other in its tests, the library's
    // first in one pair and last in the other: it observes each pair as the most of the two.
    let pair_a = picked(checkout.change(&[
        ("crates/a/src/pair_a1.rs", tested),
        ("crates/a/src/pair_a2.rs", tested),
    ]));
    let pair_b = picked(checkout.change(&[
        ("crates/a/src/pair_b1.rs", tested),
        ("crates/a/src/pair_b2.rs", tested),
    ]));
    let pair_tests = checkout.change(&[("crates/a/src/pair_b1.rs", tested)]);
    let pair_watched = all_targets(&pair_tests);
    let pair_tests = picked(pair_tests);
    let named_comment = picked(checkout.change(&[("crates/a/src/named_comment.rs", tested)]));
    let named_nested = picked(checkout.change(&[("crates/a/src/named_nested.rs", tested)]));
    let named_outside = picked(checkout.change(&[("crates/a/src/named_outside.rs", tested)]));
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
    let ab = paths(&["a", "b", "b2"]);
    let ac = paths(&["a", "c"]);
    let acd = paths(&["a", "c", "d"]);
    assert_eq!(tests, (a.clone(), a.clone(), a.clone()));
    assert_eq!(mounted, (a.clone(), a.clone(), a.clone()));
    assert_eq!(integration, (a.clone(), a.clone(), a.clone()));
    assert_eq!(integration_reader, (ac.clone(), acd.clone(), a.clone()));
    assert_eq!(shipped, (a.clone(), ab.clone(), Vec::new()));
    assert_eq!(embedded, (ac.clone(), acd.clone(), a.clone()));
    assert_eq!(both, (ac.clone(), acd.clone(), a.clone()));
    // `c`'s library spells `named.rs`: walking `c` adds `d`, fewer than `a`'s `b` and `b2`, so
    // `a` is alone and `c` is a seed whose dependent `d` is walked; `b` and `b2` are not.
    assert_eq!(named, (ac.clone(), acd, a.clone()));
    // A seed whose own change reaches its dependents brings its namers too, with their own rows:
    // `c` still spells the path, but the cone walks `a`'s dependents already, so `d` stays out.
    assert_eq!(
        named_shipped,
        (a.clone(), paths(&["a", "b", "b2", "c"]), Vec::new())
    );
    assert_eq!(shipped_rows, ac);
    assert_eq!(named_tests, (a.clone(), paths(&["a", "n"]), a.clone()));
    // A member whose tests alone observe the path joins with all of its targets.
    assert_eq!(watched, [paths(&["a", "n"]), paths(&["a", "n", "q"])]);
    let at = paths(&["a", "t"]);
    let atu = paths(&["a", "t", "u"]);
    assert_eq!(pair_a, (at.clone(), atu.clone(), a.clone()));
    assert_eq!(pair_b, (at.clone(), atu, a.clone()));
    // CONTROL: the test spelling alone adds `t` with its rows and walks nothing.
    assert_eq!(pair_tests, (a.clone(), at.clone(), a.clone()));
    assert_eq!(pair_watched, at);
    // `q`'s tests mount the file of `n` that spells the path: a reader, judged where it compiles.
    assert_eq!(
        named_support,
        (
            paths(&["a", "q"]),
            paths(&["a", "n", "q"]),
            paths(&["a", "q"])
        )
    );
    assert_eq!(named_comment, (a.clone(), a.clone(), a.clone()));
    // `o`'s test runs a nested package no member is, whose code reads the path it spells: that
    // package is `o`'s, so `o` is a seed and `od`, which links it, is walked.
    assert_eq!(
        named_nested,
        (paths(&["a", "o"]), paths(&["a", "o", "od"]), a.clone())
    );
    // CONTROL: the same package under no member is no member's spelling.
    assert_eq!(named_outside, (a.clone(), a.clone(), a.clone()));
    assert_eq!(doc, (a.clone(), a.clone(), a.clone()));
    assert_eq!(plain, (a.clone(), a.clone(), a.clone()));
    for control in [shifted, derived, in_macro] {
        assert_eq!(control, (a.clone(), ab.clone(), Vec::new()));
    }
    assert_eq!(gathered, (a, ab, Vec::new()));
}

/// End to end: a member that reads another's changed file only from its benches, examples or
/// tests is judged alone with its own rows, and so is one whose test mounts a file of a nested
/// package no member owns, with or without a `../`; one whose library or build script reads it
/// keeps its dependents, and so does one whose nested package holds a reader of it. A
/// file a foreign `#[path]` mount compiles through the mounted file's own declarations, its
/// test-only ones among them, makes the mounter a reader as the mounted file does, and a
/// declaration of that file whose default file is not there names only its own default files.
/// A test that embeds a module file by `include_str!` reads only that file's bytes, so it is no
/// reader of the module's children nor of a default file the module names: deleting one leaves
/// the embedder out. CONTROL: the member that owns the mounted file judges its own change by its
/// own placement, a library reader brings its dependent, a bench's `include_str!` of a file
/// outside every member reaches the bench's member alone, a test that mounts the same module
/// file by `#[path]` is a reader of the default file deleted under it, and the nested package's
/// copy under no member is no member's. A nested package's reader of its enclosing member's own
/// test-only file adds nothing to that member's own judgment, which stays alone; one that reads
/// another member's file, or a source that spells another member's changed path, makes the
/// enclosing member observe that path, joining with its own rows where its member is whole.
#[test]
fn a_member_whose_reads_reach_only_its_own_targets_is_judged_alone() {
    let mount = |path: &str| format!("#[path = \"{path}\"]\nmod support;\n");
    let support = "../../a/benches/support/mod.rs";
    let support_mods = "mod timing;\n#[cfg(test)]\nmod t;\n";
    let nested_package = "[package]\nname='gen'\nversion='0.1.0'\nedition='2021'\n\n[workspace]\n";
    let (made, root) = workspace(
        "atv-changed-readers",
        &[
            (
                "a",
                "",
                vec![
                    (
                        "src/lib.rs",
                        "#[cfg(all(test, unix))]\n#[path = \"../benches/support/mod.rs\"]\npub mod runner;\n".to_string(),
                    ),
                    (
                        "benches/support/mod.rs",
                        format!("/// Doc.\npub fn run() {{}}\n{support_mods}"),
                    ),
                    ("benches/support/timing.rs", "pub fn now() {}\n".to_string()),
                    ("benches/support/t.rs", "pub fn t() {}\n".to_string()),
                ],
            ),
            (
                "r",
                "\n[[bench]]\nname = \"x\"\nharness = false\n",
                vec![(
                    "benches/x.rs",
                    format!(
                        "{}const S: &str = include_str!(\"../../../tools/s.sh\");\nfn main() {{}}\n",
                        mount(support)
                    ),
                )],
            ),
            ("d", "[dependencies]\nr={path='../r'}\n", Vec::new()),
            (
                "l",
                "",
                vec![(
                    "src/lib.rs",
                    format!("#[path = \"{support}\"]\npub mod support;\n"),
                )],
            ),
            ("e", "[dependencies]\nl={path='../l'}\n", Vec::new()),
            (
                "k",
                "",
                vec![(
                    "build.rs",
                    format!(
                        "{}fn main() {{\n    println!(\"cargo:rerun-if-changed=build.rs\");\n}}\n",
                        mount("../a/benches/support/mod.rs")
                    ),
                )],
            ),
            (
                "w",
                "",
                vec![
                    ("tests/s.rs", "#[path = \"../gen/src/g.rs\"]\nmod g;\n".to_string()),
                    ("gen/Cargo.toml", nested_package.to_string()),
                    ("gen/src/g.rs", "pub fn g() {}\n".to_string()),
                ],
            ),
            (
                "g",
                "",
                vec![
                    ("src/lib.rs", "pub mod page;\npub mod part;\n".to_string()),
                    ("src/part.rs", "pub fn part() {}\n".to_string()),
                    ("src/page.rs", "mod inner;\nmod part;\n".to_string()),
                    ("src/page/inner.rs", "pub fn inner() {}\n".to_string()),
                    ("src/page/part.rs", "pub fn part() {}\n".to_string()),
                ],
            ),
            (
                "m",
                "",
                vec![(
                    "tests/m.rs",
                    "const P: &str = include_str!(\"../../g/src/page.rs\");\n".to_string(),
                )],
            ),
            (
                "q",
                "",
                vec![(
                    "tests/q.rs",
                    "#[path = \"../../g/src/page.rs\"]\nmod page;\n".to_string(),
                )],
            ),
            (
                "u",
                "",
                vec![
                    ("tests/s.rs", "#[path = \"gen/src/g.rs\"]\nmod g;\n".to_string()),
                    ("tests/gen/Cargo.toml", nested_package.to_string()),
                    ("tests/gen/src/g.rs", "pub fn g() {}\n".to_string()),
                    (
                        "tests/gen/src/peek.rs",
                        "pub const S: &str = include_str!(\"../../../../../tools/s.sh\");\n"
                            .to_string(),
                    ),
                ],
            ),
            (
                "y",
                "",
                vec![
                    (
                        "src/lib.rs",
                        "#[cfg(test)]\n#[path = \"../tests/oracle/corpus.rs\"]\nmod corpus;\n"
                            .to_string(),
                    ),
                    ("tests/oracle/corpus.rs", "pub fn c() {}\n".to_string()),
                    ("tests/recorder/Cargo.toml", nested_package.to_string()),
                    (
                        "tests/recorder/src/main.rs",
                        "#[path = \"../../oracle/corpus.rs\"]\nmod corpus;\n#[path = \"../../../../z/src/lib.rs\"]\nmod z;\n#[path = \"../../../../v/src/names.rs\"]\nmod names;\n\nfn main() {}\n"
                            .to_string(),
                    ),
                ],
            ),
            ("yd", "[dependencies]\ny={path='../y'}\n", Vec::new()),
            (
                "z",
                "",
                vec![
                    ("src/lib.rs", "pub fn z() {}\n".to_string()),
                    ("data/table.txt", "one\n".to_string()),
                ],
            ),
            (
                "v",
                "",
                vec![
                    ("src/lib.rs", "pub mod names;\n".to_string()),
                    (
                        "src/names.rs",
                        "pub const TABLE: &str = \"crates/z/data/table.txt\";\n".to_string(),
                    ),
                ],
            ),
        ],
        &[
            ("tools/s.sh", "#!/bin/sh\n"),
            ("tools/gen/Cargo.toml", nested_package),
            ("tools/gen/src/g.rs", "pub fn g() {}\n"),
        ],
    );
    let checkout = Committed::new(&root);
    let doc = picked(checkout.change(&[(
        "crates/a/benches/support/mod.rs",
        &format!("/// The doc.\npub fn run() {{}}\n{support_mods}"),
    )]));
    let script = picked(checkout.change(&[("tools/s.sh", "#!/bin/sh\nexit 0\n")]));
    let nested = picked(checkout.change(&[(
        "crates/a/benches/support/timing.rs",
        "pub fn now() { let _ = 1; }\n",
    )]));
    let nested_test = picked(checkout.change(&[(
        "crates/a/benches/support/t.rs",
        "pub fn t() { let _ = 1; }\n",
    )]));
    let generated = picked(checkout.change(&[("crates/w/gen/src/g.rs", "pub fn g() { }\n")]));
    let below = picked(checkout.change(&[("crates/u/tests/gen/src/g.rs", "pub fn g() { }\n")]));
    let outside = picked(checkout.change(&[("tools/gen/src/g.rs", "pub fn g() { }\n")]));
    let child = picked(checkout.change(&[("crates/g/src/page/inner.rs", "pub fn inner() { }\n")]));
    let deleted = picked(checkout.deleted(&["crates/g/src/part.rs"]));
    let own_oracle =
        picked(checkout.change(&[("crates/y/tests/oracle/corpus.rs", "pub fn c() { }\n")]));
    let foreign = picked(checkout.change(&[("crates/z/src/lib.rs", "pub fn z() { }\n")]));
    let spelled = picked(checkout.change(&[("crates/z/data/table.txt", "two\n")]));
    let _ = std::fs::remove_dir_all(&made);
    let (seeds, crates) = (
        paths(&["a", "k", "l", "r"]),
        paths(&["a", "e", "k", "l", "r"]),
    );
    assert_eq!(doc, (seeds.clone(), crates.clone(), paths(&["a", "r"])));
    // `u`'s nested package reads the script too: no declaration of `u` places that reader, which
    // `u`'s tests may build and run, so `u` is a seed with its dependents.
    assert_eq!(
        script,
        (paths(&["r", "u"]), paths(&["r", "u"]), paths(&["r"]))
    );
    assert_eq!(nested, (seeds.clone(), crates.clone(), paths(&["a", "r"])));
    // A test build of each mounter compiles the mounted file's `#[cfg(test)]` child.
    assert_eq!(nested_test, (seeds, crates, paths(&["a", "r"])));
    assert_eq!(generated, (paths(&["w"]), paths(&["w"]), paths(&["w"])));
    // A mount with no `../` reads its member's nested package as well.
    assert_eq!(below, (paths(&["u"]), paths(&["u"]), paths(&["u"])));
    // CONTROL: the same package under no member is no member's.
    assert_eq!(outside, (Vec::new(), Vec::new(), Vec::new()));
    assert_eq!(child, (paths(&["g"]), paths(&["g"]), Vec::new()));
    // `m` embeds `page.rs` and `q` mounts it: `page.rs`'s `mod part;` names `part.rs` beside it,
    // which only the mount compiles.
    assert_eq!(
        deleted,
        (paths(&["g", "q"]), paths(&["g", "q"]), paths(&["q"]))
    );
    // `y`'s recorder mounts the oracle file `y`'s own tests mount: no dependent of `y` reads it.
    assert_eq!(own_oracle, (paths(&["y"]), paths(&["y"]), paths(&["y"])));
    // The recorder mounts `z`'s library, whose change keeps `z` whole: `y` joins with its rows.
    assert_eq!(foreign, (paths(&["z"]), paths(&["y", "z"]), Vec::new()));
    // The recorder mounts `v`'s source that spells `z`'s changed table: `y` observes the table,
    // as `v`, which spells it, does.
    assert_eq!(
        spelled,
        (paths(&["z"]), paths(&["v", "y", "z"]), Vec::new())
    );
}

/// End to end: a foreign file a test mounts whose declarations this reader cannot settle (one
/// inside an inline module) makes its mounter a reader of any change in that file's package, and
/// one in no package makes its mounter a reader of any change at all. CONTROL: a mounted file of
/// the same package whose closure settles, and holds no changed path, makes its mounter no
/// reader, and a change outside the unsettled file's package makes its mounter none either.
#[test]
fn a_mounted_file_this_reader_cannot_settle_reads_its_whole_package() {
    let mount = |path: &str, name: &str| format!("#[path = \"{path}\"]\nmod {name};\n");
    let shim = "mod inner {\n    mod deep;\n}\n";
    let deep = "pub fn d() {}\n";
    let (made, root) = workspace(
        "atv-changed-unsettled",
        &[
            (
                "h",
                "",
                vec![
                    (
                        "src/lib.rs",
                        "pub mod other;\npub mod plain;\npub mod shim;\n".to_string(),
                    ),
                    ("src/other.rs", "pub fn o() {}\n".to_string()),
                    ("src/plain.rs", "pub fn p() {}\n".to_string()),
                    ("src/shim.rs", shim.to_string()),
                    ("src/shim/inner/deep.rs", deep.to_string()),
                ],
            ),
            (
                "j",
                "",
                vec![("tests/j.rs", mount("../../h/src/shim.rs", "shim"))],
            ),
            (
                "s",
                "",
                vec![("tests/s.rs", mount("../../h/src/plain.rs", "plain"))],
            ),
            (
                "v",
                "",
                vec![("tests/v.rs", mount("../../../tools/shim.rs", "shim"))],
            ),
            ("x", "", Vec::new()),
        ],
        &[("tools/shim.rs", shim), ("tools/shim/inner/deep.rs", deep)],
    );
    let checkout = Committed::new(&root);
    let package = picked(checkout.change(&[("crates/h/src/other.rs", "pub fn o() { }\n")]));
    let elsewhere = picked(checkout.change(&[("crates/x/src/lib.rs", "pub fn x() {}\n")]));
    let _ = std::fs::remove_dir_all(&made);
    let hjv = paths(&["h", "j", "v"]);
    assert_eq!(package, (hjv.clone(), hjv, paths(&["j", "v"])));
    let vx = paths(&["v", "x"]);
    assert_eq!(elsewhere, (vx.clone(), vx, paths(&["v"])));
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

    /// What [`resolve`] selects with `files` deleted from the working tree.
    fn deleted(&self, files: &[&str]) -> Selection {
        for file in files {
            std::fs::remove_file(self.root.join(file)).expect("the deletion");
        }
        let selection = resolve(self.root, &self.tools, &self.path, "base");
        self.git(&["checkout", "-q", "--", "."]);
        selection
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
            &BTreeSet::new(),
            &manifests(),
            &m,
            &no_packages,
            &|name| dependents(name).map(linking),
            &|_| row.clone(),
            &|_, _| false,
            &BTreeMap::new,
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
            &BTreeSet::new(),
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
            &BTreeMap::new,
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

/// A member's spec table is no member's ([`owning_member`]), so a source that spells one is no
/// namer of it: no build, include or test reads one. CONTROL: the same source spelling a source
/// file of that member is its namer.
#[test]
fn a_spelled_spec_table_makes_no_namer() {
    let repo = crate::mktemp_dir("atv-spelled-specs").expect("mktemp");
    let path_env = std::env::var_os("PATH").unwrap_or_default();
    let write = |path: &str, text: &str| {
        let file = repo.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    };
    write("crates/x/SPECS.md", "# x\n");
    write("crates/x/src/lib.rs", "pub fn x() {}\n");
    write(
        "crates/y/src/lib.rs",
        "pub const SPEC: &str = \"crates/x/SPECS.md\";\npub const LIB: &str = \"crates/x/src/lib.rs\";\n",
    );
    assert!(
        git(&repo, &path_env)
            .args(["init", "-q"])
            .status()
            .unwrap()
            .success()
    );
    let m = members(&["x", "y"]);
    let listed = listed_files(&repo, &path_env).expect("the fixture lists");
    let namers = |changed: &str| -> Vec<String> {
        with_shared_inputs(&repo, &listed, paths(&[changed]), &m)
            .unwrap()
            .named
            .into_keys()
            .collect()
    };
    let spec = namers("crates/x/SPECS.md");
    let lib = namers("crates/x/src/lib.rs");
    std::fs::remove_dir_all(&repo).unwrap();
    assert!(spec.is_empty(), "{spec:?}");
    assert_eq!(lib, ["y"]);
}

/// A build script's `rerun-if-changed` value that names a directory, with no trailing `/`, reads
/// every file under it, as Cargo watches it and the seed's own build inputs read it; and so does
/// a `#[path]` on an inline module, whose `mod y;` loads from the directory it names. CONTROL: a
/// value that names an existing file reads no path under that name, and a plain literal naming
/// the same directory still names one file.
#[test]
fn a_declared_or_mounted_directory_reads_every_file_under_it() {
    let repo = crate::mktemp_dir("atv-declared-folder").expect("mktemp");
    let path_env = std::env::var_os("PATH").unwrap_or_default();
    let write = |path: &str, text: &str| {
        let file = repo.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    };
    let script = |value: &str| {
        format!("fn main() {{\n    println!(\"cargo:rerun-if-changed={value}\");\n}}\n")
    };
    write("crates/a/assets/t.txt", "t\n");
    write("crates/a/data", "d\n");
    write("crates/b/src/y.rs", "pub fn y() {}\n");
    write("crates/k/build.rs", &script("../a/assets"));
    write("crates/j/build.rs", &script("../a/data"));
    write(
        "crates/h/src/lib.rs",
        "#[path = \"../../b/src\"]\nmod shim {\n    mod y;\n}\n",
    );
    write(
        "crates/g/src/lib.rs",
        "pub const SRC: &str = \"../../b/src\";\n",
    );
    assert!(
        git(&repo, &path_env)
            .args(["init", "-q"])
            .status()
            .unwrap()
            .success()
    );
    let m = members(&["a", "b", "g", "h", "j", "k"]);
    let listed = listed_files(&repo, &path_env).expect("the fixture lists");
    let readers = |changed: &str| -> Vec<String> {
        with_shared_inputs(&repo, &listed, paths(&[changed]), &m)
            .unwrap()
            .readers
            .into_iter()
            .collect()
    };
    let asset = readers("crates/a/assets/t.txt");
    let under_file = readers("crates/a/data/x");
    let module = readers("crates/b/src/y.rs");
    std::fs::remove_dir_all(&repo).unwrap();
    assert_eq!(asset, ["crates/k/build.rs"]);
    assert!(under_file.is_empty(), "{under_file:?}");
    assert_eq!(module, ["crates/h/src/lib.rs"]);
}

/// Whether `text`, the source at `source` of the member at `member`, a folder no other package
/// sits under, reads one of `changed` from outside it ([`literal_targets`],
/// [`changed_inputs`]).
fn shares(
    source: &Path,
    text: &str,
    member: &Path,
    changed: &BTreeSet<std::path::PathBuf>,
) -> bool {
    changed_inputs(
        &literal_targets(source, text, member, false),
        &|target| target.starts_with(member),
        changed,
    )
    .next()
    .is_some()
}

#[test]
fn a_directory_literal_seeds_its_consumer() {
    let root = Path::new("/repo");
    let member = root.join("crates/aterm-theme-pack");
    let changed = BTreeSet::from([root.join("contrib/themes/pack/Iceberg Dark.conf")]);
    let build = member.join("build.rs");
    let test = member.join("src/tests.rs");
    let folder = "let pack = dir.join(\"../../contrib/themes/pack/\");";
    assert!(shares(&build, folder, &member, &changed));
    assert!(shares(&test, folder, &member, &changed));
    let path = "let pack = dir.join(\"../../contrib/themes/pack\");";
    assert!(!shares(&build, path, &member, &changed));
    assert!(!shares(&test, path, &member, &changed));
    let elsewhere = BTreeSet::from([root.join("contrib/fonts/x.ttf")]);
    assert!(!shares(&build, folder, &member, &elsewhere));
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
                shares(source, text, &member, &changed),
                "{text} in {}",
                source.display()
            );
        }
    }

    let continued = "concat!(dir, \"/../../b/x\");";
    let relative = "include_str!(\"../../b/x\");";
    let above = BTreeSet::from([root.join("b/x")]);
    assert!(shares(&lib, relative, &member, &changed));
    assert!(shares(&lib, relative, &member, &above));
    assert!(!shares(&lib, continued, &member, &changed));
    assert!(shares(&lib, continued, &member, &above));

    let under_a_folder = BTreeSet::from([root.join("crates/b/steps/one.jsonl")]);
    let folder = "concat!(dir, \"/../b/steps/\");";
    let no_slash = "concat!(dir, \"/../b/steps\");";
    assert!(shares(&lib, folder, &member, &under_a_folder));
    assert!(!shares(&lib, no_slash, &member, &under_a_folder));

    for text in [
        "concat!(dir, \"/../c/x\");",
        "concat!(dir, \"/x/../b\");",
        "format!(\"{}/x/../b\", dir);",
        "format!(\"{}../b/x\", dir);",
        "format!(\"{{}}/../b/x\");",
    ] {
        assert!(!shares(&test, text, &member, &changed), "{text}");
    }
}

/// A literal names what rustc or Cargo resolves it to: a build script's
/// `cargo:rerun-if-changed=` value from its package, and a `#[path]` inside inline modules from a
/// folder below the source's, which takes up leading `..` segments. CONTROL: no target keeps the
/// key, a value under any other key names nothing that changed, the inline literal resolved from
/// the source's folder or the member alone names another path, and a path that stays in the
/// member is no read of another's.
#[test]
fn a_literal_names_what_rustc_and_cargo_resolve_it_to() {
    let root = Path::new("/repo");
    let member = root.join("crates/m");
    let (script, lib) = (member.join("build.rs"), member.join("src/lib.rs"));
    let changed = BTreeSet::from([root.join("crates/t/src/x.rs")]);
    for key in ["cargo:rerun-if-changed=", "cargo::rerun-if-changed="] {
        let declared = format!("println!(\"{key}../t/src/x.rs\");");
        assert!(shares(&script, &declared, &member, &changed), "{declared}");
        let targets = literal_targets(&script, &declared, &member, false);
        assert!(
            targets
                .iter()
                .all(|target| !target.path.to_string_lossy().contains("rerun-if-changed")),
            "the key is no part of any path: {targets:?}"
        );
    }
    let other = "println!(\"other:rerun-if-changed=../t/src/x.rs\");";
    assert!(
        !shares(&script, other, &member, &changed),
        "only Cargo's own key is dropped"
    );
    let inline = "mod shim {\n    #[path = \"../../../t/src/x.rs\"]\n    mod x;\n}\n";
    assert!(shares(&lib, inline, &member, &changed));
    let from_folder = literal_targets(&lib, inline, &member, false);
    assert!(
        from_folder
            .iter()
            .any(|target| target.path == root.join("crates/t/src/x.rs")),
        "{from_folder:?}"
    );
    assert_eq!(
        from_folder[..2]
            .iter()
            .map(|target| target.path.clone())
            .collect::<Vec<_>>(),
        [
            root.join("t/src/x.rs"),
            Path::new("/t/src/x.rs").to_path_buf()
        ],
        "the source's folder and the member name other paths"
    );
    let stays = "#[path = \"../src/y.rs\"]\nmod y;\n";
    assert!(!shares(
        &lib,
        stays,
        &member,
        &BTreeSet::from([member.join("src/y.rs")])
    ));
}

/// Only a `#[path]` value or an `include!` argument hands rustc a module file, whose own
/// declarations make its mounter a reader of their files; an `include_str!` or `include_bytes!`
/// argument and a run-time literal read only the file they name. A literal this reader cannot
/// tell from a module's path (in a comment, a `concat!` piece of an `include!`, or anywhere in a
/// file whose declaration paths it cannot settle) may be one. CONTROL: the same plain literal
/// beside a `cfg_attr` path is a module's.
#[test]
fn only_a_path_or_include_literal_names_a_module() {
    let root = Path::new("/repo");
    let member = root.join("crates/m");
    let lib = member.join("src/lib.rs");
    let module = |text: &str| -> Vec<bool> {
        let mut seen: Vec<bool> = literal_targets(&lib, text, &member, false)
            .iter()
            .map(|target| target.module)
            .collect();
        seen.dedup();
        seen
    };
    for (text, want) in [
        ("#[path = \"../../t/src/x.rs\"]\nmod x;\n", true),
        ("include!(\"../../t/src/x.rs\");\n", true),
        (
            "include!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/../t/src/x.rs\"));\n",
            true,
        ),
        ("include!(concat!(\"../../t/\", \"src/x.rs\"));\n", true),
        ("// See \"../../t/src/x.rs\".\n", true),
        (
            "const X: &str = include_str!(\"../../t/src/x.rs\");\n",
            false,
        ),
        (
            "const X: &[u8] = include_bytes!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/../t/src/x.rs\"));\n",
            false,
        ),
        ("const X: &str = \"../../t/src/x.rs\";\n", false),
        (
            "const X: &str = \"../../t/src/x.rs\";\n#[path = \"../../t/src/x.rs\"]\nmod x;\n",
            true,
        ),
        (
            "const X: &str = \"../../t/src/x.rs\";\n#[cfg_attr(unix, path = \"u.rs\")]\nmod u;\n",
            true,
        ),
    ] {
        assert_eq!(module(text), [want], "{text}");
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
        assert!(!shares(&lib, text, &member, &changed), "{text}");
    }
    let named = "let y = dir.join(\"../y/\");";
    assert!(shares(&lib, named, &member, &changed));
}

/// The trie finds exactly the paths `text.contains` finds, each at every offset it starts at:
/// nested and overlapping paths, a path repeated in the list, one that only a later start spells,
/// prefixes that never complete, and text around multi-byte characters.
///
/// NEGATIVE CONTROL: walk from the first byte only, or report only the deepest path of a
/// walk, and `b/c` or `a` goes missing from what this asserts; record one offset per path and
/// `aaaa/b/c` holds `a` once, not four times.
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
        let found = trie.found(text);
        assert_eq!(
            found.keys().copied().collect::<BTreeSet<_>>(),
            want,
            "{text:?}"
        );
        for (index, offsets) in &found {
            let path = paths[*index];
            let every: Vec<usize> = (0..text.len())
                .filter(|&at| text.is_char_boundary(at) && text[at..].starts_with(path))
                .collect();
            assert_eq!(offsets, &every, "{path} in {text:?}");
        }
    }
    assert_eq!(trie.found("aaaa/b/c")[&0], [0, 1, 2, 3]);
    assert_eq!(trie.found("aaaa/b/c")[&1], [3]);
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
                shares(&lib, text, &member, &changed),
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
