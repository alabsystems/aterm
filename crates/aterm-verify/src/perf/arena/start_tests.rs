// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// Every launch starts with the audible bell off (aterm's default is on),
/// the restore lane off and every lane of the scratch world's tables off
/// ([`aterm_types::world::LANES_OFF`]), as one document aterm's parser
/// reads.
#[test]
fn a_start_launch_has_the_audible_bell_off() {
    let doc = aterm_toml::from_str::<aterm_toml::Value>(&config()).expect("one document");
    let flag = |v: Option<&aterm_toml::Value>| v.and_then(aterm_toml::Value::as_bool);
    assert_eq!(flag(doc.get("bell_sound")), Some(false));
    assert_eq!(flag(doc.get("restore_session")), Some(false));
    assert_eq!(
        crate::smoke_stages::tests::lanes_left_on(&doc),
        Vec::<String>::new()
    );
}

/// A launch is marked a scratch world, the strip of inherited `ATERM_*`
/// variables notwithstanding: a release build's local-network lanes read
/// the marker raw.
#[test]
fn an_arena_launch_carries_the_scratch_worlds_marker() {
    let home = Path::new("/w/home");
    let mut cmd = command("/usr/bin/true");
    in_scratch_world(&mut cmd, home).expect("a platform with state under HOME");
    let marker = cmd
        .get_envs()
        .find(|(k, _)| *k == std::ffi::OsStr::new(aterm_types::world::MARKER))
        .and_then(|(_, v)| v.map(Path::new));
    assert_eq!(
        marker,
        aterm_types::world::state_home(home).as_deref(),
        "the marker names the world's state folder"
    );
}

/// NEGATIVE CONTROL: the first half, whose root goes.
#[cfg(unix)]
#[test]
fn a_frozen_copy_that_never_answered_keeps_its_run_root() {
    use std::os::unix::fs::PermissionsExt as _;
    let lay = |dir: &Path, frozen_says: &str| {
        let source = dir.join("source");
        std::fs::write(
            &source,
            format!(
                "#!/bin/sh
case \"$0\" in *-aterm) echo '{frozen_says}' ;; \
                 *) echo 'aterm 9.9.9' ;; esac\n"
            ),
        )
        .expect("the stand-in");
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        source
    };
    let sources = super::super::Scratch::new("atv-freeze-src").expect("scratch");

    let answered = super::super::Scratch::new("atv-freeze-ran").expect("scratch");
    let root = answered.path().to_path_buf();
    let source = lay(sources.path(), "aterm 9.9.9");
    let frozen = freeze("ran", &source, &root).expect("the copy answers as aterm");
    assert!(frozen.frozen.starts_with(&root));
    drop(answered);
    assert!(!root.exists(), "it ran: its root goes");

    let silent = super::super::Scratch::new("atv-freeze-kept").expect("scratch");
    let root = silent.path().to_path_buf();
    std::fs::remove_file(&source).expect("the next stand-in");
    let source = lay(sources.path(), "not aterm");
    let refused = freeze("kept", &source, &root);
    drop(silent);
    let kept = root.join("kept-aterm").is_file();
    let _ = std::fs::remove_dir_all(&root);
    assert!(refused.is_err(), "{refused:?}");
    assert!(
        kept,
        "a copy that never printed aterm's line keeps its root"
    );
}

/// A valid current-schema metrics read: every partition reconciles, the
/// reveal sits BELOW first-present (an early-reveal platform), and `bias`
/// rides the whole reconciling chain (both totals, the attach phase and its
/// window-setup child) so a slot offset survives every sum check.
pub(crate) fn metrics(bias: f64) -> Value {
    json!({
        "frames": 3,
        "first_present_ms": 14.9 + bias,
        "rust_main_to_first_present_ms": 15.0 + bias,
        "rust_main_to_first_visible_ms": 9.0 + bias,
        "startup_phase_schema": 1,
        "startup_phase_valid": true,
        "startup_router_ms": 0.1,
        "startup_gui_prepare_ms": 1.0,
        "startup_winit_dispatch_ms": 1.0,
        "startup_initial_surface_attach_ms": 10.0 + bias,
        "startup_surface_to_successful_redraw_ms": 2.0,
        "startup_successful_compose_ms": 0.2,
        "startup_successful_surface_transaction_ms": 0.6,
        "startup_successful_finalize_ms": 0.1,
        "startup_attach_schema": 1,
        "startup_attach_valid": true,
        "startup_attach_dispatch_ms": 0.1,
        "startup_attach_prepare_ms": 0.1,
        "startup_attach_window_create_ms": 2.0,
        "startup_attach_window_setup_ms": 1.0 + bias,
        "startup_attach_backend_finalize_ms": 5.0,
        "startup_attach_chrome_geometry_ms": 1.0,
        "startup_attach_surface_create_ms": 0.7,
        "startup_attach_finish_ms": 0.1,
        "backend": "gpu",
        "rows": 32,
        "cols": 110,
        "present_drops": 0,
        "last_present_drop_reason": "none",
        "last_present_drop_parked": false,
    })
}

pub(crate) fn obs(m: Value) -> Observation {
    Observation {
        metrics: m,
        external_ms: 20.0,
        thermal: "unavailable".into(),
    }
}

pub(crate) const LAWS: Laws = Laws {
    settle_ms: 1500,
    request: BackendRequest::Auto,
    allow_legacy: false,
};

fn with(mut m: Value, key: &str, v: Value) -> Value {
    m.as_object_mut().expect("object").insert(key.into(), v);
    m
}

fn without(mut m: Value, keys: &[&str]) -> Value {
    for k in keys {
        m.as_object_mut().expect("object").remove(*k);
    }
    m
}

fn refused(m: Value, laws: &Laws, why: &str) {
    let e = Sample::from_observation(&obs(m), laws).expect_err("must be refused");
    assert!(e.contains(why), "expected `{why}` in: {e}");
}

#[test]
fn a_current_sample_validates_with_its_reveal_below_first_present() {
    let s = Sample::from_observation(&obs(metrics(0.0)), &LAWS).expect("valid");
    assert_eq!(s.visible_ms, Some(9.0));
    assert!(s.visible_ms < s.rust_main_ms);
    assert_eq!(
        s.shape(),
        Shape {
            router: true,
            visible: true,
            phases: true,
            attach: true
        }
    );
    assert!(
        Sample::from_observation(&obs(metrics(2.0)), &LAWS).is_ok(),
        "bias reconciles"
    );
}

#[test]
fn every_partition_must_reconcile() {
    refused(
        with(metrics(0.0), "startup_successful_finalize_ms", json!(0.2)),
        &LAWS,
        "startup phase sum mismatch",
    );
    refused(
        with(metrics(0.0), "startup_attach_finish_ms", json!(0.2)),
        &LAWS,
        "startup attach sum mismatch",
    );
}

#[test]
fn a_schema_is_the_integer_one_never_a_bool_or_a_float() {
    refused(
        with(metrics(0.0), "startup_phase_schema", json!(true)),
        &LAWS,
        "startup_phase_schema=true",
    );
    refused(
        with(metrics(0.0), "startup_phase_schema", json!(1.0)),
        &LAWS,
        "startup_phase_schema=1.0; expected 1",
    );
    refused(
        with(metrics(0.0), "startup_attach_schema", json!(true)),
        &LAWS,
        "expected 1",
    );
    refused(
        with(metrics(0.0), "startup_phase_valid", json!(false)),
        &LAWS,
        "expected true",
    );
}

#[test]
fn missing_or_non_finite_totals_are_refused() {
    refused(
        without(metrics(0.0), &["first_present_ms"]),
        &LAWS,
        "first_present_ms is absent",
    );
    refused(
        with(metrics(0.0), "first_present_ms", json!("NaN")),
        &LAWS,
        "first_present_ms",
    );
    refused(with(metrics(0.0), "frames", json!(0)), &LAWS, "frames=0");
    refused(
        with(metrics(0.0), "first_present_ms", json!(1600.0)),
        &LAWS,
        "not safely below",
    );
}

#[test]
fn the_observer_brackets_every_total() {
    refused(
        with(metrics(0.0), "rust_main_to_first_visible_ms", json!(25.0)),
        &LAWS,
        "first-visible 25.00ms exceeds the external observation",
    );
    let mut late = obs(metrics(0.0));
    late.external_ms = 14.0;
    let e = Sample::from_observation(&late, &LAWS).expect_err("internal above external");
    assert!(e.contains("exceeds the external observation"), "{e}");
}

#[test]
fn a_forced_backend_must_be_the_one_negotiated() {
    let cpu = Laws {
        request: BackendRequest::Cpu,
        ..LAWS
    };
    refused(metrics(0.0), &cpu, "requested backend cpu, negotiated gpu");
    refused(with(metrics(0.0), "rows", json!(31)), &LAWS, "rows=31");
}

#[test]
fn the_drop_ledger_agrees_with_itself() {
    refused(
        with(
            metrics(0.0),
            "last_present_drop_reason",
            json!("gpu_timeout"),
        ),
        &LAWS,
        "zero drops carried",
    );
    refused(
        with(metrics(0.0), "present_drops", json!(2)),
        &LAWS,
        "nonzero drops carried reason=none",
    );
    refused(
        with(metrics(0.0), "last_present_drop_reason", json!("bogus")),
        &LAWS,
        "last_present_drop_reason",
    );
}

#[test]
fn a_legacy_binary_needs_the_explicit_switch() {
    let legacy = without(
        metrics(0.0),
        &[
            "rust_main_to_first_present_ms",
            "startup_phase_schema",
            "startup_attach_schema",
        ],
    );
    refused(
        legacy.clone(),
        &LAWS,
        "rust_main_to_first_present_ms is absent",
    );
    let allowed = Laws {
        allow_legacy: true,
        ..LAWS
    };
    let s = Sample::from_observation(&obs(legacy), &allowed).expect("legacy allowed");
    assert!(!s.shape().phases);
    refused(
        without(metrics(0.0), &["startup_phase_schema"]),
        &allowed,
        "attach drill-down is present without its parent",
    );
}

/// [`metrics`] plus a valid worker drill-down (gpu_build 4.0) and a GPU
/// drill-down whose legs close against it.
fn drilled() -> Value {
    let mut m = metrics(0.0);
    let fields = [
        ("startup_worker_schema", json!(1)),
        ("startup_worker_valid", json!(true)),
        ("startup_worker_total_ms", json!(8.0)),
        ("startup_worker_overlap_ms", json!(5.0)),
        ("startup_worker_after_join_ms", json!(3.0)),
        ("startup_worker_post_join_ms", json!(2.0)),
        ("startup_worker_prelude_ms", json!(0.5)),
        ("startup_worker_gpu_build_ms", json!(4.0)),
        ("startup_worker_font_admit_ms", json!(0.5)),
        ("startup_worker_font_apply_ms", json!(0.5)),
        ("startup_worker_font_seal_ms", json!(0.5)),
        ("startup_worker_epilogue_ms", json!(0.5)),
        ("startup_gpu_schema", json!(2)),
        ("startup_gpu_valid", json!(true)),
        ("startup_gpu_context_ms", json!(2.5)),
        ("startup_gpu_font_join_ms", json!(0.5)),
        ("startup_gpu_assemble_ms", json!(0.8)),
        ("startup_gpu_tail_ms", json!(0.2)),
        ("startup_gpu_font_thread_ms", json!(2.5)),
    ];
    for (k, v) in fields {
        m = with(m, k, v);
    }
    m
}

#[test]
fn the_worker_and_gpu_drill_downs_reconcile_when_valid() {
    let m = drilled();
    let s = Sample::from_observation(&obs(m.clone()), &LAWS).expect("both reconcile");
    assert_eq!(s.worker.as_ref().map(|w| w.total_ms), Some(8.0));
    let g = s.gpu.expect("gpu drill-down");
    assert_eq!(
        g.parallel,
        vec![2.5],
        "the font thread rides beside the sum"
    );
    for (key, v, why) in [
        ("startup_worker_total_ms", json!(9.0), "worker split"),
        (
            "startup_worker_post_join_ms",
            json!(1.0),
            "worker join split",
        ),
        ("startup_gpu_tail_ms", json!(0.5), "gpu leg sum mismatch"),
        (
            "startup_gpu_schema",
            json!(3),
            "startup_gpu_schema=3; expected 2",
        ),
        (
            "startup_gpu_font_thread_ms",
            json!(-1.0),
            "startup_gpu_font_thread_ms",
        ),
    ] {
        refused(with(m.clone(), key, v), &LAWS, why);
    }
    let s = Sample::from_observation(
        &obs(with(m.clone(), "startup_worker_valid", json!(false))),
        &LAWS,
    )
    .expect("invalid worker is skipped");
    assert!(s.worker.is_none());
    let s = Sample::from_observation(
        &obs(with(m.clone(), "startup_gpu_valid", json!(false))),
        &LAWS,
    )
    .expect("invalid gpu is skipped");
    assert!(s.gpu.is_none());
    let s = Sample::from_observation(&obs(with(m, "startup_gpu_schema", json!(1))), &LAWS)
        .expect("a wgpu-era schema is skipped");
    assert!(s.gpu.is_none());
}

#[test]
fn the_summary_carries_the_worker_total_and_the_parallel_leg() {
    let a = Sample::from_observation(&obs(drilled()), &LAWS).expect("valid");
    let samples = [a.clone(), a];
    assert_eq!(
        named(&samples, &GPU_PARALLEL, |s| {
            s.gpu.as_ref().map(|g| g.parallel.clone())
        }),
        Some(vec![(GPU_PARALLEL[0].to_string(), vec![2.5, 2.5])])
    );
    assert_eq!(
        column(&samples, |s| s.worker.as_ref().map(|w| w.total_ms)),
        Some(vec![8.0, 8.0])
    );
    let plain = Sample::from_observation(&obs(metrics(0.0)), &LAWS).expect("valid");
    assert_eq!(
        named(&[samples[0].clone(), plain], &GPU, |s| {
            s.gpu.as_ref().map(|g| g.legs.clone())
        }),
        None
    );
}

/// [`metrics`] plus the loader's share and a valid preparation split: ten
/// phases filling its 1.0 ms `startup_gui_prepare_ms` (binary fractions,
/// so the sum is exact).
fn prepared() -> Value {
    let mut m = with(metrics(0.0), BIRTH, json!(3.5));
    m = with(m, "startup_prepare_schema", json!(1));
    m = with(m, "startup_prepare_valid", json!(true));
    let ms = [
        0.25, 0.125, 0.125, 0.0625, 0.0625, 0.125, 0.0625, 0.0625, 0.0625, 0.0625,
    ];
    for (k, v) in PREPARE.into_iter().zip(ms) {
        m = with(m, k, json!(v));
    }
    m
}

#[test]
fn the_preparation_split_reconciles_with_the_phase_it_splits() {
    let s = Sample::from_observation(&obs(prepared()), &LAWS).expect("reconciles");
    assert_eq!(s.prepare.as_ref().map(|p| p.iter().sum::<f64>()), Some(1.0));
    assert_eq!(s.birth_ms, Some(3.5));
    for (key, v, why) in [
        (
            "startup_prepare_app_ms",
            json!(0.3),
            "startup prepare sum mismatch",
        ),
        (
            "startup_prepare_boot_ms",
            json!(-0.1),
            "startup_prepare_boot_ms",
        ),
        (
            "startup_prepare_schema",
            json!(2),
            "startup_prepare_schema=2",
        ),
    ] {
        refused(with(prepared(), key, v), &LAWS, why);
    }
    let invalid = with(prepared(), "startup_prepare_valid", json!(false));
    let s = Sample::from_observation(
        &obs(with(invalid, "startup_prepare_app_ms", json!(9.0))),
        &LAWS,
    )
    .expect("an invalid split is skipped");
    assert!(s.prepare.is_none());
}

#[test]
fn the_loaders_share_is_null_or_inside_the_observers_bound() {
    let s = Sample::from_observation(&obs(with(prepared(), BIRTH, Value::Null)), &LAWS)
        .expect("null is unavailable");
    assert_eq!(s.birth_ms, None);
    refused(
        with(prepared(), BIRTH, json!(-1.0)),
        &LAWS,
        "process_birth_to_rust_main_ms=-1",
    );
    refused(
        with(prepared(), BIRTH, json!("3")),
        &LAWS,
        "process_birth_to_rust_main_ms",
    );
    refused(
        with(prepared(), BIRTH, json!(6.0)),
        &LAWS,
        "birth->main 6.00ms",
    );
}

#[test]
fn a_missing_reveal_stamp_is_unavailable_not_invalid() {
    let s = Sample::from_observation(
        &obs(without(metrics(0.0), &["rust_main_to_first_visible_ms"])),
        &LAWS,
    )
    .expect("an older binary still measures");
    assert_eq!(s.visible_ms, None);
    assert!(!s.shape().visible);
}

#[test]
fn one_run_holds_one_shape_and_one_backend() {
    let mut c = Consistency::default();
    let a = Sample::from_observation(&obs(metrics(0.0)), &LAWS).expect("valid");
    c.check(&a).expect("first");
    c.check(&a).expect("same");
    let b = Sample::from_observation(
        &obs(without(metrics(0.0), &["rust_main_to_first_visible_ms"])),
        &LAWS,
    )
    .expect("valid");
    assert!(
        c.check(&b)
            .unwrap_err()
            .contains("mixed startup-metric schemas")
    );
    let mut c = Consistency::default();
    c.check(&a).expect("first");
    let cpu = Sample::from_observation(&obs(with(metrics(0.0), "backend", json!("cpu"))), &LAWS)
        .expect("valid");
    assert!(c.check(&cpu).unwrap_err().contains("mixed CPU/GPU"));
}

#[test]
fn a_record_carries_every_field_and_nulls_what_is_absent() {
    let s = Sample::from_observation(
        &obs(without(metrics(0.0), &["rust_main_to_first_visible_ms"])),
        &LAWS,
    )
    .expect("valid");
    let j = s.to_json();
    assert_eq!(j.get("rust_main_to_first_visible_ms"), Some(&Value::Null));
    assert_eq!(
        j.get("startup_attach_window_setup_ms")
            .and_then(Value::as_f64),
        Some(1.0)
    );
    assert_eq!(j.get("startup_gpu_context_ms"), Some(&Value::Null));
    assert_eq!(j.get(BIRTH), Some(&Value::Null));
    assert_eq!(j.get("startup_prepare_boot_ms"), Some(&Value::Null));
}

#[test]
fn write_json_atomic_writes_and_replaces_target() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("nested/out.json");
    write_json_atomic(&path, &json!({"ok": 1})).expect("write");
    let text = std::fs::read_to_string(&path).expect("read");
    assert_eq!(text, "{\"ok\":1}\n");
}
