// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn names(list: &[&str]) -> BTreeSet<String> {
    list.iter().map(|s| (*s).to_string()).collect()
}

/// A pin that cannot serve a cell names `verify provision` on the row itself, though rustup's
/// refusal spans lines or runs long.
#[test]
fn a_cell_without_its_pin_names_provision_on_its_row_whatever_rustup_said() {
    let label = "cell win (x86_64-pc-windows-msvc)";
    for why in [
        "`rustup which --toolchain 1.97.1 cargo` failed: error: toolchain '1.97.1-aarch64-apple-\
         darwin' is not installed\nhelp: run `rustup toolchain install 1.97.1` to install it"
            .to_string(),
        format!("`rustup target list` failed: {}", "x".repeat(400)),
    ] {
        let unready = rustup::Unready {
            why,
            fix: rustup::PROVISION.into(),
        };
        let mut r = Report::new("cells");
        record_cell(
            &mut r,
            label,
            format!("{label}: 1s"),
            CellOutcome::CouldNotRun {
                status: "NO-TOOLCHAIN".into(),
                detail: vec![unready.to_string()],
            },
        );
        let result = crate::report::judge(
            crate::checks::Check::Cells,
            &[&r],
            1.0,
            "",
            "targo --unverified verify cells --cell win".into(),
            None,
        );
        let shown = crate::report::render(&result, 5);
        let first = shown.lines().next().unwrap_or_default();
        assert!(
            first.contains(&format!("(fix: {})", rustup::PROVISION)),
            "{shown}"
        );
    }
    let long = format!("{} (fix: {})", "x".repeat(400), rustup::PROVISION);
    let cut = shortened(&long, 240);
    assert_eq!(cut.chars().count(), 240, "{cut}");
    assert!(
        cut.ends_with(&format!("x… (fix: {})", rustup::PROVISION)),
        "{cut}"
    );
    let bare = "y".repeat(400);
    assert_eq!(
        shortened(&bare, 240),
        "y".repeat(240),
        "no fix: a plain cut"
    );
    assert_eq!(shortened("short (fix: z)", 240), "short (fix: z)");
}

/// A cell that could not run says why on the check's terminal line, which shows the row and
/// nothing written under it: the run's interrupt (the 2026-10-24 bare `COULD-NOT-RUN`), a
/// full disk, a toolchain's refusal.
#[test]
fn a_cell_that_could_not_run_names_its_cause_on_the_terminal_line() {
    let label = "cell linux-arm (aarch64-unknown-linux-gnu)";
    let terminal = |outcome: CellOutcome| {
        let mut r = Report::new("cells");
        record_cell(&mut r, label, format!("{label}: 143s"), outcome);
        let result = crate::report::judge(
            crate::checks::Check::Cells,
            &[&r],
            143.0,
            "",
            "targo --unverified verify cells --cell linux-arm".into(),
            None,
        );
        assert_eq!(result.outcome, Outcome::Fail(Severity::CouldNotRun));
        crate::report::render(&result, 5)
    };
    for (status, detail, cause) in [
        (
            "COULD-NOT-RUN",
            vec!["the run was interrupted while this child ran".to_string()],
            "the run was interrupted while this child ran",
        ),
        (
            "COULD-NOT-RUN",
            vec!["the child ran out of disk (No space left on device)".to_string()],
            "No space left on device",
        ),
        (
            "NO-TOOLCHAIN",
            vec!["\nrustup: toolchain '1.97.1' is not installed\nmore".to_string()],
            "rustup: toolchain '1.97.1' is not installed",
        ),
        (
            "NO-TOOLCHAIN",
            vec![
                rustup::Unready {
                    why: "`1.97.1`, the checkout's stock pin, has no x86_64-pc-windows-msvc \
                          std"
                    .into(),
                    fix: rustup::PROVISION.into(),
                }
                .to_string(),
            ],
            "has no x86_64-pc-windows-msvc std (fix: targo --unverified verify provision \
             rustup)",
        ),
    ] {
        let shown = terminal(CellOutcome::CouldNotRun {
            status: status.into(),
            detail,
        });
        let first = shown.lines().next().unwrap_or_default();
        assert!(first.contains(&format!("{label} — {status}: ")), "{shown}");
        assert!(first.contains(cause), "{shown}");
    }
    let shown = terminal(CellOutcome::CouldNotRun {
        status: "COULD-NOT-RUN".into(),
        detail: Vec::new(),
    });
    assert!(
        shown
            .lines()
            .next()
            .unwrap_or_default()
            .contains("named no cause"),
        "{shown}"
    );
}

/// A cell the run's halt keeps from starting says so on its row, rather than the resolver's
/// failure every child it spawned would meet (`UNRESOLVED`, the cause three lines under it).
/// CONTROL: a run its halt has not ended starts it.
#[test]
fn a_cell_the_runs_end_kept_from_starting_names_that_end() {
    let ctx = Ctx::new(
        PathBuf::from("/r"),
        crate::scope::Scope::workspace(),
        crate::EnvSnapshot::default(),
        PathBuf::from("/tmp"),
    )
    .with_halt(crate::sched::Halt::at_first_red("the run", |_| {}));
    assert_eq!(cell_ended(&ctx), None, "control: nothing has ended the run");
    assert!(ctx.halt.red("lint"));
    let why = cell_ended(&ctx).expect("the halt ended the run");
    assert_eq!(Some(why.as_str()), ctx.halted());
    let label = "cell linux-arm (aarch64-unknown-linux-gnu)";
    let mut r = Report::new("cells");
    record_cell(
        &mut r,
        label,
        format!("{label}: 0s"),
        CellOutcome::CouldNotRun {
            status: "NOT-STARTED".into(),
            detail: vec![why],
        },
    );
    let row = format!(
        "{label} — NOT-STARTED: {}",
        ctx.halted().unwrap_or_default()
    );
    assert_eq!(
        r.outcomes().collect::<Vec<_>>(),
        [(Outcome::Fail(Severity::CouldNotRun), row.as_str())]
    );
}

/// A dev edge holds a cone crate there for its tests only, so a cell's closure starts from what
/// the change touched. CONTROL: a cone that knows only its crates closes over all of them, which
/// took a change to a model crate through `aterm-core` to `aterm-gui` in every cell.
#[test]
fn a_cell_closes_the_cone_over_what_the_change_touched() {
    let id = |name: &str| PkgId {
        name: name.to_string(),
        version: "1.0.0".to_string(),
    };
    let inputs = BTreeMap::from([
        ("aterm-core".to_string(), BTreeSet::from([id("aterm-core")])),
        (
            "aterm-gui".to_string(),
            BTreeSet::from([id("aterm-gui"), id("aterm-core")]),
        ),
        (
            "aterm-models-misc".to_string(),
            BTreeSet::from([id("aterm-models-misc")]),
        ),
    ]);
    let normal = names(&["aterm-core", "aterm-gui", "aterm-models-misc"]);
    let cone = vec!["aterm-core".to_string(), "aterm-models-misc".to_string()];
    let unseeded = crate::scope::Scope::changed("origin/main", cone, true);
    let seeded = unseeded
        .clone()
        .with_seeds(vec!["aterm-models-misc".to_string()]);
    assert_eq!(
        wanted_in_cell(&normal, Some(&inputs), &seeded),
        names(&["aterm-core", "aterm-models-misc"])
    );
    assert_eq!(
        wanted_in_cell(&normal, Some(&inputs), &unseeded),
        names(&["aterm-core", "aterm-gui", "aterm-models-misc"])
    );
    let touched_core = unseeded.with_seeds(vec!["aterm-core".to_string()]);
    assert!(wanted_in_cell(&normal, Some(&inputs), &touched_core).contains("aterm-gui"));
    // A seed whose change only its own tests compile closes over nothing: the cell builds it
    // and no dependent. CONTROL: above, the same seed unconfined brings aterm-gui.
    let alone = crate::scope::Scope::changed("origin/main", vec!["aterm-core".to_string()], true)
        .with_alone(vec!["aterm-core".to_string()]);
    assert_eq!(
        wanted_in_cell(&normal, Some(&inputs), &alone),
        names(&["aterm-core"])
    );
}

#[test]
fn a_policy_row_is_read_strictly() {
    let row = |cols: &str| format!("# a comment\n\n{cols}\n");
    let shims = parse_policy(&row(
        "cshim\tring@0.17.14\tx86_64-pc-windows-msvc\tring_core\twhy",
    ))
    .expect("a good row");
    assert_eq!(
        shims,
        [Shim {
            package: "ring".into(),
            version: "0.17.14".into(),
            triple: "x86_64-pc-windows-msvc".into(),
            links: "ring_core".into(),
        }]
    );
    assert_eq!(
        shims[0].config(),
        "target.x86_64-pc-windows-msvc.ring_core.rustc-link-lib=[\"ring_core\"]"
    );
    for bad in [
        "excuse\tring@1\tx86_64-pc-windows-msvc\tr\twhy",
        "cshim\tring@1\tx86_64-pc-windows-msvc",
        "cshim\tring\tx86_64-pc-windows-msvc\tr\twhy",
        "cshim\tring@1\t*\tr\twhy",
        "cshim\tring@1\twasm32-unknown-unknown\tr\twhy",
    ] {
        assert!(parse_policy(&row(bad)).is_err(), "{bad}");
    }
}

/// No `--cell` is every cell but the host's, a triple another machine could host
/// included; a name picks that cell, the host's too; an unknown name is refused.
#[test]
fn a_run_checks_every_cell_but_the_hosts_or_each_cell_named() {
    let mac = "aarch64-apple-darwin";
    let all = aterm_forge::resolve::default_cells();
    let default = selected(&[], mac).expect("default");
    assert_eq!(default, foreign_cells(mac));
    let triples: Vec<&str> = default.iter().map(|c| c.triple.as_str()).collect();
    assert!(!triples.contains(&mac), "{triples:?}");
    assert!(
        triples.contains(&"x86_64-apple-darwin") && triples.contains(&"x86_64-unknown-linux-gnu"),
        "{triples:?}"
    );
    assert_eq!(default.len() + 1, all.len());
    assert_eq!(
        selected(&[], "riscv64gc-unknown-linux-gnu").expect("no cell's"),
        all
    );
    let named = selected(&["mac-arm".to_string(), "win".to_string()], mac).expect("named");
    let names: Vec<&str> = named.iter().map(|c| c.name.as_str()).collect();
    assert!(
        names.contains(&"mac-arm") && names.contains(&"win"),
        "{names:?}"
    );
    assert_eq!(named.len(), 2);
    assert!(selected(&["plan9".to_string()], mac).is_err());
}

/// A pulled run checks one cell per OS other than the host's, the host's architecture's
/// where the OS has one, and both web modules' cells, whose roots differ.
#[test]
fn a_pulled_run_checks_one_cell_per_other_os_of_the_hosts_architecture() {
    let names =
        |host: &str| -> Vec<String> { pulled_cells(host).into_iter().map(|c| c.name).collect() };
    assert_eq!(
        names("aarch64-apple-darwin"),
        ["linux-arm", "win-arm", "wasm-cpu", "wasm-gpu", "mac-x64"]
    );
    assert_eq!(
        names("x86_64-unknown-linux-gnu"),
        ["mac-x64", "win", "wasm-cpu", "wasm-gpu", "linux-arm"]
    );
    assert_eq!(
        names("x86_64-apple-darwin"),
        ["linux", "win", "wasm-cpu", "wasm-gpu", "mac-arm"]
    );
    assert_eq!(
        names("riscv64gc-unknown-linux-gnu"),
        ["mac-arm", "win", "wasm-cpu", "wasm-gpu", "linux"],
        "no cell of its architecture: each OS's first"
    );
    assert!(lint_only_when_pulled(
        "x86_64-apple-darwin",
        "aarch64-apple-darwin"
    ));
    assert!(
        !lint_only_when_pulled("aarch64-unknown-linux-gnu", "aarch64-apple-darwin"),
        "CONTROL: another OS's test targets differ in their cfgs"
    );
}

/// A pulled run's cell of the host's own OS checks no test target, and a run naming `cells`
/// checks them all. CONTROL: another OS's cell keeps its test pass in a pulled run, and a host
/// nothing can name takes nothing away.
#[test]
fn only_a_pulled_run_skips_the_test_targets_of_a_cell_of_the_hosts_os() {
    let mac = "aarch64-apple-darwin";
    let host = || Ok(mac.to_owned());
    assert!(lint_only_in_run(true, "x86_64-apple-darwin", host));
    assert!(!lint_only_in_run(false, "x86_64-apple-darwin", host));
    assert!(!lint_only_in_run(true, "aarch64-unknown-linux-gnu", host));
    assert!(!lint_only_in_run(true, "x86_64-apple-darwin", || Err(
        "no host".to_owned()
    )));
}

/// `-p` and a pulled run narrow a cell, never the browser's; a run naming `cells` with no `-p`
/// checks the whole graph, in the change cone or not, as no other check does.
#[test]
fn only_p_and_a_pulled_run_narrow_a_cell() {
    use crate::scope::Scope;
    let one = Scope::packages(&["aterm-agent".to_string()]);
    let cone = Scope::changed("origin/main", vec!["aterm-agent".into()], true);
    for triple in ["aarch64-unknown-linux-gnu", "aarch64-pc-windows-msvc"] {
        assert!(narrows(triple, &one, false), "{triple}");
        assert!(narrows(triple, &one, true), "{triple}");
        assert!(narrows(triple, &cone, true), "{triple}");
        assert!(!narrows(triple, &cone, false), "{triple}");
        assert!(!narrows(triple, &Scope::workspace(), false), "{triple}");
        assert!(!narrows(triple, &Scope::workspace(), true), "{triple}");
    }
    assert!(!narrows("wasm32-unknown-unknown", &one, false));
    assert!(!narrows("wasm32-unknown-unknown", &cone, true));
}

/// A dependent only this cell's OS has is found in its own graph, dev edges included: the root
/// `aterm-gui` reaches `aterm-x11` here though this machine's cone could not say so. NEGATIVE
/// CONTROL: a member that does not reach it is left out.
#[test]
fn a_seeds_dependents_come_from_the_cells_own_graph() {
    let ids = |names: &[&str]| {
        names
            .iter()
            .map(|name| PkgId::new(*name, "0.1.0"))
            .collect()
    };
    let graph = BTreeMap::from([
        (
            "aterm-gui".into(),
            ids(&["aterm-gui", "aterm-window", "aterm-x11"]),
        ),
        ("aterm-x11".into(), ids(&["aterm-x11"])),
        ("aterm-grid".into(), ids(&["aterm-grid", "aterm-types"])),
    ]);
    assert_eq!(
        dependents_in_graph(&graph, &names(&["aterm-x11"])),
        names(&["aterm-gui", "aterm-x11"])
    );
    assert_eq!(
        dependents_in_graph(&graph, &names(&["aterm-window"])),
        names(&["aterm-gui"])
    );
    assert!(dependents_in_graph(&graph, &names(&["nothing"])).is_empty());
}

/// A `-p` no member answers to is an error, not a green over nothing, spec spellings included.
#[test]
fn a_p_naming_no_member_is_reported() {
    use crate::scope::Scope;
    let members = names(&["aterm-agent", "libc"]);
    let packages = |named: &[&str]| {
        Scope::packages(&named.iter().map(|n| (*n).to_string()).collect::<Vec<_>>())
    };
    assert!(unknown_packages(&packages(&["aterm-agent"]), &members).is_empty());
    assert!(unknown_packages(&packages(&["libc@0.2.0"]), &members).is_empty());
    assert_eq!(
        unknown_packages(
            &packages(&["aterm-agnet", "aterm-agent", "aterm-libc"]),
            &members
        ),
        ["aterm-agnet", "aterm-libc"]
    );
    assert!(unknown_packages(&Scope::workspace(), &members).is_empty());
}

/// A narrowed lint names selected members built for the target, never host code the cell's
/// graph builds for this machine.
#[test]
fn a_narrowed_lint_names_no_host_code_and_no_non_member() {
    let own = names(&["aterm-agent", "aterm-macros", "astream-sys", "aterm-gen"]);
    let not_owed = names(&["aterm-macros", "aterm-gen"]);
    let members = names(&["aterm-agent", "aterm-macros", "aterm-gen"]);
    assert_eq!(
        narrowed_lint(
            &own,
            &not_owed,
            &members,
            &names(&[
                "aterm-agent",
                "aterm-macros",
                "astream-sys",
                "aterm-gen",
                "aterm-objc"
            ])
        ),
        names(&["aterm-agent"])
    );
    assert!(
        narrowed_lint(&own, &not_owed, &members, &names(&["aterm-macros"])).is_empty(),
        "only host code selected: the cell has nothing to compile"
    );
}

/// A member whose name the lock shares keeps its path spec, or the `-p` would be ambiguous.
#[test]
fn package_args_keep_the_selections_own_spec() {
    use crate::scope::Scope;
    let spec = "path+file:///repo/crates/x#x@1.0.0".to_string();
    let cone = Scope::changed("origin/main", vec![spec.clone()], true);
    assert_eq!(
        package_args(&names(&["x"]), &cone),
        ["-p".to_string(), spec]
    );
    assert_eq!(
        package_args(&names(&["x"]), &Scope::workspace()),
        ["-p", "x"]
    );
}

/// The oracles judge the Windows facades alone: a narrowed run owes them only for a selection
/// holding one, and one naming `cells` with no `-p` always, so an edit to the oracle itself runs them.
#[test]
fn the_win32_oracles_are_owed_unless_a_selection_rules_them_out() {
    use crate::scope::Scope;
    let pick = |cell: &str| {
        aterm_forge::resolve::select(&aterm_forge::resolve::default_cells(), &[cell.to_string()])
            .expect("the cell")
    };
    let (win, linux) = (pick("win"), pick("linux-arm"));
    let packages = |name: &str| Scope::packages(&[name.to_string()]);
    let cone = Scope::changed("origin/main", vec!["aterm-agent".into()], true);
    assert!(!owes_win32_oracles(&win, &packages("aterm-agent"), false));
    assert!(owes_win32_oracles(&win, &packages("aterm-win32"), false));
    assert!(owes_win32_oracles(
        &win,
        &packages("aterm-win32-oracle"),
        false
    ));
    assert!(!owes_win32_oracles(&win, &cone, true));
    assert!(owes_win32_oracles(&win, &cone, false));
    assert!(owes_win32_oracles(&win, &Scope::workspace(), false));
    assert!(
        owes_win32_oracles(&win, &Scope::workspace(), true),
        "a pulled run the selection widened to the whole workspace"
    );
    assert!(!owes_win32_oracles(&linux, &Scope::workspace(), false));
    // The driver is in the cone of each of its dependencies, which does not change what the
    // oracles judge. CONTROL: a change to the driver itself, to aterm-census, which reads the
    // Windows sources for it, or to a subject still owes them.
    let driver_in_cone = |seed: &str| {
        Scope::changed(
            "origin/main",
            vec![seed.to_string(), "aterm-win32-oracle".to_string()],
            true,
        )
        .with_seeds(vec![seed.to_string()])
    };
    assert!(!owes_win32_oracles(
        &win,
        &driver_in_cone("aterm-uds"),
        true
    ));
    assert!(owes_win32_oracles(
        &win,
        &driver_in_cone("aterm-uds"),
        false
    ));
    for seed in ["aterm-census", "aterm-win32-oracle", "aterm-win32"] {
        assert!(
            owes_win32_oracles(&win, &driver_in_cone(seed), true),
            "{seed}"
        );
    }
    let facade = Scope::changed("origin/main", vec!["windows-sys@0.52.0".into()], true);
    assert!(owes_win32_oracles(&win, &facade, true), "by its spec");
}

/// A pulled cell is checked for the selected crates its graph holds: the cone's or `-p`'s,
/// every one under `--all`, and none of a selection that only names crates outside it.
#[test]
fn a_pulled_cell_is_checked_only_for_selected_crates_in_its_graph() {
    use crate::scope::Scope;
    let own = names(&["aterm", "aterm-gui", "aterm-grid"]);
    let cone = Scope::changed(
        "origin/main",
        vec!["aterm".into(), "aterm-gui".into(), "aterm-objc".into()],
        true,
    );
    assert_eq!(selected_in(&own, &cone), ["aterm", "aterm-gui"]);
    assert_eq!(
        selected_in(&own, &Scope::packages(&["aterm-grid".to_string()])),
        ["aterm-grid"]
    );
    assert_eq!(selected_in(&own, &Scope::workspace()).len(), 3);
    assert!(
        selected_in(&own, &Scope::packages(&["aterm-objc".to_string()])).is_empty(),
        "a macOS-only crate is in no other OS's graph"
    );
}

#[test]
fn the_host_triple_is_read_off_the_compilers_version() {
    let vv = "rustc 1.93.0 (trust)\nbinary: rustc\nhost: aarch64-apple-darwin\nrelease: 1\n";
    assert_eq!(host_of(vv), Some("aarch64-apple-darwin"));
    assert_eq!(host_of("rustc 1.93.0\nhost: \n"), None);
    assert_eq!(host_of(""), None);
}

/// A row's version must be one the cell's graph carries.
#[test]
fn a_shim_whose_version_left_the_graph_is_dead() {
    let shim = Shim {
        package: "ring".into(),
        version: "0.17.14".into(),
        triple: "x86_64-pc-windows-msvc".into(),
        links: "ring_core".into(),
    };
    let graph = |versions: &[&str]| -> BTreeSet<PkgId> {
        versions
            .iter()
            .map(|v| PkgId {
                name: "ring".into(),
                version: (*v).into(),
            })
            .collect()
    };
    let none = BTreeSet::new();
    assert_eq!(dead_shim(&shim, &graph(&["0.17.14"]), &none), None);
    assert_eq!(
        dead_shim(&shim, &none, &graph(&["0.17.14"])),
        None,
        "a row the test targets need is live"
    );
    let why = dead_shim(&shim, &graph(&["0.17.15"]), &none).expect("dead");
    assert!(why.contains("carry 0.17.15"), "{why}");
    assert!(
        dead_shim(&shim, &none, &none)
            .expect("dead")
            .contains("no such package")
    );
}

/// A test-only C oracle is live in the test-target pass alone; a stale
/// version or an unselected member's oracle is live in neither.
#[test]
fn a_test_only_c_oracle_is_shimmed_only_in_the_test_target_pass() {
    let normal = aterm_forge::resolve::parse_tree(
        "0app v1.0.0 (/fixture/app)\n1decoder v1.0.0 (/fixture/decoder)\n\
         1normal-c v2.0.0\n",
    )
    .expect("normal dependency tree")
    .0;
    let tests = aterm_forge::resolve::parse_tree(
        "0decoder v1.0.0 (/fixture/decoder)\n1oracle v1.0.0\n2oracle-sys v3.0.0\n\
         1normal-c v2.0.0\n",
    )
    .expect("decoder's test dependency tree")
    .0;
    let row = |package: &str, version: &str| Shim {
        package: package.into(),
        version: version.into(),
        triple: "x86_64-unknown-linux-gnu".into(),
        links: "c".into(),
    };
    assert_eq!(
        shim_lanes(&row("normal-c", "2.0.0"), &normal.nodes, &tests.nodes),
        (true, true)
    );
    assert_eq!(
        shim_lanes(&row("oracle-sys", "3.0.0"), &normal.nodes, &tests.nodes),
        (false, true),
        "an oracle is needed by test targets without entering the shipped graph"
    );
    for absent in [row("oracle-sys", "2.0.0"), row("unselected-sys", "3.0.0")] {
        assert_eq!(
            shim_lanes(&absent, &normal.nodes, &tests.nodes),
            (false, false),
            "a different version or an unselected member's oracle does not make a row live"
        );
    }
}

/// A real workspace's test-target unit graph (resolved, never compiled) holds only the selected
/// members and their dev and target dependencies: no build-only or proc-macro subtree, no
/// excluded member's oracle.
#[test]
fn the_test_target_graph_has_only_selected_members_dev_and_target_dependencies() {
    let dir = aterm_tempfile::tempdir().expect("private resolver fixture");
    let root = dir.path();
    let packages = [
        (
            "app",
            "[dependencies]\ndecoder={path='../decoder'}\npm={path='../pm'}\n\
             [target.'cfg(windows)'.dependencies]\nforeign={path='../foreign',features=['generated']}\n",
        ),
        (
            "foreign",
            "[features]\ngenerated=['dep:build-only']\n[build-dependencies]\nbuild-only={path='../build-only',optional=true}\n",
        ),
        ("decoder", "[dev-dependencies]\noracle={path='../oracle'}\n"),
        (
            "oracle",
            "[dev-dependencies]\nignored-sys={path='../ignored-sys'}\n",
        ),
        (
            "ignored",
            "[dev-dependencies]\nignored-sys={path='../ignored-sys'}\n",
        ),
        ("ignored-sys", ""),
        ("build-only", ""),
        (
            "pm",
            "[lib]\nproc-macro=true\n[dependencies]\nhost-only={path='../host-only'}\n",
        ),
        ("host-only", ""),
    ];
    let members: BTreeSet<String> = packages
        .iter()
        .map(|(name, _)| (*name).to_string())
        .filter(|name| name != "host-only")
        .collect();
    std::fs::write(
        root.join("Cargo.toml"),
        format!(
            "[workspace]\nresolver='2'\nmembers={:?}\nexclude=['host-only']\n",
            members.iter().collect::<Vec<_>>()
        ),
    )
    .expect("workspace manifest");
    for (name, deps) in packages {
        let package = root.join(name);
        std::fs::create_dir_all(package.join("src")).expect("fixture source dir");
        std::fs::write(package.join("src/lib.rs"), "").expect("empty library");
        std::fs::write(
            package.join("Cargo.toml"),
            format!("[package]\nname='{name}'\nversion='1.0.0'\nedition='2021'\n{deps}"),
        )
        .expect("package manifest");
    }
    std::fs::write(
        root.join("Cargo.lock"),
        "version=4\n\
         [[package]]\nname='app'\nversion='1.0.0'\ndependencies=['decoder','foreign','pm']\n\
         [[package]]\nname='build-only'\nversion='1.0.0'\n\
         [[package]]\nname='decoder'\nversion='1.0.0'\ndependencies=['oracle']\n\
         [[package]]\nname='foreign'\nversion='1.0.0'\ndependencies=['build-only']\n\
         [[package]]\nname='host-only'\nversion='1.0.0'\n\
         [[package]]\nname='ignored'\nversion='1.0.0'\ndependencies=['ignored-sys']\n\
         [[package]]\nname='ignored-sys'\nversion='1.0.0'\n\
         [[package]]\nname='oracle'\nversion='1.0.0'\ndependencies=['ignored-sys']\n\
         [[package]]\nname='pm'\nversion='1.0.0'\ndependencies=['host-only']\n",
    )
    .expect("locked local dependency graph");
    let normal: BTreeSet<String> = ["app", "decoder", "foreign", "pm", "host-only"]
        .into_iter()
        .map(str::to_string)
        .collect();
    let (selected, excluded) = test_selection(&members, &normal, &BTreeSet::new());
    assert_eq!(selected, ["app", "decoder", "foreign", "pm"]);
    assert_eq!(excluded, ["build-only", "ignored", "ignored-sys", "oracle"]);
    std::fs::write(
        root.join("foreign/build.rs"),
        "fn main() { panic!(\"graph query executed a build script\"); }",
    )
    .expect("build script that must never run during resolution");
    for kind in ["examples", "benches"] {
        std::fs::create_dir_all(root.join("decoder").join(kind)).unwrap();
        std::fs::write(
            root.join("decoder").join(kind).join("probe.rs"),
            "fn main() {}\n",
        )
        .unwrap();
    }
    let targo = PathBuf::from(env!("CARGO"));
    let path = std::env::var_os("PATH").unwrap_or_default();
    let graph = member_units(
        &targo,
        &path,
        root,
        "x86_64-pc-windows-msvc",
        &excluded,
        true,
    )
    .expect("resolve the actual test member selection without compiling");
    let roots: BTreeSet<String> = members
        .difference(&excluded.iter().cloned().collect())
        .cloned()
        .collect();
    let dependencies =
        units::read(&graph, "x86_64-pc-windows-msvc", &roots).expect("the selected members' graph");
    let typed =
        crate::unitgraph::Graph::read(&graph, &mut crate::unitgraph::Interner::default()).unwrap();
    for kind in ["bench", "example"] {
        assert!(
            typed
                .roots
                .iter()
                .any(|&r| typed.units[r].target_kinds.iter().any(|k| k == kind)),
            "all-targets resolution must include {kind} roots"
        );
    }
    assert_eq!(
        dependents_in_graph(&dependencies.inputs, &BTreeSet::from(["oracle".to_owned()])),
        BTreeSet::from(["decoder".to_owned()]),
        "decoder's own dev oracle is not app's normal dependency"
    );
    let nodes: BTreeSet<PkgId> = dependencies
        .targets
        .values()
        .flat_map(|nodes| nodes.iter().cloned())
        .collect();
    let names: Vec<_> = nodes.iter().map(|id| id.name.as_str()).collect();
    assert_eq!(names, ["app", "decoder", "foreign", "oracle"]);
    let changed = crate::scope::Scope::changed("main", vec!["build-only".into()], true);
    assert_eq!(
        wanted_in_cell(&normal, Some(&dependencies.inputs), &changed),
        BTreeSet::from(["app".into(), "foreign".into()])
    );
    for seed in ["pm", "host-only"] {
        let changed = crate::scope::Scope::changed("main", vec![seed.into()], true);
        assert!(wanted_in_cell(&normal, Some(&dependencies.inputs), &changed).contains("app"));
    }
    assert!(
        wanted_in_cell(
            &normal,
            Some(&dependencies.inputs),
            &crate::scope::Scope::crate_only("build-only")
        )
        .is_empty()
    );
    let browser = member_units(
        &targo,
        &path,
        root,
        "wasm32-unknown-unknown",
        &excluded,
        false,
    )
    .expect("default browser targets resolve without test targets");
    let browser = units::read(&browser, "wasm32-unknown-unknown", &roots).expect("browser inputs");
    assert!(wanted_in_cell(&normal, Some(&browser.inputs), &changed).is_empty());
    assert!(
        !browser
            .targets
            .values()
            .any(|nodes| nodes.iter().any(|id| id.name == "oracle"))
    );
    let one: BTreeSet<String> = ["app".to_string()].into_iter().collect();
    assert!(units::read("", "x86_64-pc-windows-msvc", &one).is_err());
    assert!(units::read(&graph, "x86_64-pc-windows-msvc", &one).is_err());
}

/// A member a selected proc macro depends on prints as both a target and a host node.
#[test]
fn a_member_a_proc_macro_uses_is_read_as_its_target_copy() {
    let dir = aterm_tempfile::tempdir().expect("private resolver fixture");
    let root = dir.path();
    let packages = [
        (
            "app",
            "[dependencies]\npm={path='../pm'}\nshared={path='../shared'}\n",
        ),
        (
            "pm",
            "[lib]\nproc-macro=true\n[dependencies]\nshared={path='../shared'}\n",
        ),
        (
            "shared",
            "[target.'cfg(windows)'.dependencies]\nwin-only={path='../win-only'}\n\
             [target.'cfg(not(windows))'.dependencies]\nunix-only={path='../unix-only'}\n",
        ),
        ("win-only", ""),
        ("unix-only", ""),
    ];
    let members = names(&["app", "pm", "shared"]);
    std::fs::write(
        root.join("Cargo.toml"),
        format!(
            "[workspace]\nresolver='2'\nmembers={:?}\nexclude=['win-only','unix-only']\n",
            members.iter().collect::<Vec<_>>()
        ),
    )
    .expect("workspace manifest");
    for (name, deps) in packages {
        let package = root.join(name);
        std::fs::create_dir_all(package.join("src")).expect("fixture source dir");
        std::fs::write(package.join("src/lib.rs"), "").expect("empty library");
        std::fs::write(
            package.join("Cargo.toml"),
            format!("[package]\nname='{name}'\nversion='1.0.0'\nedition='2021'\n{deps}"),
        )
        .expect("package manifest");
    }
    std::fs::write(
        root.join("Cargo.lock"),
        "version=4\n\
         [[package]]\nname='app'\nversion='1.0.0'\ndependencies=['pm','shared']\n\
         [[package]]\nname='pm'\nversion='1.0.0'\ndependencies=['shared']\n\
         [[package]]\nname='shared'\nversion='1.0.0'\ndependencies=['unix-only','win-only']\n\
         [[package]]\nname='unix-only'\nversion='1.0.0'\n\
         [[package]]\nname='win-only'\nversion='1.0.0'\n",
    )
    .expect("locked local dependency graph");
    let targo = PathBuf::from(env!("CARGO"));
    let path = std::env::var_os("PATH").unwrap_or_default();
    let graph = member_units(&targo, &path, root, "x86_64-pc-windows-msvc", &[], true)
        .expect("resolve the fixture without compiling");
    let dependencies = units::read(&graph, "x86_64-pc-windows-msvc", &members)
        .expect("a member's host copy is not a second target root");
    let nodes: BTreeSet<PkgId> = dependencies
        .targets
        .values()
        .flat_map(|nodes| nodes.iter().cloned())
        .collect();
    let found: Vec<_> = nodes.iter().map(|id| id.name.as_str()).collect();
    assert_eq!(found, ["app", "shared", "win-only"]);
    assert!(
        dependencies.targets["pm"].is_empty(),
        "host roots cannot activate target shims"
    );
}

#[test]
fn package_id_names_survive_all_three_id_shapes() {
    assert_eq!(package_id_name("registry+https://x#serde@1.0.228"), "serde");
    assert_eq!(
        package_id_name("path+file:///r/crates/foo#bar@0.1.0"),
        "bar"
    );
    assert_eq!(
        package_id_name("path+file:///r/crates/serde#1.0.228"),
        "serde"
    );
    assert_eq!(
        package_id_name("serde 1.0.228 (registry+https://x)"),
        "serde"
    );
}

/// Only an artifact under the triple's own directory counts, a host unit
/// (a proc macro, a build script) never; test units only when asked.
#[test]
fn only_the_triples_own_artifacts_count() {
    let graph = names(&["a", "b", "c"]);
    let out = [
        r#"{"reason":"compiler-artifact","package_id":"path+file:///r/a#a@1","profile":{"test":false},"filenames":["/t/x86_64-pc-windows-msvc/debug/liba.rmeta"]}"#,
        r#"{"reason":"compiler-artifact","package_id":"path+file:///r/b#b@1","profile":{"test":false},"filenames":["/t/debug/deps/libb.dylib"]}"#,
        r#"{"reason":"compiler-artifact","package_id":"path+file:///r/c#c@1","profile":{"test":true},"filenames":["/t/x86_64-pc-windows-msvc/debug/c-1"]}"#,
        r#"{"reason":"compiler-message","package_id":"path+file:///r/a#a@1","message":{"level":"error","rendered":"error[E0425]: x"}}"#,
        "error: failed to run custom build command for `zstd-sys`",
        "error: could not compile `a`",
        "   Compiling a v1",
    ]
    .join("\n");
    let all = read_cargo_json(&out, "x86_64-pc-windows-msvc", &graph, false);
    assert_eq!(all.checked, names(&["a", "c"]));
    assert_eq!(
        all.errors,
        [("a".to_string(), "error[E0425]: x".to_string())]
    );
    let tests = read_cargo_json(&out, "x86_64-pc-windows-msvc", &graph, true);
    assert_eq!(tests.checked, names(&["c"]));
    assert_eq!(
        cargo_level_errors(&out),
        ["error: failed to run custom build command for `zstd-sys`"]
    );
}

/// rustc's own warnings are the cell's: an item only another OS calls
/// (`dead_code`) or an import only it uses never warns on the host, so the
/// cell's compile is where it shows.
#[test]
fn a_rustc_warning_is_a_cell_finding_and_a_clippy_one_is_the_hosts() {
    let graph = names(&["gui"]);
    let warning = |code: &str, rendered: &str| {
        format!(
            r#"{{"reason":"compiler-message","package_id":"path+file:///r/gui#gui@1","message":{{"level":"warning","code":{code},"rendered":{}}}}}"#,
            aterm_json::to_string(rendered).unwrap()
        )
    };
    let message = "warning: function `launchctl_list` is never used";
    let location = "crates/gui/src/launch.rs:42:5";
    let dead = format!(
        "\u{1b}[1m{message}\u{1b}[0m\n  --> {location}\n   |\n42 | fn launchctl_list() {{}}\n"
    );
    let out = [
        warning(r#"{"code":"dead_code","explanation":null}"#, &dead),
        warning(r#"{"code":"dead_code","explanation":null}"#, &dead),
        warning(
            r#"{"code":"unused_imports","explanation":null}"#,
            "warning: unused import: `HashSet`",
        ),
        warning(
            r#"{"code":"clippy::duration_suboptimal_units","explanation":null}"#,
            "warning: constructing a `Duration` using a smaller unit",
        ),
        warning(
            r#"{"code":"unknown_lints","explanation":null}"#,
            "warning: unknown lint: `newer_lint`",
        ),
        warning("null", "warning: 3 warnings emitted"),
    ]
    .join("\n");
    let read = read_cargo_json(&out, "aarch64-unknown-linux-gnu", &graph, false);
    assert!(read.errors.is_empty());
    assert_eq!(
        read.warnings,
        [
            ("gui".to_string(), dead.clone()),
            (
                "gui".to_string(),
                "warning: unused import: `HashSet`".to_string()
            ),
        ]
    );
    assert!(is_cell_warning(Some("unused_variables")));
    for (triple, judged) in [
        ("aarch64-unknown-linux-gnu", true),
        ("x86_64-unknown-linux-gnu", true),
        ("aarch64-apple-darwin", true),
        ("x86_64-apple-darwin", true),
        ("x86_64-pc-windows-msvc", true),
        ("aarch64-pc-windows-msvc", true),
        ("x86_64-pc-windows-gnu", false),
        ("wasm32-unknown-unknown", false),
    ] {
        assert_eq!(judges_warnings(triple), judged, "{triple}");
    }
    assert!(!is_cell_warning(Some("clippy::single_match")));
    assert!(!is_cell_warning(Some("renamed_and_removed_lints")));
    assert!(!is_cell_warning(None));
    for (tests, status) in [(false, "WARNING(2)"), (true, "TEST-TARGET-WARNING(2)")] {
        let run = exec::Run {
            case_observations: None,
            stopped_at: None,
            hung: None,
            ok: true,
            output: out.clone(),
            code: Some(0),
            spawn_error: None,
            usage: None,
        };
        let mut red = Red::default();
        read_cargo_run(&run, "aarch64-unknown-linux-gnu", &graph, tests, &mut red)
            .expect("the compiler ran");
        assert_eq!(red.status.as_deref(), Some(status));
        assert_eq!(red.detail.len(), 2);
        assert!(red.detail[0].contains("launchctl_list"), "{:?}", red.detail);
        let mut report = Report::new("cells");
        record_cell(
            &mut report,
            "cell linux-arm (aarch64-unknown-linux-gnu)",
            "1s".into(),
            CellOutcome::Failed {
                status: red.status.unwrap(),
                detail: red.detail,
            },
        );
        let (outcome, row) = report.outcomes().next().unwrap();
        assert_eq!(outcome, Outcome::Fail(Severity::GateFailed));
        assert!(row.contains(message) && row.contains(location), "{row}");
        assert!(!row.contains('\u{1b}') && !row.contains('\n'), "{row}");
        let event = crate::report::stage_event(
            0,
            Some(crate::checks::Check::Cells),
            &report,
            std::time::Duration::ZERO,
            std::time::Duration::ZERO,
            None,
        );
        let event: aterm_json::Value =
            aterm_json::from_str(event.trim_start_matches("verify-event: ")).unwrap();
        assert_eq!(event["check"].as_str(), Some("cells"));
        assert_eq!(event["decisions"][0]["detail"].as_str(), Some(row));
        let result = crate::report::judge(
            crate::checks::Check::Cells,
            &[&report],
            1.0,
            "",
            "targo --unverified verify cells --cell linux-arm".into(),
            None,
        );
        let shown = crate::report::render(&result, 5);
        let first = shown.lines().next().unwrap();
        assert!(
            first.contains(message) && first.contains(location),
            "{shown}"
        );
        assert!(report.render().contains(&dead), "full original diagnostic");
    }
    let run = exec::Run {
        case_observations: None,
        stopped_at: None,
        hung: None,
        ok: true,
        output: out.clone(),
        code: Some(0),
        spawn_error: None,
        usage: None,
    };
    let mut red = Red::default();
    read_cargo_run(&run, "x86_64-pc-windows-msvc", &graph, false, &mut red)
        .expect("the compiler ran");
    assert_eq!(
        red.status.as_deref(),
        Some("WARNING(2)"),
        "{:?}",
        red.detail
    );
    let mut red = Red::default();
    read_cargo_run(&run, "wasm32-unknown-unknown", &graph, false, &mut red)
        .expect("the compiler ran");
    assert!(red.status.is_none(), "{:?}", red.detail);
    // A pulled run names a Windows cell's test-target warnings and is not red on them. CONTROL:
    // the same warnings in its lint pass, a run naming `cells`, and a pulled Linux cell.
    let run = exec::Run {
        case_observations: None,
        stopped_at: None,
        hung: None,
        ok: true,
        output: out.clone(),
        code: Some(0),
        spawn_error: None,
        usage: None,
    };
    let read = |triple: &str, pulled: bool, tests: bool| {
        let mut red = Red {
            leaves_test_warnings: leaves_test_warnings(pulled, triple),
            ..Red::default()
        };
        read_cargo_run(&run, triple, &graph, tests, &mut red).expect("the compiler ran");
        (red.status, red.unjudged)
    };
    let win = "aarch64-pc-windows-msvc";
    let (status, unjudged) = read(win, true, true);
    assert_eq!(status, None);
    assert_eq!(unjudged.len(), 2, "{unjudged:?}");
    assert!(unjudged[0].starts_with("in `"), "{unjudged:?}");
    assert_eq!(read(win, true, false).0.as_deref(), Some("WARNING(2)"));
    assert_eq!(
        read(win, false, true).0.as_deref(),
        Some("TEST-TARGET-WARNING(2)")
    );
    assert_eq!(
        read("aarch64-unknown-linux-gnu", true, true).0.as_deref(),
        Some("TEST-TARGET-WARNING(2)")
    );
    let clippy_only = warning(
        r#"{"code":"clippy::duration_suboptimal_units","explanation":null}"#,
        "warning: constructing a `Duration` using a smaller unit",
    );
    let run = exec::Run {
        case_observations: None,
        stopped_at: None,
        hung: None,
        ok: true,
        output: clippy_only,
        code: Some(0),
        spawn_error: None,
        usage: None,
    };
    let mut red = Red::default();
    read_cargo_run(&run, "aarch64-unknown-linux-gnu", &graph, false, &mut red)
        .expect("the compiler ran");
    assert!(red.status.is_none(), "{:?}", red.detail);
}

/// Both passes can leave every owed artifact before cargo itself fails.
#[test]
fn complete_cell_artifacts_do_not_make_a_failed_child_green() {
    let artifact = r#"{"reason":"compiler-artifact","package_id":"path+file:///r/app#app@1","profile":{"test":true},"filenames":["/t/x86_64-pc-windows-msvc/debug/app"]}"#;
    let graph = names(&["app"]);
    let run = |code| exec::Run {
        case_observations: None,
        stopped_at: None,
        hung: None,
        ok: code == Some(0),
        output: artifact.to_string(),
        code,
        spawn_error: None,
        usage: None,
    };
    for tests in [false, true] {
        let mut red = Red::default();
        let read = read_cargo_run(
            &run(Some(0)),
            "x86_64-pc-windows-msvc",
            &graph,
            tests,
            &mut red,
        )
        .expect("a successful compiler ran");
        assert_eq!(read.checked, graph);
        assert!(red.status.is_none());
        for code in [Some(17), None] {
            let mut red = Red::default();
            let read = read_cargo_run(
                &run(code),
                "x86_64-pc-windows-msvc",
                &graph,
                tests,
                &mut red,
            )
            .expect("the compiler answered a real failure");
            assert_eq!(read.checked, graph, "the fixture has complete coverage");
            assert!(
                red.status.is_some(),
                "all artifacts concealed the child failure: tests={tests}, code={code:?}"
            );
        }
    }
}

#[test]
fn a_failed_cell_keeps_its_diagnostics_and_machine_refusals_distinct() {
    let graph = names(&["app"]);
    for tests in [false, true] {
        for diagnostic in [
            r#"{"reason":"compiler-message","package_id":"path+file:///r/app#app@1","message":{"level":"error","rendered":"error[E0425]: missing"}}"#,
            "error: no matching package named `missing` found",
        ] {
            let run = exec::Run {
                case_observations: None,
                stopped_at: None,
                hung: None,
                ok: false,
                output: diagnostic.to_string(),
                code: Some(101),
                spawn_error: None,
                usage: None,
            };
            let mut red = Red::default();
            read_cargo_run(&run, "x86_64-pc-windows-msvc", &graph, tests, &mut red)
                .expect("a compiler or dependency finding, not a machine refusal");
            assert!(red.status.is_some());
            assert_eq!(red.detail.len(), 1, "keep the actual compiler diagnostic");
            assert!(red.detail[0].contains("missing"));
        }
        for (spawn_error, output) in [
            (Some("compiler could not start".to_string()), String::new()),
            (None, exec::ENOSPC_SENTENCE.to_string()),
        ] {
            let run = exec::Run {
                case_observations: None,
                stopped_at: None,
                hung: None,
                ok: false,
                output,
                code: None,
                spawn_error,
                usage: None,
            };
            let mut red = Red::default();
            assert!(matches!(
                read_cargo_run(&run, "x86_64-pc-windows-msvc", &graph, tests, &mut red),
                Err(CellOutcome::CouldNotRun { .. })
            ));
            assert!(red.status.is_none());
        }
    }
}

#[test]
fn only_successful_unselected_git_manifest_diagnostics_are_nonfatal() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let git = dir.path().join("checkout");
    let selected = git.join("selected/Cargo.toml");
    let inherited = git.join("Cargo.toml");
    let unused = git.join("fixture/Cargo.toml");
    let outside = dir.path().join("outside/Cargo.toml");
    for path in [&selected, &inherited, &unused, &outside] {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "fixture").unwrap();
    }
    let context = GitDiscovery {
        cwd: dir.path().to_owned(),
        roots: BTreeSet::from([git.canonicalize().unwrap()]),
        manifests: BTreeSet::from([
            selected.canonicalize().unwrap(),
            inherited.canonicalize().unwrap(),
        ]),
    };
    let graph = names(&["app"]);
    let artifact = r#"{"reason":"compiler-artifact","package_id":"path+file:///r/app#app@1","profile":{"test":false},"filenames":["/t/aarch64-unknown-linux-gnu/app.rmeta"]}"#;
    let diagnostic = |path: &Path| {
        format!(
            "error: invalid float, expected `inf`\n --> {}:9:7\n",
            path.display()
        )
    };
    let finish = "{\"reason\":\"build-finished\",\"success\":true}";
    let evaluate = |ok: bool, path: &Path, tail: &str| {
        let run = exec::Run {
            case_observations: None,
            stopped_at: None,
            hung: None,
            ok,
            code: Some(if ok { 0 } else { 101 }),
            spawn_error: None,
            usage: None,
            output: format!("{}{artifact}\n{tail}\n", diagnostic(path)),
        };
        let mut red = Red::default();
        let read = read_cargo_run_with_discovery(
            &run,
            "aarch64-unknown-linux-gnu",
            &graph,
            false,
            &mut red,
            Some(&context),
        )
        .unwrap();
        (read, red)
    };
    let (read, red) = evaluate(true, &unused, finish);
    assert_eq!(read.checked, graph);
    assert!(red.status.is_none());
    assert_eq!(red.discovery.len(), 1);
    assert!(red.discovery[0].contains("fixture/Cargo.toml"));
    for (ok, path, tail) in [
        (false, &unused, finish),
        (true, &selected, finish),
        (true, &inherited, finish),
        (true, &outside, finish),
        (true, &unused, ""),
        (
            true,
            &unused,
            "{\"reason\":\"build-finished\",\"success\":false}",
        ),
    ] {
        assert!(evaluate(ok, path, tail).1.status.is_some());
    }
    let compiler_error = format!(
        "{finish}\n{}",
        r#"{"reason":"compiler-message","package_id":"path+file:///r/app#app@1","message":{"level":"error","rendered":"error[E0001]: real error"}}"#
    );
    assert!(evaluate(true, &unused, &compiler_error).1.status.is_some());
}

#[test]
fn member_names_come_out_of_cargo_metadata() {
    let cells = aterm_forge::resolve::default_cells();
    let json = r#"{"packages":[{"name":"aterm","metadata":null},{"name":"arrayvec"}],"version":1}"#;
    let members = parse_members(json, &cells).expect("names");
    assert_eq!(members.names, names(&["aterm", "arrayvec"]));
    assert!(members.roots.is_empty());
    assert!(parse_members("not json", &cells).is_err());
    assert!(parse_members(r#"{"packages":[]}"#, &cells).is_err());
}

/// A member names the cells that check it beside their root; a name that
/// is no cell, or a key that is no list, is refused rather than dropped.
#[test]
fn a_member_names_the_cells_that_check_it_beside_their_root() {
    let cells = aterm_forge::resolve::default_cells();
    let cell = |name: &str| cells.iter().find(|c| c.name == name).unwrap().clone();
    let json = r#"{"packages":[
            {"name":"aterm","metadata":null},
            {"name":"guest","metadata":{"aterm":{"cells":["linux-arm"]}}},
            {"name":"both","metadata":{"aterm":{"cells":["linux-arm","win"]}}},
            {"name":"docs-only","metadata":{"docs":{"rs":{}}}}
        ]}"#;
    let members = parse_members(json, &cells).expect("members");
    assert_eq!(members.roots_of(&cell("linux-arm")), ["both", "guest"]);
    assert_eq!(members.roots_of(&cell("win")), ["both"]);
    assert_eq!(
        members.roots_of(&cell("win-arm")),
        Vec::<String>::new(),
        "a cell no member names checks its root alone"
    );
    assert_eq!(
        lint_packages(&cell("linux-arm"), &members.roots_of(&cell("linux-arm"))),
        ["-p", "aterm", "-p", "both", "-p", "guest"]
    );
    assert_eq!(lint_packages(&cell("win-arm"), &[]), ["-p", "aterm"]);
    let unknown = r#"{"packages":[{"name":"g","metadata":{"aterm":{"cells":["linux-musl"]}}}]}"#;
    let why = parse_members(unknown, &cells).unwrap_err();
    assert!(why.contains("`g`") && why.contains("linux-musl"), "{why}");
    let not_a_list = r#"{"packages":[{"name":"g","metadata":{"aterm":{"cells":"linux-arm"}}}]}"#;
    assert!(parse_members(not_a_list, &cells).is_err());
}

/// An extra root's graph is joined onto the cell's: the test pass selects it and its own
/// members, the host side stays empty, and a cell no member names still excludes it.
#[test]
fn an_extra_root_joins_the_cells_graph_and_its_test_pass() {
    let dir = aterm_tempfile::tempdir().expect("private resolver fixture");
    let root = dir.path();
    let packages = [
        ("app", "[dependencies]\nshared={path='../shared'}\n"),
        (
            "guest",
            "[dependencies]\nshared={path='../shared'}\nguest-only={path='../guest-only'}\n\
             [package.metadata.aterm]\ncells=['linux-arm']\n",
        ),
        ("shared", ""),
        ("guest-only", ""),
        ("mac-only", ""),
    ];
    let listed: Vec<&str> = packages.iter().map(|(n, _)| *n).collect();
    std::fs::write(
        root.join("Cargo.toml"),
        format!("[workspace]\nresolver='2'\nmembers={listed:?}\n"),
    )
    .expect("workspace manifest");
    for (name, deps) in packages {
        let package = root.join(name);
        std::fs::create_dir_all(package.join("src")).expect("fixture source dir");
        std::fs::write(package.join("src/lib.rs"), "").expect("empty library");
        std::fs::write(
            package.join("Cargo.toml"),
            format!("[package]\nname='{name}'\nversion='1.0.0'\nedition='2021'\n{deps}"),
        )
        .expect("package manifest");
    }
    std::fs::write(
        root.join("Cargo.lock"),
        "version=4\n\
         [[package]]\nname='app'\nversion='1.0.0'\ndependencies=['shared']\n\
         [[package]]\nname='guest'\nversion='1.0.0'\ndependencies=['guest-only','shared']\n\
         [[package]]\nname='guest-only'\nversion='1.0.0'\n\
         [[package]]\nname='mac-only'\nversion='1.0.0'\n\
         [[package]]\nname='shared'\nversion='1.0.0'\n",
    )
    .expect("locked local dependency graph");
    let targo = PathBuf::from(env!("CARGO"));
    let path = std::env::var_os("PATH").unwrap_or_default();
    let metadata = aterm_uds::spawnfd::command(&targo)
        .args([
            "metadata",
            "--no-deps",
            "--locked",
            "--offline",
            "--format-version",
            "1",
        ])
        .arg("--manifest-path")
        .arg(root.join("Cargo.toml"))
        .current_dir(root)
        .env("PATH", &path)
        .output_owned()
        .expect("targo metadata");
    assert!(metadata.status.success(), "{metadata:?}");
    let cells = aterm_forge::resolve::default_cells();
    let members = parse_members(&String::from_utf8_lossy(&metadata.stdout), &cells)
        .expect("the fixture's members");
    let cargo = aterm_forge::resolve::Cargo::pinned(&targo, &path);
    let resolve = |name: &str, package: &str| {
        let cell = Cell {
            package: package.to_string(),
            ..cells.iter().find(|c| c.name == name).unwrap().clone()
        };
        let extra = members.roots_of(&cell);
        let mut log = String::new();
        let (graph, _) = resolve_roots(&cargo, root, &cell, &extra, &mut log)
            .expect("resolve the fixture without compiling");
        (extra, graph)
    };

    let (extra, graph) = resolve("linux-arm", "app");
    assert_eq!(extra, ["guest"]);
    let normal: BTreeSet<String> = graph.nodes.iter().map(|id| id.name.clone()).collect();
    assert_eq!(normal, names(&["app", "guest", "guest-only", "shared"]));
    assert_eq!(host_side(&graph, |_| false), BTreeSet::new());
    let (selected, excluded) = test_selection(&members.names, &normal, &BTreeSet::new());
    assert_eq!(selected, ["app", "guest", "guest-only", "shared"]);
    assert_eq!(excluded, ["mac-only"]);

    // Control: a cell the guest does not name leaves it out.
    let (extra, graph) = resolve("win-arm", "app");
    assert!(extra.is_empty());
    let normal: BTreeSet<String> = graph.nodes.iter().map(|id| id.name.clone()).collect();
    assert_eq!(normal, names(&["app", "shared"]));
    let (_, excluded) = test_selection(&members.names, &normal, &BTreeSet::new());
    assert_eq!(excluded, ["guest", "guest-only", "mac-only"]);
}

/// The members the graph carries are selected; the rest are excluded,
/// except a patched one, whose exclusion would drop its patch.
#[test]
fn the_test_pass_selects_the_graph_and_never_excludes_a_patch() {
    let members = names(&["a", "b", "arrayvec", "z"]);
    let graph = names(&["a", "b", "serde"]);
    let (selected, excluded) = test_selection(&members, &graph, &names(&["arrayvec"]));
    assert_eq!(selected, ["a", "b"]);
    assert_eq!(excluded, ["z"]);
}

/// A crate only a proc macro reaches compiles for the host with it and is
/// not owed a target artifact; one a non-proc-macro path also reaches is.
#[test]
fn a_crate_only_proc_macros_reach_is_host_side() {
    let id = |n: &str| PkgId::new(n, "1.0.0");
    let mut g = Graph {
        root: id("root"),
        ..Graph::default()
    };
    for (from, to) in [
        ("root", "lib"),
        ("lib", "pm"),
        ("pm", "only_pm"),
        ("pm", "shared"),
        ("root", "shared"),
    ] {
        g.nodes.insert(id(from));
        g.nodes.insert(id(to));
        g.edges.entry(id(from)).or_default().insert(id(to));
    }
    let host = super::host_side(&g, |p| p.name == "pm");
    assert_eq!(
        host.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
        ["only_pm", "pm"]
    );
    assert!(super::host_side(&g, |_| false).is_empty());
}

#[test]
fn the_patch_table_is_read_from_the_root_manifest() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("atv-cells-")
        .suffix(".noindex")
        .tempdir()
        .expect("scratch");
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[workspace]\n[patch.crates-io]\n# x\narrayvec = { path = \"a\" }\n\"libc\" = { path = \"l\" }\n\
         windows-sys-052 = { package = \"windows-sys\", path = \"w\" }\n[profile.dev]\nopt = 1\n",
    )
    .expect("write");
    assert_eq!(
        patched_crates(dir.path()).expect("read"),
        names(&["arrayvec", "libc", "windows-sys"])
    );
    assert!(patched_crates(&dir.path().join("absent")).is_err());
    assert_eq!(
        renamed_package(r#" { path = "crates/package-x", package = "real" }"#),
        Some("real")
    );
    assert_eq!(renamed_package(r#" { path = "crates/x" }"#), None);
}

/// A CELL'S EVENT NAMES BOTH OF ITS CARGO CALLS, in the order it made them, each with its own
/// wall, CPU and peak memory, so a stage that claims memory once for every cell can be read
/// call by call; a call whose tree no read found says `null`, never zero; and every reader of
/// the events file knows the kind. NEGATIVE CONTROL: a cell that reached no cargo call names
/// none.
#[test]
fn a_cells_event_names_both_of_its_cargo_calls() {
    let cell = Cell {
        name: "linux-arm".into(),
        triple: "aarch64-unknown-linux-gnu".into(),
        package: "aterm".into(),
    };
    let ran = |usage| exec::Run {
        ok: true,
        code: Some(0),
        usage,
        ..exec::Run::default()
    };
    let used = exec::TreeUsage {
        cpu: Duration::from_secs(90),
        peak_bytes: 5 << 30,
        ..exec::TreeUsage::default()
    };
    let mut calls = Vec::new();
    assert!(noted(&mut calls, LINT_CALL, || ran(Some(used))).ok);
    assert!(noted(&mut calls, ALL_TARGETS_CALL, || ran(None)).ok);
    let read = |line: String| {
        let json = line.strip_prefix("verify-event: ").expect("an event line");
        let crate::event_log::Observation::Known { kind, value } =
            crate::event_log::observation(json)
        else {
            panic!("{json} is no event its readers know");
        };
        assert_eq!(kind, crate::event_log::EventKind::CellFinished);
        value
    };
    let event = read(cell_event(&cell, Duration::from_secs(120), &calls));
    assert_eq!(event["event"].as_str(), Some("cell-finished"));
    assert_eq!(event["cell"].as_str(), Some("linux-arm"));
    assert_eq!(event["triple"].as_str(), Some("aarch64-unknown-linux-gnu"));
    assert_eq!(event["wall_s"].as_f64(), Some(120.0));
    let named: Vec<_> = event["calls"]
        .as_array()
        .expect("its calls")
        .iter()
        .map(|call| {
            (
                call["call"].as_str().expect("a name"),
                call["cpu_s"].as_f64(),
                call["peak_bytes"].as_u64(),
            )
        })
        .collect();
    assert_eq!(
        named,
        [
            ("lint", Some(90.0), Some(5 << 30)),
            ("all-targets", None, None),
        ]
    );
    for (call, noted) in event["calls"].as_array().unwrap().iter().zip(&calls) {
        assert_eq!(call["wall_s"].as_f64(), Some(noted.wall.as_secs_f64()));
    }
    let idle = read(cell_event(&cell, Duration::ZERO, &[]));
    assert_eq!(idle["calls"].as_array().map(Vec::len), Some(0));
}

/// A cell's cargo builds in its own directory of the cells lane, capped at
/// the lane's jobs and never above the caller's own `CARGO_BUILD_JOBS`, and
/// keeps no incremental state.
#[test]
fn a_cells_cargo_runs_in_the_cells_lane_under_its_job_cap() {
    let env_of = |caller: Option<&str>| {
        let ctx = Ctx::new(
            PathBuf::from("/repo"),
            crate::Scope::workspace(),
            crate::EnvSnapshot {
                cargo_build_jobs: caller.map(Into::into),
                ..crate::EnvSnapshot::default()
            },
            PathBuf::from("/tmp"),
        );
        let toolchain = rustup::Toolchain {
            name: "1.97.1".into(),
            cargo: PathBuf::from("/r/bin/cargo"),
            rustc: PathBuf::from("/r/bin/rustc"),
            clippy: true,
        };
        let target = lane_dir(&ctx, Lane::CellsTarget)
            .expect("a side lane")
            .join("win");
        let cargo = CellCargo {
            ctx: &ctx,
            triple: "x86_64-pc-windows-msvc",
            toolchain: &toolchain,
            cwd: Path::new("/tmp/cells-win"),
            target_dir: &target,
            shim_args: &[],
        };
        let packages = ["-p".to_string(), "aterm-agent".to_string()];
        let narrowed = cargo.selected_targets(&packages);
        assert!(
            narrowed.args.iter().any(|a| a == "--all-targets")
                && narrowed.args.iter().any(|a| a == "aterm-agent")
                && !narrowed.args.iter().any(|a| a == "--workspace"),
            "{:?}",
            narrowed.args
        );
        // The lint compiles what ships, with debug assertions off, and the pass over test
        // targets, which call the seams they gate, keeps them on.
        let release = |cmd: &Cmd| cmd.args.iter().any(|a| a == "--release");
        assert!(release(&cargo.lint(&packages)), "the lint is debug-off");
        assert!(!release(&narrowed), "CONTROL: test targets are debug-on");
        let cmd = cargo.all_targets(&[]);
        let get = |k: &str| {
            cmd.envs
                .iter()
                .find(|(name, _)| name == k)
                .map(|(_, v)| v.to_string_lossy().into_owned())
        };
        assert_eq!(get("CARGO_INCREMENTAL").as_deref(), Some("0"));
        (get("CARGO_TARGET_DIR"), get("CARGO_BUILD_JOBS"))
    };
    let dir = Some(
        [
            "/repo",
            aterm_toolchain::build_cache::DEFAULT_TARGET,
            "verify",
            ".ubd",
            "cells.noindex",
            "win",
        ]
        .iter()
        .collect::<PathBuf>()
        .to_string_lossy()
        .into_owned(),
    );
    assert_eq!(env_of(None), (dir.clone(), Some("4".to_string())));
    assert_eq!(env_of(Some("2")), (dir.clone(), Some("2".to_string())));
    assert_eq!(env_of(Some("16")), (dir, Some("4".to_string())));
}

/// Either Windows cell brings both windows-sys oracles, once each; a run
/// with no Windows cell runs neither.
#[test]
fn a_windows_cell_brings_both_windows_sys_oracles_built_in_the_cells_lane() {
    let all = aterm_forge::resolve::default_cells();
    let pick = |names: &[&str]| {
        aterm_forge::resolve::select(
            &all,
            &names.iter().map(|n| (*n).to_string()).collect::<Vec<_>>(),
        )
        .expect("forge cells")
    };
    assert!(runs_win32_oracles(&pick(&["win"])));
    assert!(runs_win32_oracles(&pick(&["win-arm"])));
    assert!(runs_win32_oracles(&pick(&["linux-arm", "win", "win-arm"])));
    assert!(!runs_win32_oracles(&pick(&[
        "linux-arm",
        "wasm-cpu",
        "mac-x64"
    ])));
    assert!(
        runs_win32_oracles(&foreign_cells("aarch64-apple-darwin")),
        "the default run"
    );
    assert_eq!(
        WIN32_ORACLES.map(|(verb, _)| verb),
        ["oracle", "oracle-052"]
    );

    let root = std::path::absolute("/repo").expect("absolute");
    let ctx = Ctx::new(
        root.clone(),
        crate::Scope::workspace(),
        crate::EnvSnapshot::default(),
        PathBuf::from("/tmp"),
    );
    let cmd = win32_oracle_cmd(&ctx, "oracle-052", "1.97.1").expect("absolute paths");
    let argv = cmd.argv();
    let oracle_bin = root
        .join(aterm_toolchain::build_cache::DEFAULT_TARGET)
        .join("verify")
        .join(".ubd")
        .join("cells.noindex")
        .join("win32-oracle-bin")
        .display()
        .to_string();
    let root_str = root.display().to_string();
    assert_eq!(
        argv[1..],
        [
            "--unverified",
            "run",
            "-q",
            "-p",
            "aterm-win32-oracle",
            "--target-dir",
            oracle_bin.as_str(),
            "--",
            "oracle-052",
            "--root",
            root_str.as_str(),
            "--stock",
            "1.97.1",
        ]
    );
    assert_eq!(cmd.cwd.as_deref(), Some(root.as_path()));
    assert!(
        !cmd.envs.iter().any(|(k, _)| k == "CARGO_TARGET_DIR"),
        "the oracle's own Windows builds keep their per-checkout directories"
    );
}

/// An oracle that decided nothing says why on its last `could not run`
/// line, which becomes the row's reason; a finding's output has none.
#[test]
fn a_win32_oracle_that_could_not_run_names_its_reason() {
    assert_eq!(
        win32_oracle_not_run_reason(
            "== x86_64-pc-windows-msvc\nwin32 oracle could not run: no cached \
             windows-sys0.61.2; use --upstream\n"
        )
        .as_deref(),
        Some("no cached windows-sys0.61.2; use --upstream")
    );
    assert_eq!(
        win32_oracle_not_run_reason("windows-sys 0.52 oracle could not run: stock cargo failed\n")
            .as_deref(),
        Some("stock cargo failed")
    );
    assert_eq!(
        win32_oracle_not_run_reason("  FAIL plant-dll was not caught\nwin32 oracle: FAIL\n"),
        None
    );
}

#[test]
fn the_lint_denies_the_bans_and_the_browser_cells_only_the_clock() {
    assert!(lint_flags("x86_64-pc-windows-msvc").contains(&"clippy::disallowed_types"));
    let wasm = lint_flags("wasm32-unknown-unknown");
    assert!(wasm.contains(&"clippy::disallowed_methods"));
    assert!(!wasm.contains(&"clippy::disallowed_types"));
}

#[test]
fn patched_crates_and_renamed_package_handle_subtables_quotes_and_comments() {
    assert_eq!(
        renamed_package("{ path = \"crates/package = 'bad'\", package = 'real-pkg' }"),
        Some("real-pkg")
    );
    assert_eq!(
        renamed_package("{ my_package = \"wrong\" } # package = \"comment\""),
        None
    );
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[patch.'crates-io'] # inline table\n\
         foo = { path = \"crates/foo\" }\n\
         alias = { path = \"crates/bar\", package = 'bar-real' }\n\
         [patch.crates-io.sub]\n\
         path = \"crates/sub\"\n\
         [patch.\"crates-io\".sub_alias]\n\
         path = \"crates/sub2\"\n\
         package = \"sub-real\"\n",
    )
    .expect("write");
    assert_eq!(
        patched_crates(dir.path()).expect("patched"),
        names(&["foo", "bar-real", "sub", "sub-real"])
    );
}
