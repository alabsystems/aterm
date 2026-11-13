// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::perf::bench_build::bench_profile::Profile;

#[test]
fn a_package_is_named_from_its_id() {
    for (id, name) in [
        ("path+file:///w/crates/aterm-core#0.106.0", "aterm-core"),
        (
            "registry+https://github.com/rust-lang/crates.io-index#shlex@2.0.1",
            "shlex",
        ),
        ("path+file:///w/crates/aterm-log-shim#log@0.4.32", "log"),
        ("bare", "bare"),
    ] {
        assert_eq!(package_name(id), name, "{id}");
    }
}

fn unit(package: &str, mode: &str, kind: &str, opt: &str, deps: &[usize]) -> String {
    let deps: Vec<String> = deps.iter().map(|d| format!("{{\"index\":{d}}}")).collect();
    format!(
        "{{\"pkg_id\":\"path+file:///w/crates/{package}#0.1.0\",\"mode\":\"{mode}\",\
         \"target\":{{\"kind\":[\"{kind}\"]}},\"profile\":{{\"opt_level\":\"{opt}\",\
         \"debug_assertions\":true,\"overflow_checks\":false,\"incremental\":false,\"lto\":\"false\",\
         \"codegen_units\":16}},\"dependencies\":[{}]}}",
        deps.join(",")
    )
}

fn graph(units: &[String], roots: &[usize]) -> String {
    let roots: Vec<String> = roots.iter().map(usize::to_string).collect();
    format!(
        "{{\"version\":1,\"units\":[{}],\"roots\":[{}]}}",
        units.join(","),
        roots.join(",")
    )
}

fn plan(lib_opt: &str) -> String {
    graph(
        &[
            unit("aterm", "bench", "bench", "3", &[1, 2, 5]), // 0
            unit("aterm-core", "build", "lib", lib_opt, &[3]), // 1
            unit("aterm", "build", "bin", "3", &[1]),         // 2
            unit("aterm-core", "run-custom-build", "custom-build", "3", &[4]), // 3
            unit("aterm-core", "build", "custom-build", "0", &[6]), // 4
            unit("aterm-serde-derive", "build", "proc-macro", "0", &[7]), // 5
            unit("cc", "build", "lib", "0", &[]),             // 6
            unit("syn", "build", "lib", "0", &[]),            // 7
        ],
        &[0],
    )
}

/// The fat-LTO, one-unit, assertions-off plan of the shipped arm, `edit` applied to its text:
/// a first occurrence is the root bench's, a unit on the path.
fn shipped_plan(edit: fn(String) -> String) -> String {
    let all = plan("3")
        .replace("\"debug_assertions\":true", "\"debug_assertions\":false")
        .replace("\"lto\":\"false\"", "\"lto\":\"true\"")
        .replace("\"codegen_units\":16", "\"codegen_units\":1");
    edit(all)
}

#[test]
fn the_walk_keeps_what_runs_and_leaves_build_scripts_and_derives() {
    let audit = Audit::of(&plan("3")).expect("parses");
    let packages: BTreeSet<&str> = audit.units.iter().map(|u| u.package.as_str()).collect();
    assert_eq!(packages, BTreeSet::from(["aterm", "aterm-core"]));
    assert_eq!(
        audit.units.len(),
        3,
        "the bench, the binary and the library"
    );
    assert_eq!(audit.refusal(Promise::of(Profile::Opt)), None);
}

#[test]
fn incremental_policy_is_retained_without_changing_codegen_admission() {
    let off = plan("3");
    let on = off.replace("\"incremental\":false", "\"incremental\":true");
    let mixed = off.replacen("\"incremental\":false", "\"incremental\":true", 1);
    let off = Audit::of(&off).unwrap();
    let on = Audit::of(&on).unwrap();
    let mixed = Audit::of(&mixed).unwrap();
    for audit in [&off, &on, &mixed] {
        assert_eq!(audit.refusal(Promise::of(Profile::Opt)), None);
    }
    assert_ne!(off, on, "the parsed policy must survive the audit");
    assert_ne!(
        off.summary(),
        on.summary(),
        "saved records must distinguish policies"
    );
    assert_ne!(mixed.summary(), off.summary());
    assert_ne!(mixed.summary(), on.summary());
}

#[test]
fn missing_incremental_policy_is_not_reported_as_off() {
    let missing = plan("3").replace(",\"incremental\":false", "");
    let why = Audit::of(&missing).expect_err("an absent policy is unknown");
    assert!(why.contains("incremental"), "{why}");
}

#[test]
fn a_crate_below_the_promise_is_named_and_the_dev_mix_is_reported() {
    let audit = Audit::of(&plan("2")).expect("parses");
    let why = audit.refusal(Promise::of(Profile::Opt)).expect("refused");
    assert!(why.contains("aterm-core (opt-level 2)"), "{why}");
    assert!(!why.contains("cc") && !why.contains("syn"), "{why}");
    let dev =
        Audit::of(&plan("2").replace("\"overflow_checks\":false", "\"overflow_checks\":true"))
            .expect("the dev mix parses");
    assert_eq!(dev.refusal(Promise::of(Profile::Dev)), None);
    assert_eq!(dev.by_level()["2"].0, 1);
    assert_eq!(dev.by_level()["3"].0, 2);
    assert!(dev.units.iter().all(|unit| unit.overflow_checks));
}

#[test]
fn every_arm_refuses_a_unit_whose_overflow_checks_differ_from_its_promise() {
    for (profile, text) in [
        (
            Profile::Dev,
            plan("3").replace("\"overflow_checks\":false", "\"overflow_checks\":true"),
        ),
        (Profile::Opt, plan("3")),
        (Profile::Shipped, shipped_plan(|all| all)),
    ] {
        let promise = Promise::of(profile);
        assert_eq!(Audit::of(&text).unwrap().refusal(promise), None);
        let expected = format!("\"overflow_checks\":{}", promise.overflow_checks);
        let changed = format!("\"overflow_checks\":{}", !promise.overflow_checks);
        let altered = text.replacen(&expected, &changed, 1);
        assert_ne!(altered, text, "the measured unit was actually changed");
        let why = Audit::of(&altered)
            .unwrap()
            .refusal(promise)
            .expect("refused");
        assert!(
            why.contains("overflow checks") && why.contains("aterm"),
            "{profile:?}: {why}"
        );
    }
}

#[test]
fn a_unit_without_debug_assertions_is_refused_where_the_arm_needs_them() {
    let text = plan("3").replacen("\"debug_assertions\":true", "\"debug_assertions\":false", 1);
    let audit = Audit::of(&text).expect("parses");
    for promise in [Promise::of(Profile::Dev), Promise::of(Profile::Opt)] {
        let why = audit.refusal(promise).expect("refused");
        assert!(why.contains("without debug assertions"), "{why}");
    }
}

#[test]
fn the_shipped_arm_holds_its_lto_codegen_units_and_assertions_to_the_plan() {
    let promise = Promise::of(Profile::Shipped);
    let audit = Audit::of(&shipped_plan(|all| all)).expect("parses");
    assert_eq!(audit.refusal(promise), None);
    for (edit, says) in [
        (
            (|all: String| all.replacen("\"lto\":\"true\"", "\"lto\":\"thin\"", 1))
                as fn(String) -> String,
            "lto true",
        ),
        (
            |all: String| all.replacen("\"codegen_units\":1", "\"codegen_units\":16", 1),
            "1 codegen unit",
        ),
        (
            |all: String| all.replacen("\"codegen_units\":1", "\"codegen_units\":null", 1),
            "1 codegen unit",
        ),
        (
            |all: String| {
                all.replacen("\"debug_assertions\":false", "\"debug_assertions\":true", 1)
            },
            "with debug assertions",
        ),
    ] {
        let why = Audit::of(&shipped_plan(edit))
            .expect("parses")
            .refusal(promise)
            .expect("refused");
        assert!(why.contains(says), "{says}: {why}");
    }
    let plain_release =
        plan("3").replace("\"debug_assertions\":true", "\"debug_assertions\":false");
    assert!(
        Audit::of(&plain_release)
            .expect("parses")
            .refusal(promise)
            .is_some(),
        "16 units and no LTO are not the shipped code"
    );
}

#[test]
fn a_graph_this_lane_cannot_read_is_an_error() {
    for bad in [
        "not json",
        "{\"version\":2,\"units\":[],\"roots\":[]}",
        "{\"version\":1,\"roots\":[0]}",
        "{\"version\":1,\"units\":[],\"roots\":[0]}",
    ] {
        assert!(Audit::of(bad).is_err(), "{bad}");
    }
    let empty = Audit::of("{\"version\":1,\"units\":[],\"roots\":[]}").expect("parses");
    assert!(
        empty.refusal(Promise::of(Profile::Dev)).is_some(),
        "nothing on the path is no build"
    );
}
