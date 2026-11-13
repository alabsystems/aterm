// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
#[test]
fn compiler_pass_observations_join_fragmented_lines_and_ignore_other_diagnostics() {
    let mut pending = Vec::new();
    let mut first = &b"warning: time: {}\ntime: {\"pass\":\"LLVM_fat"[..];
    assert!(
        compiler_passes(&mut first, &mut pending)
            .unwrap()
            .is_empty()
    );
    let mut second = &b"lto\",\"time\":7.5}\ntime: not json\n"[..];
    assert_eq!(
        compiler_passes(&mut second, &mut pending).unwrap(),
        vec![aterm_json::json!({"pass":"LLVM_fatlto", "time":7.5})]
    );
    assert!(pending.is_empty());
    assert!(
        compiler_passes(&mut &b""[..], &mut pending)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn anatomy_comparisons_use_two_interleaved_takes_and_single_arms_are_diagnostic() {
    let opts = parse(["--anatomy"].into_iter().map(str::to_owned)).unwrap();
    assert_eq!(order(&opts), vec![(0, Arm::Fat1)]);
    assert!(opts.anatomy);
    assert!(opts.diagnostic_anatomy());
    let candidate = parse(
        ["--anatomy", "--arm", "fat16"]
            .into_iter()
            .map(str::to_owned),
    )
    .unwrap();
    assert_eq!(order(&candidate), vec![(0, Arm::Fat16)]);
    assert!(candidate.diagnostic_anatomy());
    let comparison = parse(
        ["--anatomy", "--arm", "fat1", "--arm", "fat16"]
            .into_iter()
            .map(str::to_owned),
    )
    .unwrap();
    assert!(!comparison.diagnostic_anatomy());
    assert_eq!(
        order(&comparison),
        vec![
            (0, Arm::Fat1),
            (0, Arm::Fat16),
            (1, Arm::Fat16),
            (1, Arm::Fat1)
        ]
    );
    for flags in [
        vec!["--takes", "1", "--arm", "fat1"],
        vec!["--anatomy", "--arm", "fat16", "--arm", "fat16"],
        vec!["--anatomy", "--inspect", "take"],
        vec!["--anatomy", "--takes", "1"],
        vec!["--anatomy", "--takes", "2"],
    ] {
        assert!(parse(flags.into_iter().map(str::to_owned)).is_err());
    }
}

#[test]
fn cargo_timings_survive_retirement_of_the_cold_graph() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let target = dir.path().join("build.noindex");
    let timings = target.join("cargo-timings");
    std::fs::create_dir_all(&timings).unwrap();
    std::fs::write(timings.join("cargo-timing.html"), b"unit timeline").unwrap();
    retain_timings(&target, dir.path()).unwrap();
    std::fs::remove_dir_all(&target).unwrap();
    assert_eq!(
        std::fs::read(dir.path().join("cargo-timings/cargo-timing.html")).unwrap(),
        b"unit timeline"
    );
    // A missing report is an observation failure, rather than a successful empty report.
    assert!(retain_timings(&target, dir.path()).is_err());
}

#[test]
fn build_samples_exclude_supervisors_and_distinguish_reused_pids() {
    let member = aterm_sysprobe::MemberUsage {
        pid: 42,
        start: 1,
        cpu_ns: 123,
        footprint: 456,
        ..Default::default()
    };
    let initial = [(42, 1)].into_iter().collect();
    let reused = aterm_sysprobe::MemberUsage { start: 2, ..member };
    let sample = resource_sample(3.0, &initial, Some(vec![member, reused]), vec![]);
    assert_eq!(sample["members"].as_array().unwrap().len(), 1);
    assert_eq!(sample["members"][0]["start"].as_u64(), Some(2));
    assert_eq!(sample["members"][0]["cpu_ns"].as_u64(), Some(123));
    assert_eq!(sample["members"][0]["footprint_bytes"].as_u64(), Some(456));
    // A refused read must remain distinguishable from an empty build group.
    assert!(resource_sample(0.0, &initial, None, vec![])["members"].is_null());
    assert_eq!(
        resource_sample(0.0, &initial, Some(vec![]), vec![])["members"],
        aterm_json::json!([])
    );
}

#[test]
fn build_sampler_retains_observations_and_returns_the_work_result() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let (result, _) = with_load(dir.path(), || 17).unwrap();
    assert_eq!(result, 17);
    let samples = std::fs::read_to_string(dir.path().join("build-resources.jsonl")).unwrap();
    assert!(!samples.is_empty());
    for line in samples.lines() {
        let row: aterm_json::Value = aterm_json::from_str(line).unwrap();
        assert!(row["elapsed_s"].as_f64().unwrap() >= 0.0);
        assert!(row.as_object().unwrap().contains_key("members"));
    }
}

#[test]
fn symbol_identity_preserves_function_offsets_and_source_locations() {
    let demangle = |_: &str| Ok("module::function".to_owned());
    let a = canonical_symbolication("_Rfirst + 4 (in aterm) (source.rs:7)", demangle).unwrap();
    let b = canonical_symbolication("_Rsecond + 4 (in aterm) (source.rs:7)", demangle).unwrap();
    assert_eq!(a, b);
    assert_ne!(
        a,
        canonical_symbolication("_Rsecond + 4 (in aterm) (source.rs:8)", demangle).unwrap()
    );
    assert_ne!(
        a,
        canonical_symbolication("_Rsecond + 8 (in aterm) (source.rs:7)", demangle).unwrap()
    );
    assert_ne!(
        a,
        canonical_symbolication("_Rsecond + 4 (in aterm) (source.rs:7)", |_| Ok(
            "module::other".to_owned()
        ))
        .unwrap()
    );
    assert!(canonical_symbolication("0x1000", demangle).is_err());
    assert!(canonical_symbolication("", demangle).is_err());
}
#[test]
fn retained_inspection_cannot_write_outside_the_pilot_cache() {
    let cache = aterm_tempfile::tempdir().unwrap();
    let outside = aterm_tempfile::tempdir().unwrap();
    let take = cache.path().join("take");
    std::fs::create_dir(&take).unwrap();
    assert_eq!(
        retained_root(cache.path(), &take).unwrap(),
        std::fs::canonicalize(&take).unwrap()
    );
    assert!(retained_root(cache.path(), outside.path()).is_err());
    assert!(retained_root(cache.path(), cache.path()).is_err());
    assert!(retained_root(cache.path(), &take.join("../..")).is_err());
    #[cfg(unix)]
    {
        let link = take.join("escape");
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();
        assert!(retained_root(cache.path(), &link).is_err());
    }
    assert!(
        parse(
            ["--inspect", "take", "--arm", "fat1"]
                .into_iter()
                .map(str::to_owned)
        )
        .is_err()
    );
}
#[test]
fn lookup_misses_are_skipped_but_tool_failures_are_not() {
    assert!(tool_answer("dwarfdump", Some(1), b"image header".to_vec(), b"", true).is_ok());
    assert!(tool_answer("dwarfdump", Some(1), vec![], b"malformed DWARF", true).is_err());
    assert!(tool_answer("dwarfdump", None, vec![], b"", true).is_err());
    assert!(tool_answer("dsymutil", Some(1), vec![], b"", false).is_err());
}
#[test]
fn retained_artifact_messages_determine_the_link_interval() {
    use std::fs::{File, FileTimes};
    use std::time::{Duration, UNIX_EPOCH};
    let dir = aterm_tempfile::tempdir().unwrap();
    let library = dir.path().join("relocated.rlib");
    let binary = dir.path().join("image");
    for (path, seconds) in [(&library, 100), (&binary, 107)] {
        File::create(path)
            .unwrap()
            .set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(seconds)))
            .unwrap();
    }
    let log = dir.path().join("build.log");
    std::fs::write(&log, "compiler diagnostics, not artifacts\n").unwrap();
    let row = aterm_json::json!({"reason":"compiler-artifact", "target":{"name":"aterm_gui"}, "filenames":[library]}).to_string();
    std::fs::write(log.with_extension("messages.jsonl"), &row).unwrap();
    assert_eq!(gui_link_interval(&log, &binary).unwrap(), Some(7.0));
    std::fs::write(
        log.with_extension("messages.jsonl"),
        row.replace("compiler-artifact", "compiler-message"),
    )
    .unwrap();
    assert_eq!(gui_link_interval(&log, &binary).unwrap(), None);
}
#[test]
fn artifact_messages_find_relocated_rlibs_and_reject_diagnostics() {
    let path = std::env::current_dir()
        .unwrap()
        .join("fresh/release/build/gui/unit/out/libaterm_gui.rlib");
    let row = aterm_json::json!({"reason":"compiler-artifact","target":{"name":"aterm_gui"},"filenames":[path]}).to_string();
    assert_eq!(gui_rlib(&row).unwrap(), path);
    assert!(gui_rlib(&row.replace("compiler-artifact", "compiler-message")).is_none());
    assert!(gui_rlib(&row.replace("aterm_gui", "other")).is_none());
}
#[test]
fn section_identity_ignores_metadata_but_detects_changed_machine_code() {
    let mut image = vec![0u8; 188];
    image[0..4].copy_from_slice(&0xfeed_facfu32.to_le_bytes());
    image[16..20].copy_from_slice(&1u32.to_le_bytes());
    image[32..36].copy_from_slice(&0x19u32.to_le_bytes());
    image[36..40].copy_from_slice(&152u32.to_le_bytes());
    image[40..46].copy_from_slice(b"__TEXT");
    image[96..100].copy_from_slice(&1u32.to_le_bytes());
    image[104..110].copy_from_slice(b"__text");
    image[144..152].copy_from_slice(&4u64.to_le_bytes());
    image[152..156].copy_from_slice(&184u32.to_le_bytes());
    image[184..188].copy_from_slice(&[1, 2, 3, 4]);
    let before = sections(&image).unwrap();
    image[8] = 3;
    assert_eq!(sections(&image).unwrap(), before);
    image[187] = 9;
    assert_ne!(sections(&image).unwrap(), before);
    image.truncate(187);
    assert!(sections(&image).is_err());
}
#[test]
fn arms_and_interleaving_are_exact() {
    let opts = parse(
        ["--arm", "fat1", "--arm", "fat1-lines"]
            .into_iter()
            .map(str::to_owned),
    )
    .unwrap();
    assert_eq!(
        order(&opts),
        vec![
            (0, Arm::Fat1),
            (0, Arm::Fat1Lines),
            (1, Arm::Fat1Lines),
            (1, Arm::Fat1)
        ]
    );
    assert_ne!(Arm::Fat1.environment(), Arm::Fat1Lines.environment());
    assert_eq!(Arm::Fat1Lines.environment()[2].1, "line-tables-only");
    assert!(parse(["--arm", "unknown"].into_iter().map(str::to_owned)).is_err());
}
#[test]
fn thin_comparison_keeps_the_shipped_debug_information() {
    let opts = parse(
        ["--arm", "fat1", "--arm", "thin16"]
            .into_iter()
            .map(str::to_owned),
    )
    .unwrap();
    assert_eq!(
        order(&opts),
        vec![
            (0, Arm::Fat1),
            (0, Arm::Thin16),
            (1, Arm::Thin16),
            (1, Arm::Fat1)
        ]
    );
    assert_eq!(
        Arm::Thin16.environment(),
        [
            ("CARGO_PROFILE_RELEASE_LTO", "thin"),
            ("CARGO_PROFILE_RELEASE_CODEGEN_UNITS", "16"),
            ("CARGO_PROFILE_RELEASE_DEBUG", "1"),
        ]
    );
    assert_eq!(Arm::Thin16.environment()[2], Arm::Fat1.environment()[2]);
    assert_ne!(
        Arm::Thin16.environment()[2],
        Arm::Thin16Lines.environment()[2]
    );
}
#[test]
fn thin_debug_twin_rejects_changed_code() {
    let row = |arm: &str, code: &str| {
        aterm_json::json!({
            "arm": arm, "slice": "native", "sha256": arm,
            "inspection": {"sections": code, "symbols": "same"}
        })
    };
    let base = row("thin16", "same");
    let lines = row("thin16-lines", "same");
    let mut rows = vec![base.clone(), base, lines.clone(), lines];
    assert!(validate(&rows).is_ok());
    let changed = row("thin16-lines", "changed");
    rows[2] = changed.clone();
    rows[3] = changed;
    assert!(validate(&rows).is_err());
}
#[test]
fn identity_rejects_mutated_code_and_nonreproducible_binary() {
    let make = |arm: &str, hash: &str, sections: &str| aterm_json::json!({"arm":arm,"slice":"native","sha256":hash,"inspection":{"sections":sections,"symbols":"same"}});
    let row = make("fat1", "a", "same");
    let mut rows = vec![row.clone(), row.clone()];
    assert!(validate(&rows).is_ok());
    rows[1] = aterm_json::json!({"arm":"fat1","slice":"native","sha256":"a","runtime_app_sha256":"different stamped bytes","inspection":{"sections":"same","symbols":"same"}});
    assert!(validate(&rows).is_err());
    rows[1] = make("fat1", "mutated", "same");
    assert!(validate(&rows).is_err());
    rows[1] = row;
    let lines = make("fat1-lines", "b", "same");
    rows.extend([lines.clone(), lines]);
    assert!(validate(&rows).is_ok());
    rows[2] = make("fat1-lines", "b", "changed");
    assert!(validate(&rows).is_err());
    rows[2] = aterm_json::json!({"arm":"fat1-lines","slice":"native","sha256":"b","inspection":{"sections":"same","symbols":"changed"}});
    assert!(validate(&rows).is_err());
    assert!(sections(&[0; 32]).is_err());
}
