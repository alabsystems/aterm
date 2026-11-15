// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// Fixed readings for every field a check of [`crate::checks::CHECKS`] reads but the external
/// ones, and every local attribute; a reseal reads `moved`'s value where it names the field.
#[derive(Clone, Debug, Default)]
pub(crate) struct Bench {
    pub(crate) read: BTreeMap<String, String>,
    pub(crate) moved: BTreeMap<String, String>,
}

impl Bench {
    /// Every field read, each as its own digest.
    pub(crate) fn every() -> Self {
        let mut read = BTreeMap::new();
        for spec in &crate::checks::CHECKS {
            for name in fields_of(spec.check) {
                if !external(&name) {
                    read.insert(name.clone(), digest([name.as_str(), "1"]));
                }
            }
        }
        for name in LOCAL {
            read.insert(name.to_owned(), digest([name, "1"]));
        }
        Self {
            read,
            moved: BTreeMap::new(),
        }
    }

    /// This bench with `name` reading `value`.
    pub(crate) fn with(mut self, name: &str, value: &str) -> Self {
        self.read.insert(name.to_owned(), value.to_owned());
        self
    }
}

impl Machine for Bench {
    fn read(&mut self, _: &Ctx, name: &str) -> Result<String, String> {
        self.read
            .get(name)
            .cloned()
            .ok_or_else(|| format!("the bench reads no {name}"))
    }

    fn reseal(&mut self, ctx: &Ctx, name: &str) -> Result<String, String> {
        match self.moved.get(name) {
            Some(why) => Err(why.clone()),
            None => self.read(ctx, name),
        }
    }
}

/// The tree a fixture run judges.
pub(crate) fn tree() -> Tree {
    Tree {
        commit: "c".repeat(40),
        tree: "d".repeat(40),
        base: "b".repeat(40),
        changed: digest(["crates/aterm-gui/src/lib.rs"]),
        clean: true,
    }
}

/// Pin `ctx`'s one read of the engine to a fixture: its baseline reading `baseline` and its
/// registry `registry`.
pub(crate) fn pin(ctx: &Ctx, baseline: &str, registry: &str) {
    ctx.engine
        .set(Ok(crate::engine::Files {
            sha: "e".repeat(40),
            shown: "the fixture engine".to_owned(),
            baseline: Ok(baseline.to_owned()),
            registry: Ok(registry.to_owned()),
        }))
        .expect("the engine is read once");
}

/// The dev set and every check it pulls, as a cone through aterm-gui and aterm-wasm plans it.
const PLANNED: [Check; 9] = [
    Check::ReleasedShim,
    Check::Fmt,
    Check::Census,
    Check::ExportScan,
    Check::Lint,
    Check::Test,
    Check::Forge,
    Check::Cells,
    Check::Appkit,
];

fn ctx() -> Ctx {
    Ctx::new(
        PathBuf::from("/repo"),
        crate::Scope::changed("origin/main", vec!["aterm-gui".into()], true),
        crate::EnvSnapshot::default(),
        PathBuf::from("/tmp"),
    )
    .with_checks(&PLANNED)
    .with_cells_pulled(true)
}

/// Each check's key in a plan of `ctx` over `bench`.
fn keys(ctx: &Ctx, bench: &Bench) -> BTreeMap<Check, String> {
    let plan = Plan::make(ctx, &mut bench.clone(), Ok(tree()));
    assert!(plan.unsealed.is_empty(), "{:?}", plan.unsealed);
    plan.required
        .iter()
        .map(|row| (row.check, row.key.clone()))
        .collect()
}

/// The checks whose keys differ between `a` and `b`.
fn changed(a: &BTreeMap<Check, String>, b: &BTreeMap<Check, String>) -> Vec<Check> {
    a.iter()
        .filter(|(check, key)| b.get(*check) != Some(*key))
        .map(|(check, _)| *check)
        .collect()
}

/// EACH SEAL FIELD CHANGES ONLY THE KEYS OF THE CHECKS THAT READ IT: a changed Trust or Apple
/// toolchain, host or Git changes every key; the stock pin only that of cells, which compiles
/// with it; a tool only the checks that need it; the engine's baseline export-scan's
/// and census's, its registry export-scan's alone; a new merged public tag released-shim's (the
/// tags it judges by); the installed release released-shim's.
/// A local attribute changes the local digest and no key. NEGATIVE CONTROL: identical inputs give
/// identical seals and keys, and the local digest is the same.
#[test]
fn each_seal_field_changes_only_what_reads_it() {
    let base = Bench::every();
    let pinned = |baseline: &str, registry: &str| {
        let ctx = ctx();
        pin(&ctx, baseline, registry);
        ctx
    };
    let at = pinned("# rows\n", "[repos]\n");
    let before = keys(&at, &base);
    assert_eq!(
        before,
        keys(&pinned("# rows\n", "[repos]\n"), &base),
        "NEGATIVE CONTROL: identical inputs key alike"
    );
    let all: Vec<Check> = PLANNED.to_vec();
    let sorted = |mut checks: Vec<Check>| {
        checks.sort_unstable();
        checks
    };
    for (field, readers) in [
        (TOOLCHAIN, all.clone()),
        (APPLE, all.clone()),
        (TRIPLE, all.clone()),
        (GIT, all.clone()),
        (STOCK, vec![Check::Cells]),
        ("tools/trustfmt", vec![Check::Fmt]),
        ("tools/gitleaks", vec![Check::ExportScan, Check::Test]),
        ("tools/targo-tippy", vec![Check::Lint]),
        (RELEASE, vec![Check::ReleasedShim]),
    ] {
        let moved = keys(&at, &base.clone().with(field, "sha256:another"));
        assert_eq!(
            changed(&before, &moved),
            sorted(readers),
            "{field} changes the keys of what reads it, and only theirs"
        );
    }
    let baseline = keys(&pinned("# rows\nquokka\n", "[repos]\n"), &base);
    assert_eq!(
        changed(&before, &baseline),
        [Check::Census, Check::ExportScan],
        "the engine's baseline"
    );
    let registry = keys(&pinned("# rows\n", "[repos]\nx = 1\n"), &base);
    assert_eq!(
        changed(&before, &registry),
        [Check::ExportScan],
        "the engine's registry"
    );
    let tag = keys(&at, &base.clone().with(RELEASE_TAGS, "sha256:a-newer-tag"));
    assert_eq!(
        changed(&before, &tag),
        [Check::ReleasedShim],
        "a new merged public tag"
    );
    let plan = Plan::make(&at, &mut base.clone(), Ok(tree()));
    for name in LOCAL {
        let local = Plan::make(
            &at,
            &mut base.clone().with(name, "sha256:another"),
            Ok(tree()),
        );
        assert_ne!(local.local_digest, plan.local_digest, "{name} is local");
        assert_eq!(
            local.required, plan.required,
            "{name} changes no key and no seal"
        );
    }
    assert_eq!(
        Plan::make(&at, &mut base.clone(), Ok(tree())).local_digest,
        plan.local_digest,
        "NEGATIVE CONTROL: the same local attributes digest alike"
    );
}

/// THE ENVIRONMENT DIGEST READS WHAT CHILDREN INHERIT AND NOTHING THAT SHAPES ONLY THIS SESSION:
/// `RUSTFLAGS`, `CARGO_PROFILE_TEST_DEBUG_ASSERTIONS` and `SDKROOT` each change it, and
/// `CARGO_BUILD_JOBS`, a different `TMPDIR` and a `PATH` with an entry repeated do not.
/// NEGATIVE CONTROL: the same variables in another order digest alike.
#[test]
fn the_environment_digest_leaves_out_shaping_and_the_session() {
    let vars = |pairs: &[(&str, &str)]| -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).into(), (*value).into()))
            .collect()
    };
    let base = [
        ("HOME", "/Users//a"),
        ("PATH", "/usr/bin:/bin"),
        ("TMPDIR", "/tmp/a/"),
    ];
    let digest_of = |extra: &[(&str, &str)]| {
        let mut all = base.to_vec();
        all.retain(|(name, _)| !extra.iter().any(|(e, _)| e == name));
        all.extend_from_slice(extra);
        environment(vars(&all))
    };
    let plain = digest_of(&[]);
    let mut reversed = base.to_vec();
    reversed.reverse();
    assert_eq!(environment(vars(&reversed)), plain, "NEGATIVE CONTROL");
    for read in [
        ("RUSTFLAGS", "-Cdebuginfo=0"),
        ("CARGO_PROFILE_TEST_DEBUG_ASSERTIONS", "false"),
        ("SDKROOT", "/sdk"),
    ] {
        assert_ne!(digest_of(&[read]), plain, "{} is read", read.0);
    }
    for shaping in [
        ("CARGO_BUILD_JOBS", "4"),
        ("TMPDIR", "/tmp/b/"),
        ("PATH", "/usr/bin:/bin:/usr/bin"),
        ("SSH_CONNECTION", "1 2 3 4"),
    ] {
        assert_eq!(digest_of(&[shaping]), plain, "{} is left out", shaping.0);
    }
}

/// THE CONFIGURATION DIGEST READS THE CONFIGURATION OUTSIDE T: an ancestor's `clippy.toml`, the
/// Cargo home's `config.toml` and an ancestor's `rustfmt.toml` each change it. NEGATIVE CONTROL:
/// read twice over the same files, it is the same.
#[test]
fn the_configuration_outside_the_tree_is_a_local_attribute() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let base = dir.path().canonicalize().expect("canonical");
    let root = base.join("outer/repo");
    std::fs::create_dir_all(&root).expect("mkdir");
    let env = crate::EnvSnapshot {
        home: base.join("home"),
        cargo_home: Some(base.join("cargo")),
        xdg_config_home: Some(base.join("xdg")),
        ..crate::EnvSnapshot::default()
    };
    let ctx = Ctx::new(root, crate::Scope::workspace(), env, base.join("scratch"));
    let first = config(&ctx).expect("the configuration reads");
    assert_eq!(config(&ctx).expect("again"), first, "NEGATIVE CONTROL");
    let mut seen = vec![first];
    for (path, text) in [
        ("outer/clippy.toml", "msrv = \"1.80\"\n"),
        ("cargo/config.toml", "[build]\njobs = 2\n"),
        ("outer/rustfmt.toml", "max_width = 80\n"),
        ("xdg/rustfmt/rustfmt.toml", "edition = \"2024\"\n"),
    ] {
        let at = base.join(path);
        std::fs::create_dir_all(at.parent().expect("parent")).expect("mkdir");
        std::fs::write(&at, text).expect("write");
        let now = config(&ctx).expect("the configuration reads");
        assert!(!seen.contains(&now), "{path} changes the configuration");
        seen.push(now);
    }
}

/// THE CHECKOUT DIGEST IS THE CHECKOUT'S OWN: two clones read two digests, and a worktree of a
/// clone a third (its path is its own, its common directory its clone's), so no checkout on one
/// Mac reuses another's passes. NEGATIVE CONTROL: one checkout read twice is the same.
#[test]
fn each_checkout_digests_its_own_path_and_clone() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let base = dir.path().canonicalize().expect("canonical");
    let git = |root: &Path, args: &[&str]| {
        crate::judged::git(root, args).unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    };
    for clone in ["one", "two"] {
        git(&base, &["init", "-q", clone]);
        let root = base.join(clone);
        git(
            &root,
            &[
                "-c",
                "user.name=Andrew Yates",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "base",
            ],
        );
    }
    git(&base.join("one"), &["worktree", "add", "-q", "../worktree"]);
    let read = |name: &str| {
        // The tool PATH the run hands git: a launcher git finds its helpers on it.
        let env = crate::EnvSnapshot {
            path: std::env::var_os("PATH").unwrap_or_default(),
            ..crate::EnvSnapshot::default()
        };
        let ctx = Ctx::new(
            base.join(name),
            crate::Scope::workspace(),
            env,
            base.join("scratch"),
        );
        checkout(&ctx).expect("the checkout reads")
    };
    let one = read("one");
    assert_eq!(read("one"), one, "NEGATIVE CONTROL");
    let (two, worktree) = (read("two"), read("worktree"));
    assert_ne!(two, one, "another clone");
    assert_ne!(worktree, one, "a worktree of the clone");
    assert_ne!(worktree, two);
}

/// A PASS WHOSE INPUTS MOVED DURING THE RUN WAS NOT SEALED THROUGH: a field that no longer reads
/// as the plan read it makes `run-resealed` say `moved` and `sealed_through: false`, and the
/// controller's own fold of the two events ([`crate::event_log::Judged::sealed_through`]) agrees,
/// as it does for a reseal that claims to hold over a seal the plan did not key. NEGATIVE
/// CONTROL: an unchanged reseal is sealed through, by both.
#[test]
fn a_pass_whose_inputs_moved_during_the_run_is_not_sealed_through() {
    let ctx = ctx();
    pin(&ctx, "# rows\n", "[repos]\n");
    let bench = Bench::every();
    let plan = Plan::make(&ctx, &mut bench.clone(), Ok(tree()));
    let folded = |resealed: &Resealed| {
        let text = format!(
            "{}{}",
            crate::report::run_planned(&plan),
            crate::report::run_resealed(resealed)
        );
        crate::event_log::Judged::from_events(&text, 0, 1)
    };
    let held = plan.reseal(&ctx, &mut bench.clone());
    assert!(held.sealed_through && held.moved.is_empty(), "{held:?}");
    assert!(folded(&held).sealed_through(), "NEGATIVE CONTROL");
    for field in [TOOLCHAIN, "tools/gitleaks", ENV] {
        let mut moving = bench.clone();
        moving
            .moved
            .insert(field.to_owned(), "a root changed".to_owned());
        let moved = plan.reseal(&ctx, &mut moving);
        assert!(!moved.sealed_through, "{field}: {moved:?}");
        assert_eq!(moved.moved, [field], "{field}");
        assert!(!folded(&moved).sealed_through(), "{field}");
    }
    let mut claimed = held;
    claimed.seals[0].1 = digest(["another seal"]);
    assert!(
        !folded(&claimed).sealed_through(),
        "a reseal over a seal the plan did not key is no reseal of it"
    );
}

/// An unreadable field takes the run's nonce: two plans over it key apart, and say why.
/// NEGATIVE CONTROL: with the field read, they key alike.
#[test]
fn an_unread_field_matches_no_other_run() {
    let ctx = ctx();
    pin(&ctx, "# rows\n", "[repos]\n");
    let mut unread = Bench::every();
    unread.read.remove(APPLE);
    let a = Plan::make(&ctx, &mut unread.clone(), Ok(tree()));
    let b = Plan::make(&ctx, &mut unread.clone(), Ok(tree()));
    assert!(
        a.unsealed.iter().any(|(field, _)| field == APPLE),
        "{:?}",
        a.unsealed
    );
    assert!(
        a.required
            .iter()
            .zip(&b.required)
            .all(|(a, b)| a.key != b.key)
    );
    let read = Bench::every();
    assert_eq!(
        Plan::make(&ctx, &mut read.clone(), Ok(tree())).required,
        Plan::make(&ctx, &mut read.clone(), Ok(tree())).required,
        "NEGATIVE CONTROL"
    );
}
