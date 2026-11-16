// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn profile_pilot_wasm_outputs_respect_the_private_target_root() {
    let cwd = std::env::current_dir().unwrap();
    let sealed = cwd.join("sealed");
    let private = cwd.join("private.noindex");
    let neutral = cwd.join("neutral");
    let default = module_build_plan(&crate::EnvSnapshot::default(), &neutral, &sealed).unwrap();
    assert_eq!(
        default.target(),
        sealed.join(aterm_toolchain::build_cache::DEFAULT_TARGET)
    );
    let env = crate::EnvSnapshot {
        cargo_target_dir: Some(private.clone()),
        ..crate::EnvSnapshot::default()
    };
    let selected = module_build_plan(&env, &neutral, &sealed).unwrap();
    assert_eq!(selected.target(), private);
    assert!(!selected.target().starts_with(&sealed));
    let empty = crate::EnvSnapshot {
        cargo_layout: vec![("CARGO_TARGET_DIR", std::ffi::OsString::new())],
        ..crate::EnvSnapshot::default()
    };
    assert!(module_build_plan(&empty, &neutral, &sealed).is_err());
}

#[cfg(target_os = "macos")]
#[test]
fn wasm_builds_guard_automatic_finals_and_read_the_captured_target() {
    use std::os::unix::fs::DirBuilderExt as _;
    for shape in ["default", "default-link", "explicit-link"] {
        let temp = aterm_tempfile::tempdir().unwrap();
        let temp = crate::physical(temp.path());
        let source = temp.join("neutral");
        let artifacts = temp.join("artifacts");
        let outside = temp.join("outside");
        for path in [&source, &artifacts, &outside] {
            std::fs::DirBuilder::new().mode(0o700).create(path).unwrap();
        }
        std::fs::write(outside.join("keep"), b"outside data").unwrap();
        let logical = artifacts.join(aterm_toolchain::build_cache::DEFAULT_TARGET);
        if shape.ends_with("link") {
            std::fs::create_dir(artifacts.join("target")).unwrap();
            std::os::unix::fs::symlink(&outside, artifacts.join("target/.ubd")).unwrap();
        }
        // A HOME of its own: with none, the front cannot tell which folders atpkg manages and
        // refuses every build (atpkg's `homeless`), as a native child here would meet it.
        let home = temp.join("home");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&home)
            .unwrap();
        let mut env = crate::EnvSnapshot {
            home,
            ..crate::EnvSnapshot::default()
        };
        if shape == "explicit-link" {
            env.cargo_layout
                .push(("CARGO_TARGET_DIR", logical.clone().into_os_string()));
        }
        let plan = module_build_plan(&env, &source, &artifacts).unwrap();
        let expected = if shape == "explicit-link" {
            outside.join("main.noindex")
        } else {
            logical
        };
        assert_eq!(plan.target(), expected);
        env.cargo_layout = vec![(
            "CARGO_TARGET_DIR",
            temp.join("later.noindex").into_os_string(),
        )];
        assert_eq!(
            module_build_plan(&env, &source, &artifacts)
                .unwrap()
                .target(),
            temp.join("later.noindex")
        );
        let mut ran = false;
        let built = run_module_build(&plan, "dev", || {
            let output = aterm_uds::spawnfd::command("/bin/echo")
                .arg("wasm-child")
                .output()
                .map_err(|error| error.to_string())?;
            assert!(output.status.success());
            ran = true;
            let modules = plan.target().join(WASM32).join("debug");
            std::fs::create_dir_all(&modules).map_err(|error| error.to_string())?;
            std::fs::write(modules.join("aterm_wasm.wasm"), output.stdout)
                .map_err(|error| error.to_string())
        });
        let admitted = shape != "default-link";
        assert_eq!(built.is_ok(), admitted, "{shape}");
        assert_eq!(ran, admitted, "{shape}");
        if let Ok(built) = built {
            assert_eq!(built.target, expected);
            assert_eq!(
                std::fs::read(built.directory.join("aterm_wasm.wasm")).unwrap(),
                b"wasm-child\n"
            );
        } else {
            assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 1);
        }
        assert!(!temp.join("later.noindex").exists());
        assert_eq!(
            std::fs::read(outside.join("keep")).unwrap(),
            b"outside data"
        );
    }
}

fn json(text: &str) -> Value {
    aterm_json::from_str(text).expect("test JSON parses")
}

fn sizes(cpu: u64, gpu: u64) -> Value {
    json(&format!(
        "{{\"wasm_cpu_module_bytes\":{cpu},\"wasm_gpu_module_bytes\":{gpu}}}"
    ))
}

/// WITHOUT A RUSTUP THE WASM BUILDS END COULD NOT RUN NAMING THE WORKSPACE PROVIDER: where
/// the `rustup` row finds none (no stamped copy, nothing on PATH), the lanes that build
/// the web modules (`browser` and the replay among them) refuse the machine, saying where the row looked
/// and naming `verify provision` (its provider) once, never Homebrew, even with a keg on
/// disk. Negative control: a rustup that is found gets past this refusal to the next
/// question (here, a stand-in with no wasm32 std).
#[test]
#[cfg(unix)]
fn with_no_rustup_the_wasm_builds_could_not_run_naming_verify_provision() {
    use crate::perf::Status;
    let tmp = aterm_tempfile::tempdir().unwrap();
    let keg = tmp.path().join("opt/homebrew/opt/rustup/bin/rustup");
    std::fs::create_dir_all(keg.parent().unwrap()).unwrap();
    std::fs::write(&keg, "#!/bin/false\n").unwrap();
    let row = aterm_uds::host::tool("rustup");
    let lookup = aterm_uds::host::Lookup {
        named: None,
        tooling: Some(tmp.path().join("tooling.noindex")),
        path: Some(tmp.path().join("empty").into_os_string()),
    };
    let repo = workspace_root();
    let Err(refused) = build_tools_with(row.locate_in(&lookup), &repo, false) else {
        panic!("no rustup built the modules");
    };
    let Status::CouldNotRun { why, fix } = refused.status else {
        panic!("{:?}", refused.status);
    };
    assert!(
        why.starts_with("no rustup (looked in $ATERM_TEST_RUSTUP"),
        "{why}"
    );
    assert_eq!(
        fix,
        format!("{}, with the stock pin's wasm32 std", row.remedy())
    );
    let said = format!("{why} {fix}");
    assert_eq!(said.matches("verify provision").count(), 1, "{said}");
    assert!(!said.contains("brew"), "{said}");

    let Err(next) = build_tools_with(Ok(PathBuf::from("/usr/bin/false")), &repo, false) else {
        panic!("a rustup that holds no stock pin built the modules");
    };
    assert!(
        matches!(&next.status, Status::CouldNotRun { why, fix }
            if why.starts_with("rustup could not list its toolchains")
                && fix == crate::rustup::PROVISION),
        "{:?}",
        next.status
    );
}

#[test]
fn the_wasm_module_ceilings_hold_absolutely() {
    let cpu_cap = WASM_CPU_MODULE_BYTES_CAP as u64;
    let gpu_cap = WASM_GPU_MODULE_BYTES_CAP as u64;
    assert!(wasm_module_fences(&sizes(cpu_cap, gpu_cap)).is_empty());
    let cpu_over = wasm_module_fences(&sizes(cpu_cap + 1, gpu_cap));
    assert_eq!(cpu_over.len(), 1);
    assert!(cpu_over[0].starts_with("wasm_cpu_module_bytes "));
    let gpu_over = wasm_module_fences(&sizes(cpu_cap, gpu_cap + 1));
    assert_eq!(gpu_over.len(), 1);
    assert!(gpu_over[0].starts_with("wasm_gpu_module_bytes "));
    assert_eq!(
        wasm_module_fences(&sizes(cpu_cap + 1, gpu_cap + 1)).len(),
        2
    );
    let missing_gpu = wasm_module_fences(&json("{\"wasm_cpu_module_bytes\":1}"));
    assert_eq!(missing_gpu.len(), 1);
    assert!(missing_gpu[0].starts_with("wasm_gpu_module_bytes "));
    let missing_cpu = wasm_module_fences(&json("{\"wasm_gpu_module_bytes\":1}"));
    assert_eq!(missing_cpu.len(), 1);
    assert!(missing_cpu[0].starts_with("wasm_cpu_module_bytes "));
}

/// The floors are judged and recorded only in the pinned shell: another
/// browser's run is not judged and keeps no samples for the trend, and a
/// record in it could not run. NEGATIVE CONTROL: the pinned product is
/// judged.
#[cfg(unix)]
#[test]
fn the_floors_are_judged_only_in_the_pinned_shell() {
    let pinned = crate::browser::chromium::pinned_product();
    assert!(floors_engine(&pinned, false).is_ok() && floors_engine(&pinned, true).is_ok());
    let other = "HeadlessChrome/1.0.0.0";
    let run = floors_engine(other, false).expect_err("another browser");
    assert!(
        matches!(&run.status, super::super::Status::NotJudged(why)
            if why.contains(&pinned) && why.contains(other)),
        "{run:?}"
    );
    assert!(run.samples.is_empty());
    let record = floors_engine(other, true).expect_err("no record");
    assert!(
        matches!(&record.status, super::super::Status::CouldNotRun { why, .. }
            if why.contains("records in the pinned shell only")),
        "{record:?}"
    );
}

#[test]
fn each_gated_keys_samples_print_in_lane_order() {
    let report = json(
        "{\"samples\":{\"wasm_gpu_typing_render_fps\":[3,4.5],\
         \"wasm_cpu_ingest_mixed_mbps\":[1,2],\"ungated\":[9]}}",
    );
    assert_eq!(
        sample_lines(&report),
        [
            "wasm_cpu_ingest_mixed_mbps: n=2 [1 2]",
            "wasm_gpu_typing_render_fps: n=2 [3 4.5]"
        ]
    );
    assert!(sample_lines(&json("{}")).is_empty());
}

/// Compare already built modules and their glue under the same browser driver and timing admission.
#[cfg(unix)]
#[test]
#[ignore = "manual paired browser measurement; requires ATERM_WASM_BASE_SITE, ATERM_WASM_AFTER_SITE and ATERM_WASM_PAIR_REPORT"]
fn paired_prebuilt_browser_costs() {
    let path = |key| std::path::PathBuf::from(std::env::var_os(key).expect(key));
    let base = path("ATERM_WASM_BASE_SITE");
    let after = path("ATERM_WASM_AFTER_SITE");
    let report = path("ATERM_WASM_PAIR_REPORT");
    assert!(
        !report.exists(),
        "use a fresh report path to retain each cohort"
    );
    assert_ne!(base.canonicalize().unwrap(), after.canonicalize().unwrap());
    let assets = [
        "cpu/aterm_wasm_bg.wasm",
        "gpu/aterm_gpu_web_bg.wasm",
        "cpu/aterm_wasm.js",
        "gpu/aterm_gpu_web.js",
    ];
    let before: Vec<_> = [&base, &after]
        .into_iter()
        .map(|site| assets.map(|name| std::fs::read(site.join(name)).unwrap()))
        .collect();
    assert!(
        before[0] != before[1],
        "all shipping JS and WASM assets are identical"
    );
    let browser = wasm_page::browser().expect("provision the pinned browser with verify provision");
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = workspace_root();
    let mut records = Vec::new();
    for pair in 0..4 {
        let order = if pair % 2 == 0 {
            [("base", &base), ("after", &after)]
        } else {
            [("after", &after), ("base", &base)]
        };
        for (side, site) in order {
            wasm_page::lay_drivers(&root, site).unwrap();
            let mut page = Page::start(&browser, site, scratch.path()).unwrap();
            let entry = super::super::measurement_entry().unwrap();
            let ran = page
                .run(
                    &format!("{}/bench.mjs", wasm_page::DRIVERS),
                    &json!({"cpu":"../cpu/aterm_wasm.js", "gpu":"../gpu/aterm_gpu_web.js",
                        "font":format!("../{}", wasm_page::FONT)}),
                )
                .unwrap();
            let qualified = entry.timing_admission();
            eprintln!(
                "paired wasm: pair={pair} side={side} browser={}",
                page.product()
            );
            records.push(json!({"pair":pair,"side":side,"browser":page.product(),
                                "timing_qualified":qualified.is_ok(),
                                "report":ran.value.clone(),"console":ran.console.clone()}));
            super::super::publication::publish(&report, &json!({"records":records}).to_string())
                .unwrap();
            qualified.unwrap();
            for key in WASM_LANE.keys {
                assert!(
                    ran.value
                        .get(key)
                        .and_then(Value::as_f64)
                        .is_some_and(|v| v.is_finite() && v > 0.0),
                    "{key}"
                );
                assert_eq!(
                    ran.value
                        .get("samples")
                        .unwrap()
                        .get(key)
                        .unwrap()
                        .as_array()
                        .unwrap()
                        .len(),
                    5,
                    "{key}"
                );
            }
        }
    }
    for (i, site) in [&base, &after].into_iter().enumerate() {
        for (j, name) in assets.into_iter().enumerate() {
            assert_eq!(
                std::fs::read(site.join(name)).unwrap(),
                before[i][j],
                "asset changed during measurement: {name}"
            );
        }
    }
}
