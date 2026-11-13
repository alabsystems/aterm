// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn compiled_slices_without_the_unsigned_build_cannot_complete_a_real_gate() {
    let ended = aterm_json::json!({
        "event":"ended", "slices":[
            {"triple":"aarch64-apple-darwin"},
            {"triple":"x86_64-apple-darwin"},
        ],
    });
    assert!(require_unsigned_build(&ended, "candidate").is_err());
    let digest = "0".repeat(64);
    for (success, commit, archs, kind, valid) in [
        (
            true,
            "candidate",
            "x86_64 arm64",
            "real-unsigned-build-finishing",
            true,
        ),
        (
            false,
            "candidate",
            "x86_64 arm64",
            "real-unsigned-build-finishing",
            false,
        ),
        (
            true,
            "another",
            "x86_64 arm64",
            "real-unsigned-build-finishing",
            false,
        ),
        (
            true,
            "candidate",
            "arm64",
            "real-unsigned-build-finishing",
            false,
        ),
        (true, "candidate", "x86_64 arm64", "surrogate", false),
    ] {
        let ended = aterm_json::json!({"event":"ended", "postlink":{
            "success":success, "commit":commit, "architectures":archs, "kind":kind,
            "snapshot_digest":digest, "macho_sha256":digest, "bundle_digest":digest,
            "content_digest":digest, "inputs":digest,
        }});
        assert_eq!(require_unsigned_build(&ended, "candidate").is_ok(), valid);
    }
}

#[test]
fn matched_build_returns_change_only_the_interleaved_handoff_policy() {
    let opts = parse(
        [
            "--mode",
            "matched",
            "--verify",
            "same",
            "--before-verify",
            "same",
            "--build-return",
            "matched",
        ]
        .into_iter()
        .map(str::to_owned),
    )
    .unwrap();
    assert_eq!(
        order(&opts.mode)
            .into_iter()
            .map(|take| opts.build_return.for_take(take))
            .collect::<Vec<_>>(),
        [
            BuildReturn::Retained,
            BuildReturn::Early,
            BuildReturn::Early,
            BuildReturn::Retained
        ]
    );
    let frozen = parse(
        frozen_arguments(&opts, &opts.verify)
            .into_iter()
            .skip(1)
            .map(|value| value.into_string().unwrap()),
    )
    .unwrap();
    assert_eq!(frozen, opts);
    assert!(
        parse(
            [
                "--mode",
                "matched",
                "--verify",
                "after",
                "--before-verify",
                "before",
                "--build-return",
                "matched"
            ]
            .into_iter()
            .map(str::to_owned)
        )
        .is_err()
    );
    assert!(
        parse(
            [
                "--mode",
                "shared",
                "--verify",
                "same",
                "--build-return",
                "matched"
            ]
            .into_iter()
            .map(str::to_owned)
        )
        .is_err()
    );
}

#[cfg(target_os = "macos")]
#[test]
fn real_tree_children_and_cleanup_share_the_release_layout() {
    fn context(repo: &Path, env: aterm_verify::EnvSnapshot) -> aterm_verify::Ctx {
        let tools = aterm_verify::Toolchain {
            stage2_dir: PathBuf::new(),
            targo: PathBuf::new(),
            trustdoc: PathBuf::new(),
            tippy: None,
            trustfmt: None,
            borrowed: Vec::new(),
            refused: None,
            store_bin: None,
            demoted: None,
            lock: None,
            lock_refusal: None,
            lock_override: None,
        };
        aterm_verify::Ctx::new_with_tools(
            repo.to_owned(),
            aterm_verify::Scope::workspace(),
            env,
            repo.join("target/.ubd/layout-test.noindex"),
            tools,
        )
    }
    fn layout(ctx: &aterm_verify::Ctx) -> aterm_json::Value {
        aterm_json::json!({
            "target": ctx.target_dir,
            "lint": aterm_verify::plan::lane_dir(ctx, aterm_verify::plan::Lane::TippyTarget),
            "index": ctx.target_dir.join("verify/test-executables.json"),
            "build_dir": ctx.main_build_dir,
        })
    }
    const CHILD_ROOT: &str = "ATERM_PILOT_LAYOUT_TEST_ROOT";
    const PREFIX: &str = "pilot-layout-context: ";
    if let Some(repo) = std::env::var_os(CHILD_ROOT) {
        let ctx = context(Path::new(&repo), aterm_verify::EnvSnapshot::capture());
        println!("{PREFIX}{}", layout(&ctx));
        return;
    }
    let temp = aterm_tempfile::tempdir().unwrap();
    let temp = aterm_verify::physical(temp.path());
    let home = temp.join("home");
    let repo = home.join("source");
    std::fs::create_dir_all(home.join("Library/Caches")).unwrap();
    aterm_uds::private_file::create_private_dir(&repo).unwrap();
    std::fs::write(repo.join("Cargo.toml"), "[workspace]\nmembers=[]\n").unwrap();
    let environment = aterm_verify::EnvSnapshot {
        home: home.clone(),
        ..Default::default()
    };
    let ctx = context(&repo, environment.clone());
    let expected = layout(&ctx);
    assert!(ctx.main_build_dir.is_some());
    assert_ne!(
        ctx.main_build_dir,
        aterm_toolchain::build_cache::target_build_config(&ctx.target_dir)
    );
    let index = ctx.target_dir.join("verify/test-executables.json");
    std::fs::create_dir_all(index.parent().unwrap()).unwrap();
    std::fs::write(&index, b"selected index").unwrap();
    let inherited = temp.join("inherited");
    let other_index = inherited.join("verify/test-executables.json");
    std::fs::create_dir_all(other_index.parent().unwrap()).unwrap();
    std::fs::write(&other_index, b"unrelated index").unwrap();
    let child = || {
        let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "qualify_pilot::tests::real_tree_children_and_cleanup_share_the_release_layout",
                "--nocapture",
            ])
            .current_dir(&repo)
            .env(CHILD_ROOT, &repo)
            .env("HOME", &home)
            .env("PATH", "");
        for name in aterm_verify::CARGO_LAYOUT_VARS {
            command.env(name, &inherited);
        }
        command
    };
    let run = |mut command: aterm_uds::spawnfd::Command| {
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .find_map(|line| line.split_once(PREFIX).map(|(_, value)| value.to_owned()))
            .map(|line| aterm_json::from_str::<aterm_json::Value>(&line).unwrap())
            .expect("child reported its actual context")
    };
    let unmodified = run(child());
    assert_ne!(
        unmodified, expected,
        "inherited layout is a real negative control"
    );
    let mut projected = child();
    apply_real_tree_environment(&mut projected, &environment);
    assert_eq!(run(projected), expected);
    let options = parse(
        ["--verify", "verify", "--mode", "tree"]
            .into_iter()
            .map(str::to_owned),
    )
    .unwrap();
    assert_eq!(
        prepare_real_tree(&repo, &options, &environment).unwrap(),
        ctx.target_dir
    );
    assert!(!index.exists());
    assert_eq!(std::fs::read(other_index).unwrap(), b"unrelated index");
    let old_lint =
        aterm_verify::plan::lane_dir(&ctx, aterm_verify::plan::Lane::TippyTarget).unwrap();
    std::fs::create_dir_all(&old_lint).unwrap();
    std::fs::write(old_lint.join("held-output"), b"prior build").unwrap();
    for label in ["first", "second"] {
        let at = temp.join(label);
        std::fs::create_dir(&at).unwrap();
        let mut isolated = environment.clone();
        isolate_side_lanes(&at, &mut isolated).unwrap();
        let selected = context(&repo, isolated.clone());
        assert_eq!(selected.target_dir, ctx.target_dir);
        let mut projected = child();
        projected.env(aterm_verify::SIDE_LANES_ENV, &old_lint);
        apply_real_tree_environment(&mut projected, &isolated);
        assert_eq!(run(projected), layout(&selected), "{label}");
        assert_eq!(
            prepare_real_tree(&repo, &options, &isolated).unwrap(),
            ctx.target_dir
        );
        assert_eq!(
            std::fs::read(old_lint.join("held-output")).unwrap(),
            b"prior build"
        );
        let lint =
            aterm_verify::plan::lane_dir(&selected, aterm_verify::plan::Lane::TippyTarget).unwrap();
        std::fs::create_dir(&lint).unwrap();
        std::fs::write(lint.join("held-output"), b"current build").unwrap();
        assert!(
            prepare_real_tree(&repo, &options, &isolated).is_err(),
            "a cold target cannot be reused"
        );
        assert_eq!(
            std::fs::read(lint.join("held-output")).unwrap(),
            b"current build"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_serial_budget_drains_build_before_tree_gets_its_compiler_tokens() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let shares =
        aterm_capacity::BranchShares::new(&dir.path().join("jobs"), 6, 1, [true, true]).unwrap();
    let barrier = std::sync::Mutex::new(Some(shares.reserve_initial().unwrap()));
    let tokens = |tree| {
        let mut tokens = Vec::new();
        while let Some(token) = shares.pool(tree).try_acquire().unwrap() {
            tokens.push(token);
        }
        tokens
    };
    assert!(
        serial_before_tree(
            false,
            || panic!("no serial transfer"),
            || -> Result<(), String> { panic!("no serial wait") },
            || panic!("no drain")
        )
        .unwrap()
        .is_none()
    );
    assert!(tokens(false).is_empty());
    assert!(tokens(true).is_empty());
    let result = serial_before_tree(
        true,
        || {
            barrier
                .lock()
                .unwrap()
                .take()
                .unwrap()
                .release_to_build()
                .map_err(|e| e.to_string())
        },
        || {
            let held = tokens(false);
            assert_eq!(
                held.len(),
                5,
                "negative control: withholding the barrier strands build"
            );
            assert!(
                tokens(true).is_empty(),
                "tree must not run beside serial slice clients"
            );
            drop(held);
            Ok(17)
        },
        || shares.serial_bundle_drained(),
    )
    .unwrap();
    assert_eq!(result, Some(17));
    assert!(tokens(false).is_empty());
    assert_eq!(
        tokens(true).len(),
        5,
        "negative control: failing to drain build strands tree"
    );
    shares.finish(0);
    shares.finish(1);
    shares.finish(2);
    assert_eq!(tokens(true).len(), 6, "the full grant is conserved");
}

#[test]
fn serial_readiness_has_its_own_label_and_full_reservation() {
    let event = aterm_json::json!({
        "event":"slice-memory-ready", "mode":"serial", "held":true,
        "label":crate::slice_units::SERIAL_PARENT, "mem":21,
    });
    assert!(ready_shape(&event, 21));
    assert!(!ready_shape(&event, 22));
    let mut mutant = event.clone();
    mutant
        .as_object_mut()
        .unwrap()
        .insert("mode".into(), "shared".into());
    assert!(!ready_shape(&mutant, 21));
}
/// An opt-in failing test on a clean mainline tree. Only the measured tree child receives
/// this marker path; export and slice children keep their ordinary passing environments.
#[test]
fn planted_red_measurement_control() {
    let Some(path) = std::env::var_os("ATERM_QUALIFY_RED_MARKER") else {
        return;
    };
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    let mut marker = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .unwrap();
    writeln!(
        marker,
        "{}",
        aterm_json::json!({
            "at_unix_ns":at.as_nanos().to_string(), "pid":std::process::id(),
            "test":"qualify_pilot::tests::planted_red_measurement_control",
        })
    )
    .unwrap();
    assert!(
        std::hint::black_box(false),
        "planted qualify red at {} ns",
        at.as_nanos()
    );
}

#[test]
fn only_the_planted_named_red_with_a_complete_verdict_is_an_expected_red() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path();
    let events = root.join("tree.events");
    let red = aterm_json::json!({"event":"check-finished","check":"test","outcome":"FAIL",
        "findings_total":1,"findings":["-p aterm-release --test it -- qualify_pilot::tests::planted_red_measurement_control"]});
    let finished = aterm_json::json!({"event":"run-finished","code":1});
    std::fs::write(&events, format!("{red}\n{finished}\n")).unwrap();
    assert!(!planted_red_only(root, &events));
    std::fs::write(
        root.join("planted-red.json"),
        aterm_json::json!({"test":"qualify_pilot::tests::planted_red_measurement_control"})
            .to_string(),
    )
    .unwrap();
    assert!(planted_red_only(root, &events));
    std::fs::write(&events, format!("{red}\n")).unwrap();
    assert!(!planted_red_only(root, &events));
    std::fs::write(&events, format!("{red}\n{red}\n{finished}\n")).unwrap();
    assert!(!planted_red_only(root, &events));
    std::fs::write(&events, format!("{red}\n{finished}\nbroken\n")).unwrap();
    assert!(!planted_red_only(root, &events));
}
#[cfg(unix)]
#[test]
fn observing_a_private_fifo_does_not_spend_its_tokens() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("test.jobs");
    let pool = aterm_capacity::Jobserver::new(&path, 3).unwrap();
    assert_eq!(fifo_snapshot(&path)["available_tokens"].as_u64(), Some(3));
    let token = pool.try_acquire().unwrap().unwrap();
    assert_eq!(fifo_snapshot(&path)["available_tokens"].as_u64(), Some(2));
    assert_eq!(fifo_snapshot(&path)["available_tokens"].as_u64(), Some(2));
    drop(token);
    assert_eq!(fifo_snapshot(&path)["available_tokens"].as_u64(), Some(3));
    assert!(fifo_snapshot(&dir.path().join("missing"))["available_tokens"].is_null());
}
#[test]
fn matched_takes_use_one_priority_and_real_export_cannot_take_a_surrogate_wall() {
    let options = parse(
        [
            "--verify",
            "after",
            "--before-verify",
            "before",
            "--mode",
            "matched",
            "--tree-work",
            "release",
            "--export-work",
            "real",
            "--plant-red",
            "test",
            "--warmup",
            "full",
        ]
        .into_iter()
        .map(str::to_owned),
    )
    .unwrap();
    assert_eq!(order(&options.mode), ["before", "after", "after", "before"]);
    assert!(options.real_export);
    assert!(options.planted_red);
    assert!(options.warmup);
    assert_eq!(
        options.take("warmup").unwrap().verify,
        PathBuf::from("before")
    );
    assert_eq!(
        options.take("after").unwrap().verify,
        PathBuf::from("after")
    );
    assert!(options.warm_slices);
    assert_eq!(options.export_seconds(), Some(0));
    let mut frozen = options.clone();
    frozen.verify = PathBuf::from("/frozen/aterm-verify");
    let arguments = frozen_arguments(&options, &frozen.verify);
    let decoded = parse(
        arguments
            .into_iter()
            .skip(1)
            .map(|argument| argument.into_string().unwrap()),
    )
    .unwrap();
    assert_eq!(decoded, frozen);
    let alone = ["--verify", "after", "--mode", "alone"];
    assert!(
        !parse(alone.into_iter().map(str::to_owned))
            .unwrap()
            .warm_slices
    );
    let warm = parse(
        alone
            .into_iter()
            .chain(["--slice-cache", "warm"])
            .map(str::to_owned),
    )
    .unwrap();
    assert!(warm.warm_slices);
    assert!(
        parse(
            ["--verify", "after", "--mode", "matched",]
                .into_iter()
                .map(str::to_owned)
        )
        .is_err()
    );
    assert!(
        parse(
            [
                "--verify",
                "after",
                "--mode",
                "shared",
                "--export-work",
                "real",
                "--export-seconds",
                "763",
            ]
            .into_iter()
            .map(str::to_owned)
        )
        .is_err()
    );
}

#[test]
fn a_full_warmup_is_green_before_the_four_red_handoff_takes() {
    let options = parse(
        "--verify same --before-verify same --mode matched --tree-work release \
         --export-work real --plant-red test --build-return matched --warmup full"
            .split_whitespace()
            .map(str::to_owned),
    )
    .unwrap();
    let order = options.order();
    assert_eq!(order, ["warmup", "before", "after", "after", "before"]);
    let takes: Vec<_> = order
        .iter()
        .map(|mode| options.take(mode).unwrap())
        .collect();
    assert!(!takes[0].planted_red);
    assert_eq!(takes[0].build_return, BuildReturn::Retained);
    assert!(
        takes
            .iter()
            .all(|take| !take.warmup && take.mode == "chain")
    );
    assert!(takes[1..].iter().all(|take| take.planted_red));
    assert_eq!(
        takes[1..]
            .iter()
            .map(|take| take.build_return)
            .collect::<Vec<_>>(),
        [
            BuildReturn::Retained,
            BuildReturn::Early,
            BuildReturn::Early,
            BuildReturn::Retained
        ]
    );
    for words in [
        "--verify same --mode alone --warmup full",
        "--verify same --before-verify same --mode matched --warmup full",
        "--verify same --before-verify same --mode matched --tree-work release --export-work real --warmup full --warmup full",
    ] {
        assert!(parse(words.split_whitespace().map(str::to_owned)).is_err());
    }
}

#[test]
fn matched_queued_tree_shares_preserve_the_workload_and_frozen_verifier() {
    let options = parse(
        "--verify same --before-verify same --mode matched --tree-work release \
         --export-work real --plant-red test --build-return early --warmup full \
         --queued-tree-share matched"
            .split_whitespace()
            .map(str::to_owned),
    )
    .unwrap();
    let takes: Vec<_> = options
        .order()
        .iter()
        .map(|take| options.take(take).unwrap())
        .collect();
    assert_eq!(
        takes
            .iter()
            .map(|take| take.queued_tree_share)
            .collect::<Vec<_>>(),
        [
            QueuedTreeShare::Retained,
            QueuedTreeShare::Retained,
            QueuedTreeShare::Resume,
            QueuedTreeShare::Resume,
            QueuedTreeShare::Retained
        ]
    );
    assert!(
        takes
            .iter()
            .all(|take| take.build_return == BuildReturn::Early
                && take.real_export
                && take.tree_work == TreeShape::Release
                && take.verify == options.verify)
    );
    assert!(!takes[0].planted_red);
    assert!(takes[1..].iter().all(|take| take.planted_red));
    let path = Path::new("frozen-verifier");
    let frozen = parse(
        frozen_arguments(&options, path)
            .into_iter()
            .skip(1)
            .map(|argument| argument.into_string().unwrap()),
    )
    .unwrap();
    assert_eq!(frozen.verify.as_path(), path);
    assert_eq!(frozen.before.as_deref(), Some(path));
    assert_eq!(frozen.queued_tree_share, QueuedTreeShare::Matched);
    assert!(frozen.warmup && frozen.planted_red);
}

#[test]
fn matched_lending_changes_only_the_cpu_policy_on_one_frozen_verifier() {
    let options = parse(
        "--verify same --before-verify same --mode matched --tree-work release \
         --export-work real --plant-red test --slice-cache cold --tree-cache warm \
         --build-return early --warmup full --queued-tree-share lend-matched --cores 15"
            .split_whitespace()
            .map(str::to_owned),
    )
    .unwrap();
    let takes: Vec<_> = options
        .order()
        .iter()
        .map(|name| options.take(name).unwrap())
        .collect();
    assert_eq!(
        takes
            .iter()
            .map(|take| take.queued_tree_share)
            .collect::<Vec<_>>(),
        [
            QueuedTreeShare::Resume,
            QueuedTreeShare::Resume,
            QueuedTreeShare::Lend,
            QueuedTreeShare::Lend,
            QueuedTreeShare::Resume,
        ]
    );
    assert!(takes.iter().all(|take| take.verify == options.verify
        && take.cores == Some(15)
        && take.build_return == BuildReturn::Early
        && take.real_export
        && take.tree_work == TreeShape::Release
        && !take.warm_slices
        && !take.cold_tree));
    assert!(!takes[0].planted_red);
    assert!(takes[1..].iter().all(|take| take.planted_red));
    let path = Path::new("frozen-verifier");
    let frozen = parse(
        frozen_arguments(&options, path)
            .into_iter()
            .skip(1)
            .map(|argument| argument.into_string().unwrap()),
    )
    .unwrap();
    assert_eq!(frozen.verify.as_path(), path);
    assert_eq!(frozen.before.as_deref(), Some(path));
    assert_eq!(frozen.queued_tree_share, QueuedTreeShare::LendingMatched);
    assert_eq!(frozen.cores, Some(15));
}

#[test]
fn lending_comparison_balances_historical_current_and_lent_full_gates() {
    let options = parse(
        "--verify current --before-verify historical --mode matched --tree-work release \
         --export-work real --plant-red test --slice-cache cold --tree-cache warm \
         --build-return early --warmup full --queued-tree-share lend-compare --cores 15"
            .split_whitespace()
            .map(str::to_owned),
    )
    .unwrap();
    let order = options.order();
    assert_eq!(
        order,
        [
            "warmup", "before", "current", "lent", "lent", "current", "before"
        ]
    );
    for (name, policy, verifier) in [
        ("warmup", QueuedTreeShare::Retained, "historical"),
        ("before", QueuedTreeShare::Retained, "historical"),
        ("current", QueuedTreeShare::Resume, "current"),
        ("lent", QueuedTreeShare::Lend, "current"),
    ] {
        let take = options.take(name).unwrap();
        assert_eq!(take.queued_tree_share, policy);
        assert_eq!(take.verify, Path::new(verifier));
        assert_eq!(take.planted_red, name != "warmup");
        assert_eq!(take.cores, Some(15));
        assert_eq!(take.tree_work, TreeShape::Release);
        assert_eq!(take.build_return, BuildReturn::Early);
        assert!(take.real_export && !take.warm_slices && !take.cold_tree);
    }
    for words in [
        "--verify current --before-verify historical --mode series --queued-tree-share lend-compare",
        "--verify current --before-verify historical --mode matched --build-return retained --queued-tree-share lend-compare",
    ] {
        assert!(parse(words.split_whitespace().map(str::to_owned)).is_err());
    }
    let frozen = parse(
        frozen_arguments(&options, Path::new("frozen-current"))
            .into_iter()
            .skip(1)
            .map(|argument| argument.into_string().unwrap()),
    )
    .unwrap();
    assert_eq!(frozen.queued_tree_share, QueuedTreeShare::LendingCompare);
    assert_eq!(frozen.verify, Path::new("frozen-current"));
    assert_eq!(frozen.before.as_deref(), Some(Path::new("historical")));
}

#[test]
fn queued_tree_share_controls_cannot_confuse_the_two_verifiers() {
    for words in [
        "--verify after --before-verify before --mode matched --queued-tree-share matched",
        "--verify after --before-verify before --mode matched --queued-tree-share lend-matched",
        "--verify same --mode shared --queued-tree-share lend-matched",
        "--verify same --mode shared --queued-tree-share matched",
        "--verify same --mode shared --queued-tree-share resume --queued-tree-share retained",
        "--verify same --mode shared --queued-tree-share unknown",
    ] {
        assert!(parse(words.split_whitespace().map(str::to_owned)).is_err());
    }
    let options = parse(
        "--verify same --mode shared"
            .split_whitespace()
            .map(str::to_owned),
    )
    .unwrap();
    assert_eq!(options.queued_tree_share, QueuedTreeShare::Lend);
    let retained = parse(
        "--verify same --mode shared --queued-tree-share retained"
            .split_whitespace()
            .map(str::to_owned),
    )
    .unwrap();
    assert_eq!(retained.queued_tree_share, QueuedTreeShare::Retained);
}

#[test]
fn three_policies_share_one_workload_and_balanced_order() {
    let options = parse(
        "--verify current --before-verify old --mode matched --tree-work release \
         --export-work real --plant-red test --slice-cache cold --tree-cache warm \
         --warmup full --queued-tree-share compare --cores 15"
            .split_whitespace()
            .map(str::to_owned),
    )
    .unwrap();
    assert_eq!(
        options.order(),
        [
            "warmup", "before", "current", "resumed", "resumed", "current", "before"
        ]
    );
    let takes: Vec<_> = options
        .order()
        .iter()
        .map(|name| options.take(name).unwrap())
        .collect();
    assert_eq!(
        takes
            .iter()
            .map(|take| take.verify.as_path())
            .collect::<Vec<_>>(),
        [
            Path::new("old"),
            Path::new("old"),
            Path::new("current"),
            Path::new("current"),
            Path::new("current"),
            Path::new("current"),
            Path::new("old")
        ]
    );
    assert_eq!(
        takes
            .iter()
            .map(|take| take.queued_tree_share)
            .collect::<Vec<_>>(),
        [
            QueuedTreeShare::Retained,
            QueuedTreeShare::Retained,
            QueuedTreeShare::Retained,
            QueuedTreeShare::Resume,
            QueuedTreeShare::Resume,
            QueuedTreeShare::Retained,
            QueuedTreeShare::Retained
        ]
    );
    assert!(takes.iter().all(|take| take.real_export
        && take.tree_work == TreeShape::Release
        && !take.warm_slices
        && !take.cold_tree
        && take.cores == Some(15)
        && take.build_return == BuildReturn::Early));
    assert!(!takes[0].planted_red);
    assert!(takes[1..].iter().all(|take| take.planted_red));
    let frozen = parse(
        frozen_arguments(&options, Path::new("frozen"))
            .into_iter()
            .skip(1)
            .map(|word| word.into_string().unwrap()),
    )
    .unwrap();
    assert_eq!(frozen.before.as_deref(), Some(Path::new("old")));
    assert_eq!(frozen.verify, Path::new("frozen"));
    assert_eq!(frozen.cores, Some(15));
    assert_eq!(frozen.order(), options.order());
    assert_eq!(options.requested_cores(18).unwrap(), 15);
    assert!(options.require_grant(15, 15).is_ok());
    assert!(options.require_grant(15, 14).is_err());
    assert!(options.requested_cores(14).is_err());
    for words in [
        "--verify v --mode chain --cores 0",
        "--verify v --mode chain --cores -1",
        "--verify v --mode chain --cores 9 --cores 9",
        "--verify v --mode chain --queued-tree-share compare",
        "--verify v --before-verify v --mode matched --queued-tree-share compare --build-return retained",
    ] {
        assert!(
            parse(words.split_whitespace().map(str::to_owned)).is_err(),
            "{words}"
        );
    }
}

#[test]
fn successive_cold_side_targets_preserve_prior_outputs_and_native_layout() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let mut env = aterm_verify::EnvSnapshot {
        cargo_target_dir: Some(scratch.path().join("native.noindex")),
        ..Default::default()
    };
    let native = env.cargo_target_dir.clone();
    let first = scratch.path().join("first");
    let second = scratch.path().join("second");
    std::fs::create_dir(&first).unwrap();
    std::fs::create_dir(&second).unwrap();
    isolate_side_lanes(&first, &mut env).unwrap();
    let old = env.side_lanes_dir.clone().unwrap();
    std::fs::write(old.join("held-output"), b"preserve").unwrap();
    isolate_side_lanes(&second, &mut env).unwrap();
    assert_ne!(env.side_lanes_dir.as_ref(), Some(&old));
    assert_eq!(std::fs::read(old.join("held-output")).unwrap(), b"preserve");
    assert_eq!(env.cargo_target_dir, native);
    assert!(
        env.side_lanes_dir
            .as_ref()
            .unwrap()
            .read_dir()
            .unwrap()
            .next()
            .is_none()
    );
    assert!(
        isolate_side_lanes(&second, &mut env).is_err(),
        "never reuse an existing cold target"
    );
    assert_eq!(std::fs::read(old.join("held-output")).unwrap(), b"preserve");
}
#[test]
fn cold_native_takes_keep_their_policy_through_the_frozen_cutter() {
    let args = [
        "--verify",
        "after",
        "--before-verify",
        "before",
        "--mode",
        "matched",
        "--tree-work",
        "release",
        "--export-work",
        "real",
        "--slice-cache",
        "cold",
        "--tree-cache",
        "cold",
    ];
    let options = parse(args.into_iter().map(str::to_owned)).unwrap();
    assert!(options.cold_tree);
    assert!(!options.warm_slices);
    let decoded = parse(
        frozen_arguments(&options, &options.verify)
            .into_iter()
            .skip(1)
            .map(|argument| argument.into_string().unwrap()),
    )
    .unwrap();
    assert_eq!(decoded, options);
    for mode in ["alone", "export", "tree", "fixtures"] {
        assert!(
            parse(
                ["--verify", "after", "--mode", mode, "--tree-cache", "cold"]
                    .into_iter()
                    .map(str::to_owned)
            )
            .is_err()
        );
    }
}

#[test]
fn cold_native_target_keeps_measurement_images_including_symlink_aliases() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let target = dir.path().join("native.noindex");
    std::fs::create_dir(&target).unwrap();
    let inside = target.join("verify");
    let outside = dir.path().join("cutter");
    std::fs::write(&inside, b"fixture image").unwrap();
    std::fs::write(&outside, b"fixture image").unwrap();
    assert!(images_outside_target(dir.path(), &target, &[outside]).is_ok());
    assert!(images_outside_target(dir.path(), &target, std::slice::from_ref(&inside)).is_err());
    assert!(
        images_outside_target(
            dir.path(),
            &target,
            &[PathBuf::from("native.noindex/verify")]
        )
        .is_err()
    );
    #[cfg(unix)]
    {
        let alias = dir.path().join("verify-alias");
        std::os::unix::fs::symlink(&inside, &alias).unwrap();
        assert!(images_outside_target(dir.path(), &target, &[alias]).is_err());
    }
    assert_eq!(std::fs::read(inside).unwrap(), b"fixture image");
}

#[test]
fn series_refuses_a_partial_grant_before_any_take_but_single_takes_can_measure_it() {
    assert_eq!(reservation().rank, aterm_capacity::Rank::Release);
    assert_eq!(reservation().cores, 0);
    assert_eq!(reservation().mem, 0);
    assert!(require_series_grant("series", 18, 18).is_ok());
    assert!(require_series_grant("series", 18, 4).is_err());
    assert!(require_series_grant("alone", 18, 4).is_ok());
}
#[test]
fn tree_first_probe_starts_tree_before_slice_memory_admission() {
    let events = std::cell::RefCell::new(Vec::new());
    let (memory, tree, started) = start_tree_around_memory(
        "tree",
        || {
            events.borrow_mut().push("memory");
            Ok(1)
        },
        || {
            events.borrow_mut().push("tree");
            2
        },
        || {
            events.borrow_mut().push("tree-started");
            Ok(3)
        },
    )
    .unwrap();
    assert_eq!((memory, tree), (1, 2));
    assert_eq!(started, Some(3));
    assert_eq!(&*events.borrow(), &["tree", "tree-started", "memory"]);

    events.borrow_mut().clear();
    let (_, _, started) = start_tree_around_memory(
        "shared",
        || {
            events.borrow_mut().push("memory");
            Ok(())
        },
        || events.borrow_mut().push("tree"),
        || -> Result<(), String> {
            panic!("the fixed order consumes its start handshake after memory admission")
        },
    )
    .unwrap();
    assert_eq!(started, None);
    assert_eq!(
        &*events.borrow(),
        &["memory", "tree"],
        "moving the control to the fixed order is the negative mutation"
    );
}

#[test]
fn snapshotting_starts_under_a_live_reservation_and_failure_releases_it() {
    struct Guard<'a>(&'a std::cell::Cell<bool>);
    impl Drop for Guard<'_> {
        fn drop(&mut self) {
            self.0.set(false);
        }
    }
    let live = std::cell::Cell::new(false);
    let reserve = || {
        live.set(true);
        Ok(Guard(&live))
    };
    let (guard, ()) = under_reservation(reserve, || {
        assert!(live.get(), "snapshot preparation must already see Release");
        Ok(())
    })
    .unwrap();
    assert!(live.get());
    drop(guard);
    assert!(!live.get());
    // Negative control: a failed snapshot must not strand a Release reservation.
    let failed = under_reservation(reserve, || Err::<(), _>("snapshot failed".into()));
    assert!(failed.is_err());
    assert!(!live.get());
}
#[test]
fn contention_workload_matches_release_checks_without_pulled_cells_or_wasm() {
    use aterm_verify::checks::Check;
    for (work, expected) in [
        (TreeWork::CompileTests, Check::Test),
        (TreeWork::Lint, Check::Lint),
    ] {
        let parsed = aterm_verify::cli::parse(work.arguments()).unwrap();
        assert_eq!(parsed.wanted(|_| true), vec![expected]);
        assert_eq!(parsed.compile_only, expected == Check::Test);
        assert!(!aterm_verify::checks::pulls_cells(
            &parsed.wanted(|_| true),
            parsed.release,
            true
        ));
    }
    // Negative controls: standalone invocations add work no release tree runs.
    let old_test = aterm_verify::cli::parse(["test", "--all", "--compile-only"]).unwrap();
    assert!(old_test.wanted(|_| true).contains(&Check::WasmSmoke));
    let old_lint = aterm_verify::cli::parse(["lint", "--all"]).unwrap();
    assert!(aterm_verify::checks::pulls_cells(
        &old_lint.wanted(|_| true),
        old_lint.release,
        true
    ));
}
#[test]
fn a_tree_cargo_stays_tree_work_and_an_unrelated_cargo_does_not() {
    let tree = aterm_json::json!({"pid":2,"ppid":1,"name":"aterm-verify"});
    let cargo = aterm_json::json!({"pid":3,"ppid":2,"name":"targo"});
    let compiler = aterm_json::json!({"pid":4,"ppid":3,"name":"trustc"});
    let other = aterm_json::json!({"pid":5,"ppid":1,"name":"cargo"});
    let rows = BTreeMap::from([(2, &tree), (3, &cargo), (4, &compiler), (5, &other)]);
    assert_eq!(branch(&compiler, &rows, "aterm-verify"), "tree");
    assert_eq!(branch(&cargo, &rows, "aterm-verify"), "tree");
    assert_eq!(branch(&other, &rows, "aterm-verify"), "compat");
    let renamed = aterm_json::json!({"pid":2,"ppid":1,"name":"verify-before"});
    let renamed_rows = BTreeMap::from([(2, &renamed), (3, &cargo), (4, &compiler)]);
    assert_eq!(branch(&compiler, &renamed_rows, "verify-before"), "tree");
    assert_eq!(branch(&compiler, &renamed_rows, "aterm-verify"), "native");
    let missing: Result<u64, i32> = Err(3);
    assert!(observation(missing)["value"].is_null());
    assert_eq!(observation(Ok(0_u64))["value"].as_u64(), Some(0));
}
#[test]
fn cargo_nested_unit_events_survive_summary_and_a_partial_chain_is_refused() {
    let dir = aterm_tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("result.json"), "{\"slots\":1}").unwrap();
    let sample = aterm_json::json!({"elapsed_s":1.0,"at_unix_s":100.0,"capacity":{
        "global":{"error":null,"records":[{"what":"release slice memory budget","mem":49478023296_u64}]},
        "slice_budget":{"error":null,"records":[{"what":"release slice x86_64 unit aterm link","mem":24696061952_u64}]}
    },"members":[
        {"pid":4,"start":1,"name":"trustc","cpu_ns":1000000000_u64,"runnable_ns":1200000000_u64},
        {"pid":5,"start":1,"name":"trustc","cpu_ns":1000000000_u64,"runnable_ns":1200000000_u64}
    ]});
    let private_unread = aterm_json::json!({"elapsed_s":2.0,"at_unix_s":101.0,"capacity":{
        "global":{"error":null,"records":[]},
        "slice_budget":{"error":"unread","records":null}
    },"members":[]});
    std::fs::write(
        dir.path().join("processes.jsonl"),
        format!("{sample}\n{private_unread}\n"),
    )
    .unwrap();
    for triple in ["aarch64-apple-darwin", "x86_64-apple-darwin"] {
        let start = aterm_json::json!({"reason":"compiler-message","message":{"event":"release-slice-unit","phase":"started","pid":3,"compiler_pid":4,"queued_unix_s":10.0,"at_unix_s":11.0,"package":"aterm","unit":"aterm bin","memory_waiting_s":0.5,"memory_waited_for":{"reason":"memory short","holders":[{"pid":8,"what":"release slice x86_64 unit aterm link","claimed_bytes":24696061952_u64}]}}});
        let end = aterm_json::json!({"reason":"compiler-message","message":{"event":"release-slice-unit","phase":"ended","pid":3,"queued_unix_s":10.0,"at_unix_s":15.0,"cpu_s":2.0,"package":"aterm","unit":"aterm bin","success":true}});
        std::fs::write(
            dir.path().join(format!("{triple}.messages.jsonl")),
            format!("{start}\n{end}\n{{\"reason\":\"compiler-artifact\",\"fresh\":false,\"filenames\":[]}}\n{{\"reason\":\"compiler-artifact\",\"fresh\":true,\"filenames\":[]}}\n"),
        )
        .unwrap();
    }
    summarize(dir.path()).unwrap();
    let summary: aterm_json::Value =
        aterm_json::from_str(&std::fs::read_to_string(dir.path().join("summary.json")).unwrap())
            .unwrap();
    assert_eq!(summary["completed_units"].as_u64(), Some(2));
    assert_eq!(summary["artifacts"][0]["compiled"].as_u64(), Some(1));
    assert_eq!(summary["artifacts"][0]["fresh"].as_u64(), Some(1));
    assert_eq!(summary["compiler_count_over_slots_s"].as_f64(), Some(1.0));
    assert_eq!(summary["global_capacity_samples"].as_u64(), Some(2));
    assert_eq!(summary["private_capacity_samples"].as_u64(), Some(1));
    assert_eq!(
        summary["private_capacity_unavailable_samples"].as_u64(),
        Some(1),
        "NEGATIVE CONTROL: global readability cannot stand in for the private ledger"
    );
    assert_eq!(summary["capacity_timeline"].as_array().unwrap().len(), 2);
    assert_eq!(
        summary["capacity_timeline"][0]["at_unix_s"].as_f64(),
        Some(100.0),
        "unit events and holder snapshots retain the same epoch clock"
    );
    assert_eq!(
        summary["slice_unit_admission_wait"]["waited_units"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        summary["slice_unit_admission_wait"]["waited_units"][0]["memory_waited_for"]["holders"][0]
            ["what"]
            .as_str(),
        Some("release slice x86_64 unit aterm link")
    );
    let units = summary["critical_units"].as_array().unwrap();
    assert_eq!(units.len(), 2);
    assert_eq!(units[0]["wall_s"].as_f64(), Some(4.0));
    assert_eq!(units[0]["cpu_s"].as_f64(), Some(2.0));
    assert_eq!(units[0]["pre_spawn_s"].as_f64(), Some(1.0));
    assert_eq!(units[0]["sampled_compiler_cpu_s"].as_f64(), Some(1.0));
    assert_eq!(units[0]["sampled_compiler_cpu_wait_s"].as_f64(), Some(0.2));
    // Negative control: no complete result may be inferred from a start without its end.
    let start = aterm_json::json!({"message":{"event":"release-slice-unit","phase":"started","pid":9,"queued_unix_s":20.0,"at_unix_s":21.0}});
    std::fs::write(
        dir.path().join("aarch64-apple-darwin.messages.jsonl"),
        format!("{start}\n"),
    )
    .unwrap();
    assert!(summarize(dir.path()).is_err());
}
#[test]
fn a_missing_or_reused_compiler_pid_has_no_invented_lifetime_counter() {
    let mut counters = BTreeMap::new();
    assert!(compiler_counters(4, &counters).is_none());
    counters.insert((4, 1), Counters::default());
    assert!(compiler_counters(4, &counters).is_some());
    counters.insert((4, 2), Counters::default());
    assert!(compiler_counters(4, &counters).is_none());
}
#[test]
fn descendant_sampling_follows_grandchildren_out_of_order_and_excludes_other_sessions() {
    use aterm_sysprobe::procs::ProcessReading;
    let row = |pid, ppid| ProcessReading {
        pid,
        ppid,
        ..ProcessReading::default()
    };
    let rows = [row(3, 2), row(90, 80), row(2, 1), row(4, 3), row(80, 0)];
    assert_eq!(descendants(&rows, 1), BTreeSet::from([1, 2, 3, 4]));
    assert_eq!(descendants(&rows, 80), BTreeSet::from([80, 90]));
}
#[test]
fn experimental_modes_require_an_explicit_verifier_and_reject_unknown_priorities() {
    let parse_words = |s: &str| parse(s.split_whitespace().map(str::to_owned));
    for mode in ["alone", "shared", "chain", "tree", "series"] {
        let before = if mode == "series" {
            " --before-verify /old"
        } else {
            ""
        };
        assert_eq!(
            parse_words(&format!("--verify /v --mode {mode}{before}"))
                .unwrap()
                .mode,
            mode
        );
    }
    assert!(parse_words("--mode chain").is_err());
    assert!(parse_words("--verify /v --mode fastest").is_err());
    assert!(parse_words("--verify /v --mode chain --mode tree").is_err());
    assert!(parse_words("--verify /v --mode series").is_err());
    assert!(parse_words("--verify /v --mode chain --before-verify /old").is_err());
    assert_eq!(
        order("series"),
        ["alone", "shared", "chain", "chain", "shared", "tree"]
    );
    assert_eq!(order("shared"), ["shared"]);
    let release = parse_words("--verify /v --mode shared --tree-work release").unwrap();
    assert_eq!(release.tree_work, TreeShape::Release);
    assert_eq!(release.export_seconds(), Some(RELEASE_EXPORT_SECONDS));
    let override_export =
        parse_words("--verify /v --mode shared --tree-work release --export-seconds 5").unwrap();
    assert_eq!(override_export.export_seconds(), Some(5));
    let no_export =
        parse_words("--verify /v --mode shared --tree-work release --export-seconds 0").unwrap();
    assert_eq!(no_export.export_seconds(), None);
    assert!(parse_words("--verify /v --mode shared --tree-work almost").is_err());
    assert!(parse_words("--verify /v --mode shared --export-seconds soon").is_err());
}

#[test]
fn release_shared_and_chain_hold_every_non_export_token_but_tree_is_only_a_probe() {
    let parse_words = |words: &str| parse(words.split_whitespace().map(str::to_owned)).unwrap();
    let shared = parse_words("--verify /v --mode shared --tree-work release");
    let chain = parse_words("--verify /v --mode chain --tree-work release");
    let old = parse_words("--verify /v --mode tree --tree-work release");
    let compile_lint = parse_words("--verify /v --mode shared");
    assert_eq!(admission_barrier_share(&shared, 18, 4), 14);
    assert_eq!(admission_barrier_share(&chain, 18, 4), 14);
    assert_eq!(admission_barrier_share(&old, 18, 4), 0);
    assert_eq!(admission_barrier_share(&compile_lint, 18, 4), 0);
    // Negative control: omitting the barrier leaves every non-export compiler token live.
    assert_eq!(18 - 4 - admission_barrier_share(&old, 18, 4), 14);
    assert_eq!(18 - 4 - admission_barrier_share(&shared, 18, 4), 0);
}

#[test]
fn only_a_live_exact_tippy_request_in_the_tree_opens_the_compiler_barrier() {
    use aterm_sysprobe::procs::ProcessReading;
    let shapes = tippy_claim_shapes().unwrap();
    let process = |pid, ppid| ProcessReading {
        pid,
        ppid,
        ..ProcessReading::default()
    };
    let processes = [process(100, 1), process(110, 100), process(200, 1)];
    let held = aterm_capacity::Held {
        admitted_ms: 2,
        cores: 0,
        alone: false,
    };
    let record = |label: &str, bytes: u64| aterm_capacity::Record {
        accounting: aterm_capacity::Accounting::Holder,
        pid: 110,
        start_ms: 1,
        seq: 2,
        checkout: "/release".into(),
        need: aterm_capacity::Need::new(label)
            .rank(aterm_capacity::Rank::Release)
            .mem(bytes)
            .covers(),
        timing: aterm_capacity::TimingEvidence::default(),
        held: Some(held),
        credit: Vec::new(),
        wrote: None,
        next_ms: None,
    };
    for shape in &shapes {
        let valid = record(&shape.label, 12 << 30);
        assert_eq!(
            matching_tree_claim(std::slice::from_ref(&valid), &processes, 100, &shapes)
                .map(|(_, state)| state),
            Some(crate::release_steps::TreeLintClaim::Held)
        );
        let mut waiting = valid;
        waiting.held = None;
        assert_eq!(
            matching_tree_claim(&[waiting], &processes, 100, &shapes).map(|(_, state)| state),
            Some(crate::release_steps::TreeLintClaim::Waiting),
            "an exact queued request selects the bounded escape"
        );
    }

    let whole = &shapes[0];
    let valid = record(&whole.label, 12 << 30);
    let mut invalid = valid.clone();
    invalid.need.mem = 0;
    assert_eq!(
        matching_tree_claim(std::slice::from_ref(&invalid), &processes, 100, &shapes)
            .map(|(_, state)| state),
        Some(crate::release_steps::TreeLintClaim::Invalid),
        "a whole-stage claim that reserves no memory refuses the tree instead of opening its barrier"
    );
    let mut lookalike = valid.clone();
    lookalike.need.what.push_str(" later");
    let mut ordinary = valid.clone();
    ordinary.need.rank = aterm_capacity::Rank::Ordinary;
    let mut uncovered = valid.clone();
    uncovered.need.covers = false;
    let mut foreign = valid.clone();
    foreign.pid = 200;
    assert!(matching_tree_claim(&[lookalike], &processes, 100, &shapes).is_none());
    for mutant in [ordinary, uncovered] {
        assert_eq!(
            matching_tree_claim(&[mutant], &processes, 100, &shapes).map(|(_, state)| state),
            Some(crate::release_steps::TreeLintClaim::Invalid)
        );
    }
    assert!(matching_tree_claim(&[foreign], &processes, 100, &shapes).is_none());

    let mut waiting = valid.clone();
    waiting.held = None;
    for records in [
        vec![invalid.clone(), waiting.clone(), valid.clone()],
        vec![valid, waiting, invalid],
    ] {
        assert_eq!(
            matching_tree_claim(&records, &processes, 100, &shapes).map(|(_, state)| state),
            Some(crate::release_steps::TreeLintClaim::Held),
            "duplicate file order cannot override the held valid request"
        );
    }
    assert!(
        matching_tree_claim(&[], &processes[1..], 100, &shapes).is_none(),
        "a missing tree root cannot authenticate a stale claim"
    );
}

#[test]
fn shared_ready_events_name_the_full_release_budget_and_relay_without_inference() {
    let event = aterm_json::json!({
        "event":"slice-memory-ready",
        "mode":"shared",
        "held":true,
        "label":"release slice memory budget",
        "mem":crate::slice_units::MIN_SHARED,
        "claim":{"pid":7,"start_ms":8,"seq":9},
    });
    assert!(ready_shape(&event, crate::slice_units::MIN_SHARED));
    assert!(
        !ready_shape(&event, crate::slice_units::MIN_SHARED + (1 << 30)),
        "NEGATIVE CONTROL: a calibrated parent cannot be authenticated at the code floor"
    );
    for mutant in [
        aterm_json::json!({
            "event":"slice-memory-ready", "mode":"shared", "held":true,
            "label":"release slice aarch64-apple-darwin",
            "mem":crate::slice_units::MIN_SHARED,
        }),
        aterm_json::json!({
            "event":"slice-memory-ready", "mode":"shared", "held":true,
            "label":"release slice memory budget",
            "mem":crate::slice_units::MIN_SHARED - 1,
        }),
        aterm_json::json!({
            "event":"slice-memory-ready", "mode":"shared", "held":false,
            "label":"release slice memory budget",
            "mem":crate::slice_units::MIN_SHARED,
        }),
    ] {
        assert!(!ready_shape(&mutant, crate::slice_units::MIN_SHARED));
    }

    let input = format!("unrelated output\n{SLICE_CHILD_EVENT}{event}\n");
    let mut transcript = Vec::new();
    let (sender, receiver) = std::sync::mpsc::channel();
    relay_slice_child_events(input.as_bytes(), &mut transcript, sender).unwrap();
    assert_eq!(receiver.recv().unwrap().unwrap(), event);
    assert_eq!(String::from_utf8(transcript).unwrap(), input);

    let (sender, receiver) = std::sync::mpsc::channel();
    assert!(
        relay_slice_child_events(
            format!("{SLICE_CHILD_EVENT}not-json\n").as_bytes(),
            Vec::new(),
            sender,
        )
        .is_ok()
    );
    assert!(receiver.recv().unwrap().is_err());
}

#[test]
fn cold_readiness_requires_the_exact_live_exclusive_parent() {
    let event = aterm_json::json!({
        "event":"slice-memory-ready",
        "mode":"cold",
        "held":true,
        "label":crate::slice_units::COLD_PARENT,
        "mem":crate::slice_units::DRIVER,
        "claim":{"pid":7,"start_ms":8,"seq":9},
    });
    assert!(ready_shape(&event, crate::slice_units::DRIVER));
    let record = aterm_capacity::Record {
        accounting: aterm_capacity::Accounting::Holder,
        pid: 7,
        start_ms: 8,
        seq: 9,
        checkout: String::new(),
        need: crate::slice_units::cold_parent_need(),
        timing: aterm_capacity::TimingEvidence::default(),
        held: Some(aterm_capacity::Held {
            admitted_ms: 10,
            cores: 0,
            alone: true,
        }),
        credit: Vec::new(),
        wrote: None,
        next_ms: None,
    };
    assert!(ready_record_matches(&event, 7, true, &record));

    let mut oversized = event.clone();
    oversized
        .as_object_mut()
        .unwrap()
        .insert("mem".into(), (crate::slice_units::DRIVER + 1).into());
    assert!(
        !ready_shape(&oversized, crate::slice_units::DRIVER),
        "NEGATIVE CONTROL: cold admission is the exact fixed driver claim"
    );
    let mut nonexclusive = record.clone();
    nonexclusive.need.alone = false;
    nonexclusive.held.as_mut().unwrap().alone = false;
    assert!(
        !ready_record_matches(&event, 7, true, &nonexclusive),
        "NEGATIVE CONTROL: a nonexclusive lookalike cannot release the build barrier"
    );
}

#[test]
fn every_pilot_error_path_kills_and_reaps_its_slice_child() {
    struct FakeProcess {
        killed: std::sync::Arc<std::sync::atomic::AtomicBool>,
        reaped: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    impl PilotSliceProcess for FakeProcess {
        fn owns_unreaped(&self) -> bool {
            !self.reaped.load(std::sync::atomic::Ordering::SeqCst)
        }

        fn kill_for_custody(&mut self) -> std::io::Result<()> {
            self.killed.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }

        fn reap_for_custody(&mut self) -> std::io::Result<()> {
            assert!(
                self.killed.load(std::sync::atomic::Ordering::SeqCst),
                "NEGATIVE CONTROL: the child must be killed before its owning reap"
            );
            self.reaped.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
    }

    fn fail_after_spawn(process: FakeProcess) -> Result<(), String> {
        let _child = PilotSliceChild::new(process);
        Err("relay failed after admission".into())
    }

    let killed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let reaped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    assert!(
        fail_after_spawn(FakeProcess {
            killed: std::sync::Arc::clone(&killed),
            reaped: std::sync::Arc::clone(&reaped),
        })
        .is_err()
    );
    assert!(killed.load(std::sync::atomic::Ordering::SeqCst));
    assert!(
        reaped.load(std::sync::atomic::Ordering::SeqCst),
        "NEGATIVE CONTROL: returning the relay error cannot orphan the slice child"
    );
}

#[test]
fn only_lower_priority_invocations_gain_nice_and_keep_the_original_program() {
    let program = Path::new("/compiler path/tool");
    assert_eq!(invocation(program, false), (program.to_owned(), Vec::new()));
    if cfg!(unix) {
        assert_eq!(
            invocation(program, true),
            (
                PathBuf::from("/usr/bin/nice"),
                vec![
                    OsString::from("-n"),
                    "10".into(),
                    program.as_os_str().to_owned()
                ]
            )
        );
    }
}
